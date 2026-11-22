use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::chord::Chord;
use crate::idle;
use crate::input::{Button, SeatInput};
use crate::launch::{self, WindowEvents};
use crate::runtime;

pub const AGENT_SEAT: &str = "agent";

pub const USER_SEAT: &str = "seat0";

const START_TIMEOUT: Duration = Duration::from_secs(20);

const STOP_TIMEOUT: Duration = Duration::from_secs(5);

const LAUNCH_LOG: &str = "launch.log";

pub struct Desktop {
    runtime_dir: PathBuf,
    sway: Child,
    wayvnc: Option<Child>,
    watcher: Option<idle::Watcher>,
    wayland_display: String,
    swaysock: PathBuf,
    width: u32,
    height: u32,
    input: SeatInput,
    launched: Vec<Child>,
}

pub struct Launched {
    pub pid: u32,
    pub window: Option<serde_json::Value>,
    pub log: PathBuf,
}

pub struct Info {
    pub id: String,
    pub runtime_dir: PathBuf,
    pub wayland_display: String,
    pub swaysock: PathBuf,
    pub vnc_socket: PathBuf,
    pub width: u32,
    pub height: u32,
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

fn log_tail(path: &Path) -> String {
    let log = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

fn spawn_child(command: &mut Command, what: &str) -> Result<Child, String> {
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            if libc::getppid() == 1 {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|e| format!("cannot start {what}: {e}"))
}

fn stop_child(child: &mut Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn runtime_state(width: u32, height: u32, wayland_display: Option<String>) -> runtime::State {
    runtime::State {
        pid: std::process::id(),
        width,
        height,
        wayland_display,
    }
}

impl Desktop {
    pub fn start(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("width and height must be positive".to_string());
        }
        let runtime_dir = runtime::make_dir()?;
        let started = runtime::write_state(&runtime_dir, &runtime_state(width, height, None))
            .and_then(|()| Self::start_in(runtime_dir.clone(), width, height));
        if started.is_err() {
            let _ = fs::remove_dir_all(&runtime_dir);
        }
        started
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
        let mut sway = spawn_child(&mut command, "sway")?;
        let deadline = Instant::now() + START_TIMEOUT;
        let (wayland_display, swaysock) = loop {
            if let Ok(Some(status)) = sway.try_wait() {
                return Err(format!(
                    "sway exited with {status}:\n{}",
                    log_tail(&log_path)
                ));
            }
            let display = find_entry(&runtime_dir, runtime::is_wayland_socket);
            let sock = find_entry(&runtime_dir, |n| {
                n.starts_with("sway-ipc.") && n.ends_with(".sock")
            });
            if let (Some(display), Some(sock)) = (display, sock) {
                break (display, runtime_dir.join(sock));
            }
            if Instant::now() > deadline {
                stop_child(&mut sway);
                return Err(format!(
                    "sway did not open its sockets within {}s:\n{}",
                    START_TIMEOUT.as_secs(),
                    log_tail(&log_path)
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let input = SeatInput::connect(
            &runtime_dir.join(&wayland_display),
            AGENT_SEAT,
            width,
            height,
        )
        .inspect_err(|_| stop_child(&mut sway))?;
        let mut desktop = Self {
            runtime_dir,
            sway,
            wayvnc: None,
            watcher: None,
            wayland_display,
            swaysock,
            width,
            height,
            input,
            launched: Vec::new(),
        };
        desktop.watch_user_seat()?;
        runtime::write_state(
            &desktop.runtime_dir,
            &runtime_state(width, height, Some(desktop.wayland_display.clone())),
        )?;
        Ok(desktop)
    }

    fn watch_user_seat(&mut self) -> Result<(), String> {
        self.watcher = Some(idle::watch(
            &self.runtime_dir.join(&self.wayland_display),
            USER_SEAT,
            &self.runtime_dir,
        )?);
        let vnc_socket = self.runtime_dir.join(runtime::VNC_SOCKET);
        let log_path = self.runtime_dir.join("wayvnc.log");
        let log = fs::File::create(&log_path).map_err(|e| e.to_string())?;
        let mut command = Command::new("wayvnc");
        command
            .args(["--unix-socket", "--seat", USER_SEAT])
            .arg("--socket")
            .arg(self.runtime_dir.join("wayvncctl"))
            .arg(&vnc_socket)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env_remove("DISPLAY")
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        let wayvnc = self.wayvnc.insert(spawn_child(&mut command, "wayvnc")?);
        let deadline = Instant::now() + START_TIMEOUT;
        while !vnc_socket.exists() {
            if let Ok(Some(status)) = wayvnc.try_wait() {
                return Err(format!(
                    "wayvnc exited with {status}:\n{}",
                    log_tail(&log_path)
                ));
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "wayvnc did not open {} within {}s:\n{}",
                    vnc_socket.display(),
                    START_TIMEOUT.as_secs(),
                    log_tail(&log_path)
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    pub fn info(&self) -> Info {
        Info {
            id: runtime::id_of(&self.runtime_dir)
                .unwrap_or_default()
                .to_string(),
            runtime_dir: self.runtime_dir.clone(),
            wayland_display: self.wayland_display.clone(),
            swaysock: self.swaysock.clone(),
            vnc_socket: self.runtime_dir.join(runtime::VNC_SOCKET),
            width: self.width,
            height: self.height,
        }
    }

    pub fn paused(&self) -> Option<String> {
        runtime::paused(&self.runtime_dir)
    }

    pub fn take_notes(&self) -> Vec<String> {
        runtime::take_notes(&self.runtime_dir)
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

    pub fn launch(&mut self, argv: &[String], wait: Option<Duration>) -> Result<Launched, String> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| "argv must name a program".to_string())?;
        self.launched
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        let mut events = WindowEvents::subscribe(&self.swaysock)?;
        let user_focus = launch::user_focus(&self.swaysock)?;
        let log_path = self.runtime_dir.join(LAUNCH_LOG);
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("cannot open {}: {e}", log_path.display()))?;
        let id = runtime::id_of(&self.runtime_dir)
            .unwrap_or_default()
            .to_string();
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(
                [
                    "PATH",
                    "HOME",
                    "USER",
                    "LOGNAME",
                    "SHELL",
                    "LANG",
                    "LANGUAGE",
                    "LC_ALL",
                    "LC_ADDRESS",
                    "LC_COLLATE",
                    "LC_CTYPE",
                    "LC_IDENTIFICATION",
                    "LC_MEASUREMENT",
                    "LC_MESSAGES",
                    "LC_MONETARY",
                    "LC_NAME",
                    "LC_NUMERIC",
                    "LC_PAPER",
                    "LC_TELEPHONE",
                    "LC_TIME",
                    "LOCALE_ARCHIVE",
                    "TZ",
                    "TERM",
                    "TERMINFO",
                    "TERMINFO_DIRS",
                    "XDG_CONFIG_HOME",
                    "XDG_CONFIG_DIRS",
                    "XDG_DATA_HOME",
                    "XDG_DATA_DIRS",
                    "XDG_CACHE_HOME",
                    "XDG_STATE_HOME",
                    "FONTCONFIG_FILE",
                    "FONTCONFIG_PATH",
                    "DBUS_SESSION_BUS_ADDRESS",
                    "AT_SPI_BUS_ADDRESS",
                ]
                .into_iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (name, value))),
            )
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env("SWAYSOCK", &self.swaysock)
            .env(launch::MARKER, id)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = spawn_child(&mut command, program)?;
        let pid = child.id();
        self.launched.push(child);
        let child = self.launched.last_mut().unwrap();
        let window = match wait {
            Some(timeout) => {
                let deadline = Instant::now() + timeout;
                loop {
                    if let Some(window) = events.new_window_of(pid, deadline)? {
                        launch::restore_user_focus(&self.swaysock, user_focus, &window)?;
                        break Some(window);
                    }
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(format!(
                            "{program} (pid {pid}) exited with {status} before a window mapped; its output is in {}",
                            log_path.display()
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "{program} (pid {pid}) is running but mapped no window within {} ms; stop_desktop stops it",
                            timeout.as_millis()
                        ));
                    }
                }
            }
            None => {
                if user_focus.is_some() {
                    let swaysock = self.swaysock.clone();
                    std::thread::spawn(move || {
                        let deadline = Instant::now() + Duration::from_secs(60);
                        while Instant::now() < deadline && launch::running(pid) {
                            match events.new_window_of(pid, deadline) {
                                Ok(Some(window)) => {
                                    if let Err(e) =
                                        launch::restore_user_focus(&swaysock, user_focus, &window)
                                    {
                                        eprintln!("sellaite: {e}");
                                    }
                                    break;
                                }
                                Ok(None) => {}
                                Err(_) => break,
                            }
                        }
                    });
                }
                None
            }
        };
        Ok(Launched {
            pid,
            window,
            log: log_path,
        })
    }

    fn stop_launched(&mut self) {
        let Some(id) = runtime::id_of(&self.runtime_dir).map(str::to_string) else {
            return;
        };
        launch::signal(&launch::marked(&id), libc::SIGTERM);
        let deadline = Instant::now() + STOP_TIMEOUT;
        loop {
            self.launched
                .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
            let left = launch::marked(&id);
            if left.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                launch::signal(&left, libc::SIGKILL);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for child in &mut self.launched {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.launched.clear();
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
        self.stop_launched();
        self.watcher = None;
        if let Some(wayvnc) = self.wayvnc.as_mut() {
            stop_child(wayvnc);
        }
        stop_child(&mut self.sway);
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}
