use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::desktop::Desktop;

const PENDING: &str = "approval.json";
const TIMEOUT: Duration = Duration::from_secs(60);

pub fn pending(dir: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(dir.join(PENDING)).ok()?).ok()
}

pub fn decide(dir: &Path, id: &str, decision: &Value) -> Result<(), String> {
    let request = pending(dir)
        .filter(|request| request["Id"].as_str() == Some(id))
        .ok_or("approval request is no longer pending")?;
    match decision["Action"].as_str() {
        Some("approve" | "reject") => {}
        Some("edit") if request["Tool"] == "type" && decision["Text"].is_string() => {}
        Some("edit") if request["Tool"] == "click" => {
            let state = crate::runtime::read_state(dir).ok_or("desktop unavailable")?;
            if !decision["X"]
                .as_u64()
                .is_some_and(|x| x < u64::from(state.width))
                || !decision["Y"]
                    .as_u64()
                    .is_some_and(|y| y < u64::from(state.height))
            {
                return Err("edited coordinates are outside the desktop".into());
            }
        }
        _ => return Err("invalid approval decision".into()),
    }
    let decision_dir = dir.join(format!("approval-{id}"));
    let lock = decision_dir.join("lock");
    // Keep the reservation in place until the MCP server removes the directory.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|e| e.to_string())?;
    let result = crate::runtime::write_atomically(
        &decision_dir.join("decision"),
        decision.to_string().as_bytes(),
    );
    if result.is_err() {
        let _ = fs::remove_file(lock);
    }
    result
}

pub fn check(
    desktop: &Desktop,
    tool: &str,
    args: &Value,
    point: Option<(u32, u32)>,
) -> Result<Value, String> {
    let intent = args
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or("ordinary");
    let domain = args["domain"].as_str().unwrap_or("");
    let target = accessibility(desktop, point)?;
    let role = target["Role"].as_str().unwrap_or("");
    let name = target["Name"].as_str().unwrap_or("");
    let reason = match intent {
        "send" | "submit" | "purchase" | "password" => intent,
        "navigate" if !allowed_domain(domain) => "navigation outside the allow list",
        "ordinary" | "navigate" if role == "password text" && tool == "type" => "password field",
        "ordinary" | "navigate" if tool == "click" && risky_button(role, name) => {
            "sensitive button"
        }
        "ordinary" | "navigate" => return Ok(Value::Null),
        _ => return Err("unknown action intent".into()),
    };
    let info = desktop.info();
    let dir = &info.runtime_dir;
    let id = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let bounds = match point {
        Some((x, y)) => {
            json!({"X": x.saturating_sub(12), "Y": y.saturating_sub(12), "Width": 24, "Height": 24})
        }
        None => json!({"X": 0, "Y": 0, "Width": info.width, "Height": info.height}),
    };
    let request = json!({
        "Id": id, "Tool": tool, "Reason": reason,
        "Domain": if intent == "navigate" { Some(domain) } else { None },
        "Target": {"Role": role, "Name": name},
        "Bounds": target.get("Bounds").unwrap_or(&bounds),
        "X": point.map(|(x, _)| x), "Y": point.map(|(_, y)| y),
    });
    let screen = desktop.screenshot()?;
    let path = dir.join(PENDING);
    let decision_dir = dir.join(format!("approval-{id}"));
    let response = decision_dir.join("decision");
    fs::create_dir(&decision_dir).map_err(|e| e.to_string())?;
    if let Err(error) = crate::runtime::write_atomically(&path, request.to_string().as_bytes()) {
        let _ = fs::remove_dir_all(&decision_dir);
        return Err(error);
    }
    let started = Instant::now();
    let result = loop {
        if let Some(reason) = desktop.paused() {
            break Err(format!("approval cancelled: {reason}"));
        }
        if started.elapsed() >= TIMEOUT {
            break Err("approval timed out".into());
        }
        if let Ok(bytes) = fs::read(&response) {
            break serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(decision_dir);
    let decision = result?;
    if decision["Action"] == "reject" {
        return Err("the user rejected the action".into());
    }
    if desktop.screenshot()? != screen || desktop.paused().is_some() {
        return Err("desktop changed during approval; take a new screenshot".into());
    }
    Ok(decision)
}

fn allowed_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        && std::env::var("SELLAITE_ALLOWED_DOMAINS")
            .unwrap_or_default()
            .split(',')
            .any(|entry| entry.trim().eq_ignore_ascii_case(domain))
}

fn risky_button(role: &str, name: &str) -> bool {
    matches!(role, "push button" | "toggle button" | "button")
        && name.split(|c: char| !c.is_ascii_alphabetic()).any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "send" | "submit" | "purchase" | "buy" | "pay" | "checkout"
            )
        })
}

fn accessibility(desktop: &Desktop, point: Option<(u32, u32)>) -> Result<Value, String> {
    let Some(library) = std::env::var_os("SELLAITE_ATSPI_LIBRARY") else {
        return Ok(Value::Null);
    };
    let info = desktop.info();
    let coords = point.map(|(x, y)| format!("{x},{y}")).unwrap_or_default();
    let output = std::process::Command::new("timeout")
        .args([
            "0.75s",
            "python3",
            "-c",
            include_str!("approval_target.py"),
            &info.id,
            &coords,
        ])
        .output()
        .map_err(|e| format!("cannot run timeout (package: coreutils): {e}"))?;
    match output.status.code() {
        Some(0) => serde_json::from_slice(&output.stdout).map_err(|e| e.to_string()),
        Some(126 | 127) => Err(format!(
            "cannot run python3 (Ubuntu package: python3, Arch package: python): {}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        )),
        Some(78) => Err(format!(
            "cannot load libatspi from SELLAITE_ATSPI_LIBRARY={}; install libatspi2.0-0t64 (Ubuntu) or at-spi2-core (Arch): {}",
            library.to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        )),
        _ => Ok(Value::Null),
    }
}
