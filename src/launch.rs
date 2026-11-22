use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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

pub fn user_focus(swaysock: &Path) -> Result<Option<u64>, String> {
    seat_focus(swaysock, crate::desktop::USER_SEAT)
}

pub fn seat_focus(swaysock: &Path, name: &str) -> Result<Option<u64>, String> {
    let seats = swaymsg(swaysock, &["-t", "get_seats"])?;
    Ok(seats
        .as_array()
        .and_then(|seats| seats.iter().find(|seat| seat["name"] == name))
        .and_then(|seat| seat["focus"].as_u64())
        .filter(|&id| id != 0))
}

pub fn restore_user_focus(
    swaysock: &Path,
    previous: Option<u64>,
    window: &Value,
) -> Result<(), String> {
    if let Some(previous) = previous
        && user_focus(swaysock)? == window["id"].as_u64()
        && Some(previous) != window["id"].as_u64()
        && let Err(error) = swaymsg(swaysock, &[&format!("[con_id={previous}] focus")])
        && !error.contains("No matching node.")
    {
        return Err(error);
    }
    Ok(())
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
