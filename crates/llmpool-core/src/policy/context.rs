use crate::model::{Account, AccountId, Session};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    SessionStart,
    /// A new usage reading arrived for the session's current account.
    UsageUpdated,
    RateLimited,
    AuthFailed,
    CwdChanged,
    /// Once a minute, for rules that run on a schedule.
    Tick,
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
    /// 0 .. 59, local time.
    pub minute: u8,
    /// Home directory, for `~` in path globs.
    pub home: &'a str,
    /// Id of the account round-robin picked last, if any.
    pub rr_cursor: Option<&'a str>,
    /// How many running sessions each account carries. Empty when unknown.
    /// On one machine this counts local sessions; a server counts everyone's.
    pub busy: &'a BTreeMap<AccountId, usize>,
}

impl<'a> Context<'a> {
    /// Sessions running on an account, this one included.
    pub fn load(&self, id: &str) -> usize {
        self.busy.get(id).copied().unwrap_or(0)
    }

    pub fn current(&self) -> Option<&'a Account> {
        let id = self.session.account_id.as_ref()?;
        self.accounts.iter().find(|a| &a.id == id)
    }
}
