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
