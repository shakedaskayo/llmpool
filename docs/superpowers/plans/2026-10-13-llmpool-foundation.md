# llmpool Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `llmpool-core` library (pool model, account store, vault, policy engine) and a `llmpool` binary that validates and dry-runs policy files.

**Architecture:** A Cargo workspace with two crates. `llmpool-core` is pure logic plus two small storage wrappers (SQLite for account metadata, OS keychain for secrets). The policy engine is a pure function from (policy, event, context) to a decision, so it is tested without any agent, daemon or network. `llmpool-cli` is a thin `clap` front end.

**Tech Stack:** Rust 1.95, serde, toml 0.8, globset, rusqlite (bundled SQLite), keyring 3, clap 4, thiserror, tempfile (tests).

**Spec:** `docs/superpowers/specs/2026-10-13-llmpool-core-design.md`

This is plan 1 of 3. Plan 2 (Claude runtime) covers the Claude Code adapter, daemon, `run`/`switch`, installer, statusline and the `schedule` trigger, which needs the daemon's ticker. Plan 3 covers the Tauri GUI.

## Global Constraints

- Crate names: `llmpool-core`, `llmpool-cli`. Binary name: `llmpool`.
- Keychain service name for secrets: `llmpool`, one item per account id.
- Secrets never enter SQLite, logs, error messages or serialized output.
- Policy evaluation: rules in file order, first rule whose trigger matches and whose conditions hold wins.
- Swap guard defaults: cooldown 300 seconds per session, margin 10 percentage points.
- Candidates are always enabled accounts with health `ok`, same provider as the session, and not the session's current account.
- An invalid policy file is rejected on write and the previous file stays in place.
- No real people's names in any committed file.
- Tests must not touch the real OS keychain unless marked `#[ignore]`.

## Review Focus

1. An account that has never been polled (no usage reading) must still be a swap candidate and must not panic or be ranked last. Test in Task 5.
2. A rule that names a tag no account carries must yield `exhausted`, not a panic or a silent no-op. Test in Task 6.
3. A `cwd` glob like `~/work/acme/**` must match the root directory itself, a trailing slash, and a `~`-relative path. Test in Task 4.
4. A missing or empty policy file is an empty policy, not an error. Test in Task 7.
5. Saving a policy with a syntax error must report a line number and leave the previous file byte-for-byte intact. Test in Task 7.

## File Structure

```
Cargo.toml                              workspace
.gitignore
crates/llmpool-core/
  Cargo.toml
  src/lib.rs                            module list
  src/model.rs                          Account, Session, Usage, Kind, Health
  src/fixtures.rs                       test-only builders
  src/policy/mod.rs                     module list, PolicyError
  src/policy/rule.rs                    PolicyFile, Rule, TriggerSpec, Conditions, ActionSpec; TOML parse
  src/policy/context.rs                 Event, Context
  src/policy/triggers.rs                Trigger trait, built-in triggers, registry
  src/policy/conditions.rs              Conditions::check, Conditions::hold
  src/policy/select.rs                  candidates, pick
  src/policy/engine.rs                  evaluate, Decision, Outcome, Scenario
  src/policy/validate.rs                validate
  src/policy/file.rs                    load, checked, save
  src/store.rs                          Store (SQLite account metadata)
  src/vault.rs                          Vault trait, MemoryVault, KeychainVault
crates/llmpool-cli/
  Cargo.toml
  src/main.rs                           `llmpool policy check|dry-run`
```

---

### Task 1: Workspace and pool model

**Files:**
- Create: `Cargo.toml`, `.gitignore`
- Create: `crates/llmpool-core/Cargo.toml`
- Create: `crates/llmpool-core/src/lib.rs`
- Create: `crates/llmpool-core/src/model.rs`
- Create: `crates/llmpool-core/src/fixtures.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `model::{AccountId, Kind, Health, Window, Usage, Account, Session}`; `Account::used(&self, window: &str) -> f64`, `Account::reset(&self, window: &str) -> Option<i64>`, `Account::peak_used(&self) -> f64`, `Account::usable(&self) -> bool`; test-only `fixtures::account(id: &str, tags: &[&str], used_5h: Option<f64>) -> Account` and `fixtures::session(cwd: &str, account: Option<&str>) -> Session`.

- [ ] **Step 1: Create the workspace files**

`Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/llmpool-core"]

[workspace.package]
version = "0.1.0"
edition = "2021"
```

`.gitignore`:

```
target/
```

`crates/llmpool-core/Cargo.toml`:

```toml
[package]
name = "llmpool-core"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

`crates/llmpool-core/src/lib.rs`:

```rust
pub mod model;

#[cfg(test)]
mod fixtures;
```

- [ ] **Step 2: Write the fixtures and the failing tests**

`crates/llmpool-core/src/fixtures.rs`:

```rust
use crate::model::{Account, Health, Kind, Session, Usage, Window};

pub fn account(id: &str, tags: &[&str], used_5h: Option<f64>) -> Account {
    Account {
        id: id.into(),
        provider: "claude".into(),
        kind: Kind::Oauth,
        label: id.into(),
        email: None,
        organization: None,
        tags: tags.iter().map(|t| t.to_string()).collect(),
        enabled: true,
        health: Health::Ok,
        usage: used_5h.map(|p| Usage {
            windows: [("5h".to_string(), Window { used_percent: p, resets_at: None })].into(),
            fetched_at: 0,
        }),
        login_expires_at: None,
    }
}

pub fn session(cwd: &str, account: Option<&str>) -> Session {
    Session {
        id: "s1".into(),
        agent: "claude-code".into(),
        provider: "claude".into(),
        cwd: cwd.into(),
        git_remote: None,
        model: None,
        account_id: account.map(Into::into),
        last_swap_at: None,
    }
}
```

`crates/llmpool-core/src/model.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::account;

    #[test]
    fn account_round_trips_through_json() {
        let a = account("a1", &["work"], Some(40.0));
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"kind\":\"oauth\""));
        assert_eq!(serde_json::from_str::<Account>(&json).unwrap(), a);
    }

    #[test]
    fn unknown_usage_counts_as_unused() {
        let a = account("a1", &[], None);
        assert_eq!(a.used("5h"), 0.0);
        assert_eq!(a.peak_used(), 0.0);
        assert_eq!(a.reset("7d"), None);
    }

    #[test]
    fn disabled_or_unhealthy_is_not_usable() {
        let mut a = account("a1", &[], None);
        assert!(a.usable());
        a.enabled = false;
        assert!(!a.usable());
        a.enabled = true;
        a.health = Health::ReloginRequired;
        assert!(!a.usable());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core`
Expected: compile error, `cannot find type Account in this scope`.

- [ ] **Step 4: Write the model**

Put this above the `tests` module in `crates/llmpool-core/src/model.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type AccountId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Oauth,
    ApiKey,
    Cloud,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    ReloginRequired,
    Quarantined,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used_percent: f64,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Keyed by window name: "5h", "7d", or a model name.
    pub windows: BTreeMap<String, Window>,
    pub fetched_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub provider: String,
    pub kind: Kind,
    pub label: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub tags: BTreeSet<String>,
    pub enabled: bool,
    pub health: Health,
    pub usage: Option<Usage>,
    pub login_expires_at: Option<i64>,
}

impl Account {
    fn window(&self, name: &str) -> Option<&Window> {
        self.usage.as_ref()?.windows.get(name)
    }

    /// Percent used in a window. An account with no reading counts as unused.
    pub fn used(&self, window: &str) -> f64 {
        self.window(window).map_or(0.0, |w| w.used_percent)
    }

    pub fn reset(&self, window: &str) -> Option<i64> {
        self.window(window)?.resets_at
    }

    /// Highest percent used across all windows.
    pub fn peak_used(&self) -> f64 {
        self.usage
            .as_ref()
            .map_or(0.0, |u| u.windows.values().map(|w| w.used_percent).fold(0.0, f64::max))
    }

    pub fn usable(&self) -> bool {
        self.enabled && self.health == Health::Ok
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub provider: String,
    pub cwd: String,
    pub git_remote: Option<String>,
    pub model: Option<String>,
    pub account_id: Option<AccountId>,
    /// Epoch seconds of the last swap, for the cooldown guard.
    pub last_swap_at: Option<i64>,
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore crates/llmpool-core
git commit -m "feat(core): workspace and pool model"
```

---

### Task 2: Policy file types and TOML parsing

**Files:**
- Modify: `crates/llmpool-core/Cargo.toml`
- Modify: `crates/llmpool-core/src/lib.rs`
- Create: `crates/llmpool-core/src/policy/mod.rs`
- Create: `crates/llmpool-core/src/policy/rule.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `policy::PolicyError` (`Parse(String)`, `Invalid(Vec<String>)`, `Io(std::io::Error)`); `policy::rule::{PolicyFile, Settings, Rule, TriggerSpec, Conditions, TimeRange, ActionSpec, Target, Pick}`; `PolicyFile::parse(text: &str) -> Result<PolicyFile, PolicyError>`; `PolicyFile::to_toml(&self) -> String`. Field names: `PolicyFile { settings, rules }`, `Settings { cooldown_secs: i64, margin_points: f64 }`, `Rule { name, enabled, on: TriggerSpec, when: Conditions, action: ActionSpec }`, `TriggerSpec { kind: String, params: toml::Table }`, `Conditions { cwd, git_remote, account_tag, model: Option<String>, time: Option<TimeRange> }`, `TimeRange { days: Vec<String>, from_hour: u8, to_hour: u8 }`, `ActionSpec { assign, swap, ask: Option<Target>, notify: bool, hold: bool }`, `Target { account, tag: Option<String>, pick: Pick }`, `Pick::{MostQuota, SoonestReset, RoundRobin}`.

- [ ] **Step 1: Add dependencies and modules**

In `crates/llmpool-core/Cargo.toml` under `[dependencies]` add:

```toml
toml = "0.8"
thiserror = "1"
```

`crates/llmpool-core/src/lib.rs`:

```rust
pub mod model;
pub mod policy;

#[cfg(test)]
mod fixtures;
```

`crates/llmpool-core/src/policy/mod.rs`:

```rust
pub mod rule;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    Parse(String),
    #[error("{}", .0.join("; "))]
    Invalid(Vec<String>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-core/src/policy/rule.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[rule]]
name = "acme repos use acme seats"
on = "session_start"
when = { cwd = "~/work/acme/**" }
do = { assign = { tag = "acme", pick = "most_quota" } }

[[rule]]
name = "swap before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same" }, notify = true }
"#;

    #[test]
    fn parses_both_trigger_forms() {
        let p = PolicyFile::parse(SAMPLE).unwrap();
        assert_eq!(p.settings, Settings { cooldown_secs: 300, margin_points: 10.0 });
        assert_eq!(p.rules[0].on.kind, "session_start");
        assert!(p.rules[0].on.params.is_empty());
        assert!(p.rules[0].enabled);
        assert_eq!(p.rules[0].when.cwd.as_deref(), Some("~/work/acme/**"));
        assert_eq!(p.rules[0].action.assign.as_ref().unwrap().pick, Pick::MostQuota);
        assert_eq!(p.rules[1].on.kind, "usage_above");
        assert_eq!(p.rules[1].on.params["percent"].as_integer(), Some(90));
        assert!(p.rules[1].action.notify);
        assert_eq!(p.rules[1].action.swap.as_ref().unwrap().tag.as_deref(), Some("same"));
    }

    #[test]
    fn round_trips_through_toml() {
        let p = PolicyFile::parse(SAMPLE).unwrap();
        assert_eq!(PolicyFile::parse(&p.to_toml()).unwrap(), p);
    }

    #[test]
    fn unknown_condition_key_is_an_error_with_a_line() {
        let text = "[[rule]]\nname = 'x'\non = 'session_start'\nwhen = { colour = 'red' }\ndo = { hold = true }\n";
        let err = PolicyFile::parse(text).unwrap_err().to_string();
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("colour"), "{err}");
    }

    #[test]
    fn trigger_table_with_two_keys_is_an_error() {
        let text = "[[rule]]\nname = 'x'\non = { rate_limited = {}, auth_failed = {} }\ndo = { hold = true }\n";
        assert!(PolicyFile::parse(text).is_err());
    }

    #[test]
    fn empty_text_is_an_empty_policy() {
        assert_eq!(PolicyFile::parse("").unwrap(), PolicyFile::default());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core policy::rule`
Expected: compile error, `cannot find type PolicyFile`.

- [ ] **Step 4: Write the types**

Put this above the `tests` module in `crates/llmpool-core/src/policy/rule.rs`:

```rust
use super::PolicyError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFile {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

impl PolicyFile {
    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        toml::from_str(text).map_err(|e| PolicyError::Parse(e.to_string()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("policy types always serialize")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    pub cooldown_secs: i64,
    pub margin_points: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { cooldown_secs: 300, margin_points: 10.0 }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub on: TriggerSpec,
    #[serde(default, skip_serializing_if = "Conditions::is_empty")]
    pub when: Conditions,
    #[serde(rename = "do")]
    pub action: ActionSpec,
}

/// `on = "session_start"` or `on = { usage_above = { percent = 90 } }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "OnRepr", into = "OnRepr")]
pub struct TriggerSpec {
    pub kind: String,
    pub params: toml::Table,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum OnRepr {
    Name(String),
    Table(BTreeMap<String, toml::Table>),
}

impl TryFrom<OnRepr> for TriggerSpec {
    type Error = String;

    fn try_from(repr: OnRepr) -> Result<Self, String> {
        match repr {
            OnRepr::Name(kind) => Ok(Self { kind, params: toml::Table::new() }),
            OnRepr::Table(mut table) if table.len() == 1 => {
                let (kind, params) = table.pop_first().expect("length checked");
                Ok(Self { kind, params })
            }
            OnRepr::Table(_) => Err("`on` must name exactly one trigger".into()),
        }
    }
}

impl From<TriggerSpec> for OnRepr {
    fn from(spec: TriggerSpec) -> Self {
        if spec.params.is_empty() {
            OnRepr::Name(spec.kind)
        } else {
            OnRepr::Table([(spec.kind, spec.params)].into())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conditions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeRange>,
}

impl Conditions {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn end_of_day() -> u8 {
    24
}

/// Holds when the weekday is listed (or `days` is empty) and
/// `from_hour <= hour < to_hour`, in local time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    #[serde(default)]
    pub days: Vec<String>,
    #[serde(default)]
    pub from_hour: u8,
    #[serde(default = "end_of_day")]
    pub to_hour: u8,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Target>,
    #[serde(default)]
    pub notify: bool,
    #[serde(default)]
    pub hold: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Target {
    /// Account label or id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// A tag, or "same" for the tags of the session's current account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub pick: Pick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    #[default]
    MostQuota,
    SoonestReset,
    RoundRobin,
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core policy::rule`
Expected: `test result: ok. 5 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/llmpool-core Cargo.lock
git commit -m "feat(core): policy file types and TOML parsing"
```

---

### Task 3: Events, context and triggers

**Files:**
- Modify: `crates/llmpool-core/src/policy/mod.rs`
- Create: `crates/llmpool-core/src/policy/context.rs`
- Create: `crates/llmpool-core/src/policy/triggers.rs`
- Modify: `crates/llmpool-core/src/fixtures.rs`

**Interfaces:**
- Consumes: `model::{Account, Session}`.
- Produces: `policy::context::Event` (`SessionStart`, `UsageUpdated`, `RateLimited`, `AuthFailed`, `CwdChanged`; `Copy`, serde snake_case); `policy::context::Context<'a> { session: &'a Session, accounts: &'a [Account], now: i64, weekday: &'a str, hour: u8, home: &'a str, rr_cursor: Option<&'a str> }` with `Context::current(&self) -> Option<&'a Account>`; `policy::triggers::Trigger` trait with `kind(&self) -> &'static str`, `matches(&self, params: &toml::Table, event: Event, ctx: &Context) -> bool`, `schema(&self) -> ParamSchema`, `check(&self, params: &toml::Table) -> Result<(), String>`, `threshold(&self, params: &toml::Table) -> Option<(String, f64)>`; `triggers::all() -> Vec<Box<dyn Trigger>>`; `triggers::find(kind: &str) -> Option<Box<dyn Trigger>>`; `triggers::{ParamSchema, Field, FieldKind}`; test-only `fixtures::ctx(session: &'a Session, accounts: &'a [Account]) -> Context<'a>` (weekday `"tue"`, hour `10`, home `"/home/u"`, now `1_000_000`).

- [ ] **Step 1: Add modules and the context fixture**

`crates/llmpool-core/src/policy/mod.rs`, replace the module list at the top with:

```rust
pub mod context;
pub mod rule;
pub mod triggers;
```

Append to `crates/llmpool-core/src/fixtures.rs`:

```rust
use crate::policy::context::Context;

pub fn ctx<'a>(session: &'a Session, accounts: &'a [Account]) -> Context<'a> {
    Context {
        session,
        accounts,
        now: 1_000_000,
        weekday: "tue",
        hour: 10,
        home: "/home/u",
        rr_cursor: None,
    }
}
```

- [ ] **Step 2: Write the context types**

`crates/llmpool-core/src/policy/context.rs`:

```rust
use crate::model::{Account, Session};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    SessionStart,
    /// A new usage reading arrived for the session's current account.
    UsageUpdated,
    RateLimited,
    AuthFailed,
    CwdChanged,
}

/// Everything a rule may look at. Local time is supplied by the caller so the
/// engine stays free of clocks and time zones.
pub struct Context<'a> {
    pub session: &'a Session,
    pub accounts: &'a [Account],
    /// Epoch seconds.
    pub now: i64,
    /// "mon" .. "sun", local time.
    pub weekday: &'a str,
    /// 0 .. 23, local time.
    pub hour: u8,
    /// Home directory, for `~` in path globs.
    pub home: &'a str,
    /// Id of the account round-robin picked last, if any.
    pub rr_cursor: Option<&'a str>,
}

impl<'a> Context<'a> {
    pub fn current(&self) -> Option<&'a Account> {
        let id = self.session.account_id.as_ref()?;
        self.accounts.iter().find(|a| &a.id == id)
    }
}
```

- [ ] **Step 3: Write the failing tests**

`crates/llmpool-core/src/policy/triggers.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{account, ctx, session};

    fn params(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn event_triggers_match_only_their_event() {
        let s = session("/w", None);
        let c = ctx(&s, &[]);
        let t = find("rate_limited").unwrap();
        assert!(t.matches(&toml::Table::new(), Event::RateLimited, &c));
        assert!(!t.matches(&toml::Table::new(), Event::AuthFailed, &c));
        assert!(t.check(&params("extra = 1")).is_err());
    }

    #[test]
    fn usage_above_reads_the_current_account() {
        let accounts = [account("a", &[], Some(91.0))];
        let s = session("/w", Some("a"));
        let c = ctx(&s, &accounts);
        let t = find("usage_above").unwrap();
        assert!(t.matches(&params("window = '5h'\npercent = 90"), Event::UsageUpdated, &c));
        assert!(t.matches(&params("percent = 90"), Event::UsageUpdated, &c));
        assert!(!t.matches(&params("percent = 95"), Event::UsageUpdated, &c));
        assert!(!t.matches(&params("percent = 90"), Event::RateLimited, &c));
        assert_eq!(t.threshold(&params("percent = 90")), Some(("5h".to_string(), 90.0)));
    }

    #[test]
    fn usage_above_without_a_current_account_never_matches() {
        let s = session("/w", None);
        let c = ctx(&s, &[]);
        let t = find("usage_above").unwrap();
        assert!(!t.matches(&params("percent = 1"), Event::UsageUpdated, &c));
    }

    #[test]
    fn usage_above_rejects_bad_percent() {
        let t = find("usage_above").unwrap();
        assert!(t.check(&params("percent = 90")).is_ok());
        assert!(t.check(&params("percent = 150")).is_err());
        assert!(t.check(&params("percent = 0")).is_err());
        assert!(t.check(&params("window = '5h'")).is_err());
    }

    #[test]
    fn every_kind_is_unique_and_findable() {
        let kinds: Vec<_> = all().iter().map(|t| t.kind()).collect();
        let mut unique = kinds.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), kinds.len());
        for kind in ["session_start", "usage_above", "rate_limited", "auth_failed", "cwd_changed"] {
            assert!(find(kind).is_some(), "{kind}");
        }
        assert!(find("nope").is_none());
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core policy::triggers`
Expected: compile error, `cannot find function find`.

- [ ] **Step 5: Write the triggers**

Put this above the `tests` module in `crates/llmpool-core/src/policy/triggers.rs`:

```rust
use super::context::{Context, Event};
use serde::Serialize;

/// Describes a trigger's parameters so a GUI can render an editor for them.
pub type ParamSchema = Vec<Field>;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Percent,
}

/// To add a trigger: implement this and add one line to `all()`.
pub trait Trigger: Send + Sync {
    /// Name used in a rule's `on` field.
    fn kind(&self) -> &'static str;

    fn matches(&self, params: &toml::Table, event: Event, ctx: &Context) -> bool;

    fn schema(&self) -> ParamSchema {
        Vec::new()
    }

    /// Validate a rule's parameters for this trigger.
    fn check(&self, params: &toml::Table) -> Result<(), String> {
        match params.keys().next() {
            Some(key) => Err(format!("`{}` takes no parameter `{key}`", self.kind())),
            None => Ok(()),
        }
    }

    /// The (window, percent) this trigger fires at, if it is usage driven.
    /// The engine uses it to filter and rank swap candidates.
    fn threshold(&self, _params: &toml::Table) -> Option<(String, f64)> {
        None
    }
}

pub fn all() -> Vec<Box<dyn Trigger>> {
    vec![
        Box::new(OnEvent("session_start", Event::SessionStart)),
        Box::new(OnEvent("rate_limited", Event::RateLimited)),
        Box::new(OnEvent("auth_failed", Event::AuthFailed)),
        Box::new(OnEvent("cwd_changed", Event::CwdChanged)),
        Box::new(UsageAbove),
    ]
}

pub fn find(kind: &str) -> Option<Box<dyn Trigger>> {
    all().into_iter().find(|t| t.kind() == kind)
}

/// Fires whenever its event occurs.
struct OnEvent(&'static str, Event);

impl Trigger for OnEvent {
    fn kind(&self) -> &'static str {
        self.0
    }

    fn matches(&self, _params: &toml::Table, event: Event, _ctx: &Context) -> bool {
        event == self.1
    }
}

struct UsageAbove;

impl UsageAbove {
    fn parse(params: &toml::Table) -> Result<(String, f64), String> {
        if let Some(key) = params.keys().find(|k| !matches!(k.as_str(), "window" | "percent")) {
            return Err(format!("`usage_above` takes no parameter `{key}`"));
        }
        let window = match params.get("window") {
            None => "5h".to_string(),
            Some(toml::Value::String(s)) => s.clone(),
            Some(_) => return Err("`usage_above.window` must be text".into()),
        };
        let percent = match params.get("percent") {
            Some(toml::Value::Integer(i)) => *i as f64,
            Some(toml::Value::Float(f)) => *f,
            _ => return Err("`usage_above` needs a numeric `percent`".into()),
        };
        if !(1.0..=100.0).contains(&percent) {
            return Err(format!("`usage_above.percent` must be 1 to 100, got {percent}"));
        }
        Ok((window, percent))
    }
}

impl Trigger for UsageAbove {
    fn kind(&self) -> &'static str {
        "usage_above"
    }

    fn matches(&self, params: &toml::Table, event: Event, ctx: &Context) -> bool {
        let (Event::UsageUpdated, Ok((window, percent)), Some(current)) =
            (event, Self::parse(params), ctx.current())
        else {
            return false;
        };
        current.used(&window) >= percent
    }

    fn schema(&self) -> ParamSchema {
        vec![
            Field { name: "window", kind: FieldKind::Text },
            Field { name: "percent", kind: FieldKind::Percent },
        ]
    }

    fn check(&self, params: &toml::Table) -> Result<(), String> {
        Self::parse(params).map(|_| ())
    }

    fn threshold(&self, params: &toml::Table) -> Option<(String, f64)> {
        Self::parse(params).ok()
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core policy::triggers`
Expected: `test result: ok. 5 passed`.

- [ ] **Step 7: Commit**

```bash
git add crates/llmpool-core
git commit -m "feat(core): policy events, context and trigger registry"
```

---

### Task 4: Conditions

**Files:**
- Modify: `crates/llmpool-core/Cargo.toml`
- Modify: `crates/llmpool-core/src/policy/mod.rs`
- Create: `crates/llmpool-core/src/policy/conditions.rs`

**Interfaces:**
- Consumes: `rule::{Conditions, TimeRange}`, `context::Context`, `fixtures::{account, session, ctx}`.
- Produces: `Conditions::check(&self) -> Result<(), String>`; `Conditions::hold(&self, ctx: &Context) -> bool`.

- [ ] **Step 1: Add the dependency and module**

In `crates/llmpool-core/Cargo.toml` under `[dependencies]` add:

```toml
globset = "0.4"
```

In `crates/llmpool-core/src/policy/mod.rs` add `pub mod conditions;` to the module list (keep it alphabetical).

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-core/src/policy/conditions.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use crate::fixtures::{account, ctx, session};
    use crate::policy::rule::Conditions;

    fn when(text: &str) -> Conditions {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn empty_conditions_always_hold() {
        let s = session("/anywhere", None);
        assert!(Conditions::default().hold(&ctx(&s, &[])));
    }

    #[test]
    fn cwd_glob_matches_root_trailing_slash_children_and_tilde() {
        let c = when("cwd = '~/work/acme/**'");
        for cwd in [
            "/home/u/work/acme",
            "/home/u/work/acme/",
            "/home/u/work/acme/api/src",
            "~/work/acme/api",
        ] {
            let s = session(cwd, None);
            assert!(c.hold(&ctx(&s, &[])), "{cwd} should match");
        }
        for cwd in ["/home/u/work/acme-old", "/home/u/work", "/elsewhere/work/acme"] {
            let s = session(cwd, None);
            assert!(!c.hold(&ctx(&s, &[])), "{cwd} should not match");
        }
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        let c = when("cwd = '/src/*'");
        assert!(c.hold(&ctx(&session("/src/api", None), &[])));
        assert!(!c.hold(&ctx(&session("/src/api/deep", None), &[])));
    }

    #[test]
    fn account_tag_needs_a_current_account_with_that_tag() {
        let c = when("account_tag = 'work'");
        let accounts = [account("a", &["work"], None), account("b", &["home"], None)];
        assert!(c.hold(&ctx(&session("/w", Some("a")), &accounts)));
        assert!(!c.hold(&ctx(&session("/w", Some("b")), &accounts)));
        assert!(!c.hold(&ctx(&session("/w", None), &accounts)));
    }

    #[test]
    fn git_remote_and_model_globs() {
        let mut s = session("/w", None);
        assert!(!when("git_remote = '*acme/*'").hold(&ctx(&s, &[])));
        s.git_remote = Some("git@github.com:acme/api.git".into());
        s.model = Some("claude-opus-5-5".into());
        assert!(when("git_remote = '*acme/*'").hold(&ctx(&s, &[])));
        assert!(when("model = '*opus*'").hold(&ctx(&s, &[])));
        assert!(!when("model = '*haiku*'").hold(&ctx(&s, &[])));
    }

    #[test]
    fn time_window_uses_weekday_and_hour() {
        // The fixture context is Tuesday, 10:00.
        let s = session("/w", None);
        let c = ctx(&s, &[]);
        assert!(when("time = { days = ['mon', 'tue'], from_hour = 9, to_hour = 18 }").hold(&c));
        assert!(when("time = { from_hour = 10 }").hold(&c));
        assert!(!when("time = { days = ['sat'] }").hold(&c));
        assert!(!when("time = { from_hour = 11 }").hold(&c));
        assert!(!when("time = { to_hour = 10 }").hold(&c));
    }

    #[test]
    fn all_conditions_must_hold() {
        let c = when("cwd = '/w/**'\naccount_tag = 'work'");
        let accounts = [account("a", &["home"], None)];
        assert!(!c.hold(&ctx(&session("/w/x", Some("a")), &accounts)));
    }

    #[test]
    fn check_rejects_bad_glob_day_and_hours() {
        assert!(when("cwd = '~/ok/**'").check().is_ok());
        assert!(when("cwd = '['").check().is_err());
        assert!(when("time = { days = ['funday'] }").check().is_err());
        assert!(when("time = { to_hour = 30 }").check().is_err());
        assert!(when("time = { from_hour = 18, to_hour = 9 }").check().is_err());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core policy::conditions`
Expected: compile error, `no method named hold found for struct Conditions`.

- [ ] **Step 4: Write the implementation**

Put this above the `tests` module in `crates/llmpool-core/src/policy/conditions.rs`:

```rust
use super::context::Context;
use super::rule::Conditions;
use globset::{Glob, GlobBuilder, GlobMatcher};

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Glob over free text such as a git remote or model name: `*` matches anything.
fn text_glob(pattern: &str) -> Result<GlobMatcher, String> {
    Glob::new(pattern).map(|g| g.compile_matcher()).map_err(|e| e.to_string())
}

/// Glob over a path: `*` stops at `/`, `**` crosses directories.
fn path_glob(pattern: &str) -> Result<GlobMatcher, String> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| e.to_string())
}

fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => path.to_string(),
    }
}

fn cwd_matches(pattern: &str, cwd: &str, home: &str) -> bool {
    let pattern = expand_home(pattern, home);
    let cwd = expand_home(cwd, home);
    let cwd = cwd.trim_end_matches('/');
    // `dir/**` should also cover `dir` itself.
    if pattern.strip_suffix("/**") == Some(cwd) {
        return true;
    }
    path_glob(&pattern).is_ok_and(|g| g.is_match(cwd))
}

fn text_matches(pattern: &str, value: Option<&str>) -> bool {
    value.is_some_and(|v| text_glob(pattern).is_ok_and(|g| g.is_match(v)))
}

impl Conditions {
    pub fn check(&self) -> Result<(), String> {
        if let Some(p) = &self.cwd {
            path_glob(p).map_err(|e| format!("`when.cwd`: {e}"))?;
        }
        if let Some(p) = &self.git_remote {
            text_glob(p).map_err(|e| format!("`when.git_remote`: {e}"))?;
        }
        if let Some(p) = &self.model {
            text_glob(p).map_err(|e| format!("`when.model`: {e}"))?;
        }
        if let Some(t) = &self.time {
            if let Some(day) = t.days.iter().find(|d| !DAYS.contains(&d.as_str())) {
                return Err(format!("`when.time.days`: unknown day `{day}`, use mon..sun"));
            }
            if t.to_hour > 24 || t.from_hour >= t.to_hour {
                return Err("`when.time`: need from_hour < to_hour <= 24".into());
            }
        }
        Ok(())
    }

    /// True when every condition that is set holds.
    pub fn hold(&self, ctx: &Context) -> bool {
        let s = ctx.session;
        self.cwd.as_deref().is_none_or(|p| cwd_matches(p, &s.cwd, ctx.home))
            && self.git_remote.as_deref().is_none_or(|p| text_matches(p, s.git_remote.as_deref()))
            && self.model.as_deref().is_none_or(|p| text_matches(p, s.model.as_deref()))
            && self
                .account_tag
                .as_ref()
                .is_none_or(|tag| ctx.current().is_some_and(|a| a.tags.contains(tag)))
            && self.time.as_ref().is_none_or(|t| {
                (t.days.is_empty() || t.days.iter().any(|d| d == ctx.weekday))
                    && (t.from_hour..t.to_hour).contains(&ctx.hour)
            })
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core policy::conditions`
Expected: `test result: ok. 8 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/llmpool-core Cargo.lock
git commit -m "feat(core): policy conditions"
```

---

### Task 5: Candidate selection

**Files:**
- Modify: `crates/llmpool-core/src/policy/mod.rs`
- Create: `crates/llmpool-core/src/policy/select.rs`

**Interfaces:**
- Consumes: `rule::{Target, Pick}`, `context::Context`, `model::Account`.
- Produces: `select::candidates<'a>(target: &Target, ctx: &Context<'a>, ceiling: Option<(&str, f64)>) -> Vec<&'a Account>`; `select::pick<'a>(pick: Pick, candidates: Vec<&'a Account>, cursor: Option<&str>) -> Option<&'a Account>`.

- [ ] **Step 1: Add the module**

In `crates/llmpool-core/src/policy/mod.rs` add `pub mod select;` to the module list.

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-core/src/policy/select.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{account, ctx, session};
    use crate::model::{Health, Window};

    fn target(text: &str) -> Target {
        toml::from_str(text).unwrap()
    }

    fn ids(accounts: Vec<&Account>) -> Vec<&str> {
        accounts.into_iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn skips_current_disabled_unhealthy_other_provider_and_full() {
        let mut disabled = account("disabled", &[], None);
        disabled.enabled = false;
        let mut dead = account("dead", &[], None);
        dead.health = Health::ReloginRequired;
        let mut other = account("other", &[], None);
        other.provider = "openai".into();
        let accounts = [
            account("current", &[], Some(95.0)),
            disabled,
            dead,
            other,
            account("full", &[], Some(100.0)),
            account("good", &[], Some(20.0)),
        ];
        let s = session("/w", Some("current"));
        assert_eq!(ids(candidates(&target(""), &ctx(&s, &accounts), None)), ["good"]);
    }

    #[test]
    fn account_never_polled_is_a_candidate_and_wins_most_quota() {
        let accounts = [
            account("current", &[], Some(95.0)),
            account("fresh", &[], None),
            account("used", &[], Some(30.0)),
        ];
        let s = session("/w", Some("current"));
        let c = candidates(&target(""), &ctx(&s, &accounts), Some(("5h", 90.0)));
        assert_eq!(ids(c.clone()), ["fresh", "used"]);
        assert_eq!(pick(Pick::MostQuota, c, None).unwrap().id, "fresh");
    }

    #[test]
    fn target_by_tag_account_and_same() {
        let accounts = [
            account("cur", &["work", "eu"], None),
            account("w2", &["work"], None),
            account("home", &["home"], None),
        ];
        let s = session("/w", Some("cur"));
        let c = ctx(&s, &accounts);
        assert_eq!(ids(candidates(&target("tag = 'home'"), &c, None)), ["home"]);
        assert_eq!(ids(candidates(&target("account = 'w2'"), &c, None)), ["w2"]);
        assert_eq!(ids(candidates(&target("tag = 'same'"), &c, None)), ["w2"]);
        assert!(candidates(&target("tag = 'missing'"), &c, None).is_empty());
    }

    #[test]
    fn tag_same_with_untagged_current_allows_any() {
        let accounts = [account("cur", &[], None), account("x", &["work"], None)];
        let s = session("/w", Some("cur"));
        assert_eq!(ids(candidates(&target("tag = 'same'"), &ctx(&s, &accounts), None)), ["x"]);
    }

    #[test]
    fn ceiling_excludes_accounts_at_or_above_it() {
        let accounts = [account("a", &[], Some(90.0)), account("b", &[], Some(89.0))];
        let s = session("/w", None);
        assert_eq!(ids(candidates(&target(""), &ctx(&s, &accounts), Some(("5h", 90.0)))), ["b"]);
    }

    #[test]
    fn most_quota_breaks_ties_by_id() {
        let accounts = [account("b", &[], Some(10.0)), account("a", &[], Some(10.0))];
        assert_eq!(pick(Pick::MostQuota, accounts.iter().collect(), None).unwrap().id, "a");
    }

    #[test]
    fn round_robin_walks_ids_and_wraps() {
        let accounts = [account("c", &[], None), account("a", &[], None), account("b", &[], None)];
        let all = || accounts.iter().collect::<Vec<_>>();
        assert_eq!(pick(Pick::RoundRobin, all(), None).unwrap().id, "a");
        assert_eq!(pick(Pick::RoundRobin, all(), Some("a")).unwrap().id, "b");
        assert_eq!(pick(Pick::RoundRobin, all(), Some("c")).unwrap().id, "a");
    }

    #[test]
    fn soonest_reset_prefers_earliest_weekly_reset() {
        let mut late = account("late", &[], Some(10.0));
        let mut soon = account("soon", &[], Some(50.0));
        let weekly = |at| Window { used_percent: 5.0, resets_at: Some(at) };
        late.usage.as_mut().unwrap().windows.insert("7d".into(), weekly(900));
        soon.usage.as_mut().unwrap().windows.insert("7d".into(), weekly(100));
        let accounts = [late, soon, account("unknown", &[], None)];
        assert_eq!(pick(Pick::SoonestReset, accounts.iter().collect(), None).unwrap().id, "soon");
    }

    #[test]
    fn pick_from_nothing_is_none() {
        assert!(pick(Pick::MostQuota, Vec::new(), None).is_none());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core policy::select`
Expected: compile error, `cannot find function candidates`.

- [ ] **Step 4: Write the implementation**

Put this above the `tests` module in `crates/llmpool-core/src/policy/select.rs`:

```rust
use super::context::Context;
use super::rule::{Pick, Target};
use crate::model::Account;

/// Accounts a rule may move a session to.
///
/// `ceiling` is the (window, percent) of a usage-driven rule: accounts at or
/// above it are excluded. Without one, accounts with any window full are.
pub fn candidates<'a>(
    target: &Target,
    ctx: &Context<'a>,
    ceiling: Option<(&str, f64)>,
) -> Vec<&'a Account> {
    let current = ctx.current();
    ctx.accounts
        .iter()
        .filter(|a| a.usable() && a.provider == ctx.session.provider)
        .filter(|a| current.is_none_or(|c| c.id != a.id))
        .filter(|a| match (&target.account, target.tag.as_deref()) {
            (Some(name), _) => &a.label == name || &a.id == name,
            (None, Some("same")) => {
                current.is_none_or(|c| c.tags.is_empty() || !c.tags.is_disjoint(&a.tags))
            }
            (None, Some(tag)) => a.tags.contains(tag),
            (None, None) => true,
        })
        .filter(|a| match ceiling {
            Some((window, percent)) => a.used(window) < percent,
            None => a.peak_used() < 100.0,
        })
        .collect()
}

/// Choose one candidate. Ties go to the lowest id so results are stable.
pub fn pick<'a>(
    pick: Pick,
    mut candidates: Vec<&'a Account>,
    cursor: Option<&str>,
) -> Option<&'a Account> {
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    match pick {
        Pick::MostQuota => candidates
            .into_iter()
            .min_by(|a, b| a.peak_used().total_cmp(&b.peak_used())),
        Pick::SoonestReset => candidates
            .into_iter()
            .min_by_key(|a| a.reset("7d").unwrap_or(i64::MAX)),
        Pick::RoundRobin => cursor
            .and_then(|last| candidates.iter().find(|a| a.id.as_str() > last).copied())
            .or_else(|| candidates.first().copied()),
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core policy::select`
Expected: `test result: ok. 9 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/llmpool-core
git commit -m "feat(core): candidate selection and pick strategies"
```

---

### Task 6: Engine

**Files:**
- Modify: `crates/llmpool-core/src/policy/mod.rs`
- Create: `crates/llmpool-core/src/policy/engine.rs`

**Interfaces:**
- Consumes: `rule::{PolicyFile, Rule, Settings}`, `context::{Context, Event}`, `triggers::find`, `Conditions::hold`, `select::{candidates, pick}`.
- Produces: `engine::Outcome` (`NoMatch`, `Hold`, `Notify`, `Assign(AccountId)`, `Swap(AccountId)`, `Ask(Vec<AccountId>)`, `Exhausted`, `Cooldown`; serde snake_case); `engine::Decision { rule: Option<String>, outcome: Outcome, notify: bool }` (Serialize); `engine::evaluate(policy: &PolicyFile, event: Event, ctx: &Context) -> Decision`; `engine::Scenario` (Deserialize: `event`, `session`, `accounts`, `now`, `weekday`, `hour`, `home`) with `Scenario::decide(&self, policy: &PolicyFile) -> Decision`.

- [ ] **Step 1: Add the module**

In `crates/llmpool-core/src/policy/mod.rs` add `pub mod engine;` to the module list.

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-core/src/policy/engine.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{account, ctx, session};

    fn policy(text: &str) -> PolicyFile {
        PolicyFile::parse(text).unwrap()
    }

    const SWAP_AT_90: &str = r#"
[[rule]]
name = "swap before the limit"
on = { usage_above = { percent = 90 } }
do = { swap = {}, notify = true }
"#;

    #[test]
    fn no_rule_is_no_match() {
        let s = session("/w", None);
        let d = evaluate(&PolicyFile::default(), Event::SessionStart, &ctx(&s, &[]));
        assert_eq!(d, Decision { rule: None, outcome: Outcome::NoMatch, notify: false });
    }

    #[test]
    fn first_matching_rule_wins_and_disabled_rules_are_skipped() {
        let p = policy(
            r#"
[[rule]]
name = "off"
enabled = false
on = "session_start"
do = { hold = true }

[[rule]]
name = "acme"
on = "session_start"
when = { cwd = "/work/acme/**" }
do = { assign = { tag = "acme" } }

[[rule]]
name = "rest"
on = "session_start"
do = { ask = {} }
"#,
        );
        let accounts = [account("a1", &["acme"], None), account("p1", &[], None)];

        let s = session("/work/acme/api", None);
        let d = evaluate(&p, Event::SessionStart, &ctx(&s, &accounts));
        assert_eq!(d.rule.as_deref(), Some("acme"));
        assert_eq!(d.outcome, Outcome::Assign("a1".into()));

        let s = session("/elsewhere", None);
        let d = evaluate(&p, Event::SessionStart, &ctx(&s, &accounts));
        assert_eq!(d.rule.as_deref(), Some("rest"));
        assert_eq!(d.outcome, Outcome::Ask(vec!["a1".into(), "p1".into()]));
    }

    #[test]
    fn hold_stops_evaluation() {
        let p = policy(
            r#"
[[rule]]
name = "leave this repo alone"
on = "rate_limited"
when = { cwd = "/pinned/**" }
do = { hold = true }

[[rule]]
name = "swap"
on = "rate_limited"
do = { swap = {} }
"#,
        );
        let accounts = [account("a", &[], None), account("b", &[], None)];
        let s = session("/pinned/x", Some("a"));
        assert_eq!(evaluate(&p, Event::RateLimited, &ctx(&s, &accounts)).outcome, Outcome::Hold);
    }

    #[test]
    fn threshold_swap_needs_the_margin() {
        let p = policy(SWAP_AT_90);
        let s = session("/w", Some("cur"));

        // 85 is under the ceiling but not 10 points better than 91.
        let accounts = [account("cur", &[], Some(91.0)), account("near", &[], Some(85.0))];
        let d = evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts));
        assert_eq!(d.outcome, Outcome::Exhausted);
        assert!(d.notify);

        let accounts = [
            account("cur", &[], Some(91.0)),
            account("near", &[], Some(85.0)),
            account("roomy", &[], Some(50.0)),
        ];
        let d = evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts));
        assert_eq!(d.outcome, Outcome::Swap("roomy".into()));
    }

    #[test]
    fn rate_limited_swap_ignores_the_margin() {
        let p = policy("[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = {} }\n");
        let accounts = [account("cur", &[], Some(91.0)), account("near", &[], Some(85.0))];
        let s = session("/w", Some("cur"));
        let d = evaluate(&p, Event::RateLimited, &ctx(&s, &accounts));
        assert_eq!(d.outcome, Outcome::Swap("near".into()));
    }

    #[test]
    fn cooldown_blocks_a_second_swap() {
        let p = policy(SWAP_AT_90);
        let accounts = [account("cur", &[], Some(95.0)), account("b", &[], Some(10.0))];
        let mut s = session("/w", Some("cur"));
        // The fixture context has now = 1_000_000 and the default cooldown is 300s.
        s.last_swap_at = Some(1_000_000 - 299);
        assert_eq!(evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts)).outcome, Outcome::Cooldown);
        s.last_swap_at = Some(1_000_000 - 300);
        assert_eq!(
            evaluate(&p, Event::UsageUpdated, &ctx(&s, &accounts)).outcome,
            Outcome::Swap("b".into())
        );
    }

    #[test]
    fn tag_no_account_carries_is_exhausted() {
        let p = policy(
            "[[rule]]\nname = 'ghost'\non = 'session_start'\ndo = { assign = { tag = 'ghost' } }\n",
        );
        let accounts = [account("a", &["work"], None)];
        let s = session("/w", None);
        let d = evaluate(&p, Event::SessionStart, &ctx(&s, &accounts));
        assert_eq!(d.rule.as_deref(), Some("ghost"));
        assert_eq!(d.outcome, Outcome::Exhausted);
    }

    #[test]
    fn notify_only_rule() {
        let p = policy("[[rule]]\nname = 'tell'\non = 'auth_failed'\ndo = { notify = true }\n");
        let s = session("/w", None);
        let d = evaluate(&p, Event::AuthFailed, &ctx(&s, &[]));
        assert_eq!(d.outcome, Outcome::Notify);
        assert!(d.notify);
    }

    #[test]
    fn scenario_decides_from_json() {
        let json = serde_json::json!({
            "event": "usage_updated",
            "session": session("/w", Some("cur")),
            "accounts": [account("cur", &[], Some(95.0)), account("b", &[], Some(10.0))],
        });
        let scenario: Scenario = serde_json::from_value(json).unwrap();
        let d = scenario.decide(&policy(SWAP_AT_90));
        assert_eq!(serde_json::to_value(&d).unwrap()["outcome"]["swap"], "b");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core policy::engine`
Expected: compile error, `cannot find function evaluate`.

- [ ] **Step 4: Write the implementation**

Put this above the `tests` module in `crates/llmpool-core/src/policy/engine.rs`:

```rust
use super::context::{Context, Event};
use super::rule::{PolicyFile, Rule, Settings};
use super::{select, triggers};
use crate::model::{Account, AccountId, Session};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// No rule fired.
    NoMatch,
    /// A rule fired and said to do nothing.
    Hold,
    /// A rule fired with only `notify`.
    Notify,
    Assign(AccountId),
    Swap(AccountId),
    /// The user should choose among these.
    Ask(Vec<AccountId>),
    /// A rule fired but no account qualifies.
    Exhausted,
    /// A swap rule fired inside the session's cooldown.
    Cooldown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    /// Name of the rule that fired.
    pub rule: Option<String>,
    pub outcome: Outcome,
    pub notify: bool,
}

/// Walk the rules in order; the first enabled rule whose trigger matches and
/// whose conditions hold decides.
pub fn evaluate(policy: &PolicyFile, event: Event, ctx: &Context) -> Decision {
    for rule in policy.rules.iter().filter(|r| r.enabled) {
        let Some(trigger) = triggers::find(&rule.on.kind) else {
            continue;
        };
        if !trigger.matches(&rule.on.params, event, ctx) || !rule.when.hold(ctx) {
            continue;
        }
        let threshold = trigger.threshold(&rule.on.params);
        return Decision {
            rule: Some(rule.name.clone()),
            outcome: run(rule, threshold, &policy.settings, ctx),
            notify: rule.action.notify,
        };
    }
    Decision { rule: None, outcome: Outcome::NoMatch, notify: false }
}

fn run(rule: &Rule, threshold: Option<(String, f64)>, settings: &Settings, ctx: &Context) -> Outcome {
    let action = &rule.action;
    let ceiling = threshold.as_ref().map(|(window, percent)| (window.as_str(), *percent));
    let chosen = |account: Option<&Account>, wrap: fn(AccountId) -> Outcome| {
        account.map_or(Outcome::Exhausted, |a| wrap(a.id.clone()))
    };

    if action.hold {
        return Outcome::Hold;
    }
    if let Some(target) = &action.assign {
        let found = select::candidates(target, ctx, ceiling);
        return chosen(select::pick(target.pick, found, ctx.rr_cursor), Outcome::Assign);
    }
    if let Some(target) = &action.ask {
        let ids: Vec<_> = select::candidates(target, ctx, ceiling).iter().map(|a| a.id.clone()).collect();
        return if ids.is_empty() { Outcome::Exhausted } else { Outcome::Ask(ids) };
    }
    if let Some(target) = &action.swap {
        if ctx.session.last_swap_at.is_some_and(|at| ctx.now - at < settings.cooldown_secs) {
            return Outcome::Cooldown;
        }
        let mut found = select::candidates(target, ctx, ceiling);
        // A usage-driven swap must be worth it: the target has to beat the
        // current account by the margin, or two accounts near the line ping-pong.
        if let (Some((window, _)), Some(current)) = (ceiling, ctx.current()) {
            found.retain(|a| a.used(window) + settings.margin_points <= current.used(window));
        }
        return chosen(select::pick(target.pick, found, ctx.rr_cursor), Outcome::Swap);
    }
    Outcome::Notify
}

fn monday() -> String {
    "mon".into()
}

fn noon() -> u8 {
    12
}

/// A self-contained situation to evaluate a policy against. Used by
/// `llmpool policy dry-run` and by the GUI's rule preview.
#[derive(Clone, Debug, Deserialize)]
pub struct Scenario {
    pub event: Event,
    pub session: Session,
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub now: i64,
    #[serde(default = "monday")]
    pub weekday: String,
    #[serde(default = "noon")]
    pub hour: u8,
    #[serde(default)]
    pub home: String,
}

impl Scenario {
    pub fn decide(&self, policy: &PolicyFile) -> Decision {
        evaluate(
            policy,
            self.event,
            &Context {
                session: &self.session,
                accounts: &self.accounts,
                now: self.now,
                weekday: &self.weekday,
                hour: self.hour,
                home: &self.home,
                rr_cursor: None,
            },
        )
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core policy::engine`
Expected: `test result: ok. 9 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/llmpool-core
git commit -m "feat(core): policy engine with swap guards and dry-run scenario"
```

---

### Task 7: Validation and policy file load/save

**Files:**
- Modify: `crates/llmpool-core/src/policy/mod.rs`
- Create: `crates/llmpool-core/src/policy/validate.rs`
- Create: `crates/llmpool-core/src/policy/file.rs`

**Interfaces:**
- Consumes: `rule::PolicyFile`, `triggers::find`, `Conditions::check`, `PolicyError`.
- Produces: `validate::validate(policy: &PolicyFile) -> Vec<String>`; `file::checked(text: &str) -> Result<PolicyFile, PolicyError>`; `file::load(path: &Path) -> Result<PolicyFile, PolicyError>` (missing file is an empty policy); `file::save(path: &Path, text: &str) -> Result<PolicyFile, PolicyError>` (validates, then replaces the file atomically; on error the file is untouched).

- [ ] **Step 1: Add the modules**

In `crates/llmpool-core/src/policy/mod.rs` add `pub mod file;` and `pub mod validate;` to the module list. The list is now:

```rust
pub mod conditions;
pub mod context;
pub mod engine;
pub mod file;
pub mod rule;
pub mod select;
pub mod triggers;
pub mod validate;
```

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-core/src/policy/validate.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn errors(text: &str) -> Vec<String> {
        validate(&PolicyFile::parse(text).unwrap())
    }

    fn rule(body: &str) -> String {
        format!("[[rule]]\nname = 'r'\n{body}\n")
    }

    #[test]
    fn a_good_policy_has_no_errors() {
        let text = rule("on = { usage_above = { percent = 90 } }\ndo = { swap = { tag = 'same' }, notify = true }");
        assert_eq!(errors(&text), Vec::<String>::new());
    }

    #[test]
    fn reports_each_problem_with_the_rule_position_and_name() {
        let text = [
            rule("on = 'session_start'\ndo = { hold = true }"),
            rule("on = 'moon_phase'\ndo = { hold = true }"),
            rule("on = { usage_above = { percent = 150 } }\ndo = { notify = true }"),
        ]
        .concat();
        let found = errors(&text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("rule 2 (\"r\")"), "{}", found[0]);
        assert!(found[0].contains("moon_phase"));
        assert!(found[1].starts_with("rule 3 (\"r\")"), "{}", found[1]);
    }

    #[test]
    fn action_must_be_exactly_one_thing() {
        assert_eq!(errors(&rule("on = 'rate_limited'\ndo = {}")).len(), 1);
        assert_eq!(errors(&rule("on = 'rate_limited'\ndo = { swap = {}, hold = true }")).len(), 1);
        assert_eq!(errors(&rule("on = 'rate_limited'\ndo = { swap = {}, ask = {} }")).len(), 1);
    }

    #[test]
    fn assign_only_on_session_start() {
        assert_eq!(errors(&rule("on = 'rate_limited'\ndo = { assign = {} }")).len(), 1);
        assert!(errors(&rule("on = 'session_start'\ndo = { assign = {} }")).is_empty());
    }

    #[test]
    fn target_takes_account_or_tag_not_both() {
        let text = rule("on = 'rate_limited'\ndo = { swap = { account = 'a', tag = 'b' } }");
        assert_eq!(errors(&text).len(), 1);
    }

    #[test]
    fn bad_condition_and_settings_are_reported() {
        assert_eq!(errors(&rule("on = 'rate_limited'\nwhen = { cwd = '[' }\ndo = { hold = true }")).len(), 1);
        assert_eq!(errors("[settings]\ncooldown_secs = -1\nmargin_points = 500\n").len(), 2);
    }
}
```

`crates/llmpool-core/src/policy/file.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const GOOD: &str = "# my rules\n[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = {} }\n";

    #[test]
    fn missing_or_empty_file_is_an_empty_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        assert_eq!(load(&path).unwrap(), PolicyFile::default());
        fs::write(&path, "").unwrap();
        assert_eq!(load(&path).unwrap(), PolicyFile::default());
    }

    #[test]
    fn save_writes_the_text_as_given_and_load_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        let saved = save(&path, GOOD).unwrap();
        assert_eq!(saved.rules.len(), 1);
        // Comments survive because the text is stored as the user wrote it.
        assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
        assert_eq!(load(&path).unwrap(), saved);
    }

    #[test]
    fn syntax_error_reports_a_line_and_keeps_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        save(&path, GOOD).unwrap();
        let err = save(&path, "[[rule]]\nname = 'broken'\non = \n").unwrap_err();
        assert!(matches!(err, PolicyError::Parse(_)));
        assert!(err.to_string().contains("line 3"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
    }

    #[test]
    fn invalid_rule_is_rejected_and_keeps_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        save(&path, GOOD).unwrap();
        let err = save(&path, "[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n").unwrap_err();
        assert!(matches!(err, PolicyError::Invalid(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
        assert!(!dir.path().join("policies.toml.tmp").exists());
    }

    #[test]
    fn load_rejects_an_invalid_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policies.toml");
        fs::write(&path, "[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n").unwrap();
        assert!(matches!(load(&path), Err(PolicyError::Invalid(_))));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core -- policy::validate policy::file`
Expected: compile error, `cannot find function validate`.

- [ ] **Step 4: Write the validator**

Put this above the `tests` module in `crates/llmpool-core/src/policy/validate.rs`:

```rust
use super::rule::PolicyFile;
use super::triggers;

/// Every problem in a parsed policy, one message each. Empty means valid.
pub fn validate(policy: &PolicyFile) -> Vec<String> {
    let mut errors = Vec::new();

    if policy.settings.cooldown_secs < 0 {
        errors.push("settings: `cooldown_secs` must not be negative".to_string());
    }
    if !(0.0..=100.0).contains(&policy.settings.margin_points) {
        errors.push("settings: `margin_points` must be 0 to 100".to_string());
    }

    for (index, rule) in policy.rules.iter().enumerate() {
        let mut fail = |message: String| errors.push(format!("rule {} ({:?}): {message}", index + 1, rule.name));

        match triggers::find(&rule.on.kind) {
            None => fail(format!("unknown trigger `{}`", rule.on.kind)),
            Some(trigger) => {
                if let Err(e) = trigger.check(&rule.on.params) {
                    fail(e);
                }
            }
        }
        if let Err(e) = rule.when.check() {
            fail(e);
        }

        let action = &rule.action;
        let targets = [&action.assign, &action.swap, &action.ask];
        let chosen = targets.iter().filter(|t| t.is_some()).count() + usize::from(action.hold);
        if chosen > 1 {
            fail("`do` takes only one of assign, swap, ask, hold".into());
        } else if chosen == 0 && !action.notify {
            fail("`do` is empty".into());
        }
        if action.assign.is_some() && rule.on.kind != "session_start" {
            fail("`assign` is only valid on `session_start`; use `swap`".into());
        }
        if targets.iter().any(|t| t.as_ref().is_some_and(|t| t.account.is_some() && t.tag.is_some())) {
            fail("give `account` or `tag`, not both".into());
        }
    }
    errors
}
```

- [ ] **Step 5: Write load and save**

Put this above the `tests` module in `crates/llmpool-core/src/policy/file.rs`:

```rust
use super::rule::PolicyFile;
use super::validate::validate;
use super::PolicyError;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Parse and validate policy text.
pub fn checked(text: &str) -> Result<PolicyFile, PolicyError> {
    let policy = PolicyFile::parse(text)?;
    let errors = validate(&policy);
    if errors.is_empty() {
        Ok(policy)
    } else {
        Err(PolicyError::Invalid(errors))
    }
}

/// A missing file is an empty policy.
pub fn load(path: &Path) -> Result<PolicyFile, PolicyError> {
    match fs::read_to_string(path) {
        Ok(text) => checked(&text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(PolicyFile::default()),
        Err(e) => Err(e.into()),
    }
}

/// Validate `text`, then replace the file with it. The text is written as
/// given so the user's comments and layout survive. On any error the existing
/// file is left untouched.
pub fn save(path: &Path, text: &str) -> Result<PolicyFile, PolicyError> {
    let policy = checked(text)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(policy)
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core -- policy::validate policy::file`
Expected: `test result: ok. 11 passed`.

- [ ] **Step 7: Commit**

```bash
git add crates/llmpool-core
git commit -m "feat(core): policy validation and atomic load/save"
```

---

### Task 8: Account store

**Files:**
- Modify: `crates/llmpool-core/Cargo.toml`
- Modify: `crates/llmpool-core/src/lib.rs`
- Create: `crates/llmpool-core/src/store.rs`

**Interfaces:**
- Consumes: `model::Account`.
- Produces: `store::StoreError` (`Sqlite`, `Json`); `store::Store` with `Store::open(path: &Path) -> Result<Store, StoreError>`, `Store::memory() -> Result<Store, StoreError>`, `put(&self, account: &Account) -> Result<(), StoreError>`, `get(&self, id: &str) -> Result<Option<Account>, StoreError>`, `list(&self) -> Result<Vec<Account>, StoreError>` (ordered by id), `remove(&self, id: &str) -> Result<bool, StoreError>`.

- [ ] **Step 1: Add the dependency and module**

In `crates/llmpool-core/Cargo.toml` under `[dependencies]` add:

```toml
rusqlite = { version = "0.32", features = ["bundled"] }
```

In `crates/llmpool-core/src/lib.rs` add `pub mod store;` after `pub mod policy;`.

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-core/src/store.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::account;

    #[test]
    fn put_get_list_remove() {
        let store = Store::memory().unwrap();
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.get("a").unwrap(), None);

        let b = account("b", &["work"], Some(12.5));
        let a = account("a", &[], None);
        store.put(&b).unwrap();
        store.put(&a).unwrap();
        assert_eq!(store.get("b").unwrap(), Some(b.clone()));
        assert_eq!(store.list().unwrap(), vec![a, b]);

        assert!(store.remove("a").unwrap());
        assert!(!store.remove("a").unwrap());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn put_replaces_an_existing_account() {
        let store = Store::memory().unwrap();
        let mut a = account("a", &[], None);
        store.put(&a).unwrap();
        a.tags.insert("work".into());
        a.enabled = false;
        store.put(&a).unwrap();
        assert_eq!(store.list().unwrap(), vec![a]);
    }

    #[test]
    fn data_survives_reopening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llmpool.db");
        Store::open(&path).unwrap().put(&account("a", &["x"], None)).unwrap();
        assert_eq!(Store::open(&path).unwrap().get("a").unwrap(), Some(account("a", &["x"], None)));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core store`
Expected: compile error, `cannot find type Store`.

- [ ] **Step 4: Write the implementation**

Put this above the `tests` module in `crates/llmpool-core/src/store.rs`:

```rust
use crate::model::Account;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored account is not valid: {0}")]
    Json(#[from] serde_json::Error),
}

/// Account metadata. Secrets are never stored here; see `vault`.
pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?)
    }

    pub fn memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self, StoreError> {
        db.execute(
            "CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY, data TEXT NOT NULL)",
            [],
        )?;
        Ok(Self { db })
    }

    pub fn put(&self, account: &Account) -> Result<(), StoreError> {
        self.db.execute(
            "INSERT INTO accounts (id, data) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![account.id, serde_json::to_string(account)?],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Account>, StoreError> {
        let data: Option<String> = self
            .db
            .query_row("SELECT data FROM accounts WHERE id = ?1", params![id], |row| row.get(0))
            .optional()?;
        Ok(data.map(|d| serde_json::from_str(&d)).transpose()?)
    }

    pub fn list(&self) -> Result<Vec<Account>, StoreError> {
        let mut statement = self.db.prepare("SELECT data FROM accounts ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut accounts = Vec::new();
        for data in rows {
            accounts.push(serde_json::from_str(&data?)?);
        }
        Ok(accounts)
    }

    /// Returns whether an account was removed.
    pub fn remove(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.db.execute("DELETE FROM accounts WHERE id = ?1", params![id])? > 0)
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core store`
Expected: `test result: ok. 3 passed`. The first build compiles bundled SQLite and takes about a minute.

- [ ] **Step 6: Commit**

```bash
git add crates/llmpool-core Cargo.lock
git commit -m "feat(core): SQLite account store"
```

---

### Task 9: Vault

**Files:**
- Modify: `crates/llmpool-core/Cargo.toml`
- Modify: `crates/llmpool-core/src/lib.rs`
- Create: `crates/llmpool-core/src/vault.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `vault::VaultError` (`Backend(String)`); `vault::Vault` trait with `put(&self, id: &str, secret: &str) -> Result<(), VaultError>`, `get(&self, id: &str) -> Result<Option<String>, VaultError>`, `remove(&self, id: &str) -> Result<(), VaultError>` (removing a missing item is `Ok`); `vault::MemoryVault` (`Default`); `vault::KeychainVault` (unit struct, service name `llmpool`).

- [ ] **Step 1: Add the dependency and module**

In `crates/llmpool-core/Cargo.toml` under `[dependencies]` add:

```toml
keyring = { version = "3", features = ["apple-native", "windows-native", "sync-secret-service"] }
```

In `crates/llmpool-core/src/lib.rs` add `pub mod vault;` after `pub mod store;`. The file is now:

```rust
pub mod model;
pub mod policy;
pub mod store;
pub mod vault;

#[cfg(test)]
mod fixtures;
```

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-core/src/vault.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(vault: &dyn Vault, id: &str) {
        assert_eq!(vault.get(id).unwrap(), None);
        vault.put(id, "first").unwrap();
        vault.put(id, "second").unwrap();
        assert_eq!(vault.get(id).unwrap().as_deref(), Some("second"));
        vault.remove(id).unwrap();
        vault.remove(id).unwrap();
        assert_eq!(vault.get(id).unwrap(), None);
    }

    #[test]
    fn memory_vault_stores_replaces_and_removes() {
        exercise(&MemoryVault::default(), "a1");
    }

    #[test]
    fn error_text_never_contains_the_secret() {
        let e = VaultError::Backend("no access".into());
        assert_eq!(e.to_string(), "secret store: no access");
    }

    /// Writes to the real OS keychain under a throwaway id.
    /// Run by hand: `cargo test -p llmpool-core vault -- --ignored`
    #[test]
    #[ignore]
    fn keychain_vault_stores_replaces_and_removes() {
        exercise(&KeychainVault, "llmpool-selftest");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-core vault`
Expected: compile error, `cannot find type MemoryVault`.

- [ ] **Step 4: Write the implementation**

Put this above the `tests` module in `crates/llmpool-core/src/vault.rs`:

```rust
use std::collections::HashMap;
use std::sync::Mutex;

/// Keychain service every llmpool secret is stored under.
const SERVICE: &str = "llmpool";

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// Carries the backend's message only, never a secret.
    #[error("secret store: {0}")]
    Backend(String),
}

/// Holds one secret per account id.
pub trait Vault: Send + Sync {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError>;
    fn get(&self, id: &str) -> Result<Option<String>, VaultError>;
    /// Removing a secret that is not there is not an error.
    fn remove(&self, id: &str) -> Result<(), VaultError>;
}

/// In-process vault for tests.
#[derive(Default)]
pub struct MemoryVault(Mutex<HashMap<String, String>>);

impl Vault for MemoryVault {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError> {
        self.0.lock().unwrap().insert(id.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}

/// The operating system's keychain.
pub struct KeychainVault;

impl KeychainVault {
    fn entry(id: &str) -> Result<keyring::Entry, VaultError> {
        keyring::Entry::new(SERVICE, id).map_err(backend)
    }
}

fn backend(e: keyring::Error) -> VaultError {
    VaultError::Backend(e.to_string())
}

impl Vault for KeychainVault {
    fn put(&self, id: &str, secret: &str) -> Result<(), VaultError> {
        Self::entry(id)?.set_password(secret).map_err(backend)
    }

    fn get(&self, id: &str) -> Result<Option<String>, VaultError> {
        match Self::entry(id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    fn remove(&self, id: &str) -> Result<(), VaultError> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(backend(e)),
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-core vault`
Expected: `test result: ok. 2 passed; 0 failed; 1 ignored`.

- [ ] **Step 6: Run the keychain test once by hand**

Run: `cargo test -p llmpool-core vault -- --ignored`
Expected: `1 passed`. macOS may show a keychain access prompt; allow it. If it fails, record the error text in the commit message and continue; Plan 2 depends on this backend and will address it.

- [ ] **Step 7: Commit**

```bash
git add crates/llmpool-core Cargo.lock
git commit -m "feat(core): vault trait with memory and OS keychain backends"
```

---

### Task 10: CLI

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/llmpool-cli/Cargo.toml`
- Create: `crates/llmpool-cli/src/main.rs`
- Create: `README.md`

**Interfaces:**
- Consumes: `llmpool_core::policy::file::load`, `llmpool_core::policy::engine::Scenario`.
- Produces: binary `llmpool` with `llmpool policy check <file>` and `llmpool policy dry-run <file> <scenario.json>`. Exit code 0 on success, 1 on any error, message on stderr prefixed `llmpool: `.

- [ ] **Step 1: Add the crate**

In the root `Cargo.toml` change `members` to:

```toml
members = ["crates/llmpool-core", "crates/llmpool-cli"]
```

`crates/llmpool-cli/Cargo.toml`:

```toml
[package]
name = "llmpool-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "llmpool"
path = "src/main.rs"

[dependencies]
llmpool-core = { path = "../llmpool-core" }
clap = { version = "4", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the failing tests**

`crates/llmpool-cli/src/main.rs` (tests and a stub `main` for now):

```rust
fn main() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    const POLICY: &str = "[[rule]]\nname = 'rl'\non = 'rate_limited'\ndo = { swap = {}, notify = true }\n";

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

    fn llmpool(args: &[&str]) -> Result<String, String> {
        let cli = Cli::try_parse_from(["llmpool"].iter().chain(args).copied()).map_err(|e| e.to_string())?;
        run(cli)
    }

    fn write(dir: &Path, name: &str, text: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn check_accepts_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let policy = write(dir.path(), "p.toml", POLICY);
        assert_eq!(llmpool(&["policy", "check", &policy]).unwrap(), "ok: 1 rule(s)");
    }

    #[test]
    fn check_reports_invalid_rules_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let bad = write(dir.path(), "bad.toml", "[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n");
        let err = llmpool(&["policy", "check", &bad]).unwrap_err();
        assert!(err.contains("moon_phase"), "{err}");

        let missing = dir.path().join("nope.toml");
        let err = llmpool(&["policy", "check", missing.to_str().unwrap()]).unwrap_err();
        assert!(err.contains("no such file"), "{err}");
    }

    #[test]
    fn dry_run_prints_the_decision_as_json() {
        let dir = tempfile::tempdir().unwrap();
        let policy = write(dir.path(), "p.toml", POLICY);
        let scenario = write(dir.path(), "s.json", SCENARIO);
        let out = llmpool(&["policy", "dry-run", &policy, &scenario]).unwrap();
        let decision: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(decision["rule"], "rl");
        assert_eq!(decision["outcome"]["swap"], "b");
        assert_eq!(decision["notify"], true);
    }

    #[test]
    fn dry_run_names_the_file_when_the_scenario_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let policy = write(dir.path(), "p.toml", POLICY);
        let scenario = write(dir.path(), "s.json", "{ not json");
        let err = llmpool(&["policy", "dry-run", &policy, &scenario]).unwrap_err();
        assert!(err.contains("s.json"), "{err}");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p llmpool-cli`
Expected: compile error, `cannot find type Cli`.

- [ ] **Step 4: Write the implementation**

Replace the `fn main() {}` line at the top of `crates/llmpool-cli/src/main.rs` with:

```rust
use clap::{Parser, Subcommand};
use llmpool_core::policy::engine::Scenario;
use llmpool_core::policy::file;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "llmpool", version, about = "A pool of LLM provider credentials for coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect and test policy files
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Validate a policy file
    Check { file: PathBuf },
    /// Show which rule fires for a scenario described in a JSON file
    DryRun { file: PathBuf, scenario: PathBuf },
}

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

fn run(cli: Cli) -> Result<String, String> {
    match cli.command {
        Command::Policy { command } => match command {
            PolicyCommand::Check { file } => {
                let policy = load_policy(&file)?;
                Ok(format!("ok: {} rule(s)", policy.rules.len()))
            }
            PolicyCommand::DryRun { file, scenario } => {
                let policy = load_policy(&file)?;
                let parsed: Scenario = serde_json::from_str(&read(&scenario)?)
                    .map_err(|e| format!("{}: {e}", scenario.display()))?;
                serde_json::to_string_pretty(&parsed.decide(&policy)).map_err(|e| e.to_string())
            }
        },
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("llmpool: {message}");
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p llmpool-cli`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 6: Write the README**

`README.md`:

````markdown
# llmpool

A pool of LLM provider credentials for coding agents. llmpool holds several
logins and keys, assigns one to each agent session, and swaps the credential of
a running session when a policy says so.

Status: foundation only. The policy engine works and can be tested from the
command line. The daemon, Claude Code integration and GUI are not built yet.
See `docs/superpowers/specs/` for the design.

## Try the policy engine

```bash
cargo build --release
./target/release/llmpool policy check policies.toml
./target/release/llmpool policy dry-run policies.toml scenario.json
```

A policy file is an ordered list of rules; the first rule that matches wins:

```toml
[[rule]]
name = "acme repos use acme seats"
on = "session_start"
when = { cwd = "~/work/acme/**" }
do = { assign = { tag = "acme", pick = "most_quota" } }

[[rule]]
name = "swap before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same" }, notify = true }
```

Triggers: `session_start`, `usage_above`, `rate_limited`, `auth_failed`,
`cwd_changed`. Conditions: `cwd`, `git_remote`, `model`, `account_tag`, `time`.
Actions: `assign`, `swap`, `ask`, `notify`, `hold`. Pick strategies:
`most_quota`, `soonest_reset`, `round_robin`.

## Development

```bash
cargo test --workspace
```
````

- [ ] **Step 7: Run everything**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all tests pass (59 passed, 1 ignored across both crates), clippy prints no warnings. Fix any clippy warning in the file it names before committing.

- [ ] **Step 8: Check the binary by hand**

```bash
printf "[[rule]]\nname = 'x'\non = 'moon_phase'\ndo = { hold = true }\n" > /tmp/llmpool-bad.toml
cargo run -q -p llmpool-cli -- policy check /tmp/llmpool-bad.toml; echo "exit=$?"
```

Expected: `llmpool: /tmp/llmpool-bad.toml: rule 1 ("x"): unknown trigger \`moon_phase\`` then `exit=1`.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock crates/llmpool-cli README.md
git commit -m "feat(cli): llmpool policy check and dry-run"
```
