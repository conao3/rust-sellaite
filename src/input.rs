use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_pointer::ButtonState;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use crate::keymap;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const KEYMAP_FORMAT_XKB_V1: u32 = 1;
const KEY_PRESSED: u32 = 1;
const KEY_RELEASED: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "middle" => Ok(Self::Middle),
            other => Err(format!(
                "unknown button {other:?}; use left, right or middle"
            )),
        }
    }

    fn code(self) -> u32 {
        match self {
            Self::Left => BTN_LEFT,
            Self::Right => BTN_RIGHT,
            Self::Middle => BTN_MIDDLE,
        }
    }
}

#[derive(Default)]
struct State {
    seats: Vec<(WlSeat, Option<String>)>,
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

ignore_events!(
    ZwlrVirtualPointerManagerV1,
    ZwlrVirtualPointerV1,
    ZwpVirtualKeyboardManagerV1,
    ZwpVirtualKeyboardV1
);

pub struct SeatInput {
    queue: EventQueue<State>,
    state: State,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    width: u32,
    height: u32,
    epoch: Instant,
}

impl SeatInput {
    pub fn connect(
        socket: &Path,
        seat_name: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let stream = UnixStream::connect(socket)
            .map_err(|e| format!("cannot connect to {}: {e}", socket.display()))?;
        let conn = Connection::from_socket(stream).map_err(|e| e.to_string())?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let mut state = State::default();
        for global in globals.contents().clone_list() {
            if global.interface == "wl_seat" {
                let seat: WlSeat =
                    globals
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
        let pointer_manager: ZwlrVirtualPointerManagerV1 = globals
            .bind(&qh, 1..=2, ())
            .map_err(|e| format!("zwlr_virtual_pointer_manager_v1: {e}"))?;
        let keyboard_manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| format!("zwp_virtual_keyboard_manager_v1: {e}"))?;
        let pointer = pointer_manager.create_virtual_pointer(Some(&seat), &qh, ());
        let keyboard = keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
        let mut input = Self {
            queue,
            state,
            pointer,
            keyboard,
            width,
            height,
            epoch: Instant::now(),
        };
        input.sync()?;
        Ok(input)
    }

    fn time(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }

    fn sync(&mut self) -> Result<(), String> {
        self.queue
            .roundtrip(&mut self.state)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn click(&mut self, x: u32, y: u32, button: Button) -> Result<(), String> {
        if x >= self.width || y >= self.height {
            return Err(format!(
                "({x}, {y}) is outside the {}x{} desktop",
                self.width, self.height
            ));
        }
        let time = self.time();
        self.pointer
            .motion_absolute(time, x, y, self.width, self.height);
        self.pointer.frame();
        self.pointer
            .button(time, button.code(), ButtonState::Pressed);
        self.pointer.frame();
        self.pointer
            .button(time, button.code(), ButtonState::Released);
        self.pointer.frame();
        self.sync()
    }

    pub fn type_text(&mut self, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        let chars: Vec<char> = text.chars().collect();
        for chunk in keymap::chunks(&chars) {
            let symbols = keymap::slots(&chunk);
            let source = keymap::xkb_source(&symbols);
            let fd = keymap_fd(&source)?;
            self.keyboard
                .keymap(KEYMAP_FORMAT_XKB_V1, fd.as_fd(), source.len() as u32 + 1);
            self.sync()?;
            for c in &chunk {
                let key =
                    keymap::evdev_key(&symbols, *c).ok_or_else(|| format!("no key for {c:?}"))?;
                let time = self.time();
                self.keyboard.key(time, key, KEY_PRESSED);
                self.keyboard.key(time, key, KEY_RELEASED);
                self.sync()?;
            }
        }
        Ok(())
    }
}

fn keymap_fd(source: &str) -> Result<OwnedFd, String> {
    let fd = unsafe { libc::memfd_create(c"sellaite-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd);
    file.write_all(source.as_bytes())
        .and_then(|()| file.write_all(&[0]))
        .map_err(|e| format!("cannot write the keymap: {e}"))?;
    Ok(OwnedFd::from(file))
}
