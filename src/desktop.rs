use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::chord::Chord;
use crate::input::{Button, SeatInput};

pub const AGENT_SEAT: &str = "agent";

pub const USER_SEAT: &str = "seat0";

const START_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Desktop {
    runtime_dir: PathBuf,
    sway: Child,
    wayland_display: String,
    swaysock: PathBuf,
    width: u32,
    height: u32,
    input: SeatInput,
}

pub struct Info {
    pub runtime_dir: PathBuf,
    pub wayland_display: String,
    pub swaysock: PathBuf,
    pub width: u32,
    pub height: u32,
}

fn short_temp_root() -> PathBuf {
    let tmp = Path::new("/tmp");
    if tmp.is_dir() {
        tmp.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

fn make_runtime_dir() -> Result<PathBuf, String> {
    let template = short_temp_root().join("sel.XXXXXX");
    let template = CString::new(template.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let raw = template.into_raw();
    let made = unsafe { libc::mkdtemp(raw) };
    let path = unsafe { CString::from_raw(raw) };
    if made.is_null() {
        return Err(format!("mkdtemp: {}", std::io::Error::last_os_error()));
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(path.as_bytes())))
}

fn sway_config(width: u32, height: u32) -> String {
    format!(
        "output HEADLESS-1 resolution {width}x{height} position 0 0\n\
         default_orientation horizontal\n\
         focus_follows_mouse no\n\
         seat {USER_SEAT} fallback true\n\
         seat {AGENT_SEAT} fallback false\n"
    )
}

fn find_entry(dir: &Path, matches: impl Fn(&str) -> bool) -> Option<String> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .find(|name| matches(name))
}

fn is_wayland_socket(name: &str) -> bool {
    name.strip_prefix("wayland-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn log_tail(path: &Path) -> String {
    let log = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

impl Desktop {
    pub fn start(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("width and height must be positive".to_string());
        }
        let runtime_dir = make_runtime_dir()?;
        match Self::start_in(runtime_dir.clone(), width, height) {
            Ok(desktop) => Ok(desktop),
            Err(e) => {
                let _ = fs::remove_dir_all(&runtime_dir);
                Err(e)
            }
        }
    }

    fn start_in(runtime_dir: PathBuf, width: u32, height: u32) -> Result<Self, String> {
        let config = runtime_dir.join("sway.config");
        fs::write(&config, sway_config(width, height)).map_err(|e| e.to_string())?;
        let log_path = runtime_dir.join("sway.log");
        let log = fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let mut command = Command::new("sway");
        command
            .arg("--config")
            .arg(&config)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("WLR_RENDERER", "pixman")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("SWAYSOCK")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::getppid() == 1 {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut sway = command
            .spawn()
            .map_err(|e| format!("cannot start sway: {e}"))?;
        let deadline = Instant::now() + START_TIMEOUT;
        let (wayland_display, swaysock) = loop {
            if let Ok(Some(status)) = sway.try_wait() {
                return Err(format!(
                    "sway exited with {status}:\n{}",
                    log_tail(&log_path)
                ));
            }
            let display = find_entry(&runtime_dir, is_wayland_socket);
            let sock = find_entry(&runtime_dir, |n| {
                n.starts_with("sway-ipc.") && n.ends_with(".sock")
            });
            if let (Some(display), Some(sock)) = (display, sock) {
                break (display, runtime_dir.join(sock));
            }
            if Instant::now() > deadline {
                let _ = sway.kill();
                let _ = sway.wait();
                return Err(format!(
                    "sway did not open its sockets within {}s:\n{}",
                    START_TIMEOUT.as_secs(),
                    log_tail(&log_path)
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let input = match SeatInput::connect(
            &runtime_dir.join(&wayland_display),
            AGENT_SEAT,
            width,
            height,
        ) {
            Ok(input) => input,
            Err(e) => {
                let _ = sway.kill();
                let _ = sway.wait();
                return Err(e);
            }
        };
        Ok(Self {
            runtime_dir,
            sway,
            wayland_display,
            swaysock,
            width,
            height,
            input,
        })
    }

    pub fn info(&self) -> Info {
        Info {
            runtime_dir: self.runtime_dir.clone(),
            wayland_display: self.wayland_display.clone(),
            swaysock: self.swaysock.clone(),
            width: self.width,
            height: self.height,
        }
    }

    pub fn click(&mut self, x: u32, y: u32, button: Button) -> Result<(), String> {
        self.input.click(x, y, button)
    }

    pub fn type_text(&mut self, text: &str) -> Result<(), String> {
        self.input.type_text(text)
    }

    pub fn key(&mut self, chord: &Chord) -> Result<(), String> {
        self.input.key(chord)
    }

    pub fn screenshot(&self) -> Result<Vec<u8>, String> {
        let output = Command::new("grim")
            .args(["-t", "png", "-"])
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run grim: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "grim failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(output.stdout)
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.sway.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(self.sway.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.sway.kill();
        let _ = self.sway.wait();
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}
