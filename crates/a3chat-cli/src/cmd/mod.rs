//! CLI subcommands.
//!
//! Each module exposes a `<Name>Cmd` enum (clap subcommand) and a
//! `pub async fn run(cmd, cfg, client) -> CliResult<()>` entry point.
//! The dispatch table in [`crate::run`] mirrors this list — adding a
//! new module requires (a) declaring it here, (b) adding a
//! `#[command(subcommand)]` variant to [`crate::Cmd`], and (c) a
//! `match` arm in [`crate::run`]. The compile errors will point at
//! any missing step.

pub mod audit;
pub mod bundle;
pub mod chat;
pub mod completions;
pub mod config;
pub mod contact;
pub mod conversation;
pub mod doctor;
pub mod group;
pub mod link;
pub mod media;
pub mod message;
pub mod moderation;
pub mod moments;
pub mod presence;
pub mod profile;
pub mod repl;
pub mod rpc;
pub mod stream;
pub mod sync;
pub mod trace;
pub mod schema;
pub mod whoami;
