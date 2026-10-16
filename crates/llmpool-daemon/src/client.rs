//! Talking to a running daemon over its local HTTP API.

use crate::http::Endpoint;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where llmpool keeps its data. `LLMPOOL_HOME` overrides the default.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LLMPOOL_HOME") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/llmpool")
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(xdg).join("llmpool")
    } else {
        home().join(".local/share/llmpool")
    }
}

/// Present in the data directory while llmpool is turned off: agents start
/// on the user's own login, as if llmpool were not installed, and nothing
/// switches them.
pub const OFF_FILE: &str = "off";

pub fn is_off() -> bool {
    data_dir().join(OFF_FILE).exists()
}

/// Which build of llmpool this is. Shared by everything built together.
pub const BUILD: &str = env!("LLMPOOL_BUILD");

pub struct Client {
    base: String,
    auth: String,
    agent: ureq::Agent,
}

impl Client {
    fn to(endpoint: &Endpoint, timeout: Duration) -> Self {
        Self {
            base: format!("http://127.0.0.1:{}", endpoint.port),
            auth: format!("Bearer {}", endpoint.token),
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
        }
    }

    /// A client for the running daemon, or `None` if there is none.
    ///
    /// A daemon left running by an earlier version of llmpool is asked to
    /// stop and reported as absent, so the caller starts the current one: an
    /// old daemon does not know what a new client asks for, and one unknown
    /// route is enough to leave a whole window empty.
    pub fn existing(timeout: Duration) -> Option<Self> {
        let client = Self::to(&Endpoint::read(&data_dir())?, timeout);
        let health = client.call("GET", "/health", None).ok()?;
        if health["build"].as_str() == Some(BUILD) {
            return Some(client);
        }
        let _ = client.call("POST", "/shutdown", None);
        for _ in 0..60 {
            if Endpoint::read(&data_dir()).is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    pub fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let request = self
            .agent
            .request(method, &format!("{}{path}", self.base))
            .set("Authorization", &self.auth);
        let response = match body {
            Some(body) => request.send_json(body),
            None => request.call(),
        };
        match response {
            Ok(response) => response.into_json().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(_, response)) => {
                let body: Value = response.into_json().unwrap_or_default();
                Err(body["error"]
                    .as_str()
                    .unwrap_or("request failed")
                    .to_string())
            }
            Err(e) => Err(format!("daemon unreachable: {e}")),
        }
    }
}
