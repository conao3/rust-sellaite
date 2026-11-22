mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use base64::Engine;
use serde_json::{Value, json};

use common::{Mcp, wait_for};

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
    let many: String = (0..300u32)
        .map(|i| char::from_u32(0x4e00 + i).unwrap())
        .collect();
    let text = format!("Agent-Typed: été, 日本!\t{many}\n");
    mcp.call("type", json!({"text": text}));
    wait_for("the text in termB", || {
        (std::fs::read_to_string(&log_b).unwrap() == text).then_some(())
    });

    assert_eq!(
        sway.seat_focus("seat0"),
        id_a,
        "the user seat's focus moved"
    );
    assert_eq!(sway.seat_focus("agent"), id_b);
    assert_eq!(std::fs::read_to_string(&log_a).unwrap(), "");

    mcp.call("key", json!({"keys": "ctrl+l"}));
    mcp.call("key", json!({"keys": "Return"}));
    mcp.call("type", json!({"text": "z"}));
    let text = format!("{text}\x0c\nz");
    wait_for("ctrl+l, Return and z in termB", || {
        (std::fs::read_to_string(&log_b).unwrap() == text).then_some(())
    });
    let unknown = mcp.request(
        "tools/call",
        json!({"name": "key", "arguments": {"keys": "hyper+l"}}),
    );
    assert_eq!(unknown["isError"], true, "{unknown}");
    assert_eq!(sway.seat_focus("seat0"), id_a);
    assert_eq!(sway.seat_focus("agent"), id_b);
    assert_eq!(std::fs::read_to_string(&log_a).unwrap(), "");

    assert!(
        b["rect"]["x"].as_i64().unwrap() < a["rect"]["x"].as_i64().unwrap(),
        "termB is not left of termA"
    );
    sway.msg(&["bindsym Mod1+Tab focus right"]);
    mcp.call("key", json!({"keys": "alt+Tab"}));
    wait_for("alt+Tab to move the agent seat to termA", || {
        (sway.seat_focus("agent") == id_a).then_some(())
    });
    assert_eq!(
        sway.seat_focus("seat0"),
        id_a,
        "alt+Tab on the agent seat moved the user seat"
    );
    mcp.call("type", json!({"text": "a"}));
    wait_for("a in termA", || {
        (std::fs::read_to_string(&log_a).unwrap() == "a").then_some(())
    });
    assert_eq!(std::fs::read_to_string(&log_b).unwrap(), text);

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
