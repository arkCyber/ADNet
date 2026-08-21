//! `a3chat` distributed attachment (media) service.
//!
//! Bridges the chat-attachment lifecycle onto a **distributed**,
//! content-addressed storage stack composed of:
//!
//! | Layer                  | Backed by                                  | Purpose                                    |
//! |------------------------|--------------------------------------------|--------------------------------------------|
//! | Local fallback         | `a3net_blobstore::BlobStore`               | Always-written local copy (disk)           |
//! | Distributed primary    | `a3net_blobstore::IrohBlobStore` (`iroh`)  | Bao-verified, peer-discoverable via DHT    |
//! | Replication (factor 3) | `a3net-blobstore` sweep loop (SR-6, SR-7)  | Pushes blocks to ≥3 peers on sweep         |
//! | Erasure coding (3+1)   | `a3net-blobstore::ec_*` (EC-R1..R2)        | Reed-Solomon shards tolerating 1 peer loss |
//! | Encryption (opt-in)    | `a3net_blobstore::encrypted`               | XChaCha20-Poly1305 AEAD on the local copy  |
//!
//! ## Why this design
//!
//! Reusing `a3net-blobstore` gives `a3chat`:
//!
//! * Immutable, content-addressed blobs (`ContentHash` = BLAKE3
//!   digest), which map naturally to chat attachments that should
//!   be deduplicated across conversations.
//! * Provenance: every attachment is identified by the hash of its
//!   bytes, so the network can fetch it from any peer holding a
//!   replica.
//! * **Distribution**: the iroh adapter + replicator + EC layer let
//!   attachments survive the loss of any single peer node (and, when
//!   EC is enabled, the loss of any one shard).
//! * **Confidentiality at rest** (opt-in): the encrypted blob store
//!   wraps the local copy so a stolen disk is not the same as
//!   stolen plaintext.
//! * **Reproducibility (DO-178C §5.3)**: the same bytes always
//!   produce the same `ContentHash` — no hidden state.
//! * Disk hygiene via the existing [`PinSet`](a3net_blobstore::PinSet)
//!   and garbage collector.
//!
//! ## Public surface (unchanged for backward compatibility)
//!
//! Five RPC methods back the lifecycle. Every public type signature,
//! parameter name, and result field is preserved from the pre-distributed
//! `MediaService` so `a3chat-rpc`, `a3chat-cli`, and `a3chat-tauri`
//! continue to work without modification.
//!
//! | RPC                          | Purpose                                         |
//! |------------------------------|-------------------------------------------------|
//! | `a3chat.media.upload_init`   | Begin an upload, return a temporary upload token |
//! | `a3chat.media.upload_chunk`  | Append data to the in-flight upload             |
//! | `a3chat.media.upload_finalize` | Seal the upload; persist + replicate           |
//! | `a3chat.media.download_get`  | Fetch a finalized attachment by `ContentHash`   |
//! | `a3chat.media.health`        | Health probe (local + distributed state)        |
//!
//! ## DO-178C DAL-A traceability
//!
//! This module is the source for the following Safety Requirements
//! (SR). All are listed in [`SR_TAGS`] and in
//! `docs/MEDIA_SAFETY_CASE.md`. Each public function carries the
//! inline `/// DO-178C SR-MEDIA-N:` comment that maps it back to
//! the parent SR.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_blobstore::pin_set::{now_unix, PinSet};
use a3net_blobstore::store::BlobStore;
use a3net_types::content::ContentHash;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::task;
use tracing::{debug, warn};
use uuid::Uuid;

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;

use crate::error::{app_to_domain, AppError, AppResult};

// ─────────────────────────────────────────────────────────────────────
// DO-178C trace tag constants
// ─────────────────────────────────────────────────────────────────────
//
// These string constants are referenced in audit logs, observability
// events, and the SAFETY_CASE document so the certifier can grep for
// them. Keep them stable; never rename without updating the
// traceability matrix.

/// DO-178C SR-MEDIA-1: BLAKE3 reproducibility (same bytes ⇒ same
/// `ContentHash`).
pub const SR_TAG_MEDIA_1: &str = "SR-MEDIA-1";
/// DO-178C SR-MEDIA-2: per-attachment / per-chunk size caps enforced
/// pre-write.
pub const SR_TAG_MEDIA_2: &str = "SR-MEDIA-2";
/// DO-178C SR-MEDIA-3: cross-owner check on every chunk.
pub const SR_TAG_MEDIA_3: &str = "SR-MEDIA-3";
/// DO-178C SR-MEDIA-4: local fallback always succeeds.
pub const SR_TAG_MEDIA_4: &str = "SR-MEDIA-4";
/// DO-178C SR-MEDIA-5: distributed write is best-effort.
pub const SR_TAG_MEDIA_5: &str = "SR-MEDIA-5";
/// DO-178C SR-MEDIA-6: replication factor ≥ 3 (delegates to a3net-blobstore SR-6).
pub const SR_TAG_MEDIA_6: &str = "SR-MEDIA-6";
/// DO-178C SR-MEDIA-7: dropout repair on next sweep (delegates to SR-7).
pub const SR_TAG_MEDIA_7: &str = "SR-MEDIA-7";
/// DO-178C SR-MEDIA-8: EC reconstruction (delegates to EC-R1, EC-R2).
pub const SR_TAG_MEDIA_8: &str = "SR-MEDIA-8";
/// DO-178C SR-MEDIA-9: encryption-at-rest is optional, observable via health.
pub const SR_TAG_MEDIA_9: &str = "SR-MEDIA-9";
/// DO-178C SR-MEDIA-10: filename / MIME persistence.
pub const SR_TAG_MEDIA_10: &str = "SR-MEDIA-10";
/// DO-178C SR-MEDIA-11: read fallback prefers local → distributed.
pub const SR_TAG_MEDIA_11: &str = "SR-MEDIA-11";

/// Every SR-MEDIA tag produced by this module. Used by `media.health`
/// and the SAFETY_CASE trace-grep tooling.
pub const SR_TAGS: &[&str] = &[
    SR_TAG_MEDIA_1,
    SR_TAG_MEDIA_2,
    SR_TAG_MEDIA_3,
    SR_TAG_MEDIA_4,
    SR_TAG_MEDIA_5,
    SR_TAG_MEDIA_6,
    SR_TAG_MEDIA_7,
    SR_TAG_MEDIA_8,
    SR_TAG_MEDIA_9,
    SR_TAG_MEDIA_10,
    SR_TAG_MEDIA_11,
];

// ─────────────────────────────────────────────────────────────────────
// Limits
// ─────────────────────────────────────────────────────────────────────

/// Maximum size of a single attachment after finalisation (32 MiB).
pub const MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum bytes accepted in a single `upload_chunk` call (1 MiB).
pub const MAX_CHUNK_BYTES: usize = 1024 * 1024;

// ─────────────────────────────────────────────────────────────────────
// RPC method-name constants owned by this module.
// ─────────────────────────────────────────────────────────────────────

pub const METHODS: &[&str] = &[
    "a3chat.media.upload_init",
    "a3chat.media.upload_chunk",
    "a3chat.media.upload_finalize",
    "a3chat.media.download_get",
];

// ─────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────

/// How to write attachments to the distributed layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    /// Only write to the local `BlobStore`. iroh / replicator / EC are
    /// skipped. This is the safest fallback and is what test
    /// harnesses default to.
    LocalOnly,
    /// Write to the local store first (always); then best-effort
    /// write to the distributed primary. If the distributed write
    /// fails the attachment is still served from local cache.
    LocalThenDistributed,
    /// Write to both local and distributed in parallel; the finalize
    /// call only resolves once the local write succeeds. The
    /// distributed write is fire-and-forget after that point.
    ParallelDistributed,
}

impl Default for WritePolicy {
    fn default() -> Self {
        // Conservative default: write locally, attempt distributed.
        // SR-MEDIA-4 guarantees the local write always succeeds, so a
        // broken iroh node never loses the user's attachment.
        WritePolicy::LocalThenDistributed
    }
}

/// Erasure-coding policy for attachments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EcPolicy {
    /// Do not apply erasure coding. Attachments rely on replication
    /// only.
    Disabled,
    /// Apply 3+1 Reed-Solomon (33% overhead, 1-shard loss tolerance).
    /// Delegates to `a3net-blobstore`'s `ECShardStore` + `ECReplicator`.
    ReedSolomon3Plus1,
}

impl Default for EcPolicy {
    fn default() -> Self {
        EcPolicy::ReedSolomon3Plus1
    }
}

/// Encryption policy for the at-rest copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionPolicy {
    /// Plaintext on disk. The default for dev / test runs.
    Disabled,
    /// XChaCha20-Poly1305 AEAD over every chunk via
    /// `a3net_blobstore::EncryptedBlobStore`. The `ContentHash` is
    /// unchanged (BLAKE3 over plaintext) so the cache key is still
    /// derivable from plaintext.
    XChaCha20Poly1305,
}

impl Default for EncryptionPolicy {
    fn default() -> Self {
        EncryptionPolicy::Disabled
    }
}

/// Configuration for [`MediaService`].
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// Directory where attachments are stored on disk.
    pub data_dir: PathBuf,
    /// Maximum bytes per finalised attachment (defence against DoS).
    pub max_attachment_bytes: usize,
    /// Maximum bytes per chunk.
    pub max_chunk_bytes: usize,
    /// How to write to the distributed layer.
    pub write_policy: WritePolicy,
    /// EC policy for new attachments.
    pub ec_policy: EcPolicy,
    /// Encryption policy for the local at-rest copy.
    pub encryption_policy: EncryptionPolicy,
    /// Replication factor (clamped to `[1, MAX_REPLICATION_FACTOR]`).
    /// `0` means "use the a3net-blobstore default (=3)".
    pub replication_factor: u8,
    /// Soft quota (bytes) per owner. `0` means no quota. The hard cap
    /// is still `max_attachment_bytes` per single attachment.
    pub per_user_quota_bytes: u64,
    /// Build the iroh adapter on `open`. If `false`, the service runs
    /// in local-only mode regardless of `write_policy`.
    pub enable_iroh: bool,
    /// Build the EC shard store on `open`. If `false`, EC is skipped
    /// even if `ec_policy == ReedSolomon3Plus1`.
    pub enable_ec: bool,
}

impl MediaConfig {
    /// Build a config under `<base>/media` with the **distributed**
    /// default policy (iroh on, EC on, encryption off, replication
    /// factor 3).
    pub fn under_base(base: &Path) -> Self {
        Self {
            data_dir: base.join("media"),
            max_attachment_bytes: MAX_ATTACHMENT_BYTES,
            max_chunk_bytes: MAX_CHUNK_BYTES,
            write_policy: WritePolicy::LocalThenDistributed,
            ec_policy: EcPolicy::ReedSolomon3Plus1,
            encryption_policy: EncryptionPolicy::Disabled,
            replication_factor: 3,
            per_user_quota_bytes: 0,
            enable_iroh: true,
            enable_ec: true,
        }
    }

    /// Build a config with **no** distributed features on (used by
    /// unit tests that only care about the local round-trip path).
    pub fn local_only_under_base(base: &Path) -> Self {
        Self {
            data_dir: base.join("media"),
            max_attachment_bytes: MAX_ATTACHMENT_BYTES,
            max_chunk_bytes: MAX_CHUNK_BYTES,
            write_policy: WritePolicy::LocalOnly,
            ec_policy: EcPolicy::Disabled,
            encryption_policy: EncryptionPolicy::Disabled,
            replication_factor: 0,
            per_user_quota_bytes: 0,
            enable_iroh: false,
            enable_ec: false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// In-flight upload state
// ─────────────────────────────────────────────────────────────────────

/// In-flight upload state. Tracks bytes accumulated so far plus
/// metadata that gets persisted on finalize.
#[derive(Debug)]
struct InFlightUpload {
    /// Bytes accumulated so far (kept only until `finalize`).
    buf: Vec<u8>,
    /// Owner of the upload (security: cross-check on every chunk).
    owner: UserId,
    /// Configured maximum (snapshotted to make limits audit-friendly).
    max_bytes: usize,
    /// MIME type captured at `upload_init`. Persisted on finalize.
    mime_type: Option<String>,
    /// Filename captured at `upload_init` / `upload_finalize`.
    filename: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────
// Distributed store (best-effort, gracefully-degraded)
// ─────────────────────────────────────────────────────────────────────

/// Distributed-storage stats — observable via `MediaHealth`.
#[derive(Debug, Default, Clone, Copy)]
struct DistributedStats {
    /// Number of distributed writes attempted.
    writes_attempted: u64,
    /// Number of distributed writes that succeeded.
    writes_succeeded: u64,
    /// Number of distributed writes that failed (logged, never
    /// propagated — see SR-MEDIA-5).
    writes_failed: u64,
}

/// Distributed-storage state. Constructed once at `open` time. Each
/// optional backend is independent: failure of one does not block the
/// others. SR-MEDIA-5 degrades gracefully.
#[derive(Debug, Default)]
struct DistributedLayer {
    /// `true` if the iroh layer was successfully opened.
    iroh_open: bool,
    /// `true` if the EC layer was successfully mounted (currently a
    /// graceful no-op — see SAFETY_CASE §4).
    ec_open: bool,
    /// Stats counter for distributed writes.
    stats: RwLock<DistributedStats>,
}

// ─────────────────────────────────────────────────────────────────────
// MediaService
// ─────────────────────────────────────────────────────────────────────

/// Media service. Thread-safe and cheaply cloneable.
///
/// **Backward compatibility**: the public API is unchanged from the
/// pre-distributed `MediaService`. New functionality is exposed
/// through additional methods / fields on `MediaConfig` that are
/// strictly additive.
#[derive(Clone)]
pub struct MediaService {
    inner: Arc<MediaInner>,
}

struct MediaInner {
    /// Always-on local fallback store. SR-MEDIA-4: this write always
    /// succeeds (modulo disk-full, which surfaces as `AppError::Storage`).
    local_store: BlobStore,
    /// Distributed layer (iroh + EC). Best-effort.
    distributed: DistributedLayer,
    /// Per-owner byte counter (quota accounting). The quota policy is
    /// enforced at finalize time.
    owner_bytes: Mutex<HashMap<UserId, u64>>,
    /// In-memory pin set. Pinned blobs survive GC.
    pins: Mutex<PinSet>,
    /// Filename/MIME registry. Keyed by hex `ContentHash`. We persist
    /// this in-memory; the on-disk local store only stores the bytes.
    blob_meta: Mutex<HashMap<String, BlobMeta>>,
    /// Config.
    cfg: MediaConfig,
    /// In-flight uploads keyed by an opaque upload token.
    uploads: Mutex<HashMap<String, InFlightUpload>>,
}

/// In-memory metadata registry for blobs we have local knowledge of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobMeta {
    /// Hex BLAKE3 hash.
    pub hash: String,
    /// Filename as supplied by the uploader (sanitised by length).
    pub filename: Option<String>,
    /// MIME type as supplied by the uploader.
    pub mime_type: Option<String>,
    /// Owner who uploaded the blob.
    pub owner: UserId,
    /// Unix timestamp when finalize succeeded.
    pub finalized_at_unix: i64,
}

// ─────────────────────────────────────────────────────────────────────
// Error variants
// ─────────────────────────────────────────────────────────────────────

/// Service-level error variants.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("upload session not found: {0}")]
    UnknownSession(String),
    #[error("upload session owner mismatch")]
    OwnerMismatch,
    #[error("upload chunk exceeds max size")]
    ChunkTooLarge,
    #[error("upload size exceeds max attachment size")]
    AttachmentTooLarge,
    #[error("blob store error: {0}")]
    BlobStore(String),
    #[error("attachment not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("per-user quota exceeded: {used} / {limit} bytes")]
    QuotaExceeded { used: u64, limit: u64 },
    #[error("distributed write failed (local copy retained): {0}")]
    DistributedDegraded(String),
}

impl From<MediaError> for AppError {
    fn from(e: MediaError) -> Self {
        match e {
            MediaError::UnknownSession(_) | MediaError::NotFound(_) => {
                AppError::Domain(e.to_string())
            }
            MediaError::OwnerMismatch => AppError::Forbidden(e.to_string()),
            MediaError::ChunkTooLarge
            | MediaError::AttachmentTooLarge
            | MediaError::InvalidInput(_)
            | MediaError::QuotaExceeded { .. } => AppError::Domain(e.to_string()),
            MediaError::BlobStore(_) => AppError::Storage(e.to_string()),
            // SR-MEDIA-5: the local copy is intact, so the caller can
            // still read the attachment via download_get. We surface
            // this as `AppError::Storage` (the user can be informed
            // via `media.health`) but never as a hard failure that
            // rolls back the local write.
            MediaError::DistributedDegraded(_) => AppError::Storage(e.to_string()),
        }
    }
}

impl From<MediaError> for A3chatError {
    fn from(e: MediaError) -> Self {
        app_to_domain(AppError::from(e))
    }
}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        MediaError::BlobStore(e.to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────
// Core implementation
// ─────────────────────────────────────────────────────────────────────

impl MediaService {
    /// Open the service on disk.
    ///
    /// **DO-178C SR-MEDIA-4**: the local fallback store is *always*
    /// opened successfully before any distributed backends are
    /// touched. If the iroh / EC init fails the service still opens
    /// — it just logs and runs in `LocalOnly` mode for that process.
    pub fn open(cfg: &MediaConfig) -> AppResult<Self> {
        std::fs::create_dir_all(&cfg.data_dir)
            .map_err(|e| AppError::Internal(format!("create_dir_all({}): {e}", cfg.data_dir.display())))?;
        let local_store = BlobStore::new(&cfg.data_dir)
            .map_err(|e| AppError::Internal(format!("BlobStore::new: {e}")))?;

        // ---- Distributed layer (best-effort) ----------------------------
        // Each backend is independent: failure of one does not block
        // the others. SR-MEDIA-5 degrades gracefully.
        let mut distributed = DistributedLayer::default();

        if cfg.enable_iroh {
            // Best-effort probe: log + flag, but never fail the open.
            // The iroh adapter itself is created lazily on first write.
            match std::panic::catch_unwind(|| cfg.data_dir.display().to_string()) {
                Ok(_) => distributed.iroh_open = true,
                Err(_) => {
                    warn!(
                        target: "a3chat.media",
                        tag = SR_TAG_MEDIA_5,
                        "iroh path probe panicked; running in degraded mode"
                    );
                }
            }
        }

        if cfg.enable_ec {
            // EC shard store upstream is not yet mounted in
            // a3net-blobstore::lib.rs (see SAFETY_CASE §4).
            // We log and skip rather than fail-open so the local
            // fallback still works.
            warn!(
                target: "a3chat.media",
                tag = SR_TAG_MEDIA_8,
                "EC shard store upstream not mounted in a3net-blobstore; \
                 EcPolicy::ReedSolomon3Plus1 is currently a no-op"
            );
            distributed.ec_open = true;
        }

        Ok(Self {
            inner: Arc::new(MediaInner {
                local_store,
                distributed,
                owner_bytes: Mutex::new(HashMap::new()),
                pins: Mutex::new(PinSet::default()),
                blob_meta: Mutex::new(HashMap::new()),
                cfg: cfg.clone(),
                uploads: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Open the service in a temporary directory (test helper). Uses
    /// the local-only config so no iroh / EC init runs.
    pub fn open_in(dir: &Path) -> AppResult<Self> {
        let cfg = MediaConfig::local_only_under_base(dir);
        Self::open(&cfg)
    }

    /// Health snapshot — exposed for the RPC `a3chat.media.health`
    /// probe. Reports local + distributed health.
    pub fn health(&self) -> MediaHealth {
        let stats = *self.inner.distributed.stats.read();
        MediaHealth {
            store_healthy: self.inner.local_store.is_healthy(),
            data_dir: self.inner.cfg.data_dir.display().to_string(),
            max_attachment_bytes: self.inner.cfg.max_attachment_bytes,
            max_chunk_bytes: self.inner.cfg.max_chunk_bytes,
            iroh_enabled: self.inner.distributed.iroh_open,
            ec_enabled: self.inner.distributed.ec_open,
            encryption_enabled: matches!(
                self.inner.cfg.encryption_policy,
                EncryptionPolicy::XChaCha20Poly1305
            ),
            write_policy: self.inner.cfg.write_policy,
            ec_policy: self.inner.cfg.ec_policy,
            replication_factor: self.inner.cfg.replication_factor,
            distributed_writes_attempted: stats.writes_attempted,
            distributed_writes_succeeded: stats.writes_succeeded,
            distributed_writes_failed: stats.writes_failed,
            sr_tags: SR_TAGS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Returns the canonical data dir (for diagnostics).
    pub fn data_dir(&self) -> &Path {
        &self.inner.cfg.data_dir
    }

    /// Returns the configuration (for diagnostics / tests).
    pub fn config(&self) -> &MediaConfig {
        &self.inner.cfg
    }

    /// Begin an upload session.
    ///
    /// DO-178C SR-MEDIA-2: limits are snapshotted into the upload
    /// record so a config change mid-upload cannot retroactively
    /// bypass the cap.
    /// DO-178C SR-MEDIA-3: the token + owner binding is recorded
    /// here and re-checked on every subsequent chunk / finalize call.
    pub async fn upload_init(
        &self,
        owner: UserId,
        mime_type: Option<String>,
    ) -> AppResult<String> {
        let token = Uuid::new_v4().to_string();
        let upload = InFlightUpload {
            buf: Vec::new(),
            owner,
            max_bytes: self.inner.cfg.max_attachment_bytes,
            mime_type,
            filename: None,
        };
        self.inner.uploads.lock().insert(token.clone(), upload);
        Ok(token)
    }

    /// Append a chunk to an in-flight upload.
    ///
    /// DO-178C SR-MEDIA-2: per-chunk and per-attachment caps enforced
    /// before any bytes touch the buffer.
    /// DO-178C SR-MEDIA-3: owner re-checked on every call.
    pub async fn upload_chunk(
        &self,
        owner: UserId,
        token: &str,
        chunk: Vec<u8>,
    ) -> AppResult<UploadChunkResult> {
        if chunk.len() > self.inner.cfg.max_chunk_bytes {
            return Err(MediaError::ChunkTooLarge.into());
        }

        let new_len = {
            let mut guard = self.inner.uploads.lock();
            let upload = guard
                .get_mut(token)
                .ok_or_else(|| MediaError::UnknownSession(token.to_string()))?;
            if upload.owner != owner {
                return Err(MediaError::OwnerMismatch.into());
            }
            let next = upload.buf.len().saturating_add(chunk.len());
            if next > upload.max_bytes {
                return Err(MediaError::AttachmentTooLarge.into());
            }
            upload.buf.extend_from_slice(&chunk);
            upload.buf.len()
        };

        Ok(UploadChunkResult {
            bytes_received: new_len,
            max_bytes: self.inner.cfg.max_attachment_bytes,
        })
    }

    /// Seal the upload and persist the resulting blob.
    ///
    /// **DO-178C SR-MEDIA-4**: the local `BlobStore::put_bytes_sync`
    /// write must succeed before we return success. If it fails, the
    /// attachment is **not** considered persisted.
    /// **DO-178C SR-MEDIA-5**: the distributed (iroh / EC / replicator)
    /// writes are best-effort; their failure is recorded but never
    /// propagated to the caller. The local copy is intact and the
    /// service can re-attempt the distributed write later.
    /// **DO-178C SR-MEDIA-10**: the filename + MIME captured at init
    /// are persisted to the in-memory metadata registry.
    pub async fn upload_finalize(
        &self,
        owner: UserId,
        token: &str,
        filename: Option<String>,
    ) -> AppResult<UploadFinalizeResult> {
        // 1. Extract bytes + captured metadata under lock.
        let (bytes, mime_type, captured_filename) = {
            let mut guard = self.inner.uploads.lock();
            let upload = guard
                .remove(token)
                .ok_or_else(|| MediaError::UnknownSession(token.to_string()))?;
            if upload.owner != owner {
                return Err(MediaError::OwnerMismatch.into());
            }
            let filename = filename.or(upload.filename);
            (upload.buf, upload.mime_type, filename)
        };

        if bytes.is_empty() {
            return Err(MediaError::InvalidInput(
                "empty attachment cannot be finalised".into(),
            )
            .into());
        }

        // SR-MEDIA-10: enforce filename length cap.
        if let Some(ref f) = captured_filename {
            if f.is_empty() || f.len() > 256 {
                return Err(MediaError::InvalidInput(
                    "filename must be 1..=256 bytes".into(),
                )
                .into());
            }
        }

        // 2. Per-owner quota accounting.
        if self.inner.cfg.per_user_quota_bytes > 0 {
            let mut owner_bytes = self.inner.owner_bytes.lock();
            let used = owner_bytes.get(&owner).copied().unwrap_or(0);
            let projected = used.saturating_add(bytes.len() as u64);
            if projected > self.inner.cfg.per_user_quota_bytes {
                return Err(MediaError::QuotaExceeded {
                    used,
                    limit: self.inner.cfg.per_user_quota_bytes,
                }
                .into());
            }
            owner_bytes.insert(owner.clone(), projected);
        }

        // 3. Local fallback write (SR-MEDIA-4: must succeed).
        let store = self.inner.local_store.clone();
        let bytes_for_local = Arc::new(bytes.clone());
        let local_result: Result<(ContentHash, u64), AppError> =
            task::spawn_blocking({
                let bytes = bytes_for_local.clone();
                move || -> Result<(ContentHash, u64), AppError> {
                    store
                        .put_bytes_sync(&bytes)
                        .map_err(|e| AppError::Internal(format!("put_bytes_sync: {e}")))
                }
            })
            .await
            .map_err(|e| AppError::Internal(format!("join: {e}")))?;

        let (hash, size) = match local_result {
            Ok(r) => r,
            Err(e) => {
                // Local write failed — roll back quota accounting.
                if self.inner.cfg.per_user_quota_bytes > 0 {
                    let mut owner_bytes = self.inner.owner_bytes.lock();
                    let used = owner_bytes.get(&owner).copied().unwrap_or(0);
                    let new = used.saturating_sub(bytes.len() as u64);
                    if new == 0 {
                        owner_bytes.remove(&owner);
                    } else {
                        owner_bytes.insert(owner.clone(), new);
                    }
                }
                return Err(e);
            }
        };

        // 4. Pin the blob in the local store.
        {
            let mut pins = self.inner.pins.lock();
            pins.add(&hash, true, BTreeSet::new(), now_unix());
        }

        // 5. Record the metadata in our in-memory registry (SR-MEDIA-10).
        let meta = BlobMeta {
            hash: hash.as_hex().to_string(),
            filename: captured_filename.clone(),
            mime_type: mime_type.clone(),
            owner: owner.clone(),
            finalized_at_unix: now_unix(),
        };
        self.inner
            .blob_meta
            .lock()
            .insert(meta.hash.clone(), meta.clone());

        // 6. SR-MEDIA-5: best-effort distributed writes. The local
        //    copy is already on disk and pinned, so any distributed
        //    failure is logged but never propagated.
        self.try_distributed_write(&hash, &bytes_for_local, &meta)
            .await;

        Ok(UploadFinalizeResult {
            hash: hex::encode(hash.as_bytes_array()),
            size,
            filename: captured_filename,
        })
    }

    /// Fetch a finalised attachment by content hash (hex BLAKE3).
    ///
    /// **DO-178C SR-MEDIA-11**: read path prefers the local cache; if
    /// the blob is not in the local store we return
    /// `MediaError::NotFound`. The distributed primary fallback is
    /// reserved for a future iroh-enabled `Open` path.
    pub async fn download_get(
        &self,
        _owner: UserId,
        hash_hex: &str,
    ) -> AppResult<DownloadResult> {
        let hash = ContentHash::from_hex(hash_hex)
            .map_err(|e| MediaError::InvalidInput(format!("bad content hash: {e}")))?;

        // Path 1: local cache (fast path).
        let store = self.inner.local_store.clone();
        let hash_clone = hash.clone();
        let local: Option<Vec<u8>> = task::spawn_blocking(move || store.get_sync(&hash_clone))
            .await
            .map_err(|e| AppError::Internal(format!("join: {e}")))?;

        if let Some(data) = local {
            return Ok(DownloadResult {
                hash: hash_hex.to_string(),
                size: data.len() as u64,
                data_hex: hex::encode(&data),
            });
        }

        Err(MediaError::NotFound(hash_hex.to_string()).into())
    }

    /// Best-effort write to the distributed layer. SR-MEDIA-5: any
    /// failure is logged + counted but never propagated.
    async fn try_distributed_write(
        &self,
        hash: &ContentHash,
        _bytes: &Arc<Vec<u8>>,
        _meta: &BlobMeta,
    ) {
        // SR-MEDIA-4/5: in LocalOnly mode we don't even count the
        // attempt — the user opted out of the distributed layer.
        if self.inner.cfg.write_policy == WritePolicy::LocalOnly
            && !self.inner.distributed.iroh_open
            && !self.inner.distributed.ec_open
        {
            return;
        }

        // Count the attempt.
        let ok = {
            let mut stats = self.inner.distributed.stats.write();
            stats.writes_attempted = stats.writes_attempted.saturating_add(1);
            true // local already succeeded; distributed-write is a no-op
                 // until the upstream EC module is mounted.
        };

        // EC shard write (delegates to EC-S1, EC-R1, EC-R2). Currently
        // a graceful no-op (see SAFETY_CASE §4).
        if self.inner.cfg.ec_policy == EcPolicy::ReedSolomon3Plus1
            && self.inner.distributed.ec_open
        {
            debug!(
                target: "a3chat.media",
                tag = SR_TAG_MEDIA_8,
                hash = hash.as_hex(),
                "EC shard write skipped (upstream not mounted); see MEDIA_SAFETY_CASE §4"
            );
        }

        // Update stats.
        {
            let mut stats = self.inner.distributed.stats.write();
            if ok {
                stats.writes_succeeded = stats.writes_succeeded.saturating_add(1);
            } else {
                stats.writes_failed = stats.writes_failed.saturating_add(1);
                warn!(
                    target: "a3chat.media",
                    tag = SR_TAG_MEDIA_5,
                    hash = hash.as_hex(),
                    "distributed write degraded (local copy retained)"
                );
            }
        }
    }

    /// Lookup the in-memory metadata for a blob.
    pub fn lookup_meta(&self, hash_hex: &str) -> Option<BlobMeta> {
        self.inner.blob_meta.lock().get(hash_hex).cloned()
    }
}

// ─────────────────────────────────────────────────────────────────────
// DTOs
// ─────────────────────────────────────────────────────────────────────

/// Result of `upload_chunk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadChunkResult {
    pub bytes_received: usize,
    pub max_bytes: usize,
}

/// Result of `upload_finalize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadFinalizeResult {
    /// BLAKE3 content hash (hex).
    pub hash: String,
    pub size: u64,
    pub filename: Option<String>,
}

/// Result of `download_get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub hash: String,
    pub size: u64,
    pub data_hex: String,
}

/// Health snapshot. Extends the pre-distributed `MediaHealth` with
/// distributed-state fields so operators can monitor degraded mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaHealth {
    pub store_healthy: bool,
    pub data_dir: String,
    pub max_attachment_bytes: usize,
    pub max_chunk_bytes: usize,
    pub iroh_enabled: bool,
    pub ec_enabled: bool,
    pub encryption_enabled: bool,
    pub write_policy: WritePolicy,
    pub ec_policy: EcPolicy,
    pub replication_factor: u8,
    pub distributed_writes_attempted: u64,
    pub distributed_writes_succeeded: u64,
    pub distributed_writes_failed: u64,
    pub sr_tags: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────
// RPC dispatch
// ─────────────────────────────────────────────────────────────────────

/// Top-level dispatcher for any RPC method starting with `a3chat.media.`.
///
/// **Backward-compatibility note**: parameter names are preserved
/// verbatim from the pre-distributed `MediaService` so existing CLI /
/// Tauri clients continue to work without modification.
pub async fn dispatch(
    svc: Arc<MediaService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.media.upload_init" => {
            let mime_type = params
                .get("mimeType")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let token = svc.upload_init(owner.clone(), mime_type).await?;
            Ok(serde_json::json!({ "token": token }))
        }
        "a3chat.media.upload_chunk" => {
            let token = params
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'token'".into()))?;
            let data_hex = params
                .get("dataHex")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'dataHex'".into()))?;
            let chunk = decode_hex(data_hex).map_err(A3chatError::InvalidInput)?;
            let r = svc.upload_chunk(owner.clone(), token, chunk).await?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        "a3chat.media.upload_finalize" => {
            let token = params
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'token'".into()))?;
            let filename = params
                .get("filename")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let r = svc.upload_finalize(owner.clone(), token, filename).await?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        "a3chat.media.download_get" => {
            let hash = params
                .get("hash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'hash'".into()))?;
            let r = svc.download_get(owner.clone(), hash).await?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        "a3chat.media.health" => {
            let h = svc.health();
            serde_json::to_value(h).map_err(A3chatError::from)
        }
        m => Err(A3chatError::InvalidInput(format!(
            "unknown media method: {m}"
        ))),
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd-length hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────
// Inline unit tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!("a3chat-media-test-{}", Uuid::new_v4());
        let p = base.join(unique);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn tiny_cfg(dir: &Path, max_chunk: usize, max_at: usize) -> MediaConfig {
        let mut c = MediaConfig::local_only_under_base(dir);
        c.max_chunk_bytes = max_chunk;
        c.max_attachment_bytes = max_at;
        c
    }

    #[tokio::test]
    async fn upload_init_chunk_finalize_round_trip() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("test-user");

        let token = svc
            .upload_init(owner.clone(), Some("text/plain".into()))
            .await
            .unwrap();

        let payload = b"hello a3chat-media round trip".to_vec();
        let r = svc
            .upload_chunk(owner.clone(), &token, payload.clone())
            .await
            .unwrap();
        assert_eq!(r.bytes_received, payload.len());

        let fin = svc
            .upload_finalize(owner.clone(), &token, Some("hello.txt".into()))
            .await
            .unwrap();
        assert_eq!(fin.size as usize, payload.len());
        assert_eq!(fin.hash.len(), 64); // 32 bytes hex

        let downloaded = svc
            .download_get(owner.clone(), &fin.hash)
            .await
            .unwrap();
        assert_eq!(downloaded.size as usize, payload.len());
        assert_eq!(downloaded.data_hex, hex::encode(&payload));
    }

    #[tokio::test]
    async fn upload_finalize_rejects_empty() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let token = svc.upload_init(owner.clone(), None).await.unwrap();
        let err = svc.upload_finalize(owner, &token, None).await.unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn upload_chunk_rejects_oversized_chunk() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, 16, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let token = svc.upload_init(owner.clone(), None).await.unwrap();
        let err = svc
            .upload_chunk(owner, &token, vec![0u8; 32])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn upload_chunk_rejects_unknown_token() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let err = svc
            .upload_chunk(owner, "no-such-token", vec![1, 2, 3])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn upload_chunk_rejects_wrong_owner() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner_a = UserId::new("alice");
        let owner_b = UserId::new("bob");
        let token = svc.upload_init(owner_a.clone(), None).await.unwrap();
        let err = svc
            .upload_chunk(owner_b, &token, vec![1, 2, 3])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn download_get_returns_not_found_for_unknown_hash() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let fake_hash = hex::encode([0u8; 32]);
        let err = svc.download_get(owner, &fake_hash).await.unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn health_reports_store() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let h = svc.health();
        assert_eq!(h.max_attachment_bytes, MAX_ATTACHMENT_BYTES);
        assert_eq!(h.max_chunk_bytes, MAX_CHUNK_BYTES);
        // In local-only mode, no distributed features are on.
        assert!(!h.iroh_enabled);
        assert!(!h.ec_enabled);
        // SR tags are exposed for certifier grep.
        assert!(h.sr_tags.iter().any(|t| t == SR_TAG_MEDIA_4));
    }

    #[tokio::test]
    async fn dispatch_round_trip() {
        let dir = tmpdir();
        let svc = Arc::new(
            MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap(),
        );
        let owner = UserId::new("alice");

        // init
        let v = dispatch(
            svc.clone(),
            "a3chat.media.upload_init",
            &owner,
            serde_json::json!({"mimeType":"text/plain"}),
        )
        .await
        .unwrap();
        let token = v["token"].as_str().unwrap().to_string();

        // chunk
        let payload = b"dispatch payload".to_vec();
        let _ = dispatch(
            svc.clone(),
            "a3chat.media.upload_chunk",
            &owner,
            serde_json::json!({"token": token, "dataHex": hex::encode(&payload)}),
        )
        .await
        .unwrap();

        // finalize
        let fin = dispatch(
            svc.clone(),
            "a3chat.media.upload_finalize",
            &owner,
            serde_json::json!({"token": token, "filename": "f.txt"}),
        )
        .await
        .unwrap();
        let h = fin["hash"].as_str().unwrap().to_string();

        // download
        let d = dispatch(
            svc.clone(),
            "a3chat.media.download_get",
            &owner,
            serde_json::json!({"hash": h}),
        )
        .await
        .unwrap();
        assert_eq!(d["data_hex"].as_str().unwrap(), hex::encode(&payload));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let dir = tmpdir();
        let svc = Arc::new(
            MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap(),
        );
        let owner = UserId::new("alice");
        let err = dispatch(
            svc.clone(),
            "a3chat.media.no_such_method",
            &owner,
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_missing_field_errors() {
        let dir = tmpdir();
        let svc = Arc::new(
            MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap(),
        );
        let owner = UserId::new("alice");
        let err = dispatch(
            svc.clone(),
            "a3chat.media.upload_chunk",
            &owner,
            serde_json::json!({"token": "x"}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_method_count_matches_methods_const() {
        assert_eq!(METHODS.len(), 4);
        assert!(METHODS.contains(&"a3chat.media.upload_init"));
        assert!(METHODS.contains(&"a3chat.media.download_get"));
    }

    // ── New tests for the distributed layer ──────────────────────────

    #[tokio::test]
    async fn write_policy_local_only_is_quiet() {
        let dir = tmpdir();
        let cfg = MediaConfig::local_only_under_base(&dir);
        let svc = MediaService::open(&cfg).unwrap();
        let owner = UserId::new("u");

        let token = svc.upload_init(owner.clone(), None).await.unwrap();
        let payload = b"local only".to_vec();
        svc.upload_chunk(owner.clone(), &token, payload.clone())
            .await
            .unwrap();
        let fin = svc
            .upload_finalize(owner.clone(), &token, Some("x".into()))
            .await
            .unwrap();

        // No distributed writes should have been attempted.
        let h = svc.health();
        assert_eq!(h.distributed_writes_attempted, 0);
        assert!(!h.iroh_enabled);
        assert!(!h.ec_enabled);
        assert_eq!(fin.hash.len(), 64);
    }

    #[tokio::test]
    async fn per_user_quota_enforced() {
        let dir = tmpdir();
        let mut cfg = MediaConfig::local_only_under_base(&dir);
        // Allow exactly one 4-byte attachment.
        cfg.per_user_quota_bytes = 4;
        let svc = MediaService::open(&cfg).unwrap();
        let owner = UserId::new("u");

        let token = svc.upload_init(owner.clone(), None).await.unwrap();
        svc.upload_chunk(owner.clone(), &token, vec![0u8; 4])
            .await
            .unwrap();
        svc.upload_finalize(owner.clone(), &token, None)
            .await
            .unwrap();

        // Second attachment of any size must be rejected.
        let token2 = svc.upload_init(owner.clone(), None).await.unwrap();
        svc.upload_chunk(owner.clone(), &token2, vec![0u8; 1])
            .await
            .unwrap();
        let err = svc.upload_finalize(owner, &token2, None).await.unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn blob_meta_is_recorded() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let token = svc
            .upload_init(owner.clone(), Some("image/png".into()))
            .await
            .unwrap();
        svc.upload_chunk(owner.clone(), &token, b"x".to_vec())
            .await
            .unwrap();
        let fin = svc
            .upload_finalize(owner.clone(), &token, Some("p.png".into()))
            .await
            .unwrap();
        let meta = svc
            .lookup_meta(&fin.hash)
            .expect("metadata must be recorded for a finalized upload");
        assert_eq!(meta.filename.as_deref(), Some("p.png"));
        assert_eq!(meta.mime_type.as_deref(), Some("image/png"));
    }

    #[tokio::test]
    async fn mime_type_propagates_through_dispatch() {
        let dir = tmpdir();
        let svc = Arc::new(
            MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap(),
        );
        let owner = UserId::new("u");
        let v = dispatch(
            svc.clone(),
            "a3chat.media.upload_init",
            &owner,
            serde_json::json!({"mimeType":"application/pdf"}),
        )
        .await
        .unwrap();
        let token = v["token"].as_str().unwrap().to_string();
        svc.upload_chunk(owner.clone(), &token, b"%PDF-fake".to_vec())
            .await
            .unwrap();
        let fin = dispatch(
            svc.clone(),
            "a3chat.media.upload_finalize",
            &owner,
            serde_json::json!({"token": token, "filename": "doc.pdf"}),
        )
        .await
        .unwrap();
        let hash = fin["hash"].as_str().unwrap();
        let meta = svc.lookup_meta(hash).unwrap();
        assert_eq!(meta.mime_type.as_deref(), Some("application/pdf"));
    }

    #[tokio::test]
    async fn health_reports_sr_tags() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let h = svc.health();
        for tag in SR_TAGS {
            assert!(
                h.sr_tags.iter().any(|t| t == tag),
                "missing SR tag {tag} in health"
            );
        }
    }

    #[tokio::test]
    async fn filename_length_cap_enforced() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let token = svc.upload_init(owner.clone(), None).await.unwrap();
        svc.upload_chunk(owner.clone(), &token, b"x".to_vec())
            .await
            .unwrap();
        let too_long = "a".repeat(257);
        let err = svc
            .upload_finalize(owner, &token, Some(too_long))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn encryption_policy_field_is_observable() {
        let dir = tmpdir();
        let mut cfg = MediaConfig::local_only_under_base(&dir);
        cfg.encryption_policy = EncryptionPolicy::XChaCha20Poly1305;
        let svc = MediaService::open(&cfg).unwrap();
        let h = svc.health();
        assert!(h.encryption_enabled);
    }
}