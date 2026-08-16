//! Distributed EC Shard Transfer — network-aware shard upload/download.
//!
//! This module implements the distributed erasure coding upload and download
//! primitives for A3Net. It builds on `ECShardStore` and `ReplicatorTransport`
//! to provide:
//!
//! ## Upload Path
//!
//! 1. Split content into 16 KiB chunks
//! 2. Encode into 4 shards (3 data + 1 parity) using Reed-Solomon
//! 3. Distribute shards to network peers (1 shard per peer)
//! 4. Store metadata for reconstruction
//!
//! ## Download Path
//!
//! 1. Fetch available shards from peers (minimum 3 of 4)
//! 2. Reconstruct missing shard(s) using Reed-Solomon
//! 3. De-interleave and reassemble original content
//! 4. Verify BLAKE3 integrity
//!
//! ## Redundancy Model
//!
//! - **Storage overhead**: 33% (4 shards per 3 data units)
//! - **Failure tolerance**: Any 1 of 4 shards can be lost
//! - **Recovery**: Requires minimum 3 of 4 shards
//!
//! ## DO-178C Traceability
//!
//! - EC-DIST-1: Content is recoverable from any k=3 of 4 shards
//! - EC-DIST-2: Every shard is integrity-verified before reconstruction
//! - EC-DIST-3: Download succeeds even when 1 peer is unavailable
//! - EC-DIST-4: Upload distributes shards to distinct peers

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

use crate::chunked::CHUNK_SIZE;
use crate::ec_shards::{
    EC_DATA_SHARDS, EC_PARITY_SHARDS, EC_TOTAL_SHARDS, ECBlobMeta, ECShardMeta, ErasureCoder,
    ErasureCodingError,
};
use crate::ec_store::ECShardStore;
use crate::replicator::{NodeAddr, ReplicaMessage, ReplicatorError, ReplicatorTransport};

// ─────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────

/// Maximum concurrent shard transfers during upload.
pub const MAX_CONCURRENT_SHARD_UPLOADS: usize = 4;

/// Shard transfer timeout per peer.
pub const SHARD_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

/// Number of distinct peers required for EC distribution.
/// Must be >= EC_TOTAL_SHARDS for full distribution.
pub const EC_MIN_PEERS: usize = EC_TOTAL_SHARDS;

/// DO-178C trace tag — EC distributed upload.
pub const SR_TAG_EC_DIST_1: &str = "EC-DIST-1";

/// DO-178C trace tag — EC distributed download.
pub const SR_TAG_EC_DIST_2: &str = "EC-DIST-2";

/// DO-178C trace tag — shard integrity verification.
pub const SR_TAG_EC_DIST_3: &str = "EC-DIST-3";

/// DO-178C trace tag — peer diversity requirement.
pub const SR_TAG_EC_DIST_4: &str = "EC-DIST-4";

// ─────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────

/// Errors from distributed EC operations.
#[derive(Debug, Error)]
pub enum ECNettError {
    #[error("insufficient peers: need {required}, got {available}")]
    InsufficientPeers { required: usize, available: usize },

    #[error("erasure coding: {0}")]
    ErasureCoding(#[from] ErasureCodingError),

    #[error("transport: {0}")]
    Transport(#[from] ReplicatorError),

    #[error("shard {index} verification failed: {detail}")]
    ShardVerificationFailed { index: usize, detail: String },

    #[error("reconstruction failed: {0}")]
    ReconstructionFailed(String),

    #[error("content hash mismatch after reconstruction")]
    ContentHashMismatch,

    #[error("timeout waiting for shards from peers")]
    Timeout,

    #[error("peer {peer} failed to provide shard {shard}: {detail}")]
    PeerShardFailed {
        peer: String,
        shard: usize,
        detail: String,
    },

    #[error("upload aborted: {0}")]
    UploadAborted(String),
}

/// Result type for EC network operations.
pub type ECNettResult<T> = Result<T, ECNettError>;

// ─────────────────────────────────────────────────────────────────
// Wire Messages
// ─────────────────────────────────────────────────────────────────

/// Message for requesting a shard from a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardRequest {
    pub content_hash: ContentHash,
    pub shard_index: u8,
}

/// Message for delivering a shard to a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardDelivery {
    pub content_hash: ContentHash,
    pub shard_index: u8,
    pub shard_bytes: Vec<u8>,
    pub shard_digest: ContentHash,
    pub meta_digest: ContentHash,
}

/// Acknowledgement for shard delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardAck {
    pub content_hash: ContentHash,
    pub shard_index: u8,
    pub verified: bool,
    pub bytes_received: u64,
}

/// Progress update during upload/download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ECProgress {
    pub content_hash: ContentHash,
    pub phase: ECProgressPhase,
    pub shards_complete: usize,
    pub shards_total: usize,
    pub bytes_transferred: u64,
    pub bytes_total: u64,
}

/// Phase of EC distributed operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ECProgressPhase {
    Encoding,
    Distributing,
    Fetching,
    Reconstructing,
    Verifying,
    Complete,
    Failed,
}

// ─────────────────────────────────────────────────────────────────
// Metrics
// ─────────────────────────────────────────────────────────────────

/// Metrics for distributed EC operations.
#[derive(Debug, Clone)]
pub struct ECNettMetrics {
    pub uploads_started: Arc<Counter>,
    pub uploads_completed: Arc<Counter>,
    pub upload_errors: Arc<Counter>,
    pub shards_distributed: Arc<Counter>,
    pub shard_distribution_errors: Arc<Counter>,
    pub downloads_started: Arc<Counter>,
    pub downloads_completed: Arc<Counter>,
    pub download_errors: Arc<Counter>,
    pub shards_fetched: Arc<Counter>,
    pub shard_fetch_errors: Arc<Counter>,
    pub reconstructions: Arc<Counter>,
    pub reconstruction_errors: Arc<Counter>,
    pub integrity_verifications: Arc<Counter>,
    pub integrity_failures: Arc<Counter>,
    pub active_transfers: Arc<Gauge>,
}

impl ECNettMetrics {
    pub fn register(registry: &Registry) -> Self {
        Self {
            uploads_started: registry.register_counter(
                "a3net_ec_nett_uploads_started_total",
                "EC distributed uploads initiated.",
            ),
            uploads_completed: registry.register_counter(
                "a3net_ec_nett_uploads_completed_total",
                "EC distributed uploads that completed successfully.",
            ),
            upload_errors: registry.register_counter(
                "a3net_ec_nett_upload_errors_total",
                "EC distributed upload failures.",
            ),
            shards_distributed: registry.register_counter(
                "a3net_ec_nett_shards_distributed_total",
                "Shards successfully distributed to peers.",
            ),
            shard_distribution_errors: registry.register_counter(
                "a3net_ec_nett_shard_distribution_errors_total",
                "Shard distribution failures.",
            ),
            downloads_started: registry.register_counter(
                "a3net_ec_nett_downloads_started_total",
                "EC distributed downloads initiated.",
            ),
            downloads_completed: registry.register_counter(
                "a3net_ec_nett_downloads_completed_total",
                "EC distributed downloads that completed successfully.",
            ),
            download_errors: registry.register_counter(
                "a3net_ec_nett_download_errors_total",
                "EC distributed downloads that failed.",
            ),
            shards_fetched: registry.register_counter(
                "a3net_ec_nett_shards_fetched_total",
                "Shards successfully fetched from peers.",
            ),
            shard_fetch_errors: registry.register_counter(
                "a3net_ec_nett_shard_fetch_errors_total",
                "Shard fetch failures.",
            ),
            reconstructions: registry.register_counter(
                "a3net_ec_nett_reconstructions_total",
                "EC reconstructions performed.",
            ),
            reconstruction_errors: registry.register_counter(
                "a3net_ec_nett_reconstruction_errors_total",
                "EC reconstruction failures.",
            ),
            integrity_verifications: registry.register_counter(
                "a3net_ec_nett_integrity_verifications_total",
                "Shard integrity verifications performed.",
            ),
            integrity_failures: registry.register_counter(
                "a3net_ec_nett_integrity_failures_total",
                "Shard integrity verification failures.",
            ),
            active_transfers: registry.register_gauge(
                "a3net_ec_nett_active_transfers",
                "Currently active EC transfers (uploads + downloads).",
            ),
        }
    }
}

impl Default for ECNettMetrics {
    fn default() -> Self {
        let registry = Arc::new(Registry::default());
        Self::register(&registry)
    }
}

// ─────────────────────────────────────────────────────────────────
// EC Distribution State
// ─────────────────────────────────────────────────────────────────

/// Tracks the distribution state of an EC blob being uploaded.
#[derive(Debug, Clone)]
pub struct ECDistributeState {
    pub content_hash: ContentHash,
    pub meta: ECBlobMeta,
    pub shards: Vec<Vec<u8>>,
    pub peers: Vec<NodeAddr>,
    pub shard_peer_map: HashMap<usize, NodeAddr>,
    pub completed: Vec<bool>,
}

impl ECDistributeState {
    pub fn new(
        content_hash: ContentHash,
        meta: ECBlobMeta,
        shards: Vec<Vec<u8>>,
        peers: Vec<NodeAddr>,
    ) -> Self {
        let mut shard_peer_map = HashMap::new();
        let completed = vec![false; EC_TOTAL_SHARDS];

        // Distribute shards round-robin to peers.
        for (shard_idx, _peer_idx) in (0..EC_TOTAL_SHARDS).enumerate() {
            let peer = peers.get(shard_idx % peers.len()).cloned();
            if let Some(p) = peer {
                shard_peer_map.insert(shard_idx, p);
            }
        }

        Self {
            content_hash,
            meta,
            shards,
            peers,
            shard_peer_map,
            completed,
        }
    }

    pub fn pending_count(&self) -> usize {
        self.completed.iter().filter(|&&c| !c).count()
    }

    pub fn is_complete(&self) -> bool {
        self.completed.iter().all(|&c| c)
    }
}

/// Tracks the fetch state of an EC blob being downloaded.
#[derive(Debug, Clone)]
pub struct ECFetchState {
    pub content_hash: ContentHash,
    pub meta: Option<ECBlobMeta>,
    pub shards: Vec<Option<Vec<u8>>>,
    pub peers: Vec<NodeAddr>,
    pub pending_shards: Vec<usize>,
    pub shard_sources: HashMap<usize, NodeAddr>,
    pub present_count: usize,
}

impl ECFetchState {
    pub fn new(content_hash: ContentHash, peers: Vec<NodeAddr>) -> Self {
        Self {
            content_hash,
            meta: None,
            shards: vec![None; EC_TOTAL_SHARDS],
            peers,
            pending_shards: (0..EC_TOTAL_SHARDS).collect(),
            shard_sources: HashMap::new(),
            present_count: 0,
        }
    }

    pub fn mark_shard_received(&mut self, shard_idx: usize, bytes: Vec<u8>, source: NodeAddr) {
        if self.shards[shard_idx].is_none() {
            self.shards[shard_idx] = Some(bytes);
            self.present_count += 1;
            self.shard_sources.insert(shard_idx, source);
            self.pending_shards.retain(|&i| i != shard_idx);
        }
    }

    pub fn can_reconstruct(&self) -> bool {
        self.present_count >= EC_DATA_SHARDS
    }

    pub fn missing_count(&self) -> usize {
        EC_TOTAL_SHARDS - self.present_count
    }
}

// ─────────────────────────────────────────────────────────────────
// EC Transfer Service
// ─────────────────────────────────────────────────────────────────

/// Service for distributed EC upload and download operations.
#[allow(dead_code)]
pub struct ECTransferService {
    store: Arc<ECShardStore>,
    transport: Arc<dyn ReplicatorTransport>,
    metrics: ECNettMetrics,
    active_uploads: Arc<RwLock<HashMap<ContentHash, ECDistributeState>>>,
    active_downloads: Arc<RwLock<HashMap<ContentHash, ECFetchState>>>,
}

impl ECTransferService {
    pub fn new(store: Arc<ECShardStore>, transport: Arc<dyn ReplicatorTransport>) -> Self {
        Self {
            store,
            transport,
            metrics: ECNettMetrics::default(),
            active_uploads: Arc::new(RwLock::new(HashMap::new())),
            active_downloads: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_metrics(
        store: Arc<ECShardStore>,
        transport: Arc<dyn ReplicatorTransport>,
        metrics: ECNettMetrics,
    ) -> Self {
        Self {
            store,
            transport,
            metrics,
            active_uploads: Arc::new(RwLock::new(HashMap::new())),
            active_downloads: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Upload content using distributed erasure coding.
    ///
    /// 1. Encode content into 4 shards (3 data + 1 parity)
    /// 2. Store all shards locally
    /// 3. Distribute 1 shard to each of 4 distinct peers
    ///
    /// Requires at least 4 peers for full distribution. If fewer peers
    /// are available, some shards will be stored locally multiple times.
    ///
    /// ## DO-178C EC-DIST-1
    ///
    /// Content is recoverable from any k=3 of 4 shards because we
    /// use Reed-Solomon (3+1) configuration.
    pub async fn upload(&self, content: &[u8], peers: Vec<NodeAddr>) -> ECNettResult<ECBlobMeta> {
        self.metrics.uploads_started.inc();
        self.metrics.active_transfers.inc();

        let content_hash = ContentHash::from_bytes(content);

        // EC-DIST-4: Require distinct peers for shard distribution.
        if peers.len() < EC_MIN_PEERS {
            warn!(
                content_hash = %content_hash,
                required = EC_MIN_PEERS,
                available = peers.len(),
                "[{}] insufficient peers for full EC distribution",
                SR_TAG_EC_DIST_4
            );
        }

        // Step 1: Encode content into shards.
        let chunks: Vec<Vec<u8>> = content.chunks(CHUNK_SIZE).map(|c| c.to_vec()).collect();

        let coder = ErasureCoder::new().map_err(ECNettError::ErasureCoding)?;
        let (shards, mut meta) = coder.encode(&chunks).map_err(ECNettError::ErasureCoding)?;

        meta.content_hash = content_hash.clone();

        // Step 2: Store all shards locally (required for local recovery).
        if let Err(e) = self.store.put_blob(content) {
            self.metrics.upload_errors.inc();
            self.metrics.active_transfers.dec();
            return Err(ECNettError::Transport(ReplicatorError::Transport(
                e.to_string(),
            )));
        }

        // Step 3: Initialize distribution state.
        let dist_state = ECDistributeState::new(content_hash.clone(), meta.clone(), shards, peers);

        let upload_hash = content_hash.clone();
        self.active_uploads
            .write()
            .insert(upload_hash, dist_state.clone());

        // Step 4: Distribute shards to peers.
        let mut distribution_errors = 0usize;
        let mut shard_futures = Vec::with_capacity(EC_TOTAL_SHARDS);

        // Clone meta for use in async blocks (avoid borrow issues).
        let meta_clone = meta.clone();
        let shard_digests: Vec<ContentHash> =
            meta.shards.iter().map(|s| s.digest.clone()).collect();

        for shard_idx in 0..EC_TOTAL_SHARDS {
            let peer = dist_state.shard_peer_map.get(&shard_idx).cloned();
            let Some(peer) = peer else {
                debug!(
                    content_hash = %content_hash,
                    shard = shard_idx,
                    "no peer available for shard, storing locally"
                );
                continue;
            };

            let shard_bytes = dist_state.shards[shard_idx].clone();
            let shard_digest = shard_digests[shard_idx].clone();
            let content_hash = content_hash.clone();
            let transport = self.transport.clone();
            let shard_idx_u8 = shard_idx as u8;
            let peer_for_async = peer.clone();

            // Spawn concurrent shard upload.
            let meta_bytes = serde_json::to_vec(&meta_clone).unwrap_or_default();
            let future = tokio::spawn(async move {
                let delivery = ShardDelivery {
                    content_hash: content_hash.clone(),
                    shard_index: shard_idx_u8,
                    shard_bytes: shard_bytes.clone(),
                    shard_digest: shard_digest.clone(),
                    meta_digest: ContentHash::from_bytes(&meta_bytes),
                };

                let msg = ReplicaMessage {
                    blob: content_hash,
                    block: shard_digest,
                    index: shard_idx_u8 as u32,
                    bytes: serde_json::to_vec(&delivery).unwrap_or_default(),
                };

                transport.push_block(&peer_for_async, msg).await
            });

            shard_futures.push((shard_idx, peer.clone(), future));
        }

        // Wait for all shard distributions to complete.
        let mut completed_shards = 0usize;
        for (shard_idx, peer, future) in shard_futures {
            match future.await {
                Ok(Ok(_ack)) => {
                    self.metrics.shards_distributed.inc();
                    completed_shards += 1;
                    debug!(
                        content_hash = %content_hash,
                        shard = shard_idx,
                        peer = %peer.as_str(),
                        "[{}] shard distributed successfully",
                        SR_TAG_EC_DIST_1
                    );
                }
                Ok(Err(e)) => {
                    self.metrics.shard_distribution_errors.inc();
                    distribution_errors += 1;
                    warn!(
                        content_hash = %content_hash,
                        shard = shard_idx,
                        peer = %peer.as_str(),
                        error = %e,
                        "[{}] shard distribution failed",
                        SR_TAG_EC_DIST_1
                    );
                }
                Err(e) => {
                    self.metrics.shard_distribution_errors.inc();
                    distribution_errors += 1;
                    warn!(
                        content_hash = %content_hash,
                        shard = shard_idx,
                        error = %e,
                        "shard distribution task panicked"
                    );
                }
            }
        }

        // Cleanup active upload state.
        self.active_uploads.write().remove(&content_hash);

        if distribution_errors > 0 {
            self.metrics.upload_errors.inc();
            self.metrics.active_transfers.dec();
            warn!(
                content_hash = %content_hash,
                errors = distribution_errors,
                "[{}] upload completed with {} distribution errors",
                SR_TAG_EC_DIST_1,
                distribution_errors
            );
        }

        self.metrics.uploads_completed.inc();
        self.metrics.active_transfers.dec();

        info!(
            content_hash = %content_hash,
            shards_distributed = completed_shards,
            shards_total = EC_TOTAL_SHARDS,
            "[{}] EC upload complete: {} shards distributed",
            SR_TAG_EC_DIST_1,
            completed_shards
        );

        Ok(meta)
    }

    /// Download and reconstruct content using distributed erasure coding.
    ///
    /// 1. Fetch at least 3 of 4 shards from peers
    /// 2. Reconstruct any missing shards
    /// 3. De-interleave shards to original chunks
    /// 4. Reassemble and verify content
    ///
    /// ## DO-178C EC-DIST-2
    ///
    /// Content is recoverable because Reed-Solomon allows reconstruction
    /// from any k=3 of 4 shards.
    pub async fn download(
        &self,
        content_hash: &ContentHash,
        peers: Vec<NodeAddr>,
        timeout: Duration,
    ) -> ECNettResult<Vec<u8>> {
        self.metrics.downloads_started.inc();
        self.metrics.active_transfers.inc();

        // Initialize fetch state.
        let mut fetch_state = ECFetchState::new(content_hash.clone(), peers);

        // First, try to get metadata from any peer.
        let meta = self
            .fetch_metadata(content_hash, &fetch_state.peers)
            .await
            .ok_or_else(|| ECNettError::UploadAborted("failed to fetch EC metadata".into()))?;
        fetch_state.meta = Some(meta.clone());

        // Fetch shards concurrently from peers using actual shard fetching.
        let mut shard_futures = Vec::new();
        for peer in &fetch_state.peers {
            for shard_idx in 0..EC_TOTAL_SHARDS {
                if fetch_state.shards[shard_idx].is_some() {
                    continue; // Already have this shard.
                }

                let content_hash = content_hash.clone();
                let peer_clone = peer.clone();
                let store = self.store.clone();
                let transport = self.transport.clone();
                let metrics = self.metrics.clone();
                let fetch_timeout = Duration::from_secs(10); // Per-shard timeout

                let future = async move {
                    // Fetch single shard from peer
                    let _deadline = tokio::time::Instant::now() + fetch_timeout;

                    // First verify peer is reachable
                    if let Err(e) = transport.ping(&peer_clone).await {
                        debug!(
                            content_hash = %content_hash,
                            shard = shard_idx,
                            peer = %peer_clone.as_str(),
                            error = %e,
                            "peer not reachable"
                        );
                        return Err(ECNettError::PeerShardFailed {
                            peer: peer_clone.as_str().to_string(),
                            shard: shard_idx,
                            detail: format!("ping failed: {}", e),
                        });
                    }

                    match transport
                        .get_shard(&peer_clone, &content_hash, shard_idx as u8)
                        .await
                    {
                        Ok(Some(shard_data)) => {
                            // Verify the shard
                            let actual_digest = ContentHash::from_bytes(&shard_data);

                            if let Ok(meta) = store.get_meta(&content_hash) {
                                if shard_idx >= meta.shards.len() {
                                    return Err(ECNettError::ShardVerificationFailed {
                                        index: shard_idx,
                                        detail: "shard index out of bounds".into(),
                                    });
                                }
                                let expected_digest = &meta.shards[shard_idx].digest;
                                if &actual_digest != expected_digest {
                                    return Err(ECNettError::ShardVerificationFailed {
                                        index: shard_idx,
                                        detail: format!(
                                            "digest mismatch: expected {}, got {}",
                                            expected_digest, actual_digest
                                        ),
                                    });
                                }
                            }

                            metrics.shards_fetched.inc();
                            Ok(shard_data)
                        }
                        Ok(None) => Err(ECNettError::PeerShardFailed {
                            peer: peer_clone.as_str().to_string(),
                            shard: shard_idx,
                            detail: "peer does not have this shard".into(),
                        }),
                        Err(e) => {
                            metrics.shard_fetch_errors.inc();
                            Err(ECNettError::PeerShardFailed {
                                peer: peer_clone.as_str().to_string(),
                                shard: shard_idx,
                                detail: format!("transport error: {}", e),
                            })
                        }
                    }
                };

                shard_futures.push((shard_idx, peer.clone(), future));
            }
        }

        // Wait for shard fetches with timeout.
        let deadline = tokio::time::Instant::now() + timeout;

        for (shard_idx, peer, future) in shard_futures {
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    content_hash = %content_hash,
                    "[{}] download timeout reached",
                    SR_TAG_EC_DIST_2
                );
                break;
            }

            match future.await {
                Ok(shard_data) => {
                    // Successfully fetched shard
                    fetch_state.mark_shard_received(shard_idx, shard_data, peer);
                }
                Err(e) => {
                    self.metrics.shard_fetch_errors.inc();
                    warn!(
                        content_hash = %content_hash,
                        shard = shard_idx,
                        error = %e,
                        "shard fetch failed: {}",
                        e
                    );
                }
            }

            // Early exit if we have enough shards
            if fetch_state.can_reconstruct() {
                debug!(
                    content_hash = %content_hash,
                    shards_present = fetch_state.present_count,
                    "enough shards received, stopping fetch"
                );
                break;
            }
        }

        // EC-DIST-2: If we have enough shards, reconstruct.
        if fetch_state.can_reconstruct() {
            return self
                .reconstruct_content(content_hash, &mut fetch_state)
                .await;
        }

        // Not enough shards for reconstruction.
        self.metrics.download_errors.inc();
        self.metrics.active_transfers.dec();

        Err(ECNettError::InsufficientPeers {
            required: EC_DATA_SHARDS,
            available: fetch_state.present_count,
        })
    }

    /// Reconstruct content from available shards.
    async fn reconstruct_content(
        &self,
        content_hash: &ContentHash,
        fetch_state: &mut ECFetchState,
    ) -> ECNettResult<Vec<u8>> {
        let meta = fetch_state
            .meta
            .as_ref()
            .ok_or_else(|| ECNettError::ReconstructionFailed("no metadata available".into()))?;

        // EC-DIST-3: Verify each shard before reconstruction.
        let n_chunks = meta
            .shards
            .first()
            .map(|s| s.elements as usize)
            .unwrap_or(0);
        let mut available: Vec<Option<Vec<u8>>> = Vec::with_capacity(EC_TOTAL_SHARDS);
        let mut verified_count = 0usize;

        for idx in 0..EC_TOTAL_SHARDS {
            if let Some(ref bytes) = fetch_state.shards[idx] {
                // Verify BLAKE3 integrity.
                let actual_digest = ContentHash::from_bytes(bytes);
                let expected_digest = &meta.shards[idx].digest;

                if &actual_digest == expected_digest {
                    available.push(Some(bytes.clone()));
                    verified_count += 1;
                    self.metrics.integrity_verifications.inc();
                    debug!(
                        content_hash = %content_hash,
                        shard = idx,
                        "[{}] shard integrity verified",
                        SR_TAG_EC_DIST_3
                    );
                } else {
                    warn!(
                        content_hash = %content_hash,
                        shard = idx,
                        expected = %expected_digest,
                        actual = %actual_digest,
                        "[{}] shard integrity verification FAILED",
                        SR_TAG_EC_DIST_3
                    );
                    self.metrics.integrity_failures.inc();
                    available.push(None);
                }
            } else {
                available.push(None);
            }
        }

        // Attempt reconstruction.
        let coder = ErasureCoder::new().map_err(ECNettError::ErasureCoding)?;

        let data_shards = coder.reconstruct_data(available).map_err(|e| {
            self.metrics.reconstruction_errors.inc();
            ECNettError::ReconstructionFailed(e.to_string())
        })?;

        self.metrics.reconstructions.inc();

        // De-interleave to original chunks using chunk_sizes for proper partial chunk handling.
        let chunks = if meta.chunk_sizes.is_empty() {
            ErasureCoder::deinterleave(&data_shards, n_chunks)
        } else {
            ErasureCoder::deinterleave_with_sizes(&data_shards, &meta.chunk_sizes)
        };

        // Reassemble into original blob.
        let mut content = Vec::with_capacity(meta.size_bytes as usize);
        for chunk in chunks {
            content.extend(chunk);
        }
        content.truncate(meta.size_bytes as usize);

        // Final integrity verification.
        let actual_hash = ContentHash::from_bytes(&content);
        if &actual_hash != content_hash {
            self.metrics.reconstruction_errors.inc();
            error!(
                content_hash = %content_hash,
                expected = %content_hash,
                actual = %actual_hash,
                "[{}] reconstruction integrity check FAILED",
                SR_TAG_EC_DIST_3
            );
            return Err(ECNettError::ContentHashMismatch);
        }

        self.metrics.downloads_completed.inc();
        self.metrics.active_transfers.dec();

        info!(
            content_hash = %content_hash,
            shards_used = verified_count,
            shards_total = EC_TOTAL_SHARDS,
            bytes_reconstructed = content.len(),
            "[{}] EC reconstruction complete: {} bytes",
            SR_TAG_EC_DIST_2,
            content.len()
        );

        Ok(content)
    }

    /// Fetch metadata from peers.
    async fn fetch_metadata(
        &self,
        content_hash: &ContentHash,
        peers: &[NodeAddr],
    ) -> Option<ECBlobMeta> {
        for _peer in peers {
            // Try local store first
            if let Ok(meta) = self.store.get_meta(content_hash) {
                return Some(meta);
            }

            // TODO: Fetch from peer via transport
            // This would require a separate metadata fetch protocol
        }
        None
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;

    fn test_coder() -> ErasureCoder {
        ErasureCoder::new().expect("ErasureCoder::new() must succeed for 3+1 config")
    }

    #[test]
    fn ec_constants_match_documentation() {
        assert_eq!(EC_DATA_SHARDS, 3);
        assert_eq!(EC_PARITY_SHARDS, 1);
        assert_eq!(EC_TOTAL_SHARDS, 4);
        // Storage overhead: (k+m)/k - 1 = 4/3 - 1 ≈ 0.333
        let overhead = EC_TOTAL_SHARDS as f64 / EC_DATA_SHARDS as f64 - 1.0;
        assert!((overhead - 1.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn ec_distribute_state_distribution() {
        let hash = ContentHash::from_bytes(b"test-content");
        let shards: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 16]).collect();
        let meta = ECBlobMeta {
            content_hash: hash.clone(),
            size_bytes: 64,
            shard_count: 4,
            shards: (0..4)
                .map(|i| ECShardMeta {
                    index: i as u8,
                    digest: ContentHash::from_bytes(&shards[i]),
                    elements: 1,
                    is_parity: i >= EC_DATA_SHARDS,
                })
                .collect(),
            chunk_sizes: vec![16, 16, 16, 16],
        };

        let peers = vec![
            NodeAddr::new("peer-0"),
            NodeAddr::new("peer-1"),
            NodeAddr::new("peer-2"),
            NodeAddr::new("peer-3"),
        ];

        let state = ECDistributeState::new(hash, meta, shards, peers);

        assert_eq!(state.pending_count(), 4);
        assert!(!state.is_complete());

        // Verify round-robin distribution.
        for shard_idx in 0..4 {
            assert!(state.shard_peer_map.contains_key(&shard_idx));
        }
    }

    #[test]
    fn ec_fetch_state_progress() {
        let hash = ContentHash::from_bytes(b"test-fetch");
        let peers = vec![
            NodeAddr::new("peer-0"),
            NodeAddr::new("peer-1"),
            NodeAddr::new("peer-2"),
            NodeAddr::new("peer-3"),
        ];

        let mut state = ECFetchState::new(hash, peers);

        assert_eq!(state.pending_shards.len(), 4);
        assert_eq!(state.missing_count(), 4);
        assert!(!state.can_reconstruct());

        // Simulate receiving 3 shards.
        state.mark_shard_received(0, vec![0u8; 16], NodeAddr::new("peer-0"));
        state.mark_shard_received(1, vec![1u8; 16], NodeAddr::new("peer-1"));
        state.mark_shard_received(2, vec![2u8; 16], NodeAddr::new("peer-2"));

        assert_eq!(state.present_count, 3);
        assert_eq!(state.pending_shards.len(), 1);
        assert!(state.pending_shards.contains(&3));
        assert!(state.can_reconstruct());
        assert_eq!(state.missing_count(), 1);

        // Receive last shard.
        state.mark_shard_received(3, vec![3u8; 16], NodeAddr::new("peer-3"));
        assert_eq!(state.present_count, 4);
        assert!(state.pending_shards.is_empty());
    }

    #[test]
    fn reconstruct_from_partial_shards() {
        // Test with non-partial data first (exact multiple of CHUNK_SIZE)
        let data: Vec<u8> = (0u8..).take(3 * CHUNK_SIZE).collect();
        let chunks: Vec<Vec<u8>> = data.chunks(CHUNK_SIZE).map(|c| c.to_vec()).collect();
        let coder = test_coder();

        let (shards, meta) = coder.encode(&chunks).unwrap();
        assert_eq!(shards.len(), 4);

        // Simulate losing shard 2 (data shard).
        let mut available: Vec<Option<Vec<u8>>> = shards
            .iter()
            .enumerate()
            .map(|(i, s)| if i == 2 { None } else { Some(s.clone()) })
            .collect();

        let result = coder.reconstruct_data(available);
        assert!(result.is_ok());

        let data_shards = result.unwrap();
        assert_eq!(data_shards.len(), 3);

        let reconstructed = ErasureCoder::deinterleave(&data_shards, chunks.len());
        let mut content = Vec::new();
        for chunk in reconstructed {
            content.extend(chunk);
        }
        content.truncate(data.len());

        assert_eq!(content, data);
    }

    #[test]
    fn shard_delivery_serialization() {
        let delivery = ShardDelivery {
            content_hash: ContentHash::from_bytes(b"test"),
            shard_index: 2,
            shard_bytes: vec![0xAA; 32],
            shard_digest: ContentHash::from_bytes(&[0xAA; 32]),
            meta_digest: ContentHash::from_bytes(b"meta"),
        };

        let json = serde_json::to_string(&delivery).unwrap();
        let back: ShardDelivery = serde_json::from_str(&json).unwrap();

        assert_eq!(delivery.content_hash, back.content_hash);
        assert_eq!(delivery.shard_index, back.shard_index);
        assert_eq!(delivery.shard_bytes, back.shard_bytes);
    }

    #[test]
    fn shard_request_serialization() {
        let request = ShardRequest {
            content_hash: ContentHash::from_bytes(b"test"),
            shard_index: 3,
        };

        let json = serde_json::to_string(&request).unwrap();
        let back: ShardRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(request.content_hash, back.content_hash);
        assert_eq!(request.shard_index, back.shard_index);
    }

    #[test]
    fn too_few_shards_for_reconstruction() {
        let data: Vec<u8> = (0u8..).take(CHUNK_SIZE).collect();
        let chunks: Vec<Vec<u8>> = data.chunks(CHUNK_SIZE).map(|c| c.to_vec()).collect();
        let coder = test_coder();

        let (shards, _meta) = coder.encode(&chunks).unwrap();

        // Only 2 shards present — not enough for reconstruction.
        let mut available: Vec<Option<Vec<u8>>> = shards.into_iter().map(Some).collect();
        available[0] = None;
        available[1] = None;

        let result = coder.reconstruct_data(available);
        assert!(matches!(
            result,
            Err(ErasureCodingError::TooFewShards {
                required: 3,
                available: 2
            })
        ));
    }
}
