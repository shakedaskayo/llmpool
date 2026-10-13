//! A team server and its members end to end: a fake sign-in provider, a real
//! server on loopback, and clients that connect by signing in or with a key.

use base64::Engine;
use llmpool_adapter_claude::profile::{NoKeychain, Profile, Secret};
use llmpool_core::vault::{FileVault, Vault};
use llmpool_daemon::team_server::{self, Config, Oidc, Serving};
use llmpool_daemon::{CaptureReq, Daemon, PatchReq, StartReq};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &str = "ci-key-0123456789abcdefghij";

struct Machine {
    _dir: tempfile::TempDir,
    data: PathBuf,
    daemon: Arc<Daemon>,
}

fn secret(email: &str, token: &str) -> Secret {
    Secret {
        oauth: json!({"accessToken": token, "refreshToken": format!("r-{token}"), "expiresAt": 4_000_000_000_000i64}),
        account: json!({"emailAddress": email, "organizationUuid": "org"}),
    }
}

/// A machine whose pool holds the given logins; none is its default login.
fn machine(logins: &[(&str, &str)]) -> Machine {
    let dir = tempfile::tempdir().unwrap();
    let (home, data) = (dir.path().join("home"), dir.path().join("data"));
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let daemon = Daemon::open(
        &data,
        &home,
        Box::new(FileVault(data.join("vault"))),
        Box::new(NoKeychain),
    )
    .unwrap();
    for (email, token) in logins {
        let scratch = data.join(format!("login-{token}"));
        Profile::at(&scratch)
            .write(&NoKeychain, &secret(email, token))
            .unwrap();
        let req = CaptureReq {
            profile_dir: Some(scratch),
            label: None,
            tags: vec!["work".into()],
        };
        daemon.capture(req).unwrap();
    }
    Machine {
        _dir: dir,
        data,
        daemon: Arc::new(daemon),
    }
}

/// A sign-in provider that vouches for whatever `who` currently says.
struct Provider {
    url: String,
    who: Arc<Mutex<Value>>,
}

fn provider() -> Provider {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}",
        server.server_addr().to_ip().unwrap().port()
    );
    let who = Arc::new(Mutex::new(json!({})));
    let (base, claims) = (url.clone(), who.clone());
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let body = if request.url().starts_with("/.well-known/") {
                json!({"issuer": base, "authorization_endpoint": format!("{base}/authorize"), "token_endpoint": format!("{base}/token")})
            } else {
                let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(claims.lock().unwrap().to_string());
                json!({"id_token": format!("h.{payload}.s")})
            };
            let _ = request.respond(tiny_http::Response::from_string(body.to_string()));
        }
    });
    Provider { url, who }
}

fn serve(pool: &Machine, provider: &Provider, share_logins: bool) -> (Serving, String) {
    let config = Config {
        public_url: "http://127.0.0.1:1".into(),
        oidc: Some(Oidc {
            issuer: provider.url.clone(),
            client_id: "client".into(),
            client_secret: "s".into(),
        }),
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        domains: vec!["x.io".into()],
        emails: vec![],
        share_logins,
        ..Config::default()
    };
    let serving = team_server::start(pool.daemon.clone(), config, "127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", serving.port);
    (serving, url)
}

/// Follow one redirect by hand, as a browser would, and say where it leads.
fn hop(url: &str) -> Result<String, (u16, String)> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(5))
        .build();
    match agent.get(url).call() {
        Ok(r) if r.status() == 302 => Ok(r.header("Location").unwrap().to_string()),
        Ok(r) => Err((r.status(), r.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(status, r)) => Err((status, r.into_string().unwrap_or_default())),
        Err(e) => Err((0, e.to_string())),
    }
}

fn param(url: &str, name: &str) -> String {
    url.split(['?', '&'])
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
        .unwrap_or_default()
        .to_string()
}

/// Sign `member` in to the server at `url` as the provider's `claims` say.
fn sign_in(member: &Machine, url: &str, provider: &Provider, claims: Value) -> Result<(), String> {
    let started = member
        .daemon
        .team_connect(url, None)
        .map_err(|e| e.message)?;
    assert_eq!(started["state"], "waiting");
    let to_provider = hop(started["auth_url"].as_str().unwrap()).map_err(|e| e.1)?;
    assert!(to_provider.starts_with(&format!("{}/authorize?", provider.url)));
    let mut claims = claims;
    claims["nonce"] = json!(param(&to_provider, "nonce"));
    claims["iss"] = json!(provider.url);
    claims["exp"] = json!(4_000_000_000i64);
    *provider.who.lock().unwrap() = claims;
    let back = format!(
        "{url}/auth/callback?code=c&state={}",
        param(&to_provider, "state")
    );
    let to_client = hop(&back).map_err(|e| e.1)?;
    assert!(to_client.starts_with("http://127.0.0.1:"));
    let landed = ureq::get(&to_client).call().map_err(|e| e.to_string())?;
    assert!(landed.into_string().unwrap().contains("Signed in"));
    // The daemon notices on its next look.
    for _ in 0..50 {
        let status = member.daemon.team_status();
        if status["state"] == "connected" {
            return Ok(());
        }
        if status["state"] == "failed" {
            return Err(status["message"].as_str().unwrap_or_default().to_string());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("never connected".into())
}

fn accounts(m: &Machine) -> BTreeMap<String, Value> {
    m.daemon
        .list_accounts()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["label"].as_str().unwrap().to_string(), a.clone()))
        .collect()
}

fn start_on(m: &Machine, account: &str) -> Value {
    let req = StartReq {
        cwd: "/w".into(),
        git_remote: None,
        pid: std::process::id(),
        account: Some(account.into()),
        tag: None,
        ..StartReq::default()
    };
    m.daemon.start_session(req).unwrap()
}

fn good(email: &str) -> Value {
    json!({"aud": "client", "email": email, "email_verified": true})
}

#[test]
fn signing_in_brings_the_teams_accounts_and_rules_read_only() {
    let pool = machine(&[("a@x.io", "a-1"), ("b@x.io", "b-1")]);
    pool.daemon
        .put_policy("[[rule]]\nname = 'team rule'\non = 'session_start'\ndo = { hold = true }\n")
        .unwrap();
    let idp = provider();
    let (_serving, url) = serve(&pool, &idp, false);
    let ann = machine(&[("ann@x.io", "ann-1")]);

    sign_in(&ann, &url, &idp, good("Ann@x.io")).unwrap();
    let status = ann.daemon.team_status();
    assert_eq!(status["email"], "ann@x.io");
    assert_eq!(status["accounts"], 2);

    let rows = accounts(&ann);
    assert_eq!(rows["a@x.io"]["shared"], true);
    assert!(rows["a@x.io"]["tags"]
        .as_array()
        .unwrap()
        .contains(&json!("team")));
    assert_eq!(rows["ann@x.io"]["shared"], false);

    // What arrived can run a session but cannot mint new tokens.
    let vault = FileVault(ann.data.join("vault"));
    let lent: Secret = serde_json::from_str(
        &vault
            .get(rows["a@x.io"]["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(lent.oauth["accessToken"], "a-1");
    assert!(lent.oauth.get("refreshToken").is_none());
    // The session with the server is stored, and is not the provider's token.
    assert_eq!(vault.get("team-session").unwrap().unwrap().len(), 64);

    // The server's accounts and rules are the server's to change.
    let patch = PatchReq {
        label: None,
        tags: None,
        enabled: Some(false),
    };
    assert_eq!(
        ann.daemon
            .patch_account("a@x.io", patch)
            .unwrap_err()
            .status,
        403
    );
    assert_eq!(ann.daemon.remove_account("a@x.io").unwrap_err().status, 403);
    let policy = ann.daemon.policy_text();
    assert_eq!(policy["team"]["rule"][0]["name"], "team rule");
    assert_eq!(policy["text"], "");

    // Leaving takes the team's accounts and rules away, and nothing else.
    ann.daemon.team_disconnect().unwrap();
    let rows = accounts(&ann);
    assert_eq!(rows.keys().collect::<Vec<_>>(), ["ann@x.io"]);
    assert!(ann.daemon.policy_text()["team"].is_null());
    assert!(vault.get("team-session").unwrap().is_none());
}

#[test]
fn people_outside_the_allow_list_and_forged_sign_ins_stay_out() {
    let pool = machine(&[("a@x.io", "a-1")]);
    let idp = provider();
    let (_serving, url) = serve(&pool, &idp, false);
    let eve = machine(&[]);

    for (claims, why) in [
        (good("eve@evil.io"), "not allowed"),
        (
            json!({"aud": "client", "email": "eve@x.io", "email_verified": false}),
            "not verified",
        ),
        (
            json!({"aud": "someone-else", "email": "eve@x.io", "email_verified": true}),
            "not meant",
        ),
    ] {
        let failed = sign_in(&eve, &url, &idp, claims).unwrap_err();
        assert!(failed.contains(why), "{failed}");
        assert_ne!(eve.daemon.team_status()["state"], "connected");
    }
    assert!(accounts(&eve).is_empty());

    // The code alone, without what the client kept to itself, opens nothing.
    let started = eve.daemon.team_connect(&url, None).unwrap();
    let to_provider = hop(started["auth_url"].as_str().unwrap()).unwrap();
    let mut claims = good("ann@x.io");
    claims["nonce"] = json!(param(&to_provider, "nonce"));
    claims["iss"] = json!(idp.url);
    claims["exp"] = json!(4_000_000_000i64);
    *idp.who.lock().unwrap() = claims;
    let to_client = hop(&format!(
        "{url}/auth/callback?code=c&state={}",
        param(&to_provider, "state")
    ))
    .unwrap();
    let stolen = json!({"code": param(&to_client, "code"), "verifier": "f".repeat(64)});
    let refused = ureq::post(&format!("{url}/auth/exchange")).send_json(stolen);
    assert!(matches!(refused, Err(ureq::Error::Status(400, _))));
    // And the code is spent: the rightful client cannot use it either.
    ureq::get(&to_client).call().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(eve.daemon.team_status()["state"], "failed");

    // Nothing is served without a session, and the sign-in only starts for
    // a client on the person's own machine with a proper challenge.
    assert!(matches!(
        ureq::post(&format!("{url}/v1/sync"))
            .set("Authorization", "Bearer nope")
            .send_json(json!({})),
        Err(ureq::Error::Status(401, _))
    ));
    assert_eq!(
        hop(&format!("{url}/auth/login?port=80&challenge=x&state=y"))
            .unwrap_err()
            .0,
        400
    );
    assert!(eve
        .daemon
        .team_connect("http://pool.example.com", None)
        .is_err());
}

#[test]
fn a_key_connects_and_a_login_in_use_is_not_lent_twice() {
    let pool = machine(&[("a@x.io", "a-1")]);
    let idp = provider();
    let (_serving, url) = serve(&pool, &idp, false);
    let (ann, ci) = (machine(&[]), machine(&[]));

    assert_eq!(
        ci.daemon
            .team_connect(&url, Some("wrong-key-000000000000000000"))
            .unwrap_err()
            .status,
        401
    );
    assert_eq!(
        ci.daemon.team_connect(&url, Some(KEY)).unwrap()["email"],
        "ci"
    );
    sign_in(&ann, &url, &idp, good("ann@x.io")).unwrap();

    // Ann starts working on the account; the server hears of it at her next sync.
    let session = start_on(&ann, "a@x.io");
    ann.daemon.team_sync();
    ci.daemon.team_sync();
    let seen = &accounts(&ci)["a@x.io"];
    assert_eq!(seen["health"], "quarantined");
    assert!(ci
        .daemon
        .start_session(StartReq {
            cwd: "/w".into(),
            git_remote: None,
            pid: std::process::id(),
            account: Some("a@x.io".into()),
            tag: None,
            ..StartReq::default()
        })
        .is_err());
    // Ann herself keeps it, and cannot leave while she runs on it.
    assert_eq!(accounts(&ann)["a@x.io"]["health"], "ok");
    assert_eq!(ann.daemon.team_disconnect().unwrap_err().status, 409);

    // The server renews the token; Ann's running session receives the new one.
    let id = accounts(&ann)["a@x.io"]["id"].as_str().unwrap().to_string();
    FileVault(pool.data.join("vault"))
        .put(
            &id,
            &serde_json::to_string(&secret("a@x.io", "a-2")).unwrap(),
        )
        .unwrap();
    ann.daemon.team_sync();
    let dir = PathBuf::from(session["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    let live = Profile::at(&dir).read(&NoKeychain).unwrap().unwrap();
    assert_eq!(live.oauth["accessToken"], "a-2");

    // She stops; the seat frees and the other member may use the account.
    ann.daemon
        .end_session(session["session_id"].as_str().unwrap());
    ann.daemon.team_sync();
    ci.daemon.team_sync();
    assert_eq!(accounts(&ci)["a@x.io"]["health"], "ok");
}

#[test]
fn a_persons_own_login_is_never_replaced_by_a_lent_one() {
    let pool = machine(&[("a@x.io", "a-1")]);
    let idp = provider();
    let (_serving, url) = serve(&pool, &idp, true);
    // This member already has the same account as their own.
    let ann = machine(&[("a@x.io", "mine-1")]);
    ann.daemon.team_connect(&url, Some(KEY)).unwrap();

    let row = &accounts(&ann)["a@x.io"];
    assert_eq!(row["shared"], false);
    let kept: Secret = serde_json::from_str(
        &FileVault(ann.data.join("vault"))
            .get(row["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(kept.oauth["refreshToken"], "r-mine-1");
}

#[test]
fn keys_for_other_providers_are_lent_and_run_any_agent() {
    use llmpool_daemon::KeyReq;
    let pool = machine(&[]);
    let key = |provider: &str, env: &[(&str, &str)]| KeyReq {
        provider: provider.into(),
        label: None,
        tags: vec![],
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    pool.daemon
        .add_key(key("xai", &[("XAI_API_KEY", "xai-secret")]))
        .unwrap();
    // An account may not set variables that change what the agent runs.
    for bad in [
        "PATH",
        "LD_PRELOAD",
        "NODE_OPTIONS",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        assert_eq!(
            pool.daemon
                .add_key(key("evil", &[(bad, "x")]))
                .unwrap_err()
                .status,
            400
        );
    }
    assert!(pool
        .daemon
        .add_key(key("claude", &[("X_API_KEY", "x")]))
        .is_err());

    let idp = provider();
    let (_serving, url) = serve(&pool, &idp, false);
    let ann = machine(&[("ann@x.io", "ann-1")]);
    ann.daemon.team_connect(&url, Some(KEY)).unwrap();
    assert_eq!(accounts(&ann)["xai"]["shared"], true);

    // Any agent on that provider starts with the key in its environment.
    let req = StartReq {
        cwd: "/w".into(),
        pid: std::process::id(),
        provider: Some("xai".into()),
        agent: Some("grok".into()),
        ..StartReq::default()
    };
    let started = ann.daemon.start_session(req).unwrap();
    assert_eq!(started["env"]["XAI_API_KEY"], "xai-secret");
    assert_eq!(started["account"]["label"], "xai");
    assert!(started["env"].get("CLAUDE_CONFIG_DIR").is_none());

    // A Claude Code session never lands on a key, and a key session cannot
    // be moved under the running agent.
    let claude = start_on(&ann, "ann@x.io");
    let sid = claude["session_id"].as_str().unwrap();
    assert_eq!(ann.daemon.swap_session(sid, "xai").unwrap_err().status, 409);
    let keyed = started["session_id"].as_str().unwrap();
    assert_eq!(
        ann.daemon
            .swap_session(keyed, "ann@x.io")
            .unwrap_err()
            .status,
        409
    );

    // Without any account of the provider there is nothing to start on.
    let none = StartReq {
        cwd: "/w".into(),
        pid: std::process::id(),
        provider: Some("openai".into()),
        ..StartReq::default()
    };
    assert_eq!(ann.daemon.start_session(none).unwrap_err().status, 409);
}

#[test]
fn members_only_get_what_their_tags_open_and_it_is_all_recorded() {
    let pool = machine(&[("a@x.io", "a-1"), ("b@x.io", "b-1")]);
    pool.daemon
        .patch_account(
            "b@x.io",
            PatchReq {
                label: None,
                tags: Some(["vip".to_string()].into()),
                enabled: None,
            },
        )
        .unwrap();
    let idp = provider();
    let config = Config {
        public_url: "http://127.0.0.1:1".into(),
        api_keys: Config::keys(&format!("ci:{KEY},gone:gone-key-0123456789abcdefgh")).unwrap(),
        access: Config::access("ci=work").unwrap(),
        blocked: vec!["gone".into()],
        ..Config::default()
    };
    let serving = team_server::start(pool.daemon.clone(), config, "127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", serving.port);
    let _ = idp;

    let ci = machine(&[]);
    ci.daemon.team_connect(&url, Some(KEY)).unwrap();
    assert_eq!(accounts(&ci).keys().collect::<Vec<_>>(), ["a@x.io"]);
    // A blocked member is out even with a key that is still configured.
    let gone = machine(&[]);
    assert_eq!(
        gone.daemon
            .team_connect(&url, Some("gone-key-0123456789abcdefgh"))
            .unwrap_err()
            .status,
        401
    );
    // With no sign-in configured, signing in is not offered.
    assert_eq!(
        gone.daemon.team_connect(&url, None).unwrap_err().status,
        400
    );

    let session = start_on(&ci, "a@x.io");
    ci.daemon.team_sync();
    ci.daemon
        .end_session(session["session_id"].as_str().unwrap());
    ci.daemon.team_sync();
    let log = std::fs::read_to_string(pool.data.join("team-audit.log")).unwrap();
    for event in [
        "\"event\":\"lent\"",
        "\"event\":\"seat_taken\"",
        "\"event\":\"seat_released\"",
    ] {
        assert!(log.contains(event), "{event} missing from {log}");
    }
    assert!(log.contains("\"who\":\"ci\"") && log.contains("a@x.io"));
    assert!(
        !log.contains("a-1"),
        "the audit log must never hold a token"
    );
}

// The HTTP client's error type is large; these are a test's helpers.
#[allow(clippy::result_large_err)]
#[test]
fn the_servers_own_page_is_for_administrators_and_cannot_be_driven_from_elsewhere() {
    let pool = machine(&[("a@x.io", "a-1")]);
    let config = Config {
        public_url: "http://127.0.0.1:1".into(),
        api_keys: Config::keys(&format!("ci:{KEY},member:member-key-0123456789abcdef")).unwrap(),
        admins: vec!["ci".into()],
        ..Config::default()
    };
    let serving = team_server::start(pool.daemon.clone(), config, "127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", serving.port);
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    let status = |r: Result<ureq::Response, ureq::Error>| match r {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(e) => panic!("{e}"),
    };

    // Nothing of the pool without a session; the page sends you to sign in.
    assert_eq!(status(agent.get(&format!("{url}/")).call()), 302);
    assert_eq!(status(agent.get(&format!("{url}/login")).call()), 200);
    let call = |cookie: &str, ours: bool, body: Value| {
        let mut request = agent
            .post(&format!("{url}/admin/invoke/api"))
            .set("Cookie", &format!("llmpool_session={cookie}"));
        if ours {
            request = request.set("X-Llmpool", "1");
        }
        request.send_json(body)
    };
    let get_accounts = json!({"method": "GET", "path": "/accounts"});
    assert_eq!(status(call("nope", true, get_accounts.clone())), 401);

    // A member who is not an administrator does not get in, with a key or
    // with a session of their own.
    let login = |key: &str| {
        agent
            .post(&format!("{url}/admin/login"))
            .send_form(&[("key", key)])
    };
    assert_eq!(status(login("member-key-0123456789abcdef")), 403);
    assert_eq!(status(login("wrong")), 403);
    assert_eq!(
        status(call(
            "member-key-0123456789abcdef",
            true,
            get_accounts.clone()
        )),
        403
    );

    // An administrator signs in and gets a cookie scripts cannot read.
    let signed = login(KEY).unwrap();
    let cookie = signed.header("Set-Cookie").unwrap().to_string();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let session = cookie.split(['=', ';']).nth(1).unwrap().to_string();
    assert_ne!(session, KEY, "the key itself is never the cookie");
    let page = agent
        .get(&format!("{url}/"))
        .set("Cookie", &format!("llmpool_session={session}"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    assert!(page.contains("/__tauri.js") && page.contains("app.js"));

    // The page's own requests work; the same request from another site's
    // form, which cannot add the header, does not.
    let listed: Value = call(&session, true, get_accounts.clone())
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(listed[0]["label"], "a@x.io");
    assert_eq!(status(call(&session, false, get_accounts)), 403);

    // What must not be reachable from a browser is not.
    for (method, path) in [
        ("POST", "/export"),
        ("POST", "/shutdown"),
        ("POST", "/logins"),
        ("POST", "/hooks/claude/s1/Stop"),
    ] {
        let refused = call(
            &session,
            true,
            json!({"method": method, "path": path, "body": {"passphrase": "x"}}),
        );
        assert_eq!(status(refused), 403, "{method} {path}");
    }

    // Settings changed on the page take effect and are kept.
    let saved: Value = call(&session, true, json!({"method": "PUT", "path": "/server", "body": {"access": "member=vip", "share_logins": true}}))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(saved["settings"]["access"], "member=vip");
    assert_eq!(saved["you"], "ci");
    let member = machine(&[]);
    member
        .daemon
        .team_connect(&url, Some("member-key-0123456789abcdef"))
        .unwrap();
    assert!(
        accounts(&member).is_empty(),
        "the account is not tagged vip"
    );
    assert!(pool.data.join("team-settings.json").exists());
    assert_eq!(
        status(call(
            &session,
            true,
            json!({"method": "PUT", "path": "/server", "body": {"access": "nonsense"}})
        )),
        400
    );

    // Signing out ends the session.
    agent
        .post(&format!("{url}/admin/invoke/sign_out"))
        .set("Cookie", &format!("llmpool_session={session}"))
        .set("X-Llmpool", "1")
        .send_json(json!({}))
        .unwrap();
    assert_eq!(
        status(call(
            &session,
            true,
            json!({"method": "GET", "path": "/accounts"})
        )),
        401
    );
}

// ---- the server as a gateway ---------------------------------------------------

/// A provider that answers by which key it is shown, and remembers what it saw.
struct Upstream {
    url: String,
    seen: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
    /// A slow answer waits until this many are open together (five seconds at
    /// most), so a test can tell answers in flight together from a queue
    /// without reading a clock.
    together: Arc<std::sync::atomic::AtomicUsize>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
}

fn upstream() -> Upstream {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}",
        server.server_addr().to_ip().unwrap().port()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let together = Arc::new(AtomicUsize::new(1));
    let (open, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (want, most) = (together.clone(), peak.clone());
    std::thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let mut headers: BTreeMap<String, String> = request
                .headers()
                .iter()
                .map(|h| {
                    (
                        h.field.as_str().as_str().to_lowercase(),
                        h.value.as_str().to_string(),
                    )
                })
                .collect();
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            headers.insert(":path".into(), request.url().to_string());
            headers.insert(":body".into(), body);
            let auth = headers.get("authorization").cloned().unwrap_or_default();
            log.lock().unwrap().push(headers);
            let header = |n: &str, v: &str| {
                tiny_http::Header::from_bytes(n.as_bytes(), v.as_bytes()).unwrap()
            };
            if auth.contains("k-limited") {
                let _ = request.respond(
                    tiny_http::Response::from_string("{\"error\":\"slow down\"}")
                        .with_status_code(429)
                        .with_header(header("retry-after", "30")),
                );
            } else if request.url().contains("stream") {
                let (open, want, most) = (open.clone(), want.clone(), most.clone());
                std::thread::spawn(move || {
                    most.fetch_max(open.fetch_add(1, SeqCst) + 1, SeqCst);
                    // Two events, a moment apart, each sent the moment it exists.
                    use std::io::Write;
                    let mut out = request.into_writer();
                    let _ = out.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n");
                    let mut event = |text: &str| {
                        let _ = write!(out, "{:x}\r\n{text}\r\n", text.len());
                        let _ = out.flush();
                    };
                    event("data: {\"message\":{\"usage\":{\"input_tokens\":5}}}\n\n");
                    let began = std::time::Instant::now();
                    while most.load(SeqCst) < want.load(SeqCst)
                        && began.elapsed() < Duration::from_secs(5)
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    std::thread::sleep(Duration::from_millis(600));
                    event("data: {\"usage\":{\"output_tokens\":9}}\n\n");
                    event("");
                    open.fetch_sub(1, SeqCst);
                });
            } else {
                // A subscription login says how full it is with every answer.
                let used = if auth.contains("a-1") {
                    "0.95"
                } else if auth.contains("m-1") {
                    "0.60"
                } else {
                    "0.10"
                };
                let _ = request.respond(
                    tiny_http::Response::from_string(
                        "{\"ok\":true,\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":22}}",
                    )
                    .with_header(header("content-type", "application/json"))
                    .with_header(header("anthropic-ratelimit-unified-5h-utilization", used))
                    .with_header(header("anthropic-ratelimit-unified-5h-reset", "4000000000")),
                );
            }
        }
    });
    Upstream {
        url,
        seen,
        together,
        peak,
    }
}

#[allow(clippy::result_large_err)]
#[test]
fn as_a_gateway_the_server_keeps_every_credential_and_switches_accounts_itself() {
    use llmpool_daemon::KeyReq;
    use std::io::{BufRead, BufReader};
    let provider = upstream();
    let pool = machine(&[("a@x.io", "a-1"), ("b@x.io", "b-1")]);
    for (label, key) in [("limited", "k-limited"), ("good", "k-good")] {
        pool.daemon
            .add_key(KeyReq {
                provider: "openai".into(),
                label: Some(label.into()),
                tags: vec![],
                env: [
                    ("OPENAI_API_KEY".to_string(), key.to_string()),
                    ("OPENAI_BASE_URL".to_string(), provider.url.clone()),
                ]
                .into(),
            })
            .unwrap();
    }
    let (_serving, url) = on_a_free_port(&pool, |url| Config {
        public_url: url,
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        gateway: true,
        upstreams: [("claude".to_string(), provider.url.clone())].into(),
        ..Config::default()
    });

    // A member is lent the gateway, not what is behind it.
    let ann = machine(&[]);
    ann.daemon.team_connect(&url, Some(KEY)).unwrap();
    let lent = accounts(&ann);
    assert_eq!(
        lent.keys().collect::<Vec<_>>(),
        ["team gateway (claude)", "team gateway (openai)"]
    );
    let vault = std::fs::read_dir(ann.data.join("vault"))
        .unwrap()
        .map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap())
        .collect::<String>();
    for secret in ["k-good", "k-limited", "a-1", "b-1"] {
        assert!(!vault.contains(secret), "{secret} reached the member");
    }
    // Any agent started on it is pointed here, with the member's own session.
    let started = ann
        .daemon
        .start_session(StartReq {
            cwd: "/w".into(),
            pid: std::process::id(),
            provider: Some("openai".into()),
            agent: Some("codex".into()),
            ..StartReq::default()
        })
        .unwrap();
    assert_eq!(
        started["env"]["OPENAI_BASE_URL"],
        format!("{url}/gateway/openai/v1")
    );
    assert_eq!(started["env"]["OPENAI_API_KEY"], KEY);
    let claude = ann.daemon.start_session(StartReq {
        cwd: "/w".into(),
        pid: std::process::id(),
        ..StartReq::default()
    });
    // Claude Code needs a rule or a name to pick an account; named, it gets the gateway.
    drop(claude);
    let named = ann
        .daemon
        .start_session(StartReq {
            cwd: "/w".into(),
            pid: std::process::id(),
            account: Some("team gateway (claude)".into()),
            ..StartReq::default()
        })
        .unwrap();
    assert_eq!(
        named["env"]["ANTHROPIC_BASE_URL"],
        format!("{url}/gateway/claude")
    );
    assert_eq!(named["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);

    // A request goes out on a pool key; one account refusing is not the member's problem.
    let post = |path: &str, token: &str| {
        ureq::post(&format!("{url}/gateway/{path}"))
            .set("Authorization", &format!("Bearer {token}"))
            .set("anthropic-beta", "tools-2099")
            .set("Cookie", "secret=1")
            .send_json(json!({"model": "m-1", "messages": [{"content": "hush-hush"}]}))
    };
    let able = |name: &str, on: bool| {
        let patch = PatchReq {
            label: None,
            tags: None,
            enabled: Some(on),
        };
        pool.daemon.patch_account(name, patch).unwrap();
    };
    // With only an account the provider is refusing, the refusal is passed on as it is.
    able("good", false);
    assert!(matches!(
        post("openai/v1/chat/completions", KEY),
        Err(ureq::Error::Status(429, _))
    ));
    able("good", true);
    let answer: Value = post("openai/v1/chat/completions", KEY)
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(answer["ok"], true);
    {
        let seen = provider.seen.lock().unwrap();
        let last = seen.last().unwrap();
        assert_eq!(last["authorization"], "Bearer k-good");
        assert_eq!(last[":path"], "/v1/chat/completions");
        assert!(last[":body"].contains("hush-hush"));
        assert!(!last.contains_key("cookie"));
        assert!(
            seen.iter().all(|h| !h.values().any(|v| v.contains(KEY))),
            "the member's token went upstream"
        );
    }
    // The refused account is left alone for a while: the next request does not try it.
    let before = provider.seen.lock().unwrap().len();
    post("openai/v1/chat/completions", KEY).unwrap();
    assert_eq!(provider.seen.lock().unwrap().len(), before + 1);

    // Strangers and unknown providers get nothing.
    assert!(matches!(
        post("openai/v1/chat/completions", "nope"),
        Err(ureq::Error::Status(401, _))
    ));
    assert!(matches!(
        post("nowhere/v1/messages", KEY),
        Err(ureq::Error::Status(404, _))
    ));

    // An answer that streams arrives as it is made, not when it is finished.
    let began = std::time::Instant::now();
    let stream = post("openai/v1/chat/completions?mode=stream", KEY).unwrap();
    assert_eq!(stream.content_type(), "text/event-stream");
    let mut lines = BufReader::new(stream.into_reader()).lines();
    assert!(lines.next().unwrap().unwrap().contains("input_tokens"));
    assert!(
        began.elapsed() < Duration::from_millis(450),
        "the first event waited for the last"
    );
    assert!(lines.any(|l| l.unwrap().contains("output_tokens")));

    // Subscription logins: sent as what they are, and when the one the member
    // is on fills up, the next request runs on another. Nobody restarts anything.
    let mut used = Vec::new();
    able("b@x.io", false);
    for round in 0..3 {
        post("claude/v1/messages", KEY).unwrap();
        able("b@x.io", true);
        let seen = provider.seen.lock().unwrap();
        let last = seen.last().unwrap();
        assert!(
            last["anthropic-beta"].contains("oauth-2025-04-20")
                && last["anthropic-beta"].contains("tools-2099"),
            "round {round}"
        );
        used.push(last["authorization"].clone());
    }
    assert_eq!(
        used,
        ["Bearer a-1", "Bearer b-1", "Bearer b-1"],
        "a-1 said it was 95% full"
    );
    let audit = std::fs::read_to_string(pool.data.join("team-audit.log")).unwrap();
    assert!(audit.contains("gateway_switch") && audit.contains("now on b@x.io"));

    // Every request left a line saying who, on what, how much; none of what was said.
    std::thread::sleep(Duration::from_millis(100));
    let traces = std::fs::read_to_string(pool.data.join("gateway-traces.log")).unwrap();
    assert!(traces.contains("\"who\":\"ci\"") && traces.contains("\"account\":\"good\""));
    assert!(traces.contains("\"input_tokens\":11") && traces.contains("\"output_tokens\":22"));
    assert!(traces.contains("\"output_tokens\":9") && traces.contains("limited: 429"));
    for secret in ["k-good", "a-1", "b-1", KEY, "hush-hush"] {
        assert!(!traces.contains(secret), "{secret} is in the traces");
    }
}

#[test]
fn an_administrator_adds_accounts_to_the_pool_from_their_own_machine() {
    use llmpool_daemon::KeyReq;
    let pool = machine(&[]);
    let config = Config {
        public_url: "http://127.0.0.1:1".into(),
        api_keys: Config::keys(&format!("ci:{KEY},member:member-key-0123456789abcdef")).unwrap(),
        admins: vec!["ci".into()],
        ..Config::default()
    };
    let serving = team_server::start(pool.daemon.clone(), config, "127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", serving.port);

    let admin = machine(&[("spare@x.io", "spare-1"), ("busy@x.io", "busy-1")]);
    admin
        .daemon
        .add_key(KeyReq {
            provider: "openai".into(),
            label: None,
            tags: vec!["work".into()],
            env: [("OPENAI_API_KEY".to_string(), "sk-shared".to_string())].into(),
        })
        .unwrap();
    // Not before joining.
    assert_eq!(
        admin.daemon.team_share("openai", true).unwrap_err().status,
        409
    );
    admin.daemon.team_connect(&url, Some(KEY)).unwrap();
    assert_eq!(admin.daemon.team_status()["admin"], true);

    // A key is copied: the pool has it, and so does its owner.
    admin.daemon.team_share("openai", true).unwrap();
    assert_eq!(accounts(&pool)["openai"]["tags"], json!(["work"]));
    assert_eq!(accounts(&admin)["openai"]["shared"], false);

    // A login is moved: one place renews it, and that place is now the server.
    // Its owner gets it back the way every member does, without the part
    // that renews.
    let moved = admin.daemon.team_share("spare@x.io", true).unwrap();
    assert_eq!(moved["moved"], true);
    let server_copy = FileVault(pool.data.join("vault"))
        .get(accounts(&pool)["spare@x.io"]["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(server_copy.contains("r-spare-1"));
    let mine = &accounts(&admin)["spare@x.io"];
    assert_eq!(mine["shared"], true);
    let lent = FileVault(admin.data.join("vault"))
        .get(mine["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert!(lent.contains("spare-1") && !lent.contains("r-spare-1"));
    // What already came from the pool is not sent back to it.
    assert_eq!(
        admin
            .daemon
            .team_share("spare@x.io", true)
            .unwrap_err()
            .status,
        409
    );

    // A login a session is running on stays where it is.
    start_on(&admin, "busy@x.io");
    assert_eq!(
        admin
            .daemon
            .team_share("busy@x.io", true)
            .unwrap_err()
            .status,
        409
    );
    assert_eq!(accounts(&admin)["busy@x.io"]["shared"], false);

    // Members who are not administrators cannot add to the pool.
    let member = machine(&[("mine@x.io", "mine-1")]);
    member
        .daemon
        .team_connect(&url, Some("member-key-0123456789abcdef"))
        .unwrap();
    assert_eq!(member.daemon.team_status()["admin"], false);
    assert_eq!(
        member
            .daemon
            .team_share("mine@x.io", true)
            .unwrap_err()
            .status,
        403
    );
    assert_eq!(
        accounts(&member)["mine@x.io"]["shared"],
        false,
        "a refused login stays"
    );
    assert!(!accounts(&pool).contains_key("mine@x.io"));

    let audit = std::fs::read_to_string(pool.data.join("team-audit.log")).unwrap();
    assert!(
        audit.contains("account_added")
            && audit.contains("spare@x.io")
            && !audit.contains("spare-1\"")
    );
}

/// Serve on a port that is certainly ours: another test may take the one we
/// were about to use between finding it free and listening on it.
fn on_a_free_port(pool: &Machine, config: impl Fn(String) -> Config) -> (Serving, String) {
    for _ in 0..20 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}");
        if let Ok(serving) = team_server::start(
            pool.daemon.clone(),
            config(url.clone()),
            &format!("127.0.0.1:{port}"),
        ) {
            return (serving, url);
        }
    }
    panic!("no free port");
}

/// A decision model that always sends a member to the account nobody is on.
fn decision_model() -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}",
        server.server_addr().to_ip().unwrap().port()
    );
    std::thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let asked: Value = serde_json::from_str(&body).unwrap_or_default();
            let free = asked["state"]["accounts"]
                .as_object()
                .and_then(|accounts| accounts.iter().find(|(_, a)| a["sessions_using_it"] == 0))
                .map(|(letter, _)| letter.clone())
                .unwrap_or_default();
            assert!(
                !body.contains("x.io"),
                "an address reached the decision model"
            );
            let answer = json!({"code": 0, "data": {"status": "completed", "result": {"answers": {"start_on": {"choice": free}}}}});
            let _ = request.respond(tiny_http::Response::from_string(answer.to_string()));
        }
    });
    url
}

#[allow(clippy::result_large_err)]
#[test]
fn the_gateway_carries_many_requests_at_once_and_follows_the_decision_model() {
    use llmpool_daemon::KeyReq;
    let provider = upstream();
    let pool = machine(&[("m@x.io", "m-1"), ("n@x.io", "n-1")]);
    pool.daemon
        .add_key(KeyReq {
            provider: "openai".into(),
            label: None,
            tags: vec![],
            env: [
                ("OPENAI_API_KEY".to_string(), "k-good".to_string()),
                ("OPENAI_BASE_URL".to_string(), provider.url.clone()),
            ]
            .into(),
        })
        .unwrap();
    pool.daemon
        .put_policy(&format!(
            "[advisor]\nenabled = true\nurl = \"{}\"\n",
            decision_model()
        ))
        .unwrap();
    FileVault(pool.data.join("vault"))
        .put("advisor-jev", "model-key")
        .unwrap();
    let (_serving, url) = on_a_free_port(&pool, |url| Config {
        public_url: url,
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        admins: vec!["ci".into()],
        gateway: true,
        upstreams: [("claude".to_string(), provider.url.clone())].into(),
        ..Config::default()
    });
    let post = |path: &str| {
        ureq::post(&format!("{url}/gateway/{path}"))
            .set("Authorization", &format!("Bearer {KEY}"))
            .send_json(json!({"model": "m-1", "messages": []}))
    };

    // Twelve answers that each take a while, all in flight together, not
    // twelve in a queue of four: none finishes until all twelve are open.
    use std::sync::atomic::Ordering::SeqCst;
    provider.together.store(12, SeqCst);
    let slow: Vec<_> = (0..12)
        .map(|_| {
            let url = url.clone();
            std::thread::spawn(move || {
                ureq::post(&format!(
                    "{url}/gateway/openai/v1/chat/completions?mode=stream"
                ))
                .set("Authorization", &format!("Bearer {KEY}"))
                .send_json(json!({"model": "m-1"}))
                .unwrap()
                .into_string()
                .unwrap()
            })
        })
        .collect();
    for answer in slow {
        assert!(answer.join().unwrap().contains("output_tokens"));
    }
    assert_eq!(provider.peak.load(SeqCst), 12);
    provider.together.store(1, SeqCst);

    // The member starts on one login, which fills past half. The decision
    // model is asked, off the request's own time, and sends them to the one
    // nobody is on; the next request goes there, before any limit was hit.
    let able = |name: &str, on: bool| {
        let patch = PatchReq {
            label: None,
            tags: None,
            enabled: Some(on),
        };
        pool.daemon.patch_account(name, patch).unwrap();
    };
    able("n@x.io", false);
    post("claude/v1/messages").unwrap();
    able("n@x.io", true);
    post("claude/v1/messages").unwrap();
    assert_eq!(
        provider.seen.lock().unwrap().last().unwrap()["authorization"],
        "Bearer m-1"
    );
    std::thread::sleep(Duration::from_millis(400));
    post("claude/v1/messages").unwrap();
    assert_eq!(
        provider.seen.lock().unwrap().last().unwrap()["authorization"],
        "Bearer n-1"
    );
    let audit = std::fs::read_to_string(pool.data.join("team-audit.log")).unwrap();
    assert!(audit.contains("gateway_advised") && audit.contains("the decision model chose n@x.io"));

    // The page's report adds it all up, and narrows to what is asked for.
    let login = ureq::AgentBuilder::new()
        .redirects(0)
        .build()
        .post(&format!("{url}/admin/login"))
        .send_form(&[("key", KEY)])
        .unwrap();
    let session = login
        .header("Set-Cookie")
        .unwrap()
        .split(['=', ';'])
        .nth(1)
        .unwrap()
        .to_string();
    let report = |filter: Value| -> Value {
        ureq::post(&format!("{url}/admin/invoke/api"))
            .set("Cookie", &format!("llmpool_session={session}"))
            .set("X-Llmpool", "1")
            .send_json(json!({"method": "POST", "path": "/server/requests", "body": filter}))
            .unwrap()
            .into_json()
            .unwrap()
    };
    let all = report(json!({}));
    assert_eq!(all["totals"]["requests"], 15);
    assert_eq!(all["totals"]["failed"], 0);
    assert_eq!(
        all["per_minute"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_u64)
            .sum::<u64>(),
        15
    );
    assert_eq!(all["decisions"][0]["chose"], "n@x.io");
    assert_eq!(all["decisions"][0]["moved"], true);
    let claude = report(json!({"provider": "claude"}));
    assert_eq!(claude["totals"]["requests"], 3);
    assert_eq!(claude["by_account"].as_array().unwrap().len(), 2);
    assert_eq!(
        report(json!({"account": "n@x.io"}))["totals"]["requests"],
        1
    );
    assert_eq!(report(json!({"result": "failed"}))["totals"]["requests"], 0);
}

#[allow(clippy::result_large_err)]
#[test]
fn the_server_tells_browsers_to_be_careful_slows_guessing_and_stops_gently() {
    use llmpool_daemon::KeyReq;
    let provider = upstream();
    let pool = machine(&[]);
    pool.daemon
        .add_key(KeyReq {
            provider: "openai".into(),
            label: None,
            tags: vec![],
            env: [
                ("OPENAI_API_KEY".to_string(), "k-good".to_string()),
                ("OPENAI_BASE_URL".to_string(), provider.url.clone()),
            ]
            .into(),
        })
        .unwrap();
    let (serving, url) = on_a_free_port(&pool, |url| Config {
        public_url: url,
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        admins: vec!["ci".into()],
        gateway: true,
        ..Config::default()
    });

    // Pages come with instructions for the browser.
    let page = ureq::get(&format!("{url}/login")).call().unwrap();
    let policy = page.header("Content-Security-Policy").unwrap();
    assert!(policy.contains("default-src 'self'") && policy.contains("frame-ancestors 'none'"));
    assert_eq!(page.header("X-Frame-Options"), Some("DENY"));
    assert_eq!(page.header("Referrer-Policy"), Some("no-referrer"));

    // An answer that is on its way when the server is told to stop still
    // arrives whole; nothing new is taken.
    let streaming = {
        let url = url.clone();
        std::thread::spawn(move || {
            ureq::post(&format!(
                "{url}/gateway/openai/v1/chat/completions?mode=stream"
            ))
            .set("Authorization", &format!("Bearer {KEY}"))
            .send_json(json!({"model": "m"}))
            .unwrap()
            .into_string()
            .unwrap()
        })
    };
    std::thread::sleep(Duration::from_millis(250));
    let began = std::time::Instant::now();
    serving.drain(Duration::from_secs(5));
    assert!(
        began.elapsed() >= Duration::from_millis(200),
        "it did not wait for the answer in flight"
    );
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "it waited after the answer was done"
    );
    assert!(streaming.join().unwrap().contains("output_tokens"));
}

#[allow(clippy::result_large_err)]
#[test]
fn guessing_tokens_gets_slow_for_everyone_who_is_guessing() {
    let pool = machine(&[]);
    let (_serving, url) = on_a_free_port(&pool, |url| Config {
        public_url: url,
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        ..Config::default()
    });
    let try_token = |token: &str| match ureq::get(&format!("{url}/v1/me"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(e) => panic!("{e}"),
    };
    let answers: Vec<u16> = (0..60).map(|i| try_token(&format!("guess-{i}"))).collect();
    assert_eq!(answers[0], 401);
    assert_eq!(*answers.last().unwrap(), 429);
    // Someone with the right token is not slowed down by the guessing.
    assert_eq!(try_token(KEY), 200);
}

#[allow(clippy::result_large_err)]
#[test]
fn the_gateway_hands_out_models_and_nothing_else_and_one_member_cannot_spoil_it() {
    use llmpool_daemon::KeyReq;
    let provider = upstream();
    let pool = machine(&[]);
    pool.daemon
        .add_key(KeyReq {
            provider: "openai".into(),
            label: None,
            tags: vec![],
            env: [
                ("OPENAI_API_KEY".to_string(), "k-good".to_string()),
                ("OPENAI_BASE_URL".to_string(), provider.url.clone()),
            ]
            .into(),
        })
        .unwrap();
    let (_serving, url) = on_a_free_port(&pool, |url| Config {
        public_url: url,
        api_keys: Config::keys(&format!("ci:{KEY},other:other-key-0123456789abcdefgh")).unwrap(),
        gateway: true,
        ..Config::default()
    });
    let send = |method: &str, path: &str, key: &str, body: Value| {
        let request = ureq::request(method, &format!("{url}/gateway/{path}"))
            .set("Authorization", &format!("Bearer {key}"));
        let answer = if method == "GET" {
            request.call()
        } else {
            request.send_json(body)
        };
        match answer {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(e) => panic!("{e}"),
        }
    };
    let seen = || provider.seen.lock().unwrap().len();

    // Asking a model is carried. What else the account could do at the
    // provider (files, stored work, the account itself) never leaves here.
    assert_eq!(
        send(
            "POST",
            "openai/v1/chat/completions",
            KEY,
            json!({"model": "m"})
        ),
        200
    );
    assert_eq!(send("GET", "openai/v1/models", KEY, json!(null)), 200);
    let before = seen();
    for (method, path) in [
        ("GET", "openai/v1/files"),
        ("POST", "openai/v1/files"),
        ("DELETE", "openai/v1/models/some-model"),
        ("POST", "openai/v1/fine_tuning/jobs"),
        ("GET", "openai/v1/organization/api_keys"),
        ("GET", "claude/api/oauth/profile"),
        ("POST", "openai/v1/../v1/files"),
        ("POST", "openai/v1/chat/completions/../../files"),
        ("POST", "openai/v1/%2e%2e/files"),
        ("POST", "openai//evil.example/v1/messages"),
        ("GET", "openai/v1/models/a/b"),
    ] {
        assert_eq!(send(method, path, KEY, json!({})), 403, "{method} {path}");
    }
    assert_eq!(seen(), before, "a refused request reached the provider");

    // What a member writes into a request cannot bloat what the server keeps.
    send(
        "POST",
        "openai/v1/chat/completions",
        KEY,
        json!({"model": "x".repeat(50_000)}),
    );
    std::thread::sleep(Duration::from_millis(100));
    let traces = std::fs::read_to_string(pool.data.join("gateway-traces.log")).unwrap();
    assert!(
        traces.lines().all(|line| line.len() < 1000),
        "a trace line grew with the request"
    );

    // One member with a great many requests in flight is told to wait; the
    // member beside them is not.
    let flood: Vec<_> = (0..24)
        .map(|_| {
            let url = url.clone();
            std::thread::spawn(move || {
                match ureq::post(&format!(
                    "{url}/gateway/openai/v1/chat/completions?mode=stream"
                ))
                .set("Authorization", &format!("Bearer {KEY}"))
                .send_json(json!({"model": "m"}))
                {
                    Ok(r) => {
                        let status = r.status();
                        let _ = r.into_string();
                        status
                    }
                    Err(ureq::Error::Status(code, _)) => code,
                    Err(e) => panic!("{e}"),
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        send(
            "POST",
            "openai/v1/chat/completions",
            "other-key-0123456789abcdefgh",
            json!({"model": "m"})
        ),
        200
    );
    let answers: Vec<u16> = flood.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(answers.contains(&429), "{answers:?}");
    assert!(
        answers.iter().filter(|a| **a == 200).count() >= 16,
        "{answers:?}"
    );
    // And when theirs are done, they are welcome again. The server lets go of
    // a request a moment after its last byte is read here, so it is given one.
    let again = || {
        send(
            "POST",
            "openai/v1/chat/completions",
            KEY,
            json!({"model": "m"}),
        )
    };
    let began = std::time::Instant::now();
    while again() != 200 && began.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(again(), 200);
}

#[test]
fn settings_that_cannot_be_read_stop_the_server_rather_than_loosen_it() {
    let pool = machine(&[]);
    std::fs::write(
        pool.data.join("team-settings.json"),
        "{\"share_logins\": tru",
    )
    .unwrap();
    let config = Config {
        public_url: "http://127.0.0.1:1".into(),
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        ..Config::default()
    };
    let refused = team_server::start(pool.daemon.clone(), config, "127.0.0.1:0");
    assert!(refused.is_err_and(|e| e.to_string().contains("team-settings.json")));
}

#[allow(clippy::result_large_err)]
#[test]
fn an_invitation_page_says_who_invites_to_what_with_the_line_to_run() {
    let pool = machine(&[]);
    let (_serving, url) = on_a_free_port(&pool, |url| Config {
        public_url: url,
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        admins: vec!["ci".into()],
        ..Config::default()
    });
    let get = |path: &str| ureq::get(&format!("{url}{path}")).call().unwrap();

    // Anyone with the link sees it; it carries no secret and runs no script
    // of its own making.
    let page = get("/join?from=Dana%20Lee").into_string().unwrap();
    assert!(page.contains("Dana Lee invited you to the"));
    assert!(
        page.contains(&format!("llmpool team join {url} --key")),
        "a key-only pool says so"
    );
    assert!(page.contains("scripts/install.sh | sh"));
    assert!(get("/join")
        .header("Content-Security-Policy")
        .unwrap()
        .contains("default-src 'self'"));
    assert!(get("/join.js").into_string().unwrap().contains("clipboard"));

    // A name on an invitation is a name: nothing else gets onto the page.
    let hostile = get("/join?from=%3Cscript%3Ealert(1)%3C%2Fscript%3E%22onload%3D")
        .into_string()
        .unwrap();
    assert!(!hostile.contains("<script>alert") && !hostile.contains("onload="));
    assert!(hostile.contains("scriptalert1script"));

    // An administrator names the pool; the invitation uses the name.
    let login = ureq::AgentBuilder::new()
        .redirects(0)
        .build()
        .post(&format!("{url}/admin/login"))
        .send_form(&[("key", KEY)])
        .unwrap();
    let session = login
        .header("Set-Cookie")
        .unwrap()
        .split(['=', ';'])
        .nth(1)
        .unwrap()
        .to_string();
    let saved: Value = ureq::post(&format!("{url}/admin/invoke/api"))
        .set("Cookie", &format!("llmpool_session={session}"))
        .set("X-Llmpool", "1")
        .send_json(
            json!({"method": "PUT", "path": "/server", "body": {"name": "Acme <b>Labs</b>"}}),
        )
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(saved["invite"], format!("{url}/join"));
    let named = get("/join").into_string().unwrap();
    assert!(named.contains("You are invited to the Acme &lt;b&gt;Labs&lt;/b&gt; LLM pool"));
}

#[allow(clippy::result_large_err)]
#[test]
fn a_claude_session_overflows_to_an_anthropic_key_and_every_request_is_a_span() {
    use llmpool_daemon::team_server::otlp::Otlp;
    use llmpool_daemon::KeyReq;
    let provider = upstream();
    // Keys go through another door than subscriptions do.
    let key_door = upstream();
    // A collector that keeps what it is sent.
    let collector = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let collector_url = format!(
        "http://127.0.0.1:{}",
        collector.server_addr().to_ip().unwrap().port()
    );
    let taken = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
    let keep = taken.clone();
    std::thread::spawn(move || {
        for mut request in collector.incoming_requests() {
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let auth = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("x-team"))
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default();
            keep.lock()
                .unwrap()
                .push((request.url().to_string(), auth, body));
            let _ = request.respond(tiny_http::Response::from_string("{}"));
        }
    });

    // One subscription login, which the provider says is nearly full, and a key.
    let pool = machine(&[("a@x.io", "a-1")]);
    pool.daemon
        .add_key(KeyReq {
            provider: "anthropic".into(),
            label: Some("paid".into()),
            tags: vec![],
            env: [("ANTHROPIC_API_KEY".to_string(), "sk-over".to_string())].into(),
        })
        .unwrap();
    let (_serving, url) = on_a_free_port(&pool, |url| Config {
        public_url: url,
        api_keys: Config::keys(&format!("ci:{KEY}")).unwrap(),
        gateway: true,
        upstreams: [
            ("claude".to_string(), provider.url.clone()),
            ("anthropic".to_string(), key_door.url.clone()),
        ]
        .into(),
        otlp: Some(Otlp {
            endpoint: collector_url.clone(),
            headers: vec![("x-team".into(), "t-1".into())],
        }),
        ..Config::default()
    });
    let ask = || {
        ureq::post(&format!("{url}/gateway/claude/v1/messages"))
            .set("Authorization", &format!("Bearer {KEY}"))
            .set("anthropic-beta", "oauth-2025-04-20,tools-1")
            .send_json(json!({"model": "m-9", "messages": []}))
            .unwrap()
            .status()
    };
    // The first runs on the subscription; its answer says that is 95% used,
    // so the next is carried by the key, as a key and not as a login.
    assert_eq!(ask(), 200);
    assert_eq!(ask(), 200);
    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "the key was shown to the subscriptions' door"
    );
    assert_eq!(seen[0]["authorization"], "Bearer a-1");
    let seen = key_door.seen.lock().unwrap().clone();
    assert_eq!(seen[0]["x-api-key"], "sk-over");
    assert!(!seen[0].contains_key("authorization"));
    assert!(!seen[0]
        .get("anthropic-beta")
        .is_some_and(|b| b.contains("oauth")));

    // A pool with nothing but the key still carries Claude sessions.
    let ann = machine(&[]);
    ann.daemon.team_connect(&url, Some(KEY)).unwrap();
    assert!(accounts(&ann).contains_key("team gateway (claude)"));

    // Both requests reached the collector, with its header, as spans.
    let began = std::time::Instant::now();
    while taken.lock().unwrap().is_empty() && began.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let taken = taken.lock().unwrap().clone();
    let (path, header, body) = &taken[0];
    assert_eq!(path, "/v1/traces");
    assert_eq!(header, "t-1");
    let sent: Value = serde_json::from_str(body).unwrap();
    let span = &sent["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(span["name"], "chat m-9");
    assert_eq!(span["traceId"].as_str().unwrap().len(), 32);
    let attributes = span["attributes"].to_string();
    assert!(attributes.contains("gen_ai.request.model"), "{attributes}");
    assert!(attributes.contains("\"ci\""), "{attributes}");
    assert!(!body.contains("sk-over") && !body.contains("a-1"));
}
