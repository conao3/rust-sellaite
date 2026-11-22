use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};

use crate::desktop::Desktop;
use crate::input::Button;
use crate::{approval, chord};

pub(crate) fn coord_from(value: &Value, key: &str) -> Result<u32, String> {
    value[key]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("{key} must be a non-negative integer"))
}

const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DEFAULT_WINDOW_TIMEOUT_MS: u64 = 10_000;
const MAX_WINDOW_TIMEOUT_MS: u64 = 60_000;
const MAX_WAIT_MS: u64 = 30_000;

pub const NOTE_MARKER: &str = "[sellaite] Note from the user";

const INSTRUCTIONS: &str = "While a desktop runs, the user watches it in the sellaite dashboard and can send you a short note. sellaite appends each note, exactly once, as its own text item to the result of your next tool call, after the tool's own output, starting with \"[sellaite] Note from the user\". A result that carries notes has no structured content, so that clients show its text items. The note comes from the user who runs you: read it and take it into account before your next step, as if the user had said it in the conversation.";

#[derive(Default)]
pub struct Server {
    desktop: Option<Desktop>,
}

fn tools() -> Value {
    json!([
        {
            "name": "start_desktop",
            "description": "Start the agent's own headless desktop (sway) with an agent seat and a user seat. The agent's clicks and keystrokes go only to the agent seat. The user watches the desktop in the sellaite dashboard and can pause the agent; while it is paused, launch, click, type and key return an error. The user can also send a note from the dashboard: it is appended once to the result of the next tool call as a text item starting with \"[sellaite] Note from the user\"; take it into account before the next step. Open applications in it with launch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "width": {"type": "integer", "minimum": 1, "description": "Width in pixels (default 1280)"},
                    "height": {"type": "integer", "minimum": 1, "description": "Height in pixels (default 720)"}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "stop_desktop",
            "description": "Stop the desktop and every application running in it.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "launch",
            "description": "Start a program inside the desktop, detached, with the desktop's WAYLAND_DISPLAY, XDG_RUNTIME_DIR and SWAYSOCK. stop_desktop stops it and every process it started. With wait_for_window, returns after a window of the program (or of a process it started) has mapped; a new window gets the agent seat's keyboard focus, so type and key can follow without a click. Returns Pid and, when waited, Window (Id, AppId, Pid, Name).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "The program and its arguments, such as [\"foot\"]; no shell is involved"},
                    "wait_for_window": {"type": "boolean", "description": "Wait until a window has mapped (default false)"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_WINDOW_TIMEOUT_MS, "description": "How long to wait for the window (default 10000)"}
                },
                "required": ["argv"],
                "additionalProperties": false
            }
        },
        {
            "name": "wait",
            "description": "Sleep for the given number of milliseconds (at most 30000), for example to let an application finish drawing before a screenshot.",
            "inputSchema": {
                "type": "object",
                "properties": {"milliseconds": {"type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS}},
                "required": ["milliseconds"],
                "additionalProperties": false
            }
        },
        {
            "name": "wait_for_stable_screen",
            "description": "Wait until sampled desktop screenshots match for quiet_ms. Use after a window maps or an action starts drawing. Increase quiet_ms for slow startup; this does not detect a shell prompt specifically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "quiet_ms": {"type": "integer", "minimum": 100, "maximum": MAX_WAIT_MS, "default": 500},
                    "timeout_ms": {"type": "integer", "minimum": 100, "maximum": MAX_WAIT_MS, "default": 10000}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "screenshot",
            "description": "Capture the whole desktop as a PNG image.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "click",
            "description": "Move the agent's pointer to (x, y) and click. Only the agent seat's pointer and focus move.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": {"type": "integer", "minimum": 0},
                    "y": {"type": "integer", "minimum": 0},
                    "button": {"type": "string", "enum": ["left", "right", "middle"], "description": "Default left"},
                    "intent": {"type": "string", "enum": ["ordinary", "send", "submit", "purchase", "password", "navigate"], "description": "Declare sensitive actions, including clicking links that leave the allowed domains"},
                    "domain": {"type": "string", "description": "Destination hostname when intent is navigate"}
                },
                "required": ["x", "y"],
                "additionalProperties": false
            }
        },
        {
            "name": "type",
            "description": "Type text into the window that has the agent seat's keyboard focus.",
            "inputSchema": {
                "type": "object",
                "properties": {"text": {"type": "string"},
                    "intent": {"type": "string", "enum": ["ordinary", "send", "submit", "purchase", "password", "navigate"], "description": "Declare sensitive actions, including clicking links that leave the allowed domains"},
                    "domain": {"type": "string", "description": "Destination hostname when intent is navigate"}},
                "required": ["text"],
                "additionalProperties": false
            }
        },
        {
            "name": "key",
            "description": "Press a key or a chord on the agent seat's keyboard, such as Return, ctrl+l or alt+Tab. Modifiers (shift, ctrl, alt, super) are held as modifier state while the key is pressed. The key is one character or Return, Tab, Escape, BackSpace, Delete, Insert, Home, End, Page_Up, Page_Down, Left, Right, Up, Down, space, plus, Menu, Print or F1 to F24.",
            "inputSchema": {
                "type": "object",
                "properties": {"keys": {"type": "string", "description": "Modifiers and a key joined by +"}},
                "required": ["keys"],
                "additionalProperties": false
            }
        }
    ])
}

fn text_result(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn error_result(message: String) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

fn note_text(text: &str) -> String {
    format!(
        "{NOTE_MARKER} (sent from the sellaite dashboard while you were working; shown once):\n{text}"
    )
}

fn bounded(args: &Value, key: &str, default: Option<u64>, max: u64) -> Result<u64, String> {
    let value = match args.get(key) {
        None | Some(Value::Null) => default.ok_or_else(|| format!("{key} is required"))?,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| format!("{key} must be a non-negative integer"))?,
    };
    if value > max {
        return Err(format!("{key} must be at most {max}"));
    }
    Ok(value)
}

fn dimension(args: &Value, key: &str, default: u32) -> Result<u32, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("{key} must be a non-negative integer")),
    }
}

impl Server {
    fn take_notes(&self) -> Vec<String> {
        self.desktop
            .as_ref()
            .map(Desktop::take_notes)
            .unwrap_or_default()
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Value {
        let mut notes = if name == "stop_desktop" {
            self.take_notes()
        } else {
            Vec::new()
        };
        let mut result = self.call(name, args).unwrap_or_else(error_result);
        notes.extend(self.take_notes());
        if notes.is_empty() {
            return result;
        }
        let notes: Vec<String> = notes.iter().map(|n| note_text(n)).collect();
        if let Some(content) = result["content"].as_array_mut() {
            content.extend(notes.iter().map(|n| json!({"type": "text", "text": n})));
        }
        if let Some(object) = result.as_object_mut() {
            object.remove("structuredContent");
        }
        result
    }

    fn desktop(&mut self) -> Result<&mut Desktop, String> {
        self.desktop
            .as_mut()
            .ok_or_else(|| "no desktop is running; call start_desktop first".to_string())
    }

    fn input_desktop(&mut self) -> Result<&mut Desktop, String> {
        let desktop = self.desktop()?;
        match desktop.paused() {
            Some(reason) => Err(format!(
                "the user paused the agent ({reason}); do not retry until the user resumes it from the sellaite dashboard"
            )),
            None => Ok(desktop),
        }
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "start_desktop" => {
                if self.desktop.is_some() {
                    return Err("a desktop is already running; call stop_desktop first".into());
                }
                let width = dimension(args, "width", DEFAULT_WIDTH)?;
                let height = dimension(args, "height", DEFAULT_HEIGHT)?;
                let desktop = Desktop::start(width, height)?;
                let info = desktop.info();
                self.desktop = Some(desktop);
                let body = json!({
                    "Id": info.id,
                    "Width": info.width,
                    "Height": info.height,
                    "XdgRuntimeDir": info.runtime_dir,
                    "WaylandDisplay": info.wayland_display,
                    "Swaysock": info.swaysock,
                    "VncSocket": info.vnc_socket,
                });
                let mut result = text_result(body.to_string());
                result["structuredContent"] = body;
                Ok(result)
            }
            "stop_desktop" => {
                self.desktop
                    .take()
                    .ok_or_else(|| "no desktop is running".to_string())?;
                Ok(text_result("stopped".into()))
            }
            "launch" => {
                let argv: Vec<String> = args
                    .get("argv")
                    .and_then(Value::as_array)
                    .and_then(|items| {
                        items
                            .iter()
                            .map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .ok_or_else(|| "argv must be an array of strings".to_string())?;
                let wait = match args.get("wait_for_window") {
                    None | Some(Value::Null) => false,
                    Some(v) => v
                        .as_bool()
                        .ok_or_else(|| "wait_for_window must be a boolean".to_string())?,
                };
                let timeout = bounded(
                    args,
                    "timeout_ms",
                    Some(DEFAULT_WINDOW_TIMEOUT_MS),
                    MAX_WINDOW_TIMEOUT_MS,
                )?;
                if timeout == 0 {
                    return Err("timeout_ms must be positive".into());
                }
                let launched = self
                    .input_desktop()?
                    .launch(&argv, wait.then(|| Duration::from_millis(timeout)))?;
                let mut body = json!({"Pid": launched.pid, "Log": launched.log});
                if let Some(window) = launched.window {
                    body["Window"] = json!({
                        "Id": window["id"],
                        "AppId": window["app_id"],
                        "Pid": window["pid"],
                        "Name": window["name"],
                    });
                }
                let mut result = text_result(body.to_string());
                result["structuredContent"] = body;
                Ok(result)
            }
            "wait" => {
                let ms = bounded(args, "milliseconds", None, MAX_WAIT_MS)?;
                std::thread::sleep(Duration::from_millis(ms));
                Ok(text_result(format!("waited {ms} ms")))
            }
            "wait_for_stable_screen" => {
                let quiet = bounded(args, "quiet_ms", Some(500), MAX_WAIT_MS)?;
                let timeout = bounded(args, "timeout_ms", Some(10_000), MAX_WAIT_MS)?;
                if quiet < 100 || timeout <= quiet {
                    return Err(
                        "quiet_ms must be at least 100 and timeout_ms must exceed quiet_ms".into(),
                    );
                }
                let desktop = self.desktop()?;
                let start = Instant::now();
                let deadline = start + Duration::from_millis(timeout);
                let mut previous = desktop.screenshot()?;
                let mut unchanged_since = Instant::now();
                loop {
                    std::thread::sleep(
                        Duration::from_millis(100)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                    let current = desktop.screenshot()?;
                    let now = Instant::now();
                    if now > deadline {
                        return Err(format!("screen did not settle within {timeout} ms"));
                    }
                    if current != previous {
                        previous = current;
                        unchanged_since = now;
                    } else if now.duration_since(unchanged_since) >= Duration::from_millis(quiet) {
                        return Ok(text_result(format!(
                            "screen settled in {} ms",
                            start.elapsed().as_millis()
                        )));
                    }
                }
            }
            "screenshot" => {
                let png = self.desktop()?.screenshot()?;
                let data = base64::engine::general_purpose::STANDARD.encode(png);
                Ok(json!({"content": [{"type": "image", "data": data, "mimeType": "image/png"}]}))
            }
            "click" => {
                let (x, y) = (coord_from(args, "x")?, coord_from(args, "y")?);
                let button = match args.get("button").and_then(Value::as_str) {
                    None => Button::Left,
                    Some(name) => Button::parse(name)?,
                };
                let desktop = self.input_desktop()?;
                let info = desktop.info();
                if x >= info.width || y >= info.height {
                    return Err("click is outside the desktop".into());
                }
                let decision = approval::check(desktop, name, args, Some((x, y)))?;
                let (x, y) = if decision["Action"] == "edit" {
                    (coord_from(&decision, "X")?, coord_from(&decision, "Y")?)
                } else {
                    (x, y)
                };
                desktop.click(x, y, button)?;
                Ok(text_result(format!("clicked at ({x}, {y})")))
            }
            "type" => {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "text must be a string".to_string())?;
                let desktop = self.input_desktop()?;
                let decision = approval::check(desktop, name, args, None)?;
                let text = decision["Text"]
                    .as_str()
                    .filter(|_| decision["Action"] == "edit")
                    .unwrap_or(text);
                desktop.type_text(text)?;
                Ok(text_result(format!(
                    "typed {} characters",
                    text.chars().count()
                )))
            }
            "key" => {
                let keys = args
                    .get("keys")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "keys must be a string".to_string())?;
                let chord = chord::parse(keys)?;
                self.input_desktop()?.key(&chord)?;
                Ok(text_result(format!("pressed {keys}")))
            }
            other => Err(format!("unknown tool {other:?}")),
        }
    }

    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id")?.clone();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str);
                let version = requested
                    .filter(|v| PROTOCOL_VERSIONS.contains(v))
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "sellaite", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(self.call_tool(name, &args))
            }
            other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        })
    }

    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(message) => self.handle(&message),
                Err(e) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": format!("parse error: {e}")}
                })),
            };
            if let Some(response) = response {
                writeln!(output, "{response}")?;
                output.flush()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_tools() {
        let mut server = Server::default();
        let response = server
            .handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .unwrap();
        let names: Vec<&str> = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "start_desktop",
                "stop_desktop",
                "launch",
                "wait",
                "wait_for_stable_screen",
                "screenshot",
                "click",
                "type",
                "key"
            ]
        );
    }

    #[test]
    fn input_tools_need_a_desktop() {
        let mut server = Server::default();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "click", "arguments": {"x": 1, "y": 1}}
            }))
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
    }

    #[test]
    fn wait_is_capped() {
        let mut server = Server::default();
        let mut call = |ms: u64| {
            server
                .handle(&json!({
                    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": "wait", "arguments": {"milliseconds": ms}}
                }))
                .unwrap()["result"]
                .clone()
        };
        assert_eq!(call(1)["content"][0]["text"], "waited 1 ms");
        let over = call(MAX_WAIT_MS + 1);
        assert_eq!(over["isError"], true, "{over}");
    }

    #[test]
    fn negotiates_the_protocol_version() {
        let mut server = Server::default();
        let version = |server: &mut Server, requested: &str| {
            server
                .handle(&json!({
                    "jsonrpc": "2.0", "id": 0, "method": "initialize",
                    "params": {"protocolVersion": requested}
                }))
                .unwrap()["result"]["protocolVersion"]
                .clone()
        };
        assert_eq!(version(&mut server, "2025-11-25"), "2025-11-25");
        assert_eq!(version(&mut server, "2025-06-18"), "2025-06-18");
        assert_eq!(version(&mut server, "2099-01-01"), "2025-11-25");
    }

    #[test]
    fn notifications_get_no_response() {
        let mut server = Server::default();
        assert!(
            server
                .handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .is_none()
        );
    }
}
