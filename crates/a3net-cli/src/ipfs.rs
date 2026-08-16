//! `a3net ipfs` — IPFS-compatible commands.
//!
//! Provides IPFS-compatible CLI commands for:
//! - DAG operations (`dag put`, `dag get`, `dag resolve`)
//! - Block operations (`block put`, `block get`, `block rm`, `block stat`)
//! - Pin management (`pin add`, `pin rm`, `pin ls`, `pin verify`)
//! - Garbage collection (`gc`)
//! - DHT operations (`dht findprovs`, `dht provide`)
//! - IPNS operations (`name publish`, `name resolve`)
//! - Gateway management (`gateway serve`, `gateway status`)

use clap::Subcommand;

#[cfg(feature = "ipfs")]
mod ipfs_exec {
    #![allow(missing_docs)]
    include!("ipfs_exec.rs");
}

/// Execute IPFS commands.
#[cfg(feature = "ipfs")]
pub use ipfs_exec::run_ipfs_command;

/// IPFS-compatible commands.
#[derive(Debug, Subcommand)]
pub enum IpfsCmd {
    /// DAG operations (add, get, resolve DAG nodes).
    Dag {
        #[command(subcommand)]
        sub: DagCmd,
    },

    /// Raw block operations.
    Block {
        #[command(subcommand)]
        sub: BlockCmd,
    },

    /// Pin management (persistent storage).
    Pin {
        #[command(subcommand)]
        sub: PinCmd,
    },

    /// Garbage collection.
    Gc {
        /// Perform a dry run without actually removing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// DHT operations for content routing.
    Dht {
        #[command(subcommand)]
        sub: DhtCmd,
    },

    /// IPNS (mutable naming) operations.
    Name {
        #[command(subcommand)]
        sub: NameCmd,
    },

    /// Manage the HTTP gateway.
    Gateway {
        #[command(subcommand)]
        sub: GatewayCmd,
    },

    /// Cat: retrieve and print content.
    Cat {
        /// The CID or path to retrieve.
        arg: String,
    },

    /// Import / export a CAR (Content Addressable aRchive) file.
    Car {
        #[command(subcommand)]
        sub: CarCmd,
    },

    /// Resolve a DNSLink domain (`/ipns/<domain>` via the
    /// `_dnslink.<domain>` TXT record).
    Dns {
        /// Domain to resolve (without the leading `_dnslink.`).
        domain: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// DAG subcommands.
#[derive(Debug, Subcommand)]
pub enum DagCmd {
    /// Add a DAG node.
    Put {
        /// Path to the file to add as a DAG node.
        path: String,
        /// Store as a DAG node (default: raw block).
        #[arg(long, short)]
        dag: bool,
        /// Pin the content after adding.
        #[arg(long, short)]
        pin: bool,
    },

    /// Get a DAG node by CID.
    Get {
        /// The CID to retrieve.
        cid: String,
        /// Optional path within the DAG.
        #[arg(long, short)]
        path: Option<String>,
    },

    /// Resolve a DAG path.
    Resolve {
        /// The path to resolve (e.g., /ipfs/QmHash/some/path).
        path: String,
    },

    /// Import a file or directory as a UnixFS DAG.
    Import {
        /// Path to the file or directory to import.
        path: String,
        /// Wrap the content in a directory node.
        #[arg(long)]
        wrap: bool,
        /// Pin the content after importing.
        #[arg(long, short)]
        pin: bool,
    },
}

/// Block subcommands.
#[derive(Debug, Subcommand)]
pub enum BlockCmd {
    /// Add a raw block.
    Put {
        /// Path to the file to add as a block.
        path: String,
        /// Pin the block after adding.
        #[arg(long, short)]
        pin: bool,
    },

    /// Get a raw block by CID.
    Get {
        /// The CID to retrieve.
        cid: String,
    },

    /// Remove a block.
    Rm {
        /// The CID of the block to remove.
        cid: String,
        /// Force removal even if the block is pinned.
        #[arg(long, short)]
        force: bool,
    },

    /// Get statistics about a block.
    Stat {
        /// The CID to get statistics for.
        cid: String,
    },
}

/// Pin subcommands.
#[derive(Debug, Subcommand)]
pub enum PinCmd {
    /// Add a pin (make content persistent).
    Add {
        /// The CID to pin.
        cid: String,
        /// Recursively pin all descendants (default: true).
        #[arg(long, short, default_value_t = true)]
        recursive: bool,
    },

    /// Remove a pin.
    Rm {
        /// The CID of the pin to remove.
        cid: String,
    },

    /// List all pins.
    Ls {
        /// Filter by specific CID.
        #[arg(long, short)]
        cid: Option<String>,
    },

    /// Verify pin status.
    Verify {
        /// The CID to verify.
        cid: String,
    },
}

/// DHT subcommands.
#[derive(Debug, Subcommand)]
pub enum DhtCmd {
    /// Find providers for a CID.
    FindProvs {
        /// The CID to find providers for.
        cid: String,
        /// Number of providers to find.
        #[arg(long, short)]
        num_providers: Option<u32>,
    },

    /// Announce that we provide a CID.
    Provide {
        /// The CID to provide.
        cid: String,
    },
}

/// IPNS subcommands.
#[derive(Debug, Subcommand)]
pub enum NameCmd {
    /// Publish an IPNS record.
    Publish {
        /// The path to publish (e.g., /ipfs/QmHash).
        path: String,
        /// Time-to-live for the record in seconds.
        #[arg(long)]
        lifetime: Option<u64>,
    },

    /// Resolve an IPNS name.
    Resolve {
        /// The IPNS name to resolve.
        name: String,
        /// Recurse until the final content is reached.
        #[arg(long, short, default_value_t = true)]
        recursive: bool,
    },
}

/// Gateway subcommands.
#[derive(Debug, Subcommand)]
pub enum GatewayCmd {
    /// Start the HTTP gateway server.
    Serve {
        /// Address to bind to.
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: String,
        /// Enable CORS.
        #[arg(long)]
        cors: bool,
        /// Enable writable gateway.
        #[arg(long)]
        writable: bool,
    },

    /// Show gateway status.
    Status {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// CAR (Content Addressable aRchive) subcommands.
#[derive(Debug, Subcommand)]
pub enum CarCmd {
    /// Import a CAR file, storing every block in the local blob store.
    Import {
        /// Path to the .car file on disk.
        path: String,
        /// Pin the imported root(s) after import.
        #[arg(long, short)]
        pin: bool,
    },

    /// Export one or more CIDs (and their blocks) into a CAR file.
    Export {
        /// CIDs to export as the root set of the archive.
        cids: Vec<String>,
        /// Path to the .car file to write.
        #[arg(long, short)]
        out: String,
    },
}
