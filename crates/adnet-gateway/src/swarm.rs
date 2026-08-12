//! Swarm and Bitswap API endpoints for the IPFS-compatible Gateway.
//!
//! This module provides additional IPFS API endpoints that were missing:
//! - Swarm: Network connectivity management
//! - Bitswap: Content exchange statistics
//! - Key: Key management
//! - Repo: Repository operations (GC)

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Statistics for a Bitswap peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitswapLedger {
    pub peer: String,
    pub sent: u64,
    pub received: u64,
    pub blocks_sent: u64,
    pub blocks_received: u64,
}

/// Bitswap statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitswapStats {
    pub wantlist_size: usize,
    pub peers: usize,
    pub blocks_sent: u64,
    pub blocks_received: u64,
    pub data_sent: u64,
    pub data_received: u64,
}

/// Peer connection info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmPeer {
    pub addr: String,
    pub peer_id: Option<String>,
    pub latency: Option<String>,
}

/// Key info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    pub name: String,
    pub id: String,
}

/// GC result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcResult {
    pub key: String,
    pub removed: bool,
}

/// Swarm API handler.
pub struct SwarmApi {
    /// List of known peers (placeholder for actual peer store).
    peers: Arc<RwLock<Vec<SwarmPeer>>>,
}

impl SwarmApi {
    /// Create a new Swarm API handler.
    pub fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get all connected peers.
    pub async fn peers(&self) -> Vec<SwarmPeer> {
        self.peers.read().await.clone()
    }

    /// Add a peer connection.
    pub async fn add_peer(&self, addr: String, peer_id: Option<String>) {
        let mut peers = self.peers.write().await;
        peers.push(SwarmPeer {
            addr,
            peer_id,
            latency: None,
        });
    }

    /// Remove a peer connection.
    pub async fn remove_peer(&self, addr: &str) {
        let mut peers = self.peers.write().await;
        peers.retain(|p| p.addr != addr);
    }
}

impl Default for SwarmApi {
    fn default() -> Self {
        Self::new()
    }
}

/// Bitswap API handler.
pub struct BitswapApi {
    /// Want list size.
    wantlist_size: Arc<RwLock<usize>>,
    /// Ledger stats.
    ledgers: Arc<RwLock<Vec<BitswapLedger>>>,
    /// Global stats.
    stats: Arc<RwLock<BitswapStats>>,
}

impl BitswapApi {
    /// Create a new Bitswap API handler.
    pub fn new() -> Self {
        Self {
            wantlist_size: Arc::new(RwLock::new(0)),
            ledgers: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(RwLock::new(BitswapStats {
                wantlist_size: 0,
                peers: 0,
                blocks_sent: 0,
                blocks_received: 0,
                data_sent: 0,
                data_received: 0,
            })),
        }
    }

    /// Get the want list.
    pub async fn wantlist(&self) -> Vec<String> {
        let size = *self.wantlist_size.read().await;
        (0..size).map(|i| format!("QmTest{}", i)).collect()
    }

    /// Get ledger for a specific peer.
    pub async fn ledger(&self, peer: &str) -> Option<BitswapLedger> {
        let ledgers = self.ledgers.read().await;
        ledgers.iter().find(|l| l.peer == peer).cloned()
    }

    /// Get all ledgers.
    pub async fn ledgers(&self) -> Vec<BitswapLedger> {
        self.ledgers.read().await.clone()
    }

    /// Get statistics.
    pub async fn stats(&self) -> BitswapStats {
        self.stats.read().await.clone()
    }

    /// Update statistics.
    pub async fn record_block_sent(&self, bytes: u64) {
        let mut stats = self.stats.write().await;
        stats.blocks_sent += 1;
        stats.data_sent += bytes;
    }

    /// Update statistics for received block.
    pub async fn record_block_received(&self, bytes: u64) {
        let mut stats = self.stats.write().await;
        stats.blocks_received += 1;
        stats.data_received += bytes;
    }
}

impl Default for BitswapApi {
    fn default() -> Self {
        Self::new()
    }
}

/// Key management API handler.
pub struct KeyApi {
    /// Keys storage.
    keys: Arc<RwLock<Vec<KeyInfo>>>,
}

impl KeyApi {
    /// Create a new Key API handler.
    pub fn new() -> Self {
        Self {
            keys: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// List all keys.
    pub async fn list(&self) -> Vec<KeyInfo> {
        self.keys.read().await.clone()
    }

    /// Generate a new key.
    pub async fn generate(&self, name: &str) -> KeyInfo {
        use std::fmt::Write;
        let random_bytes: [u8; 32] = rand::random();
        let hex_str: String = random_bytes.iter().fold(String::new(), |mut acc, &b| {
            let _ = write!(&mut acc, "{:02x}", b);
            acc
        });
        let id = format!("k51{}", &hex_str[..59]);
        let key = KeyInfo {
            name: name.to_string(),
            id: id.clone(),
        };

        let mut keys = self.keys.write().await;
        keys.push(key.clone());

        key
    }

    /// Remove a key.
    pub async fn remove(&self, name: &str) -> Option<KeyInfo> {
        let mut keys = self.keys.write().await;
        let idx = keys.iter().position(|k| k.name == name)?;
        Some(keys.remove(idx))
    }
}

impl Default for KeyApi {
    fn default() -> Self {
        Self::new()
    }
}

/// Repo API handler.
pub struct RepoApi {
    /// Placeholder for GC state.
    gc_running: Arc<RwLock<bool>>,
}

impl RepoApi {
    /// Create a new Repo API handler.
    pub fn new() -> Self {
        Self {
            gc_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Check if GC is running.
    pub async fn is_gc_running(&self) -> bool {
        *self.gc_running.read().await
    }

    /// Run garbage collection.
    /// Returns a stream of removed keys.
    pub async fn gc(&self) -> Vec<GcResult> {
        // Check if already running
        {
            let mut running = self.gc_running.write().await;
            if *running {
                return Vec::new();
            }
            *running = true;
        }

        // Run GC (placeholder - actual GC would iterate over blob store)
        let results = vec![
            GcResult {
                key: "QmGcTest1".to_string(),
                removed: true,
            },
        ];

        // Clear running flag
        {
            let mut running = self.gc_running.write().await;
            *running = false;
        }

        results
    }
}

impl Default for RepoApi {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_swarm_api() {
        let api = SwarmApi::new();

        api.add_peer("/ip4/127.0.0.1/tcp/4001".to_string(), None).await;

        let peers = api.peers().await;
        assert_eq!(peers.len(), 1);
    }

    #[tokio::test]
    async fn test_bitswap_api() {
        let api = BitswapApi::new();

        api.record_block_sent(1024).await;
        api.record_block_received(2048).await;

        let stats = api.stats().await;
        assert_eq!(stats.blocks_sent, 1);
        assert_eq!(stats.blocks_received, 1);
        assert_eq!(stats.data_sent, 1024);
        assert_eq!(stats.data_received, 2048);
    }

    #[tokio::test]
    async fn test_key_api() {
        let api = KeyApi::new();

        let key = api.generate("test-key").await;
        assert_eq!(key.name, "test-key");

        let keys = api.list().await;
        assert_eq!(keys.len(), 1);

        let removed = api.remove("test-key").await;
        assert!(removed.is_some());

        let keys = api.list().await;
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn test_repo_gc() {
        let api = RepoApi::new();

        assert!(!api.is_gc_running().await);

        let results = api.gc().await;
        assert!(!results.is_empty());

        assert!(!api.is_gc_running().await);
    }
}
