use crate::{AddAgent, Cli, Command, InstallAgent, PolicyCommand, UninstallAgent};
use llmpool_adapter_claude::install;
use llmpool_core::policy::engine::Scenario;
use llmpool_core::policy::file;
use llmpool_daemon::client::{data_dir, home, Client};
use llmpool_daemon::{http, Daemon, NATIVE};
use serde_json::{json, Value};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::Duration;

const SHIM_BEGIN: &str = "# >>> llmpool >>>";
const SHIM_END: &str = "# <<< llmpool <<<";

const TERMS_NOTICE: &str = "\
note: subscription logins are licensed per person by their provider. Use
llmpool with your own accounts, and check the provider's terms before
sharing a pool with other people.";

/// A client for the daemon, starting it in the background if needed.
/// Start the daemon in the background, without waiting for it. In a process
/// group of its own, so that closing the terminal that happened to start it
/// does not take it down with every other session's bookkeeping.
fn spawn_daemon() -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let data = data_dir();
    std::fs::create_dir_all(&data).map_err(|e| format!("{}: {e}", data.display()))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("daemon.log"))
        .map_err(|e| format!("daemon log: {e}"))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not start the daemon: {e}"))?;
    Ok(())
}

fn connect() -> Result<Client, String> {
    let timeout = Duration::from_secs(15);
    if let Some(client) = Client::existing(timeout) {
        return Ok(client);
    }
    let start = std::time::Instant::now();
    let mut last_spawn = None::<std::time::Instant>;
    while start.elapsed() < Duration::from_secs(10) {
        // Spawned again if nothing came up: a daemon that was just stopped may
        // still have held the instance lock when the first one tried. The
        // lock makes extra spawns harmless.
        if last_spawn.is_none_or(|at| at.elapsed() > Duration::from_secs(2)) {
            spawn_daemon()?;
            last_spawn = Some(std::time::Instant::now());
        }
        std::thread::sleep(Duration::from_millis(50));
        if let Some(client) = Client::existing(timeout) {
            return Ok(client);
        }
    }
    Err(format!(
        "the daemon did not start; see {}",
        data_dir().join("daemon.log").display()
    ))
}

pub fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Command::Add {
            agent:
                AddAgent::Claude {
                    from_current,
                    label,
                    tags,
                    email,
                    detach,
                    cancel,
                    interactive,
                    code,
                },
        } => add_claude(AddOptions {
            from_current,
            label,
            tags,
            email,
            detach,
            cancel,
            interactive,
            code,
        }),
        Command::Login { account } => login_again(&account),
        Command::Doctor => doctor(),
        Command::List { json } => list(json),
        Command::Tag { account, tags } => retag(&account, |set| set.extend(tags.iter().cloned())),
        Command::Untag { account, tags } => {
            retag(&account, |set| set.retain(|t| !tags.contains(t)))
        }
        Command::Enable { account } => patch(&account, json!({"enabled": true})),
        Command::Disable { account } => patch(&account, json!({"enabled": false})),
        Command::Remove { account } => {
            connect()?.call("DELETE", &format!("/accounts/{}", encode(&account)), None)?;
            done(format!("removed {account}"))
        }
        Command::Run { account, tag, args } => run_agent(account, tag, args),
        Command::Sessions { json } => sessions(json),
        Command::Switch {
            account, session, ..
        } => switch(account, session),
        Command::Repair { session } => repair(session),
        Command::Status {
            json,
            agent_session,
        } => status(json, agent_session),
        Command::Export { file } => export(&file),
        Command::Import { file, replace } => import(&file, replace),
        Command::Policy { command } => done(policy(command)?),
        Command::Install {
            agent: InstallAgent::Claude { shim },
        } => install_claude(shim),
        Command::Uninstall {
            agent: UninstallAgent::Claude,
        } => uninstall_claude(),
        Command::Daemon { stop } => daemon(stop),
        Command::Hook {
            agent,
            event,
            argument,
        } => Ok(hook(&agent, &event, argument.as_deref())),
        Command::Statusline { agent } => Ok(statusline(&agent)),
    }
}

fn done(message: String) -> Result<ExitCode, String> {
    println!("{message}");
    Ok(ExitCode::SUCCESS)
}

/// Account names may contain `@` and spaces; keep them one path segment.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'@' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn agent_bin() -> String {
    std::env::var("LLMPOOL_CLAUDE_BIN").unwrap_or_else(|_| "claude".into())
}

fn percent(row: &Value, window: &str) -> String {
    match row["usage"]["windows"][window]["used_percent"].as_f64() {
        Some(p) => format!("{p:.0}%"),
        None => "-".into(),
    }
}

fn table(header: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: Vec<String>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![line(header.iter().map(|h| h.to_string()).collect())];
    out.extend(rows.into_iter().map(line));
    out.join("\n")
}

// ---- accounts ---------------------------------------------------------------

struct AddOptions {
    from_current: bool,
    label: Option<String>,
    tags: Vec<String>,
    email: Option<String>,
    detach: bool,
    cancel: bool,
    interactive: bool,
    code: Option<String>,
}

/// Wait for the sign-in that is under way, saying where things stand.
fn follow_login(client: &Client) -> Result<ExitCode, String> {
    let mut shown_url = false;
    eprintln!("Waiting for you to finish signing in in your browser. Ctrl-C stops waiting; the sign-in itself carries on.");
    if std::io::stdin().is_terminal() {
        // Some browsers cannot hand the sign-in back by themselves and show a
        // code instead; it can simply be pasted here.
        eprintln!("If the page shows a code, paste it here and press Enter.");
        if let Some(sender) = Client::existing(Duration::from_secs(15)) {
            std::thread::spawn(move || {
                for line in std::io::stdin()
                    .lines()
                    .map_while(Result::ok)
                    .filter(|l| !l.trim().is_empty())
                {
                    if let Err(e) =
                        sender.call("POST", "/logins/code", Some(json!({"code": line.trim()})))
                    {
                        eprintln!("llmpool: {e}");
                    }
                }
            });
        }
    }
    loop {
        let status = client.call("GET", "/logins", None)?;
        match status["state"].as_str() {
            Some("waiting") => {
                if let (false, Some(url)) = (shown_url, status["url"].as_str()) {
                    eprintln!("If no page opened, go to:\n  {url}");
                    shown_url = true;
                }
                std::thread::sleep(Duration::from_millis(700));
            }
            Some("done") => {
                eprintln!("{TERMS_NOTICE}");
                return done(format!(
                    "added {}",
                    status["account"].as_str().unwrap_or_default()
                ));
            }
            _ => {
                return Err(status["message"]
                    .as_str()
                    .unwrap_or("the sign-in did not finish")
                    .to_string())
            }
        }
    }
}

fn add_claude(options: AddOptions) -> Result<ExitCode, String> {
    let client = connect()?;
    if let Some(code) = &options.code {
        client.call("POST", "/logins/code", Some(json!({"code": code})))?;
        if options.detach {
            return done("code sent; finishing the sign-in".into());
        }
        return follow_login(&client);
    }
    if options.cancel {
        let status = client.call("DELETE", "/logins", None)?;
        return done(
            status["message"]
                .as_str()
                .unwrap_or("no sign-in was waiting")
                .to_string(),
        );
    }
    let mut request = json!({"label": options.label, "tags": options.tags});
    if !options.from_current && !options.interactive {
        // Claude Code's own sign-in, run for us by the daemon: the browser
        // opens, and the login is taken into the pool when it finishes.
        request["email"] = json!(options.email);
        client.call("POST", "/logins", Some(request))?;
        if options.detach {
            return done("sign-in started; finish it in your browser".into());
        }
        return follow_login(&client);
    }
    if options.interactive {
        let scratch = data_dir().join(format!("login-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        eprintln!(
            "Claude Code will open in a temporary profile.\n\
             Log in with the account you want to add, then quit with /exit.\n\
             Do not use /logout: it would revoke the login you are adding.\n"
        );
        let status = std::process::Command::new(agent_bin())
            .env("CLAUDE_CONFIG_DIR", &scratch)
            .status();
        if let Err(e) = status {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(format!("could not run `{}`: {e}", agent_bin()));
        }
        request["profile_dir"] = json!(scratch);
    }
    let captured = client.call("POST", "/accounts/capture", Some(request.clone()));
    if captured.is_err() {
        // Nothing was added, so a half-made login must not be left lying around.
        if let Some(scratch) = request["profile_dir"].as_str() {
            let _ = std::fs::remove_dir_all(scratch);
        }
    }
    let account = captured?;
    eprintln!("{TERMS_NOTICE}");
    done(format!(
        "added {}",
        account["label"].as_str().unwrap_or_default()
    ))
}

/// `llmpool login <account>`: sign in again to an account already in the pool.
fn login_again(account: &str) -> Result<ExitCode, String> {
    let client = connect()?;
    let accounts = client.call("GET", "/accounts", None)?;
    let found = accounts
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| {
            [&a["id"], &a["label"], &a["email"]]
                .iter()
                .any(|v| v.as_str() == Some(account))
        })
        .ok_or_else(|| format!("no account named `{account}`"))?;
    client.call("POST", "/logins", Some(json!({"email": found["email"]})))?;
    follow_login(&client)
}

/// `llmpool doctor`: what is missing, and the command that fixes each thing.
fn doctor() -> Result<ExitCode, String> {
    let client = connect()?;
    let issues = client.call("GET", "/issues", None)?;
    let issues = issues.as_array().cloned().unwrap_or_default();
    if issues.is_empty() {
        return done("everything is in place".into());
    }
    for issue in &issues {
        let fix = match (issue["action"].as_str(), issue["email"].as_str()) {
            (Some("add_current"), _) => "llmpool add claude --from-current".to_string(),
            (Some("login"), Some(email)) => format!("llmpool login {email}"),
            (Some("login"), None) => "llmpool add claude".to_string(),
            (Some("install"), _) => "llmpool install claude".to_string(),
            _ => String::new(),
        };
        println!("• {}", issue["text"].as_str().unwrap_or_default());
        if !fix.is_empty() {
            println!("    fix: {fix}");
        }
    }
    Ok(ExitCode::FAILURE)
}

fn list(as_json: bool) -> Result<ExitCode, String> {
    let accounts = connect()?.call("GET", "/accounts", None)?;
    if as_json {
        return done(json!({"schemaVersion": 1, "accounts": accounts}).to_string());
    }
    let rows: Vec<Vec<String>> = accounts
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let tags: Vec<&str> = a["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let state = match (a["enabled"].as_bool(), a["health"].as_str()) {
                (Some(false), _) => "disabled".to_string(),
                (_, Some("ok")) => {
                    if a["is_default"] == true {
                        "ok (default login)".into()
                    } else {
                        "ok".into()
                    }
                }
                (_, Some(other)) => other.replace('_', " "),
                _ => String::new(),
            };
            vec![
                a["label"].as_str().unwrap_or_default().to_string(),
                tags.join(","),
                percent(a, "5h"),
                percent(a, "7d"),
                state,
                a["sessions"].as_array().map_or(0, Vec::len).to_string(),
            ]
        })
        .collect();
    if rows.is_empty() {
        return done("no accounts yet; add one with `llmpool add claude --from-current`".into());
    }
    done(table(
        &["ACCOUNT", "TAGS", "5H", "7D", "STATE", "SESSIONS"],
        rows,
    ))
}

fn patch(account: &str, body: Value) -> Result<ExitCode, String> {
    let updated = connect()?.call(
        "PATCH",
        &format!("/accounts/{}", encode(account)),
        Some(body),
    )?;
    done(format!(
        "updated {}",
        updated["label"].as_str().unwrap_or(account)
    ))
}

fn retag(
    account: &str,
    change: impl FnOnce(&mut std::collections::BTreeSet<String>),
) -> Result<ExitCode, String> {
    let client = connect()?;
    let accounts = client.call("GET", "/accounts", None)?;
    let current = accounts
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| {
            [&a["id"], &a["label"], &a["email"]]
                .iter()
                .any(|v| v.as_str() == Some(account))
        })
        .ok_or_else(|| format!("no account named `{account}`"))?;
    let mut tags = current["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    change(&mut tags);
    patch(account, json!({"tags": tags}))
}

// ---- sessions ---------------------------------------------------------------

fn git_remote(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !url.is_empty()).then_some(url)
}

/// Ask the user which account to use. `None`: they declined.
fn choose(candidates: &[Value]) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    eprintln!("llmpool: choose an account for this session");
    for (i, a) in candidates.iter().enumerate() {
        eprintln!(
            "  {}) {}  5h {}  7d {}",
            i + 1,
            a["label"].as_str().unwrap_or_default(),
            percent(a, "5h"),
            percent(a, "7d")
        );
    }
    eprint!("number (Enter for your default login): ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    let index: usize = line.trim().parse().ok()?;
    candidates.get(index.checked_sub(1)?)?["id"]
        .as_str()
        .map(str::to_string)
}

fn run_agent(
    account: Option<String>,
    tag: Option<String>,
    args: Vec<String>,
) -> Result<ExitCode, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let explicit = account.is_some() || tag.is_some();
    let request = json!({
        "cwd": cwd, "git_remote": git_remote(&cwd), "pid": std::process::id(), "account": account, "tag": tag,
    });

    // Without a daemon the agent still has to start; it runs on its own login.
    let started = connect().and_then(|client| {
        let mut reply = client.call("POST", "/sessions", Some(request))?;
        let sid = reply["session_id"].as_str().unwrap_or_default().to_string();
        if let Some(candidates) = reply["ask"].as_array().filter(|c| !c.is_empty()) {
            if let Some(id) = choose(candidates) {
                reply = client.call(
                    "POST",
                    &format!("/sessions/{sid}/assign"),
                    Some(json!({"account": id})),
                )?;
            }
        }
        Ok((client, sid, reply))
    });
    let (client, sid, reply) = match started {
        Ok((client, sid, reply)) => (Some(client), sid, reply),
        Err(e) if explicit => return Err(e),
        Err(e) => {
            eprintln!("llmpool: {e}; starting on your default login");
            (None, String::new(), Value::Null)
        }
    };

    let mut command = std::process::Command::new(agent_bin());
    command.args(args);
    match reply["account"]["label"].as_str() {
        Some(label) => {
            let rule = reply["rule"]
                .as_str()
                .map(|r| format!(" (rule \"{r}\")"))
                .unwrap_or_default();
            eprintln!("llmpool: running as {label}{rule}");
        }
        None if client.is_some() => {
            eprintln!("llmpool: no policy picked an account; using your default login")
        }
        None => {}
    }
    for (key, value) in reply["env"].as_object().into_iter().flatten() {
        command.env(key, value.as_str().unwrap_or_default());
    }
    let status = command.spawn().and_then(|mut child| {
        // The session belongs to the agent from here on: if this launcher is
        // killed while the agent keeps running, the session must stay.
        if let Some(client) = &client {
            let _ = client.call(
                "POST",
                &format!("/sessions/{sid}/pid"),
                Some(json!({"pid": child.id()})),
            );
        }
        child.wait()
    });
    if let Some(client) = &client {
        let _ = client.call("DELETE", &format!("/sessions/{sid}"), None);
    }
    let status = status.map_err(|e| format!("could not run `{}`: {e}", agent_bin()))?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

fn sessions(as_json: bool) -> Result<ExitCode, String> {
    let sessions = connect()?.call("GET", "/sessions", None)?;
    if as_json {
        return done(json!({"schemaVersion": 1, "sessions": sessions}).to_string());
    }
    let rows: Vec<Vec<String>> = sessions
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            vec![
                s["id"].as_str().unwrap_or_default().to_string(),
                s["account_label"]
                    .as_str()
                    .unwrap_or("(default login)")
                    .to_string(),
                percent(s, "5h"),
                percent(s, "7d"),
                s["cwd"].as_str().unwrap_or_default().to_string(),
            ]
        })
        .collect();
    if rows.is_empty() {
        return done("no sessions; start one with `llmpool run`".into());
    }
    done(table(
        &["SESSION", "ACCOUNT", "5H", "7D", "DIRECTORY"],
        rows,
    ))
}

/// The session a command is about: named, else this terminal's, else the only one.
fn target_session(client: &Client, named: Option<String>) -> Result<String, String> {
    if let Some(id) = named.or_else(|| std::env::var("LLMPOOL_SESSION").ok()) {
        return Ok(id);
    }
    let sessions = client.call("GET", "/sessions", None)?;
    match sessions.as_array().map(Vec::as_slice) {
        Some([only]) => Ok(only["id"].as_str().unwrap_or_default().to_string()),
        Some([]) | None => Err("no sessions are running".into()),
        Some(_) => Err(
            "several sessions are running; pick one with --session (see `llmpool sessions`)".into(),
        ),
    }
}

fn repair(session: Option<String>) -> Result<ExitCode, String> {
    let client = connect()?;
    let sid = target_session(&client, session)?;
    let reply = client.call("POST", &format!("/sessions/{sid}/reseat"), None)?;
    done(format!(
        "wrote the login of {} afresh for session {sid}",
        reply["label"].as_str().unwrap_or_default()
    ))
}

fn switch(account: Option<String>, session: Option<String>) -> Result<ExitCode, String> {
    let client = connect()?;
    let sid = target_session(&client, session)?;
    let label = match account {
        Some(account) => {
            client.call(
                "POST",
                &format!("/sessions/{sid}/swap"),
                Some(json!({"account": account})),
            )?;
            account
        }
        None => {
            let reply = client.call("POST", &format!("/sessions/{sid}/next"), None)?;
            reply["label"].as_str().unwrap_or_default().to_string()
        }
    };
    done(format!(
        "session {sid} now uses {label}; it takes effect within about a minute"
    ))
}

fn status(as_json: bool, agent_session: Option<String>) -> Result<ExitCode, String> {
    let client = connect()?;
    let sessions = client.call("GET", "/sessions", None)?;
    let accounts = client.call("GET", "/accounts", None)?;
    // This terminal's session, or the one Claude Code itself names.
    let current = std::env::var("LLMPOOL_SESSION").ok();
    let session = sessions
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| match &current {
            Some(id) => s["id"].as_str() == Some(id),
            None => {
                agent_session.is_some()
                    && s["agent_session_id"].as_str() == agent_session.as_deref()
            }
        })
        .cloned();
    if as_json {
        // Everything a surface needs to draw itself, in one call.
        let issues = client
            .call("GET", "/issues", None)
            .unwrap_or_else(|_| json!([]));
        let login = client
            .call("GET", "/logins", None)
            .unwrap_or_else(|_| json!({"state": "idle"}));
        return done(
            json!({"schemaVersion": 1, "session": session, "accounts": accounts, "sessions": sessions, "issues": issues, "login": login})
                .to_string(),
        );
    }
    match session {
        Some(s) => done(format!(
            "session {} · {} · 5h {} · 7d {}",
            s["id"].as_str().unwrap_or_default(),
            s["account_label"].as_str().unwrap_or("default login"),
            percent(&s, "5h"),
            percent(&s, "7d"),
        )),
        None => list(false),
    }
}

// ---- export and import ------------------------------------------------------

/// The passphrase for an export file: `LLMPOOL_PASSPHRASE` for scripts,
/// otherwise typed without echo (twice when a new file is being made).
fn passphrase(confirm: bool) -> Result<String, String> {
    if let Ok(passphrase) = std::env::var("LLMPOOL_PASSPHRASE") {
        return Ok(passphrase);
    }
    let first = rpassword::prompt_password("Passphrase: ")
        .map_err(|e| format!("could not read a passphrase: {e}"))?;
    if confirm && first != rpassword::prompt_password("Again: ").map_err(|e| e.to_string())? {
        return Err("the two passphrases differ".into());
    }
    Ok(first)
}

fn export(file: &Path) -> Result<ExitCode, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let client = connect()?;
    let exported = client.call(
        "POST",
        "/export",
        Some(json!({"passphrase": passphrase(true)?})),
    )?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    out.write_all(exported["bundle"].as_str().unwrap_or_default().as_bytes())
        .map_err(|e| e.to_string())?;
    eprintln!(
        "note: a login works on one machine at a time. After importing this file elsewhere,\n\
         remove the accounts here (`llmpool remove`), or the two copies will invalidate each other."
    );
    done(format!(
        "exported {} account(s) to {}",
        exported["accounts"],
        file.display()
    ))
}

/// `llmpool import claude-swap`: bring over the accounts claude-swap holds.
fn import_claude_swap() -> Result<ExitCode, String> {
    let dir = if cfg!(target_os = "linux") {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("claude-swap")
    } else {
        home().join(".claude-swap-backup")
    };
    let result = connect()?.call("POST", "/import/claude-swap", Some(json!({"dir": dir})))?;
    for name in result["added"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        println!("added {name}");
    }
    for skipped in result["skipped"].as_array().into_iter().flatten() {
        println!(
            "left out {}: {}",
            skipped["account"].as_str().unwrap_or_default(),
            skipped["why"].as_str().unwrap_or_default()
        );
    }
    eprintln!(
        "note: an imported login now belongs to llmpool. claude-swap still holds its own copy,\n\
         which stops working once the login is next used; switch accounts with llmpool from here on."
    );
    Ok(ExitCode::SUCCESS)
}

fn import(file: &Path, replace: bool) -> Result<ExitCode, String> {
    // The one word that is not a file name, unless there really is such a file.
    if file == Path::new("claude-swap") && !file.exists() {
        return import_claude_swap();
    }
    let bundle = read(file)?;
    let client = connect()?;
    let request = json!({"bundle": bundle, "passphrase": passphrase(false)?, "replace": replace});
    let result = client.call("POST", "/import", Some(request))?;
    let names = |key: &str| {
        let list: Vec<&str> = result[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if list.is_empty() {
            "none".to_string()
        } else {
            list.join(", ")
        }
    };
    done(format!(
        "added: {}\nreplaced: {}\nleft as they were: {}",
        names("added"),
        names("replaced"),
        names("skipped")
    ))
}

// ---- policies ---------------------------------------------------------------

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{}: no such file", path.display()),
        _ => format!("{}: {e}", path.display()),
    })
}

fn load_policy(path: &Path) -> Result<llmpool_core::policy::rule::PolicyFile, String> {
    // `file::load` treats a missing file as empty, which is right for the
    // daemon but wrong for a file the user named explicitly.
    read(path)?;
    file::load(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn policy(command: PolicyCommand) -> Result<String, String> {
    match command {
        PolicyCommand::Check { file } => {
            Ok(format!("ok: {} rule(s)", load_policy(&file)?.rules.len()))
        }
        PolicyCommand::DryRun { file, scenario } => {
            let policy = load_policy(&file)?;
            let parsed: Scenario = serde_json::from_str(&read(&scenario)?)
                .map_err(|e| format!("{}: {e}", scenario.display()))?;
            serde_json::to_string_pretty(&parsed.decide(&policy)).map_err(|e| e.to_string())
        }
        PolicyCommand::Path => Ok(data_dir().join("policies.toml").display().to_string()),
    }
}

// ---- install ----------------------------------------------------------------

fn claude_settings() -> PathBuf {
    // Inside a session llmpool started, CLAUDE_CONFIG_DIR is that session's
    // own profile, whose settings are a link to the user's; write to the real one.
    let custom = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|_| std::env::var_os("LLMPOOL_SESSION").is_none());
    custom
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
        .join("settings.json")
}

fn previous_status_path() -> PathBuf {
    data_dir().join("claude-statusline-previous.json")
}

/// llmpool's Claude Code mod, compiled into the binary so installing needs
/// nothing but the binary.
const MOD_FILES: [(&str, &str); 6] = [
    (
        ".claude-plugin/plugin.json",
        include_str!("../../../claude-plugin/llmpool/.claude-plugin/plugin.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("../../../claude-plugin/llmpool/hooks/hooks.json"),
    ),
    (
        "hooks/register.tsx",
        include_str!("../../../claude-plugin/llmpool/hooks/register.tsx"),
    ),
    (
        "hooks/view.ts",
        include_str!("../../../claude-plugin/llmpool/hooks/view.ts"),
    ),
    (
        "hooks/view.test.ts",
        include_str!("../../../claude-plugin/llmpool/hooks/view.test.ts"),
    ),
    (
        "types/index.d.ts",
        include_str!("../../../claude-plugin/llmpool/types/index.d.ts"),
    ),
];

fn mod_dir() -> PathBuf {
    data_dir().join("claude-plugin").join("llmpool")
}

/// Write the mod, pointing it at this binary.
fn write_mod(dir: &Path, exe: &Path) -> std::io::Result<()> {
    // The path lands inside a single-quoted TypeScript string.
    let exe = exe
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\'', "\\'");
    for (name, text) in MOD_FILES {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(dir))?;
        std::fs::write(path, text.replace("__LLMPOOL_BIN__", &exe))?;
    }
    Ok(())
}

fn shell_rc() -> Result<PathBuf, String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    match shell.rsplit('/').next() {
        Some("zsh") => Ok(home().join(".zshrc")),
        Some("bash") => Ok(home().join(".bashrc")),
        _ => Err(format!("--shim supports zsh and bash, not `{shell}`")),
    }
}

/// The rc file's text with llmpool's block removed.
fn without_shim(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let begin = lines.iter().position(|l| l.trim() == SHIM_BEGIN);
    let end = begin.and_then(|b| {
        lines[b..]
            .iter()
            .position(|l| l.trim() == SHIM_END)
            .map(|e| b + e)
    });
    // A begin marker with no end is not our block; touching it could eat the
    // rest of someone's rc file.
    let (Some(begin), Some(end)) = (begin, end) else {
        return text.to_string();
    };
    let mut kept: Vec<&str> = lines[..begin].to_vec();
    kept.extend(&lines[end + 1..]);
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn install_claude(shim: bool) -> Result<ExitCode, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let settings = claude_settings();
    std::fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
    write_mod(&mod_dir(), &exe).map_err(|e| e.to_string())?;
    // The daemon is often started without the shell's path; tell it where
    // Claude Code is while we can still see it.
    let claude = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join("claude"))
        .find(|path| path.is_file());
    match claude {
        Some(path) => std::fs::write(data_dir().join("claude-bin"), path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?,
        None => eprintln!("note: `claude` is not on your PATH; signing in from llmpool will not work until it is."),
    }
    let changed = install::install(&settings, &exe, &previous_status_path(), &mod_dir())
        .map_err(|e| e.to_string())?;
    println!(
        "{} {}",
        if changed {
            "updated"
        } else {
            "already installed in"
        },
        settings.display()
    );
    println!("installed the llmpool mod; in Claude Code run /llmpool to see accounts and switch");
    if shim {
        let rc = shell_rc()?;
        let text = without_shim(&std::fs::read_to_string(&rc).unwrap_or_default());
        let exe = exe.to_string_lossy().replace('\'', "'\\''");
        let block = format!("{SHIM_BEGIN}\nclaude() {{ '{exe}' run -- \"$@\"; }}\n{SHIM_END}\n");
        std::fs::write(&rc, text + &block).map_err(|e| format!("{}: {e}", rc.display()))?;
        println!(
            "added a `claude` function to {}; open a new terminal to use it",
            rc.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn uninstall_claude() -> Result<ExitCode, String> {
    let settings = claude_settings();
    let changed = install::uninstall(&settings, &previous_status_path(), &mod_dir())
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(data_dir().join("claude-plugin"));
    println!(
        "{} {}",
        if changed {
            "restored"
        } else {
            "nothing to remove from"
        },
        settings.display()
    );
    if let Ok(rc) = shell_rc() {
        if let Ok(text) = std::fs::read_to_string(&rc) {
            let cleaned = without_shim(&text);
            if cleaned != text {
                std::fs::write(&rc, cleaned).map_err(|e| format!("{}: {e}", rc.display()))?;
                println!("removed the `claude` function from {}", rc.display());
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---- daemon -----------------------------------------------------------------

fn daemon(stop: bool) -> Result<ExitCode, String> {
    let existing = Client::existing(Duration::from_secs(5));
    if stop {
        return match existing {
            Some(client) => client
                .call("POST", "/shutdown", None)
                .and_then(|_| done("daemon stopped".into())),
            None => done("daemon is not running".into()),
        };
    }
    if existing.is_some() {
        return Err("the daemon is already running".into());
    }
    // Taken before anything is opened: a second daemon must not touch state.
    let lock = http::instance_lock(&data_dir()).map_err(|e| e.to_string())?;
    let daemon = Daemon::open_default().map_err(|e| e.message)?;
    http::start(Arc::new(daemon), lock)
        .map_err(|e| e.to_string())?
        .wait();
    Ok(ExitCode::SUCCESS)
}

// ---- called by the agent ------------------------------------------------------

fn stdin_text() -> String {
    let mut text = String::new();
    let _ = std::io::stdin().read_to_string(&mut text);
    text
}

/// A hook must never get in the agent's way: every failure here is silent and
/// the exit code is always zero.
/// Which session this Claude Code is, as the daemon should know it: the id
/// llmpool gave it at launch, or `-` for one started on its own in the
/// user's profile. `None` for a Claude Code running in some other profile
/// that llmpool did not set up: its login is not `~/.claude`'s, and treating
/// it as if it were would credit and switch the wrong account.
fn agent_session() -> Option<String> {
    if let Ok(sid) = std::env::var("LLMPOOL_SESSION") {
        return Some(sid);
    }
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) if Path::new(&dir) != home().join(".claude") => None,
        _ => Some(NATIVE.to_string()),
    }
}

fn hook(agent: &str, event: &str, argument: Option<&str>) -> ExitCode {
    let input = stdin_text();
    if agent != "claude" {
        return ExitCode::SUCCESS;
    }
    let Some(sid) = agent_session() else {
        return ExitCode::SUCCESS;
    };
    // A hook never waits for the daemon. If there is none, a starting session
    // sets one going and moves on; its next hook finds it.
    let Some(client) = Client::existing(Duration::from_secs(2)) else {
        if event == "SessionStart" {
            let _ = spawn_daemon();
        }
        return ExitCode::SUCCESS;
    };
    let path = match argument {
        Some(argument) => format!("/hooks/claude/{sid}/{event}/{argument}"),
        None => format!("/hooks/claude/{sid}/{event}"),
    };
    let payload = serde_json::from_str(&input).unwrap_or(Value::Null);
    if let Ok(reply) = client.call("POST", &path, Some(payload)) {
        if reply.as_object().is_some_and(|o| !o.is_empty()) {
            println!("{reply}");
        }
    }
    ExitCode::SUCCESS
}

/// Report usage to the daemon, then show what the user's own status line
/// would have shown.
fn statusline(agent: &str) -> ExitCode {
    let input = stdin_text();
    let mut line = None;
    if let (Some(sid), "claude") = (agent_session(), agent) {
        if let Some(client) = Client::existing(Duration::from_millis(300)) {
            let payload = serde_json::from_str(&input).unwrap_or(Value::Null);
            line = client
                .call("POST", &format!("/statusline/claude/{sid}"), Some(payload))
                .ok()
                .and_then(|reply| reply["line"].as_str().map(str::to_string));
        }
    }
    let previous = install::previous_status_command(&previous_status_path()).and_then(|command| {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        // A command that never reads its input closes the pipe; that is fine.
        let _ = child.stdin.take()?.write_all(input.as_bytes());
        let out = child.wait_with_output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim_end().to_string()).filter(|s| !s.is_empty())
    });
    match (previous, line) {
        (Some(previous), Some(line)) => println!("{previous}  {line}"),
        (Some(text), None) | (None, Some(text)) => println!("{text}"),
        (None, None) => {}
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const POLICY: &str =
        "[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = {}, notify = true }\n";

    const SCENARIO: &str = r#"{
      "event": "rate_limited",
      "session": {"id": "s", "agent": "claude-code", "provider": "claude", "cwd": "/w",
                  "git_remote": null, "model": null, "account_id": "a", "last_swap_at": null},
      "accounts": [
        {"id": "a", "provider": "claude", "kind": "oauth", "label": "a", "email": null,
         "organization": null, "tags": [], "enabled": true, "health": "ok", "usage": null,
         "login_expires_at": null},
        {"id": "b", "provider": "claude", "kind": "oauth", "label": "b", "email": null,
         "organization": null, "tags": [], "enabled": true, "health": "ok", "usage": null,
         "login_expires_at": null}
      ]
    }"#;

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn check_accepts_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "p.toml", POLICY);
        assert_eq!(
            policy(PolicyCommand::Check { file }).unwrap(),
            "ok: 1 rule(s)"
        );
    }

    #[test]
    fn check_reports_invalid_rules_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            dir.path(),
            "bad.toml",
            "[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n",
        );
        let err = policy(PolicyCommand::Check { file }).unwrap_err();
        assert!(err.contains("moon_phase"), "{err}");
        let err = policy(PolicyCommand::Check {
            file: dir.path().join("nope.toml"),
        })
        .unwrap_err();
        assert!(err.contains("no such file"), "{err}");
    }

    #[test]
    fn dry_run_prints_the_decision_as_json() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "p.toml", POLICY);
        let scenario = write(dir.path(), "s.json", SCENARIO);
        let out = policy(PolicyCommand::DryRun { file, scenario }).unwrap();
        let decision: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(decision["rule"], "rl");
        assert_eq!(decision["outcome"]["swap"], "b");
        assert_eq!(decision["notify"], true);
    }

    #[test]
    fn dry_run_names_the_file_when_the_scenario_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "p.toml", POLICY);
        let scenario = write(dir.path(), "s.json", "{ not json");
        let err = policy(PolicyCommand::DryRun { file, scenario }).unwrap_err();
        assert!(err.contains("s.json"), "{err}");
    }

    #[test]
    fn shim_block_is_removed_cleanly_and_only_it() {
        let rc =
            format!("export A=1\n{SHIM_BEGIN}\nclaude() {{ x; }}\n{SHIM_END}\nalias k=kubectl\n");
        assert_eq!(without_shim(&rc), "export A=1\nalias k=kubectl\n");
        assert_eq!(without_shim("export A=1\n"), "export A=1\n");
        assert_eq!(without_shim("export A=1"), "export A=1");
        let unterminated = format!("export A=1\n{SHIM_BEGIN}\nalias k=kubectl\n");
        assert_eq!(without_shim(&unterminated), unterminated);
        assert_eq!(without_shim(""), "");
    }

    #[test]
    fn mod_is_written_with_the_binary_path_filled_in() {
        let dir = tempfile::tempdir().unwrap();
        write_mod(dir.path(), Path::new("/opt/it's/llmpool")).unwrap();
        let module = fs::read_to_string(dir.path().join("hooks/register.tsx")).unwrap();
        assert!(
            module.contains(r"const INSTALLED = '/opt/it\'s/llmpool'"),
            "binary path not filled in"
        );
        assert!(!module.contains("__LLMPOOL_BIN__"));
        assert!(dir.path().join(".claude-plugin/plugin.json").exists());
    }

    #[test]
    fn account_names_are_safe_path_segments() {
        assert_eq!(encode("me@x.io"), "me@x.io");
        assert_eq!(encode("work 1/2"), "work%201%2F2");
    }

    #[test]
    fn table_aligns_columns() {
        let out = table(&["A", "BB"], vec![vec!["long".into(), "x".into()]]);
        assert_eq!(out, "A     BB\nlong  x");
    }
}
