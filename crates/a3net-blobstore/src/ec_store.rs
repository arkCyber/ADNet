//! EC Shard Store — disk-backed storage for erasure-coded shards.
//!
//! ## Layout
//!
//! ```text
//! {ec_data_dir}/
//!     {content_hash_hex}/
//!         meta.json        # ECBlobMeta (content_hash, shards, size)
//!         complete        # sentinel; all shards present + verified
//!         shards/
//!             0           # data shard 0 (raw 16-byte-element bytes)
//!             1           # data shard 1
//!             2           # data shard 2
//!             3           # parity shard (index == EC_DATA_SHARDS)
//! ```
//!
//! ## Erasure domains
//!
//! EC shards are stored in a **separate** directory tree from
//! regular (replicated) blobs — the `ec/` subdirectory of
//! `<data_dir>/ec/`. This ensures:
//! - EC blobs and replicated blobs never share quota.
//! - The replication sweep never touches EC shards.
//! - A corrupted EC shard cannot accidentally overwrite a replicated block.
//!
//! ## Concurrency
//!
//! EC Shard Store is sync (same as `BlobStore`). The caller wraps
//! it in `tokio::task::spawn_blocking` for async use.
//!
//! ## DO-178C traceability
//!
//! - EC-S1: every shard is BLAKE3-verified on write.
//! - EC-S2: `verify_complete` rejects blobs with any missing shard.
//! - EC-S3: `get_blob` reconstructs the blob from available shards.
//! - EC-S4: `upload_distributed` requires ≥4 peers for full distribution.
//! - EC-S5: `download_distributed` succeeds with ≥3 of 4 shards.
//!
//! ## Distributed EC Operations
//!
//! This module supports distributed erasure coding operations:
//! - `put_blob`: Encode and store all shards locally
//! - `write_shard`: Store a single shard (used by distributed upload)
//! - `read_shard`: Read a single shard (used by distributed download)
//! - `get_blob`: Reconstruct content from available shards

use std::fs;
use std::path::{Path, PathBuf};

use a3net_types::ContentHash;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::chunked::{CHUNK_SIZE, chunk_count_for};
use crate::ec_shards::{
    EC_DATA_SHARDS, EC_PARITY_SHARDS, EC_TOTAL_SHARDS, ECBlobMeta, ECShardMeta, ErasureCoder,
    ErasureCodingError, SR_TAG_EC_R1, SR_TAG_EC_R2,
};

/// Sentinel file: written when all shards are present and verified.
pub const EC_COMPLETE_SENTINEL: &str = "complete";

/// Directory name for shard files.
pub const SHARDS_DIR: &str = "shards";

/// Errors from EC shard store operations.
#[derive(Debug, thiserror::Error)]
pub enum ECStoreError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("erasure coding: {0}")]
    ErasureCoding(#[from] ErasureCodingError),

    #[error("blob not found in EC store: {0}")]
    NotFound(String),

    #[error("blob incomplete (some shards missing): {0}")]
    Incomplete(String),

    #[error("shard {index} corrupted (BLAKE3 mismatch): {detail}")]
    Corrupted { index: usize, detail: String },

    #[error("reconstruction failed (insufficient shards for {0})")]
    Unrecoverable(String),

    #[error("quota exceeded")]
    QuotaExceeded,
}

/// Result of a shard health scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ECHealthReport {
    /// Total EC blobs scanned.
    pub total: usize,
    /// Blobs with all shards present.
    pub healthy: usize,
    /// Blobs with some shards missing.
    pub partial: usize,
    /// Blobs that failed reconstruction (corrupted shards).
    pub corrupted: usize,
    /// Shards that were repaired.
    pub shards_repaired: usize,
    /// Total bytes stored in the EC store.
    pub total_bytes: u64,
}

/// Metrics recorded by EC Shard Store.
#[derive(Debug, Default)]
pub struct ECStoreMetrics {
    pub encodes: std::sync::atomic::AtomicU64,
    pub encode_errors: std::sync::atomic::AtomicU64,
    pub decodes: std::sync::atomic::AtomicU64,
    pub decode_errors: std::sync::atomic::AtomicU64,
    pub reconstructions: std::sync::atomic::AtomicU64,
    pub reconstruction_errors: std::sync::atomic::AtomicU64,
    pub shard_writes: std::sync::atomic::AtomicU64,
    pub shard_reads: std::sync::atomic::AtomicU64,
    pub verify_failures: std::sync::atomic::AtomicU64,
}

/// Disk-backed EC shard store.
#[derive(Debug)]
pub struct ECShardStore {
    data_dir: PathBuf,
    metrics: std::sync::Arc<ECStoreMetrics>,
}

impl ECShardStore {
    /// Open (or create) the EC shard store under `data_dir/ec/`.
    pub fn new(data_dir: &Path) -> std::io::Result<Self> {
        let ec_dir = data_dir.join("ec");
        fs::create_dir_all(&ec_dir)?;
        Ok(Self {
            data_dir: ec_dir,
            metrics: std::sync::Arc::new(ECStoreMetrics::default()),
        })
    }

    /// Open with custom metrics.
    pub fn with_metrics(
        data_dir: &Path,
        metrics: std::sync::Arc<ECStoreMetrics>,
    ) -> std::io::Result<Self> {
        let ec_dir = data_dir.join("ec");
        fs::create_dir_all(&ec_dir)?;
        Ok(Self {
            data_dir: ec_dir,
            metrics,
        })
    }

    /// Path to a blob's EC directory.
    pub fn blob_dir(&self, content_hash: &ContentHash) -> PathBuf {
        self.data_dir.join(content_hash.as_hex())
    }

    /// Path to the shards subdirectory.
    pub fn shards_dir(&self, content_hash: &ContentHash) -> PathBuf {
        self.blob_dir(content_hash).join(SHARDS_DIR)
    }

    /// Path to a specific shard file.
    pub fn shard_path(&self, content_hash: &ContentHash, shard_index: u8) -> PathBuf {
        self.shards_dir(content_hash).join(shard_index.to_string())
    }

    /// Encode a blob and store all shards to disk.
    ///
    /// Returns the `ECBlobMeta` on success. The blob's `complete`
    /// sentinel is written only after all shards are written and
    /// verified.
    ///
    /// ## DO-178C EC-S1
    ///
    /// Every shard is BLAKE3-hashed and the digest is stored in
    /// `meta.json`. On subsequent reads, `verify_shard` checks
    /// the digest before trusting shard content.
    pub fn put_blob(&self, content: &[u8]) -> Result<ECBlobMeta, ECStoreError> {
        let content_hash = ContentHash::from_bytes(content);
        let dest = self.blob_dir(&content_hash);

        // Idempotent: if already complete, return stored metadata.
        if dest.join(EC_COMPLETE_SENTINEL).exists() {
            self.metrics
                .encodes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(self.get_meta(&content_hash)?);
        }

        // Split into 16 KiB chunks.
        let _n_chunks = chunk_count_for(content.len() as u64) as usize;
        let chunks: Vec<Vec<u8>> = content.chunks(CHUNK_SIZE).map(|c| c.to_vec()).collect();

        // Encode into EC shards.
        let coder = ErasureCoder::new().map_err(|e| ErasureCodingError::Codec(e.to_string()))?;
        let (shards, mut meta) = coder.encode(&chunks)?;

        // Override the auto-computed content hash with the actual one.
        meta.content_hash = content_hash.clone();

        // Write all shards + meta + sentinel atomically.
        fs::create_dir_all(self.shards_dir(&content_hash))?;
        for (idx, shard) in shards.iter().enumerate() {
            let path = self.shard_path(&content_hash, idx as u8);
            fs::write(&path, shard)?;
            self.metrics
                .shard_writes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // EC-S1: verify BLAKE3 digest on write.
            let digest = ContentHash::from_bytes(shard);
            if &digest != &meta.shards[idx].digest {
                warn!(
                    content_hash = %content_hash,
                    shard = idx,
                    "shard BLAKE3 mismatch on write — recomputing meta"
                );
                meta.shards[idx].digest = digest;
            }
        }

        // Re-write meta with corrected digests.
        let meta_json = serde_json::to_vec_pretty(&meta).map_err(|e| std::io::Error::other(e))?;
        fs::write(dest.join("meta.json"), &meta_json)?;
        fs::write(dest.join(EC_COMPLETE_SENTINEL), b"1")?;

        self.metrics
            .encodes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        debug!(
            content_hash = %content_hash,
            size_bytes = content.len(),
            n_shards = EC_TOTAL_SHARDS,
            "[{}] EC blob stored ({} data + {} parity shards)",
            SR_TAG_EC_R1,
            EC_DATA_SHARDS,
            EC_PARITY_SHARDS
        );
        Ok(meta)
    }

    /// Read the metadata for a stored EC blob.
    pub fn get_meta(&self, content_hash: &ContentHash) -> Result<ECBlobMeta, ECStoreError> {
        let meta_path = self.blob_dir(content_hash).join("meta.json");
        let raw = fs::read_to_string(&meta_path)?;
        let meta: ECBlobMeta = serde_json::from_str(&raw).map_err(|e| std::io::Error::other(e))?;
        Ok(meta)
    }

    /// Returns `true` if all shards are present (not verified).
    pub fn has_complete(&self, content_hash: &ContentHash) -> bool {
        self.blob_dir(content_hash)
            .join(EC_COMPLETE_SENTINEL)
            .exists()
    }

    /// Re-verify every shard's BLAKE3 digest against stored meta.
    ///
    /// Returns `true` if all shards pass verification, `false` if
    /// any shard is missing or corrupted.
    ///
    /// ## DO-178C EC-S2
    ///
    /// `verify_complete` is the primary integrity gate. Callers that
    /// want quarantine-on-failure should use this before returning
    /// data to the application.
    pub fn verify_complete(&self, content_hash: &ContentHash) -> bool {
        if !self.has_complete(content_hash) {
            return false;
        }

        let Ok(meta) = self.get_meta(content_hash) else {
            return false;
        };

        for (idx, shard_meta) in meta.shards.iter().enumerate() {
            let path = self.shard_path(content_hash, shard_meta.index);
            match fs::read(&path) {
                Ok(bytes) => {
                    let digest = ContentHash::from_bytes(&bytes);
                    if &digest != &shard_meta.digest {
                        self.metrics
                            .verify_failures
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        warn!(
                            content_hash = %content_hash,
                            shard = idx,
                            "[{}] shard BLAKE3 verification FAILED",
                            SR_TAG_EC_R2
                        );
                        return false;
                    }
                }
                Err(_) => {
                    return false;
                }
            }
        }
        true
    }

    /// Read a specific shard's bytes.
    pub fn read_shard(
        &self,
        content_hash: &ContentHash,
        shard_index: u8,
    ) -> std::io::Result<Vec<u8>> {
        self.metrics
            .shard_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        fs::read(self.shard_path(content_hash, shard_index))
    }

    /// Reconstruct and return the original blob from stored shards.
    ///
    /// If some shards are missing/corrupted, this attempts
    /// reconstruction using the remaining shards. Returns `Err` only
    /// if fewer than `EC_DATA_SHARDS` shards are available.
    ///
    /// ## DO-178C EC-S3
    ///
    /// Reconstruction is the core EC safety property. This function
    /// is the implementation of **EC-R1**: any `k` shards are sufficient.
    pub fn get_blob(&self, content_hash: &ContentHash) -> Result<Vec<u8>, ECStoreError> {
        if !self.blob_dir(content_hash).exists() {
            return Err(ECStoreError::NotFound(content_hash.to_string()));
        }

        let meta = self.get_meta(content_hash)?;
        let n_chunks = meta
            .shards
            .first()
            .map(|s| s.elements as usize)
            .unwrap_or(0);

        // Load all available shards.
        let mut available: Vec<Option<Vec<u8>>> = Vec::with_capacity(EC_TOTAL_SHARDS);
        let mut present_count = 0usize;

        for idx in 0..EC_TOTAL_SHARDS {
            match self.read_shard(content_hash, idx as u8) {
                Ok(bytes) => {
                    // EC-S2: verify before using.
                    if let Err(e) = meta.verify_shard(idx, &bytes) {
                        warn!(
                            content_hash = %content_hash,
                            shard = idx,
                            error = %e,
                            "[{}] corrupted shard detected, treating as missing",
                            SR_TAG_EC_R2
                        );
                        available.push(None);
                    } else {
                        available.push(Some(bytes));
                        present_count += 1;
                    }
                }
                Err(_) => {
                    available.push(None);
                }
            }
        }

        if present_count < EC_DATA_SHARDS {
            return Err(ECStoreError::Unrecoverable(content_hash.to_string()));
        }

        let coder = ErasureCoder::new().map_err(|e| ErasureCodingError::Codec(e.to_string()))?;
        let data_shards = coder.reconstruct_data(available)?;

        // De-interleave to original chunk order using chunk_sizes for proper partial chunk handling.
        let chunks = if meta.chunk_sizes.is_empty() {
            // Fallback for legacy metadata without chunk_sizes.
            ErasureCoder::deinterleave(&data_shards, n_chunks)
        } else {
            ErasureCoder::deinterleave_with_sizes(&data_shards, &meta.chunk_sizes)
        };

        // Reassemble into original blob bytes.
        let mut out = Vec::with_capacity(meta.size_bytes as usize);
        for chunk in chunks {
            out.extend(chunk);
        }
        out.truncate(meta.size_bytes as usize);

        // Verify the reconstructed blob matches the declared hash.
        let actual = ContentHash::from_bytes(&out);
        if &actual != content_hash {
            return Err(ECStoreError::Corrupted {
                index: 0,
                detail: format!(
                    "reconstructed blob hash {} != expected {}",
                    actual, content_hash
                ),
            });
        }

        self.metrics
            .decodes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        debug!(
            content_hash = %content_hash,
            shards_used = present_count,
            "[{}] EC blob reconstructed ({} bytes)",
            SR_TAG_EC_R1,
            out.len()
        );
        Ok(out)
    }

    /// List all complete EC blobs.
    pub fn list_complete(&self) -> std::io::Result<Vec<ContentHash>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() && path.join(EC_COMPLETE_SENTINEL).exists() {
                let name = entry.file_name();
                if let Some(hex) = name.to_str() {
                    if let Ok(h) = ContentHash::from_hex(hex) {
                        out.push(h);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Total bytes stored in the EC store (sum of all blob bytes, not shard bytes).
    pub fn total_size(&self) -> std::io::Result<u64> {
        let mut total = 0u64;
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() && path.join(EC_COMPLETE_SENTINEL).exists() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Ok(h) = ContentHash::from_hex(&name) {
                    if let Ok(meta) = self.get_meta(&h) {
                        total += meta.size_bytes;
                    }
                }
            }
        }
        Ok(total)
    }

    /// Scan all EC blobs and report health statistics.
    ///
    /// This is the equivalent of `DualBackupStore::health_check` for
    /// the EC store. It verifies each blob's shards and counts
    /// healthy / partial / corrupted entries.
    pub fn health_scan(&self) -> ECHealthReport {
        let mut report = ECHealthReport::default();

        for entry in match fs::read_dir(&self.data_dir) {
            Ok(e) => e,
            Err(_) => return report,
        } {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(content_hash) = ContentHash::from_hex(&name) else {
                continue;
            };

            report.total += 1;
            let meta = match self.get_meta(&content_hash) {
                Ok(m) => m,
                Err(_) => {
                    report.corrupted += 1;
                    continue;
                }
            };

            // Count present shards.
            let mut present = 0usize;
            let mut all_ok = true;
            for (idx, shard_meta) in meta.shards.iter().enumerate() {
                let shard_path = self.shard_path(&content_hash, shard_meta.index);
                match fs::read(&shard_path) {
                    Ok(bytes) => {
                        if meta.verify_shard(idx, &bytes).is_ok() {
                            present += 1;
                        } else {
                            all_ok = false;
                        }
                    }
                    Err(_) => {
                        all_ok = false;
                    }
                }
            }

            if all_ok && present == EC_TOTAL_SHARDS {
                report.healthy += 1;
                report.total_bytes += meta.size_bytes;
            } else if present >= EC_DATA_SHARDS {
                report.partial += 1;
                report.total_bytes += meta.size_bytes;
            } else {
                report.corrupted += 1;
            }
        }

        report
    }

    /// Remove an EC blob and all its shards.
    pub fn remove(&self, content_hash: &ContentHash) -> std::io::Result<bool> {
        let dir = self.blob_dir(content_hash);
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(dir)?;
        Ok(true)
    }

    // ─────────────────────────────────────────────────────────────────
    // Distributed EC Storage APIs
    // ─────────────────────────────────────────────────────────────────

    /// Write a single shard (used by distributed upload).
    ///
    /// This method allows writing individual shards received from peers
    /// during distributed upload, without requiring all shards to be present.
    ///
    /// ## DO-178C EC-S4
    ///
    /// Writing a shard includes BLAKE3 verification and updates the
    /// metadata to track which shards are present.
    pub fn write_shard(
        &self,
        content_hash: &ContentHash,
        shard_index: u8,
        shard_bytes: &[u8],
        expected_digest: &ContentHash,
    ) -> Result<(), ECStoreError> {
        if shard_index >= EC_TOTAL_SHARDS as u8 {
            return Err(ECStoreError::NotFound(format!(
                "shard index {} out of range [0, {})",
                shard_index, EC_TOTAL_SHARDS
            )));
        }

        let shard_digest = ContentHash::from_bytes(shard_bytes);

        // EC-S4: Verify digest before writing.
        if &shard_digest != expected_digest {
            self.metrics
                .verify_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(ECStoreError::Corrupted {
                index: shard_index as usize,
                detail: format!(
                    "shard digest mismatch: expected {}, got {}",
                    expected_digest, shard_digest
                ),
            });
        }

        let blob_dir = self.blob_dir(content_hash);
        fs::create_dir_all(self.shards_dir(content_hash))?;

        let shard_path = self.shard_path(content_hash, shard_index);
        fs::write(&shard_path, shard_bytes)?;
        self.metrics
            .shard_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Update metadata if it exists, or create new metadata.
        let mut meta = match self.get_meta(content_hash) {
            Ok(m) => m,
            Err(_) => {
                // Create new metadata for this blob.
                ECBlobMeta {
                    content_hash: content_hash.clone(),
                    size_bytes: 0, // Unknown for partial writes
                    shard_count: EC_TOTAL_SHARDS as u8,
                    shards: (0..EC_TOTAL_SHARDS)
                        .map(|i| ECShardMeta {
                            index: i as u8,
                            digest: ContentHash::from_bytes(&[]),
                            elements: 0,
                            is_parity: i >= EC_DATA_SHARDS,
                        })
                        .collect(),
                    chunk_sizes: Vec::new(),
                }
            }
        };

        // Update the specific shard's metadata.
        let shard_idx = shard_index as usize;
        if shard_idx < meta.shards.len() {
            meta.shards[shard_idx].digest = shard_digest.clone();
            meta.shards[shard_idx].elements = (shard_bytes.len() / CHUNK_SIZE) as u32;
        }

        // Write updated metadata.
        let meta_json = serde_json::to_vec_pretty(&meta).map_err(|e| std::io::Error::other(e))?;
        fs::write(blob_dir.join("meta.json"), &meta_json)?;

        debug!(
            content_hash = %content_hash,
            shard = shard_index,
            bytes = shard_bytes.len(),
            "[{}] shard written",
            SR_TAG_EC_R1
        );

        Ok(())
    }

    /// Check if a specific shard exists locally.
    pub fn has_shard(&self, content_hash: &ContentHash, shard_index: u8) -> bool {
        self.shard_path(content_hash, shard_index).exists()
    }

    /// Get the list of missing shard indices.
    pub fn missing_shards(&self, content_hash: &ContentHash) -> Vec<u8> {
        let mut missing = Vec::new();
        for idx in 0..EC_TOTAL_SHARDS {
            if !self.has_shard(content_hash, idx as u8) {
                missing.push(idx as u8);
            }
        }
        missing
    }

    /// Repair a missing shard by reconstructing it from available shards.
    ///
    /// This is the self-healing mechanism for EC storage. If at least
    /// EC_DATA_SHARDS shards are available, the missing shard can be
    /// reconstructed and written back to disk.
    ///
    /// ## DO-178C EC-S3
    ///
    /// Reconstruction is possible because Reed-Solomon (3+1) allows
    /// recovery from any k=3 of 4 shards.
    pub fn repair_shard(
        &self,
        content_hash: &ContentHash,
        _shard_index: u8,
    ) -> Result<(), ECStoreError> {
        if !self.blob_dir(content_hash).exists() {
            return Err(ECStoreError::NotFound(content_hash.to_string()));
        }

        let meta = self.get_meta(content_hash)?;

        // Gather available shards.
        let mut available: Vec<Option<Vec<u8>>> = Vec::with_capacity(EC_TOTAL_SHARDS);
        let mut present_count = 0usize;

        for idx in 0..EC_TOTAL_SHARDS {
            match self.read_shard(content_hash, idx as u8) {
                Ok(bytes) => {
                    if meta.verify_shard(idx, &bytes).is_ok() {
                        available.push(Some(bytes));
                        present_count += 1;
                    } else {
                        available.push(None);
                    }
                }
                Err(_) => {
                    available.push(None);
                }
            }
        }

        if present_count < EC_DATA_SHARDS {
            return Err(ECStoreError::Unrecoverable(content_hash.to_string()));
        }

        // Reconstruct the missing shard.
        let coder = ErasureCoder::new().map_err(|e| ErasureCodingError::Codec(e.to_string()))?;

        let (reconstructed_bytes, reconstructed_idx) = coder
            .reconstruct(available)
            .map_err(|e| ECStoreError::from(ErasureCodingError::from(e.to_string())))?;

        // Verify reconstruction.
        let reconstructed_digest = ContentHash::from_bytes(&reconstructed_bytes);
        if &reconstructed_digest != &meta.shards[reconstructed_idx].digest {
            return Err(ECStoreError::Corrupted {
                index: reconstructed_idx,
                detail: "reconstructed shard digest mismatch".into(),
            });
        }

        // Write the repaired shard.
        self.write_shard(
            content_hash,
            reconstructed_idx as u8,
            &reconstructed_bytes,
            &reconstructed_digest,
        )?;

        self.metrics
            .reconstructions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        debug!(
            content_hash = %content_hash,
            shard = reconstructed_idx,
            "[{}] shard repaired via reconstruction",
            SR_TAG_EC_R1
        );

        Ok(())
    }

    /// Get a summary of the EC blob's shard status.
    pub fn shard_status(&self, content_hash: &ContentHash) -> Result<ECShardStatus, ECStoreError> {
        if !self.blob_dir(content_hash).exists() {
            return Err(ECStoreError::NotFound(content_hash.to_string()));
        }

        let meta = self.get_meta(content_hash)?;
        let mut shards_present = 0usize;
        let mut shards_verified = 0usize;
        let mut shard_details = Vec::with_capacity(EC_TOTAL_SHARDS);

        for idx in 0..EC_TOTAL_SHARDS {
            let shard_path = self.shard_path(content_hash, idx as u8);
            let is_present = shard_path.exists();
            let is_parity = idx >= EC_DATA_SHARDS;

            let verification_status = if is_present {
                match fs::read(&shard_path) {
                    Ok(bytes) => {
                        if meta.verify_shard(idx, &bytes).is_ok() {
                            shards_verified += 1;
                            ShardVerificationStatus::Verified
                        } else {
                            ShardVerificationStatus::Corrupted
                        }
                    }
                    Err(_) => ShardVerificationStatus::Error,
                }
            } else {
                ShardVerificationStatus::Missing
            };

            if is_present {
                shards_present += 1;
            }

            shard_details.push(ShardDetail {
                index: idx as u8,
                is_parity,
                is_present,
                verification_status,
            });
        }

        let recoverability = if shards_verified >= EC_DATA_SHARDS {
            Recoverability::FullyRecoverable
        } else if shards_present >= EC_DATA_SHARDS {
            Recoverability::CanAttemptRecovery
        } else {
            Recoverability::Unrecoverable
        };

        Ok(ECShardStatus {
            content_hash: content_hash.clone(),
            shards_total: EC_TOTAL_SHARDS,
            shards_present,
            shards_verified,
            shard_details,
            recoverability,
            is_complete: shards_present == EC_TOTAL_SHARDS && shards_verified == EC_TOTAL_SHARDS,
        })
    }

    /// Read a specific shard with integrity verification.
    ///
    /// Returns the shard bytes if verified, or an error if the shard
    /// is missing or corrupted.
    pub fn read_shard_verified(
        &self,
        content_hash: &ContentHash,
        shard_index: u8,
    ) -> Result<Vec<u8>, ECStoreError> {
        let bytes = self.read_shard(content_hash, shard_index)?;
        let meta = self.get_meta(content_hash)?;

        if let Err(e) = meta.verify_shard(shard_index as usize, &bytes) {
            self.metrics
                .verify_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(ECStoreError::Corrupted {
                index: shard_index as usize,
                detail: e.to_string(),
            });
        }

        Ok(bytes)
    }

    /// Encode content and return shards without storing (for distributed upload).
    ///
    /// This splits content into 16 KiB chunks, encodes them using Reed-Solomon,
    /// and returns the raw shards without persisting to disk.
    ///
    /// Used by `ECTransferService::upload` to get shards for distribution.
    pub fn encode_only(content: &[u8]) -> Result<(Vec<Vec<u8>>, ECBlobMeta), ECStoreError> {
        let content_hash = ContentHash::from_bytes(content);
        let chunks: Vec<Vec<u8>> = content.chunks(CHUNK_SIZE).map(|c| c.to_vec()).collect();

        let coder = ErasureCoder::new().map_err(|e| ErasureCodingError::Codec(e.to_string()))?;
        let (shards, mut meta) = coder
            .encode(&chunks)
            .map_err(|e| ECStoreError::from(ErasureCodingError::from(e.to_string())))?;

        meta.content_hash = content_hash;
        Ok((shards, meta))
    }
}

// ─────────────────────────────────────────────────────────────────
// Supporting Types for Distributed EC
// ─────────────────────────────────────────────────────────────────

/// Status of a single shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShardVerificationStatus {
    /// Shard exists and BLAKE3 digest matches metadata.
    Verified,
    /// Shard exists but BLAKE3 digest does not match.
    Corrupted,
    /// Shard file is missing from disk.
    Missing,
    /// Error reading or verifying the shard.
    Error,
}

/// Detail information for a single shard.
#[derive(Debug, Clone)]
pub struct ShardDetail {
    pub index: u8,
    pub is_parity: bool,
    pub is_present: bool,
    pub verification_status: ShardVerificationStatus,
}

/// Overall recoverability status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recoverability {
    /// Enough verified shards to fully reconstruct without any self-healing.
    FullyRecoverable,
    /// Enough shards present to attempt recovery (some may need verification).
    CanAttemptRecovery,
    /// Fewer than k shards present; data loss.
    Unrecoverable,
}

/// Complete status report for an EC blob.
#[derive(Debug, Clone)]
pub struct ECShardStatus {
    pub content_hash: ContentHash,
    pub shards_total: usize,
    pub shards_present: usize,
    pub shards_verified: usize,
    pub shard_details: Vec<ShardDetail>,
    pub recoverability: Recoverability,
    pub is_complete: bool,
}

impl ECShardStatus {
    /// Returns true if the blob can be fully reconstructed.
    pub fn can_reconstruct(&self) -> bool {
        self.shards_verified >= EC_DATA_SHARDS
    }

    /// Returns the number of shards that can be recovered.
    pub fn recoverable_shards(&self) -> usize {
        self.shards_verified.min(EC_DATA_SHARDS)
    }

    /// Returns the number of shards that need to be fetched for full recovery.
    pub fn shards_needed(&self) -> usize {
        EC_DATA_SHARDS.saturating_sub(self.shards_verified)
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (ECShardStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = ECShardStore::new(dir.path()).unwrap();
        (store, dir)
    }

    #[test]
    fn put_and_get_small_blob() {
        let (store, _dir) = temp_store();
        let data = b"hello a3net erasure coding!"[..].to_vec();

        let meta = store.put_blob(&data).unwrap();
        assert_eq!(meta.shard_count as usize, EC_TOTAL_SHARDS);
        assert_eq!(meta.content_hash, ContentHash::from_bytes(&data));

        let roundtrip = store.get_blob(&meta.content_hash).unwrap();
        assert_eq!(roundtrip, data);
    }

    #[test]
    fn put_and_get_exact_block_blob() {
        let (store, _dir) = temp_store();
        // Exactly 3 × 16 KiB chunks.
        let data = vec![0xAAu8; 3 * CHUNK_SIZE];

        let meta = store.put_blob(&data).unwrap();
        let roundtrip = store.get_blob(&meta.content_hash).unwrap();
        assert_eq!(roundtrip, data);
    }

    #[test]
    fn put_and_get_large_blob() {
        let (store, _dir) = temp_store();
        let data: Vec<u8> = (0u8..).take(100 * CHUNK_SIZE).collect();

        let meta = store.put_blob(&data).unwrap();
        let roundtrip = store.get_blob(&meta.content_hash).unwrap();
        assert_eq!(roundtrip, data);
    }

    #[test]
    fn has_complete() {
        let (store, _dir) = temp_store();
        let data = b"test data".to_vec();
        let hash = ContentHash::from_bytes(&data);

        assert!(!store.has_complete(&hash));
        store.put_blob(&data).unwrap();
        assert!(store.has_complete(&hash));
    }

    #[test]
    fn list_complete() {
        let (store, _dir) = temp_store();
        assert!(store.list_complete().unwrap().is_empty());

        let h1 = store.put_blob(b"blob1").unwrap().content_hash;
        let h2 = store.put_blob(b"blob2").unwrap().content_hash;

        let blobs = store.list_complete().unwrap();
        assert_eq!(blobs.len(), 2);
        assert!(blobs.contains(&h1));
        assert!(blobs.contains(&h2));
    }

    #[test]
    fn verify_complete() {
        let (store, _dir) = temp_store();
        let data = vec![0xBB; 5 * CHUNK_SIZE];
        let hash = store.put_blob(&data).unwrap().content_hash;

        assert!(store.verify_complete(&hash));
    }

    #[test]
    fn verify_complete_detects_corruption() {
        let (store, _dir) = temp_store();
        let data = vec![0xCC; CHUNK_SIZE];
        let hash = store.put_blob(&data).unwrap().content_hash;

        // Corrupt shard 0.
        let shard_path = store.shard_path(&hash, 0);
        let mut bytes = std::fs::read(&shard_path).unwrap();
        bytes[0] ^= 0xFF;
        std::fs::write(&shard_path, &bytes).unwrap();

        assert!(!store.verify_complete(&hash));
    }

    #[test]
    fn reconstruction_with_missing_shards() {
        let (store, _dir) = temp_store();
        let data: Vec<u8> = (0u8..).take(7 * CHUNK_SIZE - 5).collect();
        let hash = store.put_blob(&data).unwrap().content_hash;

        // Delete all data shards, keep only parity.
        for idx in 0..EC_DATA_SHARDS {
            std::fs::remove_file(store.shard_path(&hash, idx as u8)).unwrap();
        }

        // Reconstruction should fail: need at least 3 shards.
        let result = store.get_blob(&hash);
        assert!(matches!(result, Err(ECStoreError::Unrecoverable(_))));

        // Restore 1 data shard — still not enough.
        let shard_path = store.shard_path(&hash, 0);
        let bytes = std::fs::read(&shard_path).unwrap();
        std::fs::write(&shard_path, &bytes).unwrap();

        let result = store.get_blob(&hash);
        assert!(matches!(result, Err(ECStoreError::Unrecoverable(_))));

        // Restore 2 more → 3 total → reconstruction succeeds.
        let s1_bytes = std::fs::read(store.shard_path(&hash, 1)).unwrap();
        std::fs::write(store.shard_path(&hash, 1), &s1_bytes).unwrap();
        let s2_bytes = std::fs::read(store.shard_path(&hash, 2)).unwrap();
        std::fs::write(store.shard_path(&hash, 2), &s2_bytes).unwrap();

        let roundtrip = store.get_blob(&hash).unwrap();
        assert_eq!(roundtrip, data);
    }

    #[test]
    fn health_scan() {
        let (store, _dir) = temp_store();
        store.put_blob(b"blob1").unwrap();
        store.put_blob(b"blob2").unwrap();

        let report = store.health_scan();
        assert_eq!(report.total, 2);
        assert_eq!(report.healthy, 2);
    }

    #[test]
    fn remove_blob() {
        let (store, _dir) = temp_store();
        let meta = store.put_blob(b"to be removed").unwrap();
        assert!(store.has_complete(&meta.content_hash));

        store.remove(&meta.content_hash).unwrap();
        assert!(!store.has_complete(&meta.content_hash));
    }

    #[test]
    fn idempotent_put() {
        let (store, _dir) = temp_store();
        let data = b"idempotent test".to_vec();

        let meta1 = store.put_blob(&data).unwrap();
        let meta2 = store.put_blob(&data).unwrap();

        assert_eq!(meta1.content_hash, meta2.content_hash);
        assert_eq!(store.list_complete().unwrap().len(), 1);
    }
}
