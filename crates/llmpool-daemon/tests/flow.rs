//! The daemon end to end against a fake home directory: no real Claude Code,
//! no real Keychain, no network.

use llmpool_adapter_claude::profile::{NoKeychain, Profile, Secret};
use llmpool_core::vault::{FileVault, Vault};
use llmpool_daemon::{http, CaptureReq, Daemon, StartReq};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct World {
    _dir: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
    daemon: Arc<Daemon>,
}

fn secret(email: &str, token: &str) -> Secret {
    Secret {
        oauth: json!({"accessToken": token, "refreshToken": format!("r-{token}")}),
        account: json!({"emailAddress": email, "organizationUuid": "org"}),
    }
}

/// A home whose default profile is logged in as `home@x.io`, with that login
/// and two more (`a@x.io` tagged work, `b@x.io` tagged work) in the pool.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let default = Profile::default_in(&home);
    default.write(&NoKeychain, &secret("home@x.io", "home-1")).unwrap();
    std::fs::write(default.config_dir.join("settings.json"), "{}").unwrap();

    let daemon = Daemon::open(&data, &home, Box::new(FileVault(data.join("vault"))), Box::new(NoKeychain)).unwrap();
    daemon.capture(CaptureReq::default()).unwrap();
    for (email, token) in [("a@x.io", "a-1"), ("b@x.io", "b-1")] {
        let scratch = data.join(format!("login-{token}"));
        Profile::at(&scratch).write(&NoKeychain, &secret(email, token)).unwrap();
        let req = CaptureReq { profile_dir: Some(scratch.clone()), label: None, tags: vec!["work".into()] };
        daemon.capture(req).unwrap();
        assert!(!scratch.exists(), "scratch login profile should be removed after capture");
    }
    World { _dir: dir, home, data, daemon: Arc::new(daemon) }
}

fn start(w: &World, cwd: &str, account: Option<&str>) -> Value {
    let req = StartReq { cwd: cwd.into(), git_remote: None, pid: std::process::id(), account: account.map(Into::into), tag: None };
    w.daemon.start_session(req).unwrap()
}

fn login_in(dir: &Path) -> String {
    Profile::at(dir).read(&NoKeychain).unwrap().email().unwrap().to_string()
}

/// Ties between equally used accounts go to the lower id, and ids are
/// hashes, so which work account is picked first is fixed but not obvious.
fn other(email: &str) -> &'static str {
    if email == "a@x.io" { "b@x.io" } else { "a@x.io" }
}

fn usage(percent: f64) -> Value {
    json!({"rate_limits": {"five_hour": {"used_percentage": percent, "resets_at": 4_000_000_000i64}}})
}

fn vault_token(w: &World, email: &str) -> String {
    let accounts = w.daemon.list_accounts().unwrap();
    let id = accounts.as_array().unwrap().iter().find(|a| a["email"] == email).unwrap()["id"].as_str().unwrap().to_string();
    let secret: Secret = serde_json::from_str(&FileVault(w.data.join("vault")).get(&id).unwrap().unwrap()).unwrap();
    secret.oauth["accessToken"].as_str().unwrap().to_string()
}

const POLICY: &str = r#"
[settings]
cooldown_secs = 0

[[rule]]
name = "work repos"
on = "session_start"
when = { cwd = "/work/**" }
do = { assign = { tag = "work" } }

[[rule]]
name = "swap before the limit"
on = { usage_above = { percent = 90 } }
do = { swap = { tag = "same" }, notify = true }
"#;

#[test]
fn capture_lists_accounts_and_marks_the_default_login() {
    let w = world();
    let accounts = w.daemon.list_accounts().unwrap();
    let rows = accounts.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let home = rows.iter().find(|a| a["email"] == "home@x.io").unwrap();
    assert_eq!(home["is_default"], true);
    assert_eq!(rows.iter().filter(|a| a["tags"] == json!(["work"])).count(), 2);
    // Capturing the same login again updates it rather than adding a row.
    w.daemon.capture(CaptureReq::default()).unwrap();
    assert_eq!(w.daemon.list_accounts().unwrap().as_array().unwrap().len(), 3);
}

#[test]
fn session_start_assigns_by_policy_into_its_own_profile() {
    let w = world();
    w.daemon.put_policy(POLICY).unwrap();

    let reply = start(&w, "/work/api", None);
    assert_eq!(reply["rule"], "work repos");
    let first = reply["account"]["email"].as_str().unwrap();
    assert!(first == "a@x.io" || first == "b@x.io", "{first}");
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    assert_eq!(login_in(&dir), first);
    assert!(dir.join("settings.json").symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(reply["env"]["LLMPOOL_SESSION"], reply["session_id"]);
    // The user's own profile is untouched.
    assert_eq!(Profile::default_in(&w.home).identity().unwrap().0, "home@x.io");

    // No rule matches elsewhere: the agent runs on its own login.
    let reply = start(&w, "/elsewhere", None);
    assert_eq!(reply["account"], Value::Null);
    assert_eq!(reply["env"], json!({}));
}

#[test]
fn the_default_login_and_a_shared_account_reuse_one_profile() {
    let w = world();
    // The account that is the default login runs in the default profile.
    let reply = start(&w, "/x", Some("home@x.io"));
    assert!(reply["env"].get("CLAUDE_CONFIG_DIR").is_none());

    // Two sessions on one account share a profile: a second copy of the login
    // would invalidate the first when either refreshes its token.
    let first = start(&w, "/x", Some("a@x.io"));
    let second = start(&w, "/y", Some("a@x.io"));
    assert_eq!(first["env"]["CLAUDE_CONFIG_DIR"], second["env"]["CLAUDE_CONFIG_DIR"]);
}

#[test]
fn usage_over_the_threshold_swaps_the_login_and_tells_the_session() {
    let w = world();
    w.daemon.put_policy(POLICY).unwrap();
    let reply = start(&w, "/work/api", None);
    let sid = reply["session_id"].as_str().unwrap();
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    let first = reply["account"]["email"].as_str().unwrap();
    let second = other(first);

    // Below the threshold nothing happens.
    let line = w.daemon.statusline(sid, &usage(40.0));
    assert_eq!(line["line"], format!("llmpool: {first} · 5h 40%"));
    assert_eq!(w.daemon.hook(sid, "UserPromptSubmit", "", &json!({})), json!({}));

    // Claude Code rotated the token while the session ran.
    Profile::at(&dir).write(&NoKeychain, &secret(first, "rotated")).unwrap();

    w.daemon.statusline(sid, &usage(93.0));
    assert_eq!(login_in(&dir), second);
    assert_eq!(vault_token(&w, first), "rotated", "the rotated token must be saved before it is overwritten");

    let out = w.daemon.hook(sid, "UserPromptSubmit", "", &json!({}));
    let message = out["systemMessage"].as_str().unwrap();
    assert!(message.contains(&format!("moved from {first} to {second}")), "{message}");
    assert!(message.contains("swap before the limit"), "{message}");
    assert_eq!(out["hookSpecificOutput"]["additionalContext"], out["systemMessage"]);
    // Delivered once.
    assert_eq!(w.daemon.hook(sid, "UserPromptSubmit", "", &json!({})), json!({}));

    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions[0]["account_label"], second);
    assert!(w.daemon.events().as_array().unwrap().iter().any(|e| e["kind"] == "swap"));
}

#[test]
fn a_reading_right_after_a_swap_is_not_charged_to_the_new_account() {
    let w = world();
    w.daemon.put_policy(POLICY).unwrap();
    let reply = start(&w, "/work/api", None);
    let sid = reply["session_id"].as_str().unwrap();
    let second = other(reply["account"]["email"].as_str().unwrap());
    w.daemon.statusline(sid, &usage(93.0));
    // Claude Code still reports the old account's numbers for a short while.
    w.daemon.statusline(sid, &usage(93.0));
    let accounts = w.daemon.list_accounts().unwrap();
    let target = accounts.as_array().unwrap().iter().find(|a| a["email"] == second).unwrap();
    assert_eq!(target["usage"], Value::Null);
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    assert_eq!(login_in(&dir), second, "must not bounce straight back");
}

#[test]
fn rate_limit_with_nowhere_to_go_says_so_once() {
    let w = world();
    w.daemon.put_policy("[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = { tag = 'nobody' } }\n").unwrap();
    let reply = start(&w, "/x", Some("a@x.io"));
    let sid = reply["session_id"].as_str().unwrap();
    w.daemon.hook(sid, "StopFailure", "rate_limit", &json!({}));
    w.daemon.hook(sid, "StopFailure", "rate_limit", &json!({}));
    let out = w.daemon.hook(sid, "UserPromptSubmit", "", &json!({}));
    let message = out["systemMessage"].as_str().unwrap();
    assert_eq!(message.matches("no other account is available").count(), 1, "{message}");
}

#[test]
fn manual_swap_refuses_an_account_that_is_live_elsewhere() {
    let w = world();
    let one = start(&w, "/x", Some("a@x.io"));
    start(&w, "/y", Some("b@x.io"));
    let err = w.daemon.swap_session(one["session_id"].as_str().unwrap(), "b@x.io").unwrap_err();
    assert_eq!(err.status, 409);
    assert!(err.message.contains("in use"), "{}", err.message);
    // An unmanaged session cannot be swapped at all.
    let plain = start(&w, "/z", None);
    assert_eq!(w.daemon.swap_session(plain["session_id"].as_str().unwrap(), "b@x.io").unwrap_err().status, 409);
}

#[test]
fn ending_the_last_session_saves_the_login_and_removes_the_profile() {
    let w = world();
    let reply = start(&w, "/x", Some("a@x.io"));
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    Profile::at(&dir).write(&NoKeychain, &secret("a@x.io", "a-9")).unwrap();
    w.daemon.end_session(reply["session_id"].as_str().unwrap());
    assert!(!dir.exists());
    assert_eq!(vault_token(&w, "a@x.io"), "a-9");
    assert_eq!(w.daemon.list_sessions().unwrap(), json!([]));
    // The account is free again and gets a fresh profile with the saved token.
    let again = start(&w, "/x", Some("a@x.io"));
    let dir = PathBuf::from(again["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    assert_eq!(Profile::at(&dir).read(&NoKeychain).unwrap().oauth["accessToken"], "a-9");
}

#[test]
fn sessions_of_dead_launchers_are_cleaned_up_and_state_survives_restart() {
    let w = world();
    let req = StartReq { cwd: "/x".into(), git_remote: None, pid: 999_999_999, account: Some("a@x.io".into()), tag: None };
    let reply = w.daemon.start_session(req).unwrap();
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    start(&w, "/y", Some("b@x.io"));
    assert_eq!(w.daemon.list_sessions().unwrap().as_array().unwrap().len(), 1);
    assert!(!dir.exists());

    drop(w.daemon.clone());
    let reopened = Daemon::open(&w.data, &w.home, Box::new(FileVault(w.data.join("vault"))), Box::new(NoKeychain)).unwrap();
    let sessions = reopened.list_sessions().unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["account_label"], "b@x.io");
}

#[test]
fn policies_are_validated_on_save_and_can_be_dry_run() {
    let w = world();
    let err = w.daemon.put_policy("[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n").unwrap_err();
    assert_eq!(err.status, 400);
    assert_eq!(w.daemon.policy_text()["text"], "");

    let req = serde_json::from_value(json!({"text": POLICY, "event": "session_start", "cwd": "/work/x"})).unwrap();
    let out = w.daemon.dry_run(req).unwrap();
    assert_eq!(out["decision"]["rule"], "work repos");
    let id = out["decision"]["outcome"]["assign"].as_str().unwrap();
    assert!(out["labels"][id].as_str().unwrap().ends_with("@x.io"));
    assert!(w.daemon.schema()["triggers"].as_array().unwrap().iter().any(|t| t["kind"] == "usage_above"));
}

#[test]
fn http_api_requires_the_token_and_serves_json() {
    let w = world();
    let running = http::start(w.daemon.clone()).unwrap();
    let endpoint = http::Endpoint::read(&w.data).unwrap();
    assert_eq!(endpoint.port, running.endpoint.port);
    let url = |path: &str| format!("http://127.0.0.1:{}{path}", endpoint.port);
    let auth = format!("Bearer {}", endpoint.token);

    assert_eq!(ureq::get(&url("/accounts")).call().unwrap_err().into_response().unwrap().status(), 401);
    let accounts: Value = ureq::get(&url("/accounts")).set("Authorization", &auth).call().unwrap().into_json().unwrap();
    assert_eq!(accounts.as_array().unwrap().len(), 3);

    let body = json!({"cwd": "/x", "pid": std::process::id(), "account": "a@x.io"});
    let reply: Value = ureq::post(&url("/sessions")).set("Authorization", &auth).send_json(body).unwrap().into_json().unwrap();
    let sid = reply["session_id"].as_str().unwrap();
    let line: Value = ureq::post(&url(&format!("/statusline/claude/{sid}")))
        .set("Authorization", &auth)
        .send_json(usage(12.0))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(line["line"], "llmpool: a@x.io · 5h 12%");

    let missing = ureq::post(&url("/sessions/nope/swap")).set("Authorization", &auth).send_json(json!({"account": "b@x.io"}));
    let response = missing.unwrap_err().into_response().unwrap();
    assert_eq!(response.status(), 404);
    assert!(response.into_json::<Value>().unwrap()["error"].as_str().unwrap().contains("no such session"));

    ureq::post(&url("/shutdown")).set("Authorization", &auth).call().unwrap();
    running.wait();
    assert!(http::Endpoint::read(&w.data).is_none());
}
