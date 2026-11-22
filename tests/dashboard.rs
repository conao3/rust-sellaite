mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use base64::Engine;
use serde_json::{Value, json};

use common::{Mcp, wait_for};

struct Dashboard {
    child: Child,
    host: String,
}

impl Dashboard {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sellaite"))
            .args(["dashboard", "serve", "--address", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the dashboard");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let url: Value = serde_json::from_str(&line).expect("the dashboard URL");
        let host = url["Url"]
            .as_str()
            .unwrap()
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        Self { child, host }
    }

    fn send(&self, head: &str) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.host).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("an HTTP response");
        let status = std::str::from_utf8(&response[9..12])
            .unwrap()
            .parse()
            .unwrap();
        (status, response[split + 4..].to_vec())
    }

    fn get(&self, path: &str) -> (u16, Vec<u8>) {
        self.send(&format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.host
        ))
    }

    fn post(&self, path: &str, origin: &str) -> u16 {
        self.send(&format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nOrigin: {origin}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            self.host
        ))
        .0
    }

    fn post_json(&self, path: &str, origin: &str, body: &Value) -> (u16, Value) {
        let body = body.to_string();
        let (status, response) = self.send(&format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.host,
            body.len()
        ));
        (status, serde_json::from_slice(&response).unwrap())
    }

    fn origin(&self) -> String {
        format!("http://{}", self.host)
    }

    fn desktop(&self, id: &str) -> Option<Value> {
        let (status, body) = self.get("/api/desktops");
        assert_eq!(status, 200);
        let list: Value = serde_json::from_slice(&body).unwrap();
        list["Desktops"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["Id"] == id)
            .cloned()
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn vnc_client(socket: &Path) -> UnixStream {
    let mut vnc = UnixStream::connect(socket).expect("connect to wayvnc");
    let mut version = [0u8; 12];
    vnc.read_exact(&mut version).unwrap();
    assert_eq!(&version, b"RFB 003.008\n");
    vnc.write_all(b"RFB 003.008\n").unwrap();
    let mut count = [0u8; 1];
    vnc.read_exact(&mut count).unwrap();
    let mut types = vec![0u8; usize::from(count[0])];
    vnc.read_exact(&mut types).unwrap();
    assert!(types.contains(&1), "wayvnc offers no security type None");
    vnc.write_all(&[1]).unwrap();
    let mut result = [0u8; 4];
    vnc.read_exact(&mut result).unwrap();
    assert_eq!(result, [0, 0, 0, 0]);
    vnc.write_all(&[1]).unwrap();
    let mut init = [0u8; 24];
    vnc.read_exact(&mut init).unwrap();
    let name_len = u32::from_be_bytes(init[20..24].try_into().unwrap());
    let mut name = vec![0u8; name_len as usize];
    vnc.read_exact(&mut name).unwrap();
    vnc
}

fn pointer_event(vnc: &mut UnixStream, x: u16, y: u16) {
    let mut message = vec![5u8, 0];
    message.extend_from_slice(&x.to_be_bytes());
    message.extend_from_slice(&y.to_be_bytes());
    vnc.write_all(&message).unwrap();
}

fn fake_runtime_dir(name: &str, state: Option<Value>, age: Duration) -> PathBuf {
    let dir = PathBuf::from("/tmp").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sway.config"), "").unwrap();
    if let Some(state) = state {
        std::fs::write(dir.join("sellaite.json"), state.to_string()).unwrap();
    }
    std::fs::File::open(&dir)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
    dir
}

fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn is_error(result: &Value) -> bool {
    result["isError"] == true
}

#[test]
fn the_user_seat_pauses_the_agent_and_the_dashboard_resumes_it() {
    let tag = format!("{:06}", std::process::id() % 1_000_000);
    let hour = Duration::from_secs(3600);
    let dead = fake_runtime_dir(
        &format!("sel.d{}", &tag[1..]),
        Some(json!({"Pid": dead_pid(), "Width": 1, "Height": 1, "WaylandDisplay": "wayland-1"})),
        Duration::ZERO,
    );
    let legacy = fake_runtime_dir(&format!("sel.l{}", &tag[1..]), None, hour);
    let starting = fake_runtime_dir(&format!("sel.s{}", &tag[1..]), None, Duration::ZERO);
    let live = fake_runtime_dir(
        &format!("sel.a{}", &tag[1..]),
        Some(json!({"Pid": std::process::id(), "Width": 1, "Height": 1, "WaylandDisplay": null})),
        hour,
    );

    let mut mcp = Mcp::spawn();
    assert!(!dead.exists(), "the runtime dir of a dead run was kept");
    assert!(
        !legacy.exists(),
        "an old runtime dir with no live sway was kept"
    );
    assert!(
        starting.exists(),
        "a runtime dir that is still starting was removed"
    );
    assert!(live.exists(), "the runtime dir of a live run was removed");
    std::fs::remove_dir_all(&starting).unwrap();
    std::fs::remove_dir_all(&live).unwrap();

    let info = mcp.call("start_desktop", json!({"width": 800, "height": 600}))["structuredContent"]
        .clone();
    let id = info["Id"].as_str().unwrap().to_string();
    let vnc_socket = PathBuf::from(info["VncSocket"].as_str().unwrap());
    let dashboard = Dashboard::spawn();

    let (status, page) = dashboard.get("/");
    assert_eq!(status, 200);
    assert!(
        String::from_utf8(page)
            .unwrap()
            .contains("/novnc/core/rfb.js")
    );
    assert_eq!(dashboard.get("/novnc/core/rfb.js").0, 200);
    assert_eq!(dashboard.get("/novnc/../sellaite.json").0, 404);
    let forged = dashboard.send(&format!(
        "GET /api/desktops HTTP/1.1\r\nHost: evil.example:{}\r\nConnection: close\r\n\r\n",
        dashboard.host.rsplit(':').next().unwrap()
    ));
    assert_eq!(forged.0, 403, "a request with a foreign Host was served");

    wait_for("the user seat to go idle", || {
        dashboard.desktop(&id).filter(|d| d["UserSeatIdle"] == true)
    });
    mcp.call("click", json!({"x": 400, "y": 300}));
    mcp.call("type", json!({"text": "agent"}));
    mcp.call("key", json!({"keys": "ctrl+l"}));
    std::thread::sleep(Duration::from_millis(1500));
    let desktop = dashboard.desktop(&id).unwrap();
    assert_eq!(
        desktop["Paused"], false,
        "agent input paused the agent: {desktop}"
    );
    assert_eq!(
        desktop["UserSeatIdle"], true,
        "agent input woke the user seat"
    );

    let mut vnc = vnc_client(&vnc_socket);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        dashboard.desktop(&id).unwrap()["Paused"],
        false,
        "a VNC client that sent nothing paused the agent"
    );
    pointer_event(&mut vnc, 100, 100);
    let desktop = wait_for("the user's VNC input to pause the agent", || {
        dashboard.desktop(&id).filter(|d| d["Paused"] == true)
    });
    assert!(
        desktop["PausedReason"].as_str().unwrap().contains("seat0"),
        "{desktop}"
    );
    for (tool, arguments) in [
        ("click", json!({"x": 1, "y": 1})),
        ("type", json!({"text": "x"})),
        ("key", json!({"keys": "Return"})),
    ] {
        let result = mcp.request("tools/call", json!({"name": tool, "arguments": arguments}));
        assert!(is_error(&result), "{tool} ran while paused: {result}");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("the user paused the agent"),
            "{result}"
        );
    }
    let shot = mcp.call("screenshot", json!({}));
    assert_eq!(shot["content"][0]["type"], "image");

    let resume = format!("/api/desktops/{id}/resume");
    let pause = format!("/api/desktops/{id}/pause");
    assert_eq!(dashboard.post(&resume, "http://evil.example"), 403);
    assert_eq!(dashboard.desktop(&id).unwrap()["Paused"], true);
    assert_eq!(dashboard.post(&resume, &dashboard.origin()), 200);
    assert_eq!(dashboard.desktop(&id).unwrap()["Paused"], false);
    mcp.call("click", json!({"x": 400, "y": 300}));

    assert_eq!(dashboard.post(&pause, &dashboard.origin()), 200);
    let result = mcp.request(
        "tools/call",
        json!({"name": "click", "arguments": {"x": 1, "y": 1}}),
    );
    assert!(is_error(&result), "click ran after Stop: {result}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("dashboard"),
        "{result}"
    );
    assert_eq!(dashboard.post(&resume, &dashboard.origin()), 200);
    mcp.call("type", json!({"text": "again"}));
    drop(vnc);

    let key = base64::engine::general_purpose::STANDARD.encode(b"the sample nonce");
    let mut ws = TcpStream::connect(&dashboard.host).unwrap();
    write!(
        ws,
        "GET /api/desktops/{id}/vnc HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n",
        dashboard.host,
        dashboard.origin()
    )
    .unwrap();
    let mut reader = BufReader::new(ws.try_clone().unwrap());
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    assert!(status.starts_with("HTTP/1.1 101"), "{status}");
    let mut accepted = false;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        accepted |= line.trim() == "Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        if line.trim().is_empty() {
            break;
        }
    }
    assert!(accepted);
    let mut frame = [0u8; 14];
    reader.read_exact(&mut frame).unwrap();
    assert_eq!(frame[0], 0x82);
    assert_eq!(frame[1], 12);
    assert_eq!(&frame[2..], b"RFB 003.008\n");
    drop(reader);
    drop(ws);

    mcp.call("stop_desktop", json!({}));
    assert!(
        dashboard.desktop(&id).is_none(),
        "a stopped desktop is listed"
    );
}

const NOTE_MARKER: &str = "[sellaite] Note from the user";

fn notes_in(result: &Value) -> Vec<String> {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["text"].as_str())
        .filter(|text| text.starts_with(NOTE_MARKER))
        .map(|text| text.split_once('\n').unwrap().1.to_string())
        .collect()
}

fn create_note(id: &str, text: &str) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_sellaite"))
        .args(["desktop", "create-note", "--desktop-id", id, "--text", text])
        .output()
        .unwrap();
    let json = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    (output.status.code().unwrap(), json)
}

#[test]
fn a_note_reaches_the_next_tool_call_of_its_desktop_once() {
    let mut first = Mcp::spawn();
    let mut second = Mcp::spawn();
    let start = |mcp: &mut Mcp| {
        mcp.call("start_desktop", json!({"width": 640, "height": 480}))["structuredContent"]["Id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (id, other) = (start(&mut first), start(&mut second));
    let initialize = first.request("initialize", json!({"protocolVersion": "2025-11-25"}));
    assert!(
        initialize["instructions"]
            .as_str()
            .unwrap()
            .contains(NOTE_MARKER)
    );
    let dashboard = Dashboard::spawn();

    let (code, desktop) = create_note(&id, "from the CLI");
    assert_eq!(code, 0);
    assert_eq!(desktop["PendingNotes"], 1, "{desktop}");
    let file = std::env::temp_dir().join(format!("sellaite-note-{}", std::process::id()));
    std::fs::write(&file, "from a file\n").unwrap();
    assert_eq!(create_note(&id, &format!("file://{}", file.display())).0, 0);
    std::fs::remove_file(&file).unwrap();
    let notes = format!("/api/desktops/{id}/notes");
    let (status, desktop) = dashboard.post_json(
        &notes,
        &dashboard.origin(),
        &json!({"Text": "from the dashboard"}),
    );
    assert_eq!(status, 200, "{desktop}");
    assert_eq!(desktop["PendingNotes"], 3, "{desktop}");
    let (status, _) =
        dashboard.post_json(&notes, "http://evil.example", &json!({"Text": "forged"}));
    assert_eq!(status, 403, "a note from a foreign Origin was accepted");
    let (status, _) = dashboard.post_json(&notes, &dashboard.origin(), &json!({"Text": "  "}));
    assert_eq!(status, 400, "an empty note was accepted");
    assert_eq!(create_note(&id, "").0, 252);
    assert_eq!(create_note("Zz9999", "to nobody").0, 254);
    assert_eq!(dashboard.desktop(&id).unwrap()["PendingNotes"], 3);
    assert_eq!(dashboard.desktop(&other).unwrap()["PendingNotes"], 0);

    let elsewhere = second.call("wait", json!({"milliseconds": 0}));
    assert!(
        notes_in(&elsewhere).is_empty(),
        "another desktop's agent got the note: {elsewhere}"
    );
    let result = first.call("wait", json!({"milliseconds": 0}));
    assert_eq!(result["content"][0]["text"], "waited 0 ms");
    assert_eq!(
        notes_in(&result),
        ["from the CLI", "from a file", "from the dashboard"],
        "{result}"
    );
    assert_eq!(dashboard.desktop(&id).unwrap()["PendingNotes"], 0);
    let again = first.call("wait", json!({"milliseconds": 0}));
    assert!(notes_in(&again).is_empty(), "a note arrived twice: {again}");

    let pause = format!("/api/desktops/{id}/pause");
    assert_eq!(dashboard.post(&pause, &dashboard.origin()), 200);
    assert_eq!(create_note(&id, "while paused").0, 0);
    let refused = first.request(
        "tools/call",
        json!({"name": "click", "arguments": {"x": 1, "y": 1}}),
    );
    assert!(is_error(&refused), "{refused}");
    assert_eq!(notes_in(&refused), ["while paused"], "{refused}");
    assert_eq!(
        dashboard.post(&format!("/api/desktops/{id}/resume"), &dashboard.origin()),
        200
    );

    assert_eq!(create_note(&id, "before a screenshot").0, 0);
    let shot = first.call("screenshot", json!({}));
    assert_eq!(shot["content"][0]["type"], "image");
    assert_eq!(notes_in(&shot), ["before a screenshot"]);

    assert_eq!(create_note(&id, "while launching").0, 0);
    let launched = first.call("launch", json!({"argv": ["true"]}));
    assert_eq!(notes_in(&launched), ["while launching"]);
    assert!(
        launched.get("structuredContent").is_none(),
        "a client that shows structuredContent would hide the note: {launched}"
    );
    assert!(
        first
            .call("launch", json!({"argv": ["true"]}))
            .get("structuredContent")
            .is_some()
    );

    assert_eq!(create_note(&id, "at the end").0, 0);
    let stopped = first.call("stop_desktop", json!({}));
    assert_eq!(notes_in(&stopped), ["at the end"], "{stopped}");
    second.call("stop_desktop", json!({}));
}
