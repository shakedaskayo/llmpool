//! The llmpool daemon: owns the account pool, the vault, the live sessions and
//! the policy file, and decides which login each session's profile holds.

pub mod client;
pub mod http;

use chrono::{Datelike, Local, Timelike};
use llmpool_adapter_claude::profile::{Keychain, MacKeychain, NoKeychain, Profile, Secret};
use llmpool_adapter_claude::{lock, short_hash, statusline};
use llmpool_core::model::{Account, AccountId, Health, Kind, Session};
use llmpool_core::policy::context::{Context, Event};
use llmpool_core::policy::engine::{self, Decision, Outcome};
use llmpool_core::policy::rule::{Pick, PolicyFile, Target};
use llmpool_core::policy::{file, select, triggers};
use llmpool_core::store::Store;
use llmpool_core::vault::{FileVault, KeychainVault, Vault};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Store id of the user's own `~/.claude` profile.
const DEFAULT_STORE: &str = "default";
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// Claude Code caches its Keychain for about thirty seconds, so readings
/// right after a swap may still describe the previous account.
const SETTLE_SECS: i64 = 60;
const MAX_EVENTS: usize = 200;
const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

fn fail(status: u16, message: impl Into<String>) -> ApiError {
    ApiError { status, message: message.into() }
}

impl<E: std::error::Error> From<E> for ApiError {
    fn from(e: E) -> Self {
        fail(500, e.to_string())
    }
}

type Api<T> = Result<T, ApiError>;

/// A session llmpool launched.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Managed {
    #[serde(flatten)]
    pub session: Session,
    /// The `llmpool run` process; when it is gone the session is over.
    pub pid: u32,
    /// Which profile the agent runs in. `None`: the agent's own login, untouched.
    pub store: Option<String>,
    pub agent_session_id: Option<String>,
    pub started_at: i64,
    /// Messages waiting for the next prompt.
    #[serde(default)]
    pub notices: Vec<String>,
    /// Last notice queued, so a rule that keeps firing says it once.
    #[serde(default)]
    pub last_notice: Option<String>,
}

/// A profile llmpool created, and the account whose login it holds. One
/// account lives in at most one profile, because its refresh token is
/// single-use: two copies would invalidate each other.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoreRec {
    pub profile: Profile,
    pub account_id: AccountId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEvent {
    pub at: i64,
    pub kind: String,
    pub session: Option<String>,
    pub message: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    sessions: BTreeMap<String, Managed>,
    stores: BTreeMap<String, StoreRec>,
    events: VecDeque<LogEvent>,
    rr_cursor: Option<String>,
    next: u64,
}

struct Inner {
    store: Store,
    p: Persisted,
}

#[derive(Default, Deserialize)]
pub struct CaptureReq {
    /// A profile directory to read the login from. `None`: the default profile.
    pub profile_dir: Option<PathBuf>,
    pub label: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Deserialize)]
pub struct PatchReq {
    pub label: Option<String>,
    pub tags: Option<BTreeSet<String>>,
    pub enabled: Option<bool>,
}

#[derive(Deserialize)]
pub struct StartReq {
    pub cwd: String,
    pub git_remote: Option<String>,
    pub pid: u32,
    pub account: Option<String>,
    pub tag: Option<String>,
}

#[derive(Deserialize)]
pub struct DryRunReq {
    /// Policy text to test. `None`: the saved policy.
    pub text: Option<String>,
    pub event: Event,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

pub struct Daemon {
    data_dir: PathBuf,
    home: PathBuf,
    vault: Box<dyn Vault>,
    keychain: Box<dyn Keychain>,
    inner: Mutex<Inner>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn pid_alive(pid: u32) -> bool {
    // Signal 0 checks for existence without sending anything.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn find<'a>(accounts: &'a [Account], name: &str) -> Api<&'a Account> {
    accounts
        .iter()
        .find(|a| a.id == name || a.label == name || a.email.as_deref() == Some(name))
        .ok_or_else(|| fail(404, format!("no account named `{name}`")))
}

fn same_login(account: &Account, email: &str, organization: Option<&str>) -> bool {
    account.email.as_deref() == Some(email) && account.organization.as_deref() == organization
}

impl Daemon {
    pub fn open(data_dir: &Path, home: &Path, vault: Box<dyn Vault>, keychain: Box<dyn Keychain>) -> Api<Self> {
        std::fs::create_dir_all(data_dir)?;
        let p = std::fs::read_to_string(data_dir.join("state.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let daemon = Self {
            data_dir: data_dir.to_path_buf(),
            home: home.to_path_buf(),
            vault,
            keychain,
            inner: Mutex::new(Inner { store: Store::open(&data_dir.join("llmpool.db"))?, p }),
        };
        daemon.reap(&mut daemon.lock());
        Ok(daemon)
    }

    /// Open the daemon on the user's real data directory, secrets in the OS
    /// keychain. `LLMPOOL_FILE_VAULT` and `LLMPOOL_NO_CLAUDE_KEYCHAIN` switch
    /// to plain files, for tests and machines without a keychain.
    pub fn open_default() -> Api<Self> {
        let data = client::data_dir();
        let vault: Box<dyn Vault> = match std::env::var_os("LLMPOOL_FILE_VAULT") {
            Some(_) => Box::new(FileVault(data.join("vault"))),
            None => Box::new(KeychainVault),
        };
        let keychain: Box<dyn Keychain> =
            if cfg!(target_os = "macos") && std::env::var_os("LLMPOOL_NO_CLAUDE_KEYCHAIN").is_none() {
                Box::new(MacKeychain)
            } else {
                Box::new(NoKeychain)
            };
        Self::open(&data, &client::home(), vault, keychain)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn policy_path(&self) -> PathBuf {
        self.data_dir.join("policies.toml")
    }

    fn default_profile(&self) -> Profile {
        Profile::default_in(&self.home)
    }

    fn save(&self, g: &Inner) {
        if let Ok(text) = serde_json::to_string_pretty(&g.p) {
            let tmp = self.data_dir.join("state.json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, self.data_dir.join("state.json"));
            }
        }
    }

    fn log(&self, g: &mut Inner, kind: &str, session: Option<&str>, message: String) {
        g.p.events.push_back(LogEvent { at: now(), kind: kind.into(), session: session.map(Into::into), message });
        while g.p.events.len() > MAX_EVENTS {
            g.p.events.pop_front();
        }
    }

    /// All accounts, with usage windows that have already reset dropped.
    fn accounts(&self, g: &Inner) -> Api<Vec<Account>> {
        let t = now();
        let mut accounts = g.store.list()?;
        for usage in accounts.iter_mut().filter_map(|a| a.usage.as_mut()) {
            usage.windows.retain(|_, w| w.resets_at.is_none_or(|at| at > t));
        }
        Ok(accounts)
    }

    fn profile_of(&self, g: &Inner, store: &str) -> Option<Profile> {
        if store == DEFAULT_STORE {
            Some(self.default_profile())
        } else {
            g.p.stores.get(store).map(|rec| rec.profile.clone())
        }
    }

    /// The profile currently holding this account's login, if any.
    fn holder(&self, g: &Inner, account: &Account) -> Option<String> {
        if let Some((email, organization)) = self.default_profile().identity() {
            if same_login(account, &email, organization.as_deref()) {
                return Some(DEFAULT_STORE.to_string());
            }
        }
        g.p.stores.iter().find(|(_, rec)| rec.account_id == account.id).map(|(id, _)| id.clone())
    }

    fn secret_of(&self, id: &str) -> Api<Secret> {
        let text = self.vault.get(id)?.ok_or_else(|| fail(409, "this account has no stored login; add it again"))?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Copy a profile's login back into the vault, because the agent may have
    /// rotated the token since we wrote it. Caller holds the profile's locks.
    fn read_back(&self, accounts: &[Account], profile: &Profile, account_id: &str) {
        let (Some(secret), Some(account)) =
            (profile.read(&*self.keychain), accounts.iter().find(|a| a.id == account_id))
        else {
            return;
        };
        // If the user logged this profile into something else, it is not ours to file.
        if same_login(account, secret.email().unwrap_or_default(), secret.organization()) {
            if let Ok(text) = serde_json::to_string(&secret) {
                let _ = self.vault.put(account_id, &text);
            }
        }
    }

    /// End sessions whose launcher process is gone.
    fn reap(&self, g: &mut Inner) {
        let dead: Vec<String> =
            g.p.sessions.iter().filter(|(_, m)| !pid_alive(m.pid)).map(|(id, _)| id.clone()).collect();
        for id in dead {
            self.end(g, &id);
        }
    }

    fn end(&self, g: &mut Inner, sid: &str) {
        let Some(m) = g.p.sessions.remove(sid) else {
            return;
        };
        let accounts = self.accounts(g).unwrap_or_default();
        if let Some(store_id) = &m.store {
            let shared = g.p.sessions.values().any(|s| s.store.as_ref() == Some(store_id));
            if let (Some(profile), Some(account_id)) = (self.profile_of(g, store_id), &m.session.account_id) {
                if let Ok(_locks) = lock::credentials(&profile.config_dir, LOCK_TIMEOUT) {
                    self.read_back(&accounts, &profile, account_id);
                }
                if !shared && !profile.is_default {
                    profile.destroy(&*self.keychain);
                    g.p.stores.remove(store_id);
                }
            }
        }
        self.log(g, "end", Some(sid), format!("session in {} ended", m.session.cwd));
        self.save(g);
    }

    // ---- accounts -------------------------------------------------------

    pub fn list_accounts(&self) -> Api<Value> {
        let mut g = self.lock();
        self.reap(&mut g);
        let accounts = self.accounts(&g)?;
        let rows: Vec<Value> = accounts
            .iter()
            .map(|a| {
                let sessions: Vec<&String> =
                    g.p.sessions.iter().filter(|(_, m)| m.session.account_id.as_ref() == Some(&a.id)).map(|(id, _)| id).collect();
                let mut row = serde_json::to_value(a).unwrap_or_default();
                row["sessions"] = json!(sessions);
                row["is_default"] = json!(self.holder(&g, a).as_deref() == Some(DEFAULT_STORE));
                row
            })
            .collect();
        Ok(json!(rows))
    }

    /// Add (or refresh) an account from a login that already exists in a
    /// Claude Code profile. llmpool never performs a provider login itself.
    pub fn capture(&self, req: CaptureReq) -> Api<Value> {
        let g = self.lock();
        let profile = match &req.profile_dir {
            Some(dir) => Profile::at(dir),
            None => self.default_profile(),
        };
        let secret = {
            let _locks = lock::credentials(&profile.config_dir, LOCK_TIMEOUT)?;
            profile.read(&*self.keychain)
        }
        .ok_or_else(|| fail(404, format!("no Claude login found in {}", profile.config_dir.display())))?;
        let email = secret.email().unwrap_or_default().to_string();
        let organization = secret.organization().map(str::to_string);
        let id = format!("claude-{}", short_hash(&format!("{email}|{}", organization.as_deref().unwrap_or(""))));

        let mut account = g.store.get(&id)?.unwrap_or_else(|| Account {
            id: id.clone(),
            provider: "claude".into(),
            kind: Kind::Oauth,
            label: email.clone(),
            email: Some(email.clone()),
            organization,
            tags: BTreeSet::new(),
            enabled: true,
            health: Health::Ok,
            usage: None,
            login_expires_at: None,
        });
        account.health = Health::Ok;
        if let Some(label) = req.label {
            account.label = label;
        }
        account.tags.extend(req.tags);
        self.vault.put(&id, &serde_json::to_string(&secret)?)?;
        g.store.put(&account)?;

        // A scratch profile made for this login has done its job. It is
        // removed without logging out, which would revoke the token.
        if !profile.is_default && profile.config_dir.starts_with(&self.data_dir) {
            profile.destroy(&*self.keychain);
        }
        Ok(serde_json::to_value(&account)?)
    }

    pub fn patch_account(&self, name: &str, req: PatchReq) -> Api<Value> {
        let g = self.lock();
        let accounts = g.store.list()?;
        let mut account = find(&accounts, name)?.clone();
        if let Some(label) = req.label {
            account.label = label;
        }
        if let Some(tags) = req.tags {
            account.tags = tags;
        }
        if let Some(enabled) = req.enabled {
            account.enabled = enabled;
        }
        g.store.put(&account)?;
        Ok(serde_json::to_value(&account)?)
    }

    pub fn remove_account(&self, name: &str) -> Api<Value> {
        let g = self.lock();
        let accounts = g.store.list()?;
        let account = find(&accounts, name)?;
        if g.p.sessions.values().any(|m| m.session.account_id.as_ref() == Some(&account.id)) {
            return Err(fail(409, "a running session uses this account; end it or switch it first"));
        }
        self.vault.remove(&account.id)?;
        g.store.remove(&account.id)?;
        Ok(json!({"removed": account.id}))
    }

    // ---- sessions -------------------------------------------------------

    pub fn list_sessions(&self) -> Api<Value> {
        let mut g = self.lock();
        self.reap(&mut g);
        let accounts = self.accounts(&g)?;
        let rows: Vec<Value> = g
            .p
            .sessions
            .values()
            .map(|m| {
                let mut row = serde_json::to_value(m).unwrap_or_default();
                let account = m.session.account_id.as_ref().and_then(|id| accounts.iter().find(|a| &a.id == id));
                row["account_label"] = json!(account.map(|a| &a.label));
                row["usage"] = json!(account.and_then(|a| a.usage.as_ref()));
                row
            })
            .collect();
        Ok(json!(rows))
    }

    fn decide(&self, g: &Inner, policy: &PolicyFile, event: Event, session: &Session, accounts: &[Account]) -> Decision {
        let local = Local::now();
        let home = self.home.to_string_lossy();
        engine::evaluate(
            policy,
            event,
            &Context {
                session,
                accounts,
                now: now(),
                weekday: WEEKDAYS[local.weekday().num_days_from_monday() as usize],
                hour: local.hour() as u8,
                home: &home,
                rr_cursor: g.p.rr_cursor.as_deref(),
            },
        )
    }

    /// Register a session and decide which account it runs as. The reply
    /// carries the environment the launcher must set, or a list to ask the
    /// user to choose from.
    pub fn start_session(&self, req: StartReq) -> Api<Value> {
        let mut g = self.lock();
        self.reap(&mut g);
        let accounts = self.accounts(&g)?;
        g.p.next += 1;
        let sid = format!("s{}", g.p.next);
        let session = Session {
            id: sid.clone(),
            agent: "claude-code".into(),
            provider: "claude".into(),
            cwd: req.cwd,
            git_remote: req.git_remote,
            model: None,
            account_id: None,
            last_swap_at: None,
        };

        let mut rule = None;
        let mut ask = Vec::new();
        let chosen: Option<Account> = if let Some(name) = &req.account {
            Some(find(&accounts, name)?.clone())
        } else if let Some(tag) = &req.tag {
            let target = Target { tag: Some(tag.clone()), ..Target::default() };
            let policy = PolicyFile::default();
            let home = self.home.to_string_lossy();
            let ctx = Context { session: &session, accounts: &accounts, now: now(), weekday: "mon", hour: 0, home: &home, rr_cursor: None };
            let _ = &policy;
            Some(
                select::pick(Pick::MostQuota, select::candidates(&target, &ctx, None), None)
                    .ok_or_else(|| fail(409, format!("no usable account is tagged `{tag}`")))?
                    .clone(),
            )
        } else {
            let policy = file::load(&self.policy_path()).map_err(|e| fail(400, format!("policy file: {e}")))?;
            let decision = self.decide(&g, &policy, Event::SessionStart, &session, &accounts);
            rule = decision.rule;
            match decision.outcome {
                Outcome::Assign(id) => {
                    g.p.rr_cursor = Some(id.clone());
                    accounts.iter().find(|a| a.id == id).cloned()
                }
                Outcome::Ask(ids) => {
                    ask = accounts.iter().filter(|a| ids.contains(&a.id)).cloned().collect();
                    None
                }
                _ => None,
            }
        };

        g.p.sessions.insert(
            sid.clone(),
            Managed { session, pid: req.pid, store: None, agent_session_id: None, started_at: now(), notices: Vec::new(), last_notice: None },
        );
        let env = match &chosen {
            Some(account) => match self.attach(&mut g, &sid, account) {
                Ok(env) => env,
                Err(e) => {
                    g.p.sessions.remove(&sid);
                    return Err(e);
                }
            },
            None => BTreeMap::new(),
        };
        self.save(&g);
        Ok(json!({"session_id": sid, "env": env, "account": chosen, "ask": ask, "rule": rule}))
    }

    /// Put a session on an account before its agent starts.
    pub fn assign(&self, sid: &str, name: &str) -> Api<Value> {
        let mut g = self.lock();
        let accounts = self.accounts(&g)?;
        let account = find(&accounts, name)?.clone();
        match g.p.sessions.get(sid) {
            None => return Err(fail(404, "no such session")),
            Some(m) if m.store.is_some() => return Err(fail(409, "session already has an account; use swap")),
            Some(_) => {}
        }
        let env = self.attach(&mut g, sid, &account)?;
        self.save(&g);
        Ok(json!({"session_id": sid, "env": env, "account": account}))
    }

    fn attach(&self, g: &mut Inner, sid: &str, account: &Account) -> Api<BTreeMap<String, String>> {
        if !account.usable() {
            return Err(fail(409, format!("{} is disabled or needs a new login", account.label)));
        }
        let store_id = match self.holder(g, account) {
            // Already live somewhere: join that profile rather than copy the login.
            Some(id) => id,
            None => {
                let id = format!("st-{sid}");
                let profile = Profile::at(&self.data_dir.join("stores").join(&id));
                profile.write(&*self.keychain, &self.secret_of(&account.id)?)?;
                profile.link_shared(&self.default_profile())?;
                g.p.stores.insert(id.clone(), StoreRec { profile, account_id: account.id.clone() });
                id
            }
        };
        let mut env = BTreeMap::from([("LLMPOOL_SESSION".to_string(), sid.to_string())]);
        if let Some(rec) = g.p.stores.get(&store_id) {
            env.insert("CLAUDE_CONFIG_DIR".into(), rec.profile.config_dir.to_string_lossy().into_owned());
        }
        if let Some(m) = g.p.sessions.get_mut(sid) {
            m.store = Some(store_id);
            m.session.account_id = Some(account.id.clone());
        }
        self.log(g, "assign", Some(sid), format!("session started as {}", account.label));
        Ok(env)
    }

    /// Change the login in a running session's profile. Every session sharing
    /// that profile moves with it.
    fn swap(&self, g: &mut Inner, sid: &str, to_id: &str, reason: &str) -> Api<()> {
        let m = g.p.sessions.get(sid).cloned().ok_or_else(|| fail(404, "no such session"))?;
        let store_id =
            m.store.clone().ok_or_else(|| fail(409, "this session is not managed by llmpool; start it with `llmpool run`"))?;
        if m.session.account_id.as_deref() == Some(to_id) {
            return Err(fail(409, "the session already uses that account"));
        }
        let accounts = self.accounts(g)?;
        let to = find(&accounts, to_id)?;
        if !to.usable() {
            return Err(fail(409, format!("{} is disabled or needs a new login", to.label)));
        }
        if self.holder(g, to).is_some_and(|held| held != store_id) {
            return Err(fail(409, format!("{} is in use by other sessions", to.label)));
        }
        let profile = self.profile_of(g, &store_id).ok_or_else(|| fail(500, "session profile is missing"))?;
        let secret = self.secret_of(&to.id)?;
        {
            let _locks = lock::credentials(&profile.config_dir, LOCK_TIMEOUT)?;
            if let Some(from) = &m.session.account_id {
                self.read_back(&accounts, &profile, from);
            }
            profile.write(&*self.keychain, &secret)?;
        }
        if let Some(rec) = g.p.stores.get_mut(&store_id) {
            rec.account_id = to.id.clone();
        }

        let from = m.session.account_id.as_ref().and_then(|id| accounts.iter().find(|a| &a.id == id));
        let text = format!(
            "llmpool: this session moved from {} to {} ({reason}). The switch takes effect within about a minute. \
             The first message on the new account rebuilds the prompt cache and costs more than usual.",
            from.map_or("its previous account", |a| a.label.as_str()),
            to.label,
        );
        let at = now();
        for s in g.p.sessions.values_mut().filter(|s| s.store.as_ref() == Some(&store_id)) {
            s.session.account_id = Some(to.id.clone());
            s.session.last_swap_at = Some(at);
            s.notices.push(text.clone());
            s.last_notice = Some(text.clone());
        }
        self.log(g, "swap", Some(sid), text);
        self.save(g);
        Ok(())
    }

    pub fn swap_session(&self, sid: &str, name: &str) -> Api<Value> {
        let mut g = self.lock();
        let accounts = self.accounts(&g)?;
        let id = find(&accounts, name)?.id.clone();
        self.swap(&mut g, sid, &id, "switched by hand")?;
        Ok(json!({"session_id": sid, "account": id}))
    }

    pub fn end_session(&self, sid: &str) -> Value {
        self.end(&mut self.lock(), sid);
        json!({})
    }

    /// Evaluate the policy for something that happened in a running session,
    /// and act on the outcome.
    fn react(&self, g: &mut Inner, sid: &str, event: Event) {
        let Some(m) = g.p.sessions.get(sid).cloned() else {
            return;
        };
        if m.store.is_none() {
            return;
        }
        let policy = match file::load(&self.policy_path()) {
            Ok(policy) => policy,
            Err(e) => {
                self.log(g, "policy_error", None, e.to_string());
                return;
            }
        };
        let Ok(all) = self.accounts(g) else {
            return;
        };
        // An account live in another profile cannot also be put in this one.
        let free: Vec<Account> =
            all.iter().filter(|a| self.holder(g, a).is_none_or(|held| Some(&held) == m.store.as_ref())).cloned().collect();
        let decision = self.decide(g, &policy, event, &m.session, &free);
        let rule = decision.rule.clone().unwrap_or_default();
        let label = |id: &String| all.iter().find(|a| &a.id == id).map_or(id.clone(), |a| a.label.clone());

        let notice = match &decision.outcome {
            Outcome::Swap(to) => {
                g.p.rr_cursor = Some(to.clone());
                self.swap(g, sid, to, &format!("rule \"{rule}\""))
                    .err()
                    .map(|e| format!("llmpool: rule \"{rule}\" tried to switch account but could not: {}", e.message))
            }
            Outcome::Ask(ids) => Some(format!(
                "llmpool: rule \"{rule}\" suggests switching account. Candidates: {}. Run `llmpool switch <account>`.",
                ids.iter().map(label).collect::<Vec<_>>().join(", ")
            )),
            Outcome::Notify => Some(format!("llmpool: rule \"{rule}\" fired.")),
            Outcome::Exhausted => {
                Some(format!("llmpool: rule \"{rule}\" wanted to switch account, but no other account is available."))
            }
            _ => None,
        };
        if let Some(text) = notice {
            let fresh = g.p.sessions.get_mut(sid).is_some_and(|s| {
                let fresh = s.last_notice.as_ref() != Some(&text);
                if fresh {
                    s.notices.push(text.clone());
                    s.last_notice = Some(text.clone());
                }
                fresh
            });
            if fresh {
                self.log(g, "notice", Some(sid), text);
            }
        }
        self.save(g);
    }

    // ---- signals from the agent -----------------------------------------

    /// Handle a Claude Code hook. The return value is printed by the hook
    /// command as its JSON output.
    pub fn hook(&self, sid: &str, event: &str, argument: &str, payload: &Value) -> Value {
        let mut g = self.lock();
        let Some(m) = g.p.sessions.get_mut(sid) else {
            return json!({});
        };
        match (event, argument) {
            ("SessionStart", _) => {
                m.agent_session_id = payload["session_id"].as_str().map(str::to_string);
                self.save(&g);
            }
            ("UserPromptSubmit", _) => {
                let notices = std::mem::take(&mut m.notices);
                self.save(&g);
                if !notices.is_empty() {
                    let text = notices.join("\n");
                    return json!({
                        "systemMessage": text,
                        "hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": text},
                    });
                }
            }
            ("CwdChanged", _) => {
                if let Some(cwd) = payload["cwd"].as_str() {
                    m.session.cwd = cwd.to_string();
                }
                self.react(&mut g, sid, Event::CwdChanged);
            }
            ("StopFailure", "rate_limit") => self.react(&mut g, sid, Event::RateLimited),
            ("StopFailure", "authentication_failed") => self.react(&mut g, sid, Event::AuthFailed),
            _ => {}
        }
        json!({})
    }

    /// Take a usage reading from Claude Code's status line input.
    pub fn statusline(&self, sid: &str, payload: &Value) -> Value {
        let mut g = self.lock();
        let t = now();
        let Some(m) = g.p.sessions.get_mut(sid) else {
            return json!({});
        };
        if let Some(model) = statusline::model(payload) {
            m.session.model = Some(model);
        }
        let settled = m.session.last_swap_at.is_none_or(|at| t - at >= SETTLE_SECS);
        let account_id = m.session.account_id.clone();

        let mut line = "llmpool".to_string();
        if let Some(mut account) = account_id.and_then(|id| g.store.get(&id).ok().flatten()) {
            if settled {
                if let Some(usage) = statusline::usage(payload, t) {
                    account.usage = Some(usage);
                    let _ = g.store.put(&account);
                }
            }
            line = format!("llmpool: {}", account.label);
            for window in ["5h", "7d"] {
                if account.usage.as_ref().is_some_and(|u| u.windows.contains_key(window)) {
                    line += &format!(" · {window} {:.0}%", account.used(window));
                }
            }
        }
        if settled {
            self.react(&mut g, sid, Event::UsageUpdated);
        } else {
            self.save(&g);
        }
        json!({"line": line})
    }

    // ---- policies ---------------------------------------------------------

    /// The policy as text, and parsed for editors (`null` when the file on
    /// disk does not parse).
    pub fn policy_text(&self) -> Value {
        let path = self.policy_path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        json!({"policy": PolicyFile::parse(&text).ok(), "text": text, "path": path})
    }

    /// Structured form of policy text an editor holds, validated but not saved.
    pub fn parse_policy(&self, text: &str) -> Api<Value> {
        let policy = file::checked(text).map_err(|e| fail(400, e.to_string()))?;
        Ok(json!({"policy": policy}))
    }

    /// TOML for a policy an editor holds as structured data.
    pub fn render_policy(&self, policy: Value) -> Api<Value> {
        let policy: PolicyFile = serde_json::from_value(policy).map_err(|e| fail(400, e.to_string()))?;
        Ok(json!({"text": policy.to_toml()}))
    }

    pub fn put_policy(&self, text: &str) -> Api<Value> {
        let _g = self.lock();
        let policy = file::save(&self.policy_path(), text).map_err(|e| fail(400, e.to_string()))?;
        Ok(json!({"rules": policy.rules.len()}))
    }

    /// Which rule would fire, and what it would do, without doing it.
    pub fn dry_run(&self, req: DryRunReq) -> Api<Value> {
        let g = self.lock();
        let policy = match &req.text {
            Some(text) => file::checked(text),
            None => file::load(&self.policy_path()),
        }
        .map_err(|e| fail(400, e.to_string()))?;
        let session = match &req.session_id {
            Some(id) => g.p.sessions.get(id).ok_or_else(|| fail(404, "no such session"))?.session.clone(),
            None => Session {
                id: "preview".into(),
                agent: "claude-code".into(),
                provider: "claude".into(),
                cwd: req.cwd.unwrap_or_else(|| self.home.to_string_lossy().into_owned()),
                git_remote: None,
                model: None,
                account_id: None,
                last_swap_at: None,
            },
        };
        let accounts = self.accounts(&g)?;
        let decision = self.decide(&g, &policy, req.event, &session, &accounts);
        let labels: BTreeMap<&String, &String> = accounts.iter().map(|a| (&a.id, &a.label)).collect();
        Ok(json!({"decision": decision, "labels": labels}))
    }

    /// What a rule can be made of, for editors.
    pub fn schema(&self) -> Value {
        json!({
            "triggers": triggers::all().iter().map(|t| json!({"kind": t.kind(), "fields": t.schema()})).collect::<Vec<_>>(),
            "conditions": ["cwd", "git_remote", "model", "account_tag"],
            "actions": ["assign", "swap", "ask", "notify", "hold"],
            "picks": ["most_quota", "soonest_reset", "round_robin"],
        })
    }

    pub fn events(&self) -> Value {
        json!(self.lock().p.events)
    }
}
