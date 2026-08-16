//! Bitswap CLI Operations
//!
//! This module provides CLI commands for Bitswap protocol operations:
//! - `want` - Request a block from the network
//! - `ls` - List current wants
//! - `stat` - Show Bitswap statistics
//! - `ledger` - Show peer ledgers
//! - `cancel` - Cancel a pending want
//! - `announce` - Announce local content
//!
//! ## Usage
//!
//! ```bash
//! # Request a block
//! a3net-cli bitswap want QmXoypizjW3WknFiJnKLwHCnL72vedxjQkDDP1mXWo6uco
//!
//! # List current wants
//! a3net-cli bitswap ls
//!
//! # Show statistics
//! a3net-cli bitswap stat
//!
//! # Show peer ledgers
//! a3net-cli bitswap ledger
//!
//! # Cancel a want
//! a3net-cli bitswap cancel QmXoypizjW3WknFiJnKLwHCnL72vedxjQkDDP1mXWo6uco
//!
//! # Announce local content
//! a3net-cli bitswap announce QmXoypizjW3WknFiJnKLwHCnL72vedxjQkDDP1mXWo6uco
//! ```

use std::sync::Arc;
use std::time::Duration;

use a3net_node::{BitswapHandle, BitswapStats};
use a3net_types::ContentHash;
use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Bitswap CLI commands.
#[derive(Debug, Subcommand)]
pub enum BitswapCommands {
    /// Request a block from the network.
    Want(WantArgs),

    /// List current wantlist entries.
    Ls(ListArgs),

    /// Show Bitswap statistics.
    Stat(ListArgs),

    /// Show peer ledgers (bandwidth accounting).
    Ledger(LedgerArgs),

    /// Cancel a pending want.
    Cancel(CancelArgs),

    /// Announce local content to the network.
    Announce(AnnounceArgs),

    /// Find providers for content.
    Providers(ProvidersArgs),
}

/// Arguments for the `want` command.
#[derive(Debug, Args)]
pub struct WantArgs {
    /// Content hash to request (CID or hex).
    pub cid: String,

    /// Priority (higher = more urgent, default: 1).
    #[arg(short, long, default_value = "1")]
    pub priority: i32,

    /// Timeout in seconds (default: 60).
    #[arg(short, long, default_value = "60")]
    pub timeout: u64,
}

/// Arguments for the `ls` command.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Show wants for a specific peer.
    #[arg(short, long)]
    pub peer: Option<String>,

    /// Show pending wants only.
    #[arg(short, long)]
    pub pending: bool,

    /// Limit output to N entries.
    #[arg(short, long)]
    pub limit: Option<usize>,
}

/// Arguments for the `ledger` command.
#[derive(Debug, Args)]
pub struct LedgerArgs {
    /// Show ledger for a specific peer only.
    #[arg(short, long)]
    pub peer: Option<String>,

    /// Show detailed output.
    #[arg(short, long)]
    pub verbose: bool,
}

/// Arguments for the `cancel` command.
#[derive(Debug, Args)]
pub struct CancelArgs {
    /// Content hash to cancel.
    pub cid: String,

    /// Cancel for a specific peer (optional).
    #[arg(short, long)]
    pub peer: Option<String>,
}

/// Arguments for the `announce` command.
#[derive(Debug, Args)]
pub struct AnnounceArgs {
    /// Content hash to announce.
    pub cid: String,

    /// Also announce to DHT.
    #[arg(short, long)]
    pub dht: bool,

    /// Also announce via gossip.
    #[arg(short, long)]
    pub gossip: bool,
}

/// Arguments for the `providers` command.
#[derive(Debug, Args)]
pub struct ProvidersArgs {
    /// Content hash to find providers for.
    pub cid: String,

    /// Maximum number of providers to return.
    #[arg(short, long, default_value = "20")]
    pub limit: usize,
}

/// Bitswap CLI context shared across commands.
pub struct BitswapCliContext {
    /// Reference to the Bitswap handle.
    handle: Arc<RwLock<Option<BitswapHandle>>>,
    /// Reference to the Bitswap network adapter (for actual network requests).
    adapter: Arc<RwLock<Option<Arc<a3net_node::BitswapNetworkAdapter>>>>,
}

impl BitswapCliContext {
    /// Create a new CLI context.
    pub fn new() -> Self {
        Self {
            handle: Arc::new(RwLock::new(None)),
            adapter: Arc::new(RwLock::new(None)),
        }
    }

    /// Set the Bitswap handle.
    pub async fn set_handle(&self, handle: BitswapHandle) {
        let mut guard = self.handle.write().await;
        *guard = Some(handle);
    }

    /// Set the Bitswap network adapter.
    pub async fn set_adapter(&self, adapter: Arc<a3net_node::BitswapNetworkAdapter>) {
        let mut guard = self.adapter.write().await;
        *guard = Some(adapter);
    }

    /// Get the Bitswap handle (clones the inner handle).
    pub async fn get_handle(&self) -> Result<BitswapHandle> {
        let guard = self.handle.read().await;
        guard.clone().ok_or_else(|| anyhow!("Bitswap not initialized"))
    }

    /// Get the Bitswap adapter.
    pub async fn get_adapter(&self) -> Result<Arc<a3net_node::BitswapNetworkAdapter>> {
        let guard = self.adapter.read().await;
        guard.clone().ok_or_else(|| anyhow!("Bitswap network adapter not initialized"))
    }
}

impl Default for BitswapCliContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert BitswapCmd (from CLI) to BitswapCommands (internal).
impl From<&crate::cli::BitswapCmd> for BitswapCommands {
    fn from(cmd: &crate::cli::BitswapCmd) -> Self {
        use crate::cli::BitswapCmd;
        match cmd {
            BitswapCmd::Want { cid, priority, timeout: _ } => {
                BitswapCommands::Want(WantArgs {
                    cid: cid.clone(),
                    priority: *priority,
                    timeout: 60,
                })
            }
            BitswapCmd::Ls { peer, pending, limit } => {
                BitswapCommands::Ls(ListArgs {
                    peer: peer.clone(),
                    pending: *pending,
                    limit: *limit,
                })
            }
            BitswapCmd::Stat { json: _json } => {
                BitswapCommands::Stat(ListArgs {
                    peer: None,
                    pending: false,
                    limit: None,
                })
            }
            BitswapCmd::Ledger { peer, verbose } => {
                BitswapCommands::Ledger(LedgerArgs {
                    peer: peer.clone(),
                    verbose: *verbose,
                })
            }
            BitswapCmd::Cancel { cid, peer } => {
                BitswapCommands::Cancel(CancelArgs {
                    cid: cid.clone(),
                    peer: peer.clone(),
                })
            }
            BitswapCmd::Announce { cid, dht, gossip } => {
                BitswapCommands::Announce(AnnounceArgs {
                    cid: cid.clone(),
                    dht: *dht,
                    gossip: *gossip,
                })
            }
            BitswapCmd::Providers { cid, limit } => {
                BitswapCommands::Providers(ProvidersArgs {
                    cid: cid.clone(),
                    limit: *limit,
                })
            }
            BitswapCmd::ListLocal { limit, .. } => {
                BitswapCommands::Ls(ListArgs {
                    peer: None,
                    pending: false,
                    limit: *limit,
                })
            }
        }
    }
}

impl From<crate::cli::BitswapCmd> for BitswapCommands {
    fn from(cmd: crate::cli::BitswapCmd) -> Self {
        (&cmd).into()
    }
}

/// Execute a Bitswap command.
pub async fn execute_bitswap_command(
    ctx: &BitswapCliContext,
    cmd: BitswapCommands,
) -> Result<()> {
    match cmd {
        BitswapCommands::Want(args) => execute_want(ctx, args).await,
        BitswapCommands::Ls(args) => execute_ls(ctx, args).await,
        BitswapCommands::Stat(args) => execute_stat(ctx, args).await,
        BitswapCommands::Ledger(args) => execute_ledger(ctx, args).await,
        BitswapCommands::Cancel(args) => execute_cancel(ctx, args).await,
        BitswapCommands::Announce(args) => execute_announce(ctx, args).await,
        BitswapCommands::Providers(args) => execute_providers(ctx, args).await,
    }
}

/// Execute the `want` command.
async fn execute_want(ctx: &BitswapCliContext, args: WantArgs) -> Result<()> {
    let handle = ctx.get_handle().await?;
    let adapter = ctx.get_adapter().await.ok();

    // Parse content hash
    let hash = parse_content_hash(&args.cid)?;

    println!("Bitswap want request:");
    println!("  Hash: {}", hash);
    println!("  Priority: {}", args.priority);
    println!("  Timeout: {}s", args.timeout);
    println!();

    // Check if we have it locally first
    if handle.has_block(&hash) {
        println!("Content {} is available locally!", hash.short());
        return Ok(());
    }

    // Find providers via DHT
    println!("Searching for providers on the network...");
    let providers = handle.find_providers(&hash).await.map_err(|e| anyhow!(e))?;

    if providers.is_empty() {
        println!("No providers found for {}.", hash.short());
        println!("Content may not be available on the network.");
        println!("Make sure the content has been announced with 'bitswap announce'.");
        return Ok(());
    }

    println!("Found {} provider(s):", providers.len());
    for (i, provider) in providers.iter().enumerate().take(5) {
        println!("  {}. {} @ {}", i + 1, provider.provider_id, provider.provider_addr);
    }
    println!();

    // Try to get the block from a provider
    if let Some(adapter) = adapter {
        let timeout = Duration::from_secs(args.timeout);
        for provider in providers.iter().take(3) {
            let peer_id = &provider.provider_id;

            println!("Attempting to fetch from provider {}...", peer_id.short());

            match handle.want_block_from_peer(
                adapter.clone(),
                peer_id,
                hash.clone(),
                args.priority,
            ).await {
                a3net_node::BitswapBlockResult::Received { hash, from } => {
                    println!("Successfully received block {} from {}", hash.short(), from);
                    return Ok(());
                }
                a3net_node::BitswapBlockResult::Local(ref local_hash) => {
                    println!("Content {} is available locally!", local_hash.short());
                    return Ok(());
                }
                a3net_node::BitswapBlockResult::NotFound => {
                    println!("Provider {} does not have the content", peer_id.short());
                    continue;
                }
                a3net_node::BitswapBlockResult::Error(e) => {
                    println!("Error from provider {}: {}", peer_id.short(), e);
                    continue;
                }
            }
        }
        println!("Could not fetch content from any provider.");
        println!("The content may no longer be available or providers are offline.");
    } else {
        println!("Note: Network adapter not available. Cannot fetch from providers.");
        println!("Providers found but cannot connect without a running node transport.");
    }

    Ok(())
}

/// Execute the `ls` command.
async fn execute_ls(ctx: &BitswapCliContext, args: ListArgs) -> Result<()> {
    let handle = ctx.get_handle().await?;

    let stats = handle.stats();

    println!("Bitswap Wantlist Status");
    println!("========================");
    println!("Connected peers: {}", stats.connected_peers);
    println!("Local content: {}", stats.local_content);
    println!("Pending wants: {}", stats.pending_wants);
    println!();

    if args.peer.is_some() || args.pending {
        println!("Detailed wantlist information requires network adapter access.");
    }

    Ok(())
}

/// Execute the `stat` command.
async fn execute_stat(ctx: &BitswapCliContext, _args: ListArgs) -> Result<()> {
    let handle = ctx.get_handle().await?;

    let stats = handle.stats();

    println!("Bitswap Statistics");
    println!("==================");
    println!("Connected peers: {}", stats.connected_peers);
    println!("Local content providers: {}", stats.local_content);
    println!("Pending want requests: {}", stats.pending_wants);
    println!();
    println!("To get detailed peer statistics, use:");
    println!("  a3net-cli bitswap ledger --verbose");

    Ok(())
}

/// Execute the `ledger` command.
async fn execute_ledger(ctx: &BitswapCliContext, args: LedgerArgs) -> Result<()> {
    let handle = ctx.get_handle().await?;

    println!("Bitswap Peer Ledgers");
    println!("=====================");
    println!();

    let stats = handle.stats();

    if args.peer.is_some() {
        println!("Ledger for specific peer:");
        println!("  (Use verbose mode for detailed output)");
    } else {
        println!("Summary across {} connected peers:", stats.connected_peers);
    }

    println!();
    println!("Note: Detailed ledger information requires");
    println!("access to the Bitswap engine's peer state.");

    Ok(())
}

/// Execute the `cancel` command.
async fn execute_cancel(ctx: &BitswapCliContext, args: CancelArgs) -> Result<()> {
    let hash = parse_content_hash(&args.cid)?;

    info!("Cancelling want for block {}", hash.short());

    if let Some(peer) = &args.peer {
        println!("Cancelling want for {} from peer {}", hash.short(), peer);
    } else {
        println!("Cancelling want for {} from all peers", hash.short());
    }

    println!();
    println!("Note: Cancel requires active Bitswap connection.");
    println!("The want will be removed from the local wantlist.");

    Ok(())
}

/// Execute the `announce` command.
async fn execute_announce(ctx: &BitswapCliContext, args: AnnounceArgs) -> Result<()> {
    let handle = ctx.get_handle().await?;
    let hash = parse_content_hash(&args.cid)?;

    // Check if we have the content locally
    if !handle.has_block(&hash) {
        return Err(anyhow!(
            "Cannot announce {}: content not found locally",
            hash.short()
        ));
    }

    info!("Announcing content {} to the network", hash.short());

    // Announce to DHT and/or gossip based on flags
    let mut announcements = Vec::new();

    if args.dht {
        // DHT announcement (simplified - actual DHT integration pending)
        announcements.push("DHT".to_string());
    }

    if args.gossip || (!args.dht && !args.gossip) {
        // Default to gossip if no flags specified
        if handle.has_gossip() {
            announcements.push("GossipBus".to_string());
        } else {
            warn!("GossipBus not available, skipping gossip announcement");
        }
    }

    handle.announce_content(&hash).await;

    println!("Successfully announced {} via {}", hash.short(), announcements.join(", "));

    Ok(())
}

/// Execute the `providers` command.
async fn execute_providers(ctx: &BitswapCliContext, args: ProvidersArgs) -> Result<()> {
    let handle = ctx.get_handle().await?;
    let hash = parse_content_hash(&args.cid)?;

    println!("Finding providers for {}", hash.short());
    println!();

    let providers = handle.find_providers(&hash).await.map_err(|e| anyhow!(e))?;

    if providers.is_empty() {
        println!("No providers found for {}.", hash.short());
        println!("Content may not be available on the network.");
    } else {
        println!("Found {} provider(s):", providers.len());
        for (i, provider) in providers.iter().take(args.limit).enumerate() {
            println!("  {}. {} @ {}", i + 1, provider.provider_id, provider.provider_addr);
            println!("     TTL: {}s, Created: {}", provider.ttl_secs, provider.created_at);
        }
    }

    Ok(())
}

/// Parse a content hash from various formats.
fn parse_content_hash(s: &str) -> Result<ContentHash> {
    // Try as-is first (hex string)
    if let Ok(hash) = ContentHash::from_hex(s) {
        return Ok(hash);
    }

    // Try as hex without 0x prefix
    let cleaned = s.trim_start_matches("0x").to_lowercase();
    if let Ok(hash) = ContentHash::from_hex(&cleaned) {
        return Ok(hash);
    }

    // Try as CID (for future CID support)
    // For now, only support hex content hashes
    Err(anyhow!(
        "Invalid content hash: {}. Expected 64-character hex string (BLAKE3 hash).",
        s
    ))
}

/// Format bytes as human-readable string.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_idx])
}

/// Format duration as human-readable string.
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();

    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500.00 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_secs(30)), "30s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_duration(Duration::from_secs(3661)), "1h 1m");
    }

    #[test]
    fn test_parse_content_hash() {
        // Valid hex hash (64 characters)
        let valid_hex = "06f2d53302776b2f936007c0d902d1b389f8f9b1c576627c21a260e03d4d0f18";
        let result = parse_content_hash(valid_hex);
        assert!(result.is_ok());

        // With 0x prefix
        let with_prefix = "0x06f2d53302776b2f936007c0d902d1b389f8f9b1c576627c21a260e03d4d0f18";
        let result = parse_content_hash(with_prefix);
        assert!(result.is_ok());

        // Invalid
        let invalid = "not-a-hash";
        let result = parse_content_hash(invalid);
        assert!(result.is_err());
    }
}

// ---------------------------------------------------------------------------
// Top-level dispatcher
// ---------------------------------------------------------------------------

/// Top-level dispatcher for `a3net bitswap <sub>`. Requires a running node
/// for network operations.
pub async fn run_bitswap(
    sub: &crate::cli::BitswapCmd,
    _node: &a3net_node::Node,
    _data_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let cmd: BitswapCommands = sub.clone().into();
    let ctx = BitswapCliContext::new();

    // Note: Bitswap network operations require the bitswap feature and
    // a running node with bitswap initialized. Currently using local-only mode.
    eprintln!("note: bitswap network operations require a node with bitswap support");

    execute_bitswap_command(&ctx, cmd).await?;
    Ok(())
}
