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
