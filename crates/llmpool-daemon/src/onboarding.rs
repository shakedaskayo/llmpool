//! Everything about getting logins into the pool and keeping them usable:
//! signing in through Claude Code's own login, saying what is missing and how
//! to fix it, and renewing logins that sit unused.

use super::*;
use llmpool_adapter_claude::oauth;
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};

/// How long a sign-in may sit in the browser before it is given up on.
const LOGIN_PATIENCE_SECS: i64 = 600;
/// A renewal that did not work is not tried again sooner than this.
const RENEW_RETRY_SECS: i64 = 6 * 3600;
/// With renewal working, a login this close to its end means something is wrong.
const EXPIRY_WARN_SECS: i64 = 3 * 24 * 3600;

/// One sign-in in progress: Claude Code's `auth login` running in a scratch
/// profile, waiting for the person to finish in their browser.
pub(crate) struct LoginJob {
    child: Option<Child>,
    /// Claude Code asks for a code here when the browser could not hand the
    /// sign-in back by itself.
    input: Option<ChildStdin>,
    scratch: PathBuf,
    log: PathBuf,
    started: i64,
    email: Option<String>,
    label: Option<String>,
    tags: Vec<String>,
    /// "waiting", "done", "failed" or "cancelled".
    state: &'static str,
    message: String,
    account: Option<String>,
}

#[derive(Default, Deserialize)]
pub struct LoginReq {
    /// Pre-fills the address on the sign-in page; also how "sign in again"
    /// says which account is meant.
    pub email: Option<String>,
    pub label: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl LoginJob {
    fn stop(&mut self) {
        self.input = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Never logged out, which would revoke a login just made; only removed.
        let _ = std::fs::remove_dir_all(&self.scratch);
    }

    /// The sign-in page's address, for when the browser did not open by itself.
    fn url(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.log).ok()?;
        let start = text.find("https://")?;
        // The address is printed inside terminal link codes; it ends where
        // they, or a space, begin.
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || c.is_control())
            .unwrap_or(text.len() - start);
        Some(text[start..start + end].to_string())
    }
}

impl Daemon {
    fn login_slot(&self) -> MutexGuard<'_, Option<LoginJob>> {
        self.login
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Where the `claude` program is. A daemon started from a hook or from
    /// the app does not have the user's shell path, so the installer records
    /// what it found.
    pub fn claude_bin(&self) -> Option<PathBuf> {
        let recorded = std::fs::read_to_string(self.data_dir.join("claude-bin"))
            .ok()
            .map(|t| PathBuf::from(t.trim()));
        let on_path = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|dir| dir.join("claude"));
        std::env::var_os("LLMPOOL_CLAUDE_BIN")
            .map(PathBuf::from)
            .into_iter()
            .chain(recorded)
            .chain(on_path)
            .chain([
                self.home.join(".local/bin/claude"),
                self.home.join(".claude/local/claude"),
                PathBuf::from("/opt/homebrew/bin/claude"),
                PathBuf::from("/usr/local/bin/claude"),
            ])
            .find(|path| path.is_file())
    }

    /// Start a sign-in. Claude Code opens the browser; llmpool only waits for
    /// it to finish and then takes the login into the pool. It never sees a
    /// password.
    pub fn start_login(&self, req: LoginReq) -> Api<Value> {
        self.poll_login();
        let claude = self
            .claude_bin()
            .ok_or_else(|| fail(409, "Claude Code was not found. Install it, then run `llmpool install claude` in a terminal."))?;
        let number = {
            let mut g = self.lock();
            g.p.next += 1;
            g.p.next
        };
        let mut slot = self.login_slot();
        if slot.as_ref().is_some_and(|job| job.state == "waiting") {
            return Err(fail(
                409,
                "a sign-in is already waiting in your browser; finish or cancel it first",
            ));
        }
        let scratch = self.data_dir.join(format!("login-{number}"));
        std::fs::create_dir_all(&scratch)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
        }
        let log = self.data_dir.join("login.log");
        let out = std::fs::File::create(&log)?;
        let mut command = Command::new(claude);
        command.args(["auth", "login"]);
        if let Some(email) = req.email.as_deref().filter(|e| !e.is_empty()) {
            command.args(["--email", email]);
        }
        let mut child = command
            .env("CLAUDE_CONFIG_DIR", &scratch)
            .env_remove("LLMPOOL_SESSION")
            .stdin(Stdio::piped())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .map_err(|e| fail(500, format!("could not start Claude Code's sign-in: {e}")))?;
        *slot = Some(LoginJob {
            input: child.stdin.take(),
            child: Some(child),
            scratch,
            log,
            started: now(),
            email: req.email,
            label: req.label,
            tags: req.tags,
            state: "waiting",
            message: "Finish signing in in your browser.".into(),
            account: None,
        });
        drop(slot);
        Ok(self.login_status())
    }

    /// Move a waiting sign-in along: notice that it finished, take the login
    /// in, or give up on it.
    pub fn poll_login(&self) {
        let finished = {
            let mut slot = self.login_slot();
            let Some(job) = slot.as_mut().filter(|job| job.state == "waiting") else {
                return;
            };
            let exited = job
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten());
            match exited {
                None if now() - job.started > LOGIN_PATIENCE_SECS => {
                    job.stop();
                    job.state = "failed";
                    job.message = "Nobody finished signing in within ten minutes.".into();
                    return;
                }
                None => return,
                Some(status) if !status.success() => {
                    job.stop();
                    job.state = "failed";
                    job.message = "The sign-in was not completed.".into();
                    return;
                }
                Some(_) => {
                    job.child = None;
                    (
                        job.scratch.clone(),
                        job.label.clone(),
                        job.tags.clone(),
                        job.email.clone(),
                    )
                }
            }
        };
        // Taken into the pool without the sign-in lock held: `capture` has
        // its own locking.
        let (scratch, label, tags, asked) = finished;
        // The address asked for only pre-fills the page; a browser already
        // signed in to another account returns that account. Saying "done"
        // would then be a lie about which login was added.
        let got = Profile::at(&scratch).identity().map(|(email, _)| email);
        let wrong = match (&asked, &got) {
            (Some(asked), Some(got)) if !asked.is_empty() && !asked.eq_ignore_ascii_case(got) => {
                Some((asked.clone(), got.clone()))
            }
            _ => None,
        };
        if let Some((asked, got)) = &wrong {
            let known = self.lock().store.list().is_ok_and(|accounts| {
                accounts
                    .iter()
                    .any(|a| a.email.as_deref() == Some(got.as_str()))
            });
            if known {
                // Nothing new to add, and not what was asked for.
                let mut slot = self.login_slot();
                if let Some(job) = slot.as_mut() {
                    job.stop();
                    job.state = "failed";
                    job.message = format!(
                        "Your browser signed in as {got}, not {asked}: it is already signed in to that account. \
                         Sign out of claude.ai in the browser, or open the sign-in link in a private window, and try again."
                    );
                }
                return;
            }
        }
        let captured = self.capture(CaptureReq {
            profile_dir: Some(scratch),
            label,
            tags,
        });
        let mut slot = self.login_slot();
        if let Some(job) = slot.as_mut() {
            job.stop();
            match captured {
                Ok(account) => {
                    let name = account["label"].as_str().unwrap_or_default().to_string();
                    job.state = "done";
                    job.message = match &wrong {
                        Some((asked, got)) => format!(
                            "Signed in as {got}, not {asked} as asked; {got} is in the pool."
                        ),
                        None => format!("{name} is in the pool."),
                    };
                    job.account = Some(name.clone());
                    drop(slot);
                    self.tell(&format!("{name} is signed in and in the pool."));
                }
                Err(e) => {
                    job.state = "failed";
                    job.message = e.message;
                }
            }
        }
    }

    /// Pass on the code the sign-in page shows when it cannot return to
    /// Claude Code by itself.
    pub fn submit_login_code(&self, code: &str) -> Api<Value> {
        let code = code.trim();
        if code.is_empty() || code.contains(char::is_whitespace) {
            return Err(fail(400, "that does not look like a sign-in code"));
        }
        {
            let mut slot = self.login_slot();
            let job = slot
                .as_mut()
                .filter(|job| job.state == "waiting")
                .ok_or_else(|| fail(409, "no sign-in is waiting for a code"))?;
            let input = job
                .input
                .as_mut()
                .ok_or_else(|| fail(409, "this sign-in cannot take a code any more"))?;
            writeln!(input, "{code}").and_then(|()| input.flush())?;
            job.message = "Code sent. Finishing the sign-in…".into();
        }
        Ok(self.login_status())
    }

    pub fn cancel_login(&self) -> Value {
        if let Some(job) = self
            .login_slot()
            .as_mut()
            .filter(|job| job.state == "waiting")
        {
            job.stop();
            job.state = "cancelled";
            job.message = "Sign-in cancelled.".into();
        }
        self.login_status()
    }

    pub fn login_status(&self) -> Value {
        self.poll_login();
        match self.login_slot().as_ref() {
            None => json!({"state": "idle"}),
            Some(job) => json!({
                "state": job.state,
                "message": job.message,
                "account": job.account,
                "email": job.email,
                "url": if job.state == "waiting" { job.url() } else { None },
                "seconds_left": (LOGIN_PATIENCE_SECS - (now() - job.started)).max(0),
            }),
        }
    }

    /// What stands between the user and a working pool, each with the action
    /// that fixes it. Every surface shows these the same way.
    pub fn issues(&self) -> Api<Value> {
        let g = self.lock();
        let accounts = g.store.list()?;
        let mut issues = Vec::new();
        let mut add = |kind: &str, text: String, action: &str, email: Option<&str>| {
            issues.push(json!({"kind": kind, "text": text, "action": action, "email": email}));
        };
        if self.claude_bin().is_none() {
            add(
                "claude_missing",
                "Claude Code was not found on this machine.".into(),
                "none",
                None,
            );
        }
        let hooked = std::fs::read_to_string(self.home.join(".claude/settings.json"))
            .is_ok_and(|t| t.contains(" hook claude "));
        if !hooked {
            add("hooks_missing", "llmpool is not connected to Claude Code yet. Run `llmpool install claude` in a terminal.".into(), "install", None);
        }
        let current = self.default_profile().identity();
        let in_pool = |email: &str, org: &Option<String>| {
            accounts
                .iter()
                .any(|a| same_login(a, email, org.as_deref()))
        };
        match &current {
            Some((email, org)) if !in_pool(email, org) => add(
                "login_not_in_pool",
                format!("The login Claude Code uses now ({email}) is not in the pool, so llmpool cannot switch it."),
                "add_current",
                Some(email),
            ),
            None if accounts.is_empty() => add("no_accounts", "No accounts yet. Sign in to add the first one.".into(), "login", None),
            _ => {}
        }
        let t = now();
        for account in &accounts {
            let email = account.email.as_deref();
            if account.health != Health::Ok {
                add(
                    "relogin",
                    format!("{} needs a new sign-in.", account.label),
                    "login",
                    email,
                );
            } else if let Some(left) = account
                .login_expires_at
                .map(|at| at - t)
                .filter(|left| *left < EXPIRY_WARN_SECS)
            {
                let days = (left / 86400).max(0);
                add(
                    "expiring",
                    format!(
                        "{}'s sign-in runs out in {days} day(s) and could not be renewed.",
                        account.label
                    ),
                    "login",
                    email,
                );
            }
        }
        Ok(json!(issues))
    }

    /// Renew one login that is due, if there is one. Only logins no profile
    /// holds: a Claude Code that is using a login renews it itself, and a
    /// second renewer would invalidate the first.
    pub fn renew_idle(&self) {
        let Some(url) = self.token_url.clone() else {
            return;
        };
        let picked = {
            let mut g = self.lock();
            let t = now();
            let default = self.default_profile().identity();
            let Ok(accounts) = g.store.list() else {
                return;
            };
            let due = accounts.into_iter().find_map(|account| {
                let idle = account.usable()
                    && account.kind == Kind::Oauth
                    && self.holders_given(&g, &account, &default).is_empty()
                    && !g.renewing.contains(&account.id)
                    && g.p
                        .renew_tried
                        .get(&account.id)
                        .is_none_or(|at| t - at >= RENEW_RETRY_SECS);
                let secret = idle.then(|| self.secret_of(&account.id).ok()).flatten()?;
                oauth::due(&secret, t * 1000).then_some((account, secret))
            });
            if let Some((account, _)) = &due {
                // Held out of every assignment while its only valid token is
                // in flight.
                g.renewing.insert(account.id.clone());
                g.p.renew_tried.insert(account.id.clone(), t);
            }
            due
        };
        let Some((mut account, mut secret)) = picked else {
            return;
        };
        let outcome = oauth::renew(&mut secret, &url, now() * 1000);

        let mut g = self.lock();
        g.renewing.remove(&account.id);
        match outcome {
            oauth::Outcome::Renewed => {
                // The old token is spent; the new one must not be lost.
                let text = serde_json::to_string(&secret).unwrap_or_default();
                let kept = (0..3).any(|attempt| {
                    if attempt > 0 {
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    self.vault.put(&account.id, &text).is_ok()
                });
                if kept {
                    account.login_expires_at = login_expiry(&secret).or(account.login_expires_at);
                    self.log(
                        &mut g,
                        "renew",
                        None,
                        format!("renewed the sign-in of {}", account.label),
                    );
                } else {
                    account.health = Health::ReloginRequired;
                    let text = format!(
                        "llmpool: {} was renewed but could not be saved; it needs a new sign-in.",
                        account.label
                    );
                    self.tell(&text);
                    self.log(&mut g, "login", None, text);
                }
                let _ = g.store.put(&account);
            }
            oauth::Outcome::Dead => {
                account.health = Health::ReloginRequired;
                let _ = g.store.put(&account);
                let text = format!(
                    "llmpool: {}'s sign-in has ended. Sign in again to keep using it.",
                    account.label
                );
                self.tell(&text);
                self.log(&mut g, "login", None, text);
            }
            oauth::Outcome::Later(why) => {
                self.log(
                    &mut g,
                    "renew_later",
                    None,
                    format!("could not renew {} yet: {why}", account.label),
                );
            }
        }
        self.save(&g);
    }
}
