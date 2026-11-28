use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

pub const APP_ID: &str = "sellaite-first-seat";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Seat(String),
    Button(u32),
    Key(u32),
}

pub struct FirstSeatClient {
    events: Arc<Mutex<Vec<Event>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FirstSeatClient {
    pub fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("connect to the desktop");
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<State>(&conn).unwrap();
        let qh = queue.handle();
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut state = State {
            events: Arc::clone(&events),
            shm: globals.bind(&qh, 1..=1, ()).expect("wl_shm"),
            surface: None,
            buffer: None,
            size: (640, 480),
        };
        let first_seat = globals
            .contents()
            .clone_list()
            .into_iter()
            .find(|global| global.interface == "wl_seat")
            .expect("a wl_seat");
        let _seat: WlSeat =
            globals
                .registry()
                .bind(first_seat.name, first_seat.version.min(5), &qh, ());
        let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).expect("wl_compositor");
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=2, ()).expect("xdg_wm_base");
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_app_id(APP_ID.into());
        surface.commit();
        state.surface = Some(surface);
        queue.roundtrip(&mut state).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let _objects = (wm_base, xdg_surface, toplevel);
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
                if unsafe { libc::poll(&mut fd, 1, 50) } > 0
                    && let Err(error) = guard.read()
                    && !matches!(&error, WaylandError::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock)
                {
                    break;
                }
            }
        });
        Self {
            events,
            stop,
            thread: Some(thread),
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    pub fn seat(&self) -> Option<String> {
        self.events().into_iter().find_map(|event| match event {
            Event::Seat(name) => Some(name),
            _ => None,
        })
    }

    pub fn buttons(&self) -> usize {
        self.events()
            .iter()
            .filter(|event| matches!(event, Event::Button(_)))
            .count()
    }

    pub fn keys(&self) -> usize {
        self.events()
            .iter()
            .filter(|event| matches!(event, Event::Key(_)))
            .count()
    }
}

impl Drop for FirstSeatClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct State {
    events: Arc<Mutex<Vec<Event>>>,
    shm: WlShm,
    surface: Option<WlSurface>,
    buffer: Option<WlBuffer>,
    size: (i32, i32),
}

impl State {
    fn draw(&mut self, qh: &QueueHandle<Self>, width: i32, height: i32) {
        let stride = width * 4;
        let size = stride * height;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&vec![0xff; size as usize]).unwrap();
        let pool = self.shm.create_pool(file.as_fd(), size, qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, qh, ());
        pool.destroy();
        let surface = self.surface.as_ref().unwrap();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
        if let Some(old) = self.buffer.replace(buffer) {
            old.destroy();
        }
    }
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
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_seat::Event::Name { name } => state.events.lock().unwrap().push(Event::Seat(name)),
            wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(capabilities),
            } => {
                if capabilities.contains(wl_seat::Capability::Pointer) {
                    seat.get_pointer(qh, ());
                }
                if capabilities.contains(wl_seat::Capability::Keyboard) {
                    seat.get_keyboard(qh, ());
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            button,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            state.events.lock().unwrap().push(Event::Button(button));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            state.events.lock().unwrap().push(Event::Key(key));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && width > 0
            && height > 0
        {
            state.size = (width, height);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            let (width, height) = state.size;
            state.draw(qh, width, height);
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),*) => {
        $(impl Dispatch<$proxy, ()> for State {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        })*
    };
}

ignore_events!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer);
