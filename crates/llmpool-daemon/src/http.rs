//! The daemon's local HTTP API: JSON over loopback, one bearer token.

use crate::{fail, ApiError, Daemon};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const WORKERS: usize = 4;

/// Where a running daemon can be reached. Written to `daemon.json`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

impl Endpoint {
    pub fn read(data_dir: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(data_dir.join("daemon.json")).ok()?).ok()
    }
}

pub struct Running {
    pub endpoint: Endpoint,
    workers: Vec<JoinHandle<()>>,
}

impl Running {
    /// Block until the daemon is told to shut down.
    pub fn wait(self) {
        for worker in self.workers {
            let _ = worker.join();
        }
    }
}

fn parse<T: DeserializeOwned>(body: Value) -> Result<T, ApiError> {
    serde_json::from_value(body).map_err(|e| fail(400, format!("bad request: {e}")))
}

fn text<'a>(body: &'a Value, field: &str) -> Result<&'a str, ApiError> {
    body[field].as_str().ok_or_else(|| fail(400, format!("bad request: `{field}` is required")))
}

fn route(d: &Daemon, method: &str, parts: &[&str], body: Value, stop: &AtomicBool) -> Result<Value, ApiError> {
    match (method, parts) {
        ("GET", ["health"]) => Ok(json!({"ok": true, "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION")})),
        ("POST", ["shutdown"]) => {
            stop.store(true, Ordering::SeqCst);
            Ok(json!({}))
        }
        ("GET", ["accounts"]) => d.list_accounts(),
        ("POST", ["accounts", "capture"]) => d.capture(parse(body)?),
        ("PATCH", ["accounts", id]) => d.patch_account(id, parse(body)?),
        ("DELETE", ["accounts", id]) => d.remove_account(id),
        ("GET", ["sessions"]) => d.list_sessions(),
        ("POST", ["sessions"]) => d.start_session(parse(body)?),
        ("POST", ["sessions", id, "assign"]) => d.assign(id, text(&body, "account")?),
        ("POST", ["sessions", id, "swap"]) => d.swap_session(id, text(&body, "account")?),
        ("DELETE", ["sessions", id]) => Ok(d.end_session(id)),
        ("GET", ["policies"]) => Ok(d.policy_text()),
        ("PUT", ["policies"]) => d.put_policy(text(&body, "text")?),
        ("POST", ["policies", "parse"]) => d.parse_policy(text(&body, "text")?),
        ("POST", ["policies", "render"]) => d.render_policy(body["policy"].clone()),
        ("POST", ["policies", "dry-run"]) => d.dry_run(parse(body)?),
        ("GET", ["policies", "schema"]) => Ok(d.schema()),
        ("POST", ["hooks", "claude", sid, event]) => Ok(d.hook(sid, event, "", &body)),
        ("POST", ["hooks", "claude", sid, event, argument]) => Ok(d.hook(sid, event, argument, &body)),
        ("POST", ["statusline", "claude", sid]) => Ok(d.statusline(sid, &body)),
        ("GET", ["events"]) => Ok(d.events()),
        _ => Err(fail(404, "no such route")),
    }
}

fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle(d: &Daemon, token: &str, mut request: tiny_http::Request, stop: &AtomicBool) {
    let authorized = request
        .headers()
        .iter()
        .any(|h| h.field.equiv("Authorization") && h.value.as_str() == format!("Bearer {token}"));
    let result = if !authorized {
        Err(fail(401, "missing or wrong token"))
    } else {
        let mut text = String::new();
        let _ = request.as_reader().read_to_string(&mut text);
        let body = if text.trim().is_empty() { Ok(Value::Null) } else { serde_json::from_str(&text) };
        let url = request.url().split('?').next().unwrap_or_default().to_string();
        let decoded: Vec<String> = url.split('/').filter(|p| !p.is_empty()).map(percent_decode).collect();
        let parts: Vec<&str> = decoded.iter().map(String::as_str).collect();
        match body {
            Ok(body) => route(d, request.method().as_str(), &parts, body, stop),
            Err(e) => Err(fail(400, format!("bad request: {e}"))),
        }
    };
    let (status, body) = match result {
        Ok(value) => (200, value),
        Err(e) => (e.status, json!({"error": e.message})),
    };
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("static header");
    let _ = request.respond(tiny_http::Response::from_string(body.to_string()).with_status_code(status).with_header(header));
}

/// Start serving on a free loopback port and record it in `daemon.json`.
pub fn start(daemon: Arc<Daemon>) -> std::io::Result<Running> {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").map_err(std::io::Error::other)?);
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or_default();
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let endpoint = Endpoint { port, token: token.clone(), pid: std::process::id() };

    let path = daemon.data_dir().join("daemon.json");
    std::fs::write(&path, serde_json::to_string(&endpoint)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let workers = (0..WORKERS)
        .map(|_| {
            let (server, daemon, stop, token, path) = (server.clone(), daemon.clone(), stop.clone(), token.clone(), path.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(300)) {
                        handle(&daemon, &token, request, &stop);
                    }
                }
                let _ = std::fs::remove_file(&path);
            })
        })
        .collect();
    Ok(Running { endpoint, workers })
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn decodes_escapes_and_leaves_the_rest() {
        assert_eq!(percent_decode("work%201%2F2"), "work 1/2");
        assert_eq!(percent_decode("me@x.io"), "me@x.io");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
