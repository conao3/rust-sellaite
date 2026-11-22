use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

use crate::runtime;

pub const TIMEOUT_MS: u32 = 500;

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

struct State {
    seats: Vec<(WlSeat, Option<String>)>,
    runtime_dir: PathBuf,
    seat_name: String,
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

impl Dispatch<ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.handle_idle_event(event);
    }
}

impl State {
    fn handle_idle_event(&self, event: ext_idle_notification_v1::Event) {
        match event {
            ext_idle_notification_v1::Event::Idled => {
                runtime::set_user_idle(&self.runtime_dir, true);
            }
            ext_idle_notification_v1::Event::Resumed => {
                let reason = format!("the user used their own seat ({})", self.seat_name);
                if let Err(e) = runtime::pause(&self.runtime_dir, &reason) {
                    eprintln!("sellaite: {e}");
                }
                runtime::set_user_idle(&self.runtime_dir, false);
            }
            _ => {}
        }
    }
}

pub fn watch(socket: &Path, seat_name: &str, runtime_dir: &Path) -> Result<Watcher, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| format!("cannot connect to {}: {e}", socket.display()))?;
    let conn = Connection::from_socket(stream).map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut state = State {
        seats: Vec::new(),
        runtime_dir: runtime_dir.to_path_buf(),
        seat_name: seat_name.to_string(),
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
    let seat = state
        .seats
        .iter()
        .find(|(_, name)| name.as_deref() == Some(seat_name))
        .map(|(seat, _)| seat.clone())
        .ok_or_else(|| format!("the compositor has no seat named {seat_name:?}"))?;
    let notifier: ExtIdleNotifierV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|e| format!("ext_idle_notifier_v1: {e}"))?;
    let notification = if notifier.version() >= 2 {
        notifier.get_input_idle_notification(TIMEOUT_MS, &seat, &qh, ())
    } else {
        notifier.get_idle_notification(TIMEOUT_MS, &seat, &qh, ())
    };
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        let _notification = notification;
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
            if unsafe { libc::poll(&mut fd, 1, POLL_MS) } > 0 && guard.read().is_err() {
                break;
            }
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

    #[test]
    fn idle_then_resume_updates_the_marker_and_pauses() {
        let dir = tempfile::tempdir().unwrap();
        let state = State {
            seats: Vec::new(),
            runtime_dir: dir.path().to_path_buf(),
            seat_name: "seat0".into(),
        };

        state.handle_idle_event(Event::Idled);
        assert!(runtime::user_idle(dir.path()));
        assert_eq!(runtime::paused(dir.path()), None);

        state.handle_idle_event(Event::Resumed);
        assert!(!runtime::user_idle(dir.path()));
        assert_eq!(
            runtime::paused(dir.path()).as_deref(),
            Some("the user used their own seat (seat0)")
        );
    }
}
