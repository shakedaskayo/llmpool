//! The server's own web page: the same interface the app shows, served to
//! administrators' browsers, plus a page for what only a server has (who is
//! connected, what each member may use, the audit log).
//!
//! An administrator is a member named in `LLMPOOL_ADMINS`. They sign in the
//! way members do, with the provider or an API key, and get a browser session
//! held in a cookie no script can read and no other site can send. Requests
//! that change anything must also carry a header only our own page adds.

use super::*;
use std::sync::atomic::AtomicBool;

const INDEX: &str = include_str!("../../../llmpool-gui/ui/index.html");
const APP: &str = include_str!("../../../llmpool-gui/ui/app.js");
const STYLE: &str = include_str!("../../../llmpool-gui/ui/style.css");
const ICONS: &str = include_str!("../../../llmpool-gui/ui/icons.js");

/// What the page uses in place of the app's one command: the same calls, to us.
const SHIM: &str = r#"window.LLMPOOL_HOST = "server";
window.__TAURI__ = {
  core: { invoke: async (command, args) => {
    const r = await fetch("/admin/invoke/" + command, { method: "POST", headers: { "X-Llmpool": "1", "Content-Type": "application/json" }, body: JSON.stringify(args ?? {}) });
    if (r.status === 401) { location.href = "/login"; throw "sign in again"; }
    const text = await r.text();
    const value = text ? JSON.parse(text) : null;
    if (!r.ok) throw value?.error ?? "request failed";
    return value;
  } },
  event: { listen: async () => () => {} },
};"#;

/// What of the pool's own API an administrator may use from a browser.
/// Left out on purpose: exporting every login, shutting the daemon down,
/// signing in on the server's machine, and what only running agents send.
fn allowed(method: &str, parts: &[&str]) -> bool {
    matches!(
        (method, parts),
        (
            "GET",
            ["accounts"
                | "sessions"
                | "policies"
                | "config"
                | "stats"
                | "events"
                | "issues"
                | "providers"
                | "harnesses"
                | "advisor"
                | "logins"
                | "team"]
        ) | ("GET", ["policies", "schema"])
            | ("POST", ["accounts", "key"])
            | ("PATCH" | "DELETE", ["accounts", _])
            | ("PUT", ["policies"])
            | ("POST", ["policies", "parse" | "render" | "dry-run"])
            | ("PUT", ["config"])
            | ("PUT" | "DELETE", ["advisor", "key"])
            | ("POST", ["import"])
    )
}

impl TeamServer {
    /// The administrator a browser session belongs to.
    fn admin(&self, ask: &Ask) -> Api<String> {
        let (_, who) = self.member(ask.cookie)?;
        if !self.config.admins.contains(&who.to_lowercase()) {
            return Err(fail(
                403,
                format!("{who} is not an administrator of this pool"),
            ));
        }
        Ok(who)
    }

    fn login_page(&self, note: &str) -> String {
        let sign_in = match &self.config.oidc {
            Some(_) => "<p><a class=b href=\"/auth/login?web=1\">Sign in</a></p><p class=m>or</p>",
            None => "",
        };
        let body = if self.config.admins.is_empty() {
            "<p>This server has no administrators yet. Name them in <code>LLMPOOL_ADMINS</code> \
             (the chart's <code>admins</code>): addresses that may sign in, or names of API keys.</p>"
                .to_string()
        } else {
            format!(
                "{sign_in}<form method=post action=\"/admin/login\">\
                 <input name=key type=password autocomplete=off placeholder=\"API key\" aria-label=\"API key\" required>\
                 <button>Continue with a key</button></form>"
            )
        };
        format!(
            "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content=\"width=device-width\">\
             <title>llmpool server</title><style>\
             body{{font:14px/1.5 -apple-system,system-ui,sans-serif;background:#0f1011;color:#ededee;display:grid;place-items:center;min-height:100vh;margin:0}}\
             main{{width:min(360px,90vw)}}h1{{font-size:20px;margin:0 0 4px}}.m{{color:#98a1b0;margin:6px 0}}\
             input,button,.b{{font:inherit;display:block;width:100%;box-sizing:border-box;padding:8px 10px;border-radius:8px;border:1px solid #33363b;background:#17181a;color:inherit;margin:8px 0;text-align:center;text-decoration:none}}\
             input{{text-align:left}}.b{{background:#ededee;color:#0f1011;border-color:#ededee;font-weight:600}}\
             code{{background:#17181a;padding:1px 5px;border-radius:4px}}.n{{color:#e9b955}}</style>\
             <main><h1>llmpool server</h1><p class=m>Administrators of this pool sign in here. Members connect from their own llmpool.</p>\
             {}{body}</main></html>",
            if note.is_empty() {
                String::new()
            } else {
                format!("<p class=n role=alert>{}</p>", escape(note))
            }
        )
    }

    /// Everything a browser asks for.
    pub(super) fn web(&self, ask: &Ask) -> Api<Reply> {
        let text = |s: &str| s.to_string();
        match (ask.method, ask.path) {
            ("GET", "/" | "/index.html") => match self.admin(ask) {
                Ok(_) => Ok(Reply::Asset(
                    "text/html; charset=utf-8",
                    INDEX.replace(
                        "<script src=\"icons.js\">",
                        "<script src=\"/__tauri.js\"></script><script src=\"icons.js\">",
                    ),
                )),
                Err(e) if e.status == 403 => Ok(Reply::Page(403, self.login_page(&e.message))),
                Err(_) => Ok(Reply::Redirect("/login".into())),
            },
            ("GET", "/login") => Ok(Reply::Page(200, self.login_page(""))),
            ("GET", "/app.js") => Ok(Reply::Asset("text/javascript", text(APP))),
            ("GET", "/icons.js") => Ok(Reply::Asset("text/javascript", text(ICONS))),
            ("GET", "/__tauri.js") => Ok(Reply::Asset("text/javascript", text(SHIM))),
            ("GET", "/style.css") => Ok(Reply::Asset("text/css", text(STYLE))),
            ("POST", "/admin/login") => {
                // A plain form: `key=...`.
                let key = query_of(&format!("?{}", ask.text))
                    .remove("key")
                    .unwrap_or_default();
                let found = self
                    .config
                    .api_keys
                    .iter()
                    .find(|(_, hash)| **hash == sha256_hex(key.trim()))
                    .map(|(name, _)| name.clone());
                match found {
                    Some(name)
                        if self.config.admins.contains(&name.to_lowercase())
                            && !self.settings().blocks(&name) =>
                    {
                        Ok(Reply::Session(
                            Some(self.open_session(&name, true)),
                            "/".into(),
                        ))
                    }
                    _ => {
                        self.audit("sign_in_denied", "", "web page: not an administrator's key");
                        Ok(Reply::Page(
                            403,
                            self.login_page("That key is not an administrator's."),
                        ))
                    }
                }
            }
            ("POST", "/admin/invoke/sign_out") => {
                if let (Some(cookie), true) = (ask.cookie, ask.ours) {
                    let mut state = self.state();
                    state.members.remove(&sha256_hex(cookie));
                    self.save_members(&state);
                }
                Ok(Reply::Json(json!("signed out")))
            }
            ("POST", "/admin/invoke/api") => {
                let who = self.admin(ask)?;
                if !ask.ours {
                    return Err(fail(
                        403,
                        "this request did not come from the server's own page",
                    ));
                }
                let method = ask.body["method"].as_str().unwrap_or_default();
                let path = ask.body["path"].as_str().unwrap_or_default();
                let body = ask.body["body"].clone();
                let decoded: Vec<String> = path
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .split('/')
                    .filter(|p| !p.is_empty())
                    .map(percent_decode)
                    .collect();
                let parts: Vec<&str> = decoded.iter().map(String::as_str).collect();
                if method != "GET" && path != "/server/requests" {
                    self.audit("admin", &who, &format!("{method} {path}"));
                }
                let answer = match (method, parts.as_slice()) {
                    ("GET", ["server"]) => self.overview(&who),
                    ("PUT", ["server"]) => self.configure(&body).and_then(|()| self.overview(&who)),
                    // The filter travels in the body: a member's address in a
                    // query string would end up in access logs.
                    ("POST", ["server", "requests"]) => Ok(self.gateway_report(&body)),
                    ("DELETE", ["server", "members", email]) => {
                        let mut state = self.state();
                        state.members.retain(|_, m| m.email != *email);
                        for holders in state.seats.values_mut() {
                            holders.remove(*email);
                        }
                        self.save_members(&state);
                        drop(state);
                        self.overview(&who)
                    }
                    _ if allowed(method, &parts) => crate::http::route(
                        &self.daemon,
                        method,
                        &parts,
                        body,
                        &AtomicBool::new(false),
                    ),
                    _ => Err(fail(403, "that is not available from the server's page")),
                }?;
                Ok(Reply::Json(answer))
            }
            // What only the app on a person's own machine can do.
            ("POST", path) if path.starts_with("/admin/invoke/") => {
                self.admin(ask)?;
                Ok(Reply::Json(json!("not available on a server")))
            }
            _ => Err(fail(404, "no such route")),
        }
    }

    /// What the Server page shows.
    fn overview(&self, who: &str) -> Api<Value> {
        let settings = self.settings();
        let state = self.state();
        let t = now();
        let labels: BTreeMap<AccountId, String> = {
            let g = self.daemon.lock();
            g.store
                .list()?
                .into_iter()
                .map(|a| (a.id, a.label))
                .collect()
        };
        let mut members: BTreeMap<&str, (i64, bool, usize)> = BTreeMap::new();
        for m in state.members.values().filter(|m| m.expires_at > t) {
            let entry = members.entry(&m.email).or_insert((0, m.by_key, 0));
            entry.0 = entry.0.max(m.expires_at);
            entry.2 += 1;
        }
        let seats: Vec<Value> = state
            .seats
            .iter()
            .flat_map(|(id, holders)| {
                let label = labels.get(id).cloned().unwrap_or_else(|| id.clone());
                holders
                    .iter()
                    .filter(|(_, (seen, _))| t - *seen < SEAT_SECS)
                    .map(move |(who, (seen, sessions))| {
                        json!({"account": label, "who": who, "sessions": sessions, "seen": seen})
                    })
            })
            .collect();
        let audit: Vec<Value> =
            std::fs::read_to_string(self.daemon.data_dir().join("team-audit.log"))
                .unwrap_or_default()
                .lines()
                .rev()
                .take(100)
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
        Ok(json!({
            "you": who,
            "public_url": self.config.public_url,
            "sign_in": self.config.oidc.as_ref().map(|o| &o.issuer),
            "allowed_domains": self.config.domains,
            "allowed_emails": self.config.emails,
            "key_members": self.config.api_keys.keys().collect::<Vec<_>>(),
            "admins": self.config.admins,
            "sealed": std::env::var("LLMPOOL_VAULT_KEY").is_ok_and(|k| !k.trim().is_empty()),
            "settings": {
                "share_logins": settings.share_logins,
                "access": settings.access.iter().map(|(who, tags)| format!("{who}={}", tags.join(","))).collect::<Vec<_>>().join("\n"),
                "blocked": settings.blocked,
                "gateway": settings.gateway,
            },
            "gateway": {
                "base": format!("{}/gateway", self.config.public_url),
                "recent": self.gateway_recent(),
            },
            "members": members.iter().map(|(email, (expires, by_key, sessions))| json!({
                "email": email, "expires_at": expires, "by_key": by_key, "sessions": sessions,
            })).collect::<Vec<_>>(),
            "seats": seats,
            "audit": audit,
        }))
    }

    /// Change what an administrator may change, and keep it.
    fn configure(&self, body: &Value) -> Api<()> {
        let mut settings = self.settings();
        if let Some(on) = body["share_logins"].as_bool() {
            settings.share_logins = on;
        }
        if let Some(on) = body["gateway"].as_bool() {
            settings.gateway = on;
        }
        if let Some(text) = body["access"].as_str() {
            settings.access = Config::access(text)
                .map_err(|e| fail(400, e.replace("LLMPOOL_ACCESS: each entry", "each line")))?;
        }
        if let Some(list) = body["blocked"].as_array() {
            settings.blocked = list
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
        let text = serde_json::to_string_pretty(&settings)?;
        std::fs::write(self.daemon.data_dir().join("team-settings.json"), text)?;
        *self.settings.lock().unwrap_or_else(|p| p.into_inner()) = settings;
        Ok(())
    }
}
