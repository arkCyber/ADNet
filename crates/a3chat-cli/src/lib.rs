//! `a3chat-cli` — operator CLI for the a3chat distributed communication
//! stack, built to aerospace-grade (DO-178C) standards.
//!
//! ## Subcommands
//!
//! | Subcommand | Purpose |
//! |------------|---------|
//! | `whoami`           | Print the configured local owner identity |
//! | `doctor`           | Probe the local daemon (HTTP /rpc/health, owner-aware) |
//! | `conversation`     | `list` / `open` conversations |
//! | `message`          | `send` / `ack` / `recall` / `edit` / `delete` / `search` / `typing` / `forward` / `forward-merge` |
//! | `sync`             | `snapshot` / `delta` / `compressed` (multi-device catch-up) |
//! | `profile`          | profile / public-key / device / avatar operators |
//! | `chat`             | interactive multi-turn conversation session (slash commands + SSE) — wraps `thread.list/get`, `chat.tap` |
//! | `contact`          | friend / blocklist / QR-invite (13 sub-operations) |
//! | `group`            | group creation, invitation, membership, mute, nickname (29 sub-ops) |
//! | `moments`          | 朋友圈 post / comment / reaction / follow (15 sub-ops) |
//! | `link`             | link bookmark / favorites (14 sub-ops) |
//! | `media`            | blob upload / download / health |
//! | `moderation`       | content / attachment policy gate |
//! | `presence`         | publish / subscribe presence |
//! | `bundle`           | export / import E2E state bundle |
//! | `stream`           | subscribe / unsubscribe / list event streams |
//! | `audit`            | offline static / live / full audit of the a3chat API surface |
//! | `config`           | `show` / `path` — inspect the resolved config |
//! | `trace`            | SSE event subscription |
//! | `rpc`              | raw JSON-RPC fallback — call any `a3chat.*` method |
//! | `repl`             | interactive REPL — read commands from stdin |
//! | `completions`      | generate shell completion script |
//!
//! ## DO-178C mappings
//!
//! * **Traceability (§5.2)** — every RPC carries a `X-A3Chat-Request-Id`
//!   header that is mirrored in the response and in tracing logs.
//! * **Determinism (§6.1)** — every mutating command honors `--dry-run`.
//! * **Fail-safe (§6.3)** — transport errors (`ErrorClass::Transient`) are
//!   retried with exponential backoff (3 attempts, 100/300/900 ms).
//! * **Reproducibility (§7.2)** — sync snapshots are hashed with SHA-256
//!   and the hash is written next to the snapshot file as a sidecar.
//! * **Defensive programming (§8)** — all inputs validated by
//!   `a3chat-core::validation` before they reach the wire.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod audit_report;
pub mod cmd;
pub mod config;
pub mod error;
pub mod lockfile;
pub mod output;
pub mod rpc_client;

pub use config::{CliConfig, OutputFormat};
pub use error::{CliError, CliResult};
pub use output::{print_json, print_table, Formatter};
pub use rpc_client::{HttpRpcClient, RpcClientBuilder};

use clap::{Parser, Subcommand};
use std::process::ExitCode;

/// Top-level `a3chat` CLI structure. `clap` derives `Parser` so we can
/// extend subcommands without touching `main.rs`.
#[derive(Debug, Parser)]
#[command(
    name = "a3chat",
    version,
    about = "Operator CLI for the a3chat distributed communication stack.",
    long_about = None
)]
pub struct Cli {
    /// Override the path to the config file. Defaults to
    /// `${XDG_CONFIG_HOME:-~/.config}/a3chat/config.toml`.
    #[arg(long, global = true, env = "A3CHAT_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// Override the daemon base URL (e.g. `http://127.0.0.1:53421`).
    #[arg(long, global = true, env = "A3CHAT_DAEMON_URL")]
    pub daemon_url: Option<String>,

    /// Override the local owner identity (hex NodeId).
    #[arg(long, global = true, env = "A3CHAT_OWNER")]
    pub owner: Option<String>,

    /// Output format override. Falls back to the config file, then `table`.
    #[arg(long, global = true, value_enum, env = "A3CHAT_OUTPUT")]
    pub output: Option<OutputFormat>,

    /// Number of retry attempts for transient RPC errors. Default: 3.
    #[arg(long, global = true, default_value_t = 3)]
    pub retries: u32,

    /// Print the resolved config and exit (debug aid).
    #[arg(long, global = true)]
    pub print_config: bool,

    /// Increase tracing verbosity (`-v` info, `-vv` debug, `-vvv` trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Cmd,
}

/// All `a3chat` subcommands. Keep this exhaustive — every new
/// command should be added here AND to the dispatch table in
/// [`run`].
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print the configured local owner identity.
    Whoami,
    /// Probe the running daemon and report its health.
    Doctor,
    /// Conversation commands.
    #[command(subcommand)]
    Conversation(cmd::conversation::ConversationCmd),
    /// Message commands.
    #[command(subcommand)]
    Message(cmd::message::MessageCmd),
    /// Sync (multi-device) commands.
    #[command(subcommand)]
    Sync(cmd::sync::SyncCmd),
    /// Profile commands (a3net-userstore bridge).
    #[command(subcommand)]
    Profile(cmd::profile::ProfileCmd),
    /// Interactive multi-turn conversation session (slash commands + SSE).
    Chat(cmd::chat::ChatOptions),
    /// Contact commands (friend / blocklist / QR-invite).
    #[command(subcommand)]
    Contact(cmd::contact::ContactCmd),
    /// Group commands (create / invite / membership / mute / nickname).
    #[command(subcommand)]
    Group(cmd::group::GroupCmd),
    /// Moments commands (朋友圈 — post / comment / reaction / follow).
    #[command(subcommand)]
    Moments(cmd::moments::MomentsCmd),
    /// Link bookmark commands (favorites / folders / search).
    #[command(subcommand)]
    Link(cmd::link::LinkCmd),
    /// Media commands (blob upload / download / health).
    #[command(subcommand)]
    Media(cmd::media::MediaCmd),
    /// Moderation commands (content / attachment policy gate).
    #[command(subcommand)]
    Moderation(cmd::moderation::ModerationCmd),
    /// Presence commands (publish / subscribe).
    #[command(subcommand)]
    Presence(cmd::presence::PresenceCmd),
    /// Bundle commands (export / import E2E state bundle).
    #[command(subcommand)]
    Bundle(cmd::bundle::BundleCmd),
    /// Stream commands (subscribe / unsubscribe / list event streams).
    #[command(subcommand)]
    Stream(cmd::stream::StreamCmd),
    /// Trace SSE events pushed by the daemon.
    #[command(subcommand)]
    Trace(cmd::trace::TraceCmd),
    /// Raw JSON-RPC fallback — call any `a3chat.*` method.
    #[command(subcommand)]
    Rpc(cmd::rpc::RpcCmd),
    /// Interactive REPL — read commands from stdin.
    Repl,
    /// Generate shell completion script for bash/zsh/fish/powershell.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Audit subcommands (static, live, full).
    #[command(subcommand)]
    Audit(cmd::audit::AuditCmd),
    /// Config introspection.
    #[command(subcommand)]
    Config(cmd::config::ConfigCmd),
    /// Dump the a3chat-core JSON Schema document (or one named definition).
    /// Pure offline — does not talk to a daemon. Useful in CI for codegen.
    Schema(cmd::schema::SchemaArgs),
}

/// Run the parsed CLI. Returns an exit code (0 success, 1 user error,
/// 2 internal / RPC error) so callers can use it from `main`.
pub async fn run(cli: Cli) -> ExitCode {
    // 1. Load config + apply CLI overrides.
    let cfg = match config::CliConfig::load(cli.config.as_deref()) {
        Ok(c) => c.apply_overrides(&cli),
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::from(1);
        }
    };

    if cli.print_config {
        println!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
        return ExitCode::SUCCESS;
    }

    // 2. Build the HTTP RPC client (fail-fast on bad owner).
    let client = match rpc_client::RpcClientBuilder::new(&cfg).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rpc client error: {e}");
            return ExitCode::from(2);
        }
    };

    // 3. Dispatch.
    let result = match cli.command {
        Cmd::Whoami => cmd::whoami::run(&cfg).await,
        Cmd::Doctor => cmd::doctor::run(&cfg, &client).await,
        Cmd::Conversation(c) => cmd::conversation::run(c, &cfg, &client).await,
        Cmd::Message(c) => cmd::message::run(c, &cfg, &client).await,
        Cmd::Sync(c) => cmd::sync::run(c, &cfg, &client).await,
        Cmd::Profile(p) => cmd::profile::run(p, &cfg, &client).await,
        Cmd::Chat(opts) => cmd::chat::run(&cfg, &client, opts).await,
        Cmd::Contact(c) => cmd::contact::run(c, &cfg, &client).await,
        Cmd::Group(c) => cmd::group::run(c, &cfg, &client).await,
        Cmd::Moments(c) => cmd::moments::run(c, &cfg, &client).await,
        Cmd::Link(c) => cmd::link::run(c, &cfg, &client).await,
        Cmd::Media(c) => cmd::media::run(c, &cfg, &client).await,
        Cmd::Moderation(c) => cmd::moderation::run(c, &cfg, &client).await,
        Cmd::Presence(c) => cmd::presence::run(c, &cfg, &client).await,
        Cmd::Bundle(c) => cmd::bundle::run(c, &cfg, &client).await,
        Cmd::Stream(c) => cmd::stream::run(c, &cfg, &client).await,
        Cmd::Trace(c) => cmd::trace::run(c, &cfg, &client).await,
        Cmd::Rpc(c) => cmd::rpc::run(c, &cfg, &client).await,
        Cmd::Repl => cmd::repl::run(&cfg, &client).await,
        Cmd::Completions { shell } => cmd::completions::run(shell),
        Cmd::Audit(c) => cmd::audit::run(c, &cfg, &client).await,
        Cmd::Config(c) => cmd::config::run(c, &cfg),
        Cmd::Schema(c) => cmd::schema::run(c),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // DO-178C §6.3 — `error_class()` lets us pick the
            // right exit code without parsing messages.
            use crate::error::CliError;
            let code = match &e {
                CliError::Usage(_) => 2, // user did something wrong
                CliError::Rpc(rpc) if rpc.is_retryable() => 75, // EX_TEMPFAIL (transient)
                CliError::Rpc(_) => 1,   // daemon rejected it
                CliError::Config(_) | CliError::Internal(_) => 70, // EX_SOFTWARE
                CliError::Io(_) => 73,   // EX_CANTCREAT
                CliError::Crypto(_) => 77, // security failure
            };
            eprintln!("error: {e}");
            if let Some(s) = e.suggestion() {
                eprintln!("hint:  {s}");
            }
            ExitCode::from(code as u8)
        }
    }
}