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
    assert_eq!(line["line"], format!("llmpool ● {first}  5h ▰▰▱▱▱ 40%"));
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
    assert_eq!(line["line"], "llmpool ● a@x.io  5h ▰▱▱▱▱ 12%");

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

    // A plain session is told at once which login it runs on.
    let hello = w.daemon.hook("-", "SessionStart", "", &payload);
    assert_eq!(hello["systemMessage"], "llmpool: this session runs as home@x.io (it is the login Claude Code uses). /llmpool shows usage and switches account.");
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

// ---- coming from claude-swap ------------------------------------------------

/// The Keychain as claude-swap leaves it: one item per stored account.
struct SwapKeychain(std::collections::HashMap<String, String>);

impl llmpool_adapter_claude::profile::Keychain for SwapKeychain {
    fn get(&self, _service: &str) -> std::io::Result<Option<String>> {
        Ok(None)
    }
    fn get_for(&self, service: &str, account: &str) -> std::io::Result<Option<String>> {
        Ok(self.0.get(&format!("{service}/{account}")).cloned())
    }
    fn delete(&self, _service: &str) {}
}

#[test]
fn accounts_come_over_from_claude_swap() {
    let dir = tempfile::tempdir().unwrap();
    let (home, data, swap) = (
        dir.path().join("home"),
        dir.path().join("data"),
        dir.path().join("swap"),
    );
    Profile::default_in(&home)
        .write(&NoKeychain, &secret("home@x.io", "home-1"))
        .unwrap();
    std::fs::create_dir_all(swap.join("configs")).unwrap();
    std::fs::write(
        swap.join("sequence.json"),
        r#"{"accounts": {"1": {"email": "home@x.io"}, "2": {"email": "two@x.io"}, "3": {"email": "gone@x.io"}}}"#,
    )
    .unwrap();
    let mut items = std::collections::HashMap::new();
    for (slot, email) in [("1", "home@x.io"), ("2", "two@x.io")] {
        let s = secret(email, &format!("{email}-token"));
        std::fs::write(
            swap.join("configs")
                .join(format!(".claude-config-{slot}-{email}.json")),
            json!({"oauthAccount": s.account, "projects": {}}).to_string(),
        )
        .unwrap();
        items.insert(
            format!("claude-swap/account-{slot}-{email}"),
            json!({"claudeAiOauth": s.oauth, "mcpOAuth": {}}).to_string(),
        );
    }
    let daemon = Daemon::open(
        &data,
        &home,
        Box::new(FileVault(data.join("vault"))),
        Box::new(SwapKeychain(items)),
    )
    .unwrap();

    let result = daemon.import_claude_swap(&swap).unwrap();
    assert_eq!(result["added"], json!(["two@x.io"]));
    let why = |email: &str| {
        result["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["account"] == email)
            .unwrap()["why"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(
        why("home@x.io").contains("--from-current"),
        "the live login is newer than claude-swap's copy"
    );
    assert!(why("gone@x.io").contains("could not be read"));

    // The imported account starts a session with the login claude-swap held.
    let req = StartReq {
        cwd: "/x".into(),
        git_remote: None,
        pid: std::process::id(),
        account: Some("two@x.io".into()),
        tag: None,
    };
    let reply = daemon.start_session(req).unwrap();
    let profile = Profile::at(&PathBuf::from(
        reply["env"]["CLAUDE_CONFIG_DIR"].as_str().unwrap(),
    ));
    assert_eq!(
        profile.read(&NoKeychain).unwrap().unwrap().oauth["accessToken"],
        "two@x.io-token"
    );

    // A second run adds nothing.
    assert_eq!(
        daemon.import_claude_swap(&swap).unwrap()["added"],
        json!([])
    );
    assert_eq!(
        daemon
            .import_claude_swap(&dir.path().join("nowhere"))
            .unwrap_err()
            .status,
        404
    );
}

#[test]
fn repair_rewrites_the_same_login_and_keeps_the_newest_token() {
    let w = world();
    let payload = json!({"session_id": "agent-1", "cwd": "/x"});
    w.daemon.hook("-", "SessionStart", "", &payload);
    let sid = w.daemon.list_sessions().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let default = Profile::default_in(&w.home);
    // Claude Code rotated the token and keeps other logins beside it.
    default
        .write(&NoKeychain, &secret("home@x.io", "home-rotated"))
        .unwrap();
    let path = default.config_dir.join(".credentials.json");
    let mut raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["mcpOAuth"] = json!({"server": "kept"});
    std::fs::write(&path, raw.to_string()).unwrap();

    let reply = w.daemon.reseat(&sid).unwrap();
    assert_eq!(reply["label"], "home@x.io");
    assert_eq!(default_token(&w), "home-rotated");
    assert_eq!(vault_token(&w, "home@x.io"), "home-rotated");
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["mcpOAuth"]["server"], "kept");
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "home@x.io"
    );
}

#[test]
fn quiet_signals_do_not_rewrite_the_state_file_or_repeat_advice() {
    let w = world();
    w.daemon.put_policy(POLICY).unwrap();
    let payload = json!({"session_id": "agent-1", "cwd": "/work/api", "rate_limits": {"five_hour": {"used_percentage": 30, "resets_at": 4_000_000_000i64}}});
    w.daemon.hook("-", "SessionStart", "", &payload);
    w.daemon.statusline("-", &payload);
    let state = w.data.join("state.json");
    let written = std::fs::metadata(&state).unwrap().modified().unwrap();
    // Long enough to cross into another second: time passing is not a change.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    for _ in 0..5 {
        w.daemon.statusline("-", &payload);
    }
    assert_eq!(
        std::fs::metadata(&state).unwrap().modified().unwrap(),
        written,
        "nothing changed, nothing should be written"
    );

    // The advice about this directory's rule was given; meeting the same
    // Claude Code session again later does not repeat it.
    let first = w.daemon.hook("-", "UserPromptSubmit", "", &payload);
    assert!(first["systemMessage"]
        .as_str()
        .unwrap()
        .contains("would use"));
    w.daemon.hook("-", "SessionEnd", "", &payload);
    w.daemon.hook("-", "SessionStart", "", &payload);
    assert_eq!(
        w.daemon.hook("-", "UserPromptSubmit", "", &payload),
        json!({})
    );
}

// ---- signing in, what is missing, renewal -----------------------------------

/// A stand-in `claude` whose `auth login` behaves like the real one: prints
/// where to sign in, then leaves a login in the profile it was pointed at.
fn stub_claude(w: &World) {
    let script = w.data.join("claude-stub");
    std::fs::write(
        &script,
        r#"#!/bin/sh
[ "$1 $2" = "auth login" ] || exit 2
email="${4:-new@x.io}"
echo "Opening https://claude.example/login?state=abc in your browser"
[ "$email" = "refused@x.io" ] && exit 1
[ "$email" = "slow@x.io" ] && sleep 30
# A browser already signed in elsewhere returns that account, whatever was asked.
[ "$email" = "wanted@x.io" ] && email="a@x.io"
[ "$email" = "wanted-too@x.io" ] && email="surprise@x.io"
if [ "$email" = "code@x.io" ]; then printf 'Paste code here if prompted > '; read code; [ "$code" = "c0de#st4te" ] || exit 1; fi
printf '{"claudeAiOauth":{"accessToken":"tok-%s","refreshToken":"r"}}' "$email" > "$CLAUDE_CONFIG_DIR/.credentials.json"
printf '{"oauthAccount":{"emailAddress":"%s","organizationUuid":"org"}}' "$email" > "$CLAUDE_CONFIG_DIR/.claude.json"
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Where the installer records the program it found.
    std::fs::write(w.data.join("claude-bin"), script.to_str().unwrap()).unwrap();
}

fn wait_for_login(w: &World) -> Value {
    for _ in 0..100 {
        let status = w.daemon.login_status();
        if status["state"] != "waiting" {
            return status;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the sign-in never finished");
}

/// The sign-in page's address, once Claude Code has printed it.
fn wait_for_url(w: &World) -> String {
    for _ in 0..100 {
        if let Some(url) = w.daemon.login_status()["url"].as_str() {
            return url.to_string();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("no sign-in address was offered");
}

fn login(email: &str, tags: &[&str]) -> llmpool_daemon::LoginReq {
    serde_json::from_value(json!({"email": email, "tags": tags})).unwrap()
}

#[test]
fn signing_in_through_claude_code_adds_the_account() {
    let w = world();
    stub_claude(&w);
    let started = w.daemon.start_login(login("new@x.io", &["work"])).unwrap();
    assert_eq!(started["state"], "waiting");
    let done = wait_for_login(&w);
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["account"], "new@x.io");
    assert_eq!(vault_token(&w, "new@x.io"), "tok-new@x.io");
    let accounts = w.daemon.list_accounts().unwrap();
    let new = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == "new@x.io")
        .unwrap();
    assert_eq!(new["tags"], json!(["work"]));
    // Nothing of the scratch profile is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&w.data)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("login-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_sign_in_that_is_refused_or_cancelled_adds_nothing_and_says_so() {
    let w = world();
    stub_claude(&w);
    w.daemon.start_login(login("refused@x.io", &[])).unwrap();
    let failed = wait_for_login(&w);
    assert_eq!(failed["state"], "failed");
    assert!(failed["message"]
        .as_str()
        .unwrap()
        .contains("not completed"));

    w.daemon.start_login(login("slow@x.io", &[])).unwrap();
    // The page's address is offered in case the browser did not open.
    assert!(wait_for_url(&w).starts_with("https://claude.example/login"));
    assert_eq!(
        w.daemon
            .start_login(login("new@x.io", &[]))
            .unwrap_err()
            .status,
        409,
        "one sign-in at a time"
    );
    assert_eq!(w.daemon.cancel_login()["state"], "cancelled");
    assert_eq!(
        w.daemon.list_accounts().unwrap().as_array().unwrap().len(),
        3
    );

    // Without Claude Code there is nothing to sign in with.
    std::fs::write(w.data.join("claude-bin"), "/nowhere/claude").unwrap();
    if w.daemon.claude_bin().is_none() {
        assert_eq!(
            w.daemon
                .start_login(login("new@x.io", &[]))
                .unwrap_err()
                .status,
            409
        );
    }
}

#[test]
fn what_is_missing_is_named_with_its_fix() {
    let w = world();
    let kinds = |w: &World| -> Vec<String> {
        w.daemon
            .issues()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["kind"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(kinds(&w).contains(&"hooks_missing".to_string()));
    std::fs::write(w.home.join(".claude/settings.json"), r#"{"hooks": {"SessionStart": [{"hooks": [{"command": "'/x/llmpool' hook claude SessionStart"}]}]}}"#).unwrap();
    assert!(!kinds(&w).contains(&"hooks_missing".to_string()));
    assert!(!kinds(&w).contains(&"login_not_in_pool".to_string()));

    // The user logs Claude Code in to an account the pool does not have.
    Profile::default_in(&w.home)
        .write(&NoKeychain, &secret("stranger@x.io", "s"))
        .unwrap();
    let issues = w.daemon.issues().unwrap();
    let missing = issues
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "login_not_in_pool")
        .unwrap();
    assert_eq!(missing["action"], "add_current");
    assert!(missing["text"].as_str().unwrap().contains("stranger@x.io"));

    // A login that has died.
    let reply = start(&w, "/x", Some("a@x.io"));
    w.daemon.hook(
        reply["session_id"].as_str().unwrap(),
        "StopFailure",
        "authentication_failed",
        &json!({}),
    );
    let issues = w.daemon.issues().unwrap();
    let dead = issues
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "relogin")
        .unwrap();
    assert_eq!(
        (dead["action"].as_str(), dead["email"].as_str()),
        (Some("login"), Some("a@x.io"))
    );
}

/// A stand-in token endpoint; returns its address and how many times it was asked.
fn token_endpoint(
    status: u16,
    body: &'static str,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/token",
        server.server_addr().to_ip().unwrap().port()
    );
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = asked.clone();
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ =
                request.respond(tiny_http::Response::from_string(body).with_status_code(status));
        }
    });
    (url, asked)
}

/// A daemon that renews against `url`, holding one idle account whose short
/// token ran out long ago, and the default login.
fn renewing_world(url: &str) -> (tempfile::TempDir, Daemon, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (home, data) = (dir.path().join("home"), dir.path().join("data"));
    Profile::default_in(&home)
        .write(&NoKeychain, &secret("home@x.io", "home-1"))
        .unwrap();
    let mut daemon = Daemon::open(
        &data,
        &home,
        Box::new(FileVault(data.join("vault"))),
        Box::new(NoKeychain),
    )
    .unwrap();
    daemon.set_token_url(Some(url.to_string()));
    daemon.capture(CaptureReq::default()).unwrap();
    let scratch = data.join("login-idle");
    let mut idle = secret("idle@x.io", "old");
    idle.oauth["expiresAt"] = json!(1000);
    Profile::at(&scratch).write(&NoKeychain, &idle).unwrap();
    daemon
        .capture(CaptureReq {
            profile_dir: Some(scratch),
            label: None,
            tags: vec![],
        })
        .unwrap();
    (dir, daemon, data)
}

fn stored(data: &Path, daemon: &Daemon, email: &str) -> Secret {
    let accounts = daemon.list_accounts().unwrap();
    let id = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == email)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    serde_json::from_str(&FileVault(data.join("vault")).get(&id).unwrap().unwrap()).unwrap()
}

#[test]
fn an_idle_login_is_renewed_and_one_in_use_is_left_to_claude_code() {
    let (url, asked) = token_endpoint(
        200,
        r#"{"access_token": "fresh", "refresh_token": "r-fresh", "expires_in": 3600}"#,
    );
    let (_dir, daemon, data) = renewing_world(&url);
    daemon.renew_idle();
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    let renewed = stored(&data, &daemon, "idle@x.io");
    assert_eq!(renewed.oauth["accessToken"], "fresh");
    assert_eq!(renewed.oauth["refreshToken"], "r-fresh");
    assert!(daemon
        .events()
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "renew"));

    // Fresh now, so nothing more is asked; and the default login, which Claude
    // Code holds, was never touched.
    daemon.renew_idle();
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        stored(&data, &daemon, "home@x.io").oauth["accessToken"],
        "home-1"
    );
}

#[test]
fn a_login_in_a_running_session_is_not_renewed_behind_its_back() {
    let (url, asked) = token_endpoint(200, r#"{"access_token": "fresh", "expires_in": 3600}"#);
    let (_dir, daemon, _data) = renewing_world(&url);
    let req = StartReq {
        cwd: "/x".into(),
        git_remote: None,
        pid: std::process::id(),
        account: Some("idle@x.io".into()),
        tag: None,
    };
    daemon.start_session(req).unwrap();
    daemon.renew_idle();
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn a_login_the_provider_has_ended_is_marked_and_a_shaky_answer_is_not() {
    let (url, _) = token_endpoint(400, r#"{"error": "invalid_grant"}"#);
    let (_dir, daemon, _data) = renewing_world(&url);
    daemon.renew_idle();
    let health = |d: &Daemon| {
        d.list_accounts()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["email"] == "idle@x.io")
            .unwrap()["health"]
            .clone()
    };
    assert_eq!(health(&daemon), "relogin_required");

    let (url, asked) = token_endpoint(503, "busy");
    let (_dir, daemon, data) = renewing_world(&url);
    daemon.renew_idle();
    assert_eq!(health(&daemon), "ok");
    assert_eq!(
        stored(&data, &daemon, "idle@x.io").oauth["accessToken"],
        "old"
    );
    // And it is not hammered: the next attempt waits.
    daemon.renew_idle();
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_sign_in_that_asks_for_a_code_can_be_given_one() {
    let w = world();
    stub_claude(&w);
    assert_eq!(
        w.daemon.submit_login_code("x").unwrap_err().status,
        409,
        "nothing is waiting yet"
    );
    w.daemon.start_login(login("code@x.io", &[])).unwrap();
    // The address offered must be clean enough to open.
    let url = wait_for_url(&w);
    assert_eq!(w.daemon.login_status()["state"], "waiting");
    assert_eq!(url, "https://claude.example/login?state=abc");
    assert_eq!(
        w.daemon.submit_login_code("two words").unwrap_err().status,
        400
    );
    w.daemon.submit_login_code("  c0de#st4te \n").unwrap();
    let done = wait_for_login(&w);
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["account"], "code@x.io");
}

#[test]
fn signing_in_as_the_wrong_account_is_said_plainly() {
    let w = world();
    stub_claude(&w);
    // Asked for one account; the browser was signed in to another the pool already has.
    w.daemon.start_login(login("wanted@x.io", &[])).unwrap();
    let status = wait_for_login(&w);
    assert_eq!(status["state"], "failed", "{status}");
    let message = status["message"].as_str().unwrap();
    assert!(
        message.contains("signed in as a@x.io, not wanted@x.io"),
        "{message}"
    );
    assert!(message.contains("private window"), "{message}");
    assert_eq!(
        vault_token(&w, "a@x.io"),
        "a-1",
        "the account already here is left as it was"
    );
    assert_eq!(
        w.daemon.list_accounts().unwrap().as_array().unwrap().len(),
        3
    );

    // Asked for one account; got a different, new one: it is added, and named.
    w.daemon.start_login(login("wanted-too@x.io", &[])).unwrap();
    let status = wait_for_login(&w);
    assert_eq!(status["state"], "done", "{status}");
    assert_eq!(
        status["message"],
        "Signed in as surprise@x.io, not wanted-too@x.io as asked; surprise@x.io is in the pool."
    );
}

#[test]
fn usage_is_remembered_over_time_and_the_pool_is_summed_up() {
    let w = world();
    w.daemon.put_policy(POLICY).unwrap();
    let reply = start(&w, "/work/api", None);
    let sid = reply["session_id"].as_str().unwrap();
    let first = reply["account"]["email"].as_str().unwrap();
    let reading = |p: f64| json!({"rate_limits": {"five_hour": {"used_percentage": p, "resets_at": 4_000_000_000i64}, "seven_day": {"used_percentage": p / 2.0, "resets_at": 4_000_000_000i64}}});

    // Small moves within ten minutes are one point; a jump is another.
    for p in [10.0, 11.0, 12.0, 40.0, 41.0, 72.0] {
        w.daemon.statusline(sid, &reading(p));
    }
    let accounts = w.daemon.list_accounts().unwrap();
    let row = accounts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == first)
        .unwrap();
    let points: Vec<f64> = row["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p[1].as_f64().unwrap())
        .collect();
    assert_eq!(points, vec![10.0, 40.0, 72.0]);

    // The line under the prompt says it three ways: mark, meter, number.
    let line = w.daemon.statusline(sid, &reading(72.0));
    assert_eq!(
        line["line"],
        format!("llmpool ◆ {first}  5h ▰▰▰▰▱ 72%  7d ▰▰▱▱▱ 36%")
    );
    assert!(line["colored"].as_str().unwrap().contains("\u{1b}[33m"));

    let stats = w.daemon.stats().unwrap();
    assert_eq!(
        (
            stats["accounts"].as_u64(),
            stats["usable"].as_u64(),
            stats["sessions"].as_u64()
        ),
        (Some(3), Some(3), Some(1))
    );
    // One account at 72%, two untouched: on average 76% of the 5-hour allowance is left.
    assert_eq!(stats["headroom_5h"].as_f64().unwrap().round(), 76.0);
    assert_eq!(stats["switches_24h"], 0);
    assert_eq!(stats["last_switch"], Value::Null);

    w.daemon.statusline(sid, &reading(95.0));
    let stats = w.daemon.stats().unwrap();
    assert_eq!(stats["switches_24h"], 1);
    assert!(stats["last_switch"]["text"]
        .as_str()
        .unwrap()
        .contains("swap before the limit"));
}

// ---- knowing what you got, spreading load, pressure ---------------------------

#[test]
fn a_session_is_told_once_which_account_it_got_and_why() {
    let w = world();
    w.daemon.put_policy(POLICY).unwrap();
    let reply = start(&w, "/work/api", None);
    let sid = reply["session_id"].as_str().unwrap();
    let got = reply["account"]["email"].as_str().unwrap();
    let hello = w.daemon.hook(
        sid,
        "SessionStart",
        "",
        &json!({"session_id": "agent-1", "source": "startup"}),
    );
    assert_eq!(
        hello["systemMessage"],
        format!("llmpool: this session runs as {got} (rule \"work repos\"). /llmpool shows usage and switches account.")
    );
    // Not again on resume or /clear, which raise the same hook.
    assert_eq!(
        w.daemon.hook(
            sid,
            "SessionStart",
            "",
            &json!({"session_id": "agent-1", "source": "resume"})
        ),
        json!({})
    );
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["picked_by"],
        "rule \"work repos\""
    );

    let chosen = start(&w, "/x", Some("home@x.io"));
    let hello = w.daemon.hook(
        chosen["session_id"].as_str().unwrap(),
        "SessionStart",
        "",
        &json!({"session_id": "agent-2"}),
    );
    assert!(hello["systemMessage"]
        .as_str()
        .unwrap()
        .contains("home@x.io (you chose it)"));

    // A session llmpool left on the user's own login, outside the pool, is told nothing.
    Profile::default_in(&w.home)
        .write(&NoKeychain, &secret("stranger@x.io", "s"))
        .unwrap();
    let plain = start(&w, "/elsewhere", None);
    assert_eq!(
        w.daemon.hook(
            plain["session_id"].as_str().unwrap(),
            "SessionStart",
            "",
            &json!({"session_id": "agent-3"})
        ),
        json!({})
    );
}

#[test]
fn least_busy_gives_each_new_session_the_emptiest_account() {
    let w = world();
    w.daemon
        .put_policy("[[rule]]\nname = 'spread'\non = 'session_start'\ndo = { assign = { tag = 'work', pick = 'least_busy' } }\n")
        .unwrap();
    let first = start(&w, "/one", None);
    let second = start(&w, "/two", None);
    let third = start(&w, "/three", None);
    let on = |reply: &Value| reply["account"]["email"].as_str().unwrap().to_string();
    // Two work accounts: the second session goes to the other one, the third doubles up.
    assert_ne!(on(&first), on(&second));
    assert_eq!(
        on(&third),
        on(&first),
        "ties go to the lower id, which the first session took"
    );
    // Sessions on one account share its profile.
    assert_eq!(
        first["env"]["CLAUDE_CONFIG_DIR"],
        third["env"]["CLAUDE_CONFIG_DIR"]
    );
}

#[test]
fn pressure_on_a_shared_account_moves_its_sessions_before_the_window_is_spent() {
    let w = world();
    w.daemon
        .put_policy(
            "[settings]\ncooldown_secs = 0\n\n[[rule]]\nname = 'relieve'\non = { pressure = { full_within_minutes = 30, min_sessions = 2 } }\ndo = { swap = { pick = 'least_busy' }, notify = true }\n",
        )
        .unwrap();
    let one = start(&w, "/x", Some("a@x.io"));
    let sid = one["session_id"].as_str().unwrap();
    // Half of the 5-hour window is gone and 90% is used: full in about a quarter of an hour.
    let now = chrono::Utc::now().timestamp();
    let hot =
        json!({"rate_limits": {"five_hour": {"used_percentage": 90, "resets_at": now + 9000}}});

    // One session alone on the account: the rule asks for two.
    w.daemon.statusline(sid, &hot);
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "a@x.io"
    );

    // A second session joins the same account; now it is pressure.
    let two = start(&w, "/y", Some("a@x.io"));
    w.daemon.statusline(sid, &hot);
    let sessions = w.daemon.list_sessions().unwrap();
    let labels: Vec<&str> = sessions
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["account_label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        vec!["b@x.io", "b@x.io"],
        "they share one login, so they move together"
    );
    // Both are told, and why.
    for id in [sid, two["session_id"].as_str().unwrap()] {
        let out = w.daemon.hook(id, "UserPromptSubmit", "", &json!({}));
        assert!(out["systemMessage"]
            .as_str()
            .unwrap()
            .contains("moved from a@x.io to b@x.io (rule \"relieve\")"));
    }
    assert_eq!(sessions[0]["picked_by"], "rule \"relieve\"");
}

// ---- the decision layer: own account first, and the advisor ---------------------

type Asked = Arc<std::sync::Mutex<Vec<(String, Value)>>>;

/// A stand-in for the Jev service: answers every run with `answers` and keeps
/// what it was sent (the Authorization header and the JSON body).
fn jev(status: u16, answers: Value) -> (String, Asked) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}",
        server.server_addr().to_ip().unwrap().port()
    );
    let asked: Asked = Arc::default();
    let seen = asked.clone();
    std::thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let auth = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            seen.lock()
                .unwrap()
                .push((auth, serde_json::from_str(&body).unwrap_or_default()));
            let reply = if status == 200 {
                json!({"code": 0, "message": "ok", "data": {"id": "run-1", "status": "completed", "result": {"model": "typesafe/jev-1.13", "answers": answers}}})
            } else {
                json!({"code": -1, "message": "insufficient_tokens"})
            };
            let _ = request.respond(
                tiny_http::Response::from_string(reply.to_string()).with_status_code(status),
            );
        }
    });
    (url, asked)
}

fn advisor_policy(url: &str, extra: &str) -> String {
    format!("[settings]\ncooldown_secs = 0\n\n[advisor]\nenabled = true\nurl = \"{url}\"\nswitch_above = 0.7\n\n{extra}")
}

#[test]
fn the_advisor_moves_a_session_only_when_sure_and_never_sees_who_anyone_is() {
    let (url, asked) = jev(
        200,
        json!({"switch": {"type": "noul", "noul": 0.91}, "target": {"type": "choice", "choice": "A"}, "urgency": {"type": "score", "score": 1.6}}),
    );
    let w = world();
    w.daemon.put_policy(&advisor_policy(&url, "")).unwrap();
    w.daemon.set_advisor_key(Some("sk_test_key")).unwrap();
    let reply = start(&w, "/x", Some("a@x.io"));
    let sid = reply["session_id"].as_str().unwrap();
    w.daemon.statusline(sid, &usage(64.0));

    w.daemon.advise_due();
    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions[0]["account_label"], "b@x.io");
    assert_eq!(sessions[0]["picked_by"], "advisor, 91% sure");
    assert_eq!(sessions[0]["advice"]["acted"], true);
    assert_eq!(sessions[0]["advice"]["target"], "b@x.io");

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "Bearer sk_test_key");
    let body = &asked[0].1;
    assert_eq!(body["model"], "typesafe/jev-1.13");
    assert_eq!(body["requestId"].as_str().unwrap().len(), 36);
    assert_eq!(body["questions"]["switch"]["type"], "noul");
    assert!(body["questions"]["target"]["criteria"]
        .get("stay")
        .is_some());
    assert_eq!(
        body["state"]["current_account"]["five_hour_window"]["used_percent"],
        64.0
    );
    // Accounts go by letter: no address, no id, no directory leaves the machine.
    let sent = body.to_string();
    for private in ["x.io", "claude-", "/x", "sk_test"] {
        assert!(!sent.contains(private), "{private} was sent to the advisor");
    }
    // The key is never part of what llmpool shows either.
    assert!(!w
        .daemon
        .advisor_status()
        .unwrap()
        .to_string()
        .contains("sk_test"));
}

#[test]
fn an_unsure_or_stay_answer_changes_nothing_and_is_not_asked_again_at_once() {
    for answers in [
        json!({"switch": {"noul": 0.4}, "target": {"choice": "A"}, "urgency": {"score": 0.2}}),
        json!({"switch": {"noul": 0.95}, "target": {"choice": "stay"}, "urgency": {"score": 0.0}}),
        json!({"switch": {"noul": 0.95}, "target": {"choice": "Z"}}),
    ] {
        let (url, asked) = jev(200, answers);
        let w = world();
        w.daemon.put_policy(&advisor_policy(&url, "")).unwrap();
        w.daemon.set_advisor_key(Some("k")).unwrap();
        let reply = start(&w, "/x", Some("a@x.io"));
        let sid = reply["session_id"].as_str().unwrap();
        w.daemon.statusline(sid, &usage(64.0));
        w.daemon.advise_due();
        w.daemon.advise_due();
        let sessions = w.daemon.list_sessions().unwrap();
        assert_eq!(sessions[0]["account_label"], "a@x.io");
        assert_eq!(sessions[0]["advice"]["acted"], false);
        assert_eq!(asked.lock().unwrap().len(), 1, "one question per interval");
    }
}

#[test]
fn the_advisor_is_silent_when_off_keyless_or_nothing_is_near_a_limit_and_failing_is_harmless() {
    let (url, asked) = jev(402, json!({}));
    let w = world();
    let reply = start(&w, "/x", Some("a@x.io"));
    let sid = reply["session_id"].as_str().unwrap();

    // Off.
    w.daemon
        .put_policy(&format!("[advisor]\nenabled = false\nurl = \"{url}\"\n"))
        .unwrap();
    w.daemon.set_advisor_key(Some("k")).unwrap();
    w.daemon.statusline(sid, &usage(64.0));
    w.daemon.advise_due();
    // On, but far from any limit.
    w.daemon.put_policy(&advisor_policy(&url, "")).unwrap();
    w.daemon.statusline(sid, &usage(10.0));
    w.daemon.advise_due();
    // On, near a limit, but no key.
    w.daemon.set_advisor_key(None).unwrap();
    w.daemon.statusline(sid, &usage(64.0));
    w.daemon.advise_due();
    assert_eq!(asked.lock().unwrap().len(), 0);

    // With a key the service says the account is out of tokens: noted, nothing moves,
    // and the ordinary rules still work.
    w.daemon.set_advisor_key(Some("k")).unwrap();
    w.daemon.advise_due();
    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions[0]["account_label"], "a@x.io");
    assert_eq!(
        sessions[0]["advice"]["error"],
        "the advisor account has no tokens left"
    );
    w.daemon
        .put_policy(&advisor_policy(
            &url,
            "[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = {} }\n",
        ))
        .unwrap();
    w.daemon.hook(sid, "StopFailure", "rate_limit", &json!({}));
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "b@x.io"
    );
}

#[test]
fn a_new_session_starts_where_the_advisor_says_within_what_the_rule_allows() {
    let (url, asked) = jev(200, json!({"start_on": {"type": "choice", "choice": "B"}}));
    let w = world();
    w.daemon.set_advisor_key(Some("k")).unwrap();
    w.daemon
        .put_policy(&advisor_policy(&url, "[[rule]]\nname = 'ask the advisor'\non = 'session_start'\ndo = { assign = { tag = 'work', pick = 'advisor' } }\n"))
        .unwrap();
    let reply = start(&w, "/x", None);
    // Two work accounts, lettered in the order the rule lists them; B is the second.
    let asked_body = asked.lock().unwrap()[0].1.clone();
    assert_eq!(
        asked_body["state"]["accounts"].as_object().unwrap().len(),
        2
    );
    assert_eq!(asked_body["questions"]["start_on"]["type"], "choice");
    let picked = reply["account"]["email"].as_str().unwrap();
    assert!(picked == "a@x.io" || picked == "b@x.io");
    let hello = w.daemon.hook(
        reply["session_id"].as_str().unwrap(),
        "SessionStart",
        "",
        &json!({"session_id": "agent-1"}),
    );
    assert!(hello["systemMessage"]
        .as_str()
        .unwrap()
        .contains("(the advisor chose it, under rule \"ask the advisor\")"));

    // An answer outside what the rule allows is ignored; the rule's own choice stands.
    let (url, _) = jev(200, json!({"start_on": {"choice": "Q"}}));
    let w = world();
    w.daemon.set_advisor_key(Some("k")).unwrap();
    w.daemon
        .put_policy(&advisor_policy(&url, "[[rule]]\nname = 'r'\non = 'session_start'\ndo = { assign = { tag = 'work', pick = 'advisor' } }\n"))
        .unwrap();
    let reply = start(&w, "/x", None);
    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions[0]["picked_by"], "rule \"r\"");
    assert!(reply["account"]["tags"] == json!(["work"]));
}

#[test]
fn the_users_own_account_comes_first_and_a_session_comes_home_when_it_has_room_again() {
    let w = world();
    w.daemon
        .patch_account(
            "a@x.io",
            serde_json::from_value(json!({"tags": ["own", "work"]})).unwrap(),
        )
        .unwrap();
    w.daemon
        .put_policy(
            "[settings]\ncooldown_secs = 0\n\n[[rule]]\nname = 'any work account'\non = 'session_start'\ndo = { assign = { tag = 'work' } }\n\n\
             [[rule]]\nname = 'leave when nearly full'\non = { usage_above = { percent = 90 } }\ndo = { swap = {} }\n\n\
             [[rule]]\nname = 'come home'\non = { own_available = { below = 50 } }\ndo = { swap = { tag = 'own' } }\n",
        )
        .unwrap();
    // Own account first, although the other work account is just as empty.
    let reply = start(&w, "/x", None);
    let sid = reply["session_id"].as_str().unwrap();
    assert_eq!(reply["account"]["email"], "a@x.io");

    // Nearly spent: the session leaves for the pool.
    let soon = chrono::Utc::now().timestamp() + 1;
    w.daemon.statusline(
        sid,
        &json!({"rate_limits": {"five_hour": {"used_percentage": 95, "resets_at": soon}}}),
    );
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "b@x.io"
    );

    // While the own account is still full, a tick changes nothing.
    w.daemon.tick();
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["account_label"],
        "b@x.io"
    );

    // Its window resets; the next tick brings the session home.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    w.daemon.tick();
    let sessions = w.daemon.list_sessions().unwrap();
    assert_eq!(sessions[0]["account_label"], "a@x.io");
    assert_eq!(sessions[0]["picked_by"], "rule \"come home\"");
}

#[test]
fn settings_change_from_any_surface_without_touching_the_rest_of_the_file() {
    let w = world();
    let text =
        "# my notes\n[[rule]]\nname = 'rl' # keep me\non = 'rate_limited'\ndo = { swap = {} }\n";
    w.daemon.put_policy(text).unwrap();
    let out = w
        .daemon
        .configure(&json!({"advisor_enabled": true, "switch_above": 0.8}))
        .unwrap();
    assert_eq!(
        (
            out["advisor_enabled"].as_bool(),
            out["switch_above"].as_f64()
        ),
        (Some(true), Some(0.8))
    );
    w.daemon.configure(&json!({"prefer_own": false})).unwrap();
    let saved = w.daemon.policy_text()["text"].as_str().unwrap().to_string();
    assert!(saved.starts_with("# my notes\n"), "{saved}");
    assert!(saved.contains("name = 'rl' # keep me"), "{saved}");
    assert!(saved.contains("[advisor]\nenabled = true"), "{saved}");
    assert!(saved.contains("prefer_own = false"), "{saved}");
    assert_eq!(w.daemon.advisor_status().unwrap()["enabled"], true);
    // A value that makes no sense is refused and nothing is written.
    assert_eq!(
        w.daemon
            .configure(&json!({"switch_above": 7}))
            .unwrap_err()
            .status,
        400
    );
    assert_eq!(w.daemon.policy_text()["text"].as_str().unwrap(), saved);
}

#[test]
fn the_advisors_two_views_of_switching_are_read_together() {
    // What the real service answered for an account at 91% with a roomy alternative:
    // 64% "yes", but only 19% for staying. Together that is 72.5%, enough at 70%.
    let real = json!({
        "switch": {"type": "noul", "noul": 0.64},
        "target": {"type": "choice", "choice": "A", "probabilities": {"A": 0.77, "B": 0.04, "stay": 0.19}},
        "urgency": {"type": "score", "score": 1.12, "probabilities": {"0": 0.07, "1": 0.74, "2": 0.19}}
    });
    let (url, _) = jev(200, real);
    let w = world();
    w.daemon.put_policy(&advisor_policy(&url, "")).unwrap();
    w.daemon.set_advisor_key(Some("k")).unwrap();
    let reply = start(&w, "/x", Some("a@x.io"));
    let sid = reply["session_id"].as_str().unwrap();
    w.daemon.statusline(sid, &usage(91.0));
    let advice = w.daemon.advise_session(sid).unwrap();
    assert_eq!((advice["switch"].as_f64().unwrap() * 1000.0).round(), 725.0);
    assert_eq!(advice["acted"], true);
    assert_eq!(
        w.daemon.list_sessions().unwrap()[0]["picked_by"],
        "advisor, 72% sure"
    );
}
