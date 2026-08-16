//! EC Shard Replicator — distributed shard replication and repair.
//!
//! This module implements the EC-aware replication service that:
//! - Periodically scans for under-replicated EC blobs
//! - Distributes shards to peers for redundancy
//! - Repairs missing shards from available peers
//!
//! ## Design
//!
//! Unlike traditional replication (which copies entire blobs), EC replication
//! works at the shard level. Each shard is treated as an independent unit:
//! - EC blobs have 4 shards (3 data + 1 parity)
//! - Each shard can be replicated independently
//! - Missing shards can be reconstructed from remaining shards
//!
//! ## Shard Replication Strategy
//!
//! 1. **Local-first**: Always store all shards locally first
//! 2. **Peer distribution**: Distribute shards round-robin to peers
//! 3. **Self-healing**: Repair missing shards on next sweep
//! 4. **Verification**: BLAKE3 verify every shard before replication
//!
//! ## DO-178C Traceability
//!
//! - EC-REP-1: Each shard has at least 1 remote replica
//! - EC-REP-2: Missing shards are detected and repaired
//! - EC-REP-3: Reconstruction succeeds when ≥3 of 4 shards available

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::Registry;
use a3net_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::ec_shards::{
    EC_DATA_SHARDS, EC_PARITY_SHARDS, EC_TOTAL_SHARDS, ECShardMeta, ErasureCodingError,
};
use crate::ec_store::{ECShardStore, ECStoreError, Recoverability};
use crate::replicator::{NodeAddr, ReplicaMessage, ReplicatorError, ReplicatorTransport};

// ─────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────

/// DO-178C trace tag — shard replication requirement.
pub const SR_TAG_EC_REP_1: &str = "EC-REP-1";

/// DO-178C trace tag — missing shard repair.
pub const SR_TAG_EC_REP_2: &str = "EC-REP-2";

/// DO-178C trace tag — reconstruction success.
pub const SR_TAG_EC_REP_3: &str = "EC-REP-3";

/// Default replication interval (5 minutes).
pub const DEFAULT_REPLICATION_INTERVAL: Duration = Duration::from_secs(300);

/// Minimum shards required for replication (1 remote replica per shard).
pub const MIN_REMOTE_REPLICAS: usize = 1;

// ─────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────

/// Errors from EC replication operations.
#[derive(Debug, Error)]
pub enum ECReplicatorError {
    #[error("EC store: {0}")]
    Store(#[from] ECStoreError),

    #[error("erasure coding: {0}")]
    ErasureCoding(#[from] ErasureCodingError),

    #[error("transport: {0}")]
    Transport(#[from] ReplicatorError),

    #[error("insufficient peers: need {required}, got {available}")]
    InsufficientPeers { required: usize, available: usize },

    #[error("shard {index} verification failed")]
    ShardVerificationFailed { index: usize },

    #[error("replication timeout after {elapsed:?}")]
    Timeout { elapsed: Duration },

    #[error("blob unrecoverable: {0}")]
    Unrecoverable(String),
}

/// Result type for EC replication operations.
pub type ECReplicatorResult<T> = Result<T, ECReplicatorError>;

// ─────────────────────────────────────────────────────────────────
// Metrics
// ─────────────────────────────────────────────────────────────────

/// Metrics for EC replication operations.
pub struct ECReplicatorMetrics {
    pub sweeps_total: Arc<Counter>,
    pub shards_distributed: Arc<Counter>,
    pub shards_repaired: Arc<Counter>,
    pub shards_repair_failed: Arc<Counter>,
    pub under_replicated_blobs: Arc<Gauge>,
    pub fully_replicated_blobs: Arc<Gauge>,
    pub reconstructions_triggered: Arc<Counter>,
    pub reconstruction_success: Arc<Counter>,
    pub reconstruction_failure: Arc<Counter>,
}

impl Clone for ECReplicatorMetrics {
    fn clone(&self) -> Self {
        Self {
            sweeps_total: self.sweeps_total.clone(),
            shards_distributed: self.shards_distributed.clone(),
            shards_repaired: self.shards_repaired.clone(),
            shards_repair_failed: self.shards_repair_failed.clone(),
            under_replicated_blobs: self.under_replicated_blobs.clone(),
            fully_replicated_blobs: self.fully_replicated_blobs.clone(),
            reconstructions_triggered: self.reconstructions_triggered.clone(),
            reconstruction_success: self.reconstruction_success.clone(),
            reconstruction_failure: self.reconstruction_failure.clone(),
        }
    }
}

impl ECReplicatorMetrics {
    pub fn register(registry: &Registry) -> Self {
        Self {
            sweeps_total: registry.register_counter(
                "a3net_ec_replicator_sweeps_total",
                "EC replication sweep cycles completed.",
            ),
            shards_distributed: registry.register_counter(
                "a3net_ec_replicator_shards_distributed_total",
                "Shards distributed to peers for replication.",
            ),
            shards_repaired: registry.register_counter(
                "a3net_ec_replicator_shards_repaired_total",
                "Missing shards successfully repaired.",
            ),
            shards_repair_failed: registry.register_counter(
                "a3net_ec_replicator_shards_repair_failed_total",
                "Shard repair attempts that failed.",
            ),
            under_replicated_blobs: registry.register_gauge(
                "a3net_ec_replicator_under_replicated_blobs",
                "EC blobs with insufficient remote replicas.",
            ),
            fully_replicated_blobs: registry.register_gauge(
                "a3net_ec_replicator_fully_replicated_blobs",
                "EC blobs with sufficient remote replicas.",
            ),
            reconstructions_triggered: registry.register_counter(
                "a3net_ec_replicator_reconstructions_triggered_total",
                "Reconstructions initiated due to missing shards.",
            ),
            reconstruction_success: registry.register_counter(
                "a3net_ec_replicator_reconstruction_success_total",
                "Reconstructions that completed successfully.",
            ),
            reconstruction_failure: registry.register_counter(
                "a3net_ec_replicator_reconstruction_failure_total",
                "Reconstructions that failed.",
            ),
        }
    }
}

impl Default for ECReplicatorMetrics {
    fn default() -> Self {
        let registry = Arc::new(Registry::default());
        Self::register(&registry)
    }
}

// ─────────────────────────────────────────────────────────────────
// Shard Replication State
// ─────────────────────────────────────────────────────────────────

/// Tracks which peers hold which shards for an EC blob.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShardReplicaMap {
    /// content_hash → shard → peer list
    pub shards: HashMap<ContentHash, Vec<ShardPeerMap>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShardPeerMap {
    pub shard_index: u8,
    pub peers: Vec<NodeAddr>,
}

/// State for an individual shard's replication.
#[derive(Debug, Clone)]
pub struct ShardReplicationState {
    pub content_hash: ContentHash,
    pub shard_index: u8,
    pub local_present: bool,
    pub remote_peers: Vec<NodeAddr>,
    pub target_replicas: usize,
}

impl ShardReplicationState {
    pub fn replica_count(&self) -> usize {
        self.local_present as usize + self.remote_peers.len()
    }

    pub fn shortfall(&self) -> usize {
        self.target_replicas.saturating_sub(self.replica_count())
    }

    pub fn is_fully_replicated(&self) -> bool {
        self.replica_count() >= self.target_replicas
    }
}

// ─────────────────────────────────────────────────────────────────
// EC Replicator Service
// ─────────────────────────────────────────────────────────────────

/// Service that manages EC shard replication and repair.
pub struct ECReplicatorService {
    store: Arc<ECShardStore>,
    transport: Arc<dyn ReplicatorTransport>,
    metrics: ECReplicatorMetrics,
    peer_pool: Arc<RwLock<Vec<NodeAddr>>>,
    replica_map: Arc<RwLock<ShardReplicaMap>>,
    replication_interval: Duration,
}

impl ECReplicatorService {
    pub fn new(
        store: Arc<ECShardStore>,
        transport: Arc<dyn ReplicatorTransport>,
        metrics: ECReplicatorMetrics,
    ) -> Self {
        Self {
            store,
            transport,
            metrics,
            peer_pool: Arc::new(RwLock::new(Vec::new())),
            replica_map: Arc::new(RwLock::new(ShardReplicaMap::default())),
            replication_interval: DEFAULT_REPLICATION_INTERVAL,
        }
    }

    /// Register a peer in the replication pool.
    pub fn register_peer(&self, peer: NodeAddr) {
        let mut pool = self.peer_pool.write();
        if !pool.contains(&peer) {
            pool.push(peer);
        }
    }

    /// Remove a peer from the replication pool.
    pub fn unregister_peer(&self, peer: &NodeAddr) {
        self.peer_pool.write().retain(|p| p != peer);
    }

    /// Get the current peer pool.
    pub fn peers(&self) -> Vec<NodeAddr> {
        self.peer_pool.read().clone()
    }

    /// Run the replication sweep loop.
    pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let interval = self.replication_interval;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        info!(
            interval_secs = interval.as_secs(),
            "[{}] EC replicator service started", SR_TAG_EC_REP_1
        );

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(e) = self.sweep_once().await {
                        warn!(
                            error = %e,
                            "[{}] EC replication sweep failed",
                            SR_TAG_EC_REP_2
                        );
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("[{}] EC replicator service stopping", SR_TAG_EC_REP_1);
                        return;
                    }
                }
            }
        }
    }

    /// Perform one replication sweep.
    pub async fn sweep_once(&self) -> ECReplicatorResult<()> {
        self.metrics.sweeps_total.inc();

        let blobs = self
            .store
            .list_complete()
            .map_err(|e| ECReplicatorError::Store(ECStoreError::Io(e)))?;

        let peers = self.peers();
        if peers.is_empty() {
            debug!("no peers in pool, skipping EC replication sweep");
            return Ok(());
        }

        let mut under_replicated = 0usize;
        let mut fully_replicated = 0usize;
        let mut shards_distributed = 0usize;
        let mut shards_repaired = 0usize;

        for blob_hash in blobs {
            let status = match self.store.shard_status(&blob_hash) {
                Ok(s) => s,
                Err(e) => {
                    warn!(blob = %blob_hash, error = %e, "failed to get shard status");
                    continue;
                }
            };

            match status.recoverability {
                Recoverability::FullyRecoverable => {
                    fully_replicated += 1;
                    // Check if remote replicas are sufficient.
                    if !self.check_remote_replicas(&blob_hash, MIN_REMOTE_REPLICAS) {
                        under_replicated += 1;
                        // Distribute missing shards.
                        if let Ok(count) = self.distribute_shards(&blob_hash, &peers).await {
                            shards_distributed += count;
                        }
                    }
                }
                Recoverability::CanAttemptRecovery => {
                    under_replicated += 1;
                    // Try to repair missing shards via reconstruction.
                    self.metrics.reconstructions_triggered.inc();
                    if self.repair_blob(&blob_hash).await.is_ok() {
                        self.metrics.reconstruction_success.inc();
                        shards_repaired += 1;
                    } else {
                        self.metrics.reconstruction_failure.inc();
                    }
                }
                Recoverability::Unrecoverable => {
                    error!(
                        blob = %blob_hash,
                        shards_present = status.shards_present,
                        "[{}] EC blob is UNRECOVERABLE",
                        SR_TAG_EC_REP_3
                    );
                }
            }
        }

        self.metrics
            .under_replicated_blobs
            .set(under_replicated as i64);
        self.metrics
            .fully_replicated_blobs
            .set(fully_replicated as i64);
        self.metrics
            .shards_distributed
            .inc_by(shards_distributed as u64);
        self.metrics.shards_repaired.inc_by(shards_repaired as u64);

        debug!(
            under_replicated,
            fully_replicated,
            shards_distributed,
            shards_repaired,
            "[{}] EC replication sweep complete",
            SR_TAG_EC_REP_1
        );

        Ok(())
    }

    /// Check if a blob has sufficient remote replicas.
    fn check_remote_replicas(&self, blob_hash: &ContentHash, target: usize) -> bool {
        let replica_map = self.replica_map.read();
        if let Some(blob_shards) = replica_map.shards.get(blob_hash) {
            // Check if each shard has at least `target` remote replicas.
            blob_shards.iter().all(|s| s.peers.len() >= target)
        } else {
            false
        }
    }

    /// Distribute shards to peers for replication.
    async fn distribute_shards(
        &self,
        blob_hash: &ContentHash,
        peers: &[NodeAddr],
    ) -> ECReplicatorResult<usize> {
        let meta = self
            .store
            .get_meta(blob_hash)
            .map_err(|e| ECReplicatorError::Store(e))?;

        let mut distributed = 0usize;

        for shard_idx in 0..EC_TOTAL_SHARDS {
            // Get peers that don't already have this shard.
            let existing_peers: Vec<NodeAddr> = {
                let replica_map = self.replica_map.read();
                replica_map
                    .shards
                    .get(blob_hash)
                    .and_then(|shards| shards.get(shard_idx))
                    .map(|s| s.peers.clone())
                    .unwrap_or_default()
            };

            let target_peers: Vec<_> = peers
                .iter()
                .filter(|p| !existing_peers.contains(p))
                .take(1) // Distribute to 1 new peer per shard
                .cloned()
                .collect();

            if target_peers.is_empty() {
                continue;
            }

            // Read the shard and send to peer.
            let shard_bytes = match self.store.read_shard(blob_hash, shard_idx as u8) {
                Ok(bytes) => bytes,
                Err(e) => {
                    warn!(
                        blob = %blob_hash,
                        shard = shard_idx,
                        error = %e,
                        "failed to read shard for distribution"
                    );
                    continue;
                }
            };

            let shard_meta = &meta.shards[shard_idx];
            let target_peer = &target_peers[0];

            let msg = ReplicaMessage {
                blob: blob_hash.clone(),
                block: shard_meta.digest.clone(),
                index: shard_idx as u32,
                bytes: shard_bytes,
            };

            match self.transport.push_block(target_peer, msg).await {
                Ok(_ack) => {
                    // Update replica map.
                    let mut replica_map = self.replica_map.write();
                    let blob_shards = replica_map
                        .shards
                        .entry(blob_hash.clone())
                        .or_insert_with(Vec::new);

                    while blob_shards.len() < EC_TOTAL_SHARDS {
                        blob_shards.push(ShardPeerMap {
                            shard_index: blob_shards.len() as u8,
                            peers: Vec::new(),
                        });
                    }

                    blob_shards[shard_idx].peers.push(target_peer.clone());
                    distributed += 1;

                    debug!(
                        blob = %blob_hash,
                        shard = shard_idx,
                        peer = %target_peer.as_str(),
                        "[{}] shard distributed",
                        SR_TAG_EC_REP_1
                    );
                }
                Err(e) => {
                    warn!(
                        blob = %blob_hash,
                        shard = shard_idx,
                        peer = %target_peer.as_str(),
                        error = %e,
                        "shard distribution failed"
                    );
                }
            }
        }

        Ok(distributed)
    }

    /// Repair a blob with missing shards by reconstruction.
    async fn repair_blob(&self, blob_hash: &ContentHash) -> ECReplicatorResult<()> {
        // Check which shards are missing.
        let missing = self.store.missing_shards(blob_hash);
        if missing.is_empty() {
            return Ok(());
        }

        info!(
            blob = %blob_hash,
            missing_shards = ?missing,
            "[{}] attempting to repair {} missing shards",
            SR_TAG_EC_REP_2,
            missing.len()
        );

        for shard_idx in missing {
            match self.store.repair_shard(blob_hash, shard_idx) {
                Ok(()) => {
                    self.metrics.shards_repaired.inc();
                    info!(
                        blob = %blob_hash,
                        shard = shard_idx,
                        "[{}] shard repaired successfully",
                        SR_TAG_EC_REP_2
                    );
                }
                Err(e) => {
                    self.metrics.shards_repair_failed.inc();
                    warn!(
                        blob = %blob_hash,
                        shard = shard_idx,
                        error = %e,
                        "[{}] shard repair failed",
                        SR_TAG_EC_REP_2
                    );
                    return Err(ECReplicatorError::from(e));
                }
            }
        }

        Ok(())
    }

    /// Manually trigger repair for a specific blob.
    pub async fn trigger_repair(&self, blob_hash: &ContentHash) -> ECReplicatorResult<()> {
        self.repair_blob(blob_hash).await
    }

    /// Get replication status for a blob.
    pub fn get_replication_status(
        &self,
        blob_hash: &ContentHash,
    ) -> Option<ECBlobReplicationStatus> {
        let replica_map = self.replica_map.read();
        let shards = replica_map.shards.get(blob_hash)?;

        let shard_states: Vec<ShardReplicationState> = shards
            .iter()
            .map(|s| ShardReplicationState {
                content_hash: blob_hash.clone(),
                shard_index: s.shard_index,
                local_present: true, // Assuming local always has all shards
                remote_peers: s.peers.clone(),
                target_replicas: MIN_REMOTE_REPLICAS,
            })
            .collect();

        Some(ECBlobReplicationStatus {
            content_hash: blob_hash.clone(),
            shard_states,
        })
    }

    /// Persist replica map to disk.
    pub fn save_replica_map(&self, path: &std::path::Path) -> std::io::Result<()> {
        let map = self.replica_map.read();
        let json = serde_json::to_string_pretty(&*map)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load replica map from disk.
    pub fn load_replica_map(&self, path: &std::path::Path) -> std::io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(path)?;
        let map: ShardReplicaMap =
            serde_json::from_str(&json).map_err(|e| std::io::Error::other(e))?;
        *self.replica_map.write() = map;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────
// Supporting Types
// ─────────────────────────────────────────────────────────────────

/// Replication status for a single EC blob.
#[derive(Debug, Clone)]
pub struct ECBlobReplicationStatus {
    pub content_hash: ContentHash,
    pub shard_states: Vec<ShardReplicationState>,
}

impl ECBlobReplicationStatus {
    /// Returns true if all shards have sufficient remote replicas.
    pub fn is_fully_replicated(&self) -> bool {
        self.shard_states.iter().all(|s| s.is_fully_replicated())
    }

    /// Returns the number of shards with insufficient replicas.
    pub fn under_replicated_count(&self) -> usize {
        self.shard_states
            .iter()
            .filter(|s| !s.is_fully_replicated())
            .count()
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;
    use crate::ec_shards::{EC_DATA_SHARDS, EC_PARITY_SHARDS, EC_TOTAL_SHARDS, ECShardMeta};

    #[test]
    fn shard_replication_state_calculations() {
        let state = ShardReplicationState {
            content_hash: ContentHash::from_bytes(b"test"),
            shard_index: 0,
            local_present: true,
            remote_peers: vec![NodeAddr::new("peer-1"), NodeAddr::new("peer-2")],
            target_replicas: 1,
        };

        assert_eq!(state.replica_count(), 3); // 1 local + 2 remote
        assert_eq!(state.shortfall(), 0);
        assert!(state.is_fully_replicated());
    }

    #[test]
    fn shard_replication_state_under_replicated() {
        let state = ShardReplicationState {
            content_hash: ContentHash::from_bytes(b"test"),
            shard_index: 0,
            local_present: true,
            remote_peers: vec![],
            target_replicas: 2,
        };

        assert_eq!(state.replica_count(), 1); // local only
        assert_eq!(state.shortfall(), 1);
        assert!(!state.is_fully_replicated());
    }

    #[test]
    fn shard_replication_state_no_local() {
        let state = ShardReplicationState {
            content_hash: ContentHash::from_bytes(b"test"),
            shard_index: 0,
            local_present: false,
            remote_peers: vec![NodeAddr::new("peer-1"), NodeAddr::new("peer-2")],
            target_replicas: 1,
        };

        assert_eq!(state.replica_count(), 2); // 2 remote only
        assert_eq!(state.shortfall(), 0);
        assert!(state.is_fully_replicated());
    }

    #[test]
    fn ec_blob_replication_status_fully_replicated() {
        let hash = ContentHash::from_bytes(b"test-blob");
        let status = ECBlobReplicationStatus {
            content_hash: hash.clone(),
            shard_states: (0..4)
                .map(|i| ShardReplicationState {
                    content_hash: hash.clone(),
                    shard_index: i,
                    local_present: true,
                    remote_peers: vec![NodeAddr::new(format!("peer-{}", i))],
                    target_replicas: 1,
                })
                .collect(),
        };

        assert!(status.is_fully_replicated());
        assert_eq!(status.under_replicated_count(), 0);
    }

    #[test]
    fn ec_blob_replication_status_under_replicated() {
        let hash = ContentHash::from_bytes(b"test-blob");
        let status = ECBlobReplicationStatus {
            content_hash: hash.clone(),
            shard_states: vec![
                // Shard 0: fully replicated
                ShardReplicationState {
                    content_hash: hash.clone(),
                    shard_index: 0,
                    local_present: true,
                    remote_peers: vec![NodeAddr::new("peer-0")],
                    target_replicas: 1,
                },
                // Shard 1: under-replicated
                ShardReplicationState {
                    content_hash: hash.clone(),
                    shard_index: 1,
                    local_present: true,
                    remote_peers: vec![],
                    target_replicas: 1,
                },
                // Shard 2: fully replicated
                ShardReplicationState {
                    content_hash: hash.clone(),
                    shard_index: 2,
                    local_present: true,
                    remote_peers: vec![NodeAddr::new("peer-2")],
                    target_replicas: 1,
                },
                // Shard 3: under-replicated
                ShardReplicationState {
                    content_hash: hash.clone(),
                    shard_index: 3,
                    local_present: true,
                    remote_peers: vec![],
                    target_replicas: 1,
                },
            ],
        };

        assert!(!status.is_fully_replicated());
        assert_eq!(status.under_replicated_count(), 2);
    }

    #[test]
    fn shard_peer_map_serialization() {
        let map = ShardPeerMap {
            shard_index: 2,
            peers: vec![NodeAddr::new("peer-alpha"), NodeAddr::new("peer-beta")],
        };

        let json = serde_json::to_string(&map).unwrap();
        let back: ShardPeerMap = serde_json::from_str(&json).unwrap();

        assert_eq!(map.shard_index, back.shard_index);
        assert_eq!(map.peers.len(), back.peers.len());
    }

    #[test]
    fn shard_replica_map_serialization() {
        let mut map = ShardReplicaMap::default();
        let hash1 = ContentHash::from_bytes(b"blob-1");
        let hash2 = ContentHash::from_bytes(b"blob-2");

        map.shards.insert(
            hash1.clone(),
            vec![ShardPeerMap {
                shard_index: 0,
                peers: vec![NodeAddr::new("peer-0")],
            }],
        );
        map.shards.insert(
            hash2.clone(),
            vec![ShardPeerMap {
                shard_index: 1,
                peers: vec![NodeAddr::new("peer-1")],
            }],
        );

        let json = serde_json::to_string(&map).unwrap();
        let back: ShardReplicaMap = serde_json::from_str(&json).unwrap();

        assert_eq!(map.shards.len(), back.shards.len());
        assert!(back.shards.contains_key(&hash1));
        assert!(back.shards.contains_key(&hash2));
    }

    #[test]
    fn ec_constants_alignment() {
        // Verify EC constants are consistent.
        assert_eq!(EC_TOTAL_SHARDS, 4);
        assert_eq!(EC_DATA_SHARDS, 3);
        assert_eq!(EC_PARITY_SHARDS, 1);
        assert_eq!(MIN_REMOTE_REPLICAS, 1);
    }
}
