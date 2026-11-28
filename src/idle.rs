use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

use crate::runtime;

pub const TIMEOUT_MS: u32 = 1000;

const POLL_MS: i32 = 100;

pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seat {
    User,
    Agent,
}

struct State {
    seats: Vec<(WlSeat, Option<String>)>,
    runtime_dir: PathBuf,
    user_seat: String,
    user_active: bool,
    agent_active: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Name { name } = event
            && let Some(entry) = state.seats.iter_mut().find(|(s, _)| s == seat)
        {
            entry.1 = Some(name);
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, Seat> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        seat: &Seat,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.handle_idle_event(*seat, event);
    }
}

impl State {
    fn handle_idle_event(&mut self, seat: Seat, event: ext_idle_notification_v1::Event) {
        let active = match event {
            ext_idle_notification_v1::Event::Idled => false,
            ext_idle_notification_v1::Event::Resumed => true,
            _ => return,
        };
        match seat {
            Seat::User => self.user_active = active,
            Seat::Agent => {
                self.agent_active = active;
                if let Err(e) = runtime::set_agent_seat_active(&self.runtime_dir, active) {
                    eprintln!("sellaite: {e}");
                }
            }
        }
        let result = match (seat, active) {
            (Seat::User, true) => {
                let reason = format!("the user used their own seat ({})", self.user_seat);
                runtime::user_resumed(&self.runtime_dir, &reason)
            }
            (Seat::Agent, true) if runtime::taking_over(&self.runtime_dir) => {
                runtime::user_resumed(&self.runtime_dir, runtime::TAKEOVER_REASON)
            }
            (Seat::Agent, true) => return,
            (_, false)
                if self.user_active
                    || (self.agent_active && runtime::taking_over(&self.runtime_dir)) =>
            {
                return;
            }
            (_, false) => runtime::user_idled(&self.runtime_dir),
        };
        if let Err(e) = result {
            eprintln!("sellaite: {e}");
        }
    }
}

pub fn watch(
    socket: &Path,
    user_seat: &str,
    agent_seat: &str,
    runtime_dir: &Path,
) -> Result<Watcher, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| format!("cannot connect to {}: {e}", socket.display()))?;
    let conn = Connection::from_socket(stream).map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut state = State {
        seats: Vec::new(),
        runtime_dir: runtime_dir.to_path_buf(),
        user_seat: user_seat.to_string(),
        user_active: false,
        agent_active: false,
    };
    for global in globals.contents().clone_list() {
        if global.interface == "wl_seat" {
            let seat: WlSeat = globals
                .registry()
                .bind(global.name, global.version.min(8), &qh, ());
            state.seats.push((seat, None));
        }
    }
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let notifier: ExtIdleNotifierV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|e| format!("ext_idle_notifier_v1: {e}"))?;
    let mut notifications = Vec::new();
    for (role, name) in [(Seat::User, user_seat), (Seat::Agent, agent_seat)] {
        let seat = state
            .seats
            .iter()
            .find(|(_, seat_name)| seat_name.as_deref() == Some(name))
            .map(|(seat, _)| seat.clone())
            .ok_or_else(|| format!("the compositor has no seat named {name:?}"))?;
        notifications.push(if notifier.version() >= 2 {
            notifier.get_input_idle_notification(TIMEOUT_MS, &seat, &qh, role)
        } else {
            notifier.get_idle_notification(TIMEOUT_MS, &seat, &qh, role)
        });
    }
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let _notifications = notifications;
        while !stopping.load(Ordering::Relaxed) {
            if queue.dispatch_pending(&mut state).is_err() || queue.flush().is_err() {
                break;
            }
            let Some(guard) = queue.prepare_read() else {
                continue;
            };
            let mut fd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut fd, 1, POLL_MS) } > 0
                && let Err(error) = guard.read()
                && !matches!(&error, WaylandError::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock)
            {
                break;
            }
        }
        if !stopping.load(Ordering::Relaxed)
            && let Err(e) =
                runtime::user_resumed(&state.runtime_dir, "the user seat watcher disconnected")
        {
            eprintln!("sellaite: {e}");
        }
    });
    Ok(Watcher {
        stop,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ext_idle_notification_v1::Event;

    fn state(dir: &Path) -> State {
        State {
            seats: Vec::new(),
            runtime_dir: dir.to_path_buf(),
            user_seat: "user".into(),
            user_active: false,
            agent_active: false,
        }
    }

    #[test]
    fn idle_then_resume_updates_the_marker_and_pauses() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path());

        state.handle_idle_event(Seat::User, Event::Idled);
        assert!(runtime::user_idle(dir.path()));
        assert_eq!(runtime::paused(dir.path()), None);

        state.handle_idle_event(Seat::User, Event::Resumed);
        assert!(!runtime::user_idle(dir.path()));
        assert_eq!(
            runtime::paused(dir.path()).as_deref(),
            Some("the user used their own seat (user)")
        );
    }

    #[test]
    fn the_agent_seat_counts_as_the_user_only_while_taking_over() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path());
        state.handle_idle_event(Seat::User, Event::Idled);

        state.handle_idle_event(Seat::Agent, Event::Resumed);
        assert!(runtime::user_idle(dir.path()));
        assert_eq!(runtime::paused(dir.path()), None);
        state.handle_idle_event(Seat::Agent, Event::Idled);
        assert!(runtime::user_idle(dir.path()));

        runtime::take_over(dir.path()).unwrap();
        state.handle_idle_event(Seat::Agent, Event::Resumed);
        assert!(!runtime::user_idle(dir.path()));
        state.handle_idle_event(Seat::User, Event::Resumed);
        state.handle_idle_event(Seat::User, Event::Idled);
        assert!(
            !runtime::user_idle(dir.path()),
            "the user seat idled while the user still used the agent seat"
        );
        runtime::give_back(dir.path()).unwrap();
        assert_eq!(
            runtime::paused(dir.path()).as_deref(),
            Some(runtime::RESUME_PENDING_REASON)
        );
        state.handle_idle_event(Seat::Agent, Event::Idled);
        assert!(runtime::user_idle(dir.path()));
        assert_eq!(runtime::paused(dir.path()), None);
    }

    #[test]
    fn taking_over_an_active_agent_seat_marks_the_user_active() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path());
        state.handle_idle_event(Seat::User, Event::Idled);
        state.handle_idle_event(Seat::Agent, Event::Resumed);
        assert!(runtime::user_idle(dir.path()));

        runtime::take_over(dir.path()).unwrap();
        assert!(!runtime::user_idle(dir.path()));
        state.handle_idle_event(Seat::Agent, Event::Idled);
        assert!(runtime::user_idle(dir.path()));
        runtime::give_back(dir.path()).unwrap();
        runtime::take_over(dir.path()).unwrap();
        assert!(
            runtime::user_idle(dir.path()),
            "a Take over of an idle agent seat marked the user active"
        );
    }
}
