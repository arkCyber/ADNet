//! Persistent resume state for `a3net share receive`.
//!
//! Every share-receive places its bytes in
//! `$ADNET_DATA_DIR/incoming/{hash_short}/` (where `hash_short` is the
//! first 16 hex chars of the manifest hash). Next to the iroh-blobs
//! `FsStore` directory, this module writes a small JSON sidecar
//! `resume.json` that tracks:
//!
//! - the original `ShareTicket` (so the user can re-run the same
//!   command and pick up where they left off),
//! - the manifest hash (so the iroh-blobs `FsStore` directory can be
//!   located even after restarts),
//! - per-file byte progress (so a `/metrics` scrape can show
//!   `share_receive_bytes_done / share_receive_bytes_total`),
//! - wall-clock timing,
//! - terminal status (`completed` / `interrupted` / `failed`).
//!
//! ## Why not just rely on `FsStore::local()`?
//!
//! iroh-blobs already tracks per-blob partial state internally; on
//! re-entry the `remote().local(hash)` call surfaces the missing
//! bytes and `execute_get` resumes from there. So **process-internal
//! resumption is free**. What this module adds:
//!
//! 1. A user-visible **directory convention** (`incoming/{hash_short}/`)
//!    that survives across `a3net` invocations and across machines
//!    (rsync the directory and resume on another host).
//! 2. A JSON sidecar so the operator can `cat resume.json` to see
//!    progress without booting the whole node.
//! 3. A list/clean API (`a3net share resume ls` / `clean`) so the
//!    incoming directory doesn't grow unbounded.
//!
//! ## Failure modes
//!
//! - The `FsStore` is the source of truth for byte counts; the
//!   sidecar is a **cache** that may be stale by a few seconds
//!   during a live transfer.
//! - A corrupted or missing sidecar is non-fatal — we rebuild it
//!   from the FsStore on the next receive call.
//! - A corrupted FsStore is reported to the caller as
//!   `ShareError::Backend`; the sidecar is left untouched.

use std::path::{Path, PathBuf};

use a3net_types::ContentHash;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{ShareError, ShareResult};
use crate::ticket::ShareTicket;

/// Length of the manifest-hash prefix used in directory names. 16 hex
/// chars = 64 bits of BLAKE3, which is plenty to disambiguate two
/// concurrent receives and short enough to keep paths readable.
pub const HASH_SHORT_LEN: usize = 16;

/// File name of the sidecar JSON. Picked to make `ls incoming/`
/// scannable.
pub const RESUME_STATE_FILENAME: &str = "resume.json";

/// File name of the cached manifest blob. Stores the
/// postcard-serialised [`crate::collection::Collection`] so a
/// restarted receive doesn't have to fetch it again.
pub const RESUME_MANIFEST_FILENAME: &str = "manifest.bin";

/// Lifecycle marker of an in-flight (or completed) receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStatus {
    /// `remote_fetch` is in progress.
    InProgress,
    /// The receive finished cleanly; every file in the manifest is
    /// complete on disk.
    Completed,
    /// The receive was interrupted (network drop, Ctrl-C, process
    /// crash). All completed files are still on disk; the next
    /// `receive` call with the same ticket will resume.
    Interrupted,
    /// The receive hit a fatal error. The sidecar carries the error
    /// reason so the operator can decide whether to retry or wipe.
    Failed,
}

impl Default for ResumeStatus {
    fn default() -> Self {
        Self::InProgress
    }
}

/// Per-file progress entry. We track every file in the manifest so a
/// future `share receive --only file.txt` could (in PR4+) resume a
/// subset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeFileProgress {
    /// Entry name as it appears in the [`crate::collection::Collection`].
    pub name: String,
    /// BLAKE3 hash of the file bytes (matches the manifest entry).
    pub hash: ContentHash,
    /// Expected total bytes (sum of chunk sizes once the FsStore
    /// reports `Complete`).
    pub total_bytes: u64,
    /// Bytes successfully written so far.
    pub bytes_done: u64,
    /// True when `store.has(hash)` returns true.
    pub complete: bool,
}

/// JSON-serialisable snapshot of an in-flight or completed receive.
///
/// The struct is deliberately `#[serde(deny_unknown_fields)]`-friendly
/// — fields are only added, never renamed. Removing a field requires
/// a migration (none so far).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeState {
    /// Schema version. Bump on backwards-incompatible changes.
    pub version: u32,
    /// The ticket the user typed, verbatim. Lets the operator run
    /// `a3net share resume continue {hash_short}` to resume without
    /// re-typing the URL.
    pub ticket: String,
    /// Sender's `NodeId` (carried in the ticket for fast access).
    pub sender_node_id: String,
    /// Manifest hash as hex. Also the FsStore directory name.
    pub manifest_hash: ContentHash,
    /// Sum of every file's expected size.
    pub total_bytes: u64,
    /// Sum of every file's `bytes_done` at the time the sidecar was
    /// last written.
    pub bytes_done: u64,
    /// Per-file progress. Persisted so an interrupted receive can
    /// show "42/58 files" in the operator UI.
    pub files: Vec<ResumeFileProgress>,
    /// Lifecycle status. `InProgress` while the receive is alive;
    /// the receive finalises this to `Completed` / `Interrupted` /
    /// `Failed` before exiting.
    pub status: ResumeStatus,
    /// Optional error reason when `status == Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// First time we wrote the sidecar (i.e. started this receive).
    pub started_at: DateTime<Utc>,
    /// Last time we updated the sidecar.
    pub updated_at: DateTime<Utc>,
}

impl ResumeState {
    /// Initial state for a fresh receive — no files yet, status
    /// `InProgress`, timestamps now.
    pub fn new(ticket: &ShareTicket, manifest_hash: ContentHash) -> Self {
        let now = Utc::now();
        Self {
            version: 1,
            ticket: ticket.encode(),
            sender_node_id: ticket.node_id.to_string(),
            manifest_hash,
            total_bytes: 0,
            bytes_done: 0,
            files: Vec::new(),
            status: ResumeStatus::InProgress,
            error: None,
            started_at: now,
            updated_at: now,
        }
    }

    /// Schema version. Bumped on backwards-incompatible changes.
    pub const CURRENT_VERSION: u32 = 1;

    /// Convenience: percentage (0–100) of the receive that's done.
    /// `0` when `total_bytes == 0` so callers don't NaN out.
    pub fn percent_done(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.bytes_done as f64 / self.total_bytes as f64) * 100.0
        }
    }

    /// Number of files in the manifest that have finished downloading.
    pub fn files_done(&self) -> usize {
        self.files.iter().filter(|f| f.complete).count()
    }
}

/// Compute the on-disk directory for a given manifest hash.
///
/// Result is `$data_dir/incoming/{first 16 hex chars of manifest_hash}/`.
/// Callers create the directory with `tokio::fs::create_dir_all`
/// before opening the `FsStore`.
pub fn resume_dir(data_dir: &Path, manifest_hash: &ContentHash) -> PathBuf {
    let short = &manifest_hash.as_hex()[..HASH_SHORT_LEN];
    data_dir.join("incoming").join(short)
}

/// Atomic write of a `ResumeState` JSON. Writes to `resume.json.tmp`
/// then renames — so a crash mid-write never produces a corrupt
/// `resume.json`.
pub fn save(dir: &Path, state: &ResumeState) -> ShareResult<()> {
    let final_path = dir.join(RESUME_STATE_FILENAME);
    let tmp_path = dir.join(format!("{RESUME_STATE_FILENAME}.tmp"));
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| {
        ShareError::Backend(format!("serialize resume state: {e}"))
    })?;
    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, &final_path).map_err(|e| {
        ShareError::Backend(format!(
            "rename {} → {}: {e}",
            tmp_path.display(),
            final_path.display()
        ))
    })?;
    Ok(())
}

/// Load the sidecar JSON from `dir`, or return `Ok(None)` if the
/// file does not exist. Other I/O errors propagate.
pub fn load(dir: &Path) -> ShareResult<Option<ResumeState>> {
    let path = dir.join(RESUME_STATE_FILENAME);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let state: ResumeState = serde_json::from_slice(&bytes).map_err(|e| {
                ShareError::Backend(format!(
                    "parse {}: {e}",
                    path.display()
                ))
            })?;
            Ok(Some(state))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ShareError::Io(e)),
    }
}

/// List every `resume.json` under `$data_dir/incoming/*`. Order is
/// **not** specified — callers that want a deterministic order
/// should sort by `updated_at` afterwards.
pub fn list(data_dir: &Path) -> ShareResult<Vec<ResumeState>> {
    let incoming = data_dir.join("incoming");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&incoming) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(ShareError::Io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(ShareError::Io)?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Some(state) = load(&entry.path())? {
            out.push(state);
        }
    }
    Ok(out)
}

/// Delete the on-disk state for a given manifest hash. Returns
/// `true` if something was removed, `false` otherwise.
///
/// Refuses to delete a state whose `status == InProgress` — the
/// caller must explicitly mark it `Interrupted` / `Failed` /
/// `Completed` first, so an operator never wipes a live receive
/// by accident.
pub fn clean(data_dir: &Path, manifest_hash: &ContentHash) -> ShareResult<bool> {
    let dir = resume_dir(data_dir, manifest_hash);
    if !dir.exists() {
        return Ok(false);
    }
    if let Some(state) = load(&dir)?
        && state.status == ResumeStatus::InProgress
    {
        return Err(ShareError::Backend(format!(
            "refusing to clean in-progress receive {}; \
             mark it interrupted/completed/failed first",
            state.manifest_hash.as_hex()
        )));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| {
        ShareError::Backend(format!("remove {}: {e}", dir.display()))
    })?;
    Ok(true)
}

/// Path of the cached manifest blob. Callers serialise the
/// [`crate::collection::Collection`] to postcard bytes and write to
/// this path so a restarted receive skips the manifest fetch.
pub fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(RESUME_MANIFEST_FILENAME)
}

/// True if a cached manifest is available on disk.
pub fn has_cached_manifest(dir: &Path) -> bool {
    manifest_path(dir).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::Collection;
    use a3net_types::node::Endpoint;
    use tempfile::TempDir;

    fn ticket() -> ShareTicket {
        let id = a3net_types::NodeId::random();
        let addr = a3net_types::NodeAddr::new(id.clone())
            .with_direct(Endpoint::new("127.0.0.1", 9000));
        let mh = ContentHash::from_bytes(b"x");
        ShareTicket::new(&id, &addr, &mh, &Collection::new(), 0).unwrap()
    }

    #[test]
    fn resume_dir_uses_short_hash() {
        let data = std::path::PathBuf::from("/tmp/data");
        let h = ContentHash::from_bytes(b"hello");
        let d = resume_dir(&data, &h);
        assert!(d.starts_with("/tmp/data/incoming/"));
        let last = d.file_name().unwrap().to_str().unwrap();
        assert_eq!(last.len(), HASH_SHORT_LEN);
        assert_eq!(last, &h.as_hex()[..HASH_SHORT_LEN]);
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let t = ticket();
        let mh = ContentHash::from_bytes(b"world");
        let state = ResumeState::new(&t, mh);
        save(dir.path(), &state).unwrap();
        let back = load(dir.path()).unwrap().unwrap();
        assert_eq!(state, back);
        assert_eq!(state.version, ResumeState::CURRENT_VERSION);
        assert_eq!(state.status, ResumeStatus::InProgress);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        let back = load(dir.path()).unwrap();
        assert!(back.is_none());
    }

    #[test]
    fn load_returns_err_on_corrupt() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(RESUME_STATE_FILENAME), b"not json").unwrap();
        let err = load(dir.path()).unwrap_err();
        assert!(matches!(err, ShareError::Backend(_)));
    }

    #[test]
    fn save_is_atomic_via_rename() {
        let dir = TempDir::new().unwrap();
        let t = ticket();
        let mh = ContentHash::from_bytes(b"z");
        let mut state = ResumeState::new(&t, mh);
        save(dir.path(), &state).unwrap();
        state.bytes_done = 42;
        save(dir.path(), &state).unwrap();
        // No .tmp file left over.
        assert!(!dir.path().join(format!("{RESUME_STATE_FILENAME}.tmp")).exists());
        let back = load(dir.path()).unwrap().unwrap();
        assert_eq!(back.bytes_done, 42);
    }

    #[test]
    fn list_returns_states_under_incoming() {
        let dir = TempDir::new().unwrap();
        // Pre-create two incoming dirs with sidecars.
        let t = ticket();
        for byte in [1u8, 2u8] {
            let mh = ContentHash::from_bytes(&[byte]);
            let sub = resume_dir(dir.path(), &mh);
            std::fs::create_dir_all(&sub).unwrap();
            let state = ResumeState::new(&t, mh);
            save(&sub, &state).unwrap();
        }
        let states = list(dir.path()).unwrap();
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn list_returns_empty_when_incoming_absent() {
        let dir = TempDir::new().unwrap();
        let states = list(dir.path()).unwrap();
        assert!(states.is_empty());
    }

    #[test]
    fn clean_refuses_in_progress() {
        let dir = TempDir::new().unwrap();
        let t = ticket();
        let mh = ContentHash::from_bytes(b"x");
        let sub = resume_dir(dir.path(), &mh);
        std::fs::create_dir_all(&sub).unwrap();
        save(&sub, &ResumeState::new(&t, mh.clone())).unwrap();

        let err = clean(dir.path(), &mh).unwrap_err();
        assert!(matches!(err, ShareError::Backend(_)));
        assert!(sub.exists());
    }

    #[test]
    fn clean_removes_completed() {
        let dir = TempDir::new().unwrap();
        let t = ticket();
        let mh = ContentHash::from_bytes(b"x");
        let sub = resume_dir(dir.path(), &mh);
        std::fs::create_dir_all(&sub).unwrap();
        let mut state = ResumeState::new(&t, mh.clone());
        state.status = ResumeStatus::Completed;
        save(&sub, &state).unwrap();

        let removed = clean(dir.path(), &mh).unwrap();
        assert!(removed);
        assert!(!sub.exists());
    }

    #[test]
    fn clean_returns_false_when_absent() {
        let dir = TempDir::new().unwrap();
        let mh = ContentHash::from_bytes(b"x");
        let removed = clean(dir.path(), &mh).unwrap();
        assert!(!removed);
    }

    #[test]
    fn percent_done_zero_when_total_zero() {
        let t = ticket();
        let mh = ContentHash::from_bytes(b"x");
        let state = ResumeState::new(&t, mh);
        assert_eq!(state.percent_done(), 0.0);
    }

    #[test]
    fn percent_done_computes_correctly() {
        let t = ticket();
        let mh = ContentHash::from_bytes(b"x");
        let mut state = ResumeState::new(&t, mh);
        state.total_bytes = 200;
        state.bytes_done = 50;
        assert_eq!(state.percent_done(), 25.0);
    }

    #[test]
    fn files_done_counts_only_complete() {
        let t = ticket();
        let mh = ContentHash::from_bytes(b"x");
        let mut state = ResumeState::new(&t, mh);
        for i in 0..3 {
            state.files.push(ResumeFileProgress {
                name: format!("f{i}"),
                hash: ContentHash::from_bytes(&[i as u8]),
                total_bytes: 100,
                bytes_done: 100,
                complete: i < 2,
            });
        }
        assert_eq!(state.files_done(), 2);
    }

    #[test]
    fn manifest_path_inside_resume_dir() {
        let dir = TempDir::new().unwrap();
        let p = manifest_path(dir.path());
        assert_eq!(p.file_name().unwrap(), RESUME_MANIFEST_FILENAME);
        assert!(p.starts_with(dir.path()));
    }

    #[test]
    fn has_cached_manifest_reflects_filesystem() {
        let dir = TempDir::new().unwrap();
        assert!(!has_cached_manifest(dir.path()));
        std::fs::write(manifest_path(dir.path()), b"x").unwrap();
        assert!(has_cached_manifest(dir.path()));
    }
}