use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use x11rb::connection::Connection;
use x11rb::protocol::res::{self, ClientIdMask, ClientIdSpec, ConnectionExt as _};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MapState};
use x11rb::rust_connection::RustConnection;

const MAGIC: &[u8; 6] = b"i3-ipc";
const SUBSCRIBE: u32 = 2;
const WINDOW_EVENT: u32 = 0x8000_0003;
const POLL: Duration = Duration::from_millis(100);

pub const MARKER: &str = "SELLAITE_DESKTOP";

pub struct WindowEvents {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl WindowEvents {
    pub fn subscribe(swaysock: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(swaysock)
            .map_err(|e| format!("cannot connect to {}: {e}", swaysock.display()))?;
        let mut events = Self {
            stream,
            buf: Vec::new(),
        };
        let payload = json!(["window"]).to_string();
        let mut message = MAGIC.to_vec();
        message.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        message.extend_from_slice(&SUBSCRIBE.to_ne_bytes());
        message.extend_from_slice(payload.as_bytes());
        events
            .stream
            .write_all(&message)
            .map_err(|e| format!("sway IPC: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match events.next(deadline)? {
                Some((SUBSCRIBE, reply)) if reply["success"] == true => return Ok(events),
                Some((SUBSCRIBE, reply)) => {
                    return Err(format!("sway refused to subscribe: {reply}"));
                }
                Some(_) => {}
                None if Instant::now() >= deadline => {
                    return Err("sway did not answer the subscription".into());
                }
                None => {}
            }
        }
    }

    fn take_message(&mut self) -> Result<Option<(u32, Value)>, String> {
        if self.buf.len() < 14 {
            return Ok(None);
        }
        if &self.buf[..6] != MAGIC {
            return Err("sway IPC: bad magic".into());
        }
        let len = u32::from_ne_bytes(self.buf[6..10].try_into().unwrap()) as usize;
        let kind = u32::from_ne_bytes(self.buf[10..14].try_into().unwrap());
        if self.buf.len() < 14 + len {
            return Ok(None);
        }
        let body: Vec<u8> = self.buf.drain(..14 + len).skip(14).collect();
        let value = serde_json::from_slice(&body).map_err(|e| format!("sway IPC: {e}"))?;
        Ok(Some((kind, value)))
    }

    pub fn next(&mut self, deadline: Instant) -> Result<Option<(u32, Value)>, String> {
        if let Some(message) = self.take_message()? {
            return Ok(Some(message));
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .clamp(Duration::from_millis(1), POLL);
        self.stream
            .set_read_timeout(Some(wait))
            .map_err(|e| e.to_string())?;
        let mut chunk = [0u8; 4096];
        match self.stream.read(&mut chunk) {
            Ok(0) => Err("sway closed the IPC socket".into()),
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                self.take_message()
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(format!("sway IPC: {e}")),
        }
    }

    pub fn new_window_of(&mut self, pid: u32, deadline: Instant) -> Result<Option<Value>, String> {
        match self.next(deadline)? {
            Some((WINDOW_EVENT, event)) if event["change"] == "new" => {
                let owner = event["container"]["pid"].as_u64().map(|p| p as u32);
                Ok(owner
                    .filter(|&owner| descends_from(owner, pid))
                    .map(|_| event["container"].clone()))
            }
            _ => Ok(None),
        }
    }
}

pub fn xwayland_display(compositor: u32) -> Option<String> {
    (0..=32).find_map(|display| {
        let lock = fs::read_to_string(format!("/tmp/.X{display}-lock")).ok()?;
        let owner: u32 = lock
            .trim_matches(|c: char| c.is_whitespace() || c == '\0')
            .parse()
            .ok()?;
        (owner == compositor).then(|| format!(":{display}"))
    })
}

pub const REMAP_GRACE: Duration = Duration::from_millis(500);
const REMAP_PROBE: Duration = Duration::from_millis(250);

struct X11 {
    conn: RustConnection,
    root: u32,
    net_wm_pid: u32,
}

pub struct Remapper {
    display: String,
    swaysock: PathBuf,
    x11: Option<X11>,
    disabled: bool,
    next_probe: Instant,
    unseen: HashMap<u32, Instant>,
}

impl Remapper {
    pub fn new(display: String, swaysock: PathBuf) -> Self {
        Self {
            display,
            swaysock,
            x11: None,
            disabled: false,
            next_probe: Instant::now() + REMAP_GRACE,
            unseen: HashMap::new(),
        }
    }

    pub fn poll(&mut self, pid: u32) {
        if self.disabled || Instant::now() < self.next_probe {
            return;
        }
        self.next_probe = Instant::now() + REMAP_PROBE;
        if let Err(error) = self.probe(pid) {
            eprintln!(
                "sellaite: cannot check the X11 windows of pid {pid} on {}: {error}",
                self.display
            );
            self.disabled = true;
        }
    }

    fn probe(&mut self, pid: u32) -> Result<(), String> {
        if self.x11.is_none() {
            if !xwayland_running(&self.display) {
                return Ok(());
            }
            self.x11 = Some(X11::connect(&self.display)?);
        }
        let x11 = self.x11.as_ref().unwrap();
        let windows = x11.viewable_windows_of(pid)?;
        self.unseen.retain(|window, _| windows.contains(window));
        if windows.is_empty() {
            return Ok(());
        }
        let viewed = xwayland_windows(&swaymsg(&self.swaysock, &["-t", "get_tree"])?);
        let now = Instant::now();
        for window in windows {
            if viewed.contains(&window) {
                self.unseen.remove(&window);
                continue;
            }
            let since = *self.unseen.entry(window).or_insert(now);
            if now.duration_since(since) >= REMAP_GRACE {
                x11.remap(window)?;
                self.unseen.insert(window, now);
                eprintln!(
                    "sellaite: remapped X11 window {window:#x} of pid {pid}, which was viewable for {} ms without a sway view",
                    now.duration_since(since).as_millis()
                );
            }
        }
        Ok(())
    }
}

impl X11 {
    fn connect(display: &str) -> Result<Self, String> {
        let (conn, screen) = RustConnection::connect(Some(display)).map_err(|e| e.to_string())?;
        let root = conn.setup().roots[screen].root;
        let net_wm_pid = conn
            .intern_atom(false, b"_NET_WM_PID")
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .atom;
        conn.res_query_version(res::X11_XML_VERSION.0 as u8, res::X11_XML_VERSION.1 as u8)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            root,
            net_wm_pid,
        })
    }

    fn viewable_windows_of(&self, pid: u32) -> Result<Vec<u32>, String> {
        let children = self
            .conn
            .query_tree(self.root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .children;
        let mut windows = Vec::new();
        for window in children {
            let Ok(attributes) = self
                .conn
                .get_window_attributes(window)
                .map_err(|e| e.to_string())?
                .reply()
            else {
                continue;
            };
            if attributes.map_state != MapState::VIEWABLE || attributes.override_redirect {
                continue;
            }
            if self
                .owner(window)?
                .is_some_and(|owner| descends_from(owner, pid))
            {
                windows.push(window);
            }
        }
        Ok(windows)
    }

    fn owner(&self, window: u32) -> Result<Option<u32>, String> {
        let spec = ClientIdSpec {
            client: window,
            mask: ClientIdMask::LOCAL_CLIENT_PID,
        };
        if let Ok(reply) = self
            .conn
            .res_query_client_ids(&[spec])
            .map_err(|e| e.to_string())?
            .reply()
            && let Some(&owner) = reply.ids.first().and_then(|id| id.value.first())
        {
            return Ok(Some(owner));
        }
        Ok(self
            .conn
            .get_property(false, window, self.net_wm_pid, AtomEnum::CARDINAL, 0, 1)
            .map_err(|e| e.to_string())?
            .reply()
            .ok()
            .and_then(|reply| reply.value32()?.next()))
    }

    fn remap(&self, window: u32) -> Result<(), String> {
        self.conn.unmap_window(window).map_err(|e| e.to_string())?;
        self.conn
            .get_input_focus()
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        self.conn.map_window(window).map_err(|e| e.to_string())?;
        self.conn.flush().map_err(|e| e.to_string())
    }
}

fn xwayland_running(display: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        fs::read(entry.path().join("cmdline")).is_ok_and(|cmdline| {
            let mut args = cmdline.split(|&b| b == 0);
            args.next()
                .is_some_and(|program| program.ends_with(b"Xwayland"))
                && args.any(|arg| arg == display.as_bytes())
        })
    })
}

fn xwayland_windows(node: &Value) -> Vec<u32> {
    let mut windows: Vec<u32> = node["window"]
        .as_u64()
        .filter(|_| node["shell"] == "xwayland")
        .map(|window| window as u32)
        .into_iter()
        .collect();
    for key in ["nodes", "floating_nodes"] {
        for child in node[key].as_array().into_iter().flatten() {
            windows.extend(xwayland_windows(child));
        }
    }
    windows
}

pub fn seat_focus(swaysock: &Path, name: &str) -> Result<Option<u64>, String> {
    let seats = swaymsg(swaysock, &["-t", "get_seats"])?;
    Ok(seats
        .as_array()
        .and_then(|seats| seats.iter().find(|seat| seat["name"] == name))
        .and_then(|seat| seat["focus"].as_u64())
        .filter(|&id| id != 0))
}

pub fn focuses_x11(swaysock: &Path, seat: &str) -> Result<bool, String> {
    let Some(id) = seat_focus(swaysock, seat)? else {
        return Ok(false);
    };
    let tree = swaymsg(swaysock, &["-t", "get_tree"])?;
    Ok(find_node(&tree, id).is_some_and(|node| node["shell"] == "xwayland"))
}

fn find_node(node: &Value, id: u64) -> Option<&Value> {
    if node["id"].as_u64() == Some(id) {
        return Some(node);
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|key| node[key].as_array())
        .flatten()
        .find_map(|child| find_node(child, id))
}

pub fn swaymsg(swaysock: &Path, args: &[&str]) -> Result<Value, String> {
    let output = std::process::Command::new("swaymsg")
        .args(["-r", "-s"])
        .arg(swaysock)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run swaymsg (package: sway): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "swaymsg {args:?} failed: {}",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("swaymsg: {e}"))
}

fn stat(pid: u32) -> Option<(char, u32)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat.get(stat.rfind(')')? + 2..)?.split(' ');
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((state, ppid))
}

fn descends_from(mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        match stat(pid) {
            Some((_, ppid)) if ppid > 1 => pid = ppid,
            _ => return false,
        }
    }
    false
}

pub fn running(pid: u32) -> bool {
    stat(pid).is_some_and(|(state, _)| state != 'Z' && state != 'X')
}

pub fn marked(id: &str) -> Vec<u32> {
    let needle = format!("{MARKER}={id}");
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids: Vec<u32> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .filter(|&pid| {
            fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|&b| b == 0).any(|var| var == needle.as_bytes()))
        })
        .filter(|&pid| running(pid))
        .collect();
    pids.sort_unstable();
    pids
}

pub fn signal(pids: &[u32], signal: libc::c_int) {
    for &pid in pids {
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
}
