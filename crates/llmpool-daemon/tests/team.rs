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
