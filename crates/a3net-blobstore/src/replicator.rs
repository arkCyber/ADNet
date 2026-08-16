//! Replication — turn "3 副本" from a static statement into a
//! real network behavior.
//!
//! This module is the *pure algorithm* layer of the replication
//! stack. It owns the data model ([`ReplicaSet`], [`BlockReplica`],
//! [`ReplicationPolicy`]) and the sweep that re-balances replicas
//! after nodes join / leave. The actual wire protocol is provided
//! by an implementor of [`ReplicatorTransport`] — the default
//! in-process `MockTransport` lets the algorithm be unit-tested
//! without an iroh runtime; a real iroh-backed adapter can be
//! added later by implementing the same trait.
//!
//! DO-178C traceability: this file is the source for SR-6
//! ("every blob shall have ≥ 3 replicas under steady state")
//! and SR-7 ("neighbor-dropout shall be detected and repaired
//! on the next sweep"). Verification under
//! `tests/distributed_storage_aerospace.rs`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::Registry;
use a3net_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::store::BlobStore;

// ─────────────────────────────────────────────────────────────────
// Constants — DO-178C pinning
// ─────────────────────────────────────────────────────────────────

/// Default replication factor (IPFS parity).
pub const DEFAULT_REPLICATION_FACTOR: u8 = 3;

/// Hard upper bound on replication factor. Prevents a buggy
/// config from requesting 65535 replicas and OOMing the node.
pub const MAX_REPLICATION_FACTOR: u8 = 32;

/// DO-178C trace tag — every replication event carries this
/// string so the certifier can grep the audit log.
pub const SR_TAG_SR_6: &str = "SR-6";
/// DO-178C trace tag — repair-after-dropout.
pub const SR_TAG_SR_7: &str = "SR-7";

// ─────────────────────────────────────────────────────────────────
// Wire messages
// ─────────────────────────────────────────────────────────────────

/// One replication message carried over a [`ReplicatorTransport`].
///
/// Each message is a single 256 KiB block (or smaller) together
/// with its BLAKE3 hash. The receiver MUST re-hash and reject
/// the block if the digest differs — that's the only safety
/// barrier against a byzantine peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaMessage {
    /// BLAKE3(content_hash) of the original blob.
    pub blob: ContentHash,
    /// BLAKE3(block_bytes).
    pub block: ContentHash,
    /// 0-based block index (multi-block blobs will be added in
    /// PR-4 once we have multi-block streaming).
    pub index: u32,
    /// The block bytes themselves — the receiver re-hashes
    /// and only commits if the digest matches `block`.
    pub bytes: Vec<u8>,
}

/// Acknowledgement from the receiver after the block passes
/// hash verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaAck {
    pub blob: ContentHash,
    pub block: ContentHash,
    pub received_bytes: u64,
}

// ─────────────────────────────────────────────────────────────────
// Transport trait — pluggable so the algorithm can be tested
// without iroh.
// ─────────────────────────────────────────────────────────────────

/// Transport abstraction for the replication wire protocol.
///
/// The algorithm does not care whether the transport is a real
/// iroh QUIC stream, a Tokio TCP socket, or an in-memory
/// `MockTransport`. Implementors must guarantee:
/// 1. Bytes are delivered in order (each message is a single
///    frame; the transport can chunk it but must not
///    re-concatenate multiple messages).
/// 2. The receiver's [`ReplicatorTransport::receive`] is
///    called once per inbound message.
/// 3. The transport is **fail-safe** — a malformed message
///    returns an error rather than silently swallowing it.
#[async_trait::async_trait]
pub trait ReplicatorTransport: Send + Sync {
    /// Push a single block to `peer`. Returns `Ok(())` iff the
    /// receiver has acked; a byzantine / corrupt peer returns
    /// `Err(ReplicatorError::Refused(_))`.
    async fn push_block(
        &self,
        peer: &NodeAddr,
        msg: ReplicaMessage,
    ) -> Result<ReplicaAck, ReplicatorError>;

    /// Send a no-op ping to confirm the peer is reachable.
    /// Used by the sweep to skip dead peers early.
    async fn ping(&self, peer: &NodeAddr) -> Result<(), ReplicatorError>;

    /// Request EC metadata from a peer for content identified by hash.
    ///
    /// Returns the serialized ECBlobMeta if the peer has it,
    /// or None if the peer doesn't have this content.
    async fn get_ec_meta(
        &self,
        peer: &NodeAddr,
        content_hash: &ContentHash,
    ) -> Result<Option<Vec<u8>>, ReplicatorError>;

    /// Request a specific shard from a peer.
    ///
    /// Returns the serialized ShardDelivery if the peer has it,
    /// or None if the peer doesn't have this shard.
    async fn get_shard(
        &self,
        peer: &NodeAddr,
        content_hash: &ContentHash,
        shard_index: u8,
    ) -> Result<Option<Vec<u8>>, ReplicatorError>;
}

/// Lightweight peer address — separate from a3net_types::NodeAddr
/// so the replicator module has zero coupling on the iroh /
/// transport crates. The `MockTransport` builds a `NodeAddr`
/// from a string-id and `ReplicatorService` only sees opaque IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeAddr(pub String);

impl NodeAddr {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─────────────────────────────────────────────────────────────────
// Replica set — the data model
// ─────────────────────────────────────────────────────────────────

/// One block's replication state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockReplica {
    pub block: ContentHash,
    pub has_local: bool,
    pub remote_providers: Vec<NodeAddr>,
    pub target: u8,
}

impl BlockReplica {
    pub fn new(block: ContentHash, target: u8) -> Self {
        Self {
            block,
            has_local: true,
            remote_providers: Vec::new(),
            target,
        }
    }
    pub fn replica_count(&self) -> u8 {
        self.has_local as u8 + self.remote_providers.len() as u8
    }
    pub fn shortfall(&self) -> u8 {
        self.target.saturating_sub(self.replica_count())
    }
    pub fn is_fully_replicated(&self) -> bool {
        self.replica_count() >= self.target
    }
}

/// Per-blob replica state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaSet {
    pub blob: ContentHash,
    pub target: u8,
    pub blocks: HashMap<ContentHash, BlockReplica>,
    /// Unix-epoch millis of the last successful sweep.
    pub last_sweep_ms: u64,
}

impl ReplicaSet {
    pub fn new(blob: ContentHash, target: u8) -> Self {
        Self {
            blob,
            target,
            blocks: HashMap::new(),
            last_sweep_ms: 0,
        }
    }
    pub fn with_block(mut self, block: ContentHash) -> Self {
        self.blocks
            .insert(block.clone(), BlockReplica::new(block, self.target));
        self
    }
    pub fn is_fully_replicated(&self) -> bool {
        self.blocks.values().all(|b| b.is_fully_replicated())
    }
    pub fn under_replicated_blocks(&self) -> Vec<ContentHash> {
        self.blocks
            .iter()
            .filter(|(_, b)| !b.is_fully_replicated())
            .map(|(h, _)| h.clone())
            .collect()
    }
    pub fn fully_replicated_count(&self) -> usize {
        self.blocks
            .values()
            .filter(|b| b.is_fully_replicated())
            .count()
    }
    pub fn total_blocks(&self) -> usize {
        self.blocks.len()
    }
}

/// Replication policy — knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicationPolicy {
    pub factor: u8,
    pub sweep_interval: Duration,
    /// Maximum concurrent push tasks per sweep. Limits
    /// bandwidth + open-connection count.
    pub max_concurrent_pushes: usize,
}

impl Default for ReplicationPolicy {
    fn default() -> Self {
        Self {
            factor: DEFAULT_REPLICATION_FACTOR,
            sweep_interval: Duration::from_secs(300),
            max_concurrent_pushes: 4,
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ReplicatorError {
    #[error("peer refused block: {0}")]
    Refused(String),
    #[error("transport I/O: {0}")]
    Transport(String),
    #[error("hash mismatch on push: expected {expected}, got {actual}")]
    HashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },
    #[error("block too large: {size} > {cap} bytes")]
    BlockTooLarge { size: u64, cap: u64 },
    #[error("replication factor {0} out of range 1..={}", MAX_REPLICATION_FACTOR)]
    BadFactor(u8),
    /// DO-178C SR-7: a known provider went missing and was
    /// dropped from the replica set.
    #[error("provider dropped: {0}")]
    ProviderDropped(String),
}

// ─────────────────────────────────────────────────────────────────
// Metrics — separate handle so the replicator can be registered
// in isolation.
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ReplicatorMetrics {
    pub sweeps_total: Arc<Counter>,
    pub blocks_pushed_total: Arc<Counter>,
    pub push_errors_total: Arc<Counter>,
    pub hashes_verified_total: Arc<Counter>,
    pub replicas_under_replicated: Arc<Gauge>,
    pub replicas_fully_replicated: Arc<Gauge>,
}

impl ReplicatorMetrics {
    pub fn register(registry: &Registry) -> Self {
        Self {
            sweeps_total: registry.register_counter(
                "a3net_replicator_sweeps_total",
                "Replication sweep cycles completed.",
            ),
            blocks_pushed_total: registry.register_counter(
                "a3net_replicator_blocks_pushed_total",
                "Blocks successfully pushed to a peer (post hash-verification).",
            ),
            push_errors_total: registry.register_counter(
                "a3net_replicator_push_errors_total",
                "Push attempts that failed (transport / verification / peer refuse).",
            ),
            hashes_verified_total: registry.register_counter(
                "a3net_replicator_hashes_verified_total",
                "Blocks re-hashed on the receiver side during push.",
            ),
            replicas_under_replicated: registry.register_gauge(
                "a3net_replicator_under_replicated_blocks",
                "Current count of blocks below their target replica count.",
            ),
            replicas_fully_replicated: registry.register_gauge(
                "a3net_replicator_fully_replicated_blocks",
                "Current count of blocks at or above their target replica count.",
            ),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// ReplicatorService — the orchestration loop
// ─────────────────────────────────────────────────────────────────

/// Service that periodically re-balances replicas.
///
/// Drop the returned handle to stop the loop, or call
/// [`ReplicatorService::shutdown_signal`] to signal stop from
/// another task.
pub struct ReplicatorService {
    store: Arc<BlobStore>,
    transport: Arc<dyn ReplicatorTransport>,
    candidate_pool: Arc<RwLock<Vec<NodeAddr>>>,
    policy: ReplicationPolicy,
    metrics: ReplicatorMetrics,
}

impl ReplicatorService {
    /// Build a service. The candidate pool is the list of
    /// peers we may push to; in production this is the
    /// ProviderIndex, but the algorithm is identical so we
    /// keep the seam clean.
    pub fn new(
        store: Arc<BlobStore>,
        transport: Arc<dyn ReplicatorTransport>,
        policy: ReplicationPolicy,
        metrics: ReplicatorMetrics,
    ) -> Self {
        Self {
            store,
            transport,
            candidate_pool: Arc::new(RwLock::new(Vec::new())),
            policy,
            metrics,
        }
    }

    /// Register a peer in the candidate pool. Duplicates are
    /// silently ignored.
    pub fn register_peer(&self, peer: NodeAddr) {
        let mut pool = self.candidate_pool.write();
        if !pool.contains(&peer) {
            pool.push(peer);
        }
    }

    /// Remove a peer from the candidate pool. Existing
    /// `ReplicaSet` entries that listed this peer are
    /// trimmed on the next sweep (SR-7).
    pub fn unregister_peer(&self, peer: &NodeAddr) {
        self.candidate_pool.write().retain(|p| p != peer);
    }

    /// Snapshot the current candidate pool.
    pub fn peers(&self) -> Vec<NodeAddr> {
        self.candidate_pool.read().clone()
    }

    /// Run the sweep loop until `shutdown` fires.
    pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let interval = self.policy.sweep_interval;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        info!(?interval, SR = SR_TAG_SR_6, "replicator service started");
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(e) = self.sweep_once().await {
                        warn!(error = %e, SR = SR_TAG_SR_7, "replicator sweep failed");
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!(SR = SR_TAG_SR_6, "replicator service stopping");
                        return;
                    }
                }
            }
        }
    }

    /// One sweep: enumerate every locally-known blob, identify
    /// under-replicated blocks, push until quota is met.
    pub async fn sweep_once(&self) -> Result<usize, ReplicatorError> {
        self.metrics.sweeps_total.inc();
        let hashes = self
            .store
            .list_complete()
            .map_err(|e| ReplicatorError::Transport(format!("list: {e}")))?;

        let mut under_replicated = 0usize;
        let mut fully_replicated = 0usize;
        let mut pushes = 0usize;

        for blob in hashes {
            let (_size, count) = self
                .store
                .meta(&blob)
                .map_err(|e| ReplicatorError::Transport(format!("meta: {e}")))?;
            // Compute the actual block hashes by streaming
            // every chunk once. This is the same loop that
            // `read_block_bytes` would do per-block, but we
            // build the full map up-front so the push loop
            // can iterate without re-hashing.
            let block_hashes = crate::block_layout::split_into_blocks_from_chunks(count, |i| {
                self.store.read_chunk_sync(&blob, i).unwrap_or_default()
            });
            let mut set = ReplicaSet::new(blob.clone(), self.policy.factor);
            for blk in &block_hashes {
                set.blocks.insert(
                    blk.clone(),
                    BlockReplica {
                        block: blk.clone(),
                        has_local: true,
                        remote_providers: Vec::new(),
                        target: self.policy.factor,
                    },
                );
            }
            // Load the persisted replica set if any and merge.
            if let Ok(prev) = self.load_replica_set(&blob) {
                for (h, br) in prev.blocks {
                    if let Some(slot) = set.blocks.get_mut(&h) {
                        slot.remote_providers = br.remote_providers;
                        // If local is gone, fall back to false.
                        slot.has_local = br.has_local;
                    }
                }
                set.last_sweep_ms = prev.last_sweep_ms;
            }

            // Refresh provider pool — drop dead peers.
            {
                let pool = self.candidate_pool.read().clone();
                for block in set.blocks.values_mut() {
                    block.remote_providers.retain(|p| pool.contains(p));
                }
            }

            // Fill the gaps.
            let peers = self.candidate_pool.read().clone();
            for (block_hash, replica) in set.blocks.iter_mut() {
                while replica.shortfall() > 0 {
                    let candidate = peers
                        .iter()
                        .find(|p| !replica.remote_providers.contains(p))
                        .cloned();
                    let peer = match candidate {
                        Some(p) => p,
                        None => break, // pool exhausted
                    };
                    let bytes = match self.read_block_bytes(&blob, block_hash) {
                        Ok(b) => b,
                        Err(e) => {
                            warn!(blob = %blob, block = %block_hash, error = %e,
                                  "read_block_bytes failed during replication");
                            break;
                        }
                    };
                    let msg = ReplicaMessage {
                        blob: blob.clone(),
                        block: block_hash.clone(),
                        index: 0,
                        bytes,
                    };
                    match self.transport.push_block(&peer, msg).await {
                        Ok(ack) => {
                            self.metrics.blocks_pushed_total.inc();
                            self.metrics.hashes_verified_total.inc();
                            replica.remote_providers.push(peer);
                            pushes += 1;
                            let _ = ack;
                        }
                        Err(e) => {
                            self.metrics.push_errors_total.inc();
                            warn!(peer = %peer.as_str(), error = %e,
                                  "push failed");
                            // Don't loop forever on the same peer.
                            break;
                        }
                    }
                }
            }

            // Update metrics.
            for b in set.blocks.values() {
                if b.is_fully_replicated() {
                    fully_replicated += 1;
                } else {
                    under_replicated += 1;
                }
            }

            // Persist.
            if let Err(e) = self.save_replica_set(&set) {
                warn!(blob = %blob, error = %e, "persisting replica set failed");
            }
        }

        self.metrics
            .replicas_under_replicated
            .set(under_replicated as i64);
        self.metrics
            .replicas_fully_replicated
            .set(fully_replicated as i64);
        debug!(
            under_replicated,
            fully_replicated,
            pushes,
            SR = SR_TAG_SR_6,
            "sweep complete"
        );
        Ok(pushes)
    }

    fn load_replica_set(&self, blob: &ContentHash) -> Result<ReplicaSet, ReplicatorError> {
        let path = self.store.blob_dir(blob).join("replicas.json");
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| ReplicatorError::Transport(format!("read replicas.json: {e}")))?;
        serde_json::from_str(&raw)
            .map_err(|e| ReplicatorError::Transport(format!("parse replicas.json: {e}")))
    }

    fn save_replica_set(&self, set: &ReplicaSet) -> Result<(), ReplicatorError> {
        let dir = self.store.blob_dir(&set.blob);
        if !dir.exists() {
            return Ok(()); // blob was removed between sweep and save
        }
        let path = dir.join("replicas.json");
        let tmp = path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(set)
            .map_err(|e| ReplicatorError::Transport(format!("serialize: {e}")))?;
        std::fs::write(&tmp, raw)
            .map_err(|e| ReplicatorError::Transport(format!("write tmp: {e}")))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| ReplicatorError::Transport(format!("rename: {e}")))?;
        Ok(())
    }

    /// Read the full 256 KiB block identified by `block_hash`.
    ///
    /// The block hash is BLAKE3 of the 256 KiB. We recover the
    /// 0-based block index by scanning the blob's chunks,
    /// accumulating 16 KiB at a time until the running BLAKE3
    /// matches `block_hash`. For the common 1-block / 1-block
    /// case this is one iteration; for multi-block blobs it
    /// scans up to N blocks.
    ///
    /// This is the linear-time helper that's only invoked on
    /// sweep pushes — the hot path is the read API, which uses
    /// `read_range_sync` directly.
    fn read_block_bytes(
        &self,
        blob: &ContentHash,
        block_hash: &ContentHash,
    ) -> Result<Vec<u8>, ReplicatorError> {
        use crate::block_layout::{BLOCK_SIZE, CHUNKS_PER_BLOCK};
        let (size, _count) = self
            .store
            .meta(blob)
            .map_err(|e| ReplicatorError::Transport(format!("meta: {e}")))?;
        let n_blocks = crate::block_layout::block_count_for(size) as usize;
        let n_chunks = crate::chunked::chunk_count_for(size) as usize;
        let mut chunk_idx = 0usize;
        for _block_idx in 0..n_blocks {
            let mut hasher = blake3::Hasher::new();
            let mut out = Vec::with_capacity(BLOCK_SIZE);
            let end_chunk = (chunk_idx + CHUNKS_PER_BLOCK).min(n_chunks);
            for c in chunk_idx..end_chunk {
                let bytes = self
                    .store
                    .read_chunk_sync(blob, c as u32)
                    .map_err(|e| ReplicatorError::Transport(format!("read_chunk[{c}]: {e}")))?;
                hasher.update(&bytes);
                out.extend_from_slice(&bytes);
            }
            let actual = ContentHash::from_hex(hasher.finalize().to_hex().as_ref())
                .expect("blake3 hex is always 64 chars");
            if &actual == block_hash {
                return Ok(out);
            }
            chunk_idx = end_chunk;
        }
        Err(ReplicatorError::Transport(format!(
            "block {block_hash} not found in blob {blob} (size={size})"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_replica_count_local_only() {
        let h = ContentHash::from_bytes(b"x");
        let r = BlockReplica::new(h, 3);
        assert_eq!(r.replica_count(), 1);
        assert_eq!(r.shortfall(), 2);
        assert!(!r.is_fully_replicated());
    }

    #[test]
    fn block_replica_count_with_remotes() {
        let h = ContentHash::from_bytes(b"x");
        let mut r = BlockReplica::new(h, 3);
        r.remote_providers.push(NodeAddr::new("p1"));
        r.remote_providers.push(NodeAddr::new("p2"));
        assert_eq!(r.replica_count(), 3);
        assert_eq!(r.shortfall(), 0);
        assert!(r.is_fully_replicated());
    }

    #[test]
    fn replica_set_fully_replicated_when_all_blocks_full() {
        let blob = ContentHash::from_bytes(b"blob");
        let mut s = ReplicaSet::new(blob, 3);
        let b1 = ContentHash::from_bytes(b"b1");
        let b2 = ContentHash::from_bytes(b"b2");
        let mut br1 = BlockReplica::new(b1.clone(), 3);
        br1.remote_providers.push(NodeAddr::new("p1"));
        br1.remote_providers.push(NodeAddr::new("p2"));
        let mut br2 = BlockReplica::new(b2.clone(), 3);
        br2.remote_providers.push(NodeAddr::new("p1"));
        br2.remote_providers.push(NodeAddr::new("p2"));
        s.blocks.insert(b1, br1);
        s.blocks.insert(b2, br2);
        assert!(s.is_fully_replicated());
        assert_eq!(s.under_replicated_blocks().len(), 0);
    }

    #[test]
    fn replica_set_under_replicated_blocks() {
        let blob = ContentHash::from_bytes(b"blob");
        let mut s = ReplicaSet::new(blob, 3);
        let b1 = ContentHash::from_bytes(b"b1");
        let b2 = ContentHash::from_bytes(b"b2");
        // b1 has only the local copy → 1 < 3 under-replicated.
        s.blocks
            .insert(b1.clone(), BlockReplica::new(b1.clone(), 3));
        // b2 has local + 2 remote → 3 fully replicated.
        let mut br2 = BlockReplica::new(b2.clone(), 3);
        br2.remote_providers.push(NodeAddr::new("p1"));
        br2.remote_providers.push(NodeAddr::new("p2"));
        s.blocks.insert(b2.clone(), br2);
        let under = s.under_replicated_blocks();
        assert!(
            under.contains(&b1),
            "b1 must be under-replicated; under={under:?}"
        );
        assert!(
            !under.contains(&b2),
            "b2 is fully replicated; under={under:?}"
        );
    }

    #[test]
    fn replication_policy_default_uses_three() {
        let p = ReplicationPolicy::default();
        assert_eq!(p.factor, 3);
        assert_eq!(p.max_concurrent_pushes, 4);
    }

    #[test]
    fn replica_message_roundtrip() {
        let msg = ReplicaMessage {
            blob: ContentHash::from_bytes(b"blob"),
            block: ContentHash::from_bytes(b"block"),
            index: 7,
            bytes: vec![1, 2, 3, 4],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ReplicaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn replica_ack_roundtrip() {
        let ack = ReplicaAck {
            blob: ContentHash::from_bytes(b"blob"),
            block: ContentHash::from_bytes(b"block"),
            received_bytes: 1024,
        };
        let json = serde_json::to_string(&ack).unwrap();
        let back: ReplicaAck = serde_json::from_str(&json).unwrap();
        assert_eq!(ack, back);
    }
}
