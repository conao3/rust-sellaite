use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_graphql_axum::GraphQLResponse;
use axum::extract::{
    DefaultBodyLimit, Path as RoutePath, Query, Request, State, WebSocketUpgrade, ws::Message,
};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::{Mutex, Notify};

use crate::{graphql, runtime};

const INDEX: &str = include_str!("dashboard.html");
const LOGIN_HEADER: &str = "Tailscale-User-Login";

struct Site {
    host: String,
    origin: String,
}

#[derive(Default)]
pub(crate) struct Control {
    connections: HashMap<String, (String, Arc<Notify>, bool)>,
    owners: HashMap<String, String>,
}

impl Control {
    pub(crate) fn owns(&self, id: &str, connection: &str) -> bool {
        self.owners.get(id).is_some_and(|owner| owner == connection)
    }

    pub(crate) fn take(&mut self, id: &str, dir: &Path, connection: &str) -> Result<(), String> {
        if !self
            .connections
            .get(connection)
            .is_some_and(|(desktop, _, retired)| desktop == id && !retired)
        {
            return Err("no live VNC connection for this desktop".into());
        }
        if self.owners.contains_key(id) {
            return Err("another connection already controls this desktop".into());
        }
        runtime::take_over(dir)?;
        self.owners.insert(id.to_owned(), connection.to_owned());
        Ok(())
    }

    pub(crate) fn release(&mut self, id: &str, dir: &Path, connection: &str) -> Result<(), String> {
        if !self.owns(id, connection) {
            return Err("this connection does not control the desktop".into());
        }
        self.owners.remove(id);
        if let Some((_, close, retired)) = self.connections.get_mut(connection) {
            *retired = true;
            close.notify_one();
        }
        runtime::give_back(dir)
    }
}

pub(crate) type SharedControl = Arc<Mutex<Control>>;

struct Config {
    schema: graphql::DashboardSchema,
    control: SharedControl,
    novnc_dir: PathBuf,
    sites: Vec<Site>,
    allowed_logins: Option<Vec<String>>,
    token: Option<String>,
    cookie_name: String,
}

impl Config {
    fn authorize(
        &self,
        host: Option<&str>,
        origin: Option<&str>,
        login: Option<&str>,
        needs_origin: bool,
    ) -> Result<&Site, &'static str> {
        let site = self
            .sites
            .iter()
            .find(|s| Some(s.host.as_str()) == host)
            .ok_or("unexpected Host header")?;
        if let Some(logins) = &self.allowed_logins
            && !login.is_some_and(|l| logins.iter().any(|a| a == l))
        {
            return Err("the request has no allowed Tailscale identity");
        }
        if needs_origin && origin != Some(site.origin.as_str()) {
            return Err("cross-origin request");
        }
        Ok(site)
    }
}

pub fn parse_origin(text: &str) -> Result<String, String> {
    let text = text.strip_suffix('/').unwrap_or(text);
    let authority = text
        .strip_prefix("https://")
        .ok_or_else(|| format!("{text} does not start with https://"))?;
    if authority.parse::<axum::http::uri::Authority>().is_err() || authority.contains('@') {
        return Err(format!(
            "{text} is not an HTTPS origin (a scheme and a host with an optional port)"
        ));
    }
    Ok(text.to_ascii_lowercase())
}

pub enum Token {
    PerStart,
    File(PathBuf),
    None,
}

fn matches_token(token: &str, value: &str) -> bool {
    bool::from(token.as_bytes().ct_eq(value.as_bytes()))
}

#[tokio::main]
pub async fn serve(
    address: SocketAddr,
    novnc_dir: &Path,
    public_origins: &[String],
    allowed_logins: Option<Vec<String>>,
    token: Token,
) -> Result<(), String> {
    let novnc_hint = "install noVNC (Ubuntu package: novnc) and point --novnc-dir or SELLAITE_NOVNC_DIR at the directory containing core/rfb.js";
    let novnc_dir = novnc_dir.canonicalize().map_err(|e| {
        format!(
            "cannot open the noVNC directory {}: {e}; {novnc_hint}",
            novnc_dir.display()
        )
    })?;
    if !novnc_dir.join("core/rfb.js").is_file() {
        return Err(format!(
            "{} has no core/rfb.js; {novnc_hint}",
            novnc_dir.display()
        ));
    }
    let listener = TcpListener::bind(address)
        .await
        .map_err(|e| format!("cannot listen on {address}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    let port = local.port();
    let mut allowed_hosts = vec![
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
    ];
    let local_host = local.to_string();
    if !allowed_hosts.contains(&local_host) {
        allowed_hosts.push(local_host);
    }
    let token = match token {
        Token::File(path) => Some(
            persistent_token(&path)
                .map_err(|e| format!("cannot use dashboard token file {}: {e}", path.display()))?,
        ),
        Token::PerStart => {
            Some(generate_token().map_err(|e| format!("cannot generate a dashboard token: {e}"))?)
        }
        Token::None => None,
    };
    let query = token
        .as_ref()
        .map_or(String::new(), |token| format!("?token={token}"));
    let url = format!("http://{local}/{query}");
    let mut sites: Vec<Site> = allowed_hosts
        .into_iter()
        .map(|host| Site {
            origin: format!("http://{host}"),
            host,
        })
        .collect();
    sites.extend(public_origins.iter().map(|origin| {
        Site {
            host: origin
                .split_once("://")
                .map_or(origin.as_str(), |(_, a)| a)
                .to_string(),
            origin: origin.clone(),
        }
    }));
    let public_urls: Vec<String> = public_origins
        .iter()
        .map(|o| format!("{o}/{query}"))
        .collect();
    println!("{}", json!({"Url": url, "PublicUrls": public_urls}));
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let control = SharedControl::default();
    let config = Arc::new(Config {
        schema: graphql::schema_with_control(control.clone()),
        control,
        novnc_dir,
        sites,
        allowed_logins,
        token,
        cookie_name: format!("sellaite-{port}"),
    });
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .route("/api/graphql", post(execute_graphql))
        .route("/api/desktops/{id}/vnc", get(vnc))
        .route("/novnc/{*path}", get(serve_file))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            authorize_request,
        ))
        .with_state(config);
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

fn generate_token() -> io::Result<String> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(io::Error::other)?;
    Ok(URL_SAFE_NO_PAD.encode(secret))
}

fn persistent_token(path: &Path) -> io::Result<String> {
    match read_token(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        result => return result,
    }
    let token = generate_token()?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // Publish the complete token without replacing a concurrent starter's token.
    let mut file = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile_in(parent)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    writeln!(file, "{token}")?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(path) {
        Ok(_) => Ok(token),
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => read_token(path),
        Err(e) => Err(e.error),
    }
}

fn read_token(path: &Path) -> io::Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "expected a regular file with no group or other permissions (chmod 600)",
        ));
    }
    // A 32-byte base64url token is 43 characters, with an optional newline.
    let mut text = String::new();
    file.take(45).read_to_string(&mut text)?;
    let token = text.strip_suffix('\n').unwrap_or(&text);
    if !matches!(URL_SAFE_NO_PAD.decode(token), Ok(bytes) if bytes.len() == 32) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a 32-byte base64url token",
        ));
    }
    Ok(token.to_owned())
}

async fn authorize_request(
    State(config): State<Arc<Config>>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = dashboard_response(&config, request, next).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
        (header::X_FRAME_OPTIONS, "DENY"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

async fn dashboard_response(config: &Config, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let value = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let host = value("host").map(str::to_ascii_lowercase);
    let origin = value("origin").map(str::to_ascii_lowercase);
    let needs_origin = matches!(request.uri().path(), "/api/graphql")
        || request.uri().path().starts_with("/api/desktops/");
    let site = match config.authorize(
        host.as_deref(),
        origin.as_deref(),
        value(LOGIN_HEADER),
        needs_origin,
    ) {
        Ok(site) => site,
        Err(message) => return (StatusCode::FORBIDDEN, message).into_response(),
    };
    let Some(token) = &config.token else {
        return next.run(request).await;
    };
    let secure = site.origin.starts_with("https://");
    let cookie_name = if secure {
        format!("__Host-{}", config.cookie_name)
    } else {
        config.cookie_name.clone()
    };
    if request.method() == Method::GET
        && request.uri().path() == "/"
        && request
            .uri()
            .query()
            .and_then(|query| query.strip_prefix("token="))
            .is_some_and(|value| matches_token(token, value))
    {
        let attributes = if secure { "; Secure" } else { "" };
        let cookie = format!("{cookie_name}={token}; Path=/; HttpOnly; SameSite=Lax{attributes}");
        let mut response = Redirect::to("/").into_response();
        response
            .headers_mut()
            .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
        return response;
    }
    let authenticated = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == cookie_name && matches_token(token, value));
    if !authenticated {
        return (StatusCode::FORBIDDEN, "open the dashboard's startup URL").into_response();
    }
    next.run(request).await
}

async fn execute_graphql(
    State(config): State<Arc<Config>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<async_graphql::Request>,
) -> GraphQLResponse {
    let connection = headers
        .get("x-sellaite-connection")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    config
        .schema
        .execute(request.data(graphql::Connection(connection.to_owned())))
        .await
        .into()
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

async fn serve_file(
    State(config): State<Arc<Config>>,
    RoutePath(path): RoutePath<String>,
) -> Result<impl IntoResponse, StatusCode> {
    if path
        .split('/')
        .any(|part| part.is_empty() || matches!(part, "." | "..") || part.contains('\\'))
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let path = config
        .novnc_dir
        .join(path)
        .canonicalize()
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !path.starts_with(&config.novnc_dir) || !path.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    let bytes = std::fs::read(&path).map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(([(header::CONTENT_TYPE, content_type(&path))], bytes))
}

#[derive(serde::Deserialize)]
struct VncQuery {
    connection: String,
}

async fn vnc(
    State(config): State<Arc<Config>>,
    RoutePath(id): RoutePath<String>,
    Query(VncQuery { connection }): Query<VncQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    if connection.len() != 32 || !connection.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let dir = runtime::live_dir(&id).ok_or(StatusCode::NOT_FOUND)?;
    let vnc = UnixStream::connect(dir.join(runtime::VNC_SOCKET))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(upgrade.protocols(["binary"]).max_message_size(1 << 20).on_upgrade(move |mut browser| async move {
        let close = Arc::new(Notify::new());
        {
            let mut control = config.control.lock().await;
            let std::collections::hash_map::Entry::Vacant(entry) =
                control.connections.entry(connection.clone())
            else {
                return;
            };
            entry.insert((id.clone(), close.clone(), false));
        }
        let (mut incoming, mut outgoing) = vnc.into_split();
        let mut buffer = vec![0; 64 * 1024];
        let mut pending = Vec::new();
        let mut handshake = 0;
        let mut encodings = Vec::new();
        let mut controller: Option<(OwnedWriteHalf, tokio::task::JoinHandle<()>)> = None;
        'proxy: loop {
            tokio::select! {
                _ = close.notified() => break,
                frame = browser.recv() => {
                    match frame {
                        Some(Ok(Message::Binary(data))) => {
                            pending.extend_from_slice(&data);
                            let mut remaining = pending.as_slice();
                            while !remaining.is_empty() {
                                let (size, input) = match client_message(remaining, handshake) {
                                    Ok(Some(message)) => message,
                                    Ok(None) => break,
                                    Err(()) => break 'proxy,
                                };
                                if remaining.len() < size { break; }
                                let control = config.control.lock().await;
                                if control.connections[&connection].2 {
                                    break 'proxy;
                                }
                                let message = &remaining[..size];
                                if handshake == 3 && message[0] == 2 {
                                    encodings = message.to_vec();
                                }
                                let target = if !input {
                                    Some(&mut outgoing)
                                } else if control.owns(&id, &connection) {
                                    if controller.is_none() {
                                        match control_vnc(&dir, &encodings).await {
                                            Ok(connected) => controller = Some(connected),
                                            Err(error) => {
                                                eprintln!("sellaite: cannot open the control VNC connection: {error}");
                                                break 'proxy;
                                            }
                                        }
                                    }
                                    controller.as_mut().map(|(writer, _)| writer)
                                } else {
                                    None
                                };
                                if let Some(target) = target
                                    && !matches!(tokio::time::timeout(Duration::from_secs(2), target.write_all(message)).await, Ok(Ok(())))
                                {
                                    break 'proxy;
                                }
                                handshake = (handshake + 1).min(3);
                                remaining = &remaining[size..];
                            }
                            let consumed = pending.len() - remaining.len();
                            pending.drain(..consumed);
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        _ => {}
                    }
                }
                result = incoming.read(&mut buffer) => {
                    let Ok(size) = result else { break };
                    if size == 0 || !matches!(tokio::time::timeout(Duration::from_secs(2), browser.send(Message::Binary(buffer[..size].to_vec().into()))).await, Ok(Ok(()))) {
                        break;
                    }
                }
            }
        }
        drop(outgoing);
        drop(incoming);
        if let Some((writer, drain)) = controller {
            drop(writer);
            drain.abort();
        }
        let mut control = config.control.lock().await;
        if control.owns(&id, &connection)
            && let Err(error) = control.release(&id, &dir, &connection)
        {
            eprintln!("{error}");
        }
        control.connections.remove(&connection);
    }))
}

async fn control_vnc(
    dir: &Path,
    encodings: &[u8],
) -> io::Result<(OwnedWriteHalf, tokio::task::JoinHandle<()>)> {
    let handshake = async {
        let mut vnc = UnixStream::connect(dir.join(runtime::CONTROL_VNC_SOCKET)).await?;
        let mut version = [0u8; 12];
        vnc.read_exact(&mut version).await?;
        vnc.write_all(b"RFB 003.008\n").await?;
        let mut count = [0u8; 1];
        vnc.read_exact(&mut count).await?;
        let mut types = vec![0u8; usize::from(count[0])];
        vnc.read_exact(&mut types).await?;
        if !types.contains(&1) {
            return Err(io::Error::other("wayvnc offers no security type None"));
        }
        vnc.write_all(&[1]).await?;
        let mut result = [0u8; 4];
        vnc.read_exact(&mut result).await?;
        if result != [0; 4] {
            return Err(io::Error::other("wayvnc refused the security handshake"));
        }
        vnc.write_all(&[1]).await?;
        let mut init = [0u8; 24];
        vnc.read_exact(&mut init).await?;
        let name = u32::from_be_bytes(init[20..24].try_into().unwrap());
        if name > 1 << 16 {
            return Err(io::Error::other("wayvnc sent an oversized desktop name"));
        }
        let mut name = vec![0u8; name as usize];
        vnc.read_exact(&mut name).await?;
        vnc.write_all(encodings).await?;
        Ok(vnc)
    };
    let vnc = tokio::time::timeout(Duration::from_secs(2), handshake)
        .await
        .map_err(|_| io::Error::other("the control VNC handshake timed out"))??;
    let (mut reader, writer) = vnc.into_split();
    let drain = tokio::spawn(async move {
        let mut buffer = vec![0; 16 * 1024];
        while matches!(reader.read(&mut buffer).await, Ok(size) if size > 0) {}
    });
    Ok((writer, drain))
}

fn client_message(bytes: &[u8], handshake: u8) -> Result<Option<(usize, bool)>, ()> {
    if handshake < 3 {
        return match handshake {
            0 if b"RFB 003.008\n".starts_with(&bytes[..bytes.len().min(12)]) => {
                Ok(Some((12, false)))
            }
            1 | 2 if bytes[0] == 1 => Ok(Some((1, false))),
            _ => Err(()),
        };
    }
    let message = match bytes[0] {
        0 => (20, false),
        2 if bytes.len() >= 4 => (
            4 + 4 * usize::from(u16::from_be_bytes([bytes[2], bytes[3]])),
            false,
        ),
        3 | 150 => (10, false),
        4 => (8, true),
        5 if bytes.len() >= 2 => (if bytes[1] & 0x80 == 0 { 6 } else { 7 }, true),
        6 if bytes.len() >= 8 => {
            let len = i32::from_be_bytes(bytes[4..8].try_into().unwrap()).unsigned_abs() as usize;
            if len > 1 << 20 {
                return Err(());
            }
            (8 + len, true)
        }
        248 if bytes.len() >= 9 && bytes[8] <= 64 => (9 + usize::from(bytes[8]), false),
        248 if bytes.len() >= 9 => return Err(()),
        250 => (4, true),
        251 if bytes.len() >= 8 => (8 + 16 * usize::from(bytes[6]), true),
        255 if bytes.len() >= 2 && bytes[1] == 0 => (12, true),
        255 if bytes.len() >= 2 => return Err(()),
        2 | 5 | 6 | 248 | 251 | 255 => return Ok(None),
        _ => return Err(()),
    };
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(allowed_logins: Option<Vec<String>>) -> Config {
        Config {
            schema: graphql::schema(),
            control: SharedControl::default(),
            novnc_dir: PathBuf::new(),
            sites: vec![
                Site {
                    host: "127.0.0.1:8730".into(),
                    origin: "http://127.0.0.1:8730".into(),
                },
                Site {
                    host: "box.example.ts.net:8731".into(),
                    origin: "https://box.example.ts.net:8731".into(),
                },
            ],
            allowed_logins,
            token: None,
            cookie_name: String::new(),
        }
    }

    #[test]
    fn authorizes_by_host_login_and_origin() {
        let open = config(None);
        assert!(
            open.authorize(Some("127.0.0.1:8730"), None, None, false)
                .is_ok()
        );
        assert!(
            open.authorize(Some("evil:8730"), None, None, false)
                .is_err()
        );
        assert!(open.authorize(None, None, None, false).is_err());
        let guarded = config(Some(vec!["me@example.com".into()]));
        let public = Some("box.example.ts.net:8731");
        let origin = Some("https://box.example.ts.net:8731");
        assert!(guarded.authorize(public, None, None, false).is_err());
        assert!(
            guarded
                .authorize(public, None, Some("other@example.com"), false)
                .is_err()
        );
        assert!(
            guarded
                .authorize(public, None, Some("me@example.com"), false)
                .is_ok()
        );
        assert!(
            guarded
                .authorize(public, origin, Some("me@example.com"), true)
                .is_ok()
        );
        assert!(
            guarded
                .authorize(
                    public,
                    Some("http://127.0.0.1:8730"),
                    Some("me@example.com"),
                    true
                )
                .is_err()
        );
        assert!(
            guarded
                .authorize(public, None, Some("me@example.com"), true)
                .is_err()
        );
    }

    #[test]
    fn parses_an_origin() {
        assert_eq!(
            parse_origin("https://Box.example.ts.net:8731/").unwrap(),
            "https://box.example.ts.net:8731"
        );
        assert!(parse_origin("box.example.ts.net").is_err());
        assert!(parse_origin("https://box.example.ts.net/dashboard").is_err());
        assert!(parse_origin("https://").is_err());
    }
}
