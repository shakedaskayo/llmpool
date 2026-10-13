//! Accounts that are keys rather than logins: any provider whose agent reads
//! its credential from the environment. llmpool hands the key to the agent it
//! starts; it cannot change it under a running agent, and it cannot read how
//! much of such an account has been used.

use super::*;
use llmpool_core::providers;

#[derive(Deserialize)]
pub struct KeyReq {
    pub provider: String,
    pub label: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Variable name to value, e.g. `OPENAI_API_KEY`.
    pub env: BTreeMap<String, String>,
}

pub(crate) fn id_of(provider: &str, label: &str) -> AccountId {
    format!("{provider}-{}", short_hash(label))
}

pub(crate) fn secret_of(env: &BTreeMap<String, String>) -> String {
    json!({ "env": env }).to_string()
}

/// The variables a stored key sets, checked: a stored secret can come from a
/// file or from a team's server.
pub(crate) fn env_of(secret: &str) -> Api<BTreeMap<String, String>> {
    let value: Value = serde_json::from_str(secret)
        .map_err(|_| fail(409, "this account's stored key is damaged; add it again"))?;
    let env: BTreeMap<String, String> = serde_json::from_value(value["env"].clone())
        .map_err(|_| fail(409, "this account's stored key is damaged; add it again"))?;
    if env.is_empty() {
        return Err(fail(409, "this account holds no key; add it again"));
    }
    for (name, value) in &env {
        if !providers::env_name_ok(name) {
            return Err(fail(
                400,
                format!("`{name}` is not a variable an account may set"),
            ));
        }
        if value.trim().is_empty() || value.contains('\0') {
            return Err(fail(400, format!("`{name}` has no value")));
        }
    }
    Ok(env)
}

impl Daemon {
    /// The providers llmpool knows the variables of, for forms.
    pub fn providers(&self) -> Value {
        json!(providers::KNOWN
            .iter()
            .map(|(name, title, env)| json!({"name": name, "title": title, "env": env}))
            .collect::<Vec<_>>())
    }

    /// Add (or replace the key of) a key account.
    pub fn add_key(&self, req: KeyReq) -> Api<Value> {
        let provider = req.provider.trim().to_lowercase();
        if !providers::name_ok(&provider) || provider == CLAUDE {
            return Err(fail(
                400,
                "the provider is a short lowercase name, e.g. openai, xai, gemini",
            ));
        }
        let env: BTreeMap<String, String> = req
            .env
            .into_iter()
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
        let env = env_of(&secret_of(&env))?;
        let label = req
            .label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| provider.clone());
        let id = id_of(&provider, &label);
        let g = self.lock();
        if g.p.shared.contains(&id) {
            return Err(fail(403, TEAM_OWNED));
        }
        let accounts = g.store.list()?;
        label_is_free(&accounts, &id, &label)?;
        let kind = if matches!(provider.as_str(), "bedrock" | "vertex") {
            Kind::Cloud
        } else {
            Kind::ApiKey
        };
        let mut account = g.store.get(&id)?.unwrap_or_else(|| Account {
            id: id.clone(),
            provider: provider.clone(),
            kind,
            label,
            email: None,
            organization: None,
            tags: BTreeSet::new(),
            enabled: true,
            health: Health::Ok,
            usage: None,
            login_expires_at: None,
        });
        account.health = Health::Ok;
        account.tags.extend(req.tags);
        self.vault.put(&id, &secret_of(&env))?;
        g.store.put(&account)?;
        Ok(serde_json::to_value(&account)?)
    }

    /// Put a session on a key account: the key goes into the environment of
    /// the agent that is about to start.
    pub(crate) fn attach_key(
        &self,
        g: &mut Inner,
        sid: &str,
        account: &Account,
    ) -> Api<BTreeMap<String, String>> {
        let text = self
            .vault
            .get(&account.id)?
            .ok_or_else(|| fail(409, "this account has no stored key; add it again"))?;
        let mut env = env_of(&text)?;
        env.insert("LLMPOOL_SESSION".into(), sid.to_string());
        if let Some(m) = g.p.sessions.get_mut(sid) {
            m.session.account_id = Some(account.id.clone());
        }
        self.log(
            g,
            "assign",
            Some(sid),
            format!("session started on {}", account.label),
        );
        Ok(env)
    }
}
