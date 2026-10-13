//! This machine's membership of a team's shared pool: connecting to the
//! server (by signing in or with an API key), and once a minute fetching the
//! team's rules and the accounts it lends us.
//!
//! What the server lends is an access token only. It cannot be renewed here,
//! so the server's copy is the truth and ours is replaced whenever it changes.

use super::*;
use crate::team_server::{header, page, query_of, random_hex, sha256_hex, url_encode};
use std::sync::Arc;

/// Where the vault keeps our session with (or key for) the server.
pub(crate) const TOKEN_ID: &str = "team-session";
/// How long a sign-in may sit in the browser.
const CONNECT_PATIENCE_SECS: i64 = 300;
/// The tag every account from the server carries, for rules and for the eye.
pub const TEAM_TAG: &str = "team";

#[derive(Clone, Default, Serialize, Deserialize)]
struct Link {
    url: String,
    email: String,
    synced_at: i64,
    error: Option<String>,
    /// Whether the server lets us add accounts to its pool.
    #[serde(default)]
    admin: bool,
}

/// A sign-in to a server waiting in the browser.
pub(crate) struct Job {
    url: String,
    verifier: String,
    auth_url: String,
    started: i64,
    /// The code the browser brought back, or why none will come.
    result: Arc<Mutex<Option<Result<String, String>>>>,
    failed: Option<String>,
}

fn slot<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// A server address we are willing to send a credential to: `https`, or
/// plain `http` on this machine only.
fn normalize(url: &str) -> Api<String> {
    let url = url.trim().trim_end_matches('/');
    let local = url.strip_prefix("http://").is_some_and(|rest| {
        let host = rest.split('/').next().unwrap_or_default();
        let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
        matches!(host, "localhost" | "127.0.0.1" | "[::1]")
    });
    if !(local || url.starts_with("https://") && url.len() > "https://".len()) {
        return Err(fail(
            400,
            "the server address must start with https:// (http:// only for this machine)",
        ));
    }
    Ok(url.to_string())
}

fn call(
    url: &str,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, (u16, String)> {
    let mut request = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        // A redirect could carry our token to another host.
        .redirects(0)
        .build()
        .request(method, &format!("{url}{path}"));
    if let Some(token) = token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    let response = match body {
        Some(body) => request.send_json(body),
        None => request.call(),
    };
    match response {
        Ok(response) if response.status() == 200 => response
            .into_json()
            .map_err(|_| (502, "the server answered oddly".to_string())),
        Ok(response) => Err((response.status(), "the server answered oddly".into())),
        Err(ureq::Error::Status(status, response)) => {
            let body: Value = response.into_json().unwrap_or_default();
            Err((
                status,
                body["error"]
                    .as_str()
                    .unwrap_or("the server refused")
                    .to_string(),
            ))
        }
        Err(e) => Err((0, format!("the server is unreachable: {e}"))),
    }
}

fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Wait on our own machine for the browser to come back from the server.
fn await_callback(
    server: tiny_http::Server,
    state: String,
    result: Arc<Mutex<Option<Result<String, String>>>>,
) {
    let deadline = now() + CONNECT_PATIENCE_SECS;
    loop {
        // Cancelled, or replaced by a newer attempt.
        if slot(&result).is_some() {
            return;
        }
        if now() > deadline {
            *slot(&result) = Some(Err("the sign-in was not finished in time".into()));
            return;
        }
        let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(400)) else {
            continue;
        };
        let query = query_of(request.url());
        let ours = request.url().starts_with("/callback")
            && query.get("state") == Some(&state)
            && query.get("code").is_some_and(|c| !c.is_empty());
        let html = if ours {
            page("Signed in", "You can close this tab and return to llmpool.")
        } else {
            page(
                "Not a sign-in",
                "This address only finishes a sign-in llmpool started.",
            )
        };
        let _ = request.respond(
            tiny_http::Response::from_string(html)
                .with_status_code(if ours { 200 } else { 400 })
                .with_header(header("Content-Type", "text/html; charset=utf-8")),
        );
        if ours {
            *slot(&result) = Some(Ok(query["code"].clone()));
            return;
        }
    }
}

impl Daemon {
    fn link_path(&self) -> PathBuf {
        self.data_dir.join("team.json")
    }

    fn link(&self) -> Option<Link> {
        serde_json::from_str(&std::fs::read_to_string(self.link_path()).ok()?).ok()
    }

    fn save_link(&self, link: &Link) {
        if let Ok(text) = serde_json::to_string_pretty(link) {
            let _ = std::fs::write(self.link_path(), text);
        }
    }

    /// Become a member: hold the session, remember the server, fetch the pool.
    fn joined(&self, url: &str, token: &str, email: &str) -> Api<()> {
        self.vault.put(TOKEN_ID, token)?;
        self.save_link(&Link {
            url: url.to_string(),
            email: email.to_string(),
            synced_at: 0,
            error: None,
            admin: false,
        });
        self.team_sync();
        Ok(())
    }

    /// Connect to a team's server. With `key`, at once; without, by signing
    /// in: the answer carries the address to open in a browser, and
    /// `team_status` says when it is done.
    pub fn team_connect(&self, url: &str, key: Option<&str>) -> Api<Value> {
        let url = normalize(url)?;
        let health = call(&url, None, "GET", "/health", None).map_err(|(_, m)| fail(502, m))?;
        if health["service"] != "llmpool" {
            return Err(fail(502, "that address is not an llmpool server"));
        }
        if let Some(job) = slot(&self.team_job).take() {
            slot(&job.result).get_or_insert(Err("replaced".into()));
        }
        if let Some(key) = key.map(str::trim).filter(|k| !k.is_empty()) {
            let me =
                call(&url, Some(key), "GET", "/v1/me", None).map_err(
                    |(status, m)| match status {
                        401 => fail(401, "the server does not accept that key"),
                        _ => fail(502, m),
                    },
                )?;
            self.joined(&url, key, me["email"].as_str().unwrap_or("key"))?;
            return Ok(self.team_status());
        }
        if health["sign_in"].is_null() {
            return Err(fail(
                400,
                "this pool has no sign-in; ask its administrator for an API key",
            ));
        }
        let server = tiny_http::Server::http("127.0.0.1:0").map_err(std::io::Error::other)?;
        let port = server
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .unwrap_or_default();
        let (verifier, state) = (random_hex(32), random_hex(16));
        let auth_url = format!(
            "{url}/auth/login?port={port}&challenge={}&state={}",
            sha256_hex(&verifier),
            url_encode(&state)
        );
        let result = Arc::new(Mutex::new(None));
        {
            let result = result.clone();
            std::thread::spawn(move || await_callback(server, state, result));
        }
        *slot(&self.team_job) = Some(Job {
            url,
            verifier,
            auth_url: auth_url.clone(),
            started: now(),
            result,
            failed: None,
        });
        if self.notify {
            open_browser(&auth_url);
        }
        Ok(self.team_status())
    }

    /// Finish a sign-in whose browser has come back.
    pub(crate) fn team_poll(&self) {
        let ready = {
            let mut job = slot(&self.team_job);
            let outcome = job
                .as_ref()
                .filter(|j| j.failed.is_none())
                .and_then(|j| slot(&j.result).clone());
            match (outcome, job.as_mut()) {
                (Some(Ok(code)), Some(j)) => Some((j.url.clone(), j.verifier.clone(), code)),
                (Some(Err(why)), Some(j)) => {
                    j.failed = Some(why);
                    None
                }
                _ => None,
            }
        };
        let Some((url, verifier, code)) = ready else {
            return;
        };
        let traded = call(
            &url,
            None,
            "POST",
            "/auth/exchange",
            Some(json!({"code": code, "verifier": verifier})),
        );
        let done = traded.map_err(|(_, m)| m).and_then(|answer| {
            let token = answer["token"]
                .as_str()
                .ok_or("the server sent no session")?;
            self.joined(&url, token, answer["email"].as_str().unwrap_or_default())
                .map_err(|e| e.message)
        });
        let mut job = slot(&self.team_job);
        match done {
            Ok(()) => *job = None,
            Err(why) => {
                if let Some(j) = job.as_mut() {
                    j.failed = Some(why);
                }
            }
        }
    }

    pub fn team_status(&self) -> Value {
        self.team_poll();
        let shared = self.lock().p.shared.len();
        let job = slot(&self.team_job);
        let link = self.link();
        let (state, message) = match (job.as_ref(), &link) {
            (Some(j), _) if j.failed.is_none() => ("waiting", None),
            (Some(j), None) => ("failed", j.failed.clone()),
            (_, Some(link)) => ("connected", link.error.clone()),
            (None, None) => ("off", None),
        };
        json!({
            "state": state,
            "message": message,
            "auth_url": job.as_ref().filter(|j| j.failed.is_none()).map(|j| &j.auth_url),
            "waiting_since": job.as_ref().map(|j| j.started),
            "url": link.as_ref().map(|l| &l.url),
            "email": link.as_ref().map(|l| &l.email),
            "synced_at": link.as_ref().map(|l| l.synced_at),
            "admin": link.as_ref().is_some_and(|l| l.admin),
            "accounts": shared,
        })
    }

    /// Leave the team's pool: its accounts and rules go, ours stay.
    pub fn team_disconnect(&self) -> Api<Value> {
        if let Some(job) = slot(&self.team_job).take() {
            slot(&job.result).get_or_insert(Err("cancelled".into()));
        }
        let mut g = self.lock();
        let in_use = g.p.sessions.values().any(|m| {
            m.session
                .account_id
                .as_ref()
                .is_some_and(|id| g.p.shared.contains(id))
        });
        if in_use {
            return Err(fail(
                409,
                "a running session uses one of the team's accounts; switch or end it first",
            ));
        }
        if let (Some(link), Ok(Some(token))) = (self.link(), self.vault.get(TOKEN_ID)) {
            // A courtesy; an API key has no session to end and says so.
            let _ = call(&link.url, Some(&token), "POST", "/auth/logout", None);
        }
        for id in std::mem::take(&mut g.p.shared) {
            let _ = g.store.remove(&id);
            let _ = self.vault.remove(&id);
        }
        g.p.team_busy.clear();
        let _ = self.vault.remove(TOKEN_ID);
        let _ = std::fs::remove_file(self.link_path());
        let _ = std::fs::remove_file(self.team_policy_path());
        self.log(&mut g, "team", None, "left the team's pool".into());
        self.save(&g);
        Ok(json!({"state": "off"}))
    }

    /// Hand one of this machine's accounts to the team's pool.
    ///
    /// A login is moved: it can be renewed in one place only, so once the
    /// server has it this machine forgets it (and is lent it back like any
    /// member). A key is copied unless `keep` is false.
    pub fn team_share(&self, name: &str, keep: bool) -> Api<Value> {
        let link = self.link().ok_or_else(|| {
            fail(
                409,
                "connect to a team's pool first: `llmpool team connect <address>`",
            )
        })?;
        let token = self
            .vault
            .get(TOKEN_ID)?
            .ok_or_else(|| fail(409, "connect to the team's pool again"))?;
        let (account, secret, is_login) = {
            let mut g = self.lock();
            let accounts = self.accounts(&g)?;
            let account = find(&accounts, name)?.clone();
            if g.p.shared.contains(&account.id) {
                return Err(fail(
                    409,
                    format!("{} already comes from the team's pool", account.label),
                ));
            }
            let is_login = account.kind == Kind::Oauth;
            if is_login {
                let in_use =
                    g.p.sessions
                        .values()
                        .any(|m| m.session.account_id.as_ref() == Some(&account.id));
                if in_use || !self.holders(&g, &account).is_empty() {
                    return Err(fail(
                        409,
                        format!(
                            "{} is the login an agent on this machine is using. Sign in to it as a separate account \
                             (`llmpool add claude --to-team`), or switch your sessions off it first.",
                            account.label
                        ),
                    ));
                }
                // Out of every assignment while its only copy travels.
                g.renewing.insert(account.id.clone());
            }
            let secret = self
                .vault
                .get(&account.id)?
                .ok_or_else(|| fail(409, format!("{} has no stored login", account.label)))?;
            (account, secret, is_login)
        };
        let mut sent = account.clone();
        sent.usage = None;
        sent.tags.remove(select::OWN);
        let answer = call(
            &link.url,
            Some(&token),
            "POST",
            "/v1/accounts",
            Some(json!({"account": sent, "secret": secret})),
        );
        let mut g = self.lock();
        g.renewing.remove(&account.id);
        let answer =
            answer.map_err(|(status, why)| fail(if status == 403 { 403 } else { 502 }, why))?;
        let moved = is_login || !keep;
        if moved {
            g.store.remove(&account.id)?;
            self.vault.remove(&account.id)?;
        }
        self.log(
            &mut g,
            "team",
            None,
            format!("{} was added to the team's pool", account.label),
        );
        self.save(&g);
        drop(g);
        self.team_sync();
        Ok(json!({"label": answer["label"], "moved": moved}))
    }

    /// Tell the server what we run on and take what it lends us.
    pub fn team_sync(&self) {
        let Some(mut link) = self.link() else {
            return;
        };
        let Ok(Some(token)) = self.vault.get(TOKEN_ID) else {
            return;
        };
        let report = {
            let g = self.lock();
            let mut using = BTreeMap::<&AccountId, usize>::new();
            for id in
                g.p.sessions
                    .values()
                    .filter_map(|m| m.session.account_id.as_ref())
                    .filter(|id| g.p.shared.contains(*id))
            {
                *using.entry(id).or_default() += 1;
            }
            let usage: BTreeMap<&AccountId, _> =
                g.p.shared
                    .iter()
                    .filter_map(|id| Some((id, g.store.get(id).ok()??.usage?)))
                    .collect();
            json!({"using": using, "usage": usage})
        };
        // The network is never waited on with the daemon's lock held.
        match call(&link.url, Some(&token), "POST", "/v1/sync", Some(report)) {
            Ok(body) => {
                link.error = self.team_apply(&body).err().map(|e| e.message);
                link.synced_at = now();
                if let Some(email) = body["email"].as_str() {
                    link.email = email.to_string();
                }
                link.admin = body["admin"] == true;
            }
            Err((401, why)) => link.error = Some(format!("{why}. Connect again.")),
            Err((_, why)) => link.error = Some(why),
        }
        self.save_link(&link);
    }

    fn team_apply(&self, body: &Value) -> Api<()> {
        let mut g = self.lock();
        let policy = body["policy"].as_str().unwrap_or_default();
        // Rules that do not parse here (a newer server, say) are not half-applied.
        if file::checked(policy).is_ok() {
            std::fs::write(self.team_policy_path(), policy)?;
        }
        let default = self.default_profile().identity();
        let mut seen = BTreeSet::new();
        let mut team_busy = BTreeMap::new();
        for row in body["accounts"].as_array().into_iter().flatten() {
            let Ok(mut account) = serde_json::from_value::<Account>(row["account"].clone()) else {
                continue;
            };
            // The id names a vault entry: it must not be one of our own.
            if account.id == TOKEN_ID || account.id == advisor::KEY_ID || account.id.is_empty() {
                continue;
            }
            let existing = g.store.get(&account.id)?;
            // A login of this person's own is never replaced by a lent one,
            // which could not be renewed: theirs wins.
            let own_login = self.secret_of(&account.id).ok().is_some_and(|s| {
                s.oauth["refreshToken"]
                    .as_str()
                    .is_some_and(|r| !r.is_empty())
            });
            if own_login || (existing.is_some() && !g.p.shared.contains(&account.id)) {
                continue;
            }
            let held = row["held"] == true;
            account.enabled = true;
            account.health = if held {
                Health::Quarantined
            } else {
                Health::Ok
            };
            account.login_expires_at = None;
            // Whose account is "mine" is for this machine to say.
            account.tags.remove(select::OWN);
            account.tags.insert(TEAM_TAG.to_string());
            if let Some(have) = existing.and_then(|a| a.usage) {
                if account
                    .usage
                    .as_ref()
                    .is_none_or(|theirs| have.fetched_at > theirs.fetched_at)
                {
                    account.usage = Some(have);
                }
            }
            if account.kind != Kind::Oauth {
                // Checked here too: the server is not trusted to choose
                // which variables our agents run with.
                let text = row["secret"].to_string();
                if !row["secret"].is_null() {
                    let named_for_it = keys::env_of(&text).is_ok_and(|env| {
                        env.keys().all(|name| {
                            llmpool_core::providers::team_env_ok(&account.provider, name)
                        })
                    });
                    if !named_for_it {
                        continue;
                    }
                    if self.vault.get(&account.id)?.as_deref() != Some(text.as_str()) {
                        self.vault.put(&account.id, &text)?;
                    }
                }
            } else if let Ok(secret) = serde_json::from_value::<Secret>(row["secret"].clone()) {
                let text = serde_json::to_string(&secret)?;
                if self.vault.get(&account.id)?.as_deref() != Some(text.as_str()) {
                    self.vault.put(&account.id, &text)?;
                }
                // A running session on this account gets the fresh token.
                for holder in self.holders_given(&g, &account, &default) {
                    let Some(profile) = self.profile_of(&g, &holder) else {
                        continue;
                    };
                    let Ok(_locks) = lock::credentials(&profile.config_dir, BRIEF_LOCK) else {
                        continue;
                    };
                    let stale = profile
                        .read(&*self.keychain)
                        .ok()
                        .flatten()
                        .is_some_and(|have| {
                            // Never over a login that can renew itself.
                            have.oauth["refreshToken"]
                                .as_str()
                                .is_none_or(str::is_empty)
                                && have.oauth["accessToken"] != secret.oauth["accessToken"]
                        });
                    if stale {
                        let _ = profile.write(&*self.keychain, &secret);
                    }
                }
            }
            g.store.put(&account)?;
            team_busy.insert(
                account.id.clone(),
                row["others"].as_u64().unwrap_or(0) as usize,
            );
            seen.insert(account.id);
        }
        // What the server no longer lends goes, unless a session is on it:
        // that one is held out of rotation until the session moves.
        for id in std::mem::take(&mut g.p.shared) {
            if seen.contains(&id) {
                continue;
            }
            let in_use =
                g.p.sessions
                    .values()
                    .any(|m| m.session.account_id.as_ref() == Some(&id));
            if in_use {
                if let Some(mut account) = g.store.get(&id)? {
                    account.health = Health::Quarantined;
                    g.store.put(&account)?;
                }
                seen.insert(id);
            } else {
                g.store.remove(&id)?;
                self.vault.remove(&id)?;
            }
        }
        g.p.shared = seen;
        g.p.team_busy = team_busy;
        self.save(&g);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn only_https_or_this_machine() {
        assert_eq!(
            normalize(" https://pool.example.com/ ").unwrap(),
            "https://pool.example.com"
        );
        assert!(normalize("http://localhost:8787").is_ok());
        assert!(normalize("http://127.0.0.1:1").is_ok());
        assert!(normalize("http://pool.example.com").is_err());
        assert!(normalize("http://localhost.evil.com").is_err());
        assert!(normalize("http://127.0.0.1@evil.com").is_err());
        assert!(normalize("https://").is_err());
        assert!(normalize("pool.example.com").is_err());
    }
}
