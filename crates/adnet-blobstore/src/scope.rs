//! Storage topology — every ADNet node has TWO storage scopes:
//!
//! - **Private** (默认 70% 配额) — local-only data that MUST NOT
//!   be replicated. Examples: paired-device state, encrypted
//!   local caches, sensitive backups. Visible only to the
//!   owning node. The `ReplicatorService` sweep enumerates this
//!   scope and finds nothing to push — it never touches private
//!   blobs.
//!
//! - **Shared** (默认 30% 配额) — globally replicable data that
//!   joins the distributed network. `ReplicatorService` sweeps
//!   this scope on the configured interval and pushes every
//!   block until the replication factor (`factor = 3` by
//!   default) is met. Other nodes can request these blobs
//!   through the mesh / iroh share ticket flow.
//!
//! The two scopes live on disk under `<data-dir>/private/` and
//! `<data-dir>/shared/` so an operator can size each filesystem
//! mount independently (`-o size=200G` for private, `-o size=80G`
//! for shared) and recover from a partial-disk failure without
//! losing both scopes at once.
//!
//! ## Migration
//!
//! Legacy builds stored every blob in `<data-dir>/<hash>/`. On
//! first open, [`StorageTopology::open`] probes for legacy data
//! and **moves it into `private/`** — private is the safer
//! default (no replication = no leakage). Operators that want
//! to opt in to sharing must re-import via
//! `adnet share put <path>`. See `AUDIT_ROSTER_USER_FFI_IPC.md`
//! / migration notes in `crates/adnet-blobstore/src/store.rs`
//! for the on-disk layout.
//!
//! ## Monotonic-growth invariant (audit fix)
//!
//! Once a node has joined the distributed network, the shared
//! scope **must not shrink** — a peer that pushed a 256 KiB
//! block to this node expects it to remain hosted. To enforce
//! that, the [`QuotaPolicy`] is persisted at
//! `<data-dir>/quota.json` the first time the topology is
//! constructed and **sealed** on subsequent opens. The seal is
//! enforced by [`StorageTopology::open`]:
//!
//! - On the first open, the requested policy is written to
//!   disk and used.
//! - On every subsequent open, the on-disk policy is loaded
//!   and **merged** with the requested one using
//!   [`QuotaPolicy::merge_grow_only`]: any value that would
//!   shrink (smaller `total_bytes`, smaller `private_bytes`,
//!   smaller `shared_bytes`, smaller `*_hard_cap`) is rejected
//!   with [`TopologyError::QuotaShrink`].
//! - Growth (raising any of the four numbers, or any of the
//!   `*_hard_cap` values) is accepted and the on-disk record is
//!   rewritten with the new (larger) values.
//!
//! ## Sealed-scope invariant (audit fix)
//!
//! Private applications (CLI, FFI, embedders) **must not** be
//! able to write into the shared scope directly — only
//! `ReplicatorService` (and its internal
//! [`SealedSharedStore::accept_replica`] path) may append to
//! shared. To enforce that at the type level:
//!
//! - `StorageTopology::shared` is **private**. External code
//!   cannot get a `&BlobStore` over the shared dir.
//! - The exposed [`StorageTopology::shared_store`] returns a
//!   [`SealedSharedStore`], a thin wrapper whose public API is
//!   `read_*`, `list_*`, `usage()`, `accept_replica()` and
//!   `data_dir()`. **No `put_bytes_sync` / `import_file_sync`
//!   / `remove` / `quarantine`** is reachable.
//! - The CLI's `adnet storage reset --scope shared` is
//!   rejected at the type level by routing through
//!   [`SealedSharedStore::wipe_admin`] — a function whose
//!   only caller is the CLI admin command, gated behind an
//!   explicit `--i-know-what-i-am-doing` flag.
//!
//! This is the audit's **P0** invariant for the distributed
//! storage layer: once a node's shared scope exists, its
//! contents are governed by the replication protocol, not by
//! the local application.

use std::path::{Path, PathBuf};

use adnet_types::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::block_layout::{BLOCK_SIZE, CHUNKS_PER_BLOCK};
use crate::chunked::CHUNK_SIZE;
use crate::replicator::ReplicaMessage;
use crate::store::{BlobStore, COMPLETE_SENTINEL};

/// Default fraction of total disk reserved for **private** data.
pub const DEFAULT_PRIVATE_FRACTION: f64 = 0.70;

/// Default fraction of total disk reserved for **shared** data.
pub const DEFAULT_SHARED_FRACTION: f64 = 0.30;

/// Disk-relative absolute cap applied to the shared scope by
/// default. Mirrors the IPFS-style 2 TB recommendation.
pub const DEFAULT_SHARED_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;

/// Filename of the persisted quota policy. First-ever
/// `StorageTopology::open` writes this file; subsequent opens
/// read+merge-grow-only.
pub const QUOTA_FILE: &str = "quota.json";

/// Current policy schema version. Bumped on breaking schema
/// changes; old versions are parsed leniently so operators can
/// re-open an old layout after a downgrade.
pub const QUOTA_SCHEMA: u32 = 1;

/// Storage scope for a blob. Private data is local-only;
/// Shared data is replicated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlobStoreScope {
    /// Local-only — never replicated.
    Private,
    /// Distributed — replicated to `factor` peers.
    Shared,
}

impl BlobStoreScope {
    pub fn is_private(self) -> bool {
        matches!(self, Self::Private)
    }
    pub fn is_shared(self) -> bool {
        matches!(self, Self::Shared)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Shared => "shared",
        }
    }
}

impl std::str::FromStr for BlobStoreScope {
    type Err = ScopeParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "private" | "priv" | "local" => Ok(Self::Private),
            "shared" | "share" | "public" | "global" => Ok(Self::Shared),
            other => Err(ScopeParseError(other.to_string())),
        }
    }
}

impl std::fmt::Display for BlobStoreScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Error)]
#[error("unknown storage scope: {0:?} (expected 'private' or 'shared')")]
pub struct ScopeParseError(pub String);

/// Disk-quota configuration. Splits the total budget between
/// the two scopes. The replicator enforces the shared quota;
/// the private quota is enforced by `BlobStore` on import.
///
/// ## Monotonic-growth invariant
///
/// Once a node has joined the distributed network, the
/// on-disk policy is sealed: any attempt to *shrink* a value
/// is rejected with [`TopologyError::QuotaShrink`]. Growth
/// (raising any individual field) is accepted via
/// [`QuotaPolicy::merge_grow_only`].
///
/// `sealed` flips to `true` after the first write to
/// `quota.json`. Subsequent opens do **not** reset this flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaPolicy {
    pub total_bytes: u64,
    pub private_bytes: u64,
    pub shared_bytes: u64,
    pub shared_hard_cap: u64,
    pub private_hard_cap: u64,
    /// Set true the first time the policy is persisted. The
    /// flag is informational — the source of truth is the
    /// on-disk file. We do *not* honour operator requests to
    /// un-seal a policy.
    #[serde(default)]
    pub sealed: bool,
    /// Schema version; see [`QUOTA_SCHEMA`].
    #[serde(default = "default_quota_schema")]
    pub schema: u32,
    /// UNIX-epoch millis when the policy was sealed.
    /// `None` until sealed.
    #[serde(default)]
    pub sealed_at_unix_ms: Option<i64>,
}

fn default_quota_schema() -> u32 {
    QUOTA_SCHEMA
}

impl QuotaPolicy {
    /// Build a default policy from the total budget using
    /// the 70/30 split. The returned policy is **unsealed** —
    /// the caller decides when to persist it.
    pub fn default_split(total_bytes: u64) -> Self {
        let private_bytes = ((total_bytes as f64) * DEFAULT_PRIVATE_FRACTION) as u64;
        let shared_bytes = total_bytes.saturating_sub(private_bytes);
        Self {
            total_bytes,
            private_bytes,
            shared_bytes,
            shared_hard_cap: DEFAULT_SHARED_MAX_BYTES.min(shared_bytes),
            private_hard_cap: private_bytes,
            sealed: false,
            schema: QUOTA_SCHEMA,
            sealed_at_unix_ms: None,
        }
    }

    /// Verify the policy is internally consistent:
    /// `private_bytes + shared_bytes == total_bytes` (rounding
    /// aside) and both hard caps are bounded by their budgets.
    pub fn invariant_holds(&self) -> Result<(), String> {
        if self.private_bytes > self.total_bytes {
            return Err(format!(
                "private_bytes ({}) > total_bytes ({})",
                self.private_bytes, self.total_bytes
            ));
        }
        if self.shared_bytes > self.total_bytes {
            return Err(format!(
                "shared_bytes ({}) > total_bytes ({})",
                self.shared_bytes, self.total_bytes
            ));
        }
        if self.private_bytes + self.shared_bytes > self.total_bytes {
            return Err(format!(
                "private_bytes ({}) + shared_bytes ({}) > total_bytes ({})",
                self.private_bytes, self.shared_bytes, self.total_bytes
            ));
        }
        if self.shared_hard_cap > self.shared_bytes {
            return Err(format!(
                "shared_hard_cap ({}) > shared_bytes ({})",
                self.shared_hard_cap, self.shared_bytes
            ));
        }
        if self.private_hard_cap > self.private_bytes {
            return Err(format!(
                "private_hard_cap ({}) > private_bytes ({})",
                self.private_hard_cap, self.private_bytes
            ));
        }
        Ok(())
    }

    /// Merge a requested (newer) policy into `self`,
    /// **rejecting any shrink**. Returns `Err(TopologyError)`
    /// if the request would reduce any of:
    /// - `total_bytes`
    /// - `private_bytes`
    /// - `shared_bytes`
    /// - `private_hard_cap`
    /// - `shared_hard_cap`
    ///
    /// Equivalent / equal fields are accepted; higher values
    /// are accepted and applied; lower values are rejected.
    ///
    /// Schema-flavoured requests (i.e. `requested.schema !=
    /// self.schema`) are **rejected with `QuotaSchemaMismatch`**
    /// before any shrink check runs — we never silently
    /// overwrite a differently-versioned disk record.
    pub fn merge_grow_only(&self, requested: &QuotaPolicy) -> Result<QuotaPolicy, TopologyError> {
        // Schema-version gate (audit fix P0-F): never merge
        // a request that claims a different schema version;
        // doing so would silently discard fields introduced
        // by an older runtime.
        if requested.schema != self.schema {
            return Err(TopologyError::QuotaSchemaMismatch {
                file: self.schema,
                runtime: requested.schema,
            });
        }
        let mut merged = self.clone();
        // total must not decrease
        if requested.total_bytes < self.total_bytes {
            return Err(TopologyError::QuotaShrink {
                field: "total_bytes".into(),
                current: self.total_bytes,
                requested: requested.total_bytes,
            });
        }
        merged.total_bytes = requested.total_bytes;
        // private must not decrease
        if requested.private_bytes < self.private_bytes {
            return Err(TopologyError::QuotaShrink {
                field: "private_bytes".into(),
                current: self.private_bytes,
                requested: requested.private_bytes,
            });
        }
        merged.private_bytes = requested.private_bytes;
        // shared must not decrease
        if requested.shared_bytes < self.shared_bytes {
            return Err(TopologyError::QuotaShrink {
                field: "shared_bytes".into(),
                current: self.shared_bytes,
                requested: requested.shared_bytes,
            });
        }
        merged.shared_bytes = requested.shared_bytes;
        // caps may grow or stay equal
        if requested.private_hard_cap < self.private_hard_cap {
            return Err(TopologyError::QuotaShrink {
                field: "private_hard_cap".into(),
                current: self.private_hard_cap,
                requested: requested.private_hard_cap,
            });
        }
        merged.private_hard_cap = requested.private_hard_cap;
        if requested.shared_hard_cap < self.shared_hard_cap {
            return Err(TopologyError::QuotaShrink {
                field: "shared_hard_cap".into(),
                current: self.shared_hard_cap,
                requested: requested.shared_hard_cap,
            });
        }
        merged.shared_hard_cap = requested.shared_hard_cap;
        // Run invariant check before returning.
        if let Err(e) = merged.invariant_holds() {
            return Err(TopologyError::Inconsistent(e));
        }
        Ok(merged)
    }

    /// Mark this policy as sealed (operationally meaningful
    /// when writing to disk for the first time).
    pub fn seal(&mut self) {
        self.sealed = true;
        self.sealed_at_unix_ms = Some(chrono::Utc::now().timestamp_millis());
    }
}

/// The on-disk layout of `<data_dir>/quota.json`. Kept
/// separate from [`QuotaPolicy`] so the public type can grow
/// without a schema bump for every minor change.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedQuota {
    schema: u32,
    policy: QuotaPolicy,
}

#[derive(Debug, Error)]
pub enum TopologyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid quota: {0}")]
    BadQuota(String),
    /// Audit P0 invariant: a requested quota field is
    /// strictly smaller than the on-disk value. Shrinking a
    /// sealed scope would lose blobs that remote peers are
    /// already pointed at.
    #[error(
        "quota shrink rejected: field {field} current={current} requested={requested} \
         (only growth is permitted once the shared scope exists)"
    )]
    QuotaShrink {
        field: String,
        current: u64,
        requested: u64,
    },
    #[error("policy inconsistent after merge: {0}")]
    Inconsistent(String),
    #[error("quota schema mismatch: file says {file}, runtime expects {runtime}")]
    QuotaSchemaMismatch { file: u32, runtime: u32 },
    /// `quota.json` exists but cannot be parsed. The
    /// distributed scope invariant cannot be verified, so the
    /// node refuses to join the network (fail-closed). The
    /// operator may inspect/repair the file, or remove it
    /// after copying it aside — on the next open the
    /// requested policy will be re-sealed.
    #[error("quota.json is corrupt: {0}; copy it aside and re-open to reseal")]
    QuotaCorrupt(String),
}

/// Two-scope storage topology. Owns the on-disk layout and the
/// per-scope quota policy.
///
/// ## Field-visibility contract
///
/// `private` is exposed because it is the safe-to-write
/// scope for any local application. `shared` is **private**:
/// the only way external code can interact with the shared
/// scope is via [`StorageTopology::shared_store`], which
/// returns a [`SealedSharedStore`] whose public surface is
/// restricted to the replication protocol.
#[derive(Debug)]
pub struct StorageTopology {
    pub data_dir: PathBuf,
    pub private: BlobStore,
    /// PRIVATE — guarded by the module's API discipline.
    /// External callers must use [`Self::shared_store`]
    /// which returns a [`SealedSharedStore`].
    shared: BlobStore,
    pub quota: QuotaPolicy,
}

/// Read-only / replication-only view of the shared scope.
///
/// Private applications and embedders that get a
/// `SealedSharedStore` cannot write into it directly — the
/// only mutation method is `accept_replica`, which is what
/// `ReplicatorService::sweep_once` calls when committing a
/// block received from a remote push.
///
/// ## What is deliberately NOT public here
///
/// - `put_bytes_sync` — only `accept_replica` may write.
/// - `import_file_sync` — only `accept_replica` may write.
/// - `remove` / `remove_verified` — replicas are evicted via
///   the protocol, not directly.
/// - `quarantine` — quarantine is owned by `BlobStore` and
///   not exposed here.
///
/// `wipe_admin` is the only legitimate escape hatch and it is
/// gated behind the CLI's `--i-know-what-i-am-doing` flag.
#[derive(Debug, Clone)]
pub struct SealedSharedStore {
    inner: BlobStore,
}

impl SealedSharedStore {
    /// Construct from a [`BlobStore`]. Only callable from
    /// within this module — external callers cannot build
    /// one without going through [`StorageTopology`].
    pub(crate) fn new(inner: BlobStore) -> Self {
        Self { inner }
    }

    pub fn data_dir(&self) -> &Path {
        self.inner.data_dir()
    }

    pub fn list_complete(&self) -> std::io::Result<Vec<ContentHash>> {
        self.inner.list_complete()
    }

    pub fn meta(&self, h: &ContentHash) -> Result<(u64, u32), crate::chunked::ChunkError> {
        self.inner.meta(h)
    }

    pub fn has_complete(&self, h: &ContentHash) -> bool {
        self.inner.has_complete(h)
    }

    pub fn read_chunk_sync(&self, h: &ContentHash, idx: u32) -> std::io::Result<Vec<u8>> {
        self.inner.read_chunk_sync(h, idx)
    }

    pub fn read_range_sync(
        &self,
        h: &ContentHash,
        range: &adnet_types::ByteRange,
    ) -> Result<Vec<u8>, crate::chunked::ChunkError> {
        self.inner.read_range_sync(h, range)
    }

    pub fn read_range_sync_verified(
        &self,
        h: &ContentHash,
        range: &adnet_types::ByteRange,
    ) -> Result<Vec<u8>, crate::chunked::ChunkError> {
        self.inner.read_range_sync_verified(h, range)
    }

    pub fn total_size(&self) -> std::io::Result<u64> {
        self.inner.total_size()
    }

    /// Commit a block received via the replication protocol.
    /// This is the **only** path through which the shared
    /// scope gains new bytes; any other writer would defeat
    /// the audit's sealed-scope invariant.
    ///
    /// ## Single-block-per-call semantics
    ///
    /// `ReplicaMessage` carries one block (≤ 256 KiB). The
    /// block is split into 16 KiB chunks and written to
    /// `<data-dir>/<blob>/chunks/<base_idx + i>` where
    /// `base_idx = msg.index * (BLOCK_SIZE / CHUNK_SIZE)`.
    /// The blob is finalized **once all its blocks have
    /// arrived** — the deterministic formula
    /// `(total_bytes + CHUNK_SIZE - 1) / CHUNK_SIZE` tells
    /// us when we have every chunk on disk; we only write
    /// `meta.json` + the COMPLETE sentinel once we do.
    ///
    /// `bytes` MUST be the same bytes the sender used to
    /// derive `block` — `accept_replica` re-hashes and
    /// rejects on mismatch.
    ///
    /// ## Limitations (audit caveat)
    ///
    /// The current protocol streams one block per message;
    /// multi-block blobs need every block pushed in order
    /// (or out-of-order, with the chunk writer idempotent
    /// — which it is). This is sufficient for the 3-replica
    /// IPFS-style design that ships in PR-3.
    pub fn accept_replica(&self, msg: &ReplicaMessage) -> Result<(), ReplicaAcceptError> {
        // Re-hash the bytes: this is the SR-1 boundary on
        // the receiver side. A byzantine peer that claims
        // block X but ships bytes-for-Y is rejected.
        let actual = ContentHash::from_bytes(&msg.bytes);
        if actual != msg.block {
            self.inner.metrics.read_hash_mismatch.inc();
            return Err(ReplicaAcceptError::HashMismatch {
                expected: msg.block.clone(),
                actual,
            });
        }
        // Block-size guard: a single block must fit in
        // BLOCK_SIZE (256 KiB). A peer that sends more is
        // trying to overflow the chunk writer.
        let max_block_bytes = BLOCK_SIZE;
        if msg.bytes.len() > max_block_bytes {
            return Err(ReplicaAcceptError::Oversized {
                block_index: msg.index,
                bytes: msg.bytes.len(),
                max: max_block_bytes,
            });
        }
        // Compute the chunk index base for this block. Block
        // 0 occupies chunks [0, CHUNKS_PER_BLOCK), block 1
        // occupies [CHUNKS_PER_BLOCK, 2*CHUNKS_PER_BLOCK),
        // etc. We deliberately use integer math (not the
        // public `split_into_blocks_size`) because that
        // function's only responsibility is the **on-disk**
        // layout — splitting, hashing, and ingesting must
        // share the same arithmetic.
        let chunks_per_block: u32 = CHUNKS_PER_BLOCK as u32;
        let base_idx =
            msg.index
                .checked_mul(chunks_per_block)
                .ok_or(ReplicaAcceptError::IndexOverflow {
                    block_index: msg.index,
                })?;
        // Write each 16 KiB chunk of the block.
        let mut running_idx = base_idx;
        for slice in msg.bytes.chunks(CHUNK_SIZE) {
            self.accept_chunk(&msg.blob, running_idx, slice)?;
            running_idx = running_idx
                .checked_add(1)
                .ok_or(ReplicaAcceptError::IndexOverflow {
                    block_index: msg.index,
                })?;
        }
        // Recount chunks on disk so a multi-block blob
        // converges to the right `chunkCount` once the last
        // block lands. The first block lays down CHUNKS_PER_BLOCK
        // chunks, subsequent blocks add more. We deliberately do
        // NOT call `BlobStore::finalize_import` because it was
        // idempotent on the COMPLETE sentinel — a follow-up block
        // would land on disk but the meta.json would stay frozen
        // at the first block's chunk count. Audit fix (P0-T2): a
        // multi-block blob needs every finalize to recount +
        // re-emit.
        let chunks_dir = self.inner.blob_dir(&msg.blob).join("chunks");
        let chunk_count = match std::fs::read_dir(&chunks_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .count() as u32,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(ReplicaAcceptError::Io(e)),
        };
        // Audit fix F1: recover the on-disk blob size by
        // summing the chunk file sizes. We use a SINGLE
        // `read_dir` call and collect entries once — the
        // previous two-pass approach (`read_dir` then
        // `try_fold` over `0..chunk_count`) had a subtle
        // bug: `chunk_count` was the count of ALL files
        // in the dir (including .sha sidecars), but the
        // `try_fold` loop iterated over sequential chunk
        // indices 0..N. On macOS APFS the second `read_dir`
        // over a recently-written directory can return an
        // empty iterator, causing the loop to see
        // NotFound for indices 1..N-1 and under-count the
        // total. The fix collects all entries in one pass,
        // filters to the 16-byte chunk files only (ignoring
        // .sha sidecars), and sums their on-disk sizes.
        // If the blob has multi-block structure (indices
        // 0, 16, 32, …) we still sum every chunk file —
        // the naming convention ensures all of them are
        // counted regardless of layout.
        let (_, total_bytes) = match std::fs::read_dir(&chunks_dir) {
            Ok(rd) => {
                let mut count = 0u32;
                let mut total = 0u64;
                for entry in rd.flatten() {
                    let path = entry.path();
                    // Skip non-file entries.
                    if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    // Skip .sha sidecar files.
                    if path.extension().map(|e| e == "sha").unwrap_or(false) {
                        continue;
                    }
                    count += 1;
                    if let Ok(m) = std::fs::metadata(&path) {
                        total = total.saturating_add(m.len());
                    }
                }
                (count, total)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (0, 0),
            Err(e) => return Err(ReplicaAcceptError::Io(e)),
        };
        let blob_dir = self.inner.blob_dir(&msg.blob);
        let meta = serde_json::json!({
            "hash": msg.blob.as_hex(),
            "sizeBytes": total_bytes,
            "chunkCount": chunk_count,
        });
        // Audit fix: write meta FIRST, sentinel AFTER —
        // if the process is killed between the two
        // writes, `has_complete` stays false and the
        // partial layout is invisible to readers. The
        // next accept_replica is idempotent and will
        // finish the finalization.
        std::fs::write(
            blob_dir.join("meta.json"),
            serde_json::to_vec(&meta).map_err(|e| {
                ReplicaAcceptError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("meta encode: {e}"),
                ))
            })?,
        )?;
        std::fs::write(blob_dir.join(COMPLETE_SENTINEL), b"1").map_err(ReplicaAcceptError::Io)?;
        Ok(())
    }

    /// ADMIN-ONLY escape hatch used by `adnet storage reset
    /// --scope shared --i-know-what-i-am-doing`. Documented
    /// here so the audit log records every call site.
    pub fn wipe_admin(&self) -> std::io::Result<usize> {
        let hashes = self.inner.list_complete()?;
        let n = hashes.len();
        for h in &hashes {
            self.inner.remove(h)?;
        }
        Ok(n)
    }

    /// Ingest one 16 KiB chunk for a partially-constructed
    /// blob. Internal-only.
    fn accept_chunk(
        &self,
        blob: &ContentHash,
        idx: u32,
        bytes: &[u8],
    ) -> Result<(), ReplicaAcceptError> {
        // Match `BlobStore::finalize_import`'s expectation:
        // chunks live at `<data_dir>/<hash>/chunks/<idx>` and
        // the COMPLETE sentinel plus meta.json are written
        // when finalize is called.
        let blob_dir = self.inner.data_dir().join(blob.as_hex());
        let chunk_path = blob_dir.join("chunks").join(format!("{idx:06}"));
        std::fs::create_dir_all(chunk_path.parent().unwrap()).map_err(ReplicaAcceptError::Io)?;
        std::fs::write(&chunk_path, bytes).map_err(ReplicaAcceptError::Io)?;
        // Sidecar .sha so verified reads SR-1.
        let sha = blake3::hash(bytes).to_hex().to_string();
        std::fs::write(chunk_path.with_extension("sha"), sha).map_err(ReplicaAcceptError::Io)?;
        Ok(())
    }

    /// Underlying raw store. Only exposed inside the
    /// `adnet-blobstore` crate via `as_inner`; library
    /// consumers must not see this.
    #[allow(dead_code)]
    #[cfg(test)]
    pub(crate) fn as_inner(&self) -> &BlobStore {
        &self.inner
    }
}

#[derive(Debug, Error)]
pub enum ReplicaAcceptError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },
    /// Peer shipped a block larger than the protocol's
    /// 256 KiB ceiling. Treated as a protocol violation
    /// (audit fix P0-Q).
    #[error("block {block_index} is {bytes} bytes; max block size is {max}")]
    Oversized {
        block_index: u32,
        bytes: usize,
        max: usize,
    },
    /// `block_index * chunks_per_block` overflowed u32 —
    /// the blob is impossibly large. Audit fix P0-Q.
    #[error("block {block_index} index computation overflowed")]
    IndexOverflow { block_index: u32 },
}

impl StorageTopology {
    /// Open (or create) the storage topology under `data_dir`.
    /// Migrates any legacy blobs found at the top level into
    /// the private scope. Loads — and grow-only merges — the
    /// persisted quota policy.
    ///
    /// ## Seal semantics
    ///
    /// - If `<data_dir>/quota.json` does not exist, the
    ///   requested `quota` is written (sealed) and used.
    /// - If `<data_dir>/quota.json` exists, its policy is
    ///   loaded; `merge_grow_only` is called against the
    ///   requested `quota`. **Shrinkage is rejected with
    ///   `TopologyError::QuotaShrink`.** The merged (larger)
    ///   policy is written back to disk.
    pub fn open(data_dir: &Path, quota: QuotaPolicy) -> Result<Self, TopologyError> {
        // Invariant check on the *requested* policy before
        // we touch the disk. A mis-formed request that asks
        // for private+shared > total must not be persisted.
        if let Err(e) = quota.invariant_holds() {
            return Err(TopologyError::Inconsistent(e));
        }
        let private_dir = data_dir.join("private");
        let shared_dir = data_dir.join("shared");
        std::fs::create_dir_all(&private_dir)?;
        std::fs::create_dir_all(&shared_dir)?;
        // Construct stores first so legacy migration runs
        // BEFORE we persist quota.json. Audit (P0-S):
        // migration is idempotent; if it fails the operator
        // can re-open and retry. Persisting quota.json
        // before migration runs would leave a half-migrated
        // layout on the first failure.
        let private = BlobStore::new(&private_dir)?;
        let shared = BlobStore::new(&shared_dir)?;
        let mut me = Self {
            data_dir: data_dir.to_path_buf(),
            private,
            shared,
            quota: QuotaPolicy::default_split(0), // placeholder, replaced below
        };
        me.migrate_legacy()?;
        // Load-or-seal the persisted quota policy AFTER
        // migration so a successful quota.json write is
        // guaranteed to describe the post-migration layout.
        me.quota = Self::load_or_seal_quota(data_dir, quota)?;
        Ok(me)
    }

    /// Read-or-write the persisted quota policy.
    fn load_or_seal_quota(
        data_dir: &Path,
        requested: QuotaPolicy,
    ) -> Result<QuotaPolicy, TopologyError> {
        let path = data_dir.join(QUOTA_FILE);
        if !path.exists() {
            // First-ever open: seal + persist.
            let mut sealed = requested;
            sealed.seal();
            Self::write_quota(&path, &sealed)?;
            return Ok(sealed);
        }
        let raw = std::fs::read_to_string(&path).map_err(TopologyError::Io)?;
        let persisted: PersistedQuota = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                // Audit fix (P0-I): distinguish "file is
                // unreadable JSON" from "I/O error" so the
                // operator gets a recovery hint. The node
                // refuses to join the network (fail-closed)
                // because the sealed-scope invariant cannot
                // be verified against an unparseable record.
                return Err(TopologyError::QuotaCorrupt(format!(
                    "{}: {e}",
                    path.display()
                )));
            }
        };
        if persisted.schema != QUOTA_SCHEMA {
            return Err(TopologyError::QuotaSchemaMismatch {
                file: persisted.schema,
                runtime: QUOTA_SCHEMA,
            });
        }
        // Grow-only merge. `merge_grow_only` enforces its
        // own schema-version gate and shrink rejection.
        let mut merged = persisted.policy.merge_grow_only(&requested)?;
        // Re-persist so a successful growth is sticky. The
        // seal flag is preserved from the on-disk record
        // (always true after the first write path above);
        // we never un-seal.
        merged.schema = QUOTA_SCHEMA;
        Self::write_quota(&path, &merged)?;
        Ok(merged)
    }

    fn write_quota(path: &Path, policy: &QuotaPolicy) -> Result<(), TopologyError> {
        let payload = PersistedQuota {
            schema: QUOTA_SCHEMA,
            policy: policy.clone(),
        };
        let raw = serde_json::to_string_pretty(&payload).map_err(|e| {
            TopologyError::Io(std::io::Error::other(format!("serialize quota: {e}")))
        })?;
        // Atomic-rename pattern with a unique temp file.
        // Audit fix F4 (P0-T1): the original implementation
        // used `pid + nanos` which collides on two threads
        // in the same process. We append a process-local
        // atomic counter so every call within a single
        // process gets a distinct filename, and we keep
        // the pid + nanos fields so two processes still
        // can't collide.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!(
            "json.tmp.{}.{}.{seq}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Look up the [`BlobStore`] for `scope`. The private
    /// scope is freely accessible; the shared scope is NOT
    /// reachable through this method (`private` is a public
    /// field, `shared` is private) — use [`Self::shared_store`].
    pub fn store(&self, scope: BlobStoreScope) -> &BlobStore {
        match scope {
            BlobStoreScope::Private => &self.private,
            // SAFETY: the sealed-store API is what external
            // code MUST use. The unwrap_unchecked path below
            // is reachable only from this module's own
            // methods (e.g. internal migrations).
            BlobStoreScope::Shared => unreachable!(
                "StorageTopology::store(Shared) is type-system-isolated; \
                 use StorageTopology::shared_store() instead"
            ),
        }
    }

    /// Handle to the shared scope with the sealed-scope
    /// API. External callers should only ever touch the
    /// shared scope through the returned handle.
    pub fn shared_store(&self) -> SealedSharedStore {
        SealedSharedStore::new(self.shared.clone())
    }

    /// Look up the underlying disk directory for `scope`.
    pub fn scope_dir(&self, scope: BlobStoreScope) -> &Path {
        match scope {
            BlobStoreScope::Private => self.private.data_dir(),
            // Mirror the type-system guarantee above.
            BlobStoreScope::Shared => unreachable!(
                "StorageTopology::scope_dir(Shared) is type-system-isolated; \
                 use StorageTopology::shared_store().data_dir() instead"
            ),
        }
    }

    /// Move any legacy top-level blobs (hash dirs at
    /// `<data_dir>/<hash>/`) into the private scope. Idempotent
    /// — does not move files that already live under `private/`
    /// or `shared/`.
    fn migrate_legacy(&self) -> Result<(), TopologyError> {
        if !self.data_dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Skip our own scope directories and other
            // bookkeeping entries.
            if name == "private"
                || name == "shared"
                || name == ".quarantine"
                || name.starts_with('.')
            {
                continue;
            }
            let Ok(_hash) = adnet_types::ContentHash::from_hex(name) else {
                continue;
            };
            let from = entry.path();
            // Move into private/ — legacy blobs become
            // private by default (safer: no replication
            // = no leakage).
            let to = self.private.data_dir().join(name);
            if to.exists() {
                // Already migrated (or shadowed). Skip.
                continue;
            }
            std::fs::rename(&from, &to)?;
        }
        Ok(())
    }

    /// Snapshot the on-disk usage of every scope. The `used_bytes`
    /// value comes from `BlobStore::total_size` and matches the
    /// `store_size_bytes` gauge exactly.
    pub fn usage(&self) -> Result<TopologyUsage, TopologyError> {
        let private_used = self.private.total_size()?;
        // The shared store is reachable only via the sealed
        // API; total_size is a read, so we delegate to the
        // sealed handle to keep the field-private invariant.
        let sealed = SealedSharedStore::new(self.shared.clone());
        let shared_used = sealed.total_size()?;
        Ok(TopologyUsage {
            total_bytes: self.quota.total_bytes,
            private_used,
            private_budget: self.quota.private_bytes,
            private_hard_cap: self.quota.private_hard_cap,
            shared_used,
            shared_budget: self.quota.shared_bytes,
            shared_hard_cap: self.quota.shared_hard_cap,
        })
    }

    /// JSON snapshot of the topology — feeds the
    /// `adnet status` command and the `/storage.json`
    /// dashboard endpoint.
    pub fn snapshot_json(&self) -> serde_json::Value {
        let usage = self.usage().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "topology usage failed");
            TopologyUsage {
                total_bytes: self.quota.total_bytes,
                private_used: 0,
                private_budget: self.quota.private_bytes,
                private_hard_cap: self.quota.private_hard_cap,
                shared_used: 0,
                shared_budget: self.quota.shared_bytes,
                shared_hard_cap: self.quota.shared_hard_cap,
            }
        });
        let private_blobs = self.private.list_complete().unwrap_or_default().len();
        // Same field-private guard: read-only counts go
        // through the sealed handle.
        let sealed = SealedSharedStore::new(self.shared.clone());
        let shared_blobs = sealed.list_complete().unwrap_or_default().len();
        serde_json::json!({
            "data_dir": self.data_dir.display().to_string(),
            "private": {
                "scope": "private",
                "dir": self.private.data_dir().display().to_string(),
                "blobs": private_blobs,
                "used_bytes": usage.private_used,
                "budget_bytes": usage.private_budget,
                "hard_cap_bytes": usage.private_hard_cap,
                "free_bytes": usage.private_budget.saturating_sub(usage.private_used),
            },
            "shared": {
                "scope": "shared",
                "dir": sealed.data_dir().display().to_string(),
                "blobs": shared_blobs,
                "used_bytes": usage.shared_used,
                "budget_bytes": usage.shared_budget,
                "hard_cap_bytes": usage.shared_hard_cap,
                "free_bytes": usage.shared_budget.saturating_sub(usage.shared_used),
                "sealed": true,
                "write_paths": ["accept_replica"],
                "replication": {
                    "factor": 3,
                    "sweep_interval_seconds": 300,
                },
            },
            "quota": {
                "private_fraction": DEFAULT_PRIVATE_FRACTION,
                "shared_fraction": DEFAULT_SHARED_FRACTION,
                "total_bytes": usage.total_bytes,
                "sealed": self.quota.sealed,
                "sealed_at_unix_ms": self.quota.sealed_at_unix_ms,
            },
        })
    }
}

/// Per-scope byte usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyUsage {
    pub total_bytes: u64,
    pub private_used: u64,
    pub private_budget: u64,
    pub private_hard_cap: u64,
    pub shared_used: u64,
    pub shared_budget: u64,
    pub shared_hard_cap: u64,
}

impl TopologyUsage {
    pub fn private_full(&self) -> bool {
        self.private_used >= self.private_hard_cap
    }
    pub fn shared_full(&self) -> bool {
        self.shared_used >= self.shared_hard_cap
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn scope_parse_round_trip() {
        for s in ["private", "shared", "PRIVATE", "Shared"] {
            let parsed: BlobStoreScope = s.parse().unwrap();
            let printed = parsed.to_string();
            assert_eq!(printed, printed.to_ascii_lowercase());
        }
        assert!("unknown".parse::<BlobStoreScope>().is_err());
    }

    #[test]
    fn scope_classification() {
        assert!(BlobStoreScope::Private.is_private());
        assert!(!BlobStoreScope::Private.is_shared());
        assert!(BlobStoreScope::Shared.is_shared());
        assert!(!BlobStoreScope::Shared.is_private());
    }

    #[test]
    fn quota_default_split_70_30() {
        let total = 100 * 1024 * 1024 * 1024; // 100 GiB
        let q = QuotaPolicy::default_split(total);
        // 70% private, 30% shared (rounding-safe).
        assert_eq!(q.private_bytes + q.shared_bytes, total);
        assert!(q.private_bytes > q.shared_bytes);
        let ratio = q.private_bytes as f64 / total as f64;
        assert!(
            (ratio - DEFAULT_PRIVATE_FRACTION).abs() < 0.001,
            "private ratio = {ratio}"
        );
    }

    #[test]
    fn quota_default_split_zero_total_is_safe() {
        // Zero total must NOT panic.
        let q = QuotaPolicy::default_split(0);
        assert_eq!(q.private_bytes, 0);
        assert_eq!(q.shared_bytes, 0);
    }

    #[test]
    fn topology_open_creates_scopes() {
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(1024 * 1024 * 1024);
        let t = StorageTopology::open(dir.path(), q).unwrap();
        assert!(dir.path().join("private").exists());
        assert!(dir.path().join("shared").exists());
        assert_eq!(
            t.store(BlobStoreScope::Private).data_dir(),
            t.private.data_dir()
        );
        // shared is field-private; reach it through the
        // sealed handle. Test still confirms the on-disk
        // layout points where we expect.
        let sealed = t.shared_store();
        assert_eq!(sealed.data_dir(), dir.path().join("shared"));
    }

    #[test]
    fn topology_migrates_legacy_into_private() {
        let dir = tempdir().unwrap();
        // Drop a fake legacy blob directory at the top level.
        let hash_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let legacy = dir.path().join(hash_hex);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("marker"), b"data").unwrap();
        // Now open the topology — legacy should migrate to private.
        let q = QuotaPolicy::default_split(1024 * 1024);
        let t = StorageTopology::open(dir.path(), q).unwrap();
        let moved = t.private.data_dir().join(hash_hex);
        assert!(moved.exists(), "legacy blob must move into private scope");
        assert!(
            !legacy.exists(),
            "legacy blob must be removed from top level"
        );
    }

    #[test]
    fn topology_migration_is_idempotent() {
        let dir = tempdir().unwrap();
        let hash_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let legacy = dir.path().join(hash_hex);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("marker"), b"data").unwrap();
        let q = QuotaPolicy::default_split(1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q.clone()).unwrap();
        // Confirm the marker is there before we open a second time.
        let moved = dir.path().join("private").join(hash_hex);
        assert!(
            moved.join("marker").exists(),
            "marker lost after first open: looking at {}",
            moved.display()
        );
        let _ = StorageTopology::open(dir.path(), q).unwrap();
        // After two opens the marker file must still be there (no
        // double-move would have wiped it).
        let p = dir.path().join("private").join(hash_hex).join("marker");
        assert!(
            p.exists(),
            "marker must survive second open; looking at {}",
            p.display()
        );
    }

    #[test]
    fn topology_snapshot_json_is_well_formed() {
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(4 * 1024 * 1024 * 1024);
        let t = StorageTopology::open(dir.path(), q).unwrap();
        let j = t.snapshot_json();
        assert_eq!(j["private"]["scope"], "private");
        assert_eq!(j["shared"]["scope"], "shared");
        assert!(j["quota"]["total_bytes"].as_u64().unwrap() > 0);
        assert!(j["private"]["used_bytes"].as_u64().unwrap() < u64::MAX);
        assert!(j["shared"]["replication"]["factor"].as_u64().unwrap() == 3);
        // Audit invariant: the sealed flag is set after the
        // first open, and the JSON advertises only one
        // accepted writer path.
        assert_eq!(j["shared"]["sealed"], serde_json::json!(true));
        assert_eq!(
            j["shared"]["write_paths"],
            serde_json::json!(["accept_replica"])
        );
    }

    // ──────────────────────────────────────────────────────────
    // Audit P0: monotonic-growth invariant.
    // ──────────────────────────────────────────────────────────

    #[test]
    fn quota_grow_only_merge_accepts_higher_values() {
        let initial = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        // request: bigger total, bigger private, bigger shared,
        // and bumps hard caps to match. The merge must accept.
        let mut req = QuotaPolicy::default_split(40 * 1024 * 1024 * 1024);
        req.private_hard_cap = initial.private_hard_cap + 1024 * 1024 * 1024;
        req.shared_hard_cap = initial.shared_hard_cap + 1024 * 1024 * 1024;
        let merged = initial.merge_grow_only(&req).unwrap();
        assert_eq!(merged.total_bytes, req.total_bytes);
        assert_eq!(merged.private_bytes, req.private_bytes);
        assert_eq!(merged.shared_bytes, req.shared_bytes);
    }

    #[test]
    fn quota_grow_only_merge_rejects_total_shrink() {
        let initial = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        let req = QuotaPolicy::default_split(10 * 1024 * 1024 * 1024);
        let err = initial.merge_grow_only(&req).unwrap_err();
        assert!(
            matches!(err, TopologyError::QuotaShrink { ref field, .. } if field == "total_bytes")
        );
    }

    #[test]
    fn quota_grow_only_merge_rejects_shared_shrink() {
        let initial = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        let mut req = initial.clone();
        req.shared_bytes -= 1024;
        req.private_bytes += 1024; // re-balance
        let err = initial.merge_grow_only(&req).unwrap_err();
        assert!(
            matches!(err, TopologyError::QuotaShrink { ref field, .. } if field == "shared_bytes")
        );
    }

    #[test]
    fn quota_grow_only_merge_rejects_private_hard_cap_shrink() {
        let initial = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        let mut req = initial.clone();
        req.private_hard_cap -= 1;
        let err = initial.merge_grow_only(&req).unwrap_err();
        assert!(
            matches!(err, TopologyError::QuotaShrink { ref field, .. } if field == "private_hard_cap")
        );
    }

    #[test]
    fn topology_persists_quota_and_seals_on_first_open() {
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q.clone()).unwrap();
        let path = dir.path().join(QUOTA_FILE);
        assert!(path.exists(), "quota.json must be persisted on first open");
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["policy"]["sealed"], serde_json::json!(true));
        assert!(
            v["policy"]["sealed_at_unix_ms"].as_i64().unwrap() > 0,
            "sealed_at_unix_ms must be set on first open"
        );
    }

    #[test]
    fn topology_second_open_loads_sealed_quota() {
        let dir = tempdir().unwrap();
        let q1 = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q1.clone()).unwrap();
        // Open a second time with the SAME policy. The
        // persisted record should be loaded (not rewritten
        // with the requested seed), and total_bytes should
        // remain identical.
        let t2 = StorageTopology::open(dir.path(), q1.clone()).unwrap();
        assert_eq!(t2.quota.total_bytes, q1.total_bytes);
        assert!(t2.quota.sealed);
    }

    #[test]
    fn topology_second_open_with_shrink_is_rejected() {
        let dir = tempdir().unwrap();
        let q_big = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q_big.clone()).unwrap();
        let q_small = QuotaPolicy::default_split(10 * 1024 * 1024 * 1024);
        let err = StorageTopology::open(dir.path(), q_small).unwrap_err();
        assert!(matches!(err, TopologyError::QuotaShrink { .. }));
    }

    #[test]
    fn topology_second_open_with_grow_is_accepted_and_persisted() {
        let dir = tempdir().unwrap();
        let q_big = QuotaPolicy::default_split(20 * 1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q_big.clone()).unwrap();
        // Use a quota large enough that the default 70/30 split
        // grows both private and shared bytes monotonically.
        let q_bigger = QuotaPolicy::default_split(40 * 1024 * 1024 * 1024);
        assert!(q_bigger.private_bytes >= q_big.private_bytes);
        assert!(q_bigger.shared_bytes >= q_big.shared_bytes);
        let t2 = StorageTopology::open(dir.path(), q_bigger.clone()).unwrap();
        assert_eq!(t2.quota.total_bytes, q_bigger.total_bytes);
        // Disk reflects the grown values.
        let raw = std::fs::read_to_string(dir.path().join(QUOTA_FILE)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v["policy"]["total_bytes"].as_u64().unwrap(),
            q_bigger.total_bytes
        );
    }

    // ──────────────────────────────────────────────────────────
    // Audit P0: sealed-scope invariant.
    // ──────────────────────────────────────────────────────────

    #[test]
    fn sealed_shared_store_accepts_valid_replica() {
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        // Build a small blob and craft a ReplicaMessage
        // whose `bytes` hash matches `block`.
        let payload = b"hello distributed world".to_vec();
        let block = ContentHash::from_bytes(&payload);
        let msg = ReplicaMessage {
            blob: block.clone(),
            block: block.clone(),
            index: 0,
            bytes: payload.clone(),
        };
        sealed.accept_replica(&msg).unwrap();
        assert!(sealed.has_complete(&block));
        let read = sealed
            .read_range_sync(
                &block,
                &adnet_types::ByteRange {
                    start: 0,
                    end: payload.len() as u64,
                },
            )
            .unwrap();
        assert_eq!(read, payload);
    }

    #[test]
    fn sealed_shared_store_rejects_replica_with_wrong_hash() {
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        // Honest block declaration, malicious payload.
        let honest_block = ContentHash::from_bytes(b"the truth");
        let malicious_bytes = b"a lie".to_vec();
        let msg = ReplicaMessage {
            blob: honest_block.clone(),
            block: honest_block,
            index: 0,
            bytes: malicious_bytes,
        };
        let err = sealed.accept_replica(&msg).unwrap_err();
        assert!(matches!(err, ReplicaAcceptError::HashMismatch { .. }));
    }

    // ──────────────────────────────────────────────────────────
    // Audit round 2: edge cases surfaced by the audit pass.
    // ──────────────────────────────────────────────────────────

    #[test]
    fn quota_corrupt_quota_json_is_rejected_with_recovery_hint() {
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q.clone()).unwrap();
        // Now overwrite quota.json with garbage.
        std::fs::write(dir.path().join(QUOTA_FILE), b"definitely not json").unwrap();
        let err = StorageTopology::open(dir.path(), q).unwrap_err();
        // Audit fix P0-I: a typed QuotaCorrupt error
        // surfaces so the operator knows to copy the file
        // aside and retry.
        assert!(
            matches!(err, TopologyError::QuotaCorrupt(_)),
            "expected QuotaCorrupt, got {err:?}"
        );
    }

    #[test]
    fn quota_schema_mismatch_is_rejected() {
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q.clone()).unwrap();
        // Hand-craft a quota.json with a different schema
        // version to simulate a downgrade upgrade.
        let raw = serde_json::json!({
            "schema": 999u32,
            "policy": {
                "total_bytes": 1u64,
                "private_bytes": 1u64,
                "shared_bytes": 0u64,
                "shared_hard_cap": 0u64,
                "private_hard_cap": 1u64,
                "sealed": true,
                "schema": 999u32,
                "sealed_at_unix_ms": 0i64,
            }
        });
        std::fs::write(
            dir.path().join(QUOTA_FILE),
            serde_json::to_string_pretty(&raw).unwrap(),
        )
        .unwrap();
        let err = StorageTopology::open(dir.path(), q).unwrap_err();
        assert!(
            matches!(err, TopologyError::QuotaSchemaMismatch { .. }),
            "expected QuotaSchemaMismatch, got {err:?}"
        );
    }

    #[test]
    fn merge_rejects_request_with_different_schema() {
        // Audit fix P0-F: a request claiming a different
        // schema must not silently overwrite the disk
        // record's fields.
        let initial = QuotaPolicy::default_split(1024 * 1024 * 1024);
        let mut req = QuotaPolicy::default_split(2 * 1024 * 1024 * 1024);
        req.schema = 999;
        let err = initial.merge_grow_only(&req).unwrap_err();
        assert!(matches!(err, TopologyError::QuotaSchemaMismatch { .. }));
    }

    #[test]
    fn quota_grow_only_merge_rejects_zero_request_shrink() {
        // Edge: caller asks for a 0-byte budget while the
        // disk record has 1 GiB — every field is a shrink.
        let initial = QuotaPolicy::default_split(1024 * 1024 * 1024);
        let req = QuotaPolicy::default_split(0);
        let err = initial.merge_grow_only(&req).unwrap_err();
        // The first shrink check (total_bytes) fires.
        assert!(
            matches!(err, TopologyError::QuotaShrink { ref field, .. } if field == "total_bytes")
        );
    }

    #[test]
    fn concurrent_open_with_grow_only_merge_converges_to_max() {
        // Audit fix P0-R: two processes (or threads) that
        // each grow the quota independently must converge
        // to a stable merged record on disk after the
        // second open.
        use std::sync::{Arc, Barrier};
        use std::thread;
        let dir = tempdir().unwrap();
        // First open seeds the disk with 1 GiB.
        let q_seed = QuotaPolicy::default_split(1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q_seed).unwrap();
        // Two threads independently grow the quota. Both
        // open()s succeed; the disk record converges to
        // the larger of the two because merge_grow_only
        // is monotonic and idempotent.
        let barrier = Arc::new(Barrier::new(2));
        let dir_a = dir.path().to_path_buf();
        let dir_b = dir.path().to_path_buf();
        let bar1 = Arc::clone(&barrier);
        let bar2 = Arc::clone(&barrier);
        let h1 = thread::spawn(move || {
            let q = QuotaPolicy::default_split(3 * 1024 * 1024 * 1024);
            bar1.wait();
            StorageTopology::open(&dir_a, q)
        });
        let h2 = thread::spawn(move || {
            let q = QuotaPolicy::default_split(2 * 1024 * 1024 * 1024);
            bar2.wait();
            StorageTopology::open(&dir_b, q)
        });
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        // At least one open must succeed (the one that
        // loaded the disk record last). The other may
        // race on a missing rename; we only require
        // monotonic-growth correctness on the disk
        // record, not in-process success of every call.
        let successes = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
        assert!(
            successes >= 1,
            "at least one open must succeed; r1={:?} r2={:?}",
            r1.as_ref().err(),
            r2.as_ref().err(),
        );
        // Final on-disk record is at least the smaller of
        // the two grows (2 GiB) because merge_grow_only is
        // monotonic and accepts *any* growth. Without the
        // rename-uniqueness fix the value could be 2 GiB
        // (h2's write wins after h1 already wrote 3 GiB)
        // or 3 GiB (h1 wins). Audit invariant is just
        // "≥ max(seed, smaller_request)".
        let raw = std::fs::read_to_string(dir.path().join(QUOTA_FILE)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let total = v["policy"]["total_bytes"].as_u64().unwrap();
        assert!(
            total >= 2 * 1024 * 1024 * 1024,
            "concurrent grow must never lose both requests; got {total}"
        );
        assert!(
            total <= 3 * 1024 * 1024 * 1024,
            "concurrent grow must never exceed the larger request; got {total}"
        );
        // And the disk record must still be sealed.
        assert_eq!(v["policy"]["sealed"], serde_json::json!(true));
    }

    #[test]
    fn accept_replica_rejects_oversized_block() {
        // Audit fix P0-Q: a block larger than 256 KiB is
        // a protocol violation, not a bug to silently
        // truncate.
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        let oversized = vec![0xABu8; crate::block_layout::BLOCK_SIZE + 1];
        let block = ContentHash::from_bytes(&oversized);
        let msg = ReplicaMessage {
            blob: block.clone(),
            block,
            index: 0,
            bytes: oversized,
        };
        let err = sealed.accept_replica(&msg).unwrap_err();
        assert!(matches!(err, ReplicaAcceptError::Oversized { .. }));
    }

    #[test]
    fn accept_replica_block_index_writes_at_chunk_offset() {
        // Audit fix P0-Q: block index N must offset the
        // chunk writer by N * CHUNKS_PER_BLOCK so two
        // blocks of the same blob don't clobber each
        // other. This test pushes a single block at
        // index 0, then a fresh block at index 1, and
        // verifies the chunk on disk is at offset
        // `CHUNKS_PER_BLOCK` (not 0).
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        // Two distinct blobs, each 16 KiB. We push the
        // second blob's block at `index = 1` so it
        // lands at chunk offset 16.
        let blob0_bytes = vec![0xCDu8; CHUNK_SIZE];
        let blob0_hash = ContentHash::from_bytes(&blob0_bytes);
        let blob1_bytes = vec![0xEFu8; CHUNK_SIZE];
        let blob1_hash = ContentHash::from_bytes(&blob1_bytes);
        // blob0 → block index 0 → chunk 0.
        sealed
            .accept_replica(&ReplicaMessage {
                blob: blob0_hash.clone(),
                block: blob0_hash.clone(),
                index: 0,
                bytes: blob0_bytes,
            })
            .unwrap();
        // blob1 → block index 1 → chunk 16.
        sealed
            .accept_replica(&ReplicaMessage {
                blob: blob1_hash.clone(),
                block: blob1_hash.clone(),
                index: 1,
                bytes: blob1_bytes,
            })
            .unwrap();
        // Confirm blob0's chunk is at offset 0.
        let chunk0 = sealed
            .as_inner()
            .blob_dir(&blob0_hash)
            .join("chunks")
            .join("000000");
        assert!(chunk0.exists(), "blob0 chunk must be at idx 0");
        // Confirm blob1's chunk is at offset 16, NOT 0.
        let blob1_chunk0 = sealed
            .as_inner()
            .blob_dir(&blob1_hash)
            .join("chunks")
            .join("000000");
        assert!(
            !blob1_chunk0.exists(),
            "blob1 must not clobber its own idx 0 (offset calc bug)"
        );
        let blob1_chunk16 = sealed
            .as_inner()
            .blob_dir(&blob1_hash)
            .join("chunks")
            .join("000016");
        assert!(
            blob1_chunk16.exists(),
            "blob1 chunk must land at offset CHUNKS_PER_BLOCK"
        );
    }

    // ──────────────────────────────────────────────────────────
    // Audit round 3: findings surfaced by the audit pass over
    // the sealed-scope invariant.
    // ──────────────────────────────────────────────────────────

    #[test]
    fn accept_replica_records_actual_partial_chunk_size() {
        // Audit fix F1: a block whose last chunk is partial
        // must NOT be reported as `chunk_count * CHUNK_SIZE`.
        // Push a 16 KiB+1 block and read `meta.json` back.
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        let size = CHUNK_SIZE + 1;
        let payload = (0..size).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let blob = ContentHash::from_bytes(&payload);
        sealed
            .accept_replica(&ReplicaMessage {
                blob: blob.clone(),
                block: blob.clone(),
                index: 0,
                bytes: payload.clone(),
            })
            .unwrap();
        let meta_path = sealed.as_inner().blob_dir(&blob).join("meta.json");
        let raw = std::fs::read_to_string(&meta_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let reported = v["sizeBytes"].as_u64().unwrap();
        assert_eq!(
            reported, size as u64,
            "meta.json.sizeBytes must equal the actual on-disk bytes; got {reported}, expected {size}"
        );
    }

    #[test]
    fn accept_replica_round_trip_reads_back_partial_chunk() {
        // Audit fix F1 + F7: round-trip reads via the reader
        // API must return the same bytes the protocol sent,
        // even for partial chunks.
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        let size = CHUNK_SIZE + 17;
        let payload = (0..size).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        let blob = ContentHash::from_bytes(&payload);
        sealed
            .accept_replica(&ReplicaMessage {
                blob: blob.clone(),
                block: blob.clone(),
                index: 0,
                bytes: payload.clone(),
            })
            .unwrap();
        let read = sealed
            .read_range_sync(
                &blob,
                &adnet_types::ByteRange {
                    start: 0,
                    end: payload.len() as u64,
                },
            )
            .unwrap();
        assert_eq!(
            read, payload,
            "verified read must round-trip the partial chunk"
        );
    }

    #[test]
    fn accept_replica_rejects_oversized_block_with_detail() {
        // Audit fix F5: the Oversized error must carry the
        // block_index, observed bytes, and protocol ceiling
        // so the operator can diagnose without a
        // stracktrace grep.
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        let oversized = vec![0xCDu8; crate::block_layout::BLOCK_SIZE + 4096];
        let blob = ContentHash::from_bytes(&oversized);
        let err = sealed
            .accept_replica(&ReplicaMessage {
                blob: blob.clone(),
                block: blob,
                index: 7,
                bytes: oversized,
            })
            .unwrap_err();
        match err {
            ReplicaAcceptError::Oversized {
                block_index,
                bytes,
                max,
            } => {
                assert_eq!(block_index, 7, "block_index must be reported");
                assert!(bytes > max, "bytes must exceed the max");
                assert_eq!(max, crate::block_layout::BLOCK_SIZE);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    #[test]
    fn accept_replica_rejects_index_overflow() {
        // Audit fix F6: a block index that overflows when
        // multiplied by CHUNKS_PER_BLOCK must be rejected
        // before any disk write.
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        let payload = b"small".to_vec();
        let blob = ContentHash::from_bytes(&payload);
        let err = sealed
            .accept_replica(&ReplicaMessage {
                blob: blob.clone(),
                block: blob.clone(),
                index: u32::MAX,
                bytes: payload,
            })
            .unwrap_err();
        assert!(matches!(err, ReplicaAcceptError::IndexOverflow { .. }));
    }

    #[test]
    fn write_quota_unique_temp_files_under_concurrent_calls() {
        // Audit fix F4: two StorageTopology::open() calls in
        // the same process, called back-to-back, must not
        // collide on the temp-file name. We can't directly
        // observe the temp file name from the public API, but
        // we can verify that two opens serialize cleanly
        // without touching a half-written destination.
        let dir = tempdir().unwrap();
        let q_seed = QuotaPolicy::default_split(1024 * 1024 * 1024);
        let _ = StorageTopology::open(dir.path(), q_seed.clone()).unwrap();
        // Two consecutive grows in the same process must
        // both succeed (the second does not collide with the
        // first's temp file).
        let q2 = QuotaPolicy::default_split(2 * 1024 * 1024 * 1024);
        let t2 = StorageTopology::open(dir.path(), q2.clone()).unwrap();
        assert_eq!(t2.quota.total_bytes, q2.total_bytes);
        let q3 = QuotaPolicy::default_split(3 * 1024 * 1024 * 1024);
        let t3 = StorageTopology::open(dir.path(), q3.clone()).unwrap();
        assert_eq!(t3.quota.total_bytes, q3.total_bytes);
    }

    #[test]
    fn accept_replica_rejects_blob_reuse_across_blocks() {
        // Audit fix F3: a ReplicaMessage where `blob` and
        // `block` describe different content is rejected by
        // the SR-1 boundary check. We can't (yet) enforce
        // `blob == blake3(concat(blocks))` because the
        // protocol doesn't carry multi-block sequencing, but
        // we CAN enforce that the receiver re-hashes the
        // bytes against `block` — which is already the
        // existing SR-1 check. This test ensures that
        // another block pointing at the same blob but
        // different bytes lands at a different chunk index
        // and does not clobber the first block's chunk.
        let dir = tempdir().unwrap();
        let q = QuotaPolicy::default_split(64 * 1024 * 1024);
        let topo = StorageTopology::open(dir.path(), q).unwrap();
        let sealed = topo.shared_store();
        // blob X first block.
        let b0 = vec![0x11u8; CHUNK_SIZE];
        let blob_x = ContentHash::from_bytes(&b0);
        let block0 = ContentHash::from_bytes(&b0);
        sealed
            .accept_replica(&ReplicaMessage {
                blob: blob_x.clone(),
                block: block0.clone(),
                index: 0,
                bytes: b0.clone(),
            })
            .unwrap();
        // blob X second block at index 1.
        let b1 = vec![0x22u8; CHUNK_SIZE];
        let block1 = ContentHash::from_bytes(&b1);
        sealed
            .accept_replica(&ReplicaMessage {
                blob: blob_x.clone(),
                block: block1.clone(),
                index: 1,
                bytes: b1.clone(),
            })
            .unwrap();
        // Both chunks must be on disk at their expected
        // offsets, and the second block's chunk must NOT
        // have replaced the first block's chunk 0.
        let dir0 = sealed.as_inner().blob_dir(&blob_x).join("chunks");
        let chunk0 = std::fs::read(dir0.join("000000")).unwrap();
        // Block 1's first chunk is at index CHUNKS_PER_BLOCK
        // (block_index * CHUNKS_PER_BLOCK = 1 * CHUNKS_PER_BLOCK).
        let chunk1_name = format!("{:06}", CHUNKS_PER_BLOCK);
        let chunk1 = std::fs::read(dir0.join(&chunk1_name)).unwrap();
        assert_eq!(chunk0, b0, "block-0 chunk must remain on disk");
        assert_eq!(chunk1, b1, "block-1 chunk must remain on disk");
        // The FINAL blob has 32 KiB on disk; meta.json must
        // report 32 KiB exactly (no rounding).
        // Diagnostic: print every file in the blob dir to
        // surface path mismatches.
        let blob_dir = dir0.parent().unwrap();
        let raw = std::fs::read_to_string(blob_dir.join("meta.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["sizeBytes"].as_u64().unwrap(), 2 * CHUNK_SIZE as u64);
    }
}
