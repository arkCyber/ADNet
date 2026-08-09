//! `adnet` — command-line interface to the ADNet runtime.
//!
//! Subcommands:
//! - `init`      — print a fresh node id + show data dir
//! - `serve`     — start the mesh HTTP server, blocking
//! - `announce`  — import a file and announce it into a room
//! - `feed`      — print the room feed (assets + peer sources)
//! - `echo`      — publish a synthetic announcement and exit (smoke test)
//! - `run`       — start the node and drop into an interactive `/cmd` REPL

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "adnet", version, about = "ADNet CLI demo")]
pub struct Cli {
    /// Path to the local data directory.
    #[arg(long, global = true, default_value = "./.adnet-data")]
    pub data_dir: String,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print this node's identity.
    Init,

    /// Start the mesh HTTP server and block.
    Serve,

    /// Import a file and announce it into a room.
    Announce {
        /// Room / lobby id.
        #[arg(long)]
        room: String,
        /// Path to the local file to share.
        #[arg(long)]
        file: String,
        /// Display title.
        #[arg(long, default_value = "shared file")]
        title: String,
        /// Kind (article, ai_model, video_model, dataset, generic_file).
        #[arg(long, default_value = "generic_file")]
        kind: String,
    },

    /// Print the current room feed.
    Feed {
        #[arg(long)]
        room: String,
    },

    /// Publish a synthetic announcement and exit (smoke test).
    Echo {
        #[arg(long)]
        room: String,
    },

    /// Start the node and enter an interactive `/cmd` REPL.
    ///
    /// Once the node is up, type `/help` to list the slash commands.
    /// Anything that does not start with `/` is logged as a free-form
    /// note (handy for leaving breadcrumbs during a debugging session).
    Run,
}
