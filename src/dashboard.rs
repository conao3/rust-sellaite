use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{Value, json};

use crate::runtime;

const INDEX: &str = include_str!("dashboard.html");

const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const STOP_REASON: &str = "the user pressed Stop in the dashboard";

const MAX_HEADER_BYTES: usize = 16 * 1024;

const MAX_FRAME_BYTES: u64 = 1 << 20;

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Config {
    novnc_dir: PathBuf,
    allowed_hosts: Vec<String>,
}

pub fn serve(address: SocketAddr, novnc_dir: &Path) -> Result<(), String> {
    let novnc_dir = novnc_dir.canonicalize().map_err(|e| {
        format!(
            "cannot open the noVNC directory {}: {e}",
            novnc_dir.display()
        )
    })?;
    if !novnc_dir.join("core/rfb.js").is_file() {
        return Err(format!(
            "{} has no core/rfb.js; pass noVNC's directory with --novnc-dir",
            novnc_dir.display()
        ));
    }
    let listener =
        TcpListener::bind(address).map_err(|e| format!("cannot listen on {address}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    let port = local.port();
    let mut allowed_hosts = vec![
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
    ];
    if !local.ip().is_unspecified() {
        allowed_hosts.push(local.to_string());
    }
    let url = format!(
        "http://{}/",
        allowed_hosts.last().unwrap_or(&allowed_hosts[1])
    );
    println!("{}", json!({"Url": url}));
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let config = Arc::new(Config {
        novnc_dir,
        allowed_hosts,
    });
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let config = Arc::clone(&config);
        std::thread::spawn(move || {
            let _ = handle(stream, &config);
        });
    }
    Ok(())
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut lines = Vec::new();
    let mut total = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).ok()?;
        total += n;
        if n == 0 || total > MAX_HEADER_BYTES {
            return None;
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut parts = lines.first()?.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let path = target.split('?').next()?.to_string();
    let headers = lines[1..]
        .iter()
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        .collect();
    Some(Request {
        method,
        path,
        headers,
    })
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn respond_json(stream: &mut TcpStream, status: &str, body: &Value) {
    respond(
        stream,
        status,
        "application/json",
        body.to_string().as_bytes(),
    );
}

fn respond_error(stream: &mut TcpStream, status: &str, message: &str) {
    respond_json(stream, status, &json!({"Error": message}));
}

fn handle(stream: TcpStream, config: &Config) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    let Some(request) = read_request(&mut reader) else {
        respond_error(&mut stream, "400 Bad Request", "malformed request");
        return Ok(());
    };
    let host = request.header("Host").unwrap_or("");
    if !config.allowed_hosts.iter().any(|h| h == host) {
        respond_error(&mut stream, "403 Forbidden", "unexpected Host header");
        return Ok(());
    }
    let same_origin = request.header("Origin") == Some(format!("http://{host}").as_str());
    let segments: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", [""]) => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX.as_bytes(),
        ),
        ("GET", ["api", "desktops"]) => respond_json(&mut stream, "200 OK", &desktops()),
        ("POST", ["api", "desktops", id, action @ ("pause" | "resume")]) => {
            if !same_origin {
                respond_error(&mut stream, "403 Forbidden", "cross-origin request");
                return Ok(());
            }
            let Some(dir) = live_dir(id) else {
                respond_error(&mut stream, "404 Not Found", "no such desktop");
                return Ok(());
            };
            let done = if *action == "pause" {
                runtime::pause(&dir, STOP_REASON)
            } else {
                runtime::resume(&dir)
            };
            match done {
                Ok(()) => respond_json(&mut stream, "200 OK", &desktop_json(id, &dir)),
                Err(e) => respond_error(&mut stream, "500 Internal Server Error", &e),
            }
        }
        ("GET", ["api", "desktops", id, "vnc"]) => {
            if !same_origin {
                respond_error(&mut stream, "403 Forbidden", "cross-origin request");
                return Ok(());
            }
            let Some(dir) = live_dir(id) else {
                respond_error(&mut stream, "404 Not Found", "no such desktop");
                return Ok(());
            };
            proxy_vnc(stream, reader, &request, &dir.join(runtime::VNC_SOCKET));
        }
        ("GET", ["novnc", rest @ ..]) => serve_file(&mut stream, &config.novnc_dir, rest),
        _ => respond_error(&mut stream, "404 Not Found", "not found"),
    }
    Ok(())
}

fn live_dir(id: &str) -> Option<PathBuf> {
    runtime::live_desktops()
        .into_iter()
        .find(|(i, _, _)| i == id)
        .map(|(_, dir, _)| dir)
}

fn desktop_json(id: &str, dir: &Path) -> Value {
    let state = runtime::read_state(dir);
    let paused = runtime::paused(dir);
    json!({
        "Id": id,
        "Width": state.as_ref().map(|s| s.width),
        "Height": state.as_ref().map(|s| s.height),
        "Pid": state.as_ref().map(|s| s.pid),
        "Paused": paused.is_some(),
        "PausedReason": paused,
        "UserSeatIdle": runtime::user_idle(dir),
    })
}

fn desktops() -> Value {
    let list: Vec<Value> = runtime::live_desktops()
        .iter()
        .map(|(id, dir, _)| desktop_json(id, dir))
        .collect();
    json!({"Desktops": list})
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn serve_file(stream: &mut TcpStream, root: &Path, segments: &[&str]) {
    let safe = !segments.is_empty()
        && segments
            .iter()
            .all(|s| !s.is_empty() && *s != "." && *s != ".." && !s.contains('\\'));
    let path = segments.iter().fold(root.to_path_buf(), |p, s| p.join(s));
    let file = safe
        .then(|| path.canonicalize().ok())
        .flatten()
        .filter(|p| p.starts_with(root) && p.is_file());
    match file.and_then(|p| std::fs::read(&p).ok().map(|body| (p, body))) {
        Some((p, body)) => respond(stream, "200 OK", content_type(&p), &body),
        None => respond_error(stream, "404 Not Found", "not found"),
    }
}

fn websocket_accept(key: &str) -> String {
    let digest = sha1_smol::Sha1::from(format!("{key}{WEBSOCKET_GUID}")).digest();
    base64::engine::general_purpose::STANDARD.encode(digest.bytes())
}

fn write_frame(stream: &Mutex<TcpStream>, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => frame.push(n as u8),
        n if n <= 0xffff => {
            frame.push(126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    let mut stream = stream.lock().unwrap_or_else(|e| e.into_inner());
    stream.write_all(&frame)
}

fn read_frame(reader: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let invalid = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let mut head = [0u8; 2];
    reader.read_exact(&mut head)?;
    let opcode = head[0] & 0x0f;
    if head[1] & 0x80 == 0 {
        return Err(invalid("client frames must be masked"));
    }
    let len = match head[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            reader.read_exact(&mut b)?;
            u64::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0u8; 8];
            reader.read_exact(&mut b)?;
            u64::from_be_bytes(b)
        }
        n => u64::from(n),
    };
    if len > MAX_FRAME_BYTES {
        return Err(invalid("frame too large"));
    }
    let mut mask = [0u8; 4];
    reader.read_exact(&mut mask)?;
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload)?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Ok((opcode, payload))
}

fn proxy_vnc(
    mut stream: TcpStream,
    mut reader: BufReader<TcpStream>,
    request: &Request,
    socket: &Path,
) {
    let upgrade = request
        .header("Upgrade")
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let Some(key) = request.header("Sec-WebSocket-Key").filter(|_| upgrade) else {
        respond_error(
            &mut stream,
            "400 Bad Request",
            "expected a WebSocket upgrade",
        );
        return;
    };
    let Ok(vnc) = UnixStream::connect(socket) else {
        respond_error(&mut stream, "502 Bad Gateway", "cannot reach wayvnc");
        return;
    };
    let protocol = request
        .header("Sec-WebSocket-Protocol")
        .filter(|p| p.split(',').any(|p| p.trim() == "binary"))
        .map(|_| "Sec-WebSocket-Protocol: binary\r\n")
        .unwrap_or("");
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n{protocol}\r\n",
        websocket_accept(key)
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let Ok(mut vnc_reader) = vnc.try_clone() else {
        return;
    };
    let browser = Arc::new(Mutex::new(stream));
    let to_browser = Arc::clone(&browser);
    let downstream = std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = vnc_reader.read(&mut buf) {
            if n == 0 || write_frame(&to_browser, 0x2, &buf[..n]).is_err() {
                break;
            }
        }
        let _ = write_frame(&to_browser, 0x8, &[]);
        if let Ok(s) = to_browser.lock() {
            let _ = s.shutdown(Shutdown::Both);
        }
    });
    let mut vnc_writer = vnc;
    while let Ok((opcode, payload)) = read_frame(&mut reader) {
        let sent = match opcode {
            0x0..=0x2 => vnc_writer.write_all(&payload),
            0x8 => break,
            0x9 => write_frame(&browser, 0xa, &payload),
            _ => Ok(()),
        };
        if sent.is_err() {
            break;
        }
    }
    let _ = vnc_writer.shutdown(Shutdown::Both);
    let _ = downstream.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_rfc6455_sample_key() {
        assert_eq!(
            websocket_accept(
                &base64::engine::general_purpose::STANDARD.encode(b"the sample nonce")
            ),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn reads_a_masked_frame() {
        let mask = [1u8, 2, 3, 4];
        let payload = b"RFB 003.008\n";
        let mut frame = vec![0x82, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let (opcode, data) = read_frame(&mut frame.as_slice()).unwrap();
        assert_eq!(opcode, 0x2);
        assert_eq!(data, payload);
    }

    #[test]
    fn rejects_an_unmasked_frame() {
        assert!(read_frame(&mut [0x82u8, 0x01, 0x00].as_slice()).is_err());
    }
}
