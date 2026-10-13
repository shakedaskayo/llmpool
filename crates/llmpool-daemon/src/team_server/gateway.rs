//! The server as a gateway: members' agents send their requests here, and
//! the server forwards each one to the provider on an account from the pool.
//!
//! With this on, no login or key leaves the server at all. A member's agent
//! holds only the member's own session with us, and which account a request
//! runs on is decided here, per request: the one the member was last on while
//! it has room (so the provider's prompt cache stays warm), another when it is
//! filling up, cooling off after a refusal, or failing. That is the switch,
//! and the agent never sees it.
//!
//! Every request leaves one line in `gateway-traces.log`: who, which account,
//! which model, how long, how many tokens. Never what was said.

use super::*;
use llmpool_core::model::{Account, Window};
use llmpool_core::providers;
use std::collections::VecDeque;
use std::io::Write;

/// A request body larger than this is refused rather than held in memory.
const MAX_REQUEST: u64 = 32 << 20;
/// An account at or above this much of any window is left for later while
/// another has room.
const CEILING: f64 = 90.0;
/// How long an account that refused a request for being over its rate is
/// passed over, when the provider does not say.
const COOL_SECS: i64 = 60;
const ATTEMPTS: usize = 3;
/// Requests one member may have in flight at once. An agent with a few
/// helpers needs several; nobody needs so many that others are kept out.
const MAX_PER_MEMBER: usize = 16;
/// Free text from a request that ends up in a trace is kept this short.
const TRACE_TEXT: usize = 128;

/// What a member's agent may ask a provider for through the pool's
/// credentials: asking a model something, and listing models. Everything
/// else an account can do at a provider (its files, its billing, its keys,
/// other people's stored work) is not the gateway's to hand out.
fn allowed(method: &str, path: &str) -> bool {
    let path = path.split('?').next().unwrap_or_default();
    let clean = !path.is_empty()
        && path.len() <= 200
        && !path.contains("..")
        && !path.contains("//")
        && !path.contains('%')
        && !path.contains('\\')
        && path.bytes().all(|b| b.is_ascii_graphic());
    if !clean {
        return false;
    }
    // Some providers put the version elsewhere or leave it out.
    let tail = ["v1beta/", "v1/", "openai/v1/", "api/v1/"]
        .iter()
        .find_map(|prefix| path.strip_prefix(prefix))
        .unwrap_or(path);
    match method {
        "POST" => {
            matches!(
                tail,
                "messages"
                    | "messages/count_tokens"
                    | "chat/completions"
                    | "completions"
                    | "responses"
                    | "embeddings"
            ) || (tail.starts_with("models/")
                && [
                    ":generateContent",
                    ":streamGenerateContent",
                    ":countTokens",
                    ":embedContent",
                ]
                .iter()
                .any(|verb| tail.ends_with(verb)))
        }
        "GET" => tail == "models" || (tail.starts_with("models/") && !tail[7..].contains('/')),
        _ => false,
    }
}

fn short(text: &str) -> String {
    text.chars().take(TRACE_TEXT).collect()
}
/// How long an account the provider refused outright is passed over.
const REFUSED_SECS: i64 = 300;
/// Requests remembered for the server's page; the log file keeps them all.
const KEPT: usize = 2000;
/// The decision model is asked about one member and provider no more often
/// than this, and what it says holds for this long.
const ADVICE_SECS: i64 = 300;
/// An account below this much of its windows needs no second opinion.
const ASK_ABOVE: f64 = 50.0;

/// How many requests may be in flight at once: `LLMPOOL_GATEWAY_MAX`, or 256.
pub(super) fn max_concurrent() -> usize {
    std::env::var("LLMPOOL_GATEWAY_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(256)
}

#[derive(Default)]
pub(super) struct Gateway {
    /// The account each member's requests to a provider last ran on.
    sticky: BTreeMap<(String, String), AccountId>,
    /// Accounts to pass over, and until when.
    cooling: BTreeMap<AccountId, i64>,
    /// Accounts a provider refused outright for one member, and until when.
    /// Kept per member: what one member's request provokes must not take an
    /// account away from the others.
    refused: BTreeMap<(String, AccountId), i64>,
    /// Requests each member has in flight.
    in_flight: BTreeMap<String, usize>,
    recent: VecDeque<Value>,
    /// Where the decision model said a member's requests should go, and until when.
    preferred: BTreeMap<(String, String), (AccountId, i64)>,
    /// When it was last asked about each member and provider.
    asked: BTreeMap<(String, String), i64>,
    decisions: VecDeque<Value>,
}

impl Gateway {
    /// With what the log already holds, so a restart does not empty the page.
    pub(super) fn restored(data_dir: &std::path::Path) -> Self {
        let text = std::fs::read_to_string(data_dir.join("gateway-traces.log")).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let recent = lines
            .iter()
            .rev()
            .take(KEPT)
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Self {
            recent,
            ..Self::default()
        }
    }
}

/// One account a request could run on: what to send, and where.
struct Upstream {
    account: Account,
    credential: String,
    base: String,
}

/// Reads the token counts out of an answer as it streams past, without
/// keeping the answer.
#[derive(Default)]
struct Sniff {
    line: Vec<u8>,
    whole: Vec<u8>,
    input: u64,
    output: u64,
}

impl Sniff {
    fn note(&mut self, value: &Value) {
        for usage in [&value["usage"], &value["message"]["usage"]] {
            let read = |names: [&str; 2]| names.iter().filter_map(|n| usage[*n].as_u64()).max();
            if let Some(n) = read(["input_tokens", "prompt_tokens"]) {
                self.input = self.input.max(n);
            }
            if let Some(n) = read(["output_tokens", "completion_tokens"]) {
                self.output = self.output.max(n);
            }
        }
    }

    fn feed(&mut self, bytes: &[u8], events: bool) {
        if !events {
            // A plain answer is one document; a huge one is not worth holding.
            if self.whole.len() + bytes.len() <= 1 << 20 {
                self.whole.extend_from_slice(bytes);
            }
            return;
        }
        for byte in bytes {
            if *byte != b'\n' {
                if self.line.len() < 1 << 16 {
                    self.line.push(*byte);
                }
                continue;
            }
            let line = std::mem::take(&mut self.line);
            if let Some(data) = line.strip_prefix(b"data:") {
                if let Ok(value) = serde_json::from_slice::<Value>(data) {
                    self.note(&value);
                }
            }
        }
    }

    fn finish(&mut self) {
        if let Ok(value) = serde_json::from_slice::<Value>(&std::mem::take(&mut self.whole)) {
            self.note(&value);
        }
    }
}

/// Headers that belong to one hop, or that we set ourselves.
fn passes(name: &str) -> bool {
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "x-api-key"
            | "x-goog-api-key"
            | "host"
            | "content-length"
            | "connection"
            | "transfer-encoding"
            | "content-encoding"
            | "accept-encoding"
            | "keep-alive"
            | "proxy-authorization"
            | "te"
            | "upgrade"
            | "cookie"
            | "set-cookie"
            | "x-llmpool"
    )
}

pub(super) fn refuse(request: tiny_http::Request, status: u16, message: &str) {
    let _ = request.respond(
        tiny_http::Response::from_string(
            json!({"error": {"type": "llmpool_gateway", "message": message}}).to_string(),
        )
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json")),
    );
}

impl TeamServer {
    fn gateway_state(&self) -> std::sync::MutexGuard<'_, Gateway> {
        self.gateway.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The addresses and variables a member's agent needs to reach `provider`
    /// through us, with the member's own token as its key.
    pub(super) fn gateway_env(&self, provider: &str, token: &str) -> BTreeMap<String, String> {
        let base = format!("{}/gateway/{provider}", self.config.public_url);
        let upper = provider.to_uppercase().replace('-', "_");
        let (key, url) = match provider {
            // Claude Code takes a bearer token and a base address.
            "claude" => (
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                ("ANTHROPIC_BASE_URL".to_string(), base),
            ),
            "anthropic" => (
                "ANTHROPIC_API_KEY".to_string(),
                ("ANTHROPIC_BASE_URL".to_string(), base),
            ),
            _ => {
                let key = providers::env_of(provider)
                    .and_then(|names| names.first())
                    .map_or_else(|| format!("{upper}_API_KEY"), |n| n.to_string());
                // The convention for these is an address that ends in the version.
                (key, (format!("{upper}_BASE_URL"), format!("{base}/v1")))
            }
        };
        BTreeMap::from([(key, token.to_string()), url])
    }

    /// The accounts of `provider` this member may use right now, the best first.
    fn upstreams(&self, who: &str, provider: &str) -> Api<Vec<Upstream>> {
        let settings = self.settings();
        let moved = self.config.upstreams.get(provider).cloned();
        let default_base = moved
            .or_else(|| providers::upstream_of(provider).map(str::to_string))
            .ok_or_else(|| {
                fail(
                    404,
                    format!("this gateway does not know where `{provider}` lives"),
                )
            })?;
        let d = &self.daemon;
        let g = d.lock();
        let t = now();
        let mut state = self.gateway_state();
        state.cooling.retain(|_, until| *until > t);
        state.refused.retain(|_, until| *until > t);
        let pair = (who.to_string(), provider.to_string());
        let sticky = state.sticky.get(&pair).cloned();
        state.preferred.retain(|_, (_, until)| *until > t);
        let preferred = state.preferred.get(&pair).map(|(id, _)| id.clone());
        let mut found = Vec::new();
        for account in d.accounts(&g)? {
            if account.provider != provider
                || !account.usable()
                || g.renewing.contains(&account.id)
                || state.cooling.contains_key(&account.id)
                || state
                    .refused
                    .contains_key(&(who.to_string(), account.id.clone()))
                || !settings.opens(who, &account.tags)
            {
                continue;
            }
            let (credential, base) = if account.kind == Kind::Oauth {
                let Ok(secret) = d.secret_of(&account.id) else {
                    continue;
                };
                let Some(token) = secret.oauth["accessToken"].as_str() else {
                    continue;
                };
                (token.to_string(), default_base.clone())
            } else {
                // One entry that cannot be read is that account's trouble only.
                let Some(env) = d
                    .vault
                    .get(&account.id)
                    .ok()
                    .flatten()
                    .and_then(|t| crate::keys::env_of(&t).ok())
                else {
                    continue;
                };
                let ends = |ends: &[&str]| {
                    env.iter()
                        .find(|(name, _)| ends.iter().any(|e| name.ends_with(e)))
                        .map(|(_, value)| value.clone())
                };
                let Some(key) = ends(&["_API_KEY", "_KEY", "_TOKEN"]) else {
                    continue;
                };
                (
                    key,
                    ends(&["_BASE_URL", "_API_BASE", "_ENDPOINT"]).map_or_else(
                        || default_base.clone(),
                        |b| b.trim_end_matches('/').to_string(),
                    ),
                )
            };
            found.push(Upstream {
                account,
                credential,
                base,
            });
        }
        // What the decision model chose, if it has spoken lately; then where
        // the member already is, while it has room; then the emptiest.
        found.sort_by_key(|u| {
            (
                u.account.peak_used() >= CEILING,
                preferred.as_ref() != Some(&u.account.id),
                sticky.as_ref() != Some(&u.account.id),
                u.account.peak_used() as u64,
                u.account.id.clone(),
            )
        });
        Ok(found)
    }

    fn trace(&self, line: Value) {
        eprintln!("gateway {line}");
        append_line(
            &self.daemon.data_dir().join("gateway-traces.log"),
            &line.to_string(),
        );
        let mut state = self.gateway_state();
        state.recent.push_front(line);
        state.recent.truncate(KEPT);
    }

    pub(super) fn gateway_recent(&self) -> Vec<Value> {
        self.gateway_state()
            .recent
            .iter()
            .take(200)
            .cloned()
            .collect()
    }

    /// Everything the Requests page shows: the requests that match, and what
    /// they add up to.
    pub(super) fn gateway_report(&self, filter: &Value) -> Value {
        let state = self.gateway_state();
        let wanted = |name: &str, t: &Value| {
            filter[name]
                .as_str()
                .is_none_or(|want| want.is_empty() || t[name].as_str() == Some(want))
        };
        let failed = |t: &Value| t["status"].as_u64().unwrap_or(0) >= 400;
        let matching: Vec<&Value> = state
            .recent
            .iter()
            .filter(|t| wanted("who", t) && wanted("provider", t) && wanted("account", t))
            .filter(|t| match filter["result"].as_str() {
                Some("ok") => !failed(t),
                Some("failed") => failed(t),
                _ => true,
            })
            .collect();
        let sum =
            |name: &str, rows: &[&Value]| rows.iter().filter_map(|t| t[name].as_u64()).sum::<u64>();
        let mut times: Vec<u64> = matching.iter().filter_map(|t| t["ms"].as_u64()).collect();
        times.sort_unstable();
        let at = |q: f64| {
            times
                .get(((times.len() as f64 - 1.0) * q).round() as usize)
                .copied()
        };
        let group = |name: &str| -> Vec<Value> {
            let mut by: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
            for t in &matching {
                by.entry(t[name].as_str().unwrap_or("none").to_string())
                    .or_default()
                    .push(t);
            }
            let mut rows: Vec<Value> = by
                .iter()
                .map(|(key, rows)| {
                    json!({
                        "name": key, "provider": rows[0]["provider"], "requests": rows.len(),
                        "failed": rows.iter().filter(|t| failed(t)).count(),
                        "input_tokens": sum("input_tokens", rows), "output_tokens": sum("output_tokens", rows),
                    })
                })
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r["requests"].as_u64()));
            rows
        };
        // The last hour, a minute at a time, oldest first.
        let now_minute = now() / 60;
        let mut minutes = vec![0u64; 60];
        for t in &matching {
            let ago = now_minute - t["at"].as_i64().unwrap_or(0) / 60;
            if (0..60).contains(&ago) {
                minutes[59 - ago as usize] += 1;
            }
        }
        let distinct = |name: &str| -> Vec<&str> {
            let mut all: Vec<&str> = state
                .recent
                .iter()
                .filter_map(|t| t[name].as_str())
                .collect();
            all.sort_unstable();
            all.dedup();
            all
        };
        json!({
            "requests": matching.iter().take(200).collect::<Vec<_>>(),
            "totals": {
                "requests": matching.len(), "failed": matching.iter().filter(|t| failed(t)).count(),
                "input_tokens": sum("input_tokens", &matching), "output_tokens": sum("output_tokens", &matching),
                "p50_ms": at(0.5), "p95_ms": at(0.95),
                "moved": matching.iter().filter(|t| t["tried"].as_array().is_some_and(|a| !a.is_empty())).count(),
            },
            "by_account": group("account"),
            "by_member": group("who"),
            "per_minute": minutes,
            "members": distinct("who"), "providers": distinct("provider"), "accounts": distinct("account"),
            "decisions": state.decisions.iter().take(50).collect::<Vec<_>>(),
            "in_flight": self.active.load(std::sync::atomic::Ordering::SeqCst),
            "limit": max_concurrent(),
            "kept": state.recent.len(),
        })
    }

    /// Ask the decision model where this member's requests to a provider
    /// should go next, if it is switched on and there is something to decide.
    /// Never on a request's own time: this runs after the answer has gone out.
    fn consider(&self, who: &str, provider: &str) {
        let d = &self.daemon;
        let policy = d.advisor_settings();
        if !policy.enabled {
            return;
        }
        let pair = (who.to_string(), provider.to_string());
        let settings = self.settings();
        let t = now();
        let (candidates, load, history, current) = {
            // The daemon's lock before the gateway's, here as everywhere:
            // two orders would one day wait on each other for ever.
            let mut g = d.lock();
            let mut state = self.gateway_state();
            if state
                .asked
                .get(&pair)
                .is_some_and(|at| t - at < ADVICE_SECS)
            {
                return;
            }
            let Ok(accounts) = d.accounts(&g) else {
                return;
            };
            let candidates: Vec<Account> = accounts
                .into_iter()
                .filter(|a| {
                    a.provider == provider
                        && a.usable()
                        && a.peak_used() < 100.0
                        && !state.cooling.contains_key(&a.id)
                        && !state.refused.contains_key(&(who.to_string(), a.id.clone()))
                        && settings.opens(who, &a.tags)
                })
                .collect();
            let current = state.sticky.get(&pair).cloned();
            let busy = current
                .as_ref()
                .and_then(|id| candidates.iter().find(|a| &a.id == id))
                .is_none_or(|a| a.peak_used() >= ASK_ABOVE);
            if candidates.len() < 2 || !busy || !d.spend_call(&mut g, &policy) {
                return;
            }
            state.asked.insert(pair.clone(), t);
            // How many members each account is carrying right now.
            let mut load = BTreeMap::new();
            for id in state.sticky.values() {
                *load.entry(id.clone()).or_insert(0usize) += 1;
            }
            (candidates, load, g.p.history.clone(), current)
        };
        let whole = llmpool_core::policy::rule::PolicyFile {
            advisor: policy,
            ..Default::default()
        };
        let chosen = d.advise_start(&candidates, &load, &whole, &history);
        let label = |id: &AccountId| {
            candidates
                .iter()
                .find(|a| &a.id == id)
                .map_or_else(|| id.clone(), |a| a.label.clone())
        };
        let mut state = self.gateway_state();
        let record = match &chosen {
            Some(id) => {
                state
                    .preferred
                    .insert(pair, (id.clone(), t + ADVICE_SECS * 2));
                json!({"at": t, "who": who, "provider": provider, "chose": label(id),
                       "was_on": current.as_ref().map(label), "among": candidates.len(),
                       "moved": current.as_ref() != Some(id)})
            }
            None => json!({"at": t, "who": who, "provider": provider, "chose": null,
                           "was_on": current.as_ref().map(label), "among": candidates.len(),
                           "note": "the model did not answer; the rules decide"}),
        };
        state.decisions.push_front(record.clone());
        state.decisions.truncate(200);
        drop(state);
        if let Some(id) = &chosen {
            self.audit(
                "gateway_advised",
                who,
                &format!("{provider}: the decision model chose {}", label(id)),
            );
        }
    }

    /// What a subscription account's answer says about how full it is.
    fn learn_usage(&self, account: &Account, response: &ureq::Response) {
        let mut usage = account.usage.clone().unwrap_or_default();
        let mut seen = false;
        for window in ["5h", "7d"] {
            let read = |what: &str| {
                response.header(&format!("anthropic-ratelimit-unified-{window}-{what}"))
            };
            let Some(used) = read("utilization").and_then(|v| v.parse::<f64>().ok()) else {
                continue;
            };
            seen = true;
            usage.windows.insert(
                window.to_string(),
                Window {
                    // Sent as a fraction of the window.
                    used_percent: if used <= 1.5 { used * 100.0 } else { used },
                    resets_at: read("reset").and_then(|v| v.parse().ok()),
                },
            );
        }
        if !seen {
            return;
        }
        usage.fetched_at = now();
        let g = self.daemon.lock();
        if let Ok(Some(mut current)) = g.store.get(&account.id) {
            current.usage = Some(usage);
            let _ = g.store.put(&current);
        }
    }

    /// Forward one request of a member's agent to a provider.
    pub(super) fn gateway(&self, mut request: tiny_http::Request) {
        let started = std::time::Instant::now();
        let header_of = |request: &tiny_http::Request, name: &str| {
            request
                .headers()
                .iter()
                .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
                .map(|h| h.value.as_str().to_string())
        };
        let token = header_of(&request, "Authorization")
            .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
            .or_else(|| header_of(&request, "x-api-key"))
            .or_else(|| header_of(&request, "x-goog-api-key"));
        if !self.settings().gateway {
            return refuse(request, 404, "this server's gateway is off");
        }
        let who = match self.member(token.as_deref()) {
            Ok((_, who)) => who,
            Err(e) => return refuse(request, e.status, &e.message),
        };
        let url = request.url().to_string();
        let mut parts = url.trim_start_matches("/gateway/").splitn(2, '/');
        let provider = parts.next().unwrap_or_default().to_string();
        let rest = parts.next().unwrap_or_default().to_string();
        let method = request.method().as_str().to_string();
        if !llmpool_core::providers::name_ok(&provider) || !allowed(&method, &rest) {
            return refuse(
                request,
                403,
                "this gateway carries requests to models and nothing else an account can do",
            );
        }
        // Counted for as long as this request lives, however it ends.
        struct Slot<'a>(&'a TeamServer, String);
        impl Drop for Slot<'_> {
            fn drop(&mut self) {
                let mut state = self.0.gateway_state();
                if let Some(n) = state.in_flight.get_mut(&self.1) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        state.in_flight.remove(&self.1);
                    }
                }
            }
        }
        {
            let mut state = self.gateway_state();
            let mine = state.in_flight.entry(who.clone()).or_insert(0);
            if *mine >= MAX_PER_MEMBER {
                drop(state);
                return refuse(
                    request,
                    429,
                    "you have too many requests in flight; let some finish",
                );
            }
            *mine += 1;
        }
        let _slot = Slot(self, who.clone());
        let mut body = Vec::new();
        if request
            .as_reader()
            .take(MAX_REQUEST + 1)
            .read_to_end(&mut body)
            .is_err()
            || body.len() as u64 > MAX_REQUEST
        {
            return refuse(request, 413, "the request is too large for this gateway");
        }
        let model = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v["model"].as_str().map(short));
        let upstreams = match self.upstreams(&who, &provider) {
            Ok(found) if !found.is_empty() => found,
            Ok(_) => {
                return refuse(
                    request,
                    503,
                    &format!("no {provider} account in the pool is free for you right now"),
                )
            }
            Err(e) => return refuse(request, e.status, &e.message),
        };
        let forwarded: Vec<(String, String)> = request
            .headers()
            .iter()
            .filter(|h| passes(h.field.as_str().as_str()))
            .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
            .collect();
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(600))
            .redirects(0)
            .build();

        let mut tried = Vec::new();
        let mut answer = None;
        for upstream in upstreams.into_iter().take(ATTEMPTS) {
            let mut call = agent.request(&method, &format!("{}/{rest}", upstream.base));
            let mut beta = None;
            for (name, value) in &forwarded {
                if name.eq_ignore_ascii_case("anthropic-beta") {
                    beta = Some(value.clone());
                } else {
                    call = call.set(name, value);
                }
            }
            call = match (provider.as_str(), upstream.account.kind) {
                // A subscription token is a bearer token, and says so.
                ("claude", _) => {
                    let mut betas: Vec<&str> = beta
                        .as_deref()
                        .unwrap_or_default()
                        .split(',')
                        .map(str::trim)
                        .filter(|b| !b.is_empty())
                        .collect();
                    if !betas.contains(&"oauth-2025-04-20") {
                        betas.push("oauth-2025-04-20");
                    }
                    call.set("Authorization", &format!("Bearer {}", upstream.credential))
                        .set("anthropic-beta", &betas.join(","))
                }
                ("anthropic", _) => {
                    let call = call.set("x-api-key", &upstream.credential);
                    match &beta {
                        Some(beta) => call.set("anthropic-beta", beta),
                        None => call,
                    }
                }
                ("gemini", _) => call.set("x-goog-api-key", &upstream.credential),
                _ => call.set("Authorization", &format!("Bearer {}", upstream.credential)),
            };
            let sent = if body.is_empty() && method == "GET" {
                call.call()
            } else {
                call.send_bytes(&body)
            };
            let response = match sent {
                Ok(response) => response,
                Err(ureq::Error::Status(_, response)) => response,
                Err(e) => {
                    tried.push(format!(
                        "{}: unreachable ({})",
                        upstream.account.label,
                        e.kind()
                    ));
                    continue;
                }
            };
            let status = response.status();
            // Refusals that are about the account, not the request: try the
            // next account, and leave this one alone for a while.
            // Refusals that are about the account or the provider's moment,
            // not about the request, and that mean nothing was done with it:
            // only those are worth another account. (A plain 500 may have
            // run; sending it again could run, and bill, it twice.)
            if matches!(status, 401 | 403 | 429 | 502 | 503 | 529) {
                if matches!(status, 401 | 403) {
                    // Set aside for this member only: it may be their request
                    // the provider objects to, not the account.
                    let fresh = self
                        .gateway_state()
                        .refused
                        .insert(
                            (who.clone(), upstream.account.id.clone()),
                            now() + REFUSED_SECS,
                        )
                        .is_none();
                    if fresh {
                        self.audit(
                            "account_refused",
                            &who,
                            &format!(
                                "{provider}: the provider answered {status} on {}",
                                upstream.account.label
                            ),
                        );
                    }
                }
                if status == 429 {
                    let wait = response
                        .header("retry-after")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(COOL_SECS)
                        .clamp(5, 300);
                    self.gateway_state()
                        .cooling
                        .insert(upstream.account.id.clone(), now() + wait);
                }
                tried.push(format!("{}: {status}", upstream.account.label));
                answer = Some((upstream, response));
                continue;
            }
            answer = Some((upstream, response));
            break;
        }
        let Some((upstream, response)) = answer else {
            self.trace(json!({"at": now(), "who": who, "provider": provider, "model": model, "status": 502, "tried": tried}));
            return refuse(
                request,
                502,
                "no account in the pool could reach the provider",
            );
        };
        let status = response.status();
        if upstream.account.kind == Kind::Oauth {
            self.learn_usage(&upstream.account, &response);
        }
        if status < 400 {
            let key = (who.clone(), provider.clone());
            let previous = self
                .gateway_state()
                .sticky
                .insert(key, upstream.account.id.clone());
            if previous.as_ref().is_some_and(|p| *p != upstream.account.id) {
                self.audit(
                    "gateway_switch",
                    &who,
                    &format!("{provider}: now on {}", upstream.account.label),
                );
            }
        }

        // Written by hand, a piece at a time: an answer that streams must
        // reach the agent as it arrives, not when a buffer fills.
        let events = response.content_type() == "text/event-stream";
        let mut head = format!("HTTP/1.1 {status} {}\r\n", response.status_text());
        for name in response.headers_names() {
            if passes(&name) {
                if let Some(value) = response.header(&name) {
                    head += &format!("{name}: {value}\r\n");
                }
            }
        }
        head += "Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        let mut out = request.into_writer();
        let mut reader = response.into_reader();
        let mut sniff = Sniff::default();
        let mut bytes = 0usize;
        let mut buffer = [0u8; 16 * 1024];
        let mut ok = out.write_all(head.as_bytes()).is_ok();
        while ok {
            let n = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            bytes += n;
            sniff.feed(&buffer[..n], events);
            ok = write!(out, "{n:x}\r\n").is_ok()
                && out.write_all(&buffer[..n]).is_ok()
                && out.write_all(b"\r\n").is_ok()
                && out.flush().is_ok();
        }
        if ok {
            let _ = out.write_all(b"0\r\n\r\n");
            let _ = out.flush();
        }
        sniff.finish();
        // The agent has its answer; nothing below is on its time.
        drop(out);
        let took = started.elapsed().as_millis() as u64;
        self.trace(json!({
            "at": now(), "who": who, "provider": provider, "account": upstream.account.label,
            "model": model, "method": method, "path": rest.split('?').next().map(short), "status": status,
            "ms": took, "bytes": bytes,
            "input_tokens": sniff.input, "output_tokens": sniff.output,
            "tried": tried, "streamed": events,
        }));
        self.consider(&who, &provider);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_are_read_from_plain_and_streamed_answers() {
        let mut plain = Sniff::default();
        plain.feed(br#"{"usage":{"prompt_tokens":12,"#, false);
        plain.feed(br#""completion_tokens":34}}"#, false);
        plain.finish();
        assert_eq!((plain.input, plain.output), (12, 34));

        let mut stream = Sniff::default();
        stream.feed(b"event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":1}}}\n\nda", true);
        stream.feed(
            b"ta: {\"usage\":{\"output_tokens\":21}}\n\ndata: [DONE]\n",
            true,
        );
        stream.finish();
        assert_eq!((stream.input, stream.output), (7, 21));
    }

    #[test]
    fn credentials_and_hop_headers_are_never_forwarded() {
        for name in [
            "Authorization",
            "x-api-key",
            "Cookie",
            "Host",
            "Content-Length",
            "X-Llmpool",
        ] {
            assert!(!passes(name), "{name}");
        }
        for name in [
            "anthropic-version",
            "content-type",
            "user-agent",
            "x-stainless-lang",
        ] {
            assert!(passes(name), "{name}");
        }
    }
}
