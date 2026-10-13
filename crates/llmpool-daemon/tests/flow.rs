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
    default
        .write(&NoKeychain, &secret("home@x.io", "home-1"))
        .unwrap();
    std::fs::write(default.config_dir.join("settings.json"), "{}").unwrap();

    let daemon = Daemon::open(
        &data,
        &home,
        Box::new(FileVault(data.join("vault"))),
        Box::new(NoKeychain),
    )
    .unwrap();
    daemon.capture(CaptureReq::default()).unwrap();
    for (email, token) in [("a@x.io", "a-1"), ("b@x.io", "b-1")] {
        let scratch = data.join(format!("login-{token}"));
        Profile::at(&scratch)
            .write(&NoKeychain, &secret(email, token))
            .unwrap();
        let req = CaptureReq {
            profile_dir: Some(scratch.clone()),
            label: None,
            tags: vec!["work".into()],
        };
        daemon.capture(req).unwrap();
        assert!(
            !scratch.exists(),
            "scratch login profile should be removed after capture"
        );
    }
    World {
        _dir: dir,
        home,
        data,
        daemon: Arc::new(daemon),
    }
}

fn start(w: &World, cwd: &str, account: Option<&str>) -> Value {
    let req = StartReq {
        cwd: cwd.into(),
        git_remote: None,
        pid: std::process::id(),
        account: account.map(Into::into),
        tag: None,
    };
    w.daemon.start_session(req).unwrap()
}

fn login_in(dir: &Path) -> String {
    Profile::at(dir)
        .read(&NoKeychain)
        .unwrap()
        .unwrap()
        .email()
        .unwrap()
        .to_string()
}

/// Ties between equally used accounts go to the lower id, and ids are
/// hashes, so which work account is picked first is fixed but not obvious.
fn other(email: &str) -> &'static str {
    if email == "a@x.io" {
        "b@x.io"
    } else {
        "a@x.io"
    }
}

fn usage(percent: f64) -> Value {
    json!({"rate_limits": {"five_hour": {"used_percentage": percent, "resets_at": 4_000_000_000i64}}})
}

fn vault_token(w: &World, email: &str) -> String {
    let accounts = w.daemon.list_accounts().unwrap();
    let id = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == email)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let secret: Secret =
        serde_json::from_str(&FileVault(w.data.join("vault")).get(&id).unwrap().unwrap()).unwrap();
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
    assert_eq!(
        rows.iter().filter(|a| a["tags"] == json!(["work"])).count(),
        2
    );
    // Capturing the same login again updates it rather than adding a row.
    w.daemon.capture(CaptureReq::default()).unwrap();
    assert_eq!(
        w.daemon.list_accounts().unwrap().as_array().unwrap().len(),
        3
    );
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
    assert!(dir
        .join("settings.json")
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(reply["env"]["LLMPOOL_SESSION"], reply["session_id"]);
    // The user's own profile is untouched.
    assert_eq!(
        Profile::default_in(&w.home).identity().unwrap().0,
        "home@x.io"
    );

    // No rule matches elsewhere: the agent runs on its own login.
    let reply = start(&w, "/elsewhere", None);
    assert_eq!(reply["account"], Value::Null);
    assert_eq!(
        reply["env"],
        json!({"LLMPOOL_SESSION": reply["session_id"]})
    );
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
    assert_eq!(
        first["env"]["CLAUDE_CONFIG_DIR"],
        second["env"]["CLAUDE_CONFIG_DIR"]
    );
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
    assert_eq!(
        w.daemon.hook(sid, "UserPromptSubmit", "", &json!({})),
        json!({})
    );

    // Claude Code rotated the token while the session ran.
    Profile::at(&dir)
        .write(&NoKeychain, &secret(first, "rotated"))
        .unwrap();

    w.daemon.statusline(sid, &usage(93.0));
    assert_eq!(login_in(&dir), second);
    assert_eq!(
        vault_token(&w, first),
        "rotated",
        "the rotated token must be saved before it is overwritten"
    );

    let out = w.daemon.hook(sid, "UserPromptSubmit", "", &json!({}));
    let message = out["systemMessage"].as_str().unwrap();
    assert!(
        message.contains(&format!("moved from {first} to {second}")),
        "{message}"
    );
    assert!(message.contains("swap before the limit"), "{message}");
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        out["systemMessage"]
    );
    // Delivered once.
    assert_eq!(
        w.daemon.hook(sid, "UserPromptSubmit", "", &json!({})),
        json!({})
    );

    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions[0]["account_label"], second);
    assert!(w
        .daemon
        .events()
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "swap"));
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
    let target = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == second)
        .unwrap();
    assert_eq!(target["usage"], Value::Null);
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    assert_eq!(login_in(&dir), second, "must not bounce straight back");
}

#[test]
fn rate_limit_with_nowhere_to_go_says_so_once() {
    let w = world();
    w.daemon
        .put_policy(
            "[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = { tag = 'nobody' } }\n",
        )
        .unwrap();
    let reply = start(&w, "/x", Some("a@x.io"));
    let sid = reply["session_id"].as_str().unwrap();
    w.daemon.hook(sid, "StopFailure", "rate_limit", &json!({}));
    w.daemon.hook(sid, "StopFailure", "rate_limit", &json!({}));
    let out = w.daemon.hook(sid, "UserPromptSubmit", "", &json!({}));
    let message = out["systemMessage"].as_str().unwrap();
    assert_eq!(
        message.matches("no other account is available").count(),
        1,
        "{message}"
    );
}

#[test]
fn manual_swap_refuses_an_account_that_is_live_elsewhere() {
    let w = world();
    let one = start(&w, "/x", Some("a@x.io"));
    start(&w, "/y", Some("b@x.io"));
    let err = w
        .daemon
        .swap_session(one["session_id"].as_str().unwrap(), "b@x.io")
        .unwrap_err();
    assert_eq!(err.status, 409);
    assert!(err.message.contains("in use"), "{}", err.message);
    // An unmanaged session cannot be swapped at all.
    let plain = start(&w, "/z", None);
    assert_eq!(
        w.daemon
            .swap_session(plain["session_id"].as_str().unwrap(), "b@x.io")
            .unwrap_err()
            .status,
        409
    );
}

#[test]
fn ending_the_last_session_saves_the_login_and_removes_the_profile() {
    let w = world();
    let reply = start(&w, "/x", Some("a@x.io"));
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    Profile::at(&dir)
        .write(&NoKeychain, &secret("a@x.io", "a-9"))
        .unwrap();
    w.daemon.end_session(reply["session_id"].as_str().unwrap());
    assert!(!dir.exists());
    assert_eq!(vault_token(&w, "a@x.io"), "a-9");
    assert_eq!(w.daemon.list_sessions().unwrap(), json!([]));
    // The account is free again and gets a fresh profile with the saved token.
    let again = start(&w, "/x", Some("a@x.io"));
    let dir = PathBuf::from(again["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    assert_eq!(
        Profile::at(&dir).read(&NoKeychain).unwrap().unwrap().oauth["accessToken"],
        "a-9"
    );
}

#[test]
fn sessions_of_dead_launchers_are_cleaned_up_and_state_survives_restart() {
    let w = world();
    let req = StartReq {
        cwd: "/x".into(),
        git_remote: None,
        pid: 999_999_999,
        account: Some("a@x.io".into()),
        tag: None,
    };
    let reply = w.daemon.start_session(req).unwrap();
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    start(&w, "/y", Some("b@x.io"));
    assert_eq!(
        w.daemon.list_sessions().unwrap().as_array().unwrap().len(),
        1
    );
    assert!(!dir.exists());

    drop(w.daemon.clone());
    let reopened = Daemon::open(
        &w.data,
        &w.home,
        Box::new(FileVault(w.data.join("vault"))),
        Box::new(NoKeychain),
    )
    .unwrap();
    let sessions = reopened.list_sessions().unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["account_label"], "b@x.io");
}

#[test]
fn policies_are_validated_on_save_and_can_be_dry_run() {
    let w = world();
    let err = w
        .daemon
        .put_policy("[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n")
        .unwrap_err();
    assert_eq!(err.status, 400);
    assert_eq!(w.daemon.policy_text()["text"], "");

    let req =
        serde_json::from_value(json!({"text": POLICY, "event": "session_start", "cwd": "/work/x"}))
            .unwrap();
    let out = w.daemon.dry_run(req).unwrap();
    assert_eq!(out["decision"]["rule"], "work repos");
    let id = out["decision"]["outcome"]["assign"].as_str().unwrap();
    assert!(out["labels"][id].as_str().unwrap().ends_with("@x.io"));
    // An editor can go from text to structure and back without saving.
    let parsed = w.daemon.parse_policy(POLICY).unwrap();
    assert_eq!(
        parsed["policy"]["rule"][1]["on"]["usage_above"]["percent"],
        90
    );
    let text = w.daemon.render_policy(parsed["policy"].clone()).unwrap();
    assert_eq!(
        w.daemon
            .parse_policy(text["text"].as_str().unwrap())
            .unwrap(),
        parsed
    );
    assert_eq!(
        w.daemon
            .parse_policy("[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n")
            .unwrap_err()
            .status,
        400
    );
    assert_eq!(w.daemon.policy_text()["text"], "", "parsing must not save");
    assert!(w.daemon.schema()["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["kind"] == "usage_above"));
}

#[test]
fn http_api_requires_the_token_and_serves_json() {
    let w = world();
    let running = http::start(w.daemon.clone(), http::instance_lock(&w.data).unwrap()).unwrap();
    let endpoint = http::Endpoint::read(&w.data).unwrap();
    assert_eq!(endpoint.port, running.endpoint.port);
    let url = |path: &str| format!("http://127.0.0.1:{}{path}", endpoint.port);
    let auth = format!("Bearer {}", endpoint.token);

    assert_eq!(
        ureq::get(&url("/accounts"))
            .call()
            .unwrap_err()
            .into_response()
            .unwrap()
            .status(),
        401
    );
    let accounts: Value = ureq::get(&url("/accounts"))
        .set("Authorization", &auth)
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(accounts.as_array().unwrap().len(), 3);

    let body = json!({"cwd": "/x", "pid": std::process::id(), "account": "a@x.io"});
    let reply: Value = ureq::post(&url("/sessions"))
        .set("Authorization", &auth)
        .send_json(body)
        .unwrap()
        .into_json()
        .unwrap();
    let sid = reply["session_id"].as_str().unwrap();
    let line: Value = ureq::post(&url(&format!("/statusline/claude/{sid}")))
        .set("Authorization", &auth)
        .send_json(usage(12.0))
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(line["line"], "llmpool: a@x.io · 5h 12%");

    let missing = ureq::post(&url("/sessions/nope/swap"))
        .set("Authorization", &auth)
        .send_json(json!({"account": "b@x.io"}));
    let response = missing.unwrap_err().into_response().unwrap();
    assert_eq!(response.status(), 404);
    assert!(response.into_json::<Value>().unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("no such session"));

    ureq::post(&url("/shutdown"))
        .set("Authorization", &auth)
        .call()
        .unwrap();
    running.wait();
    assert!(http::Endpoint::read(&w.data).is_none());
}

// ---- findings from review -------------------------------------------------

fn default_token(w: &World) -> String {
    Profile::default_in(&w.home)
        .read(&NoKeychain)
        .unwrap()
        .unwrap()
        .oauth["accessToken"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_profile_whose_login_cannot_be_saved_yet_is_kept_until_it_can() {
    let w = world();
    let reply = start(&w, "/x", Some("a@x.io"));
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    Profile::at(&dir)
        .write(&NoKeychain, &secret("a@x.io", "a-rotated"))
        .unwrap();
    // Claude Code is in the middle of refreshing this login.
    std::fs::create_dir(dir.join(".oauth_refresh.lock")).unwrap();

    w.daemon.end_session(reply["session_id"].as_str().unwrap());
    assert!(
        dir.exists(),
        "the only copy of the rotated login must not be deleted"
    );
    assert_eq!(vault_token(&w, "a@x.io"), "a-1");
    // Starting the account again reuses that profile instead of an old copy.
    let again = start(&w, "/x", Some("a@x.io"));
    assert_eq!(
        again["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap(),
        dir.to_str().unwrap()
    );
    w.daemon.end_session(again["session_id"].as_str().unwrap());

    std::fs::remove_dir(dir.join(".oauth_refresh.lock")).unwrap();
    w.daemon.list_sessions().unwrap();
    assert!(!dir.exists());
    assert_eq!(vault_token(&w, "a@x.io"), "a-rotated");
}

#[test]
fn swap_respects_what_the_profile_really_holds() {
    let w = world();
    let reply = start(&w, "/x", Some("home@x.io"));
    let sid = reply["session_id"].as_str().unwrap();
    let default = Profile::default_in(&w.home);

    // The user logged ~/.claude in to an account outside the pool.
    default
        .write(&NoKeychain, &secret("stranger@x.io", "stranger-1"))
        .unwrap();
    let err = w.daemon.swap_session(sid, "a@x.io").unwrap_err();
    assert_eq!(err.status, 409);
    assert!(err.message.contains("not in the pool"), "{}", err.message);
    assert_eq!(
        default_token(&w),
        "stranger-1",
        "a login llmpool cannot save must not be overwritten"
    );

    // The user logged ~/.claude in to pool account a, with a newer token than the vault's.
    default
        .write(&NoKeychain, &secret("a@x.io", "a-live"))
        .unwrap();
    w.daemon.swap_session(sid, "a@x.io").unwrap();
    assert_eq!(
        default_token(&w),
        "a-live",
        "the live token must not be replaced by the vault's older copy"
    );
    assert_eq!(vault_token(&w, "a@x.io"), "a-live");
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "a@x.io"
    );
}

#[test]
fn swap_refuses_an_account_already_live_in_a_session_profile() {
    let w = world();
    start(&w, "/x", Some("a@x.io"));
    let on_default = start(&w, "/y", Some("home@x.io"));
    // ~/.claude was logged in to a outside llmpool: a is now in two places.
    Profile::default_in(&w.home)
        .write(&NoKeychain, &secret("a@x.io", "a-other"))
        .unwrap();
    let err = w
        .daemon
        .swap_session(on_default["session_id"].as_str().unwrap(), "a@x.io")
        .unwrap_err();
    assert_eq!(err.status, 409);
}

#[test]
fn capture_only_ever_deletes_its_own_scratch_login_directories() {
    let w = world();
    // Outside the data directory, reached through `..`.
    let victim = w.data.parent().unwrap().join("victim");
    Profile::at(&victim)
        .write(&NoKeychain, &secret("v@x.io", "v-1"))
        .unwrap();
    let sneaky = w.data.join("..").join("victim");
    w.daemon
        .capture(CaptureReq {
            profile_dir: Some(sneaky),
            label: None,
            tags: vec![],
        })
        .unwrap();
    assert!(victim.exists());

    // A running session's profile.
    let reply = start(&w, "/x", Some("a@x.io"));
    let dir = PathBuf::from(reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap());
    let err = w
        .daemon
        .capture(CaptureReq {
            profile_dir: Some(dir.clone()),
            label: None,
            tags: vec![],
        })
        .unwrap_err();
    assert_eq!(err.status, 400);
    assert!(dir.exists());

    // A directory that is not there is not created.
    let missing = w.data.join("login-nope");
    let err = w
        .daemon
        .capture(CaptureReq {
            profile_dir: Some(missing.clone()),
            label: None,
            tags: vec![],
        })
        .unwrap_err();
    assert_eq!(err.status, 404);
    assert!(!missing.exists());
}

#[test]
fn a_session_lives_as_long_as_its_agent_not_its_launcher() {
    let w = world();
    let req = StartReq {
        cwd: "/x".into(),
        git_remote: None,
        pid: 999_999_999,
        account: Some("a@x.io".into()),
        tag: None,
    };
    // Registered and re-pointed at the agent before anything reaps it.
    let reply = w.daemon.start_session(req).unwrap();
    let sid = reply["session_id"].as_str().unwrap();
    w.daemon.set_pid(sid, std::process::id()).unwrap();
    assert_eq!(
        w.daemon.list_sessions().unwrap().as_array().unwrap().len(),
        1
    );
    w.daemon.set_pid(sid, 999_999_999).unwrap();
    assert_eq!(w.daemon.list_sessions().unwrap(), json!([]));
}

#[test]
fn the_default_login_is_kept_fresh_in_the_vault() {
    let w = world();
    Profile::default_in(&w.home)
        .write(&NoKeychain, &secret("home@x.io", "home-2"))
        .unwrap();
    w.daemon.sync_default();
    assert_eq!(vault_token(&w, "home@x.io"), "home-2");
}

#[test]
fn only_one_daemon_can_serve_a_data_directory() {
    let w = world();
    let first = http::instance_lock(&w.data).unwrap();
    assert!(http::instance_lock(&w.data).is_err());
    drop(first);
    assert!(http::instance_lock(&w.data).is_ok());
}

// ---- sessions started as plain `claude` -------------------------------------

#[test]
fn a_plain_claude_session_is_adopted_and_can_be_switched() {
    let w = world();
    w.daemon.put_policy(POLICY).unwrap();
    let payload = json!({"session_id": "agent-xyz", "cwd": "/work/api"});

    assert_eq!(w.daemon.hook("-", "SessionStart", "", &payload), json!({}));
    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions[0]["account_label"], "home@x.io");
    assert_eq!(sessions[0]["native"], true);
    assert_eq!(sessions[0]["cwd"], "/work/api");

    // The directory's rule wanted a work account; a plain session says so.
    let out = w.daemon.hook("-", "UserPromptSubmit", "", &payload);
    let message = out["systemMessage"].as_str().unwrap();
    assert!(
        message.contains("rule \"work repos\" would use"),
        "{message}"
    );
    assert!(message.contains("runs as home@x.io"), "{message}");

    // The same Claude Code session is recognised again, not adopted twice.
    w.daemon.statusline(
        "-",
        &json!({"session_id": "agent-xyz", "rate_limits": {"five_hour": {"used_percentage": 30}}}),
    );
    assert_eq!(
        w.daemon.list_sessions().unwrap().as_array().unwrap().len(),
        1
    );

    let sid = sessions[0]["id"].as_str().unwrap();
    w.daemon.swap_session(sid, "a@x.io").unwrap();
    assert_eq!(Profile::default_in(&w.home).identity().unwrap().0, "a@x.io");
    assert_eq!(vault_token(&w, "home@x.io"), "home-1");
    let out = w.daemon.hook("-", "UserPromptSubmit", "", &payload);
    assert!(out["systemMessage"]
        .as_str()
        .unwrap()
        .contains("Every plain `claude` session shares this login"));

    w.daemon.hook("-", "SessionEnd", "", &payload);
    assert_eq!(w.daemon.list_sessions().unwrap(), json!([]));
}

#[test]
fn a_plain_session_on_a_login_outside_the_pool_is_left_alone() {
    let w = world();
    Profile::default_in(&w.home)
        .write(&NoKeychain, &secret("stranger@x.io", "s-1"))
        .unwrap();
    assert_eq!(
        w.daemon.hook(
            "-",
            "SessionStart",
            "",
            &json!({"session_id": "agent-1", "cwd": "/x"})
        ),
        json!({})
    );
    assert_eq!(w.daemon.list_sessions().unwrap(), json!([]));
    // A signal with no session id at all is ignored too.
    assert_eq!(w.daemon.statusline("-", &json!({})), json!({}));
}

// ---- schedule, dead logins, names -------------------------------------------

#[test]
fn a_dead_login_is_marked_and_nothing_is_moved_onto_it() {
    let w = world();
    w.daemon
        .put_policy("[[rule]]\nname = 'dead'\non = 'auth_failed'\ndo = { swap = {} }\n")
        .unwrap();
    let reply = start(&w, "/x", Some("a@x.io"));
    let sid = reply["session_id"].as_str().unwrap();
    w.daemon
        .hook(sid, "StopFailure", "authentication_failed", &json!({}));

    let accounts = w.daemon.list_accounts().unwrap();
    let a = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == "a@x.io")
        .unwrap();
    assert_eq!(a["health"], "relogin_required");
    // The rule moved the session off the dead login.
    assert_ne!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "a@x.io"
    );
    // And it cannot be chosen again until it is captured afresh.
    let other = start(&w, "/y", None);
    let err = w
        .daemon
        .swap_session(other["session_id"].as_str().unwrap(), "a@x.io");
    assert!(err.is_err());
}

#[test]
fn a_tick_runs_scheduled_rules_only_on_their_minute() {
    let w = world();
    // A time that is never "now" by more than a day's worth of minutes.
    let minute = chrono::Local::now() + chrono::Duration::minutes(17);
    let later = minute.format("%H:%M").to_string();
    let now = chrono::Local::now().format("%H:%M").to_string();
    let reply = start(&w, "/x", Some("a@x.io"));

    w.daemon.put_policy(&format!("[[rule]]\nname = 'later'\non = {{ schedule = {{ at = '{later}' }} }}\ndo = {{ swap = {{}} }}\n")).unwrap();
    w.daemon.tick();
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "a@x.io"
    );

    w.daemon.put_policy(&format!("[[rule]]\nname = 'now'\non = {{ schedule = {{ at = '{now}' }} }}\ndo = {{ swap = {{}} }}\n")).unwrap();
    w.daemon.tick();
    assert_ne!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "a@x.io"
    );
    let _ = reply;
}

#[test]
fn a_label_cannot_take_another_accounts_name() {
    let w = world();
    let rename = |label: &str| {
        w.daemon.patch_account(
            "a@x.io",
            serde_json::from_value(json!({"label": label})).unwrap(),
        )
    };
    assert_eq!(rename("b@x.io").unwrap_err().status, 409);
    assert_eq!(rename("  ").unwrap_err().status, 409);
    rename("work-a").unwrap();
    // Renaming to its own current name, or its own email, is fine.
    rename("work-a").unwrap();
    rename("a@x.io").unwrap();
}

// ---- export and import ------------------------------------------------------

#[test]
fn the_pool_moves_between_machines_as_one_encrypted_file() {
    let w = world();
    let exported = w.daemon.export("correct horse battery").unwrap();
    let bundle = exported["bundle"].as_str().unwrap();
    assert_eq!(exported["accounts"], 3);
    assert!(bundle.starts_with("-----BEGIN AGE ENCRYPTED FILE-----"));
    for secret in ["home-1", "a-1", "b-1", "x.io"] {
        assert!(
            !bundle.contains(secret),
            "{secret} is readable in the export"
        );
    }

    let other = world();
    // The second machine already has the same three; drop one to see it return.
    other.daemon.remove_account("a@x.io").unwrap();
    let wrong = other
        .daemon
        .import(bundle, "not the passphrase", false)
        .unwrap_err();
    assert_eq!(wrong.status, 400);
    assert!(
        wrong.message.contains("wrong passphrase"),
        "{}",
        wrong.message
    );
    assert_eq!(
        other
            .daemon
            .import("hello", "correct horse battery", false)
            .unwrap_err()
            .status,
        400
    );

    let result = other
        .daemon
        .import(bundle, "correct horse battery", false)
        .unwrap();
    assert_eq!(result["added"], json!(["a@x.io"]));
    assert_eq!(result["skipped"].as_array().unwrap().len(), 2);
    assert_eq!(vault_token(&other, "a@x.io"), "a-1");
    // The restored account works.
    let reply = start(&other, "/x", Some("a@x.io"));
    assert_eq!(reply["account"]["tags"], json!(["work"]));

    // Replacing is asked for explicitly, and never under a running session.
    let result = other
        .daemon
        .import(bundle, "correct horse battery", true)
        .unwrap();
    assert_eq!(result["skipped"], json!(["a@x.io"]));
    assert_eq!(result["replaced"].as_array().unwrap().len(), 2);
}

#[test]
fn a_weak_passphrase_is_refused() {
    let w = world();
    assert_eq!(w.daemon.export("short").unwrap_err().status, 400);
}

#[test]
fn next_switches_to_the_free_account_with_the_most_room() {
    let w = world();
    let one = start(&w, "/x", Some("home@x.io"));
    let sid = one["session_id"].as_str().unwrap();
    // b is busy in another session, so `next` must not pick it.
    start(&w, "/y", Some("b@x.io"));
    let reply = w.daemon.swap_next(sid).unwrap();
    assert_eq!(reply["label"], "a@x.io");
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "a@x.io"
    );
    // Now home is free again and is the only other choice.
    assert_eq!(w.daemon.swap_next(sid).unwrap()["label"], "home@x.io");
    w.daemon.remove_account("a@x.io").unwrap();
    assert_eq!(w.daemon.swap_next(sid).unwrap_err().status, 409);
}

// ---- findings from the second review ----------------------------------------

#[test]
fn a_session_in_the_users_profile_follows_the_login_that_is_really_there() {
    let w = world();
    let payload = json!({"session_id": "agent-1", "cwd": "/x"});
    w.daemon.hook("-", "SessionStart", "", &payload);
    // The user runs /login as another pool account.
    Profile::default_in(&w.home)
        .write(&NoKeychain, &secret("a@x.io", "a-live"))
        .unwrap();

    w.daemon.statusline("-", &json!({"session_id": "agent-1", "rate_limits": {"five_hour": {"used_percentage": 95, "resets_at": 4_000_000_000i64}}}));
    let accounts = w.daemon.list_accounts().unwrap();
    let by = |email: &str| {
        accounts
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["email"] == email)
            .unwrap()
            .clone()
    };
    assert_eq!(by("a@x.io")["usage"]["windows"]["5h"]["used_percent"], 95.0);
    assert_eq!(
        by("home@x.io")["usage"],
        Value::Null,
        "the reading belongs to the login that made it"
    );

    w.daemon
        .hook("-", "StopFailure", "authentication_failed", &payload);
    let accounts = w.daemon.list_accounts().unwrap();
    let by = |email: &str| {
        accounts
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["email"] == email)
            .unwrap()
            .clone()
    };
    assert_eq!(
        by("home@x.io")["health"],
        "ok",
        "a healthy account must not be blamed for another's failure"
    );
    assert_eq!(by("a@x.io")["health"], "relogin_required");

    // Logged in to something outside the pool: the session is let go.
    Profile::default_in(&w.home)
        .write(&NoKeychain, &secret("stranger@x.io", "s-1"))
        .unwrap();
    w.daemon.statusline("-", &json!({"session_id": "agent-1"}));
    assert_eq!(w.daemon.list_sessions().unwrap(), json!([]));
}

#[test]
fn a_launched_session_with_no_account_is_one_session_and_becomes_switchable() {
    let w = world();
    let reply = start(&w, "/elsewhere", None);
    let sid = reply["session_id"].as_str().unwrap();
    // Its hooks carry the id llmpool gave it.
    w.daemon.hook(
        sid,
        "SessionStart",
        "",
        &json!({"session_id": "agent-9", "cwd": "/elsewhere"}),
    );
    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["account_label"], "home@x.io");
    w.daemon.swap_session(sid, "a@x.io").unwrap();
}

#[test]
fn a_session_that_is_only_ending_is_not_adopted() {
    let w = world();
    w.daemon.hook(
        "-",
        "SessionEnd",
        "",
        &json!({"session_id": "never-seen", "cwd": "/x"}),
    );
    assert_eq!(w.daemon.list_sessions().unwrap(), json!([]));
    assert!(!w
        .daemon
        .events()
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "adopt"));
}

#[test]
fn import_does_not_trust_the_file() {
    let w = world();
    let pass = "correct horse battery";
    let bundle = |entries: Value| {
        // Built the way a hostile file would be: valid encryption, false contents.
        let text = serde_json::to_string(&entries).unwrap();
        let recipient =
            age::Encryptor::with_user_passphrase(age::secrecy::Secret::new(pass.to_owned()));
        let mut out = Vec::new();
        let armor =
            age::armor::ArmoredWriter::wrap_output(&mut out, age::armor::Format::AsciiArmor)
                .unwrap();
        let mut writer = recipient.wrap_output(armor).unwrap();
        std::io::Write::write_all(&mut writer, text.as_bytes()).unwrap();
        writer.finish().and_then(|a| a.finish()).unwrap();
        String::from_utf8(out).unwrap()
    };
    let account = |id: &str, label: &str| {
        json!({"id": id, "provider": "claude", "kind": "oauth", "label": label, "email": "evil@x.io",
        "organization": "org", "tags": [], "enabled": true, "health": "ok", "usage": null, "login_expires_at": null})
    };
    let login = serde_json::to_string(&secret("evil@x.io", "tok-SECRET-123")).unwrap();

    // An id that is a path, an id that is another account's name, and a login that is not a login.
    for (id, stored) in [
        ("../escaped", login.as_str()),
        ("home@x.io", login.as_str()),
        ("claude-00000000", "\"tok-SECRET-123\""),
    ] {
        let err = w
            .daemon
            .import(
                &bundle(json!([{"account": account(id, "x"), "secret": stored}])),
                pass,
                true,
            )
            .unwrap_err();
        assert_eq!(err.status, 400, "{id}");
        assert!(!err.message.contains("SECRET"), "{}", err.message);
    }
    assert!(!w.data.join("escaped").exists());
    assert_eq!(
        w.daemon.list_accounts().unwrap().as_array().unwrap().len(),
        3
    );

    // An honest entry whose label collides with an existing account keeps its own email as its name.
    let real_id = {
        let other = world();
        Profile::default_in(&other.home)
            .write(&NoKeychain, &secret("evil@x.io", "tok"))
            .unwrap();
        other.daemon.capture(CaptureReq::default()).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let result = w
        .daemon
        .import(
            &bundle(json!([{"account": account(&real_id, "home@x.io"), "secret": login}])),
            pass,
            false,
        )
        .unwrap();
    assert_eq!(result["added"], json!(["evil@x.io"]));
}
