use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sellaite"))
            .args(["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start sellaite");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let init = mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "two-seats-test", "version": "0"}
            }),
        );
        assert_eq!(init["serverInfo"]["name"], "sellaite");
        writeln!(
            mcp.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .unwrap();
        mcp
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).expect("a JSON-RPC response");
        assert_eq!(response["id"], id);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        assert_ne!(result["isError"], true, "{tool} failed: {result}");
        result
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Sway {
    swaysock: PathBuf,
}

impl Sway {
    fn msg(&self, args: &[&str]) -> Value {
        let output = Command::new("swaymsg")
            .arg("--raw")
            .args(args)
            .env("SWAYSOCK", &self.swaysock)
            .output()
            .expect("run swaymsg");
        assert!(output.status.success(), "swaymsg {args:?} failed");
        serde_json::from_slice(&output.stdout).expect("swaymsg JSON")
    }

    fn windows(&self) -> Vec<Value> {
        fn walk(node: &Value, out: &mut Vec<Value>) {
            if node["app_id"].is_string() {
                out.push(node.clone());
            }
            for key in ["nodes", "floating_nodes"] {
                for child in node[key].as_array().into_iter().flatten() {
                    walk(child, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.msg(&["-t", "get_tree"]), &mut out);
        out
    }

    fn window(&self, app_id: &str) -> Option<Value> {
        self.windows().into_iter().find(|w| w["app_id"] == app_id)
    }

    fn seat_focus(&self, seat: &str) -> i64 {
        self.msg(&["-t", "get_seats"])
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == seat)
            .unwrap_or_else(|| panic!("no seat {seat}"))["focus"]
            .as_i64()
            .unwrap()
    }
}

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn terminal(info: &Value, app_id: &str, log: &Path) -> Child {
    Command::new("foot")
        .args(["--app-id", app_id, "sh", "-c"])
        .arg(format!("stty -icanon; exec cat > '{}'", log.display()))
        .env("XDG_RUNTIME_DIR", info["XdgRuntimeDir"].as_str().unwrap())
        .env("WAYLAND_DISPLAY", info["WaylandDisplay"].as_str().unwrap())
        .env_remove("DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start foot")
}

fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    let be = |b: &[u8]| u32::from_be_bytes(b.try_into().unwrap());
    (be(&png[16..20]), be(&png[20..24]))
}

/// The agent seat clicks and types into one terminal while the user seat keeps
/// its focus on the other one.
#[test]
fn agent_input_stays_on_the_agent_seat() {
    let logs = std::env::temp_dir().join(format!("sellaite-test-{}", std::process::id()));
    std::fs::create_dir_all(&logs).unwrap();
    let (log_a, log_b) = (logs.join("a.log"), logs.join("b.log"));

    let mut mcp = Mcp::spawn();
    let info =
        mcp.call("start_desktop", json!({"width": 1280, "height": 720}))["structuredContent"]
            .clone();
    let runtime_dir = PathBuf::from(info["XdgRuntimeDir"].as_str().unwrap());
    let sway = Sway {
        swaysock: PathBuf::from(info["Swaysock"].as_str().unwrap()),
    };

    // termA maps last, so both seats start focused on it.
    let mut term_b = terminal(&info, "termB", &log_b);
    wait_for("termB", || sway.window("termB"));
    let mut term_a = terminal(&info, "termA", &log_a);
    wait_for("termA", || sway.window("termA"));
    wait_for("both shells", || {
        (log_a.exists() && log_b.exists()).then_some(())
    });
    let a = sway.window("termA").unwrap();
    let b = sway.window("termB").unwrap();
    let (id_a, id_b) = (a["id"].as_i64().unwrap(), b["id"].as_i64().unwrap());

    wait_for("both seats to focus termA", || {
        (sway.seat_focus("seat0") == id_a && sway.seat_focus("agent") == id_a).then_some(())
    });

    let rect = &b["rect"];
    let x = rect["x"].as_u64().unwrap() + rect["width"].as_u64().unwrap() / 2;
    let y = rect["y"].as_u64().unwrap() + rect["height"].as_u64().unwrap() / 2;
    mcp.call("click", json!({"x": x, "y": y}));
    wait_for("the agent seat to focus termB", || {
        (sway.seat_focus("agent") == id_b).then_some(())
    });
    assert_eq!(
        sway.seat_focus("seat0"),
        id_a,
        "the agent's click moved the user seat"
    );
    mcp.call("type", json!({"text": "agent-typed"}));
    wait_for("the text in termB", || {
        (std::fs::read_to_string(&log_b).unwrap() == "agent-typed").then_some(())
    });

    assert_eq!(
        sway.seat_focus("seat0"),
        id_a,
        "the user seat's focus moved"
    );
    assert_eq!(sway.seat_focus("agent"), id_b);
    assert_eq!(std::fs::read_to_string(&log_a).unwrap(), "");

    let shot = mcp.call("screenshot", json!({}));
    let image = &shot["content"][0];
    assert_eq!(image["type"], "image");
    assert_eq!(image["mimeType"], "image/png");
    let png = base64::engine::general_purpose::STANDARD
        .decode(image["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(png_size(&png), (1280, 720));

    mcp.call("stop_desktop", json!({}));
    assert!(
        !runtime_dir.exists(),
        "the runtime directory was left behind"
    );
    let _ = term_a.kill();
    let _ = term_b.kill();
    let _ = term_a.wait();
    let _ = term_b.wait();
    let _ = std::fs::remove_dir_all(&logs);
}
