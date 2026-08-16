//! `a3net` — command-line interface to the A3Net runtime.
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
//! - `bitswap want|ls|stat|ledger|cancel|announce|providers` — Bitswap protocol
//! - `channel create|list|info|subscribe|unsubscribe|post|receive|history|invite` — pub/sub channels
//! - `name publish|resolve` — IPNS name management
//! - `key gen|list|rm|rename|export|import` — key management
//! - `share send|receive|resume` — P2P file sharing
//! - `storage info|usage|list|quota|reset` — storage management
//! - `status [--json]` — node status snapshot
//! - `config show|set|reset|edit` — configuration management
//! - `user add|show|list|delete|digit|info` — user profile management
//! - `roster add|list|search|show|delete|group-create|group-list|digit-add|digit-resolve|info` — contact directory
//! - `diagnostics [--json]` — diagnostics and peer info
//! - `bandwidth` — bandwidth statistics
//! - `profile get|set|delete` — profile management
//! - `news post|list|subscribe|receive` — news feed
//! - `moments post|list|receive` — moments/stories
//! - `mdns discover|announce` — mDNS discovery
//! - `pair create|list|revoke` — device-pairing invitations
//! - `invite render|text`     — email invitation rendering
//! - `qr render|parse`        — A3Net QR payload handling
//! - `mesh admit`             — closed-mesh admission queue
//! - `webhook list|save|test`  — webhook endpoint management
//!
//! Top-level commands (audit V6):

use clap::{Parser, Subcommand};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "a3net", version, about = "A3Net CLI demo")]
pub struct Cli {
    /// Path to the local data directory.
    #[arg(long, global = true, default_value = "./.a3net-data")]
    pub data_dir: String,

    /// Enable distributed tracing via OpenTelemetry.
    #[arg(long, global = true)]
    pub trace: bool,

    /// OTLP endpoint for trace export (e.g., http://localhost:4317).
    #[arg(long, global = true, value_name = "ENDPOINT")]
    pub trace_endpoint: Option<String>,

    /// Trace sampling ratio (0.0 to 1.0, default 1.0).
    #[arg(long, global = true, value_name = "RATIO")]
    pub trace_sample: Option<f64>,

    /// Log filter (e.g., info,a3net=debug).
    #[arg(long, global = true, value_name = "FILTER")]
    pub log_filter: Option<String>,

    /// Enable verbose console output.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Language for CLI output (en/zh-CN).
    #[arg(long, global = true, default_value = "en")]
    pub lang: String,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print this node's identity.
    Init,

    /// Start the mesh HTTP server and block.
    Serve {
        /// Optional address to expose the Prometheus `/metrics` and
        /// `/health` HTTP server on (e.g. `127.0.0.1:9090`). When
        /// omitted, the metrics server is not started.
        #[arg(long, value_name = "ADDR")]
        metrics_addr: Option<String>,
    },

    /// Start the Prometheus `/metrics` + `/health` HTTP server
    /// standalone. Useful when the gateway is running as a
    /// separate process and the operator wants a metrics surface
    /// without booting the mesh server.
    ///
    /// The bind address is configured via `--metrics-addr`
    /// (default `127.0.0.1:9090`).
    MetricsServer {
        #[arg(long, default_value = "127.0.0.1:9090")]
        metrics_addr: String,
    },

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
        /// `a3net pin add` on each result.
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
    /// Refuses blobs larger than 16 MiB — use `a3net get` for those.
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
    /// exposes but our existing `a3net dht {provide,find,peers,
    /// stats,ipns}` family doesn't).
    Dht {
        #[command(subcommand)]
        sub: DhtExtraCmd,
    },

    /// libp2p-style swarm connection management (Kubo `ipfs swarm`
    /// analogue). A3Net's QUIC transport does not expose a
    /// connection manager today, so this surfaces a clear
    /// error and points operators at `a3net diagnostics --json`
    /// and `a3net dht peers` for the supported surfaces.
    Swarm {
        #[command(subcommand)]
        sub: SwarmCmd,
    },

    /// Bitswap protocol operations.
    Bitswap {
        #[command(subcommand)]
        sub: BitswapCmd,
    },

    /// Information channels via gossip.
    Channel {
        #[command(subcommand)]
        sub: ChannelCmd,
    },

    /// IPNS name management.
    Name {
        #[command(subcommand)]
        sub: NameCmd,
    },

    /// Key management for IPNS.
    Key {
        #[command(subcommand)]
        sub: KeyCmd,
    },

    /// P2P file sharing via tickets.
    Share {
        #[command(subcommand)]
        sub: ShareCmd,
    },

    /// Storage scope management.
    Storage {
        #[command(subcommand)]
        sub: StorageCmd,
    },

    /// Node status snapshot (storage + replication).
    Status {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
        /// Compact single-line output.
        #[arg(long, short = 'c')]
        compact: bool,
        /// Watch mode: refresh every N seconds (0 = once).
        #[arg(long, value_name = "SECONDS")]
        watch: Option<u64>,
    },

    /// Configuration management.
    Config {
        #[command(subcommand)]
        sub: ConfigCmd,
    },

    /// User profile management.
    User {
        #[command(subcommand)]
        sub: UserCmd,
    },

    /// Node identity management (email / nickname / avatar / wallet / DNS id).
    ///
    /// This is the user-facing identity layer (see
    /// [`crate::identity_ops`]). Distinct from `Cmd::Profile`,
    /// which is the key-value configuration profile.
    Identity {
        #[command(subcommand)]
        sub: IdentityCmd,
    },

    /// Local contact list management (add / remove / rename / block / reputation).
    Contacts {
        #[command(subcommand)]
        sub: ContactsCmd,
    },

    /// Render the local node's profile page (HTML).
    ProfilePage {
        #[command(subcommand)]
        sub: ProfilePageCmd,
    },

    /// Contact directory management.
    Roster {
        #[command(subcommand)]
        sub: RosterCmd,
    },

    /// Diagnostics and peer information.
    Diagnostics {
        /// Emit JSON instead of human-readable output.
        #[arg(long, short)]
        json: bool,
    },

    /// Bandwidth statistics.
    Bandwidth {
        /// Emit JSON instead of human-readable output.
        #[arg(long, short)]
        json: bool,
    },

    /// Profile management.
    Profile {
        #[command(subcommand)]
        sub: ProfileCmd,
    },

    /// News feed operations.
    News {
        #[command(subcommand)]
        sub: NewsCmd,
    },

    /// Moments/stories operations.
    Moments {
        #[command(subcommand)]
        sub: MomentsCmd,
    },

    /// mDNS discovery operations.
    Mdns {
        #[command(subcommand)]
        sub: MdnsCmd,
    },

    /// Device pairing (issue / list / revoke pairing invitations).
    Pair {
        #[command(subcommand)]
        sub: PairCmd,
    },

    /// Build an email-ready invitation from a previously-issued
    /// pairing record.
    Invite {
        #[command(subcommand)]
        sub: InviteCmd,
    },

    /// Render / parse A3Net QR payloads (pairing, peer tickets,
    /// chatmail provisioning, …).
    Qr {
        #[command(subcommand)]
        sub: QrCmd,
    },

    /// Closed-mesh admission queue — pending `ray requests` style
    /// entries that the coordinator consumes.
    Mesh {
        #[command(subcommand)]
        sub: MeshCmd,
    },

    /// Manage webhook endpoints (`a3net-webhook`).
    Webhook {
        #[command(subcommand)]
        sub: WebhookCmd,
    },

    /// Ask a remote peer's AI agent a question over P2P.
    ///
    /// The CLI sends a JSON-RPC `agent.ask` request to the local daemon,
    /// which dials the target peer and forwards the question using the
    /// `agent.v1` P2P protocol. The peer's `NodeAgentBridge` dispatches
    /// it to the registered model and returns the response.
    ///
    /// The target peer must have the CLI operator's NodeId on their
    /// ACL allow-list. Use `a3net agent acl grant <node_id>` to
    /// pre-authorise a peer before using `ask`.
    Ask {
        /// 64-hex NodeId of the peer whose agent to query.
        peer: String,
        /// The question to ask the remote agent.
        question: String,
        /// Emit the full JSON-RPC envelope instead of a one-liner.
        #[arg(long)]
        json: bool,
    },
}

/// `a3net webhook <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum WebhookCmd {
    /// Load endpoints from a JSON file and print a summary.
    List {
        #[arg(value_name = "PATH")]
        config: PathBuf,
    },
    /// Save the current endpoint config to a JSON file. Reads
    /// from stdin (one endpoint per line, JSON) so operators
    /// can pipe `jq` / `curl` results in.
    Save {
        #[arg(value_name = "PATH")]
        output: PathBuf,
    },
    /// Send a synthetic event to the configured endpoints
    /// (one-shot smoke test against a deployed receiver).
    Test {
        #[arg(value_name = "PATH")]
        config: PathBuf,
        #[arg(long, default_value = "lobby")]
        room: String,
    },
}

/// `a3net pair <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum PairCmd {
    /// Issue a new pairing invitation and persist the issuer
    /// record under `<data_dir>/pairing/issuer.json`.
    Create {
        /// 64-char hex NodeId of the issuing node.
        #[arg(long)]
        node_id: Option<String>,
        /// Wallet file containing a secp256k1 private key in hex.
        #[arg(long, value_name = "PATH")]
        wallet_private: PathBuf,
        /// Time-to-live in seconds (default: 15 minutes).
        #[arg(long)]
        ttl_seconds: Option<i64>,
        /// Short human-readable note embedded in the invitation.
        #[arg(long)]
        note: Option<String>,
        /// Capability grants (repeatable). Defaults to nothing.
        #[arg(long = "cap", value_name = "CAP")]
        capabilities: Vec<String>,
        /// Emit the raw signed invitation JSON instead of the
        /// human-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// List trusted-device records persisted on disk.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Revoke a trusted-device record by `credential_id`.
    Revoke {
        #[arg(value_name = "CREDENTIAL_ID")]
        credential_id: String,
    },
}

/// `a3net invite <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum InviteCmd {
    /// Render a draft `.eml` body around the most recent pairing
    /// invitation that the local node has issued.
    Render {
        #[arg(long, default_value = "friend@example.com")]
        recipient: String,
        #[arg(long, default_value = "Pair with me on A3Net")]
        subject: String,
        /// Output path (defaults to
        /// `<data_dir>/invites/invite-<ts>.eml`).
        #[arg(long, value_name = "PATH")]
        output: Option<String>,
    },
    /// Print the human-readable summary of the most recent
    /// invitation the local node has issued.
    Text,
}

/// `a3net qr <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum QrCmd {
    /// Render the most recent issuer record as a typed QR payload
    /// (JSON by default).
    Render {
        /// Output path. Defaults to `<data_dir>/qr/pair-<id>.json`.
        #[arg(long, value_name = "PATH")]
        output: Option<String>,
        /// `json` (default) | `svg` | `txt`. The CLI only emits
        /// `json` directly; `svg`/`txt` are forwarded to the
        /// dedicated `a3net-qr` example binary.
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Read a text payload (e.g. a `qrcodegen`-style SVG) and
    /// decode the embedded `QrPayload`.
    Parse {
        #[arg(value_name = "PATH")]
        input: PathBuf,
    },
}

/// `a3net mesh <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum MeshCmd {
    /// Queue a `mesh admit` request for a node id in the
    /// closed-mesh flow. The coordinator consumes the resulting
    /// JSON file at `<data_dir>/mesh/pending.json`.
    Admit {
        #[arg(long)]
        node_id: String,
        /// Optional note shown to the operator in the approval UI.
        #[arg(long)]
        note: Option<String>,
    },
}

/// `a3net pin <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum PinCmd {
    /// Pin a CID (make it immune to GC).
    Add {
        cid: String,
        /// Recursively pin all descendants. The audit-V7 path
        /// walks the blob's chunk tree and records the chunk
        /// CIDs in `pin.json` as well, so the recursive GC
        /// pass knows to keep them alive.
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
    /// Sweep orphan `Chunk` pins whose parent `Root` is gone.
    /// Useful after a batch of `adbnet pin rm` operations.
    Gc,
}

/// `a3net repo <sub>` subcommands.
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
    /// reports the candidate count; the destructive flags
    /// require `--i-know-what-i-am-doing` to acknowledge.
    ///
    /// Modes:
    /// * no flag → dry-run report only.
    /// * `--prune-unpinned` (with `--i-know-what-i-am-doing`) →
    ///   actually drop every private-scope blob that is **not**
    ///   in `pin.json`. This is what audit-V7 wired up: the
    ///   `PinSet` finally feeds the `BlobStore::gc_orphans` path.
    /// * `--prune-all` (with `--i-know-what-i-am-doing`) → drop
    ///   every private-scope blob, pins be damned. Operator's
    ///   "reset" button.
    Gc {
        #[arg(long)]
        dry_run: bool,
        /// Drop unpinned blobs in the private scope.
        #[arg(long)]
        prune_unpinned: bool,
        /// Drop every blob in the private scope.
        #[arg(long)]
        prune_all: bool,
        /// Acknowledge the destructive flag.
        #[arg(long)]
        i_know_what_i_am_doing: bool,
        #[arg(long)]
        json: bool,
    },
    /// Verify every blob's metadata on disk.
    Verify {
        #[arg(long)]
        json: bool,
    },
}

/// `a3net routing <sub>` subcommands.
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

/// `a3net dht <sub>` subcommands (additive to the existing
/// `a3net dht {provide,find,peers,stats,ipns}` family).
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

/// `a3net swarm <sub>` subcommands.
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

// ════════════════════════════════════════════════════════════════════════════
// Additional command enums for Bitswap, Channel, Name, Key, Share, Storage,
// Config, User, Roster, Diagnostics, Bandwidth, Profile, News, Moments, Mdns
// ════════════════════════════════════════════════════════════════════════════

/// `a3net bitswap <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum BitswapCmd {
    /// Request a block from the network.
    Want {
        /// Content hash to request (CID or hex).
        cid: String,
        /// Priority (higher = more urgent).
        #[arg(short, long, default_value_t = 1)]
        priority: i32,
        /// Timeout in seconds.
        #[arg(short, long, default_value_t = 60)]
        timeout: u64,
    },
    /// List current wantlist entries.
    Ls {
        /// Show wants for a specific peer.
        #[arg(short, long)]
        peer: Option<String>,
        /// Show pending wants only.
        #[arg(short, long)]
        pending: bool,
        /// Limit output to N entries.
        #[arg(short, long)]
        limit: Option<usize>,
    },
    /// Show Bitswap statistics.
    Stat {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Show peer ledgers (bandwidth accounting).
    Ledger {
        /// Show ledger for a specific peer only.
        #[arg(short, long)]
        peer: Option<String>,
        /// Show detailed output.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Cancel a pending want.
    Cancel {
        /// Content hash to cancel.
        cid: String,
        /// Cancel for a specific peer.
        #[arg(short, long)]
        peer: Option<String>,
    },
    /// Announce local content to the network.
    Announce {
        /// Content hash to announce.
        cid: String,
        /// Also announce to DHT.
        #[arg(short, long)]
        dht: bool,
        /// Also announce via gossip.
        #[arg(short, long)]
        gossip: bool,
    },
    /// Find providers for content.
    Providers {
        /// Content hash to find providers for.
        cid: String,
        /// Maximum number of providers to return.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// List local blocks.
    ListLocal {
        /// Limit output to N entries.
        #[arg(short, long)]
        limit: Option<usize>,
    },
}

// ── Helpers for BitswapCmd ─────────────────────────────────────────────────

impl BitswapCmd {
    /// Returns the `--json` flag if present.
    pub fn json_flag(&self) -> bool {
        match self {
            BitswapCmd::Want { .. } => false,
            BitswapCmd::Ls { .. } => false,
            BitswapCmd::Stat { json } => *json,
            BitswapCmd::Ledger { .. } => false,
            BitswapCmd::Cancel { .. } => false,
            BitswapCmd::Announce { .. } => false,
            BitswapCmd::Providers { .. } => false,
            BitswapCmd::ListLocal { .. } => false,
        }
    }
}

/// `a3net channel <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum ChannelCmd {
    /// Create a new channel.
    Create {
        /// Channel name.
        name: String,
        /// Channel description.
        #[arg(long)]
        description: Option<String>,
        /// Make channel private.
        #[arg(long, short)]
        private: bool,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// List available channels.
    List {
        /// Limit output.
        #[arg(long)]
        limit: Option<u32>,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show channel info.
    Info {
        /// Channel id or name.
        #[arg(long)]
        channel: Option<String>,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Subscribe to a channel.
    Subscribe {
        /// Channel id or name.
        #[arg(long)]
        channel: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Unsubscribe from a channel.
    Unsubscribe {
        /// Channel id or name.
        #[arg(long)]
        channel: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Post a message to a channel.
    Post {
        /// Channel id or name.
        #[arg(long)]
        channel: String,
        /// Message content.
        #[arg(long)]
        message: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Receive messages from a channel.
    Receive {
        /// Channel id or name.
        #[arg(long)]
        channel: String,
        /// Timeout in seconds.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show channel history.
    History {
        /// Channel id or name.
        #[arg(long)]
        channel: String,
        /// Limit messages.
        #[arg(long)]
        limit: Option<u32>,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Invite someone to a channel.
    Invite {
        /// Channel id or name.
        #[arg(long)]
        channel: String,
        /// Target user id.
        #[arg(long)]
        target: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
}

// ── Helpers for ChannelCmd ─────────────────────────────────────────────────

impl ChannelCmd {
    pub fn channel(&self) -> Option<&String> {
        match self {
            ChannelCmd::Create { .. } => None,
            ChannelCmd::List { .. } => None,
            ChannelCmd::Info { channel, .. } => channel.as_ref(),
            ChannelCmd::Subscribe { channel, .. } => Some(channel),
            ChannelCmd::Unsubscribe { channel, .. } => Some(channel),
            ChannelCmd::Post { channel, .. } => Some(channel),
            ChannelCmd::Receive { channel, .. } => Some(channel),
            ChannelCmd::History { channel, .. } => Some(channel),
            ChannelCmd::Invite { channel, .. } => Some(channel),
        }
    }

    pub fn message(&self) -> Option<&String> {
        match self {
            ChannelCmd::Create { description, .. } => description.as_ref(),
            ChannelCmd::Post { message, .. } => Some(message),
            ChannelCmd::Invite { target, .. } => Some(target),
            _ => None,
        }
    }

    pub fn private(&self) -> bool {
        match self {
            ChannelCmd::Create { private, .. } => *private,
            _ => false,
        }
    }

    pub fn limit(&self) -> Option<u32> {
        match self {
            ChannelCmd::List { limit, .. } => *limit,
            ChannelCmd::History { limit, .. } => *limit,
            _ => None,
        }
    }

    pub fn timeout_secs(&self) -> u64 {
        match self {
            ChannelCmd::Receive { timeout, .. } => *timeout,
            _ => 60,
        }
    }

    pub fn json(&self) -> bool {
        match self {
            ChannelCmd::Create { json, .. } => *json,
            ChannelCmd::List { json, .. } => *json,
            ChannelCmd::Info { json, .. } => *json,
            ChannelCmd::Subscribe { json, .. } => *json,
            ChannelCmd::Unsubscribe { json, .. } => *json,
            ChannelCmd::Post { json, .. } => *json,
            ChannelCmd::Receive { json, .. } => *json,
            ChannelCmd::History { json, .. } => *json,
            ChannelCmd::Invite { json, .. } => *json,
        }
    }
}

impl From<ChannelCmd> for crate::channel_ops::ChannelSubcommand {
    fn from(cmd: ChannelCmd) -> Self {
        match cmd {
            ChannelCmd::Create { .. } => crate::channel_ops::ChannelSubcommand::Create,
            ChannelCmd::List { .. } => crate::channel_ops::ChannelSubcommand::List,
            ChannelCmd::Info { .. } => crate::channel_ops::ChannelSubcommand::Info,
            ChannelCmd::Subscribe { .. } => crate::channel_ops::ChannelSubcommand::Subscribe,
            ChannelCmd::Unsubscribe { .. } => crate::channel_ops::ChannelSubcommand::Unsubscribe,
            ChannelCmd::Post { .. } => crate::channel_ops::ChannelSubcommand::Post,
            ChannelCmd::Receive { .. } => crate::channel_ops::ChannelSubcommand::Receive,
            ChannelCmd::History { .. } => crate::channel_ops::ChannelSubcommand::History,
            ChannelCmd::Invite { .. } => crate::channel_ops::ChannelSubcommand::Invite,
        }
    }
}

/// `a3net name <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum NameCmd {
    /// Publish an IPNS name.
    Publish {
        /// Value to publish (typically /ipfs/<cid>).
        path: String,
        /// Record lifetime in seconds.
        #[arg(long)]
        lifetime: Option<u64>,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Resolve an IPNS name.
    Resolve {
        /// IPNS name to resolve.
        name: String,
        /// Recursive resolution.
        #[arg(long, short)]
        recursive: bool,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show local IPNS names.
    Local {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
}

/// `a3net key <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum KeyCmd {
    /// Generate a new key pair.
    Gen {
        /// Key name.
        name: String,
        /// Key type (ed25519, rsa, secp256k1).
        #[arg(long, default_value = "ed25519")]
        key_type: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// List local keys.
    List {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Remove a key.
    Rm {
        /// Key name to remove.
        name: String,
        /// Force removal.
        #[arg(long, short)]
        force: bool,
    },
    /// Rename a key.
    Rename {
        /// Old key name.
        old_name: String,
        /// New key name.
        new_name: String,
    },
    /// Export a key.
    Export {
        /// Key name to export.
        name: String,
        /// Output file.
        #[arg(long, short)]
        output: Option<String>,
    },
    /// Import a key.
    Import {
        /// Key name.
        name: String,
        /// Input file.
        #[arg(long, short)]
        input: Option<String>,
    },
}

/// `a3net share <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum ShareCmd {
    /// Send a file or directory to a peer.
    Send {
        /// Path to file or directory.
        path: String,
        /// Allow symbolic links.
        #[arg(long)]
        allow_symlinks: bool,
        /// Include hidden files.
        #[arg(long)]
        include_hidden: bool,
        /// Show manifest details.
        #[arg(long)]
        show_manifest: bool,
    },
    /// Receive a shared file or directory.
    Receive {
        /// Share ticket.
        ticket: String,
        /// Output directory.
        #[arg(long, short = 'o')]
        out_dir: Option<String>,
        /// Overwrite existing files.
        #[arg(long, short)]
        overwrite: bool,
    },
    /// Resume an interrupted receive.
    Resume {
        #[command(subcommand)]
        sub: ShareResumeCmd,
    },
}

/// `a3net share resume <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum ShareResumeCmd {
    /// List interrupted receives.
    Ls {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show receive info.
    Info {
        /// Short hash of the manifest.
        hash_short: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Clean up receive state.
    Clean {
        /// Short hash of the manifest.
        hash_short: String,
        /// Skip confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Continue interrupted receive.
    Continue {
        /// Short hash of the manifest.
        hash_short: String,
        /// Overwrite existing files.
        #[arg(long, short)]
        overwrite: bool,
    },
}

/// `a3net storage <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum StorageCmd {
    /// Show storage info.
    Info {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show storage usage.
    Usage {
        /// Filter by scope (private/shared).
        #[arg(long)]
        scope: Option<String>,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// List blobs.
    List {
        /// Filter by scope (private/shared).
        #[arg(long)]
        scope: Option<String>,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show or set storage quota.
    Quota {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
        /// Set the total budget (e.g. `"50GiB"`, `"10737418240"`).
        /// When supplied, the value is parsed and persisted into
        /// `topology.json` so the next CLI session picks it up.
        #[arg(long, value_name = "BYTES")]
        r#set: Option<String>,
        /// Override the private scope hard cap.
        #[arg(long, value_name = "BYTES")]
        set_private_hard_cap: Option<String>,
        /// Override the shared scope hard cap.
        #[arg(long, value_name = "BYTES")]
        set_shared_hard_cap: Option<String>,
        /// Set the private scope fraction (0.0..=1.0). The shared
        /// scope gets the complementary value.
        #[arg(long, value_name = "FRACTION")]
        set_private_fraction: Option<f64>,
        /// Seal the shared scope (no further writes from the
        /// replication protocol).
        #[arg(long)]
        seal: bool,
    },
    /// Reset/clear storage.
    Reset {
        /// Scope to reset.
        #[arg(long)]
        scope: String,
        /// Skip confirmation.
        #[arg(long, short)]
        yes: bool,
        /// Dry run.
        #[arg(long)]
        dry_run: bool,
        /// Acknowledge danger.
        #[arg(long)]
        i_know_what_i_am_doing: bool,
    },

    // ────────────────────────────────────────────────────────────────
    //  Encryption lifecycle (audit-V7). The private scope's chunks
    //  are sealed with XChaCha20-Poly1305; the master key lives
    //  in `<data_dir>/keys/storage.key` (mode 0600). These
    //  subcommands drive that lifecycle.
    // ────────────────────────────────────────────────────────────────
    /// Initialise the encryption subsystem. Generates a fresh
    /// random master key and writes it to
    /// `<data_dir>/keys/storage.key`. Idempotent — refuses to
    /// overwrite an existing key.
    EncryptInit {
        /// Use an Argon2id-derived key from a passphrase instead
        /// of a random key. The passphrase itself is **never**
        /// written to disk.
        #[arg(long, value_name = "PASSPHRASE")]
        passphrase: Option<String>,
        /// Overwrite an existing key file (DANGEROUS — destroys
        /// the old key and renders all prior ciphertexts
        /// unreadable).
        #[arg(long)]
        force: bool,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Print the encryption status (enabled / key_present / key_kind).
    EncryptStatus {
        #[arg(long)]
        json: bool,
    },
    /// Remove the on-disk key file. After this, any blob
    /// written under the old key can no longer be decrypted.
    EncryptDisable {
        /// Skip confirmation prompt.
        #[arg(long, short)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

/// `a3net config <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Show effective configuration.
    Show {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Set a configuration value.
    Set {
        /// Dot-separated key path.
        key: String,
        /// Value to set.
        value: String,
    },
    /// Reset to default configuration.
    Reset {
        /// Skip confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Edit configuration in $EDITOR.
    Edit,
    /// Validate configuration file.
    Validate {
        /// Config file path.
        #[arg(long)]
        file: Option<String>,
    },
    /// Interactive configuration wizard.
    Wizard,
}

// ── Helpers for ConfigCmd ──────────────────────────────────────────────────

impl ConfigCmd {
    pub fn file(&self) -> Option<&String> {
        match self {
            ConfigCmd::Show { .. } => None,
            ConfigCmd::Set { .. } => None,
            ConfigCmd::Reset { .. } => None,
            ConfigCmd::Edit => None,
            ConfigCmd::Validate { file } => file.as_ref(),
            ConfigCmd::Wizard => None,
        }
    }
}

/// `a3net identity <sub>` subcommands.
///
/// Identity is the local node's self-description (email, nickname,
/// avatar, wallet, DNS id, 128-char description). It is the
/// user-facing layer that other nodes see in gossip; distinct
/// from the routing key (`NodeId`) and from the key-value
/// `Cmd::Profile` configuration.
#[derive(Debug, Subcommand)]
pub enum IdentityCmd {
    /// Print the local node's identity as JSON.
    Get,

    /// Set the local node's email address.
    SetEmail {
        /// Email address (e.g. `alice@example.com`).
        email: String,
    },

    /// Set the local node's nickname.
    SetNickname {
        /// Display name (max 64 bytes).
        nickname: String,
    },

    /// Set the local node's 128-char description.
    SetDescription {
        /// Description (max 128 bytes).
        description: String,
    },

    /// Set the local node's avatar URL.
    SetAvatar {
        /// HTTPS URL of the avatar image.
        url: String,
    },

    /// Set the local node's wallet address.
    SetWallet {
        /// 0x-prefixed 20-byte hex (EVM-style).
        wallet: String,
    },

    /// Set the local node's DNS-assigned 12-digit id.
    SetDnsNodeId {
        /// 12-digit decimal id (e.g. `483726150931`).
        dns_node_id: String,
    },
}

/// `a3net contacts <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum ContactsCmd {
    /// List every contact in the local address book.
    List {
        /// Emit JSON instead of a human-readable table.
        #[arg(long, short)]
        json: bool,
    },

    /// Show a single contact by node id.
    Get {
        /// 64-hex-char node id.
        node_id: String,
    },

    /// Add a contact manually.
    Add {
        /// 64-hex-char node id.
        node_id: String,
        /// Local nickname (max 64 bytes).
        nickname: String,
    },

    /// Remove a contact from the address book.
    Remove {
        /// 64-hex-char node id.
        node_id: String,
    },

    /// Rename an existing contact (local nickname only).
    Rename {
        /// 64-hex-char node id.
        node_id: String,
        /// New nickname.
        nickname: String,
    },

    /// Block or unblock a contact.
    SetBlocked {
        /// 64-hex-char node id.
        node_id: String,
        /// `true` to block, `false` to unblock.
        #[arg(long, short)]
        blocked: bool,
    },

    /// Increase a contact's reputation. Saturates at MAX_REPUTATION.
    BumpReputation {
        /// 64-hex-char node id.
        node_id: String,
        /// Amount to add (0..=1000).
        delta: u32,
    },

    /// Set a contact's reputation to a specific value (0..=1000).
    SetReputation {
        /// 64-hex-char node id.
        node_id: String,
        /// Target reputation score.
        reputation: u32,
    },

    /// Print the aggregate reputation summary across the list.
    ReputationSummary {
        /// Emit JSON instead of `{:#?}` debug.
        #[arg(long, short)]
        json: bool,
    },
}

/// `a3net profile-page <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum ProfilePageCmd {
    /// Render the profile page and write it to a file.
    Render {
        /// Output HTML file path.
        out: String,
    },
    /// Render the profile page to stdout.
    Print,
}

/// `a3net user <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum UserCmd {
    /// Add a user profile.
    Add {
        /// Profile file (JSON) or stdin.
        #[arg(long)]
        from_file: Option<String>,
    },
    /// Show user profile.
    Show {
        /// User ID.
        #[arg(long)]
        user_id: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// List user profiles.
    List {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Delete a user profile.
    Delete {
        /// User ID.
        #[arg(long)]
        user_id: String,
        /// Skip confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Generate user digit.
    Digit {
        /// User ID.
        #[arg(long)]
        user_id: String,
    },
    /// Show user store info.
    Info,
}

/// `a3net roster <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum RosterCmd {
    /// Add a contact.
    Add {
        /// Contact file (JSON) or stdin.
        #[arg(long)]
        from_file: Option<String>,
    },
    /// List contacts.
    List {
        /// Filter by type.
        #[arg(long)]
        r#type: Option<String>,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Search contacts.
    Search {
        /// Search query.
        query: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show contact details.
    Show {
        /// Contact ID.
        #[arg(long)]
        contact_id: String,
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Delete a contact.
    Delete {
        /// Contact ID.
        #[arg(long)]
        contact_id: String,
        /// Skip confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Create a contact group.
    GroupCreate {
        /// Group file (JSON) or stdin.
        #[arg(long)]
        from_file: Option<String>,
    },
    /// List contact groups.
    GroupList {
        /// Emit JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Add a digit mapping.
    DigitAdd {
        /// Digit ID.
        #[arg(long)]
        digit_id: String,
        /// Node ID.
        #[arg(long)]
        node_id: String,
    },
    /// Resolve a digit to node.
    DigitResolve {
        /// Digit ID.
        #[arg(long)]
        digit_id: String,
    },
    /// Show roster info.
    Info,
}

/// `a3net profile <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum ProfileCmd {
    /// Get profile.
    Get {
        /// Profile key.
        key: Option<String>,
    },
    /// Set profile.
    Set {
        /// Profile key.
        key: String,
        /// Profile value.
        value: String,
    },
    /// Delete profile.
    Delete {
        /// Profile key.
        key: String,
    },
}

/// `a3net news <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum NewsCmd {
    /// Post a news item.
    Post {
        /// News content.
        content: String,
        /// Tags.
        #[arg(long)]
        tags: Option<String>,
    },
    /// List news items.
    List {
        /// Limit.
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Subscribe to news.
    Subscribe {
        /// Channel.
        channel: String,
    },
    /// Receive news.
    Receive {
        /// Channel.
        channel: String,
        /// Timeout.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

/// `a3net moments <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum MomentsCmd {
    /// Post a moment.
    Post {
        /// Content path.
        path: Option<String>,
        /// Caption.
        #[arg(long)]
        caption: Option<String>,
    },
    /// List moments.
    List {
        /// Limit.
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Receive moments.
    Receive {
        /// Channel.
        channel: String,
        /// Timeout.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

/// `a3net mdns <sub>` subcommands.
#[derive(Debug, Subcommand)]
pub enum MdnsCmd {
    /// Discover peers via mDNS.
    Discover {
        /// Timeout.
        #[arg(long, default_value_t = 10)]
        timeout: u64,
    },
    /// Announce via mDNS.
    Announce {
        /// Info string.
        info: Option<String>,
    },
}

// ── From conversions for StorageCmd ───────────────────────────────────────
// storage.rs has its own StorageCmd enum with the same variants.
// This impl bridges the clap-generated cli::StorageCmd to the module type.

impl From<&crate::cli::StorageCmd> for crate::storage::StorageCmd {
    fn from(cmd: &crate::cli::StorageCmd) -> Self {
        match cmd {
            crate::cli::StorageCmd::Info { .. } => crate::storage::StorageCmd::Info,
            crate::cli::StorageCmd::Usage { scope, json } => {
                crate::storage::StorageCmd::Usage {
                    scope: scope.as_ref().map(|s| {
                        match s.as_str() {
                            "private" | "priv" | "local" => crate::storage::ScopeArg::Private,
                            "shared" | "public" | "global" => crate::storage::ScopeArg::Shared,
                            _ => crate::storage::ScopeArg::Private,
                        }
                    }),
                    json: *json,
                }
            }
            crate::cli::StorageCmd::List { scope, json } => {
                crate::storage::StorageCmd::List {
                    scope: scope.as_ref().map(|s| {
                        match s.as_str() {
                            "private" | "priv" | "local" => crate::storage::ScopeArg::Private,
                            "shared" | "public" | "global" => crate::storage::ScopeArg::Shared,
                            _ => crate::storage::ScopeArg::Private,
                        }
                    }),
                    json: *json,
                }
            }
            crate::cli::StorageCmd::Quota {
                json,
                set,
                set_private_hard_cap,
                set_shared_hard_cap,
                set_private_fraction,
                seal,
            } => crate::storage::StorageCmd::Quota {
                json: *json,
                set: set.clone(),
                set_private_hard_cap: set_private_hard_cap.clone(),
                set_shared_hard_cap: set_shared_hard_cap.clone(),
                set_private_fraction: *set_private_fraction,
                seal: *seal,
            },
            crate::cli::StorageCmd::Reset {
                scope,
                yes,
                dry_run,
                i_know_what_i_am_doing,
            } => crate::storage::StorageCmd::Reset {
                scope: match scope.as_str() {
                    "shared" | "public" | "global" => crate::storage::ScopeArg::Shared,
                    _ => crate::storage::ScopeArg::Private,
                },
                yes: *yes,
                dry_run: *dry_run,
                i_know_what_i_am_doing: *i_know_what_i_am_doing,
            },
            crate::cli::StorageCmd::EncryptInit { passphrase, force, json } => {
                crate::storage::StorageCmd::EncryptInit {
                    passphrase: passphrase.clone(),
                    force: *force,
                    json: *json,
                }
            }
            crate::cli::StorageCmd::EncryptStatus { json } => {
                crate::storage::StorageCmd::EncryptStatus { json: *json }
            }
            crate::cli::StorageCmd::EncryptDisable { yes, json } => {
                crate::storage::StorageCmd::EncryptDisable {
                    yes: *yes,
                    json: *json,
                }
            }
        }
    }
}

