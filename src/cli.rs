use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Fast,
    Balanced,
    Best,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProviderArg {
    Anthropic,
    Openai,
    Ollama,
    #[value(alias = "gemini")]
    Google,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ClassifierCommands {
    /// Download the Laya model into DIR and check every file against the
    /// hashes built into this binary; changes no config (the nectra-laya package).
    Fetch {
        #[arg(long)]
        dir: PathBuf,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "nectra",
    version,
    about = "Route conversations to Claude Code, Codex CLI, Antigravity (agy), or Ollama"
)]
pub struct Cli {
    /// Show the real provider terminal in interactive chat, or use structured streams.
    #[arg(global = true, long, value_enum, default_value = "auto")]
    pub provider_view: crate::executor::native::ProviderView,
    /// Override the saved terminal theme for this launch.
    #[arg(global = true, long, value_enum)]
    pub theme: Option<crate::terminal::Theme>,
    /// Color human-readable output; NO_COLOR disables automatic coloring.
    #[arg(global = true, long, value_enum, default_value = "auto")]
    pub color: crate::terminal::ColorMode,
    /// Disable provider tool approvals for this launch (Codex also disables its sandbox).
    #[arg(global = true, long)]
    pub dangerously_skip_permissions: bool,
    #[command(subcommand)]
    pub command: Option<Commands>,
    /// Bias routing toward speed, balance, or maximum capability.
    #[arg(global = true, long, value_enum)]
    pub mode: Option<Mode>,
    /// Restrict routing to one provider.
    #[arg(global = true, long, value_enum)]
    pub provider: Option<ProviderArg>,
    /// Force one model id; see `nectra models` for the roster.
    #[arg(global = true, long)]
    pub model: Option<String>,
    /// Use this configuration file instead of the installed one.
    #[arg(global = true, long)]
    pub config: Option<PathBuf>,
    /// Give every provider this file as system context, in place of
    /// `[knowledge] index` (docs/reference.md).
    #[arg(global = true, long, value_name = "PATH")]
    pub knowledge: Option<PathBuf>,
    /// Print the routing decision and its basis to stderr before running.
    #[arg(global = true, long)]
    pub explain: bool,
    /// Show the routing decision as JSON without launching a provider.
    #[arg(global = true, long)]
    pub dry_run: bool,
    /// Chat: continue the latest conversation started in this directory.
    #[arg(global = true, short = 'c', long = "continue")]
    pub continue_latest: bool,
    /// Read the prompt from stdin (cannot be combined with positional text).
    #[arg(global = true, long)]
    pub stdin: bool,
    #[arg(trailing_var_arg = true)]
    pub prompt: Vec<String>,
}
#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Compare two providers inside read-only Linux containers (requires Docker).
    Debate {
        /// Verify debate participants are launched with no tools. Inspects
        /// launch arguments only; add --live to launch the real CLI.
        #[arg(long, conflicts_with = "session")]
        check_isolation: bool,
        /// With --check-isolation, launch the provider CLI and assert its
        /// reported tool inventory is empty. Makes one small model call.
        #[arg(long, requires = "check_isolation")]
        live: bool,
        /// Save the completed answer as a resumable session.
        #[arg(long)]
        session: Option<PathBuf>,
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Show available themes, or save a default theme.
    Theme {
        #[arg(value_enum)]
        name: Option<crate::terminal::Theme>,
    },
    /// Choose how messages are classified: `laya` downloads a free local model
    /// (about 750 MB, nothing leaves the machine), `auto` uses Jev when a
    /// TypeSafe key is set, `keywords` uses keywords only.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Classifier {
        #[arg(value_enum, required = true)]
        backend: Option<crate::config::ClassifierBackend>,
        #[command(subcommand)]
        command: Option<ClassifierCommands>,
        /// With `laya`: use this model directory instead of `classifier.model_dir`.
        #[arg(long)]
        model_dir: Option<PathBuf>,
        /// With `laya`: hash every file already in the model directory against its
        /// pin and write the verification stamp. Downloads nothing and leaves the
        /// configuration alone; run as root, since only root's stamp is trusted.
        #[arg(long)]
        verify_only: bool,
    },
    /// Configure an Ollama server and selected local models without prompts.
    Ollama {
        #[arg(long, default_value = "http://localhost:11434")]
        url: String,
        #[arg(long, value_delimiter = ',', required = true)]
        models: Vec<String>,
        #[arg(long, value_enum, default_value = "local-only")]
        policy: crate::ollama::LocalPolicy,
    },
    /// Add, remove, or list provider selections after installation.
    Provider {
        #[command(subcommand)]
        command: ProviderCommands,
    },
    /// Select provider access and check required CLIs (also run by installers).
    Setup {
        /// Comma-separated provider IDs; 'none' disables all providers.
        #[arg(long)]
        providers: Option<String>,
        /// Preserve an existing configuration without prompting.
        #[arg(long)]
        if_needed: bool,
    },
    /// Start one conversation with automatic routing on every message.
    Chat {
        /// Save a new session to this file (never overwrites an existing file).
        #[arg(long, conflicts_with = "resume")]
        session: Option<PathBuf>,
        /// Resume a saved Nectra conversation.
        #[arg(long)]
        resume: Option<PathBuf>,
    },
    /// Print the routing decision as JSON without launching an agent.
    Route {
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Route and execute a task; `--events jsonl` runs it unattended (docs/headless.md).
    Run {
        #[command(flatten)]
        headless: crate::headless::Args,
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Check configured executable paths and versions, without making model calls.
    Doctor,
    /// Show configuration path or effective config.
    Config {
        #[arg(long)]
        print: bool,
    },
    /// List configured models.
    Models,
    /// Evaluate task classification against a local JSON corpus (uses Jev when TYPESAFE_API_KEY is set).
    Eval { corpus: PathBuf },
    /// Tokens and API-equivalent cost per model across your Nectra chats, from
    /// the providers' own logs.
    Usage {
        /// Only chats active in the last N days.
        #[arg(long)]
        days: Option<u64>,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Count one chat only: a session file from `/session`.
        #[arg(long)]
        session: Option<PathBuf>,
        /// Also count Claude Code and Codex sessions you ran without Nectra, to compare.
        #[arg(long, conflicts_with = "session")]
        direct: bool,
        /// Price the same tokens at this model's rates, for comparison.
        #[arg(long, default_value = crate::usage::BASELINE)]
        baseline: String,
    },
    /// Which provider CLI has each of your MCP servers. Nectra shares local
    /// servers with Claude and Codex when it launches them.
    Mcp {
        /// Add every local server to each installed CLI's own config, Gemini included.
        #[arg(long)]
        sync: bool,
    },
    /// Which provider CLI sees each of your skills (Claude, Codex and Gemini each
    /// read different folders).
    Skills {
        /// Link your user skills between ~/.claude/skills and ~/.agents/skills so
        /// every CLI sees them. Never replaces an existing entry.
        #[arg(long)]
        link: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProviderCommands {
    /// Open the provider CLI's interactive login/reauthentication workflow.
    Login { id: String },
    /// Add a provider while preserving other selections; validates required CLIs.
    Add { id: String },
    /// Exclude a provider from future routing (does not uninstall its software).
    Remove { id: String },
    /// List provider IDs, selections, and implementation status.
    List,
}
