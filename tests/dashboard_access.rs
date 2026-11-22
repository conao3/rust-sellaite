use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const PUBLIC_HOST: &str = "box.example.ts.net:8731";
const PUBLIC_ORIGIN: &str = "https://box.example.ts.net:8731";
const LOGIN: &str = "owner@example.com";

struct Dashboard {
    child: Child,
    address: String,
}

impl Dashboard {
    fn spawn(extra: &[&str]) -> Self {
        let novnc = std::env::temp_dir().join(format!("sellaite-novnc-{}", std::process::id()));
        std::fs::create_dir_all(novnc.join("core")).unwrap();
        std::fs::write(novnc.join("core/rfb.js"), "").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_sellaite"))
            .args(["dashboard", "serve", "--address", "127.0.0.1:0"])
            .arg("--novnc-dir")
            .arg(&novnc)
            .args(extra)
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the dashboard");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let url: Value = serde_json::from_str(&line).expect("the dashboard URL");
        let address = url["Url"]
            .as_str()
            .unwrap()
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        Self { child, address }
    }

    fn status(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> u16 {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        std::str::from_utf8(&response[9..12])
            .unwrap()
            .parse()
            .unwrap()
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_public_origin_needs_an_allowed_tailscale_login() {
    let dashboard =
        Dashboard::spawn(&["--public-origins", PUBLIC_ORIGIN, "--allowed-logins", LOGIN]);
    let public = ("Host", PUBLIC_HOST);
    let owner = ("Tailscale-User-Login", LOGIN);
    let origin = ("Origin", PUBLIC_ORIGIN);

    assert_eq!(dashboard.status("GET", "/", &[public, owner]), 200);
    assert_eq!(dashboard.status("GET", "/health", &[public, owner]), 204);
    assert_eq!(dashboard.status("GET", "/", &[public]), 403);
    assert_eq!(
        dashboard.status(
            "GET",
            "/health",
            &[public, ("Tailscale-User-Login", "someone@example.com")]
        ),
        403
    );
    let local = dashboard.address.clone();
    assert_eq!(
        dashboard.status("GET", "/health", &[("Host", &local)]),
        403,
        "a loopback request without a login was served"
    );
    assert_eq!(
        dashboard.status("GET", "/", &[("Host", "evil.example:8731"), owner]),
        403
    );

    let stop = "/api/graphql";
    assert_eq!(dashboard.status("POST", stop, &[public, origin]), 403);
    assert_eq!(dashboard.status("POST", stop, &[public, owner]), 403);
    assert_eq!(
        dashboard.status(
            "POST",
            "/api/graphql",
            &[public, owner, ("Origin", "https://evil.example")]
        ),
        403
    );
    assert_eq!(
        dashboard.status("POST", stop, &[public, owner, origin]),
        415,
        "an authorized request did not reach GraphQL body extraction"
    );
    assert_eq!(
        dashboard.status(
            "GET",
            "/api/desktops/none/vnc",
            &[public, origin, ("Upgrade", "websocket")]
        ),
        403
    );
}

#[test]
fn public_origins_require_allowed_logins() {
    let status = Command::new(env!("CARGO_BIN_EXE_sellaite"))
        .args([
            "dashboard",
            "serve",
            "--novnc-dir",
            "/nonexistent",
            "--public-origins",
            PUBLIC_ORIGIN,
        ])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(252));
}
