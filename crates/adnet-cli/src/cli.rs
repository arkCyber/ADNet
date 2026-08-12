//! `adnet` — command-line interface to the ADNet runtime.
//!
//! Top-level commands (audit V6):
//! - `init`            — print a fresh node id + show data dir
//! - `serve`           — start the mesh HTTP server, blocking
//! - `announce`        — import a file and announce it into a room
//! - `feed`            — print the room feed (assets + peer sources)
//! - `echo`            — publish a synthetic announcement and exit (smoke test)
//! - `run`             — start the node and drop into an interactive `/cmd` REPL
//! - `add <path>`      — ingest a file or directory (Kubo `ipfs add`)
//! - `get <cid> [-o]`  — download a blob to disk (Kubo `ipfs get`)
//! - `cat <cid>`       — dump a blob to stdout (Kubo `ipfs cat`)
//! - `ls <cid>`        — list a HAMT/directory root (Kubo `ipfs ls`)
//! - `pin add|rm|ls|verify <cid>` — pin lifecycle (Kubo `ipfs pin`)
//! - `repo stat|ls|gc|verify`     — repository inspection (Kubo `ipfs repo`)
//! - `routing findprovs|findpeer|get|put` — DHT routing (Kubo `ipfs routing`)
//! - `dht findpeer|query|get|put` — DHT peers (Kubo `ipfs dht`)
//! - `swarm peers|connect|disconnect|addrs|filters` — libp2p swarm (Kubo `ipfs swarm`)

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

    /// Ingest a file or directory into the local blob store
    /// (Kubo `ipfs add` analogue). Writes to the **private** scope
    /// by default; pass `--pin` to record the resulting CIDs in
    /// `pin.json` so they survive GC.
    Add {
        /// Path to the local file or directory.
        path: String,
        /// Recurse into directories. Without this flag, `add`
        /// refuses directories unless `--wrap-in-dir` is set.
        #[arg(long, short)]
        recursive: bool,
        /// Wrap a single file in a directory manifest before
        /// returning the root. Mirrors `ipfs add -w`.
        #[arg(long, short = 'w')]
        wrap_in_dir: bool,
        /// Pin every added CID. Equivalent to running
        /// `adnet pin add` on each result.
        #[arg(long, short)]
        pin: bool,
        /// Emit a JSON summary instead of the human-readable form.
        #[arg(long)]
        json: bool,
    },

    /// Download a CID's bytes to disk (Kubo `ipfs get` analogue).
    /// Works against either the private or shared scope; the
    /// shared scope wins when both contain the same CID.
    Get {
        /// The CID to fetch.
        cid: String,
        /// Output path. Defaults to `./<short-hash>` in the
        /// current directory.
        #[arg(long, short)]
        output: Option<String>,
        /// Emit a JSON envelope instead of a plain `saved` line.
        #[arg(long)]
        json: bool,
    },

    /// Dump a CID's bytes to stdout (Kubo `ipfs cat` analogue).
    /// Refuses blobs larger than 16 MiB — use `adnet get` for those.
    Cat {
        /// The CID to print.
        cid: String,
        /// Emit `{ cid, size, bytes_b64 }` instead of raw bytes.
        #[arg(long)]
        json: bool,
    },

    /// List a CID's directory entries (Kubo `ipfs ls` analogue).
    /// For a single-blob CID, prints one line summarising the blob.
    Ls {
        /// The CID to list.
        cid: String,
        /// Emit a JSON array of `{name, cid, size, chunk_count}`.
        #[arg(long)]
        json: bool,
    },

    /// Pin / unpin / list / verify CIDs in the local blob store
    /// (Kubo `ipfs pin` analogue). Pins are persisted to
    /// `<data_dir>/pin.json`.
    Pin {
        #[command(subcommand)]
        sub: PinCmd,
    },

    /// Repository inspection / GC (Kubo `ipfs repo` analogue).
    Repo {
        #[command(subcommand)]
        sub: RepoCmd,
    },

    /// DHT routing commands (Kubo `ipfs routing` analogue).
    Routing {
        #[command(subcommand)]
        sub: RoutingCmd,
    },

    /// DHT peer lookups (Kubo `ipfs dht` analogue — the
    /// `findpeer / query / get / put` surfaces that `ipfs dht`
    /// exposes but our existing `adnet dht {provide,find,peers,
    /// stats,ipns}` family doesn't).
    Dht {
        #[command(subcommand)]
        sub: DhtExtraCmd,
    },

    /// libp2p-style swarm connection management (Kubo `ipfs swarm`
    /// analogue). ADNet's QUIC transport does not expose a
    /// connection manager today, so this surfaces a clear
    /// error and points operators at `adnet diagnostics --json`
    /// and `adnet dht peers` for the supported surfaces.
    Swarm {
        #[command(subcommand)]
        sub: SwarmCmd,
    },
}

/// `adnet pin <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum PinCmd {
    /// Pin a CID (make it immune to GC).
    Add {
        cid: String,
        /// Recursively pin all descendants. Currently a no-op
        /// flag — the audit keeps it for UX parity with `ipfs
        /// pin add`.
        #[arg(long, short, default_value_t = true)]
        recursive: bool,
    },
    /// Remove a pin.
    Rm { cid: String },
    /// List every pin (or a single pin when `--cid` is supplied).
    Ls {
        #[arg(long, short)]
        cid: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Verify that a pinned CID still exists in the local store.
    Verify { cid: String },
}

/// `adnet repo <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum RepoCmd {
    /// Print repository size (per scope).
    Stat {
        #[arg(long)]
        json: bool,
    },
    /// List every blob hash in the local store.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Garbage-collect orphans. Defaults to a dry run that
    /// reports the candidate count; without a PinService we
    /// refuse to drop anything for real.
    Gc {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Verify every blob's metadata on disk.
    Verify {
        #[arg(long)]
        json: bool,
    },
}

/// `adnet routing <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum RoutingCmd {
    /// Find providers for a CID.
    FindProvs {
        cid: String,
        /// Cap the number of providers to print.
        #[arg(long, short)]
        num: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Find the closest peers to a target PeerID by XOR distance.
    FindPeer {
        peer_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Placeholder for the wire-level `ipfs routing get` surface.
    Get {
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Placeholder for the wire-level `ipfs routing put` surface.
    Put {
        key: String,
        value: String,
        #[arg(long)]
        json: bool,
    },
}

/// `adnet dht <sub>` subcommands (additive to the existing
/// `adnet dht {provide,find,peers,stats,ipns}` family).
#[derive(Debug, Subcommand)]
pub enum DhtExtraCmd {
    /// Closest-peers lookup by XOR distance.
    FindPeer {
        peer_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Generic iterative-query entry point.
    Query {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Placeholder for the wire-level `ipfs dht put` surface.
    Put {
        key: String,
        value: String,
        #[arg(long)]
        json: bool,
    },
    /// Placeholder for the wire-level `ipfs dht get` surface.
    Get {
        key: String,
        #[arg(long)]
        json: bool,
    },
}

/// `adnet swarm <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum SwarmCmd {
    /// List currently-connected peers.
    Peers {
        #[arg(long)]
        json: bool,
    },
    /// Dial a peer by multiaddr.
    Connect { addr: String },
    /// Close the connection to a peer.
    Disconnect { peer_id: String },
    /// List our own listen addresses.
    Addrs {
        #[arg(long)]
        json: bool,
    },
    /// Inspect connection filters.
    Filters {
        #[arg(long)]
        json: bool,
    },
}
