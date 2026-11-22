use std::ffi::CString;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::launch;

const PREFIX: &str = "sel.";

const STATE_FILE: &str = "sellaite.json";

const PAUSED_FILE: &str = "paused";

const USER_IDLE_FILE: &str = "user-idle";

const NOTES_DIR: &str = "notes";

pub const MAX_NOTE_CHARS: usize = 2000;

pub const VNC_SOCKET: &str = "vnc.sock";

const STARTING_GRACE: Duration = Duration::from_secs(60);

const REAP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Cleanup(Child);

impl Cleanup {
    pub fn start() -> Result<(Self, PathBuf), String> {
        let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command
            .args(["mcp", "reap-desktop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start desktop cleanup: {e}"))?;
        let stdout = child.stdout.take().unwrap();
        let cleanup = Self(child);
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(|e| format!("cannot read desktop runtime directory: {e}"))?;
        let dir = PathBuf::from(line.trim_end_matches('\n'));
        if dir.parent() != Some(temp_root().as_path()) || !id_of(&dir).is_some_and(is_id) {
            return Err("desktop cleanup did not provide a runtime directory".into());
        }
        Ok((cleanup, dir))
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

pub fn reap_desktop() -> Result<(), String> {
    let dir = make_dir()?;
    let lock = fs::File::open(&dir).and_then(|lock| {
        lock.lock()?;
        Ok(lock)
    });
    let _lock = lock
        .inspect_err(|_| {
            let _ = fs::remove_dir(&dir);
        })
        .map_err(|e| format!("cannot lock {}: {e}", dir.display()))?;
    let watch = (|| {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{}", dir.display())?;
        stdout.flush()?;
        drop(stdout);
        io::copy(&mut io::stdin().lock(), &mut io::sink())?;
        Ok::<(), io::Error>(())
    })();
    reap_dir(&dir)?;
    match watch {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.to_string()),
        _ => Ok(()),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct State {
    pub pid: u32,
    pub width: u32,
    pub height: u32,
    pub wayland_display: Option<String>,
}

pub fn temp_root() -> PathBuf {
    let tmp = Path::new("/tmp");
    if tmp.is_dir() {
        tmp.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

pub fn make_dir() -> Result<PathBuf, String> {
    let template = temp_root().join(format!("{PREFIX}XXXXXX"));
    let template = CString::new(template.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let raw = template.into_raw();
    let made = unsafe { libc::mkdtemp(raw) };
    let path = unsafe { CString::from_raw(raw) };
    if made.is_null() {
        return Err(format!("mkdtemp: {}", std::io::Error::last_os_error()));
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(path.as_bytes())))
}

pub fn id_of(dir: &Path) -> Option<&str> {
    dir.file_name()?.to_str()?.strip_prefix(PREFIX)
}

pub fn is_id(id: &str) -> bool {
    id.len() == 6 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(crate) fn write_atomically(path: &Path, contents: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn write_state(dir: &Path, state: &State) -> Result<(), String> {
    let json = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    write_atomically(&dir.join(STATE_FILE), &json)
}

pub fn read_state(dir: &Path) -> Option<State> {
    serde_json::from_slice(&fs::read(dir.join(STATE_FILE)).ok()?).ok()
}

pub fn pause(dir: &Path, reason: &str) -> Result<(), String> {
    write_atomically(&dir.join(PAUSED_FILE), reason.as_bytes())
}

pub fn resume(dir: &Path) -> Result<(), String> {
    match fs::remove_file(dir.join(PAUSED_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot resume: {e}")),
        _ => Ok(()),
    }
}

pub fn taking_over(dir: &Path) -> bool {
    dir.join("takeover").exists()
}

pub fn take_over(dir: &Path) -> Result<(), String> {
    fs::write(dir.join("takeover"), b"").map_err(|e| format!("cannot take over: {e}"))
}

pub fn give_back(dir: &Path) -> Result<(), String> {
    add_note(
        dir,
        "The user changed the screen. Take a screenshot before acting.",
    )?;
    resume(dir)?;
    fs::remove_file(dir.join("takeover")).map_err(|e| format!("cannot give back: {e}"))
}

pub fn paused(dir: &Path) -> Option<String> {
    if taking_over(dir) {
        return Some("the user has taken over the desktop".into());
    }
    fs::read_to_string(dir.join(PAUSED_FILE)).ok()
}

pub fn set_user_idle(dir: &Path, idle: bool) {
    let path = dir.join(USER_IDLE_FILE);
    if idle {
        let _ = fs::write(path, b"");
    } else {
        let _ = fs::remove_file(path);
    }
}

pub fn user_idle(dir: &Path) -> bool {
    dir.join(USER_IDLE_FILE).exists()
}

pub fn check_note(text: &str) -> Result<&str, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the note is empty".into());
    }
    let chars = text.chars().count();
    if chars > MAX_NOTE_CHARS {
        return Err(format!(
            "the note has {chars} characters; at most {MAX_NOTE_CHARS} are allowed"
        ));
    }
    Ok(text)
}

fn note_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.join(NOTES_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

pub fn add_note(dir: &Path, text: &str) -> Result<usize, String> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let text = check_note(text)?;
    let notes = dir.join(NOTES_DIR);
    match fs::create_dir(&notes) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(format!("cannot create {}: {e}", notes.display()));
        }
        _ => {}
    }
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = format!(
        "{nanos:020}-{:010}-{:010}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = notes.join(format!(".{name}.tmp"));
    fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, notes.join(&name)).map_err(|e| format!("cannot add the note: {e}"))?;
    Ok(pending_notes(dir))
}

pub fn pending_notes(dir: &Path) -> usize {
    note_names(dir).len()
}

pub fn take_notes(dir: &Path) -> Vec<String> {
    let notes = dir.join(NOTES_DIR);
    note_names(dir)
        .into_iter()
        .filter_map(|name| {
            let path = notes.join(&name);
            let text = fs::read_to_string(&path).ok()?;
            fs::remove_file(&path).ok()?;
            Some(text)
        })
        .collect()
}

pub fn live_dir(id: &str) -> Option<PathBuf> {
    live_desktops()
        .into_iter()
        .find(|(i, _, _)| i == id)
        .map(|(_, dir, _)| dir)
}

fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

fn wayland_socket_alive(dir: &Path) -> bool {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let name = entry.file_name();
            name.to_str().is_some_and(is_wayland_socket)
                && UnixStream::connect(entry.path()).is_ok()
        })
}

pub fn is_wayland_socket(name: &str) -> bool {
    name.strip_prefix("wayland-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn owned_dirs() -> Vec<PathBuf> {
    let uid = unsafe { libc::getuid() };
    fs::read_dir(temp_root())
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| id_of(path).is_some_and(is_id))
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && m.uid() == uid))
        .collect()
}

pub fn is_live(dir: &Path) -> bool {
    match read_state(dir) {
        Some(state) => process_alive(state.pid),
        None => {
            let young = fs::symlink_metadata(dir)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_none_or(|age| age < STARTING_GRACE);
            young || wayland_socket_alive(dir)
        }
    }
}

pub fn remove_stale() -> Vec<PathBuf> {
    owned_dirs()
        .into_iter()
        .filter(|dir| !is_live(dir))
        .filter(|dir| {
            let Ok(lock) = fs::File::open(dir) else {
                return false;
            };
            lock.try_lock().is_ok() && !is_live(dir) && reap_dir(dir).is_ok()
        })
        .collect()
}

fn reap_dir(dir: &Path) -> Result<(), String> {
    let id = id_of(dir).ok_or_else(|| format!("invalid runtime directory {}", dir.display()))?;
    let deadline = Instant::now() + REAP_TIMEOUT;
    let mut empty = false;
    loop {
        let left = launch::marked(id);
        if left.is_empty() && empty {
            break;
        }
        empty = left.is_empty();
        if !empty && Instant::now() >= deadline {
            return Err(format!(
                "marked processes remain for desktop {id}: {left:?}"
            ));
        }
        if !empty {
            launch::signal(&left, libc::SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(format!("cannot remove {}: {e}", dir.display()))
        }
        _ => Ok(()),
    }
}

pub fn live_desktops() -> Vec<(String, PathBuf, State)> {
    let mut desktops: Vec<_> = owned_dirs()
        .into_iter()
        .filter_map(|dir| {
            let state = read_state(&dir)?;
            (state.wayland_display.is_some() && process_alive(state.pid)).then_some(())?;
            Some((id_of(&dir)?.to_string(), dir, state))
        })
        .collect();
    desktops.sort_by(|a, b| a.0.cmp(&b.0));
    desktops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_the_mkdtemp_suffix() {
        assert!(is_id("Ab12Cd"));
        assert!(!is_id("../etc"));
        assert!(!is_id("Ab12C"));
        assert_eq!(id_of(Path::new("/tmp/sel.Ab12Cd")), Some("Ab12Cd"));
        assert_eq!(id_of(Path::new("/tmp/other")), None);
    }

    #[test]
    fn pause_and_resume_round_trip() {
        let dir = make_dir().unwrap();
        assert_eq!(paused(&dir), None);
        pause(&dir, "the user pressed stop").unwrap();
        assert_eq!(paused(&dir).as_deref(), Some("the user pressed stop"));
        resume(&dir).unwrap();
        resume(&dir).unwrap();
        assert_eq!(paused(&dir), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn notes_are_taken_once_in_order() {
        let dir = make_dir().unwrap();
        assert_eq!(take_notes(&dir), Vec::<String>::new());
        assert_eq!(add_note(&dir, "  first\n").unwrap(), 1);
        assert_eq!(add_note(&dir, "second").unwrap(), 2);
        fs::write(dir.join(NOTES_DIR).join(".half-written.tmp"), "partial").unwrap();
        assert_eq!(pending_notes(&dir), 2);
        assert_eq!(take_notes(&dir), ["first", "second"]);
        assert_eq!(take_notes(&dir), Vec::<String>::new());
        assert_eq!(pending_notes(&dir), 0);
        assert!(add_note(&dir, " \n ").is_err());
        assert!(add_note(&dir, &"x".repeat(MAX_NOTE_CHARS + 1)).is_err());
        assert!(add_note(&dir, &"\u{3042}".repeat(MAX_NOTE_CHARS)).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }
}
