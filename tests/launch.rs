mod common;

use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use common::{Mcp, wait_for};

fn seat_focus(swaysock: &Path, seat: &str) -> i64 {
    let output = Command::new("swaymsg")
        .args(["--raw", "-t", "get_seats"])
        .env("SWAYSOCK", swaysock)
        .output()
        .expect("run swaymsg");
    assert!(output.status.success(), "swaymsg get_seats failed");
    let seats: Value = serde_json::from_slice(&output.stdout).unwrap();
    seats
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == seat)
        .unwrap_or_else(|| panic!("no seat {seat}"))["focus"]
        .as_i64()
        .unwrap()
}

fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        let state = stat[stat.rfind(')').unwrap() + 2..].chars().next();
        !matches!(state, Some('Z' | 'X'))
    })
}

fn marked(id: &str) -> Vec<u32> {
    let needle = format!("SELLAITE_DESKTOP={id}");
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .filter(|&pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|&b| b == 0).any(|v| v == needle.as_bytes()))
        })
        .filter(|&pid| running(pid))
        .collect()
}

fn children_of(pid: u32) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&child| {
            std::fs::read_to_string(format!("/proc/{child}/stat")).is_ok_and(|stat| {
                stat[stat.rfind(')').unwrap() + 2..].split(' ').nth(1) == Some(&pid.to_string())
            })
        })
        .collect()
}

fn error_text(mcp: &mut Mcp, tool: &str, arguments: Value) -> String {
    let result = mcp.request("tools/call", json!({"name": tool, "arguments": arguments}));
    assert_eq!(result["isError"], true, "{tool} succeeded: {result}");
    result["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn launch_opens_a_focused_window_and_stop_desktop_ends_every_launched_process() {
    let out = std::env::temp_dir().join(format!("sellaite-launch-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let file = out.join("out");

    let mut mcp = Mcp::spawn_with_env([
        ("SELLAITE_TEST_SECRET", "test-secret"),
        ("SSH_AUTH_SOCK", "/tmp/sellaite-test-ssh.sock"),
    ]);
    let message = error_text(&mut mcp, "launch", json!({"argv": ["foot"]}));
    assert!(message.contains("start_desktop"), "{message}");
    let info = mcp.call("start_desktop", json!({}))["structuredContent"].clone();
    let id = info["Id"].as_str().unwrap().to_string();
    let runtime_dir = PathBuf::from(info["XdgRuntimeDir"].as_str().unwrap());
    let swaysock = PathBuf::from(info["Swaysock"].as_str().unwrap());

    let launched = mcp.call(
        "launch",
        json!({"argv": ["foot", "--app-id", "launched", "sh"], "wait_for_window": true}),
    )["structuredContent"]
        .clone();
    let foot = launched["Pid"].as_u64().unwrap() as u32;
    let window = &launched["Window"];
    assert_eq!(window["AppId"], "launched", "{launched}");
    assert_eq!(window["Pid"], foot);
    let environment = std::fs::read(format!("/proc/{foot}/environ")).unwrap();
    let environment: Vec<_> = environment.split(|&byte| byte == 0).collect();
    for entry in [
        format!("XDG_RUNTIME_DIR={}", runtime_dir.display()),
        format!("SWAYSOCK={}", swaysock.display()),
        format!(
            "WAYLAND_DISPLAY={}",
            info["WaylandDisplay"].as_str().unwrap()
        ),
        format!("SELLAITE_DESKTOP={id}"),
    ] {
        assert!(environment.contains(&entry.as_bytes()), "missing {entry}");
    }
    assert_eq!(
        seat_focus(&swaysock, "agent"),
        window["Id"].as_i64().unwrap(),
        "the launched window does not have the agent seat's focus"
    );

    mcp.call(
        "type",
        json!({"text": format!("echo launched-ok > '{}'", file.display())}),
    );
    mcp.call("key", json!({"keys": "Return"}));
    wait_for("the shell in the launched foot to run the command", || {
        (std::fs::read_to_string(&file).ok()? == "launched-ok\n").then_some(())
    });

    mcp.call(
        "launch",
        json!({"argv": ["sh", "-c", "(sleep 600 &); exec sleep 601"]}),
    );
    let message = error_text(
        &mut mcp,
        "launch",
        json!({"argv": ["sleep", "602"], "wait_for_window": true, "timeout_ms": 300}),
    );
    assert!(
        message.contains("mapped no window within 300 ms"),
        "{message}"
    );
    let message = error_text(
        &mut mcp,
        "launch",
        json!({"argv": ["true"], "wait_for_window": true}),
    );
    assert!(message.contains("before a window mapped"), "{message}");
    let message = error_text(
        &mut mcp,
        "launch",
        json!({"argv": ["sellaite-no-such-program"]}),
    );
    assert!(message.contains("cannot start"), "{message}");
    let message = error_text(&mut mcp, "launch", json!({"argv": []}));
    assert!(message.contains("argv"), "{message}");

    let user_focus = seat_focus(&swaysock, "seat0");
    let second = mcp.call(
        "launch",
        json!({"argv": ["foot", "--app-id", "second", "sh"], "wait_for_window": true}),
    );
    assert_eq!(seat_focus(&swaysock, "seat0"), user_focus);
    assert_eq!(
        seat_focus(&swaysock, "agent"),
        second["structuredContent"]["Window"]["Id"]
            .as_i64()
            .unwrap()
    );
    let stable = mcp.call("wait_for_stable_screen", json!({"quiet_ms": 500}));
    println!("{}", stable["content"][0]["text"]);

    let shells = children_of(foot);
    assert!(!shells.is_empty(), "foot has no shell");
    let before = marked(&id);
    for &pid in shells.iter().chain([&foot]) {
        assert!(before.contains(&pid), "{pid} is not marked: {before:?}");
    }
    assert!(before.len() >= 5, "{before:?}");

    mcp.call("stop_desktop", json!({}));
    let left: Vec<u32> = before.iter().copied().filter(|&pid| running(pid)).collect();
    assert!(
        left.is_empty(),
        "launched processes outlived stop_desktop: {left:?}"
    );
    assert!(marked(&id).is_empty());
    assert!(!runtime_dir.exists());
    let _ = std::fs::remove_dir_all(&out);
    for name in [b"SELLAITE_TEST_SECRET=".as_slice(), b"SSH_AUTH_SOCK="] {
        assert!(
            !environment.iter().any(|entry| entry.starts_with(name)),
            "launch inherited {name:?}"
        );
    }
}

#[test]
fn wait_sleeps_and_refuses_long_waits() {
    let mut mcp = Mcp::spawn();
    let started = Instant::now();
    mcp.call("wait", json!({"milliseconds": 200}));
    assert!(started.elapsed() >= Duration::from_millis(200));
    let message = error_text(&mut mcp, "wait", json!({"milliseconds": 30_001}));
    assert!(message.contains("at most 30000"), "{message}");
}

fn assert_signal_cleanup(signal: libc::c_int) {
    let mut mcp = Mcp::spawn();
    let info = mcp.call("start_desktop", json!({}))["structuredContent"].clone();
    let id = info["Id"].as_str().unwrap();
    let dir = Path::new(info["XdgRuntimeDir"].as_str().unwrap());
    let parent = mcp.call(
        "launch",
        json!({"argv": ["sh", "-c", "(sleep 600 &); exec sleep 601"]}),
    )["structuredContent"]["Pid"]
        .as_u64()
        .unwrap() as u32;
    let before = wait_for("the orphaned sleep and its marked launcher", || {
        let pids = marked(id);
        (pids.len() == 2 && pids.contains(&parent)).then_some(pids)
    });

    assert_eq!(mcp.signal_and_wait(signal).signal(), Some(signal));
    let deadline = Instant::now() + Duration::from_secs(10);
    let (left, dir_exists) = loop {
        let left = marked(id);
        let dir_exists = dir.exists();
        if (left.is_empty() && !dir_exists) || std::time::Instant::now() >= deadline {
            break (left, dir_exists);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    for &pid in &left {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    if dir_exists {
        let _ = std::fs::remove_dir_all(dir);
    }
    assert!(
        left.is_empty() && !dir_exists,
        "signal {signal}: marked PIDs before {before:?}, after {left:?}; runtime directory still exists: {dir_exists}"
    );
}

#[test]
fn sigterm_cleans_up_the_desktop() {
    assert_signal_cleanup(libc::SIGTERM);
}

#[test]
fn sigkill_cleans_up_the_desktop() {
    assert_signal_cleanup(libc::SIGKILL);
}

#[test]
fn mcp_startup_cleans_up_marked_processes_from_a_stale_desktop() {
    let dir = tempfile::Builder::new()
        .prefix("sel.")
        .tempdir_in("/tmp")
        .unwrap();
    let id = dir.path().file_name().unwrap().to_str().unwrap();
    let id = id.strip_prefix("sel.").unwrap();
    let lock = std::fs::File::open(dir.path()).unwrap();
    lock.lock().unwrap();
    let mut process = Command::new("sh")
        .args(["-c", "printf R; read -r line"])
        .env("SELLAITE_DESKTOP", id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = [0];
    process
        .stdout
        .take()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(ready, *b"R");
    assert_eq!(marked(id), [process.id()]);
    std::fs::write(
        dir.path().join("sellaite.json"),
        json!({"Pid": 0, "Width": 640, "Height": 480, "WaylandDisplay": "wayland-1"}).to_string(),
    )
    .unwrap();

    let startup = std::thread::spawn(Mcp::spawn);
    std::thread::sleep(Duration::from_millis(300));
    drop(lock);
    let _mcp = startup.join().unwrap();
    let status = process.try_wait().unwrap();
    if status.is_none() {
        process.kill().unwrap();
        process.wait().unwrap();
    }
    assert_eq!(
        status.and_then(|status| status.signal()),
        Some(libc::SIGKILL)
    );
    assert!(marked(id).is_empty());
    assert!(!dir.path().exists());
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[test]
fn launch_opens_an_x11_window_through_the_desktops_xwayland() {
    if !on_path("Xwayland") {
        use std::io::Write;
        writeln!(
            std::io::stderr(),
            "skipped: Xwayland is not on PATH (add xwayland to the devShell)"
        )
        .unwrap();
        return;
    }
    let out = std::env::temp_dir().join(format!("sellaite-xterm-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let file = out.join("typed");
    let mut mcp = Mcp::spawn_with_env([("DISPLAY", ":sellaite-user-display")]);
    let info = mcp.call("start_desktop", json!({}))["structuredContent"].clone();
    let swaysock = PathBuf::from(info["Swaysock"].as_str().unwrap());

    let launched = mcp.call(
        "launch",
        json!({
            "argv": ["xterm", "-e", "sh", "-c", format!("cat > '{}'", file.display())],
            "wait_for_window": true
        }),
    )["structuredContent"]
        .clone();
    let xterm = launched["Pid"].as_u64().unwrap() as u32;
    let window = &launched["Window"];
    assert_eq!(window["Class"], "XTerm", "{launched}");
    assert_eq!(window["Pid"], xterm, "{launched}");
    assert_eq!(
        seat_focus(&swaysock, "agent"),
        window["Id"].as_i64().unwrap(),
        "the X11 window does not have the agent seat's focus"
    );

    let environment = std::fs::read(format!("/proc/{xterm}/environ")).unwrap();
    let display = environment
        .split(|&byte| byte == 0)
        .find_map(|entry| entry.strip_prefix(b"DISPLAY="))
        .map(|value| String::from_utf8(value.to_vec()).unwrap())
        .expect("xterm has no DISPLAY");
    let lock = std::fs::read_to_string(format!("/tmp/.X{}-lock", &display[1..])).unwrap();
    let owner = lock.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    let runtime_dir = format!(
        "XDG_RUNTIME_DIR={}",
        info["XdgRuntimeDir"].as_str().unwrap()
    );
    let sway = std::fs::read_to_string(format!("/proc/{owner}/stat")).unwrap_or_default();
    let sway_environment = std::fs::read(format!("/proc/{owner}/environ")).unwrap_or_default();
    assert!(
        sway.contains("(sway)")
            && sway_environment
                .split(|&byte| byte == 0)
                .any(|entry| entry == runtime_dir.as_bytes()),
        "DISPLAY {display} does not belong to the desktop's sway: {sway}"
    );

    let text = "Hello, World! ~`@#$%^&*()_+-={}[]|\\:;\"'<>?,./ 0123456789 xyz";
    mcp.call("type", json!({"text": "discarded"}));
    mcp.call("key", json!({"keys": "ctrl+u"}));
    mcp.call("type", json!({"text": text}));
    mcp.call("key", json!({"keys": "Return"}));
    let deadline = Instant::now() + Duration::from_secs(20);
    let typed = loop {
        let typed = std::fs::read_to_string(&file).unwrap_or_default();
        if typed.ends_with('\n') || Instant::now() > deadline {
            break typed;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        typed,
        format!("{text}\n"),
        "xterm received other characters"
    );
    let message = error_text(&mut mcp, "type", json!({"text": "café ü"}));
    assert!(
        message.contains("X11 window") && message.contains("\"éü\""),
        "{message}"
    );
    let message = error_text(&mut mcp, "key", json!({"keys": "ctrl+é"}));
    assert!(message.contains("X11 window"), "{message}");
    std::fs::remove_dir_all(&out).unwrap();

    mcp.call("stop_desktop", json!({}));
    assert!(!running(xterm), "xterm outlived stop_desktop");
}
