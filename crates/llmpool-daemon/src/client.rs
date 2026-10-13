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
    pub fn existing(timeout: Duration) -> Option<Self> {
        let client = Self::to(&Endpoint::read(&data_dir())?, timeout);
        client.call("GET", "/health", None).ok().map(|_| client)
    }

    pub fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let request = self.agent.request(method, &format!("{}{path}", self.base)).set("Authorization", &self.auth);
        let response = match body {
            Some(body) => request.send_json(body),
            None => request.call(),
        };
        match response {
            Ok(response) => response.into_json().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(_, response)) => {
                let body: Value = response.into_json().unwrap_or_default();
                Err(body["error"].as_str().unwrap_or("request failed").to_string())
            }
            Err(e) => Err(format!("daemon unreachable: {e}")),
        }
    }
}
