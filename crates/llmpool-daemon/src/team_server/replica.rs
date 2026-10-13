//! A gateway replica: a second process that carries members' requests and
//! keeps nothing. The primary still holds every login, renews them, signs
//! people in and serves the page; a replica asks it who a token is and what
//! their request may run on, forwards the request itself, and tells the
//! primary how it went. Refresh tokens never come here.

use super::gateway::{refuse, Report, Upstream};
use crate::{fail, Api};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an answer from the primary is used without asking again.
const FRESH: Duration = Duration::from_secs(15);
/// How long it may still be used while the primary cannot be reached: a
/// restart of the primary then costs members' agents nothing.
const USABLE: Duration = Duration::from_secs(300);
/// The largest request passed on to the primary's own pages.
const MAX_RELAY: u64 = 16 * 1024 * 1024;

type Picked = (Instant, String, Vec<Upstream>);

pub(super) struct Primary {
    url: String,
    key: String,
    agent: ureq::Agent,
    fresh: Duration,
    /// By (hash of the member's token, provider).
    cache: Mutex<BTreeMap<(String, String), Picked>>,
    /// Set when the primary did not answer: until then it is not asked
    /// again for what is already known, so its trouble is not every
    /// request's wait.
    away_until: Mutex<Option<Instant>>,
}

impl Primary {
    pub(super) fn new(url: &str, key: &str, fresh: Option<Duration>) -> Self {
        Self {
            fresh: fresh.unwrap_or(FRESH),
            url: url.trim_end_matches('/').to_string(),
            key: key.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout(Duration::from_secs(20))
                .redirects(0)
                .build(),
            cache: Mutex::default(),
            away_until: Mutex::default(),
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), Picked>> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Who this token is, and the accounts their request may run on.
    pub(super) fn pick(&self, token: Option<&str>, provider: &str) -> Api<(String, Vec<Upstream>)> {
        let at = (
            super::sha256_hex(token.unwrap_or_default()),
            provider.to_string(),
        );
        let known = self.cache().get(&at).cloned();
        if let Some((when, who, upstreams)) = &known {
            let away = self
                .away_until
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some_and(|until| Instant::now() < until);
            if when.elapsed() < self.fresh || (away && when.elapsed() < USABLE) {
                return Ok((who.clone(), upstreams.clone()));
            }
        }
        let asked = self
            .agent
            .post(&format!("{}/replica/pick", self.url))
            .set("Authorization", &format!("Bearer {}", self.key))
            .timeout(Duration::from_secs(5))
            .send_json(json!({"token": token, "provider": provider}));
        match asked {
            Ok(response) => {
                let answer: Value = response.into_json().map_err(|_| {
                    fail(502, "the pool's primary gave an answer that cannot be read")
                })?;
                let who = answer["who"].as_str().unwrap_or_default().to_string();
                let upstreams: Vec<Upstream> = serde_json::from_value(answer["upstreams"].clone())
                    .map_err(|_| {
                        fail(502, "the pool's primary gave an answer that cannot be read")
                    })?;
                let mut cache = self.cache();
                if cache.len() > 2000 {
                    cache.retain(|_, (when, ..)| when.elapsed() < USABLE);
                }
                cache.insert(at, (Instant::now(), who.clone(), upstreams.clone()));
                Ok((who, upstreams))
            }
            // The primary's word on this member is final, whatever was known.
            Err(ureq::Error::Status(status, response)) => {
                self.cache().remove(&at);
                let said: Value = response.into_json().unwrap_or_default();
                Err(fail(
                    status,
                    said["error"]
                        .as_str()
                        .unwrap_or("the pool refused this request"),
                ))
            }
            // Out of reach: what it last said holds for a few minutes.
            Err(_) => {
                *self.away_until.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(Instant::now() + Duration::from_secs(10));
                match known {
                    Some((when, who, upstreams)) if when.elapsed() < USABLE => Ok((who, upstreams)),
                    _ => Err(fail(503, "the pool's primary cannot be reached; try again")),
                }
            }
        }
    }

    /// Tell the primary how a request went. Never on the request's own time.
    pub(super) fn report(&self, report: Report) {
        // An account the provider just refused is not offered from here again.
        if !report.refusals.is_empty() {
            let mut cache = self.cache();
            for (account, ..) in &report.refusals {
                for (_, _, upstreams) in cache.values_mut() {
                    upstreams.retain(|u| &u.account.id != account);
                }
            }
        }
        if let Some(line) = &report.trace {
            eprintln!("gateway {line}");
        }
        let (agent, url, key) = (self.agent.clone(), self.url.clone(), self.key.clone());
        std::thread::spawn(move || {
            let sent = agent
                .post(&format!("{url}/replica/report"))
                .set("Authorization", &format!("Bearer {key}"))
                .send_json(serde_json::to_value(&report).unwrap_or_default());
            if let Err(e) = sent {
                eprintln!("llmpool: the primary did not take a report: {}", e.kind());
            }
        });
    }

    /// Everything that is not a member's request to a model belongs to the
    /// primary: pass it on and hand its answer back unchanged.
    pub(super) fn relay(&self, mut request: tiny_http::Request) {
        // What a primary and its replicas say to each other is not passed
        // on for anyone else.
        if request.url().starts_with("/replica/") {
            return refuse(request, 404, "not found");
        }
        let mut body = Vec::new();
        if request
            .as_reader()
            .take(MAX_RELAY + 1)
            .read_to_end(&mut body)
            .is_err()
            || body.len() as u64 > MAX_RELAY
        {
            return refuse(request, 413, "the request is too large");
        }
        let mut call = self.agent.request(
            request.method().as_str(),
            &format!("{}{}", self.url, request.url()),
        );
        for h in request.headers() {
            let name = h.field.as_str().as_str();
            if !hop(name) {
                call = call.set(name, h.value.as_str());
            }
        }
        let sent = if body.is_empty() {
            call.call()
        } else {
            call.send_bytes(&body)
        };
        let response = match sent {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(_) => {
                return refuse(
                    request,
                    503,
                    "the pool's primary cannot be reached; try again",
                )
            }
        };
        let status = response.status();
        let headers: Vec<tiny_http::Header> = response
            .headers_names()
            .into_iter()
            .filter(|name| !hop(name))
            .flat_map(|name| {
                response
                    .all(&name)
                    .into_iter()
                    .filter_map(|value| {
                        tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).ok()
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut answer = Vec::new();
        let _ = response
            .into_reader()
            .take(MAX_RELAY)
            .read_to_end(&mut answer);
        let mut reply = tiny_http::Response::from_data(answer).with_status_code(status);
        for header in headers {
            reply = reply.with_header(header);
        }
        let _ = request.respond(reply);
    }
}

/// Headers that belong to one connection, not to the request.
fn hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "upgrade"
            | "te"
            | "trailer"
    )
}
