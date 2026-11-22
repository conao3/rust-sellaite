use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_graphql_axum::GraphQLResponse;
use axum::extract::{
    DefaultBodyLimit, Path as RoutePath, Request, State, WebSocketUpgrade, ws::Message,
};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};

use crate::{graphql, runtime};

const INDEX: &str = include_str!("dashboard.html");
const LOGIN_HEADER: &str = "Tailscale-User-Login";

struct Site {
    host: String,
    origin: String,
}

struct Config {
    schema: graphql::DashboardSchema,
    novnc_dir: PathBuf,
    sites: Vec<Site>,
    allowed_logins: Option<Vec<String>>,
}

impl Config {
    fn authorize(
        &self,
        host: Option<&str>,
        origin: Option<&str>,
        login: Option<&str>,
        needs_origin: bool,
    ) -> Result<(), &'static str> {
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
        Ok(())
    }
}

pub fn parse_origin(text: &str) -> Result<String, String> {
    let text = text.strip_suffix('/').unwrap_or(text);
    let authority = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .ok_or_else(|| format!("{text} does not start with https:// or http://"))?;
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return Err(format!(
            "{text} is not an origin (a scheme and a host with an optional port)"
        ));
    }
    Ok(text.to_ascii_lowercase())
}

#[tokio::main]
pub async fn serve(
    address: SocketAddr,
    novnc_dir: &Path,
    public_origins: &[String],
    allowed_logins: Option<Vec<String>>,
) -> Result<(), String> {
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
    if !local.ip().is_unspecified() {
        allowed_hosts.push(local.to_string());
    }
    let url = format!(
        "http://{}/",
        allowed_hosts.last().unwrap_or(&allowed_hosts[1])
    );
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
    let public_urls: Vec<String> = public_origins.iter().map(|o| format!("{o}/")).collect();
    println!("{}", json!({"Url": url, "PublicUrls": public_urls}));
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let config = Arc::new(Config {
        schema: graphql::schema(),
        novnc_dir,
        sites,
        allowed_logins,
    });
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .route("/api/graphql", post(execute_graphql))
        .route("/api/desktops/{id}/vnc", get(vnc))
        .route("/novnc/{*path}", get(serve_file))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            authorize_request,
        ))
        .with_state(config);
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

async fn authorize_request(
    State(config): State<Arc<Config>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let value = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let host = value("host").map(str::to_ascii_lowercase);
    let origin = value("origin").map(str::to_ascii_lowercase);
    let needs_origin = matches!(request.uri().path(), "/api/graphql")
        || request.uri().path().starts_with("/api/desktops/");
    if let Err(message) = config.authorize(
        host.as_deref(),
        origin.as_deref(),
        value(LOGIN_HEADER),
        needs_origin,
    ) {
        return (StatusCode::FORBIDDEN, message).into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn execute_graphql(
    State(config): State<Arc<Config>>,
    Json(request): Json<async_graphql::Request>,
) -> GraphQLResponse {
    config.schema.execute(request).await.into()
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

async fn vnc(
    RoutePath(id): RoutePath<String>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    let dir = runtime::live_dir(&id).ok_or(StatusCode::NOT_FOUND)?;
    let vnc = UnixStream::connect(dir.join(runtime::VNC_SOCKET))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(upgrade.protocols(["binary"]).max_message_size(1 << 20).on_upgrade(move |mut browser| async move {
        let (mut incoming, mut outgoing) = vnc.into_split();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            tokio::select! {
                frame = browser.recv() => {
                    match frame {
                        Some(Ok(Message::Binary(data))) => {
                            if outgoing.write_all(&data).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        _ => {}
                    }
                }
                result = incoming.read(&mut buffer) => {
                    let Ok(size) = result else { break };
                    if size == 0 || browser.send(Message::Binary(buffer[..size].to_vec().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(allowed_logins: Option<Vec<String>>) -> Config {
        Config {
            schema: graphql::schema(),
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
