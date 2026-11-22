use std::io::{BufRead, Write};

use base64::Engine;
use serde_json::{Value, json};

use crate::chord;
use crate::desktop::Desktop;
use crate::input::Button;

const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;

#[derive(Default)]
pub struct Server {
    desktop: Option<Desktop>,
}

fn tools() -> Value {
    json!([
        {
            "name": "start_desktop",
            "description": "Start the agent's own headless desktop (sway) with an agent seat and a user seat. The agent's clicks and keystrokes go only to the agent seat. The result gives WAYLAND_DISPLAY, XDG_RUNTIME_DIR and SWAYSOCK for launching applications inside the desktop.",
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
                    "button": {"type": "string", "enum": ["left", "right", "middle"], "description": "Default left"}
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
                "properties": {"text": {"type": "string"}},
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
    fn desktop(&mut self) -> Result<&mut Desktop, String> {
        self.desktop
            .as_mut()
            .ok_or_else(|| "no desktop is running; call start_desktop first".to_string())
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
                    "Width": info.width,
                    "Height": info.height,
                    "XdgRuntimeDir": info.runtime_dir,
                    "WaylandDisplay": info.wayland_display,
                    "Swaysock": info.swaysock,
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
            "screenshot" => {
                let png = self.desktop()?.screenshot()?;
                let data = base64::engine::general_purpose::STANDARD.encode(png);
                Ok(json!({"content": [{"type": "image", "data": data, "mimeType": "image/png"}]}))
            }
            "click" => {
                let coord = |key: &str| {
                    args.get(key)
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| format!("{key} must be a non-negative integer"))
                };
                let (x, y) = (coord("x")?, coord("y")?);
                let button = match args.get("button").and_then(Value::as_str) {
                    None => Button::Left,
                    Some(name) => Button::parse(name)?,
                };
                self.desktop()?.click(x, y, button)?;
                Ok(text_result(format!("clicked at ({x}, {y})")))
            }
            "type" => {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "text must be a string".to_string())?;
                self.desktop()?.type_text(text)?;
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
                self.desktop()?.key(&chord)?;
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
                    "serverInfo": {"name": "sellaite", "version": env!("CARGO_PKG_VERSION")}
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(self.call(name, &args).unwrap_or_else(error_result))
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
