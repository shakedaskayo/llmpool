mod commands;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "llmpool",
    version,
    about = "A pool of LLM provider credentials for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add an account to the pool
    Add {
        #[command(subcommand)]
        agent: AddAgent,
    },
    /// Sign in again to an account whose login has ended
    Login { account: String },
    /// Check that everything is in place, and say how to fix what is not
    Doctor,
    /// Show the accounts in the pool with their usage
    List {
        #[arg(long)]
        json: bool,
    },
    /// Add tags to an account
    Tag { account: String, tags: Vec<String> },
    /// Remove tags from an account
    Untag { account: String, tags: Vec<String> },
    /// Return an account to rotation
    Enable { account: String },
    /// Hold an account out of rotation
    Disable { account: String },
    /// Remove an account from the pool
    Remove { account: String },
    /// Start Claude Code on the account a policy picks
    Run {
        /// Use this account, skipping policies
        #[arg(long, conflicts_with = "tag")]
        account: Option<String>,
        /// Use the account with the most quota among those with this tag
        #[arg(long)]
        tag: Option<String>,
        /// Arguments passed to the agent
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Show running sessions
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// Move a running session to another account
    Switch {
        /// Account to switch to
        #[arg(required_unless_present = "next")]
        account: Option<String>,
        /// Switch to the usable account with the most quota left
        #[arg(long, conflicts_with = "account")]
        next: bool,
        /// Session id; defaults to the current or only session
        #[arg(long)]
        session: Option<String>,
    },
    /// Write a session's own login into its profile afresh, without changing account
    Repair {
        /// Session id; defaults to the current or only session
        #[arg(long)]
        session: Option<String>,
    },
    /// Show the current session's account and usage
    Status {
        #[arg(long)]
        json: bool,
        /// Claude Code's own session id, for a session `llmpool run` did not start
        #[arg(long)]
        agent_session: Option<String>,
    },
    /// Save every account and its login to one passphrase-encrypted file
    Export { file: PathBuf },
    /// Add the accounts of an exported file to this machine's pool, or
    /// `import claude-swap` to bring over what claude-swap holds
    Import {
        file: PathBuf,
        /// Also overwrite accounts that are already here
        #[arg(long)]
        replace: bool,
    },
    /// Inspect and test policies
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Add llmpool's hooks and status line to an agent
    Install {
        #[command(subcommand)]
        agent: InstallAgent,
    },
    /// Remove what `install` added
    Uninstall {
        #[command(subcommand)]
        agent: UninstallAgent,
    },
    /// Run or stop the background daemon
    Daemon {
        /// Stop the running daemon
        #[arg(long)]
        stop: bool,
    },
    /// Called by agent hooks; not for direct use
    #[command(hide = true)]
    Hook {
        agent: String,
        event: String,
        argument: Option<String>,
    },
    /// Called as the agent's status line; not for direct use
    #[command(hide = true)]
    Statusline { agent: String },
}

#[derive(Subcommand)]
enum AddAgent {
    /// A Claude subscription login
    Claude {
        /// Capture the login Claude Code already has, instead of a new one
        #[arg(long)]
        from_current: bool,
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Pre-fill this address on the sign-in page
        #[arg(long)]
        email: Option<String>,
        /// Start the sign-in and return at once, without waiting for it
        #[arg(long)]
        detach: bool,
        /// Give up on the sign-in that is waiting in the browser
        #[arg(long)]
        cancel: bool,
        /// Sign in inside Claude Code's own screen instead of the browser flow
        #[arg(long)]
        interactive: bool,
    },
}

#[derive(Subcommand)]
enum InstallAgent {
    Claude {
        /// Also make plain `claude` go through `llmpool run`
        #[arg(long)]
        shim: bool,
    },
}

#[derive(Subcommand)]
enum UninstallAgent {
    Claude,
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Validate a policy file
    Check { file: PathBuf },
    /// Show which rule fires for a scenario described in a JSON file
    DryRun { file: PathBuf, scenario: PathBuf },
    /// Print where the active policy file lives
    Path,
}

fn main() -> ExitCode {
    match commands::run(Cli::parse()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("llmpool: {message}");
            ExitCode::FAILURE
        }
    }
}
