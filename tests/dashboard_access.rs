use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;

const PUBLIC_HOST: &str = "box.example.ts.net:8731";
const PUBLIC_ORIGIN: &str = "https://box.example.ts.net:8731";
const LOGIN: &str = "owner@example.com";

struct Dashboard {
    child: Child,
    address: String,
    token: String,
}

impl Dashboard {
    fn spawn(extra: &[&str]) -> Self {
        Self::spawn_at("127.0.0.1:0", extra)
    }

    fn spawn_at(address: &str, extra: &[&str]) -> Self {
        let mut child = dashboard_command(address)
            .args(extra)
            .spawn()
            .expect("start the dashboard");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let url: Value = serde_json::from_str(&line).expect("the dashboard URL");
        let (address, token) = url["Url"]
            .as_str()
            .unwrap()
            .trim_start_matches("http://")
            .split_once("/?token=")
            .unwrap();
        if extra.contains(&"--public-origins") {
            assert_eq!(
                url["PublicUrls"][0],
                format!("{PUBLIC_ORIGIN}/?token={token}")
            );
        }
        Self {
            child,
            address: address.to_owned(),
            token: token.to_owned(),
        }
    }

    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let end = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&response[..end]).unwrap();
        (head[9..12].parse().unwrap(), head.to_owned())
    }

    fn status(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> u16 {
        self.request(method, path, headers).0
    }
}

fn dashboard_command(address: &str) -> Command {
    let novnc = std::env::temp_dir().join(format!("sellaite-novnc-{}", std::process::id()));
    fs::create_dir_all(novnc.join("core")).unwrap();
    fs::write(novnc.join("core/rfb.js"), "").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sellaite"));
    command
        .args(["dashboard", "serve", "--address", address, "--novnc-dir"])
        .arg(novnc)
        .stdout(Stdio::piped());
    command
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
    let url = format!("/?token={}", dashboard.token);
    assert_eq!(dashboard.status("GET", &url, &[public]), 403);
    let (status, headers) = dashboard.request("GET", &url, &[public, owner]);
    assert_eq!(status, 303);
    let port = dashboard.address.rsplit_once(':').unwrap().1;
    let session = format!("__Host-sellaite-{port}={}", dashboard.token);
    let cookie = ("Cookie", session.as_str());
    assert!(headers.lines().any(|line| line == "location: /"));
    assert!(headers.lines().any(
        |line| line == format!("set-cookie: {session}; Path=/; HttpOnly; SameSite=Lax; Secure")
    ));

    assert_eq!(dashboard.status("GET", "/", &[public, owner, cookie]), 200);
    assert_eq!(
        dashboard.status("GET", "/health", &[public, owner, cookie]),
        204
    );
    assert_eq!(dashboard.status("GET", "/", &[public, cookie]), 403);
    assert_eq!(
        dashboard.status(
            "GET",
            "/health",
            &[
                public,
                cookie,
                ("Tailscale-User-Login", "someone@example.com")
            ]
        ),
        403
    );
    let local = ("Host", dashboard.address.as_str());
    assert_eq!(dashboard.status("GET", &url, &[local]), 403);
    assert_eq!(
        dashboard.status("GET", "/health", &[local]),
        403,
        "a loopback request without a login was served"
    );
    assert_eq!(
        dashboard.status("GET", &url, &[("Host", "evil.example:8731"), owner]),
        403
    );

    let stop = "/api/graphql";
    assert_eq!(
        dashboard.status("POST", stop, &[public, origin, cookie]),
        403
    );
    assert_eq!(
        dashboard.status("POST", stop, &[public, owner, cookie]),
        403
    );
    assert_eq!(
        dashboard.status(
            "POST",
            "/api/graphql",
            &[public, owner, cookie, ("Origin", "https://evil.example")]
        ),
        403
    );
    assert_eq!(
        dashboard.status("POST", stop, &[public, owner, origin, cookie]),
        415,
        "an authorized request did not reach GraphQL body extraction"
    );
}

#[test]
fn forged_headers_cannot_reach_dashboard_routes() {
    let dashboard =
        Dashboard::spawn(&["--public-origins", PUBLIC_ORIGIN, "--allowed-logins", LOGIN]);
    let forged = [
        ("Host", PUBLIC_HOST),
        ("Origin", PUBLIC_ORIGIN),
        ("Tailscale-User-Login", LOGIN),
    ];
    let websocket_key = STANDARD.encode(b"the sample nonce");
    for (method, path) in [
        ("GET", "/"),
        ("GET", "/health"),
        ("GET", "/novnc/core/rfb.js"),
        ("POST", "/api/graphql"),
        ("GET", "/missing"),
    ] {
        assert_eq!(
            dashboard.status(method, path, &forged),
            403,
            "{method} {path}"
        );
    }
    assert_eq!(
        dashboard.status(
            "GET",
            "/api/desktops/none/vnc?connection=0123456789abcdef0123456789abcdef",
            &[
                forged[0],
                forged[1],
                forged[2],
                ("Connection", "Upgrade"),
                ("Upgrade", "websocket"),
                ("Sec-WebSocket-Key", &websocket_key),
                ("Sec-WebSocket-Version", "13"),
            ],
        ),
        403
    );
    let bootstrap = format!("/?token={}", dashboard.token);
    let health = format!("/health?token={}", dashboard.token);
    assert_eq!(dashboard.status("POST", &bootstrap, &forged), 403);
    assert_eq!(dashboard.status("GET", &health, &forged), 403);
}

#[test]
fn the_dashboard_rejects_another_starts_token() {
    let stale = Dashboard::spawn(&[]).token.clone();
    let dashboard = Dashboard::spawn(&[]);
    assert_ne!(dashboard.token, stale);
    let host = ("Host", dashboard.address.as_str());
    assert_eq!(
        dashboard.status("GET", &format!("/?token={stale}"), &[host]),
        403
    );
    let port = dashboard.address.rsplit_once(':').unwrap().1;
    for token in [&stale, &dashboard.token] {
        let session = format!("sellaite-{port}={token}");
        let cookie = ("Cookie", session.as_str());
        let expected = if *token == stale { 403 } else { 204 };
        assert_eq!(
            dashboard.status("GET", "/health", &[host, cookie]),
            expected
        );
    }
    let (_, headers) = dashboard.request("GET", &format!("/?token={}", dashboard.token), &[host]);
    assert!(headers.lines().any(|line| line
        == format!(
            "set-cookie: sellaite-{port}={}; Path=/; HttpOnly; SameSite=Lax",
            dashboard.token
        )));
}

#[test]
fn token_file_preserves_the_public_url_and_cookie_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token");
    let args = [
        "--public-origins",
        PUBLIC_ORIGIN,
        "--allowed-logins",
        LOGIN,
        "--token-file",
        path.to_str().unwrap(),
    ];
    let first = Dashboard::spawn(&args);
    let address = first.address.clone();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bootstrap = format!("/?token={}", first.token);
    let public = ("Host", PUBLIC_HOST);
    let owner = ("Tailscale-User-Login", LOGIN);
    let (status, headers) = first.request("GET", &bootstrap, &[public, owner]);
    assert_eq!(status, 303);
    let cookie = headers
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    drop(first);

    let second = Dashboard::spawn_at(&address, &args);
    assert_eq!(
        second.status("GET", "/", &[public, owner, ("Cookie", cookie)]),
        200
    );
    assert_eq!(second.status("GET", &bootstrap, &[public, owner]), 303);
    assert_eq!(second.status("GET", "/", &[public, owner]), 403);
}

#[test]
fn token_file_must_be_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token");
    let args = ["--token-file", path.to_str().unwrap()];
    drop(Dashboard::spawn(&args));

    for mode in [0o640, 0o604] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let mut child = dashboard_command("127.0.0.1:0")
            .args(args)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut startup = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut startup)
            .unwrap();
        if !startup.is_empty() {
            child.kill().unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(255), "file mode {mode:o}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("chmod 600"));
    }
}

#[test]
fn dashboard_responses_cannot_be_framed() {
    let dashboard = Dashboard::spawn(&[]);
    let host = ("Host", dashboard.address.as_str());
    let port = dashboard.address.rsplit_once(':').unwrap().1;
    let session = format!("sellaite-{port}={}", dashboard.token);
    let (status, page) = dashboard.request("GET", "/", &[host, ("Cookie", &session)]);
    assert_eq!(status, 200);
    assert!(
        page.lines()
            .any(|line| line == "content-security-policy: frame-ancestors 'none'")
    );
    assert!(page.lines().any(|line| line == "x-frame-options: DENY"));
}

#[test]
fn the_dashboard_refuses_non_loopback_addresses() {
    for address in ["0.0.0.0:0", "[::]:0"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sellaite"))
            .args([
                "dashboard",
                "serve",
                "--address",
                address,
                "--novnc-dir",
                "/nonexistent",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(252));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("the dashboard needs a loopback address")
        );
    }
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
