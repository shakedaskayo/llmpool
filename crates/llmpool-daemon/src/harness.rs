//! Agents other than Claude Code that keep a login of their own in files
//! (Codex CLI, OpenCode, anything described in `harnesses.toml`).
//!
//! An account is a copy of the agent's login files. A session on an account
//! other than the one the agent is logged in to gets a directory of its own:
//! those files, and links to everything else the agent keeps, so settings and
//! history stay shared. The agent is pointed at it through its own variable.
//! When the session ends the files are read back, because agents renew their
//! tokens in place.
//!
//! The login is read when the agent starts. llmpool does not change it under
//! a running agent of this kind: which account it runs on is decided at start.

use super::*;
use base64::Engine;
use llmpool_core::harness::{self, FileLogin, Harness};

/// A directory llmpool made for sessions of one file-login account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FileStore {
    root: PathBuf,
    inner: PathBuf,
    account_id: AccountId,
    files: Vec<String>,
}

#[derive(Default, Deserialize)]
pub struct FilesReq {
    pub harness: String,
    pub label: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Read the login from here instead of the agent's usual place: a
    /// directory a sign-in was just done in.
    pub home: Option<PathBuf>,
}

type Files = BTreeMap<String, String>;

fn files_of(secret: &str) -> Api<Files> {
    let value: Value = serde_json::from_str(secret)
        .map_err(|_| fail(409, "this account's stored login is damaged; add it again"))?;
    let files: Files = serde_json::from_value(value["files"].clone())
        .map_err(|_| fail(409, "this account's stored login is damaged; add it again"))?;
    // Names only: a stored login can come from a file or a team's server.
    if files.is_empty()
        || files
            .keys()
            .any(|n| n.is_empty() || n.contains(['/', '\\']) || n.starts_with('.'))
    {
        return Err(fail(
            409,
            "this account's stored login is damaged; add it again",
        ));
    }
    Ok(files)
}

/// Whether a stored secret is an agent's login files.
pub(crate) fn is_login(secret: &str) -> bool {
    files_of(secret).is_ok()
}

fn read_files(dir: &Path, names: &[String]) -> Option<Files> {
    let files: Files = names
        .iter()
        .filter_map(|name| Some((name.clone(), std::fs::read_to_string(dir.join(name)).ok()?)))
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    (!files.is_empty()).then_some(files)
}

/// Who a login belongs to, when its files say: the address inside an ID token.
fn identity(files: &Files) -> Option<String> {
    fn id_token(value: &Value) -> Option<&str> {
        match value {
            Value::Object(map) => map
                .get("id_token")
                .and_then(Value::as_str)
                .or_else(|| map.values().find_map(id_token)),
            _ => None,
        }
    }
    files.values().find_map(|text| {
        let value: Value = serde_json::from_str(text).ok()?;
        let payload = id_token(&value)?.split('.').nth(1)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .ok()?;
        let claims: Value = serde_json::from_slice(&bytes).ok()?;
        claims["email"].as_str().map(str::to_lowercase)
    })
}

/// Whether two sets of login files are the same login, possibly renewed.
fn same(a: &Files, b: &Files) -> bool {
    match (identity(a), identity(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(text.as_bytes())
}

/// Link everything in `from` into `to`, except the names in `skip`.
fn link_all(from: &Path, to: &Path, skip: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if skip.iter().any(|s| name.to_string_lossy() == *s) {
            continue;
        }
        let _ = std::os::unix::fs::symlink(entry.path(), to.join(&name));
    }
    Ok(())
}

/// Where a command can be found, looking also where agents install
/// themselves: an app started at login has a short `PATH`.
fn installed(command: &str, home: &Path) -> bool {
    if command.contains('/') {
        return Path::new(command).is_file();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let extra = [
        format!(".{command}/bin"),
        ".local/bin".into(),
        ".bun/bin".into(),
        ".cargo/bin".into(),
        ".npm-global/bin".into(),
    ];
    std::env::split_paths(&path)
        .chain(extra.iter().map(|d| home.join(d)))
        .chain(
            ["/opt/homebrew/bin", "/usr/local/bin"]
                .iter()
                .map(PathBuf::from),
        )
        .any(|dir| dir.join(command).is_file())
}

impl Daemon {
    pub(crate) fn harness_list(&self) -> Vec<Harness> {
        harness::load(&self.data_dir.join("harnesses.toml")).unwrap_or_else(|_| harness::builtin())
    }

    fn harness(&self, name: &str) -> Api<(Harness, FileLogin)> {
        let found = self
            .harness_list()
            .into_iter()
            .find(|h| h.name == name)
            .ok_or_else(|| fail(404, format!("llmpool knows no agent called `{name}`")))?;
        let login = found.login.clone().ok_or_else(|| {
            fail(
                400,
                format!(
                    "{} has no login of its own; give it a key with `llmpool add key`",
                    found.title
                ),
            )
        })?;
        Ok((found, login))
    }

    /// Every agent llmpool can start, with what it found of each on this machine.
    pub fn harnesses(&self) -> Api<Value> {
        let g = self.lock();
        let accounts = g.store.list()?;
        let file_error = harness::load(&self.data_dir.join("harnesses.toml")).err();
        let rows: Vec<Value> = self
            .harness_list()
            .iter()
            .map(|h| {
                let current = h
                    .login
                    .as_ref()
                    .and_then(|l| read_files(&l.dirs(&self.home).1, &l.files));
                let known = current.as_ref().is_some_and(|files| {
                    accounts.iter().filter(|a| a.provider == h.name).any(|a| {
                        self.vault
                            .get(&a.id)
                            .ok()
                            .flatten()
                            .and_then(|s| files_of(&s).ok())
                            .is_some_and(|have| same(&have, files))
                    })
                });
                json!({
                    "name": h.name, "title": h.title, "command": h.command, "provider": h.provider,
                    "installed": installed(&h.command, &self.home),
                    "has_login": h.login.is_some(),
                    "logged_in": current.is_some(),
                    "login_as": current.as_ref().and_then(identity),
                    "login_in_pool": known,
                    "accounts": accounts.iter().filter(|a| a.provider == h.name || Some(&a.provider) == h.provider.as_ref()).count(),
                })
            })
            .collect();
        Ok(
            json!({"harnesses": rows, "error": file_error, "file": self.data_dir.join("harnesses.toml")}),
        )
    }

    /// Add (or refresh) an account from the login an agent has now.
    pub fn capture_files(&self, req: FilesReq) -> Api<Value> {
        let (harness, login) = self.harness(&req.harness)?;
        let dir = match &req.home {
            Some(home) => match &login.subdir {
                Some(sub) => home.join(sub),
                None => home.clone(),
            },
            None => login.dirs(&self.home).1,
        };
        let files = read_files(&dir, &login.files).ok_or_else(|| {
            fail(
                404,
                format!(
                    "{} is not signed in ({} holds no login)",
                    harness.title,
                    dir.display()
                ),
            )
        })?;
        let label = req
            .label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .or_else(|| identity(&files))
            .unwrap_or_else(|| harness.name.clone());
        let g = self.lock();
        // The same address is often a login of another agent too; then this
        // one says which agent it is for.
        let taken = g.store.list()?.iter().any(|a| {
            a.provider != harness.name && (a.label == label || a.email.as_deref() == Some(&label))
        });
        let label = if taken {
            format!("{}:{label}", harness.name)
        } else {
            label
        };
        let id = keys::id_of(&harness.name, &label);
        if g.p.shared.contains(&id) {
            return Err(fail(403, TEAM_OWNED));
        }
        label_is_free(&g.store.list()?, &id, &label)?;
        let mut account = g.store.get(&id)?.unwrap_or_else(|| Account {
            id: id.clone(),
            provider: harness.name.clone(),
            kind: Kind::Oauth,
            label,
            email: identity(&files),
            organization: None,
            tags: BTreeSet::new(),
            enabled: true,
            health: Health::Ok,
            usage: None,
            login_expires_at: None,
        });
        account.health = Health::Ok;
        account.tags.extend(req.tags);
        self.vault
            .put(&id, &json!({ "files": files }).to_string())?;
        g.store.put(&account)?;
        Ok(serde_json::to_value(&account)?)
    }

    /// Put a session on a file-login account before its agent starts.
    pub(crate) fn attach_files(
        &self,
        g: &mut Inner,
        sid: &str,
        account: &Account,
    ) -> Api<BTreeMap<String, String>> {
        let (_, login) = self.harness(&account.provider)?;
        let text = self
            .vault
            .get(&account.id)?
            .ok_or_else(|| fail(409, "this account has no stored login; add it again"))?;
        let files = files_of(&text)?;
        let mut env = BTreeMap::from([("LLMPOOL_SESSION".to_string(), sid.to_string())]);
        let (default_root, default_inner) = login.dirs(&self.home);

        let at_home = read_files(&default_inner, &login.files);
        if let Some(current) = at_home.as_ref().filter(|current| same(current, &files)) {
            // The agent is already logged in as this account: it runs where
            // it always does, and its copy, renewed or not, is the newer one.
            self.vault
                .put(&account.id, &json!({ "files": current }).to_string())?;
        } else {
            let key = format!("fs-{}", account.id);
            if !g.p.file_stores.contains_key(&key) {
                let root = self.data_dir.join("stores").join(&key);
                let _ = std::fs::remove_dir_all(&root);
                let inner = match &login.subdir {
                    Some(sub) => {
                        link_all(&default_root, &root, &[sub.as_str()])?;
                        root.join(sub)
                    }
                    None => root.clone(),
                };
                let skip: Vec<&str> = login.files.iter().map(String::as_str).collect();
                link_all(&default_inner, &inner, &skip)?;
                for (name, text) in &files {
                    write_private(&inner.join(name), text)?;
                }
                g.p.file_stores.insert(
                    key.clone(),
                    FileStore {
                        root,
                        inner,
                        account_id: account.id.clone(),
                        files: login.files.clone(),
                    },
                );
            }
            env.insert(
                login.home_env.clone(),
                g.p.file_stores[&key].root.to_string_lossy().into_owned(),
            );
        }
        if let Some(m) = g.p.sessions.get_mut(sid) {
            m.session.account_id = Some(account.id.clone());
        }
        self.log(
            g,
            "assign",
            Some(sid),
            format!("session started as {}", account.label),
        );
        Ok(env)
    }

    /// Retire directories no session runs in: keep their login, which the
    /// agent may have renewed, then remove them.
    pub(crate) fn sweep_files(&self, g: &mut Inner) {
        let idle: Vec<String> =
            g.p.file_stores
                .iter()
                .filter(|(_, store)| {
                    !g.p.sessions
                        .values()
                        .any(|m| m.session.account_id.as_ref() == Some(&store.account_id))
                })
                .map(|(key, _)| key.clone())
                .collect();
        for key in idle {
            let store = g.p.file_stores[&key].clone();
            let kept = match read_files(&store.inner, &store.files) {
                Some(files) => self
                    .vault
                    .put(&store.account_id, &json!({ "files": files }).to_string())
                    .is_ok(),
                // Signed out inside the session: nothing newer to keep.
                None => true,
            };
            if kept {
                let _ = std::fs::remove_dir_all(&store.root);
                g.p.file_stores.remove(&key);
            }
        }
    }
}
