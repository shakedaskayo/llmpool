//! Serving this machine's pool to signed-in teammates.
//!
//! People sign in with the organisation's identity provider (Google unless
//! configured otherwise) and receive a session token of ours. With it their
//! own llmpool fetches the shared rules and, for the accounts they may use,
//! short-lived access tokens. Refresh tokens never leave this machine.
//!
//! This listener speaks plain HTTP; put it behind something that terminates
//! TLS. Clients refuse a server that is not `https` unless it is on loopback.

mod admin;
mod gateway;

use crate::http::percent_decode;
use crate::{fail, now, Api, ApiError, Daemon};
use base64::Engine;
use llmpool_core::model::{AccountId, Kind, Usage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A sign-in may sit in the browser this long.
const PENDING_SECS: i64 = 600;
/// The code handed to the client's loopback address is good for this long.
const GRANT_SECS: i64 = 120;
/// How long a session of ours lasts before the person signs in again.
const SESSION_SECS: i64 = 30 * 24 * 3600;
/// A browser's session lasts as long as its cookie.
const WEB_SESSION_SECS: i64 = 12 * 3600;
/// Logins one member may hold for themselves at once, and the sessions they
/// may claim on any one account.
const MAX_SEATS: usize = 3;
const MAX_SESSIONS: usize = 64;
/// A seat is given up when its holder has not reported using it for this long.
const SEAT_SECS: i64 = 180;
const MAX_PENDING: usize = 256;
const MAX_BODY: u64 = 1 << 20;
/// Requests other than the gateway's answered at once.
const MAX_CONTROL: usize = 64;
/// Wrong tokens tolerated in ten seconds before everyone without a right one
/// is told to wait: guessing must be slow even from many addresses at once.
const MAX_FAILURES: usize = 40;

#[derive(Default)]
pub struct Config {
    /// Where people's browsers reach this server, e.g. `https://pool.example.com`.
    pub public_url: String,
    /// Sign-in through an OpenID Connect provider (Google, Okta, Keycloak,
    /// Entra, ...). `None`: API keys only.
    pub oidc: Option<Oidc>,
    /// Members who connect with a key instead of signing in: name to the
    /// SHA-256 of their key.
    pub api_keys: BTreeMap<String, String>,
    /// Who may sign in through the provider: anyone whose verified address is in one of these
    /// domains, or is one of these addresses. Both empty admits nobody.
    pub domains: Vec<String>,
    pub emails: Vec<String>,
    /// Let several people run on one subscription login at once. Off, a login
    /// in use by one person is not handed to another.
    pub share_logins: bool,
    /// Which accounts each member may use: a member pattern (an address or
    /// key name, `*@domain`, or `*`) and the tags it opens (`*` for all).
    /// Empty: every member may use every account.
    pub access: Vec<(String, Vec<String>)>,
    /// Members turned away whatever else admits them.
    pub blocked: Vec<String>,
    /// Members who may run the server from its web page: addresses or key
    /// names. Empty: nobody, and the page says so.
    pub admins: Vec<String>,
    /// Be a gateway: members' agents send their requests here, and no login
    /// or key leaves the server.
    pub gateway: bool,
    /// Where the gateway reaches a provider, when not at its usual address:
    /// a proxy of the organisation's, a compatible endpoint, or a provider
    /// llmpool has not heard of. Provider name to base address.
    pub upstreams: BTreeMap<String, String>,
}

/// What an administrator can change while the server runs. It starts as the
/// deployment says and, once changed, is kept in `team-settings.json`.
#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    share_logins: bool,
    access: Vec<(String, Vec<String>)>,
    blocked: Vec<String>,
    #[serde(default)]
    gateway: bool,
    /// What the pool is called on its invitation, e.g. the organisation's name.
    #[serde(default)]
    name: String,
}

pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
}

/// A key shorter than this is too easy to guess to guard a pool of logins.
const MIN_KEY: usize = 24;

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let need = |name: &str| var(name).ok_or_else(|| format!("{name} is not set"));
        let list = |name: &str| -> Vec<String> {
            var(name)
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let config = Self {
            public_url: need("LLMPOOL_PUBLIC_URL")?
                .trim_end_matches('/')
                .to_string(),
            oidc: match var("LLMPOOL_OIDC_CLIENT_ID") {
                Some(client_id) => Some(Oidc {
                    issuer: var("LLMPOOL_OIDC_ISSUER")
                        .unwrap_or_else(|| "https://accounts.google.com".into())
                        .trim_end_matches('/')
                        .to_string(),
                    client_id,
                    client_secret: need("LLMPOOL_OIDC_CLIENT_SECRET")?,
                }),
                None => None,
            },
            api_keys: Self::keys(&var("LLMPOOL_API_KEYS").unwrap_or_default())?,
            domains: list("LLMPOOL_ALLOWED_DOMAINS"),
            emails: list("LLMPOOL_ALLOWED_EMAILS"),
            share_logins: var("LLMPOOL_SHARE_LOGINS").is_some_and(|v| v == "1" || v == "true"),
            access: Self::access(&var("LLMPOOL_ACCESS").unwrap_or_default())?,
            blocked: list("LLMPOOL_BLOCKED"),
            admins: list("LLMPOOL_ADMINS"),
            gateway: var("LLMPOOL_GATEWAY").is_some_and(|v| v == "1" || v == "true"),
            // `LLMPOOL_UPSTREAM_<PROVIDER>=https://...`
            upstreams: std::env::vars()
                .filter_map(|(name, url)| {
                    let provider = name.strip_prefix("LLMPOOL_UPSTREAM_")?;
                    let url = url.trim().trim_end_matches('/').to_string();
                    (!url.is_empty()).then(|| (provider.to_lowercase().replace('_', "-"), url))
                })
                .collect(),
        };
        if config.oidc.is_none() && config.api_keys.is_empty() {
            return Err("set LLMPOOL_OIDC_CLIENT_ID (sign-in) or LLMPOOL_API_KEYS: with neither, nobody can connect".into());
        }
        if config.oidc.is_some() && config.domains.is_empty() && config.emails.is_empty() {
            return Err(
                "set LLMPOOL_ALLOWED_DOMAINS or LLMPOOL_ALLOWED_EMAILS: with neither, nobody may sign in"
                    .into(),
            );
        }
        Ok(config)
    }

    /// `name:key` pairs, separated by commas or line breaks.
    pub fn keys(text: &str) -> Result<BTreeMap<String, String>, String> {
        let mut keys = BTreeMap::new();
        for pair in text
            .split([',', '\n'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let (name, key) = pair
                .split_once(':')
                .map(|(n, k)| (n.trim(), k.trim()))
                .filter(|(n, _)| !n.is_empty())
                .ok_or("LLMPOOL_API_KEYS: each entry is `name:key`")?;
            if key.len() < MIN_KEY {
                return Err(format!(
                    "LLMPOOL_API_KEYS: the key of `{name}` is shorter than {MIN_KEY} characters"
                ));
            }
            keys.insert(name.to_string(), sha256_hex(key));
        }
        Ok(keys)
    }

    /// `pattern=tag,tag` entries, separated by semicolons or line breaks.
    pub fn access(text: &str) -> Result<Vec<(String, Vec<String>)>, String> {
        text.split([';', '\n'])
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let (who, tags) = entry
                    .split_once('=')
                    .ok_or("LLMPOOL_ACCESS: each entry is `member=tag,tag`")?;
                let tags: Vec<String> = tags
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                if who.trim().is_empty() || tags.is_empty() {
                    return Err("LLMPOOL_ACCESS: each entry is `member=tag,tag`".to_string());
                }
                Ok((who.trim().to_lowercase(), tags))
            })
            .collect()
    }

    fn admits(&self, email: &str) -> bool {
        let email = email.to_lowercase();
        let domain = email.rsplit('@').next().unwrap_or_default();
        self.emails.contains(&email) || self.domains.iter().any(|d| d == domain)
    }
}

impl Settings {
    /// Whether `who` may use an account carrying `tags`.
    fn opens(&self, who: &str, tags: &std::collections::BTreeSet<String>) -> bool {
        if self.access.is_empty() {
            return true;
        }
        let who = who.to_lowercase();
        self.access
            .iter()
            .filter(|(pattern, _)| match pattern.strip_prefix("*@") {
                Some(domain) => who.rsplit_once('@').is_some_and(|(_, d)| d == domain),
                None => pattern == "*" || *pattern == who,
            })
            .any(|(_, open)| open.iter().any(|t| t == "*" || tags.contains(t)))
    }

    fn blocks(&self, who: &str) -> bool {
        self.blocked.contains(&who.to_lowercase())
    }
}

struct Pending {
    port: u16,
    challenge: String,
    client_state: String,
    nonce: String,
    at: i64,
}

struct Grant {
    challenge: String,
    email: String,
    at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Member {
    email: String,
    expires_at: i64,
    /// Signed in on the web page with an API key rather than the provider:
    /// good for as long as that key is configured.
    #[serde(default)]
    by_key: bool,
}

#[derive(Default)]
struct State {
    pending: BTreeMap<String, Pending>,
    grants: BTreeMap<String, Grant>,
    /// By the SHA-256 of the session token, so the file holds nothing usable.
    members: BTreeMap<String, Member>,
    /// Who runs how many sessions on which account, and when they last said so.
    seats: BTreeMap<AccountId, BTreeMap<String, (i64, usize)>>,
    /// When tokens that opened nothing were last shown to us.
    failures: std::collections::VecDeque<i64>,
    /// Who has been lent what since the server started, so each is recorded once.
    lent: std::collections::BTreeSet<(String, AccountId)>,
}

pub struct TeamServer {
    config: Config,
    daemon: Arc<Daemon>,
    state: Mutex<State>,
    /// The identity provider's (issuer, authorization, token) addresses.
    provider: Mutex<Option<(String, String, String)>>,
    members_path: PathBuf,
    settings: Mutex<Settings>,
    gateway: Mutex<gateway::Gateway>,
    /// Gateway requests in flight right now.
    active: std::sync::atomic::AtomicUsize,
    /// Every other request being answered right now.
    busy: std::sync::atomic::AtomicUsize,
}

enum Reply {
    Json(Value),
    Redirect(String),
    Page(u16, String),
    /// A file of the web page: its type and its text.
    Asset(&'static str, String),
    /// Start (or with `None` end) a browser session, then go somewhere.
    Session(Option<String>, String),
}

/// Everything about one request that deciding on it can need.
struct Ask<'a> {
    method: &'a str,
    path: &'a str,
    query: &'a BTreeMap<String, String>,
    bearer: Option<&'a str>,
    /// The browser session, from its cookie.
    cookie: Option<&'a str>,
    /// Sent by our own page's script; a form on another site cannot add it.
    ours: bool,
    body: &'a Value,
    text: &'a str,
}

/// A log file is started afresh at this size; the one before is kept beside it.
const LOG_MAX: u64 = 32 << 20;

/// Add a line to a log, keeping the file and its predecessor to a bounded
/// size: a full volume would stop the server from saving anything else.
pub(crate) fn append_line(path: &std::path::Path, line: &str) {
    append_bounded(path, line, LOG_MAX)
}

fn append_bounded(path: &std::path::Path, line: &str, max: u64) {
    use std::io::Write;
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// A place in a count, given back when the work ends, however it ends.
struct Counted<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Set when the process is asked to stop (SIGTERM or SIGINT).
static TERMINATING: AtomicBool = AtomicBool::new(false);

extern "C" fn note_termination(_signal: libc::c_int) {
    TERMINATING.store(true, Ordering::SeqCst);
}

/// Start noticing requests to stop. Returns the flag that says one came.
pub fn watch_for_termination() -> &'static AtomicBool {
    let handler = note_termination as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    &TERMINATING
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("the system has no randomness");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub(crate) fn query_of(url: &str) -> BTreeMap<String, String> {
    url.split_once('?')
        .map(|(_, q)| q)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (percent_decode(k), percent_decode(&v.replace('+', " "))))
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>llmpool</title>\
         <body style=\"font:15px/1.5 -apple-system,system-ui,sans-serif;max-width:32em;margin:18vh auto;padding:0 1em\">\
         <h1 style=\"font-size:18px\">{}</h1><p>{}</p>",
        escape(title),
        escape(text)
    )
}

fn denied(text: &str) -> ApiError {
    fail(403, text)
}

/// The claims of an ID token. Its signature is not checked here: the token
/// came straight from the provider's token endpoint over verified TLS, which
/// OpenID Connect Core 3.1.3.7 accepts in place of the signature check.
/// Never use this on a token that arrived any other way.
fn claims_of(id_token: &str) -> Option<Value> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

impl TeamServer {
    pub fn new(daemon: Arc<Daemon>, config: Config) -> std::io::Result<Self> {
        let members_path = daemon.data_dir().join("team-members.json");
        let members = std::fs::read_to_string(&members_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // What an administrator set is kept or the server does not start:
        // falling back to the deployment's values could reopen what they
        // closed (a blocked member, lending instead of the gateway).
        let saved = daemon.data_dir().join("team-settings.json");
        let settings = match std::fs::read_to_string(&saved) {
            Ok(text) => Some(serde_json::from_str(&text).map_err(|e| {
                std::io::Error::other(format!(
                    "{} cannot be read ({e}); repair or remove it before starting",
                    saved.display()
                ))
            })?),
            Err(_) => None,
        }
        .unwrap_or_else(|| Settings {
            share_logins: config.share_logins,
            access: config.access.clone(),
            blocked: config.blocked.clone(),
            gateway: config.gateway,
            name: String::new(),
        });
        Ok(Self {
            settings: Mutex::new(settings),
            gateway: Mutex::new(gateway::Gateway::restored(daemon.data_dir())),
            active: std::sync::atomic::AtomicUsize::new(0),
            busy: std::sync::atomic::AtomicUsize::new(0),
            config,
            daemon,
            state: Mutex::new(State {
                members,
                ..State::default()
            }),
            provider: Mutex::new(None),
            members_path,
        })
    }

    /// One line per thing worth knowing afterwards: who came in, who was
    /// turned away, who was lent or took which account. To standard error
    /// (a container's log) and to `team-audit.log`. Never a secret.
    fn audit(&self, event: &str, who: &str, detail: &str) {
        let line = json!({
            "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "event": event, "who": who, "detail": detail,
        })
        .to_string();
        eprintln!("audit {line}");
        append_line(&self.daemon.data_dir().join("team-audit.log"), &line);
    }

    fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save_members(&self, state: &State) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = self.members_path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| {
                f.write_all(
                    serde_json::to_string(&state.members)
                        .unwrap_or_default()
                        .as_bytes(),
                )
            });
        if written.is_ok() {
            let _ = std::fs::rename(tmp, &self.members_path);
        }
    }

    /// The provider's addresses, from its discovery document, read once.
    fn provider(&self) -> Api<(String, String, String)> {
        let mut cached = self.provider.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(found) = cached.as_ref() {
            return Ok(found.clone());
        }
        let url = format!("{}/.well-known/openid-configuration", self.oidc()?.issuer);
        let doc: Value = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .get(&url)
            .call()
            .map_err(|e| fail(502, format!("the sign-in provider is unreachable: {e}")))?
            .into_json()
            .map_err(|e| fail(502, format!("the sign-in provider answered oddly: {e}")))?;
        let field = |name: &str| {
            doc[name]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| fail(502, format!("the sign-in provider names no {name}")))
        };
        let found = (
            field("issuer")?,
            field("authorization_endpoint")?,
            field("token_endpoint")?,
        );
        *cached = Some(found.clone());
        Ok(found)
    }

    fn oidc(&self) -> Api<&Oidc> {
        self.config
            .oidc
            .as_ref()
            .ok_or_else(|| fail(404, "this pool has no sign-in; connect with an API key"))
    }

    fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.config.public_url)
    }

    /// Step one: the client's browser arrives, saying where on its own
    /// machine to come back to and what it will prove itself with.
    fn login(&self, query: &BTreeMap<String, String>) -> Api<Reply> {
        let get = |name: &str| query.get(name).map(String::as_str).unwrap_or_default();
        // From the server's own page there is no client to hand back to:
        // port 0 marks a sign-in that ends in a browser session here.
        let web = get("web") == "1";
        let port: u16 = if web {
            0
        } else {
            get("port")
                .parse()
                .ok()
                .filter(|p| *p >= 1024)
                .ok_or_else(|| fail(400, "bad request: `port` is required"))?
        };
        let (challenge, client_state) = (get("challenge"), get("state"));
        let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !web && (!hex(challenge, 64) || !hex(client_state, 32)) {
            return Err(fail(
                400,
                "bad request: `challenge` and `state` are required",
            ));
        }
        let oidc = self.oidc()?;
        let (_, authorize, _) = self.provider()?;
        let (state_id, nonce) = (random_hex(16), random_hex(16));
        {
            let mut state = self.state();
            let t = now();
            state.pending.retain(|_, p| t - p.at < PENDING_SECS);
            // A flood of started sign-ins pushes out the oldest of its own
            // kind; it cannot keep real people from starting theirs.
            while state.pending.len() >= MAX_PENDING {
                let Some(oldest) = state
                    .pending
                    .iter()
                    .min_by_key(|(_, p)| p.at)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                state.pending.remove(&oldest);
            }
            state.pending.insert(
                state_id.clone(),
                Pending {
                    port,
                    challenge: challenge.into(),
                    client_state: client_state.into(),
                    nonce: nonce.clone(),
                    at: t,
                },
            );
        }
        let mut url = format!(
            "{authorize}{}response_type=code&scope=openid%20email&client_id={}&redirect_uri={}&state={state_id}&nonce={nonce}&prompt=select_account",
            if authorize.contains('?') { '&' } else { '?' },
            url_encode(&oidc.client_id),
            url_encode(&self.redirect_uri()),
        );
        // A hint only; the address is checked when it comes back.
        if let ([domain], true) = (
            self.config.domains.as_slice(),
            self.config.emails.is_empty(),
        ) {
            url += &format!("&hd={}", url_encode(domain));
        }
        Ok(Reply::Redirect(url))
    }

    /// Step two: the provider sends the browser back with a code. We trade it
    /// for the person's identity, decide whether they may come in, and send
    /// the browser on to the client that started this.
    fn callback(&self, query: &BTreeMap<String, String>) -> Api<Reply> {
        let get = |name: &str| query.get(name).map(String::as_str).unwrap_or_default();
        let pending = self
            .state()
            .pending
            .remove(get("state"))
            .filter(|p| now() - p.at < PENDING_SECS)
            .ok_or_else(|| fail(400, "this sign-in was not started here, or took too long"))?;
        if !get("error").is_empty() {
            return Err(denied(&format!(
                "the sign-in was not completed ({})",
                get("error")
            )));
        }
        let oidc = self.oidc()?;
        let (issuer, _, token_url) = self.provider()?;
        let answer: Value = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .post(&token_url)
            .send_form(&[
                ("grant_type", "authorization_code"),
                ("code", get("code")),
                ("redirect_uri", &self.redirect_uri()),
                ("client_id", &oidc.client_id),
                ("client_secret", &oidc.client_secret),
            ])
            .map_err(|_| fail(502, "the sign-in provider did not accept the sign-in"))?
            .into_json()
            .map_err(|_| fail(502, "the sign-in provider answered oddly"))?;
        let claims = answer["id_token"]
            .as_str()
            .and_then(claims_of)
            .ok_or_else(|| fail(502, "the sign-in provider sent no identity"))?;
        let audience_ok = match &claims["aud"] {
            Value::String(aud) => *aud == oidc.client_id,
            Value::Array(all) => all.iter().any(|a| a == oidc.client_id.as_str()),
            _ => false,
        };
        let verified = claims["email_verified"] == true || claims["email_verified"] == "true";
        let email = claims["email"].as_str().unwrap_or_default().to_lowercase();
        if claims["iss"].as_str() != Some(issuer.as_str())
            || !audience_ok
            || claims["nonce"].as_str() != Some(pending.nonce.as_str())
            || claims["exp"].as_i64().is_none_or(|exp| exp < now())
        {
            return Err(denied(
                "the identity the provider sent was not meant for this sign-in",
            ));
        }
        if email.is_empty() || !verified {
            return Err(denied(
                "the provider has not verified an email address for you",
            ));
        }
        if !self.config.admits(&email) || self.settings().blocks(&email) {
            return Err(denied(&format!("{email} is not allowed to use this pool")));
        }
        if pending.port == 0 {
            return Ok(Reply::Session(
                Some(self.open_session(&email, false, WEB_SESSION_SECS)),
                "/".into(),
            ));
        }
        let code = random_hex(24);
        {
            let mut state = self.state();
            let t = now();
            state.grants.retain(|_, g| t - g.at < GRANT_SECS);
            state.grants.insert(
                code.clone(),
                Grant {
                    challenge: pending.challenge,
                    email,
                    at: t,
                },
            );
        }
        // Only ever to the person's own machine.
        Ok(Reply::Redirect(format!(
            "http://127.0.0.1:{}/callback?code={code}&state={}",
            pending.port, pending.client_state
        )))
    }

    /// Step three: the client proves it is the one that started the sign-in
    /// and receives its session.
    fn exchange(&self, body: &Value) -> Api<Reply> {
        let text = |name: &str| body[name].as_str().unwrap_or_default();
        let mut state = self.state();
        let t = now();
        let grant = state
            .grants
            .remove(text("code"))
            .filter(|g| t - g.at < GRANT_SECS && g.challenge == sha256_hex(text("verifier")))
            .ok_or_else(|| fail(400, "this sign-in code is unknown, used or too old"))?;
        drop(state);
        let token = self.open_session(&grant.email, false, SESSION_SECS);
        let expires_at = t + SESSION_SECS;
        Ok(Reply::Json(
            json!({"token": token, "email": grant.email, "expires_at": expires_at}),
        ))
    }

    /// A new session of ours for `email`; the token is returned once and
    /// only its hash is kept.
    fn open_session(&self, email: &str, by_key: bool, lasts: i64) -> String {
        let token = random_hex(32);
        let t = now();
        let mut state = self.state();
        state.members.retain(|_, m| m.expires_at > t);
        state.members.insert(
            sha256_hex(&token),
            Member {
                email: email.to_string(),
                expires_at: t + lasts,
                by_key,
            },
        );
        self.save_members(&state);
        drop(state);
        self.audit("sign_in", email, if by_key { "with a key" } else { "" });
        token
    }

    fn member(&self, bearer: Option<&str>) -> Api<(String, String)> {
        let found = self.member_of(bearer);
        if found.as_ref().is_err_and(|e| e.status == 401) {
            let mut state = self.state();
            let t = now();
            state.failures.push_back(t);
            while state.failures.front().is_some_and(|at| t - at > 10) {
                state.failures.pop_front();
            }
            if state.failures.len() > MAX_FAILURES {
                return Err(fail(429, "too many wrong tokens; wait a little"));
            }
        }
        found
    }

    fn member_of(&self, bearer: Option<&str>) -> Api<(String, String)> {
        let key = sha256_hex(bearer.unwrap_or_default());
        // A key member is whoever the server's configuration says holds it.
        if let Some((name, _)) = self.config.api_keys.iter().find(|(_, hash)| **hash == key) {
            if self.settings().blocks(name) {
                return Err(fail(401, "you are no longer allowed to use this pool"));
            }
            return Ok((key, name.clone()));
        }
        let member = self
            .state()
            .members
            .get(&key)
            .filter(|m| m.expires_at > now())
            .cloned()
            .ok_or_else(|| fail(401, "sign in to this pool again"))?;
        // Leaving the allow list ends the session, not only future sign-ins.
        let still = if member.by_key {
            self.config.api_keys.contains_key(&member.email)
        } else {
            self.config.admits(&member.email)
        };
        if !still || self.settings().blocks(&member.email) {
            return Err(fail(401, "you are no longer allowed to use this pool"));
        }
        Ok((key, member.email))
    }

    /// What one teammate's llmpool asks every minute: here is what I am
    /// running on and what I saw of its usage; what may I use?
    fn sync(&self, email: &str, body: &Value) -> Api<Reply> {
        let using: BTreeMap<AccountId, usize> =
            serde_json::from_value(body["using"].clone()).unwrap_or_default();
        let usage: BTreeMap<AccountId, Usage> =
            serde_json::from_value(body["usage"].clone()).unwrap_or_default();
        let settings = self.settings();
        let d = &self.daemon;
        let g = d.lock();
        let accounts = d.accounts(&g)?;
        let mut state = self.state();
        let t = now();
        let seated = |state: &State| -> std::collections::BTreeSet<AccountId> {
            state
                .seats
                .iter()
                .filter(|(_, holders)| holders.contains_key(email))
                .map(|(id, _)| id.clone())
                .collect()
        };
        let before = seated(&state);
        for holders in state.seats.values_mut() {
            holders.retain(|who, (seen, _)| who != email && t - *seen < SEAT_SECS);
        }
        let mut rows = Vec::new();
        let mut news = Vec::new();
        let mut taken = 0usize;
        for mut account in accounts {
            if !account.usable() || g.renewing.contains(&account.id) {
                continue;
            }
            // Another agent's own login renews itself wherever it runs, so a
            // copy on every member's machine would end it: it is not lent.
            if account.kind == Kind::Oauth && account.provider != "claude" {
                continue;
            }
            if !settings.opens(email, &account.tags) {
                continue;
            }
            let holders = state.seats.entry(account.id.clone()).or_default();
            let others: usize = holders.values().map(|(_, n)| n.min(&MAX_SESSIONS)).sum();
            let exclusive = account.kind == Kind::Oauth && !settings.share_logins;
            let held = exclusive && others > 0;
            // A member's word for what they run on is taken within limits:
            // nobody reserves the whole pool by saying they use all of it.
            let seated = match (using.get(&account.id), held) {
                (Some(sessions), false) if !exclusive || taken < MAX_SEATS => {
                    holders.insert(email.to_string(), (t, (*sessions).clamp(1, MAX_SESSIONS)));
                    taken += usize::from(exclusive);
                    true
                }
                _ => false,
            };
            // The server runs no sessions; what it knows of usage is what
            // the members running on an account tell it, and only they.
            if let (Some(seen), true) = (usage.get(&account.id), seated) {
                let sane = seen.fetched_at <= t
                    && seen.windows.len() <= 8
                    && seen.windows.iter().all(|(name, w)| {
                        name.len() <= 16 && (0.0..=100.0).contains(&w.used_percent)
                    });
                if sane
                    && account
                        .usage
                        .as_ref()
                        .is_none_or(|have| seen.fetched_at > have.fetched_at)
                {
                    account.usage = Some(seen.clone());
                    g.store.put(&account)?;
                }
            }
            let secret = if held {
                Value::Null
            } else if account.kind != Kind::Oauth {
                // A key is lent whole: there is nothing shorter-lived to give.
                // One entry that cannot be read is that account's trouble only.
                match d.vault.get(&account.id).ok().flatten() {
                    Some(text) if crate::keys::env_of(&text).is_ok() => {
                        serde_json::from_str(&text)?
                    }
                    _ => continue,
                }
            } else {
                match d.secret_of(&account.id) {
                    Ok(mut secret) => {
                        // The part that can mint new tokens stays here.
                        if let Some(oauth) = secret.oauth.as_object_mut() {
                            oauth.remove("refreshToken");
                            oauth.remove("refreshTokenExpiresAt");
                        }
                        serde_json::to_value(&secret)?
                    }
                    Err(_) => continue,
                }
            };
            if !secret.is_null() && state.lent.insert((email.to_string(), account.id.clone())) {
                news.push(("lent", account.label.clone()));
            }
            account.login_expires_at = None;
            rows.push(
                json!({"account": account, "secret": secret, "others": others, "held": held}),
            );
        }
        let after = seated(&state);
        let label = |id: &AccountId| {
            g.store
                .get(id)
                .ok()
                .flatten()
                .map_or_else(|| id.clone(), |a| a.label)
        };
        news.extend(
            after
                .difference(&before)
                .map(|id| ("seat_taken", label(id))),
        );
        news.extend(
            before
                .difference(&after)
                .map(|id| ("seat_released", label(id))),
        );
        drop(state);
        for (event, account) in news {
            self.audit(event, email, &account);
        }
        let policy = std::fs::read_to_string(d.policy_path()).unwrap_or_default();
        let admin = self.config.admins.contains(&email.to_lowercase());
        Ok(Reply::Json(
            json!({"email": email, "policy": policy, "accounts": rows, "admin": admin}),
        ))
    }

    /// What a member is lent while the server is a gateway: for each
    /// provider the pool can serve them, one account that is the gateway
    /// itself. Its key is the member's own session with us; the real logins
    /// and keys stay here.
    fn sync_gateway(&self, email: &str, token: &str) -> Api<Reply> {
        let settings = self.settings();
        let d = &self.daemon;
        let g = d.lock();
        let mut providers = std::collections::BTreeSet::new();
        for account in d.accounts(&g)? {
            let lendable = account.kind != Kind::Oauth || account.provider == "claude";
            if account.usable() && lendable && settings.opens(email, &account.tags) {
                providers.insert(account.provider);
            }
        }
        let rows: Vec<Value> = providers
            .iter()
            .filter(|p| {
                llmpool_core::providers::upstream_of(p).is_some()
                    || self.config.upstreams.contains_key(*p)
            })
            .map(|provider| {
                json!({
                    "account": {
                        "id": format!("gateway-{provider}"),
                        "provider": provider,
                        "kind": "api_key",
                        "label": format!("team gateway ({provider})"),
                        "email": null, "organization": null,
                        "tags": ["gateway"],
                        "enabled": true, "health": "ok", "usage": null, "login_expires_at": null,
                    },
                    "secret": {"env": self.gateway_env(provider, token)},
                    "others": 0, "held": false,
                })
            })
            .collect();
        let policy = std::fs::read_to_string(d.policy_path()).unwrap_or_default();
        let admin = self.config.admins.contains(&email.to_lowercase());
        Ok(Reply::Json(
            json!({"email": email, "policy": policy, "accounts": rows, "gateway": true, "admin": admin}),
        ))
    }

    fn route(&self, ask: &Ask) -> Api<Reply> {
        let Ask {
            method,
            path,
            query,
            bearer,
            body,
            ..
        } = *ask;
        match (method, path) {
            ("GET", "/health") => Ok(Reply::Json(json!({
                "ok": true,
                "service": "llmpool",
                "sign_in": self.config.oidc.as_ref().map(|o| o.issuer.as_str()),
                "api_keys": !self.config.api_keys.is_empty(),
                "gateway": self.settings().gateway,
            }))),
            ("GET", "/auth/login") => self.login(query),
            ("GET", "/auth/callback") => self.callback(query),
            ("POST", "/auth/exchange") => self.exchange(body),
            ("GET", "/v1/me") => {
                let (_, who) = self.member(bearer)?;
                let admin = self.config.admins.contains(&who.to_lowercase());
                Ok(Reply::Json(json!({"email": who, "admin": admin})))
            }
            // An administrator hands an account from their own machine to the pool.
            ("POST", "/v1/accounts") => {
                let (_, who) = self.member(bearer)?;
                if !self.config.admins.contains(&who.to_lowercase()) {
                    return Err(fail(
                        403,
                        "only an administrator of this pool can add accounts to it",
                    ));
                }
                let account = serde_json::from_value(body["account"].clone())
                    .map_err(|_| fail(400, "bad request: `account` is required"))?;
                let secret = body["secret"]
                    .as_str()
                    .ok_or_else(|| fail(400, "bad request: `secret` is required"))?;
                let taken = self.daemon.receive(account, secret.to_string())?;
                let label = taken["added"][0]
                    .as_str()
                    .or(taken["replaced"][0].as_str())
                    .unwrap_or_default()
                    .to_string();
                self.audit("account_added", &who, &label);
                Ok(Reply::Json(json!({"label": label})))
            }
            ("POST", "/auth/logout") => {
                let (key, _) = self.member(bearer)?;
                let mut state = self.state();
                let gone = state.members.remove(&key);
                self.save_members(&state);
                drop(state);
                if let Some(member) = gone {
                    self.audit("sign_out", &member.email, "");
                }
                Ok(Reply::Json(json!({})))
            }
            ("POST", "/v1/sync") => {
                let (_, email) = self.member(bearer)?;
                if self.settings().gateway {
                    return self.sync_gateway(&email, bearer.unwrap_or_default());
                }
                self.sync(&email, body)
            }
            _ => self.web(ask),
        }
    }

    /// Take a request off the accepting thread. A slow or stalled client then
    /// holds a thread of its own, never one of the few that accept.
    /// What every page of ours tells the browser: run only our own scripts,
    /// show us in no frame, send no referrer, guess no types, and come back
    /// over TLS only.
    fn guarded<R: std::io::Read>(
        &self,
        response: tiny_http::Response<R>,
    ) -> tiny_http::Response<R> {
        let response = response
            .with_header(header(
                "Content-Security-Policy",
                "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                 object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
            ))
            .with_header(header("X-Frame-Options", "DENY"))
            .with_header(header("X-Content-Type-Options", "nosniff"))
            .with_header(header("Referrer-Policy", "no-referrer"))
            .with_header(header("Cache-Control", "no-store"));
        if self.config.public_url.starts_with("https://") {
            response.with_header(header(
                "Strict-Transport-Security",
                "max-age=31536000; includeSubDomains",
            ))
        } else {
            response
        }
    }

    fn dispatch(self: &Arc<Self>, request: tiny_http::Request) {
        if request.url().starts_with("/gateway/") {
            return self.handle(request);
        }
        // Whether the server is alive is answered at once and counts against
        // nothing: a crowd at the door must not get the process restarted.
        if request.url() == "/health" && *request.method() == tiny_http::Method::Get {
            return self.handle(request);
        }
        if self.busy.fetch_add(1, Ordering::SeqCst) >= MAX_CONTROL {
            self.busy.fetch_sub(1, Ordering::SeqCst);
            return gateway::refuse(request, 503, "the server is busy; try again");
        }
        let server = self.clone();
        std::thread::spawn(move || {
            let _slot = Counted(&server.busy);
            server.handle(request);
        });
    }

    fn handle(self: &Arc<Self>, mut request: tiny_http::Request) {
        if request.url().starts_with("/gateway/") {
            // A request to a model can run for minutes; each gets a thread of
            // its own so the few that accept connections are never all busy.
            let limit = gateway::max_concurrent();
            if self.active.fetch_add(1, Ordering::SeqCst) >= limit {
                self.active.fetch_sub(1, Ordering::SeqCst);
                return gateway::refuse(
                    request,
                    503,
                    "this gateway is at its limit of requests in flight; try again",
                );
            }
            let server = self.clone();
            std::thread::spawn(move || {
                let _slot = Counted(&server.active);
                server.gateway(request);
            });
            return;
        }
        let bearer = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
            .map(str::to_string);
        let mut text = String::new();
        // Nothing is waited for where nothing is expected.
        if *request.method() != tiny_http::Method::Get {
            let _ = request.as_reader().take(MAX_BODY).read_to_string(&mut text);
        }
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or_default().to_string();
        let browser = path.starts_with("/auth/") && *request.method() == tiny_http::Method::Get;
        let cookie = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Cookie"))
            .and_then(|h| {
                h.value
                    .as_str()
                    .split(';')
                    .find_map(|pair| pair.trim().strip_prefix("llmpool_session="))
                    .map(str::to_string)
            });
        let ours = request.headers().iter().any(|h| h.field.equiv("X-Llmpool"));
        let query = query_of(&url);
        let result = self.route(&Ask {
            method: request.method().as_str(),
            path: &path,
            query: &query,
            bearer: bearer.as_deref(),
            cookie: cookie.as_deref(),
            ours,
            body: &body,
            text: &text,
        });
        if let (Err(e), "/auth/callback") = (&result, path.as_str()) {
            self.audit("sign_in_denied", "", &e.message);
        }
        let reply = match result {
            Ok(reply) => reply,
            // A person is looking at this one.
            Err(e) if browser => Reply::Page(e.status, page("Sign-in did not work", &e.message)),
            Err(e) => {
                let _ = request.respond(
                    tiny_http::Response::from_string(json!({"error": e.message}).to_string())
                        .with_status_code(e.status)
                        .with_header(header("Content-Type", "application/json")),
                );
                return;
            }
        };
        let _ = match reply {
            Reply::Json(value) => request.respond(
                tiny_http::Response::from_string(value.to_string())
                    .with_header(header("Content-Type", "application/json"))
                    .with_header(header("Cache-Control", "no-store")),
            ),
            Reply::Redirect(to) => request.respond(
                tiny_http::Response::empty(302)
                    .with_header(header("Location", &to))
                    .with_header(header("Cache-Control", "no-store")),
            ),
            Reply::Page(status, html) => request.respond(
                self.guarded(
                    tiny_http::Response::from_string(html)
                        .with_status_code(status)
                        .with_header(header("Content-Type", "text/html; charset=utf-8")),
                ),
            ),
            Reply::Asset(kind, text) => request.respond(self.guarded(
                tiny_http::Response::from_string(text).with_header(header("Content-Type", kind)),
            )),
            Reply::Session(token, to) => {
                // Readable by no script, sent to no other site, and only
                // over TLS wherever the server is reached over TLS.
                let secure = if self.config.public_url.starts_with("https://") {
                    "; Secure"
                } else {
                    ""
                };
                let cookie = match token {
                    Some(token) => format!(
                        "llmpool_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=43200{secure}"
                    ),
                    None => format!("llmpool_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}"),
                };
                // A page rather than a redirect: the browser only sends a
                // strict cookie on a move our own page starts, and the move
                // that brought it here may have started at the provider.
                let html = format!(
                    "<!doctype html><meta charset=utf-8><meta http-equiv=refresh content=\"0;url={}\"><title>llmpool</title>",
                    escape(&to)
                );
                request.respond(
                    self.guarded(
                        tiny_http::Response::from_string(html)
                            .with_header(header("Content-Type", "text/html; charset=utf-8"))
                            .with_header(header("Set-Cookie", &cookie)),
                    ),
                )
            }
        };
    }
}

pub(crate) fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
        .unwrap_or_else(|_| tiny_http::Header::from_bytes(&b"X-Bad"[..], &b"1"[..]).unwrap())
}

pub struct Serving {
    pub port: u16,
    stop: Arc<AtomicBool>,
    team: Arc<TeamServer>,
}

impl Serving {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stop taking new requests and give those in flight up to `patience`
    /// to finish: an answer half-streamed to an agent is worth waiting for.
    pub fn drain(&self, patience: Duration) {
        self.stop();
        let until = std::time::Instant::now() + patience;
        while std::time::Instant::now() < until
            && self.team.active.load(Ordering::SeqCst) + self.team.busy.load(Ordering::SeqCst) > 0
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Serve the pool on `listen` (e.g. `0.0.0.0:8787`) until stopped.
pub fn start(daemon: Arc<Daemon>, config: Config, listen: &str) -> std::io::Result<Serving> {
    let server = Arc::new(tiny_http::Server::http(listen).map_err(std::io::Error::other)?);
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let team = Arc::new(TeamServer::new(daemon, config)?);
    let stop = Arc::new(AtomicBool::new(false));
    for _ in 0..4 {
        let (server, team, stop) = (server.clone(), team.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(300)) {
                    team.dispatch(request);
                }
            }
        });
    }
    Ok(Serving { port, stop, team })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(domains: &[&str], emails: &[&str]) -> Config {
        Config {
            public_url: "https://pool.example".into(),
            oidc: None,
            api_keys: BTreeMap::new(),
            domains: domains.iter().map(|s| s.to_string()).collect(),
            emails: emails.iter().map(|s| s.to_string()).collect(),
            share_logins: false,
            ..Config::default()
        }
    }

    #[test]
    fn access_rules_open_accounts_by_tag_per_member() {
        let tags = |t: &[&str]| t.iter().map(|s| s.to_string()).collect();
        let mut c = Settings {
            share_logins: false,
            access: vec![],
            blocked: vec![],
            gateway: false,
            name: String::new(),
        };
        assert!(c.opens("ann@x.io", &tags(&["anything"])));
        c.access = Config::access("*@x.io=work; Ann@x.io = vip ;ci=ci\nboss@x.io=*").unwrap();
        assert!(c.opens("bob@x.io", &tags(&["work"])));
        assert!(!c.opens("bob@x.io", &tags(&["vip"])));
        assert!(c.opens("ann@x.io", &tags(&["vip"])));
        assert!(c.opens("boss@x.io", &tags(&[])));
        assert!(c.opens("ci", &tags(&["ci", "x"])));
        assert!(!c.opens("ci", &tags(&["work"])));
        assert!(!c.opens("eve@y.io", &tags(&["work"])));
        assert!(!c.opens("bob@x.io", &tags(&[])));
        assert!(Config::access("nonsense").is_err());
        assert!(Config::access("a=").is_err());
    }

    #[test]
    fn admits_by_domain_or_address_and_nobody_by_default() {
        let c = config(&["x.io"], &["guest@y.io"]);
        assert!(c.admits("Ann@X.io"));
        assert!(c.admits("guest@y.io"));
        assert!(!c.admits("ann@y.io"));
        // A domain is matched whole, not as an ending.
        assert!(!c.admits("ann@evilx.io"));
        assert!(!c.admits("ann@x.io.evil.com"));
        assert!(!config(&[], &[]).admits("ann@x.io"));
    }

    #[test]
    fn a_log_starts_afresh_when_it_is_big_and_keeps_the_one_before() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("x.log");
        for i in 0..40 {
            append_bounded(&log, &format!("line {i:03} of some length"), 200);
        }
        let now = std::fs::read_to_string(&log).unwrap();
        let before = std::fs::read_to_string(dir.path().join("x.log.1")).unwrap();
        assert!(
            now.len() < 260 && before.len() < 260,
            "{} {}",
            now.len(),
            before.len()
        );
        assert!(now.contains("line 039") && !now.contains("line 000"));
        // Nothing but the two files, however long it runs.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn api_keys_are_named_hashed_and_long_enough() {
        let keys =
            Config::keys("ci:0123456789abcdefghijklmnop,\n ann@x.io : zyxwvutsrqponmlkjihgfedcba")
                .unwrap();
        assert_eq!(keys["ci"], sha256_hex("0123456789abcdefghijklmnop"));
        assert!(keys.contains_key("ann@x.io"));
        assert!(Config::keys("ci:short").is_err());
        assert!(Config::keys("nokey").is_err());
        assert!(Config::keys("").unwrap().is_empty());
    }

    #[test]
    fn reads_claims_and_query_and_escapes_pages() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"email":"a@x.io","aud":"client"}"#);
        let claims = claims_of(&format!("h.{payload}.s")).unwrap();
        assert_eq!(claims["email"], "a@x.io");
        assert!(claims_of("not-a-token").is_none());
        let q = query_of("/auth/callback?code=a%2Fb&state=s+1");
        assert_eq!(q["code"], "a/b");
        assert_eq!(q["state"], "s 1");
        assert!(page("t", "<script>").contains("&lt;script&gt;"));
        assert_eq!(url_encode("a b/c"), "a%20b%2Fc");
    }
}
