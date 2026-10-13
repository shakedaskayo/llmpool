//! What llmpool knows about providers whose accounts are keys rather than
//! logins. A provider not listed here works too: say which environment
//! variables its agent reads.

/// (name, how people write it, the variables its key travels in).
pub const KNOWN: &[(&str, &str, &[&str])] = &[
    ("anthropic", "Anthropic API", &["ANTHROPIC_API_KEY"]),
    ("openai", "OpenAI", &["OPENAI_API_KEY"]),
    ("xai", "xAI (Grok)", &["XAI_API_KEY"]),
    ("gemini", "Google Gemini", &["GEMINI_API_KEY"]),
    ("openrouter", "OpenRouter", &["OPENROUTER_API_KEY"]),
    ("groq", "Groq", &["GROQ_API_KEY"]),
    ("mistral", "Mistral", &["MISTRAL_API_KEY"]),
    ("deepseek", "DeepSeek", &["DEEPSEEK_API_KEY"]),
    (
        "bedrock",
        "Amazon Bedrock",
        &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_REGION"],
    ),
    (
        "azure",
        "Azure OpenAI",
        &["AZURE_OPENAI_API_KEY", "AZURE_OPENAI_ENDPOINT"],
    ),
];

/// Agents whose provider is not in doubt, by the command that starts them.
const AGENTS: &[(&str, &str)] = &[("codex", "openai"), ("gemini", "gemini"), ("grok", "xai")];

pub fn env_of(provider: &str) -> Option<&'static [&'static str]> {
    KNOWN
        .iter()
        .find(|(name, ..)| *name == provider)
        .map(|(.., env)| *env)
}

/// Where a provider's API lives, for a gateway that forwards to it. `claude`
/// is Anthropic's API reached with a subscription login.
pub fn upstream_of(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "claude" | "anthropic" => "https://api.anthropic.com",
        "openai" => "https://api.openai.com",
        "xai" => "https://api.x.ai",
        "gemini" => "https://generativelanguage.googleapis.com",
        "openrouter" => "https://openrouter.ai/api",
        "groq" => "https://api.groq.com/openai",
        "mistral" => "https://api.mistral.ai",
        "deepseek" => "https://api.deepseek.com",
        _ => return None,
    })
}

/// The provider an agent talks to, when its command name settles it.
pub fn of_agent(command: &str) -> Option<&'static str> {
    let name = command.rsplit('/').next().unwrap_or(command);
    AGENTS
        .iter()
        .find(|(agent, _)| *agent == name)
        .map(|(_, provider)| *provider)
}

const ENDINGS: &[&str] = &[
    "_API_KEY",
    "_KEY",
    "_TOKEN",
    "_ACCESS_KEY_ID",
    "_BASE_URL",
    "_API_BASE",
    "_ENDPOINT",
    "_REGION",
    "_LOCATION",
    "_PROFILE",
    "_PROJECT",
    "_PROJECT_ID",
    "_ORG_ID",
    "_ORGANIZATION",
    "_DEPLOYMENT",
    "_API_VERSION",
];

/// Whether an account may set this variable for an agent. An account can come
/// from a team's server, so this is a list of what a credential looks like,
/// not of what is known to be dangerous: `PATH`, `LD_PRELOAD`, `NODE_OPTIONS`
/// and the like can never pass.
pub fn env_name_ok(name: &str) -> bool {
    let shaped = name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    shaped
        && ENDINGS.iter().any(|end| name.ends_with(end))
        && !["LLMPOOL_", "CLAUDE_", "LD_", "DYLD_", "GIT_", "SSH_"]
            .iter()
            .any(|p| name.starts_with(p))
}

/// A provider or agent name safe to file an account under.
pub fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_use_variables_that_pass_their_own_rule() {
        for (provider, _, env) in KNOWN {
            assert!(name_ok(provider));
            assert!(env.iter().all(|name| env_name_ok(name)), "{provider}");
        }
        assert_eq!(env_of("xai"), Some(&["XAI_API_KEY"][..]));
        assert_eq!(env_of("nobody"), None);
    }

    #[test]
    fn only_credential_shaped_variables_may_be_set() {
        for bad in [
            "PATH",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "NODE_OPTIONS",
            "PYTHONPATH",
            "HOME",
            "GIT_SSH_KEY",
            "LLMPOOL_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "lower_api_key",
            "A=B_KEY",
            "",
        ] {
            assert!(!env_name_ok(bad), "{bad}");
        }
        assert!(env_name_ok("MY_PROXY_BASE_URL"));
        assert!(env_name_ok("AWS_SESSION_TOKEN"));
    }

    #[test]
    fn agents_name_their_provider() {
        assert_eq!(of_agent("codex"), Some("openai"));
        assert_eq!(of_agent("/opt/bin/grok"), Some("xai"));
        assert_eq!(of_agent("aider"), None);
    }
}
