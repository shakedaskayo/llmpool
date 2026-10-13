//! The optional decision layer: a calibrated decision model asked for a
//! second opinion on whether a session should switch and where to.
//!
//! It advises and nothing more. What it is shown is a compact picture of the
//! pool with accounts named by letter, never by address; what it answers is
//! checked against the same guardrails every rule obeys (cooldown, only
//! accounts that are usable and free) before anything happens. Without it, or
//! when it does not answer, the rules decide exactly as before.

use super::*;
use llmpool_core::policy::rule::AdvisorSettings;
use llmpool_core::policy::select::OWN;

/// Where the advisor's key is kept in the vault.
pub(crate) const KEY_ID: &str = "advisor-jev";
const JEV_URL: &str = "https://jevai.info";
const JEV_MODEL: &str = "typesafe/jev-1.13";
/// Below this, an account is simply not worth asking about.
const WORTH_ASKING: f64 = 50.0;

/// What the advisor last said about a session.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Advice {
    pub at: i64,
    /// Probability that switching now is right; `None` while unanswered.
    pub switch: Option<f64>,
    /// The account it would move to, by label; `None` for "stay".
    pub target: Option<String>,
    /// 0 can wait, 1 within minutes, 2 now.
    pub urgency: Option<f64>,
    /// Whether llmpool acted on it.
    pub acted: bool,
    pub error: Option<String>,
}

fn uuid() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// One round of questions with Jev. Returns the answers by question name.
fn ask(
    settings: &AdvisorSettings,
    key: &str,
    state: &Value,
    questions: &Value,
) -> Result<Value, String> {
    let url = format!(
        "{}/api/jev/run",
        settings
            .url
            .as_deref()
            .unwrap_or(JEV_URL)
            .trim_end_matches('/')
    );
    let body =
        json!({"requestId": uuid(), "model": JEV_MODEL, "state": state, "questions": questions});
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let response = agent
        .post(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("User-Agent", concat!("llmpool/", env!("CARGO_PKG_VERSION")))
        .send_json(body);
    let reply: Value = match response {
        Ok(response) => response.into_json().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(code, response)) => {
            let reply: Value = response.into_json().unwrap_or_default();
            let said = reply["message"].as_str().unwrap_or_default();
            return Err(match (code, said) {
                (401, _) => "the advisor refused the key".into(),
                (402, _) => "the advisor account has no tokens left".into(),
                (429, _) => "the advisor asked to slow down".into(),
                _ => format!("the advisor answered {code} {said}")
                    .trim()
                    .to_string(),
            });
        }
        Err(e) => return Err(format!("the advisor could not be reached: {e}")),
    };
    match (
        reply["code"].as_i64(),
        reply["data"]["status"].as_str(),
        reply["data"]["result"]["answers"].as_object(),
    ) {
        (Some(0), Some("completed"), Some(_)) => Ok(reply["data"]["result"]["answers"].clone()),
        (_, status, _) => Err(format!(
            "the advisor did not finish ({})",
            status.unwrap_or("no status")
        )),
    }
}

/// A number an answer carries under whichever of these names the service uses.
fn number(answer: &Value, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| answer[*name].as_f64())
}

/// How a window is going, in the words a person would use.
fn pace(account: &Account, window: &str, window_secs: i64, t: i64) -> Value {
    let (used, Some(reset)) = (account.used(window), account.reset(window)) else {
        return json!({"used_percent": account.used(window).round()});
    };
    let left = (reset - t).max(0);
    let spent = (window_secs - left).max(0) as f64;
    let outlook = if used >= 100.0 {
        "limit reached".to_string()
    } else if spent < 600.0 || used < 2.0 {
        "too early to tell".to_string()
    } else {
        let until_full = (100.0 - used) / (used / spent);
        if until_full < left as f64 {
            format!(
                "full in about {} minutes at this pace",
                (until_full / 60.0).round()
            )
        } else {
            "lasts until it resets at this pace".to_string()
        }
    };
    json!({"used_percent": used.round(), "resets_in_minutes": left / 60, "outlook": outlook})
}

/// One account as the advisor sees it: no address, no id, only what bears on
/// the decision.
fn picture(
    account: &Account,
    load: usize,
    history: Option<&VecDeque<(i64, f64, f64)>>,
    t: i64,
) -> Value {
    json!({
        "users_own_account": account.tags.contains(OWN),
        "tags": account.tags.iter().filter(|tag| tag.as_str() != OWN).collect::<Vec<_>>(),
        "five_hour_window": pace(account, "5h", 5 * 3600, t),
        "seven_day_window": pace(account, "7d", 7 * 24 * 3600, t),
        "sessions_using_it": load,
        "recent_five_hour_percent": history.map(|h| h.iter().rev().take(6).rev().map(|p| p.1.round()).collect::<Vec<_>>()),
    })
}

fn summary(account: &Account, load: usize) -> String {
    format!(
        "{}5-hour window {:.0}% used, 7-day {:.0}% used, {} session(s) on it",
        if account.tags.contains(OWN) {
            "the user's own account, "
        } else {
            ""
        },
        account.used("5h"),
        account.used("7d"),
        load
    )
}

/// What to ask about a running session, and which letter stands for which account.
type Round = (Value, Value, Vec<(String, Account)>);

impl Daemon {
    fn advisor_key(&self) -> Option<String> {
        self.vault
            .get(KEY_ID)
            .ok()
            .flatten()
            .filter(|key| !key.is_empty())
    }

    pub(crate) fn advisor_settings(&self) -> AdvisorSettings {
        file::load(&self.policy_path())
            .map(|p| p.advisor)
            .unwrap_or_default()
    }

    /// Count one question against today's ceiling; false when it is reached.
    pub(crate) fn spend_call(&self, g: &mut Inner, settings: &AdvisorSettings) -> bool {
        let today = Local::now().format("%Y-%m-%d").to_string();
        if g.p.advisor_day != today {
            g.p.advisor_day = today;
            g.p.advisor_calls = 0;
        }
        if g.p.advisor_calls >= settings.max_calls_per_day {
            return false;
        }
        g.p.advisor_calls += 1;
        true
    }

    /// The accounts this session could move to right now.
    fn movable(&self, g: &Inner, m: &Managed, accounts: &[Account]) -> Vec<Account> {
        let default = self.default_profile().identity();
        accounts
            .iter()
            .filter(|a| {
                a.usable()
                    && a.provider == m.session.provider
                    && m.session.account_id.as_ref() != Some(&a.id)
                    && a.peak_used() < 100.0
            })
            .filter(|a| {
                self.holders_given(g, a, &default)
                    .iter()
                    .all(|held| Some(held) == m.store.as_ref())
            })
            .cloned()
            .collect()
    }

    fn round_for(&self, g: &Inner, m: &Managed, settings: &Settings) -> Option<Round> {
        let t = now();
        let accounts = self.accounts(g).ok()?;
        let current = accounts
            .iter()
            .find(|a| Some(&a.id) == m.session.account_id.as_ref())?;
        let options = self.movable(g, m, &accounts);
        if options.is_empty() {
            return None;
        }
        let load = busy(g);
        let on = |a: &Account| load.get(&a.id).copied().unwrap_or(0);
        let letters: Vec<(String, Account)> = options
            .into_iter()
            .take(8)
            .enumerate()
            .map(|(i, a)| (((b'A' + i as u8) as char).to_string(), a))
            .collect();
        let mut candidates = serde_json::Map::new();
        let mut criteria = serde_json::Map::new();
        for (letter, account) in &letters {
            candidates.insert(
                letter.clone(),
                picture(account, on(account), g.p.history.get(&account.id), t),
            );
            criteria.insert(
                letter.clone(),
                json!(format!(
                    "Move to account {letter}: {}.",
                    summary(account, on(account))
                )),
            );
        }
        criteria.insert(
            "stay".into(),
            json!("Keep the session on its current account."),
        );
        let state = json!({
            "situation": "A coding-agent session draws on one subscription account at a time. Each account has a 5-hour and a 7-day usage window; at 100% the session is blocked until that window resets.",
            "current_account": picture(current, on(current), g.p.history.get(&current.id), t),
            "other_accounts": candidates,
            "costs_and_preferences": [
                "Switching account makes the next message more expensive once, so a switch that is not needed is a loss.",
                "An account shared by several sessions is spent faster than its numbers alone suggest.",
                if settings.prefer_own { "The user's own account is preferred over the others whenever it has room." } else { "No account is preferred over another." },
            ],
        });
        let questions = json!({
            "switch": {
                "type": "noul",
                "instructions": "Should this session move to one of the other accounts now, rather than stay on its current account? Yes only if the current account is likely to be blocked before its window resets and one of the others is clearly better placed."
            },
            "target": {
                "type": "choice",
                "instructions": "If the session were to continue for the next hour, which choice serves it best?",
                "criteria": criteria,
            },
            "urgency": {
                "type": "score",
                "instructions": "How soon does this session need to move off its current account?",
                "criteria": ["It can stay; no move is needed.", "It should move within the next few minutes.", "It should move now."],
            },
        });
        Some((state, questions, letters))
    }

    /// Ask the advisor about one session and, if it is confident enough and
    /// the guardrails agree, act. Runs without the daemon's lock while the
    /// advisor thinks, so nothing else waits on it.
    pub fn advise_session(&self, sid: &str) -> Api<Value> {
        let policy = file::load(&self.policy_path()).map_err(|e| fail(400, e.to_string()))?;
        let key = self
            .advisor_key()
            .ok_or_else(|| fail(409, "the advisor has no key yet; run `llmpool advisor key`"))?;
        let (state, questions, letters) = {
            let mut g = self.lock();
            let m =
                g.p.sessions
                    .get(sid)
                    .cloned()
                    .ok_or_else(|| fail(404, "no such session"))?;
            let round = self
                .round_for(&g, &m, &policy.settings)
                .ok_or_else(|| fail(409, "there is no other account this session could move to, so there is nothing to ask"))?;
            if !self.spend_call(&mut g, &policy.advisor) {
                return Err(fail(429, "today's ceiling on advisor questions is reached"));
            }
            // Marked as asked before the answer comes, so it is not asked twice.
            if let Some(m) = g.p.sessions.get_mut(sid) {
                m.advice = Some(Advice {
                    at: now(),
                    ..Advice::default()
                });
            }
            round
        };
        let answered = ask(&policy.advisor, &key, &state, &questions);

        let mut g = self.lock();
        let mut advice = Advice {
            at: now(),
            ..Advice::default()
        };
        let mut move_to = None;
        match answered {
            Err(e) => advice.error = Some(e),
            Ok(answers) => {
                // Two views of the same question: how likely "yes, move" is, and
                // how firmly "stay" is rejected among the choices. The model
                // spreads its confidence across both, so they are averaged.
                let yes = number(&answers["switch"], &["noul", "probability", "value"]);
                let not_stay = answers["target"]["probabilities"]["stay"]
                    .as_f64()
                    .map(|stay| 1.0 - stay);
                advice.switch = match (yes, not_stay) {
                    (Some(yes), Some(not_stay)) => Some((yes + not_stay) / 2.0),
                    (yes, _) => yes,
                };
                advice.urgency = number(&answers["urgency"], &["score", "value"]);
                let chosen = answers["target"]["choice"]
                    .as_str()
                    .and_then(|letter| letters.iter().find(|(l, _)| l == letter));
                advice.target = chosen.map(|(_, account)| account.label.clone());
                // Confident enough, a real destination, and not inside the cooldown.
                let sure = advice
                    .switch
                    .is_some_and(|p| p >= policy.advisor.switch_above);
                let rested = g.p.sessions.get(sid).is_some_and(|m| {
                    m.session
                        .last_swap_at
                        .is_none_or(|at| now() - at >= policy.settings.cooldown_secs)
                });
                if let (true, true, Some((_, account))) = (sure, rested, chosen) {
                    move_to = Some(account.id.clone());
                }
            }
        }
        if let Some(id) = move_to {
            let reason = format!(
                "advisor, {:.0}% sure",
                advice.switch.unwrap_or_default() * 100.0
            );
            match self.swap(&mut g, sid, &id, &reason) {
                Ok(()) => advice.acted = true,
                Err(e) => advice.error = Some(e.message),
            }
        }
        if let Some(m) = g.p.sessions.get_mut(sid) {
            m.advice = Some(advice.clone());
        }
        self.save(&g);
        Ok(serde_json::to_value(&advice)?)
    }

    /// Find one session worth asking about and ask. Called on a timer; cheap
    /// when the advisor is off or nothing is close to a limit.
    pub fn advise_due(&self) {
        let settings = self.advisor_settings();
        if !settings.enabled || self.advisor_key().is_none() {
            return;
        }
        let due = {
            let g = self.lock();
            let t = now();
            g.p.sessions.iter().find_map(|(id, m)| {
                m.store.as_ref()?;
                let current = g.store.get(m.session.account_id.as_ref()?).ok().flatten()?;
                let fresh = m
                    .advice
                    .as_ref()
                    .is_some_and(|a| t - a.at < settings.min_interval_secs);
                (!fresh && current.peak_used() >= WORTH_ASKING).then(|| id.clone())
            })
        };
        if let Some(sid) = due {
            let _ = self.advise_session(&sid);
        }
    }

    /// Which account a new session should start on, when the rule that fired
    /// leaves that to the advisor. `None`: no advisor, or it did not answer;
    /// the rule's fallback (most quota) then stands.
    pub(crate) fn advise_start(
        &self,
        candidates: &[Account],
        load: &BTreeMap<AccountId, usize>,
        policy: &PolicyFile,
        history: &BTreeMap<AccountId, VecDeque<(i64, f64, f64)>>,
    ) -> Option<AccountId> {
        let key = self.advisor_key()?;
        if !policy.advisor.enabled || candidates.len() < 2 {
            return None;
        }
        let t = now();
        let on = |a: &Account| load.get(&a.id).copied().unwrap_or(0);
        let letters: Vec<(String, &Account)> = candidates
            .iter()
            .take(8)
            .enumerate()
            .map(|(i, a)| (((b'A' + i as u8) as char).to_string(), a))
            .collect();
        let mut accounts = serde_json::Map::new();
        let mut criteria = serde_json::Map::new();
        for (letter, account) in &letters {
            accounts.insert(
                letter.clone(),
                picture(account, on(account), history.get(&account.id), t),
            );
            criteria.insert(
                letter.clone(),
                json!(format!(
                    "Start on account {letter}: {}.",
                    summary(account, on(account))
                )),
            );
        }
        let state = json!({
            "situation": "A new coding-agent session is about to start and must be put on one subscription account. Each account has a 5-hour and a 7-day usage window; at 100% a session on it is blocked until that window resets.",
            "accounts": accounts,
            "preferences": [
                if policy.settings.prefer_own { "The user's own account is preferred whenever it has room." } else { "No account is preferred over another." },
                "Spreading sessions across accounts keeps any one of them from being spent too fast.",
            ],
        });
        let questions = json!({"start_on": {"type": "choice", "instructions": "Which account should this new session start on so that it can work longest without being blocked?", "criteria": criteria}});
        let answers = ask(&policy.advisor, &key, &state, &questions).ok()?;
        let letter = answers["start_on"]["choice"].as_str()?;
        letters
            .iter()
            .find(|(l, _)| l == letter)
            .map(|(_, account)| account.id.clone())
    }

    // ---- turning it on and off -------------------------------------------

    pub fn advisor_status(&self) -> Api<Value> {
        let settings = self.advisor_settings();
        let g = self.lock();
        let today = Local::now().format("%Y-%m-%d").to_string();
        Ok(json!({
            "enabled": settings.enabled,
            "provider": settings.provider,
            "has_key": self.advisor_key().is_some(),
            "switch_above": settings.switch_above,
            "calls_today": if g.p.advisor_day == today { g.p.advisor_calls } else { 0 },
            "max_calls_per_day": settings.max_calls_per_day,
        }))
    }

    /// The switches every surface shows: own-account preference and advisor.
    pub fn config(&self) -> Api<Value> {
        let policy = file::load(&self.policy_path()).unwrap_or_default();
        let mut view = self.advisor_status()?;
        view["prefer_own"] = json!(policy.settings.prefer_own);
        view["own_until"] = json!(policy.settings.own_until);
        view["advisor_enabled"] = view["enabled"].clone();
        Ok(view)
    }

    pub fn set_advisor_key(&self, key: Option<&str>) -> Api<Value> {
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(key) => self.vault.put(KEY_ID, key)?,
            None => self.vault.remove(KEY_ID)?,
        }
        self.advisor_status()
    }

    /// Change the advisor's settings, or the own-account preference, leaving
    /// the rest of the policy file, comments included, exactly as it is.
    pub fn configure(&self, change: &Value) -> Api<Value> {
        let _g = self.lock();
        let path = self.policy_path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut policy = file::checked(&text)
            .map_err(|e| fail(400, format!("the policy file needs fixing first: {e}")))?;
        let mut text = text;
        if change.get("advisor_enabled").is_some() || change.get("switch_above").is_some() {
            if let Some(on) = change["advisor_enabled"].as_bool() {
                policy.advisor.enabled = on;
            }
            if let Some(p) = change["switch_above"].as_f64() {
                policy.advisor.switch_above = p;
            }
            let a = &policy.advisor;
            let mut block = format!(
                "[advisor]\nenabled = {}\nprovider = \"{}\"\nswitch_above = {}\nmin_interval_secs = {}\nmax_calls_per_day = {}\n",
                a.enabled, a.provider, a.switch_above, a.min_interval_secs, a.max_calls_per_day
            );
            if let Some(url) = &a.url {
                block += &format!("url = \"{url}\"\n");
            }
            text = replace_table(&text, "advisor", &block);
        }
        if change.get("prefer_own").is_some() || change.get("own_until").is_some() {
            if let Some(on) = change["prefer_own"].as_bool() {
                policy.settings.prefer_own = on;
            }
            if let Some(p) = change["own_until"].as_f64() {
                policy.settings.own_until = p;
            }
            let s = &policy.settings;
            let block = format!(
                "[settings]\ncooldown_secs = {}\nmargin_points = {}\nprefer_own = {}\nown_until = {}\n",
                s.cooldown_secs, s.margin_points, s.prefer_own, s.own_until
            );
            text = replace_table(&text, "settings", &block);
        }
        file::save(&path, &text).map_err(|e| fail(400, e.to_string()))?;
        Ok(
            json!({"prefer_own": policy.settings.prefer_own, "own_until": policy.settings.own_until, "advisor_enabled": policy.advisor.enabled, "switch_above": policy.advisor.switch_above}),
        )
    }
}

/// `text` with the table `[name]` replaced by `block`, or with `block` put
/// before the first rule when there is no such table. Everything else,
/// comments and layout included, is left untouched.
fn replace_table(text: &str, name: &str, block: &str) -> String {
    let header = format!("[{name}]");
    let lines: Vec<&str> = text.lines().collect();
    let is_header = |line: &str| line.trim_start().starts_with('[');
    let mut out: Vec<String> = Vec::new();
    match lines.iter().position(|l| l.trim() == header) {
        Some(start) => {
            let mut end = lines[start + 1..]
                .iter()
                .position(|l| is_header(l))
                .map_or(lines.len(), |i| start + 1 + i);
            // Comments and blank lines just above the next table introduce
            // that table, not this one; they stay.
            while end > start + 1
                && (lines[end - 1].trim().is_empty()
                    || lines[end - 1].trim_start().starts_with('#'))
            {
                end -= 1;
            }
            out.extend(lines[..start].iter().map(|l| l.to_string()));
            out.push(block.trim_end().to_string());
            // One blank line before whatever follows, unless it brings its own.
            if lines.get(end).is_some_and(|next| !next.trim().is_empty()) {
                out.push(String::new());
            }
            out.extend(lines[end..].iter().map(|l| l.to_string()));
        }
        None => {
            // Plain tables must come before the array of rules to read well.
            let first = lines
                .iter()
                .position(|l| is_header(l))
                .unwrap_or(lines.len());
            out.extend(lines[..first].iter().map(|l| l.to_string()));
            out.push(block.trim_end().to_string());
            out.push(String::new());
            out.extend(lines[first..].iter().map(|l| l.to_string()));
        }
    }
    let mut joined = out.join("\n");
    while joined.ends_with("\n\n") {
        joined.pop();
    }
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_is_replaced_in_place_and_comments_survive() {
        let text = "# my rules\n[settings]\ncooldown_secs = 60 # short\n\n# first rule\n[[rule]]\nname = 'x'\n";
        let out = replace_table(text, "settings", "[settings]\ncooldown_secs = 300\n");
        assert_eq!(
            out,
            "# my rules\n[settings]\ncooldown_secs = 300\n\n# first rule\n[[rule]]\nname = 'x'\n"
        );
        // A table that is not there yet goes before the rules.
        let out = replace_table(
            "# my rules\n[[rule]]\nname = 'x'\n",
            "advisor",
            "[advisor]\nenabled = true\n",
        );
        assert_eq!(
            out,
            "# my rules\n[advisor]\nenabled = true\n\n[[rule]]\nname = 'x'\n"
        );
        assert_eq!(
            replace_table("", "advisor", "[advisor]\nenabled = true\n"),
            "[advisor]\nenabled = true\n"
        );
    }

    #[test]
    fn request_ids_are_version_four_uuids() {
        let id = uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, uuid());
    }
}
