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
        /// Put what is added straight into the team's pool (administrators)
        #[arg(long, global = true)]
        to_team: bool,
    },
    /// Sign in again to an account whose login has ended
    Login { account: String },
    /// Check that everything is in place, and say how to fix what is not
    Doctor,
    /// The optional decision layer: show it, turn it on or off, set its key
    Advisor {
        #[command(subcommand)]
        command: Option<AdvisorCommand>,
    },
    /// Turn llmpool off: agents start on your own login and nothing switches them
    Off,
    /// Turn llmpool back on
    On,
    /// Prefer your own account (tagged `own`) whenever it has room: on or off
    PreferOwn {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
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
    /// Run any agent (codex, gemini, grok, opencode, aider, ...) on a key
    /// from the pool
    Exec {
        /// Use this account
        #[arg(long, conflicts_with = "tag")]
        account: Option<String>,
        /// Use the least busy account with this tag
        #[arg(long)]
        tag: Option<String>,
        /// Whose key the agent needs, when its name does not say
        #[arg(long)]
        provider: Option<String>,
        /// The command to run and its arguments
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// The coding agents llmpool can start, and what it found of each
    Agents {
        #[arg(long)]
        json: bool,
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
    /// Set this Mac up in one go: command line tool, Claude Code hooks and
    /// mod, your current login, and the app starting at login
    Setup {
        /// Take everything `setup` did back out
        #[arg(long)]
        undo: bool,
    },
    /// Whether the llmpool app starts when you log in: on, off or status
    LoginItem {
        #[arg(value_parser = ["on", "off", "status"])]
        state: String,
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
    /// A team's shared pool: connect to its server, see the link, leave
    Team {
        #[command(subcommand)]
        command: Option<TeamCommand>,
    },
    /// Serve this machine's pool to a team (configured by LLMPOOL_* variables;
    /// see the README)
    Serve {
        /// Address to listen on; put TLS in front of it
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: String,
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
enum TeamCommand {
    /// Connect to a team's server by signing in, or with an API key
    Connect {
        /// The server's address, e.g. https://pool.example.com
        url: String,
        /// Use an API key instead of signing in (typed or piped, never an argument)
        #[arg(long)]
        key: bool,
        /// Start the sign-in and return at once, without waiting for it
        #[arg(long)]
        detach: bool,
    },
    /// Join a team's pool in one go: connect, have new sessions use it, and
    /// make plain `claude` start through it. What an invitation runs
    Join {
        /// The server's address, e.g. https://pool.example.com
        url: String,
        /// Use an API key instead of signing in (typed or piped, never an argument)
        #[arg(long)]
        key: bool,
    },
    /// Put accounts of this machine's into the team's pool (administrators).
    /// A login moves there; a key is copied
    Share {
        #[arg(required = true)]
        accounts: Vec<String>,
        /// Also remove a key from this machine once the pool has it
        #[arg(long)]
        r#move: bool,
    },
    /// Fetch the team's accounts and rules now
    Sync,
    /// Leave the team's pool
    Disconnect,
}

#[derive(Subcommand)]
enum AdvisorCommand {
    /// Let the advisor suggest and make switches
    On,
    /// Rules alone decide
    Off,
    /// Store the advisor's API key in the keychain (typed or piped, never an argument)
    Key {
        /// Forget the stored key
        #[arg(long)]
        clear: bool,
    },
    /// Ask the advisor about a session right now and show what it says
    Ask {
        #[arg(long)]
        session: Option<String>,
    },
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
        /// Pass on the code the sign-in page shows, for a sign-in already started
        #[arg(long)]
        code: Option<String>,
        /// Give up on the sign-in that is waiting in the browser
        #[arg(long)]
        cancel: bool,
        /// Sign in inside Claude Code's own screen instead of the browser flow
        #[arg(long)]
        interactive: bool,
    },
    /// The login another agent (codex, opencode, ...) has now
    Login {
        agent: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Sign the agent in to another account first, without touching the
        /// login it has now
        #[arg(long)]
        new: bool,
    },
    /// A key for any provider (openai, xai, gemini, anthropic, openrouter,
    /// bedrock, ...): typed or piped, never an argument
    Key {
        provider: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// The variable the agent reads the key from, for a provider llmpool
        /// does not know; repeat for several
        #[arg(long = "env")]
        env: Vec<String>,
    },
}

#[derive(Subcommand)]
enum InstallAgent {
    Claude {
        /// Also make plain `claude` go through `llmpool run`
        #[arg(long)]
        shim: bool,
    },
    /// Make the plain command of every other agent with an account in the
    /// pool (codex, opencode, ...) start through the pool
    Agents,
}

#[derive(Subcommand)]
enum UninstallAgent {
    Claude,
    Agents,
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
