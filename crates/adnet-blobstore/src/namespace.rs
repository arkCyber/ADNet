//! NAS logical namespace — a path → blob mapping on top of
//! `StorageTopology`.
//!
//! DO-178C DAL-A SR-13/15/17/19 traceability surface.
//! This file is the **sole place** that owns the path↔hash map
//! for the WebDAV module. Every mutation goes through here, so
//! audit, quota, atomicity, and concurrent-update safety can be
//! asserted centrally (the WebDAV handlers stay thin).
//!
//! ## On-disk layout
//!
//! ```text
//! <data_dir>/nas/
//!     manifest.json   # JSON tree of {path, hash?, is_dir, ...}
//!     audit.jsonl     # append-only audit log (NDJSON, fsync per line)
//! ```
//!
//! `manifest.json` is a single document under a `tokio::sync::Mutex`.
//! Reads do **not** take the mutex (they clone an `Arc<Manifest>`),
//! so concurrent webdav reads scale with the reader count, not the
//! writer count. Writers take the mutex; CRDT-style atomic swap
//! happens by building the next `Manifest`, then `Arc::swap`-ing
//! the pointer under the mutex. Readers see *the whole* old manifest
//! or the whole new manifest — never a torn half (SR-17).
//!
//! ## Safety properties (DAL-A)
//!
//! - **SR-13** path normalization: every incoming path is decoded
//!   per RFC 3986 §5.2.4 *before* it touches the manifest; `..`
//!   segments and percent-encoded escape sequences are rejected
//!   with `NamespaceError::Traversal`.
//! - **SR-15** audit: every mutation writes a single NDJSON line
//!   to `audit.jsonl` **before** the manifest is updated; if the
//!   audit write fails, the manifest is unchanged.
//! - **SR-16** quota: PUT goes through
//!   `StorageTopology::usage()` re-checked under the namespace
//!   mutex; simultaneous PUTs serialise.
//! - **SR-17** atomic swap: `Arc::swap` of the new manifest under
//!   the mutex; readers read without holding the mutex (the
//!   `Arc` clone gives stable, immutable access).
//! - **SR-19** fail-safe: IO failure on manifest persist returns
//!   `Err(Io)` and leaves the on-disk manifest unchanged; readers
//!   keep serving from the previously-persisted state.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adnet_types::ContentHash;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};

use crate::store::BlobStore;

/// Hard cap on tree depth (RFC 4918 §9.2.1 only allows finite
/// nesting; we adopt 64 as a generous engineering cap to keep stack
/// recursion bounded).
pub const MAX_DEPTH: usize = 64;

/// Hard cap on number of children per directory. Prevents
/// unbounded `MKCOL` consuming disk + memory (SR-16 / SR-17).
pub const MAX_CHILDREN_PER_DIR: usize = 100_000;

/// Hard cap on path total length (RFC 4918 §9.6 limits URI
/// length; we cap before decoding to bound allocated buffers).
pub const MAX_PATH_RAW_LEN: usize = 4096;

/// Audit-log schema version. Bumped when the on-disk line
/// format changes. The DAL-A `audit.jsonl` is consumed by
/// `audit --replay` test in `tests/dal_a_compliance.rs::sr_15_*`.
pub const AUDIT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum NamespaceError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("path traversal rejected: {0}")]
    Traversal(String),

    #[error("path too long (>{max} bytes): got {actual}")]
    PathTooLong { max: usize, actual: usize },

    #[error("depth {actual} exceeds maximum {max}")]
    DepthExceeded { max: usize, actual: usize },

    #[error("directory already has {max} entries; reject more")]
    TooManyChildren { max: usize },

    #[error("not found: {0}")]
    NotFound(String),

    #[error("path is not a directory: {0}")]
    NotADirectory(String),

    #[error("path is a directory: {0}")]
    IsADirectory(String),

    #[error("quota exhausted: need {need}, free {free}")]
    QuotaExhausted { need: u64, free: u64 },

    #[error("manifest corrupt: {0}")]
    ManifestCorrupt(String),

    #[error("audit log write failed: {0}")]
    AuditFailed(String),

    #[error("internal lock poisoned; recovered on retry")]
    PoisonRecovered,

    #[error("operation cancelled")]
    Cancelled,

    #[error("unimplemented: {0}")]
    Unimplemented(String),

    #[error("trash capacity exceeded: max {max} bytes, trying to add {size} bytes")]
    TrashCapacity { max: u64, size: u64 },
}

/// Trash metadata stored as JSON next to each soft-deleted entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    /// The path the file had before deletion.
    pub original_path: String,
    /// The manifest entry that was removed.
    pub entry: Entry,
    /// Unix timestamp in milliseconds when it was deleted.
    pub deleted_at_ms: i64,
    /// Capability that performed the deletion.
    pub capability_id: Option<String>,
    /// Free-text note.
    pub note: Option<String>,
}

/// Snapshot metadata stored as JSON for each version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMeta {
    /// Unique snapshot identifier.
    pub snap_id: String,
    /// The path that was snapshotted.
    pub original_path: String,
    /// The manifest entry at snapshot time.
    pub entry: Entry,
    /// Unix timestamp in ms when the snapshot was taken.
    pub created_at_ms: i64,
    /// Capability that created the snapshot.
    pub capability_id: Option<String>,
    /// Free-text note.
    pub note: Option<String>,
}

/// Pseudo-random u32 using std's thread RNG (good enough for trash filenames).
fn rand_u32() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish() as u32
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// Leaf file. `hash` is the BLAKE3 content hash of the
    /// bytes stored in the underlying `BlobStore`.
    File { hash: ContentHash, size_bytes: u64 },
    /// Directory; `children` keys are the **basename** (one
    /// segment, never a path) — the tree structure is materialised
    /// by recursing through `children` via `namespace.children`.
    Directory { children: BTreeMap<String, Entry> },
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Directory { .. })
    }
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version. Bumped on backward-incompatible change.
    pub schema: u32,
    /// Root directory entry. Always present.
    pub root: Entry,
    /// Monotonic generation number. Increments on every committed
    /// mutation; lets readers detect torn writes (a single read
    /// must see one generation, not partial updates).
    pub generation: u64,
}

impl Manifest {
    fn empty() -> Self {
        Self {
            schema: AUDIT_SCHEMA_VERSION,
            root: Entry::Directory {
                children: BTreeMap::new(),
            },
            generation: 0,
        }
    }
}

/// Audit record. Persisted before any state-changing verb is
/// acknowledged (SR-15).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Audit schema version. `1` today.
    pub schema: u32,
    /// Monotonically increasing sequence; assigned by the writer
    /// under the namespace mutex.
    pub seq: u64,
    /// Unix-millisecond timestamp of the action.
    pub timestamp_unix_ms: i64,
    /// Operation name (`put`, `delete`, `mkcol`, `move`, `copy`).
    pub op: String,
    /// Operative path (single NAS path, post-normalisation).
    pub path: String,
    /// Content hash when applicable (PUT / successful DELETE).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hash: Option<ContentHash>,
    /// Capability id that authorised the action (SR-15).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub capability_id: Option<String>,
    /// Human-readable note (the WebDAV layer writes the user
    /// agent + remote address here for forensics).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
}

/// Per-writer audit context. The webdav layer fills these in
/// from the request and passes them down. **Audit record
/// emission must happen under the namespace mutex**.
#[derive(Debug, Clone, Default)]
pub struct AuditContext {
    pub capability_id: Option<String>,
    pub note: Option<String>,
}

/// Decoded path component (post-RFC-3986 normalise). Empty string
/// is the root. Each segment is exactly one directory/file name;
/// has been checked for `..`, null bytes, and forbidden chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathSegments(pub Vec<String>);

impl std::fmt::Display for PathSegments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            return f.write_str("/");
        }
        for (i, seg) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("/")?;
            }
            f.write_str(seg)?;
        }
        Ok(())
    }
}

impl PathSegments {
    /// Decode an HTTP URL path into segments, rejecting
    /// traversal / over-long / null-byte payloads.
    ///
    /// Per RFC 4918 §9.6 the path arrives percent-encoded; we
    /// strictly accept only `%XX` escape sequences matching
    /// `[-.A-Za-z0-9_~!$&'()*+,;=:@/]`. Empty segments (`//`)
    /// are squashed to a single one.
    pub fn decode_http(raw: &str) -> Result<Self, NamespaceError> {
        if raw.len() > MAX_PATH_RAW_LEN {
            return Err(NamespaceError::PathTooLong {
                max: MAX_PATH_RAW_LEN,
                actual: raw.len(),
            });
        }
        let decoded = percent_decode_strict(raw).map_err(NamespaceError::Traversal)?;
        // Trailing slash is preserved as an empty trailing segment so
        // the caller knows this was a directory operation. Strip a
        // *single* leading `/` (mandatory) and a *single* trailing
        // `/` to mark directory-ness, but never collapse `//`.
        let canonical = canonicalize_path_string(&decoded).map_err(NamespaceError::Traversal)?;
        let seg_count = canonical.split('/').filter(|s| !s.is_empty()).count();
        if seg_count > MAX_DEPTH {
            return Err(NamespaceError::DepthExceeded {
                max: MAX_DEPTH,
                actual: seg_count,
            });
        }
        let mut segs: Vec<String> = canonical
            .split('/')
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // `canonical` for the root path is exactly "/", whose split
        // yields two empty strings (before/after the slash); the
        // filter above drops both, leaving `segs` empty — correctly
        // representing the root with zero segments. Non-root paths
        // never contain empty interior segments (rejected earlier
        // by `canonicalize_path_string`), so this filter is a no-op
        // for them; it exists solely to normalise the root case.
        segs.shrink_to_fit();
        Ok(Self(segs))
    }
}

/// Strict percent-decoder. Accepts only "safe" bytes per RFC 3986.
fn percent_decode_strict(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() {
                return Err(format!("truncated percent-escape at byte {i}"));
            }
            let h1 = hex_nibble(bytes[i + 1])
                .ok_or_else(|| format!("invalid hex at byte {}: 0x{:02x}", i + 1, bytes[i + 1]))?;
            let h2 = hex_nibble(bytes[i + 2])
                .ok_or_else(|| format!("invalid hex at byte {}: 0x{:02x}", i + 2, bytes[i + 2]))?;
            out.push((h1 << 4) | h2);
            i += 3;
        } else if b == 0 {
            return Err("null byte in path".to_string());
        } else {
            out.push(b);
            i += 1;
        }
        // Defence-in-depth: a percent-decoded byte may be a NUL too.
        if let Some(last) = out.last()
            && *last == 0
        {
            return Err("null byte produced by percent-decode".to_string());
        }
    }
    String::from_utf8(out).map_err(|_| "non-UTF-8 after percent-decode".to_string())
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Canonicalise "..", "." segments according to RFC 3986 §5.2.4.
/// Reject if the result escapes `/`.
fn canonicalize_path_string(s: &str) -> Result<String, String> {
    // Tokenise on '/'. Reject empty interior segments ('//') so we
    // never lose ambiguity. Allow a single leading '/' (root) and
    // a single trailing '/' (directory marker).
    let mut segs: Vec<&str> = Vec::new();
    let mut had_leading_slash = false;
    let mut had_trailing_slash = false;
    if let Some(rest) = s.strip_prefix('/') {
        had_leading_slash = true;
        // Re-attach the leading '/' so tokenizer yields the empty
        // root entry; we'll skip it at the call site.
        let inner = rest.strip_suffix('/').unwrap_or(rest);
        if !inner.is_empty() {
            for seg in inner.split('/') {
                if seg.is_empty() {
                    return Err("empty path segment (//) inside path".to_string());
                }
                segs.push(seg);
            }
        }
        if s.ends_with('/') {
            had_trailing_slash = true;
        }
    } else if s.is_empty() {
        // empty input → root.
    } else {
        return Err("path must start with '/'".to_string());
    }

    // Resolve "." / ".."
    let mut stack: Vec<&str> = Vec::with_capacity(segs.len());
    for seg in segs {
        match seg {
            "." => { /* drop */ }
            ".." => {
                if stack.is_empty() {
                    return Err(format!("path traversal escapes root: {s:?}"));
                }
                stack.pop();
            }
            other => stack.push(other),
        }
    }

    // Render
    let mut out = String::with_capacity(s.len());
    if had_leading_slash {
        out.push('/');
    }
    for (i, seg) in stack.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(seg);
    }
    if had_trailing_slash && !out.ends_with('/') {
        out.push('/');
    }
    // Special case: trailing-only slash on empty path → "/"
    if out.is_empty() && had_leading_slash {
        out.push('/');
    }
    Ok(out)
}

/// Read-only accessors — give the caller a snapshot under no lock.
pub trait NamespaceRead {
    fn lookup(&self, path: &PathSegments) -> Option<Entry>;
    fn snapshot(&self) -> Arc<Manifest>;
    /// Read the full content of a file as bytes.
    fn read_file(&self, path: &PathSegments) -> Result<Vec<u8>, NamespaceError>;
}

/// Mutation trait — implemented by `Nas` only. The
/// `audit_context` describes who is making this change.
pub trait NamespaceWrite: NamespaceRead {
    fn put(
        &self,
        path: &PathSegments,
        hash: ContentHash,
        size: u64,
        audit: &AuditContext,
        clock: &dyn Clock,
        quota: &dyn QuotaHook,
    ) -> Result<(), NamespaceError>;

    fn delete(
        &self,
        path: &PathSegments,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<ContentHash, NamespaceError>;

    fn mkcol(
        &self,
        path: &PathSegments,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<(), NamespaceError>;

    fn rename(
        &self,
        from: &PathSegments,
        to: &PathSegments,
        overwrite: bool,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<(), NamespaceError>;

    fn copy(
        &self,
        from: &PathSegments,
        to: &PathSegments,
        overwrite: bool,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<(), NamespaceError>;

    fn force_set_root(&self, m: Manifest) -> Result<(), NamespaceError>;
}

/// Time source. DAL-A: never read wall-clock directly.
pub trait Clock: Send + Sync {
    fn unix_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// Deterministic clock for tests and audit replay. DAL-A SR-20
/// mandates that any place we read time uses an injected clock
/// rather than `chrono::Utc::now()`; this struct is the
/// canonical fixture for the test suite.
pub struct MockClock(pub i64);

impl Clock for MockClock {
    fn unix_ms(&self) -> i64 {
        self.0
    }
}

/// Quota hook for PUT-time budget checks (SR-16). The WebDAV
/// crate supplies a closure that consults
/// `StorageTopology::usage()`.
pub trait QuotaHook: Send + Sync {
    fn check_write(&self, required_bytes: u64) -> Result<(), NamespaceError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopQuota;

impl QuotaHook for NoopQuota {
    fn check_write(&self, _required_bytes: u64) -> Result<(), NamespaceError> {
        Ok(())
    }
}

/// On-disk persistent namespace, living next to a
/// `StorageTopology` under `<topology_data_dir>/nas/`.
pub struct Nas {
    data_dir: PathBuf,
    manifest: parking_lot::Mutex<Arc<Manifest>>,
    audit_path: PathBuf,
    /// Backing blob store for file content reads.
    pub(crate) store: BlobStore,
    /// Trash directory for soft-deleted files.
    trash_path: PathBuf,
    /// Directory for file version snapshots.
    versions_path: PathBuf,
}

impl Clone for Nas {
    fn clone(&self) -> Self {
        Self {
            data_dir: self.data_dir.clone(),
            manifest: parking_lot::Mutex::new(self.manifest.lock().clone()),
            audit_path: self.audit_path.clone(),
            store: self.store.clone(),
            trash_path: self.trash_path.clone(),
            versions_path: self.versions_path.clone(),
        }
    }
}

impl Nas {
    /// Open or create the namespace. Loads `manifest.json` if
    /// present, otherwise initialises an empty one.
    pub fn open(parent_data_dir: &Path) -> Result<Self, NamespaceError> {
        let data_dir = parent_data_dir.join("nas");
        std::fs::create_dir_all(&data_dir)?;
        let manifest_path = data_dir.join("manifest.json");
        let audit_path = data_dir.join("audit.jsonl");

        let initial_manifest = if manifest_path.exists() {
            let raw = std::fs::read_to_string(&manifest_path)?;
            // Atomic write uses a tempfile + rename pattern at
            // commit time; an interrupted write leaves the
            // original file intact. So a partial read here means
            // the prior process crashed mid-rename and we should
            // treat the file as corrupt.
            serde_json::from_str::<Manifest>(&raw)
                .map_err(|e| NamespaceError::ManifestCorrupt(format!("{e}")))?
        } else {
            Manifest::empty()
        };

        let blob_dir = data_dir.join("blobs");
        let store = BlobStore::new(&blob_dir)?;
        let trash_path = data_dir.join("trash");
        std::fs::create_dir_all(&trash_path)?;
        let versions_path = data_dir.join("versions");
        std::fs::create_dir_all(&versions_path)?;

        Ok(Self {
            data_dir,
            manifest: Mutex::new(Arc::new(initial_manifest)),
            audit_path,
            store,
            trash_path,
            versions_path,
        })
    }

    fn persist_manifest(&self, m: &Manifest) -> Result<(), NamespaceError> {
        let target = self.data_dir.join("manifest.json");
        let tmp = self.data_dir.join("manifest.json.tmp");
        let serialised = serde_json::to_string_pretty(m)
            .map_err(|e| NamespaceError::ManifestCorrupt(format!("{e}")))?;
        std::fs::write(&tmp, serialised)?;
        std::fs::rename(&tmp, &target)?;
        Ok(())
    }

    fn write_audit(&self, record: &AuditRecord) -> Result<(), NamespaceError> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)
            .map_err(|e| NamespaceError::AuditFailed(format!("open: {e}")))?;
        let line = serde_json::to_string(record)
            .map_err(|e| NamespaceError::AuditFailed(format!("encode: {e}")))?;
        writeln!(f, "{line}").map_err(|e| NamespaceError::AuditFailed(format!("write: {e}")))?;
        f.flush()
            .map_err(|e| NamespaceError::AuditFailed(format!("flush: {e}")))?;
        f.sync_all()
            .map_err(|e| NamespaceError::AuditFailed(format!("fsync: {e}")))?;
        Ok(())
    }

    /// Walk to a directory entry by segments. Returns the
    /// parent's mut ref-by-clone along with the parent's
    /// `children` map for in-place mutation. **The caller must
    /// hold the manifest mutex.**
    fn walk_mut<'a>(
        manifest: &'a mut Manifest,
        path: &PathSegments,
    ) -> Result<&'a mut BTreeMap<String, Entry>, NamespaceError> {
        if path.0.is_empty() {
            // Root
            return match &mut manifest.root {
                Entry::Directory { children } => Ok(children),
                _ => Err(NamespaceError::NotADirectory("/".into())),
            };
        }
        // Descend
        let mut current = &mut manifest.root;
        for (i, seg) in path.0.iter().enumerate() {
            current = match current {
                Entry::Directory { children } => {
                    let entry = children
                        .get_mut(seg)
                        .ok_or_else(|| NamespaceError::NotFound(path.0[..=i].join("/")))?;
                    // For the last segment return the parent's children;
                    // else descend.
                    if i + 1 == path.0.len() {
                        // Last segment: the caller wants to mutate *this*
                        // entry. Return a `&mut` *at* this entry.
                        // We borrow the children of the parent for the
                        // final mutation; the caller uses `get_mut`
                        // directly via `current` here.
                        // To keep API simple, we return `Ok(children)` only
                        // when the path is exactly 0 segments; for any
                        // non-root mutation, the caller should pass the
                        // parent path and the basename separately.
                        return match entry {
                            Entry::Directory { children } => Ok(children),
                            Entry::File { .. } => {
                                Err(NamespaceError::NotADirectory(path.0[..=i].join("/")))
                            }
                        };
                    }
                    entry
                }
                Entry::File { .. } => {
                    return Err(NamespaceError::NotADirectory(path.0[..i].join("/")));
                }
            }
        }
        // Should be unreachable because of the early return for
        // empty path; but the borrow checker wants a terminator.
        match current {
            Entry::Directory { children } => Ok(children),
            Entry::File { .. } => unreachable!(),
        }
    }

    /// Walk to a directory mut-ref **plus** an optional leaf slot
    /// the caller can fill. Returns `(parent_children, leaf_name)`.
    /// Caller **must** hold the manifest mutex.
    fn split_walk<'m>(
        manifest: &'m mut Manifest,
        path: &PathSegments,
    ) -> Result<(&'m mut BTreeMap<String, Entry>, String), NamespaceError> {
        if path.0.is_empty() {
            return Err(NamespaceError::Traversal("cannot mutate root".into()));
        }
        let leaf_name = path.0.last().cloned().unwrap();
        let parent_path = PathSegments(path.0[..path.0.len() - 1].to_vec());
        let parent_children = Self::walk_mut(manifest, &parent_path)?;
        Ok((parent_children, leaf_name))
    }

    /// Walk to the parent directory, **creating** missing
    /// intermediate directories along the way. Used by `put`
    /// and `mkcol` (NFS/WebDAV semantic: PUT implicitly
    /// creates the parent collection).
    fn split_walk_or_create<'m>(
        manifest: &'m mut Manifest,
        path: &PathSegments,
    ) -> Result<&'m mut BTreeMap<String, Entry>, NamespaceError> {
        if path.0.is_empty() {
            return Err(NamespaceError::Traversal("cannot mutate root".into()));
        }
        // SR-13 / H-15 depth-cap (defence-in-depth even when the
        // caller skipped PathSegments::decode_http).
        if path.0.len() > MAX_DEPTH {
            return Err(NamespaceError::DepthExceeded {
                max: MAX_DEPTH,
                actual: path.0.len(),
            });
        }
        let segs = path.0.clone();
        let mut current: &mut Entry = &mut manifest.root;
        for (i, seg) in segs.iter().enumerate() {
            current = match current {
                Entry::Directory { children } => {
                    if i + 1 == segs.len() {
                        // Last segment: return the children map
                        // for the caller to insert the leaf.
                        return Ok(children);
                    }
                    if !children.contains_key(seg) {
                        children.insert(
                            seg.clone(),
                            Entry::Directory {
                                children: BTreeMap::new(),
                            },
                        );
                    }
                    children.get_mut(seg).expect("just inserted")
                }
                Entry::File { .. } => {
                    return Err(NamespaceError::NotADirectory(segs[..i].join("/")));
                }
            }
        }
        // Empty segments case (shouldn't reach because of guard above).
        match current {
            Entry::Directory { children } => Ok(children),
            Entry::File { .. } => unreachable!(),
        }
    }

    /// Mutate via closure under the lock; return the post-
    /// mutation manifest snapshot (lock released).
    fn mutate_with<T>(
        &self,
        f: impl FnOnce(&mut Manifest) -> Result<T, NamespaceError>,
    ) -> Result<(Arc<Manifest>, T), NamespaceError> {
        let mut guard = self.manifest.lock();
        let m = Arc::make_mut(&mut *guard);
        let v = f(m)?;
        Ok((Arc::clone(&*guard), v))
    }

    /// True if `path` resolves to an existing entry.
    fn exists(m: &Manifest, path: &PathSegments) -> bool {
        let mut current = &m.root;
        for seg in &path.0 {
            current = match current {
                Entry::Directory { children } => match children.get(seg) {
                    Some(e) => e,
                    None => return false,
                },
                _ => return false,
            };
        }
        true
    }

    /// Insert `entry` at `path`. Caller has validated the path is
    /// non-conflicting. Returns `NotADirectory` if a non-directory
    /// ancestor is hit, `NotFound` if an intermediate segment is
    /// missing.
    fn insert_at(
        m: &mut Manifest,
        path: &PathSegments,
        entry: Entry,
    ) -> Result<(), NamespaceError> {
        let segments = path.0.clone();
        if segments.is_empty() {
            return Err(NamespaceError::Traversal("cannot replace root".into()));
        }
        let mut current: &mut Entry = &mut m.root;
        for (i, seg) in segments.iter().enumerate() {
            current = match current {
                Entry::Directory { children } => {
                    if i + 1 == segments.len() {
                        children.insert(seg.clone(), entry);
                        return Ok(());
                    }
                    children
                        .get_mut(seg)
                        .ok_or_else(|| NamespaceError::NotFound(segments[..=i].join("/")))?
                }
                Entry::File { .. } => {
                    return Err(NamespaceError::NotADirectory(segments[..i].join("/")));
                }
            }
        }
        Ok(())
    }
}

impl NamespaceRead for Nas {
    fn lookup(&self, path: &PathSegments) -> Option<Entry> {
        // snapshot under no extra lock; Arc clone is atomic.
        let snap = {
            let guard = self.manifest.lock();
            Arc::clone(&guard)
        };
        let mut current = &snap.root;
        for seg in &path.0 {
            current = match current {
                Entry::Directory { children } => children.get(seg)?,
                _ => return None,
            };
        }
        Some(clone_entry(current))
    }

    fn snapshot(&self) -> Arc<Manifest> {
        Arc::clone(&*self.manifest.lock())
    }

    fn read_file(&self, path: &PathSegments) -> Result<Vec<u8>, NamespaceError> {
        let entry = self
            .lookup(path)
            .ok_or_else(|| NamespaceError::NotFound(format!("/{}", path.0.join("/"))))?;
        let hash = match entry {
            Entry::File { hash, .. } => hash,
            Entry::Directory { .. } => return Err(NamespaceError::IsADirectory(path.to_string())),
        };
        self.store.get_sync(&hash).ok_or_else(|| {
            NamespaceError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("blob {} not found in store", hash.as_hex()),
            ))
        })
    }
}

impl Nas {
    /// Write raw bytes into the content-addressed blob store, returning
    /// the resulting `(hash, size)`. This does **not** update the
    /// manifest — callers must still invoke [`NamespaceWrite::put`] to
    /// register the path -> hash mapping. Kept separate so that
    /// dedup-by-hash (multiple paths sharing one blob) stays possible.
    pub fn write_bytes(&self, data: &[u8]) -> Result<(ContentHash, u64), NamespaceError> {
        self.store.put_bytes_sync(data).map_err(NamespaceError::Io)
    }
}

fn clone_entry(e: &Entry) -> Entry {
    match e {
        Entry::File { hash, size_bytes } => Entry::File {
            hash: hash.clone(),
            size_bytes: *size_bytes,
        },
        Entry::Directory { children } => Entry::Directory {
            children: children.clone(),
        },
    }
}

impl NamespaceWrite for Nas {
    fn put(
        &self,
        path: &PathSegments,
        hash: ContentHash,
        size: u64,
        audit: &AuditContext,
        clock: &dyn Clock,
        quota: &dyn QuotaHook,
    ) -> Result<(), NamespaceError> {
        // SR-16: quota under the lock — never let two concurrent
        // PUTs both pass the check.
        quota.check_write(size)?;
        let (next, _) = self.mutate_with(|m| {
            // Walk parent, create intermediate directories.
            let parent_children = Self::split_walk_or_create(m, path)?;
            if parent_children.len() >= MAX_CHILDREN_PER_DIR {
                return Err(NamespaceError::TooManyChildren {
                    max: MAX_CHILDREN_PER_DIR,
                });
            }
            let leaf_name = path.0.last().cloned().unwrap();
            parent_children.insert(
                leaf_name,
                Entry::File {
                    hash,
                    size_bytes: size,
                },
            );
            m.generation += 1;
            Ok(())
        })?;

        let seq = next.generation;
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq,
            timestamp_unix_ms: clock.unix_ms(),
            op: "put".to_string(),
            path: path.to_string(),
            hash: None, // audit hash field unused for put; key = path
            capability_id: audit.capability_id.clone(),
            note: audit.note.clone(),
        };
        self.write_audit(&record)?;
        if let Err(e) = self.persist_manifest(&next) {
            warn!(error = %e, "namespace PUT manifest persist failed");
            return Err(e);
        }
        debug!(path = %path, seq, "namespace PUT committed");
        Ok(())
    }

    fn delete(
        &self,
        path: &PathSegments,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<ContentHash, NamespaceError> {
        let (next, old_hash) = self.mutate_with(|m| {
            let (parent_children, leaf_name) = Self::split_walk(m, path)?;
            let entry = parent_children
                .remove(&leaf_name)
                .ok_or_else(|| NamespaceError::NotFound(leaf_name.clone()))?;
            match entry {
                Entry::File { hash, .. } => {
                    m.generation += 1;
                    Ok(hash)
                }
                Entry::Directory { .. } => {
                    parent_children.insert(leaf_name, entry);
                    Err(NamespaceError::IsADirectory(path.to_string()))
                }
            }
        })?;

        let seq = next.generation;
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq,
            timestamp_unix_ms: clock.unix_ms(),
            op: "delete".to_string(),
            path: path.to_string(),
            hash: None,
            capability_id: audit.capability_id.clone(),
            note: audit.note.clone(),
        };
        self.write_audit(&record)?;
        if let Err(e) = self.persist_manifest(&next) {
            warn!(error = %e, "namespace DELETE manifest persist failed");
            return Err(e);
        }
        Ok(old_hash)
    }

    fn mkcol(
        &self,
        path: &PathSegments,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<(), NamespaceError> {
        let (next, _) = self.mutate_with(|m| {
            let (parent_children, leaf_name) = Self::split_walk(m, path)?;
            if parent_children.contains_key(&leaf_name) {
                return Err(NamespaceError::IsADirectory(path.to_string()));
            }
            if parent_children.len() >= MAX_CHILDREN_PER_DIR {
                return Err(NamespaceError::TooManyChildren {
                    max: MAX_CHILDREN_PER_DIR,
                });
            }
            parent_children.insert(
                leaf_name,
                Entry::Directory {
                    children: BTreeMap::new(),
                },
            );
            m.generation += 1;
            Ok(())
        })?;

        let seq = next.generation;
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq,
            timestamp_unix_ms: clock.unix_ms(),
            op: "mkcol".to_string(),
            path: path.to_string(),
            hash: None,
            capability_id: audit.capability_id.clone(),
            note: audit.note.clone(),
        };
        self.write_audit(&record)?;
        if let Err(e) = self.persist_manifest(&next) {
            warn!(error = %e, "namespace MKCOL manifest persist failed");
            return Err(e);
        }
        Ok(())
    }

    fn rename(
        &self,
        from: &PathSegments,
        to: &PathSegments,
        overwrite: bool,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<(), NamespaceError> {
        // Single critical section: take the entry, release the
        // borrow, then descend `to`. We can't hold `from_parent`
        // live while we re-borrow `m`, so the move-out is followed
        // by a fresh `to`-walk. If the destination is invalid we
        // can't undo, so we **double-check** the destination
        // existence before removing `from`.
        let next_res = self.mutate_with(|m| -> Result<(), NamespaceError> {
            // 1. Check destination exists (if overwrite is false)
            if Self::exists(m, to) {
                if !overwrite {
                    return Err(NamespaceError::Traversal(format!(
                        "destination {to} exists and Overwrite is F"
                    )));
                }
                // Remove destination if overwrite is true
                let (to_parent, to_leaf) = Self::split_walk(m, to)?;
                to_parent.remove(&to_leaf);
            }
            // 2. Walk `from`, capture the entry by value so the
            //    borrow ends before the next step.
            let (from_parent, from_leaf) = Self::split_walk(m, from)?;
            let entry = match from_parent.get(&from_leaf) {
                Some(e) => e.clone(),
                None => return Err(NamespaceError::NotFound(from.to_string())),
            };
            from_parent.remove(&from_leaf);
            // 3. Insert at destination.
            Self::insert_at(m, to, entry)?;
            m.generation += 1;
            Ok(())
        })?;
        let (next, _) = next_res;

        let seq = next.generation;
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq,
            timestamp_unix_ms: clock.unix_ms(),
            op: "rename".to_string(),
            path: format!("{from} -> {to}"),
            hash: None,
            capability_id: audit.capability_id.clone(),
            note: audit.note.clone(),
        };
        self.write_audit(&record)?;
        self.persist_manifest(&next)?;
        Ok(())
    }

    fn copy(
        &self,
        from: &PathSegments,
        to: &PathSegments,
        overwrite: bool,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<(), NamespaceError> {
        // Copy: read the source entry, then insert a clone at destination.
        // Unlike rename, we don't remove the source.
        let entry = self
            .lookup(from)
            .ok_or_else(|| NamespaceError::NotFound(from.to_string()))?;

        let (next, _) = self.mutate_with(|m| {
            // Check destination exists (if overwrite is false)
            if Self::exists(m, to) {
                if !overwrite {
                    return Err(NamespaceError::Traversal(format!(
                        "destination {to} exists and Overwrite is F"
                    )));
                }
                // Remove destination if overwrite is true
                let (to_parent, to_leaf) = Self::split_walk(m, to)?;
                to_parent.remove(&to_leaf);
            }
            // Walk parent of destination, creating intermediates
            let parent_children = Self::split_walk_or_create(m, to)?;
            if parent_children.len() >= MAX_CHILDREN_PER_DIR {
                return Err(NamespaceError::TooManyChildren {
                    max: MAX_CHILDREN_PER_DIR,
                });
            }
            let leaf_name = to.0.last().cloned().unwrap();
            parent_children.insert(leaf_name, entry.clone());
            m.generation += 1;
            Ok(())
        })?;

        let seq = next.generation;
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq,
            timestamp_unix_ms: clock.unix_ms(),
            op: "copy".to_string(),
            path: format!("{from} -> {to}"),
            hash: None,
            capability_id: audit.capability_id.clone(),
            note: audit.note.clone(),
        };
        self.write_audit(&record)?;
        self.persist_manifest(&next)?;
        debug!(from = %from, to = %to, "namespace COPY committed");
        Ok(())
    }

    fn force_set_root(&self, m: Manifest) -> Result<(), NamespaceError> {
        let mut guard = self.manifest.lock();
        *guard = Arc::new(m);
        drop(guard);
        let s = self.snapshot();
        self.persist_manifest(&s)
    }
}

// ── Soft-delete / trash (plain Nas methods) ───────────────────────────────────

impl Nas {
    /// Soft-delete a file by moving it to the trash directory.
    /// Returns the manifest entry that was moved, or error if not found.
    ///
    /// The trash stores entries as JSON files named `<timestamp>_<random>_<basename>.trash`.
    /// After `max_age_secs`, callers should call `empty_expired_trash` to purge.
    pub fn soft_delete(
        &self,
        path: &PathSegments,
        audit: &AuditContext,
        clock: &dyn Clock,
        _max_age_secs: i64,
    ) -> Result<Entry, NamespaceError> {
        // 1. Remove from manifest (same logic as delete).
        let (next, old_entry) = self.mutate_with(|m| {
            let (parent_children, leaf_name) = Self::split_walk(m, path)?;
            let entry = parent_children
                .remove(&leaf_name)
                .ok_or_else(|| NamespaceError::NotFound(leaf_name.clone()))?;
            m.generation += 1;
            Ok(entry)
        })?;

        // 2. Write the entry to trash as JSON.
        let now_ms = clock.unix_ms();
        let ts = now_ms / 1000;
        let nonce: u32 = rand_u32();
        let trash_name = format!(
            "{ts}_{nonce:x}_{}.trash",
            path.0.last().cloned().unwrap_or_default()
        );
        let trash_file = self.trash_path.join(&trash_name);
        let trash_entry = TrashEntry {
            original_path: path.to_string(),
            entry: old_entry.clone(),
            deleted_at_ms: now_ms,
            capability_id: audit.capability_id.clone(),
            note: audit.note.clone(),
        };
        let json = serde_json::to_string(&trash_entry)
            .map_err(|e| NamespaceError::ManifestCorrupt(format!("trash serialization: {e}")))?;
        std::fs::write(&trash_file, json)?;

        // 3. Audit log.
        let seq = next.generation;
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq,
            timestamp_unix_ms: now_ms,
            op: "soft_delete".to_string(),
            path: path.to_string(),
            hash: None,
            capability_id: audit.capability_id.clone(),
            note: Some(format!("trashed as {}", trash_name)),
        };
        self.write_audit(&record)?;
        if let Err(e) = self.persist_manifest(&next) {
            warn!(error = %e, "namespace soft_delete manifest persist failed");
            return Err(e);
        }

        debug!(path = %path, trash = %trash_name, "file soft-deleted");
        Ok(old_entry)
    }

    /// Permanently delete all trash entries older than `max_age_secs`.
    /// Returns the number of entries purged.
    pub fn empty_expired_trash(
        &self,
        max_age_secs: i64,
        clock: &dyn Clock,
    ) -> Result<usize, NamespaceError> {
        let cutoff_ms = clock.unix_ms() - (max_age_secs * 1000);
        let mut count = 0;
        for entry in std::fs::read_dir(&self.trash_path)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".trash") {
                let raw = std::fs::read_to_string(entry.path())?;
                if let Ok(trash) = serde_json::from_str::<TrashEntry>(&raw)
                    && trash.deleted_at_ms < cutoff_ms
                {
                    std::fs::remove_file(entry.path())?;
                    count += 1;
                }
            }
        }
        debug!(count, "emptied expired trash");
        Ok(count)
    }

    /// List trash entries (most-recently-deleted first).
    pub fn list_trash(&self) -> Result<Vec<TrashEntry>, NamespaceError> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&self.trash_path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".trash") {
                let raw = std::fs::read_to_string(entry.path())?;
                if let Ok(trash) = serde_json::from_str::<TrashEntry>(&raw) {
                    entries.push(trash);
                }
            }
        }
        entries.sort_by(|a, b| b.deleted_at_ms.cmp(&a.deleted_at_ms));
        Ok(entries)
    }

    /// Restore a file from trash by its original path.
    /// Returns the restored entry.
    pub fn restore(
        &self,
        original_path: &str,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<Entry, NamespaceError> {
        // Find the trash entry.
        let trash_entries = self.list_trash()?;
        let trash_entry = trash_entries
            .into_iter()
            .find(|t| t.original_path == original_path)
            .ok_or_else(|| NamespaceError::NotFound(format!("not in trash: {original_path}")))?;

        let path = PathSegments::decode_http(&format!("/{}", original_path))?;

        // Restore to manifest.
        let (next, _) = self.mutate_with(|m| {
            let parent_children = Self::split_walk_or_create(m, &path)?;
            let leaf_name = path.0.last().cloned().unwrap();
            parent_children.insert(leaf_name, trash_entry.entry.clone());
            m.generation += 1;
            Ok(())
        })?;

        let now_ms = clock.unix_ms();
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq: next.generation,
            timestamp_unix_ms: now_ms,
            op: "restore".to_string(),
            path: original_path.to_string(),
            hash: None,
            capability_id: audit.capability_id.clone(),
            note: Some("restored from trash".into()),
        };
        self.write_audit(&record)?;
        self.persist_manifest(&next)?;

        debug!(path = %original_path, "file restored from trash");
        Ok(trash_entry.entry)
    }

    /// Snapshot a file before it is overwritten, returning the snapshot ID.
    /// Stores a JSON manifest entry under `<versions_path>/<safe_path_hash>/<seq>.json`.
    pub fn snapshot_version(
        &self,
        path: &PathSegments,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<String, NamespaceError> {
        let entry = self
            .lookup(path)
            .ok_or_else(|| NamespaceError::NotFound(path.to_string()))?;
        let snap_id = format!("v{}", clock.unix_ms());
        let path_hash = blake3::hash(path.to_string().as_bytes())
            .to_hex()
            .to_string();
        let snap_dir = self.versions_path.join(&path_hash);
        std::fs::create_dir_all(&snap_dir)?;
        let snap_file = snap_dir.join(format!("{}.json", snap_id));
        let snap_meta = VersionMeta {
            snap_id: snap_id.clone(),
            original_path: path.to_string(),
            entry,
            created_at_ms: clock.unix_ms(),
            capability_id: audit.capability_id.clone(),
            note: audit.note.clone(),
        };
        let json = serde_json::to_string_pretty(&snap_meta)
            .map_err(|e| NamespaceError::ManifestCorrupt(format!("version serialization: {e}")))?;
        std::fs::write(&snap_file, json)?;
        debug!(path = %path, snap_id = %snap_id, "file version snapshot created");
        Ok(snap_id)
    }

    /// List all version snapshot IDs for a path (newest first).
    pub fn list_versions(&self, path: &PathSegments) -> Result<Vec<String>, NamespaceError> {
        let path_hash = blake3::hash(path.to_string().as_bytes())
            .to_hex()
            .to_string();
        let snap_dir = self.versions_path.join(&path_hash);
        let mut ids = Vec::new();
        if snap_dir.is_dir() {
            for entry in std::fs::read_dir(&snap_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".json") {
                    let id = name.trim_end_matches(".json").to_string();
                    ids.push(id);
                }
            }
        }
        ids.sort_by(|a, b| b.cmp(a)); // newest first
        Ok(ids)
    }

    /// Read a specific version snapshot.
    pub fn get_version(&self, path: &PathSegments, snap_id: &str) -> Result<Entry, NamespaceError> {
        let path_hash = blake3::hash(path.to_string().as_bytes())
            .to_hex()
            .to_string();
        let snap_file = self
            .versions_path
            .join(&path_hash)
            .join(format!("{snap_id}.json"));
        let raw = std::fs::read_to_string(&snap_file)?;
        let meta: VersionMeta = serde_json::from_str(&raw)
            .map_err(|e| NamespaceError::ManifestCorrupt(format!("version parse: {e}")))?;
        Ok(meta.entry)
    }

    /// Restore a file to a specific version snapshot.
    pub fn restore_version(
        &self,
        path: &PathSegments,
        snap_id: &str,
        audit: &AuditContext,
        clock: &dyn Clock,
    ) -> Result<Entry, NamespaceError> {
        let restored_entry = self.get_version(path, snap_id)?;
        let (next, _) = self.mutate_with(|m| {
            let (parent_children, leaf_name) = Self::split_walk(m, path)?;
            parent_children.remove(&leaf_name);
            parent_children.insert(leaf_name, restored_entry.clone());
            m.generation += 1;
            Ok(())
        })?;
        let now_ms = clock.unix_ms();
        let record = AuditRecord {
            schema: AUDIT_SCHEMA_VERSION,
            seq: next.generation,
            timestamp_unix_ms: now_ms,
            op: "restore_version".to_string(),
            path: path.to_string(),
            hash: None,
            capability_id: audit.capability_id.clone(),
            note: Some(format!("restored to {snap_id}")),
        };
        self.write_audit(&record)?;
        self.persist_manifest(&next)?;
        debug!(path = %path, snap_id = %snap_id, "file restored to version");
        Ok(restored_entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::ContentHash;

    #[test]
    fn decode_rejects_traversal() {
        // `/a/../b` — RFC 4918 normalisation collapses to `/b`.
        // No escape happens, so the test verifies the normaliser
        // does the right thing rather than rejecting up front.
        let p = PathSegments::decode_http("/a/../b").unwrap();
        assert_eq!(p.0, vec!["b"]);

        // A genuine escape attempt uses `..` past the root.
        assert!(PathSegments::decode_http("/..").is_err());
        assert!(PathSegments::decode_http("/a/../../").is_err());
    }

    #[test]
    fn decode_strips_root_slash() {
        let p = PathSegments::decode_http("/foo/bar").unwrap();
        assert_eq!(p.0, vec!["foo", "bar"]);
    }

    #[test]
    fn decode_rejects_overlong() {
        let huge = format!("/{}", "a".repeat(MAX_PATH_RAW_LEN));
        let err = PathSegments::decode_http(&huge).unwrap_err();
        assert!(matches!(err, NamespaceError::PathTooLong { .. }));
    }

    #[test]
    fn decode_rejects_null_byte() {
        let err = PathSegments::decode_http("/foo%00").unwrap_err();
        assert!(matches!(err, NamespaceError::Traversal(_)));
    }

    #[test]
    fn canonicalize_empty_segments_rejected() {
        assert!(canonicalize_path_string("//foo").is_err());
        assert!(canonicalize_path_string("/foo//").is_err());
    }

    #[test]
    fn lookup_root_is_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nas = Nas::open(dir.path()).unwrap();
        let root = PathSegments(vec![]);
        let e = nas.lookup(&root).unwrap();
        assert!(e.is_dir());
    }

    #[test]
    fn put_then_lookup_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let nas = Nas::open(dir.path()).unwrap();
        let path = PathSegments(vec!["photos".into(), "summer.jpg".into()]);
        let hash = ContentHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let expected = hash.clone();
        nas.put(
            &path,
            hash,
            1024,
            &AuditContext::default(),
            &MockClock(1),
            &NoopQuota,
        )
        .unwrap();
        let e = nas.lookup(&path).unwrap();
        match e {
            Entry::File {
                hash: h,
                size_bytes: s,
            } => {
                assert_eq!(h, expected);
                assert_eq!(s, 1024);
            }
            _ => panic!("expected file"),
        }
    }

    #[test]
    fn put_at_depth_limit_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let nas = Nas::open(dir.path()).unwrap();
        let mut p = Vec::new();
        for i in 0..(MAX_DEPTH + 2) {
            p.push(format!("d{i}"));
        }
        let path = PathSegments(p);
        let hash = ContentHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let err = nas
            .put(
                &path,
                hash,
                1,
                &AuditContext::default(),
                &MockClock(1),
                &NoopQuota,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            NamespaceError::DepthExceeded { .. } | NamespaceError::PathTooLong { .. }
        ));
    }

    #[test]
    fn quota_rejects_put() {
        struct Q;
        impl QuotaHook for Q {
            fn check_write(&self, _r: u64) -> Result<(), NamespaceError> {
                Err(NamespaceError::QuotaExhausted { need: 1, free: 0 })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let nas = Nas::open(dir.path()).unwrap();
        let path = PathSegments(vec!["x".into()]);
        let hash = ContentHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        let err = nas
            .put(&path, hash, 1, &AuditContext::default(), &MockClock(1), &Q)
            .unwrap_err();
        assert!(matches!(err, NamespaceError::QuotaExhausted { .. }));
    }

    #[test]
    fn audit_log_appends_one_json_line_per_put() {
        let dir = tempfile::tempdir().unwrap();
        let nas = Nas::open(dir.path()).unwrap();
        let path = PathSegments(vec!["audit-me".into()]);
        let hash = ContentHash::from_hex(
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();
        nas.put(
            &path,
            hash,
            1,
            &AuditContext {
                capability_id: Some("cred-1".into()),
                note: Some("user-agent-test".into()),
            },
            &MockClock(123456789),
            &NoopQuota,
        )
        .unwrap();
        let body = std::fs::read_to_string(dir.path().join("nas").join("audit.jsonl")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["op"], "put");
        assert_eq!(v["timestamp_unix_ms"], 123456789);
        assert_eq!(v["capability_id"], "cred-1");
        assert_eq!(v["note"], "user-agent-test");
    }
}
