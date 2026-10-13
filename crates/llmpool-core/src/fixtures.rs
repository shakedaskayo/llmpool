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
