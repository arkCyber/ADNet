//! Persistent content blocklist.
//!
//! The blocklist is the **canonical store of "this hash is forbidden
//! on our public surface"**. It is consulted on every gateway read /
//! write / pin operation and is the source of truth for takedown
//! reporting.
//!
//! ## On-disk format
//!
//! ```text
//! <data_dir>/blocklist.json
//! ```
//!
//! ```json
//! {
//!   "version": 1,
//!   "next_entry_id": 42,
//!   "entries": [
//!     {
//!       "id": 1,
//!       "hash": "5f4a...e3",
//!       "reason": "csam",
//!       "source": "ncmec",
//!       "evidence": "NCMEC case 12345",
//!       "operator": "alice@a3net.example",
//!       "issued_unix": 1730000000,
//!       "expires_unix": null,
//!       "revoked": false
//!     }
//!   ]
//! }
//! ```
//!
//! The file is rewritten on every mutating operation using the
//! **write-to-temp + atomic rename** pattern so a crash mid-write can
//! never leave a truncated file behind.
//!
//! ## Concurrency
//!
//! All access is guarded by an interior `parking_lot::RwLock`. The
//! read path (`is_blocked`) is hot — every gateway request hits it.
//! The `indexmap` preserves insertion order so [`Blocklist::list`]
//! yields a deterministic output for CLI / audit streams.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use a3net_types::ContentHash;

use crate::error::{ModerationError, ModerationResult};

/// Default filename for the on-disk blocklist. Lives under
/// `<data_dir>/moderation/blocklist.json`.
pub const DEFAULT_BLOCKLIST_FILENAME: &str = "blocklist.json";

/// Folder under `data_dir` that holds all moderation state.
pub const MODERATION_DIR: &str = "moderation";

/// Schema version of the on-disk blocklist. Bump when the format
/// changes in a non-backward-compatible way.
pub const BLOCKLIST_FORMAT_VERSION: u32 = 1;

/// Why a content hash was added to the blocklist.
///
/// The numeric tags are stable; new kinds are appended at the bottom
/// (do not renumber existing entries — operators index reports by
/// tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TakedownReason {
    /// Child sexual abuse material.
    Csam = 1,
    /// Copyright infringement (DMCA / EU CDSM / etc.).
    Copyright = 2,
    /// Terrorism / violent extremism content.
    Terrorism = 3,
    /// Non-consensual intimate imagery.
    Ncii = 4,
    /// Doxxing / private data exposure.
    Doxxing = 5,
    /// Court order or law-enforcement seizure.
    LegalOrder = 6,
    /// Malware / phishing payload.
    Malware = 7,
    /// Platform-specific terms of service violation that the
    /// operator has elected to enforce at the storage layer.
    TermsOfService = 8,
    /// Catch-all for cases not covered by the above.
    Other = 99,
}

impl TakedownReason {
    /// Default threat level attached to this reason. Used by the
    /// reputation bridge (`a3net-moderation::reputation_bridge`) to
    /// translate a takedown into a per-peer score penalty.
    pub fn severity(&self) -> u8 {
        match self {
            TakedownReason::Csam => 10,
            TakedownReason::Terrorism => 10,
            TakedownReason::Ncii => 9,
            TakedownReason::LegalOrder => 9,
            TakedownReason::Malware => 7,
            TakedownReason::Copyright => 5,
            TakedownReason::Doxxing => 6,
            TakedownReason::TermsOfService => 3,
            TakedownReason::Other => 2,
        }
    }
}

/// Where the takedown signal originated. Recorded for the audit so
/// operators can demonstrate the provenance of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlocklistSource {
    /// National Center for Missing & Exploited Children (US).
    Ncmec,
    /// Internet Watch Foundation (UK).
    Iwf,
    /// INTERPOL ICSE database.
    Interpol,
    /// Operator-initiated review (manual).
    Operator,
    /// Third-party trusted feed (hashing service, NGO, …).
    TrustedFeed,
    /// Court / law-enforcement order.
    LegalOrder,
    /// On-chain / signed message from a governance DAO.
    Governance,
}

impl Default for BlocklistSource {
    fn default() -> Self {
        Self::Operator
    }
}

/// One row of the blocklist. Inserts are append-only — to remove a
/// block you mark `revoked = true` (never delete) so the audit trail
/// is preserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlocklistEntry {
    /// Monotonically increasing id assigned by [`Blocklist::add`].
    /// Stable across saves (so external audit logs can reference an
    /// id by number).
    pub id: u64,

    /// BLAKE3 hex of the blocked content.
    pub hash: ContentHash,

    /// Why the block was filed.
    pub reason: TakedownReason,

    /// Who / what filed the block.
    #[serde(default)]
    pub source: BlocklistSource,

    /// Free-form text — case number, URL, court order reference, …
    #[serde(default)]
    pub evidence: String,

    /// Operator identity that executed the block. Captured for
    /// audit. May be empty for entries imported from external feeds.
    #[serde(default)]
    pub operator: String,

    /// Unix-seconds when the block was filed.
    pub issued_unix: i64,

    /// Unix-seconds when the block auto-expires. `None` =
    /// indefinite (the common case for CSAM / court orders).
    #[serde(default)]
    pub expires_unix: Option<i64>,

    /// Associated [`a3net_types::NodeId`] of the publishing peer, if
    /// known. Stored as a hex-encoded 32-byte string so the on-disk
    /// format is JSON-clean. Empty string when the publisher is
    /// unknown.
    #[serde(default)]
    pub publisher_node_hex: String,

    /// `true` once the block is revoked. Revoked entries are kept
    /// for audit but no longer block reads / writes.
    #[serde(default)]
    pub revoked: bool,
}

impl BlocklistEntry {
    /// Is this block currently active? An entry is active when
    /// `revoked == false` and the optional expiry is in the future.
    pub fn is_active(&self, now_unix: i64) -> bool {
        if self.revoked {
            return false;
        }
        match self.expires_unix {
            Some(exp) => now_unix < exp,
            None => true,
        }
    }
}

/// Aggregate statistics for the CLI / dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlocklistStats {
    /// Total entries currently active (non-revoked, non-expired).
    pub active: usize,
    /// Total entries ever added (including revoked / expired).
    pub total: usize,
    /// Breakdown of active entries by reason.
    pub by_reason: IndexMap<String, usize>,
}

/// In-memory + on-disk blocklist.
///
/// The on-disk file is rewritten on every mutation. Reads are
/// in-memory only after the initial load.
pub struct Blocklist {
    /// File path of the persisted blocklist.
    path: PathBuf,
    /// `hash → entry_id` lookup. Holds the active entry for each
    /// hash. Repeat-adds get a new entry id but the lookup table
    /// points to the most recent.
    by_hash: RwLock<IndexMap<String, u64>>,
    /// All entries in insertion order. The index in this map is the
    /// `id` field of the entry (1-based).
    entries: RwLock<IndexMap<u64, BlocklistEntry>>,
    /// Next entry id to assign on `add`.
    next_id: RwLock<u64>,
}

impl std::fmt::Debug for Blocklist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let entries = self.entries.read();
        let by_hash = self.by_hash.read();
        f.debug_struct("Blocklist")
            .field("path", &self.path)
            .field("active_entries", &entries.len())
            .field("tracked_hashes", &by_hash.len())
            .finish()
    }
}

impl Blocklist {
    /// Load or create the blocklist at `<root>/moderation/blocklist.json`.
    /// A missing file yields an empty blocklist.
    pub fn load(root: &Path) -> ModerationResult<Self> {
        let path = Self::path_at(root);
        Self::load_from_path(&path)
    }

    /// Load from an explicit file path. Missing file → empty.
    pub fn load_from_path(path: &Path) -> ModerationResult<Self> {
        if !path.exists() {
            return Ok(Self::empty_at(path));
        }
        let bytes = fs::read(path)?;
        if bytes.is_empty() {
            return Ok(Self::empty_at(path));
        }
        let document: BlocklistDocument = serde_json::from_slice(&bytes)
            .map_err(|e| ModerationError::InvalidBlocklist(e.to_string()))?;

        let mut by_hash = IndexMap::new();
        let mut entries = IndexMap::new();
        for entry in document.entries {
            by_hash.insert(entry.hash.as_hex().to_string(), entry.id);
            entries.insert(entry.id, entry);
        }
        let next_id = document.next_entry_id.max(
            entries
                .keys()
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(1),
        );

        Ok(Self {
            path: path.to_path_buf(),
            by_hash: RwLock::new(by_hash),
            entries: RwLock::new(entries),
            next_id: RwLock::new(next_id),
        })
    }

    /// Construct an in-memory-only blocklist (no persistence).
    /// Useful for tests and for the `a3net moderation list` filter
    /// paths that don't need to write.
    pub fn in_memory() -> Self {
        Self::empty_at(Path::new("<memory>"))
    }

    fn empty_at(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            by_hash: RwLock::new(IndexMap::new()),
            entries: RwLock::new(IndexMap::new()),
            next_id: RwLock::new(1),
        }
    }

    fn path_at(root: &Path) -> PathBuf {
        root.join(MODERATION_DIR).join(DEFAULT_BLOCKLIST_FILENAME)
    }

    /// Resolve the absolute path of the blocklist file under `root`.
    pub fn blocklist_path(root: &Path) -> PathBuf {
        Self::path_at(root)
    }

    /// Is the given content hash currently blocked?
    ///
    /// This is the hot path. It takes a read lock on the by_hash map
    /// and reads the entry without taking the entries lock.
    pub fn is_blocked(&self, hash: &ContentHash) -> bool {
        let by_hash = self.by_hash.read();
        let Some(id) = by_hash.get(hash.as_hex()).copied() else {
            return false;
        };
        drop(by_hash);
        let entries = self.entries.read();
        entries
            .get(&id)
            .is_some_and(|e| e.is_active(now_unix()))
    }

    /// Like [`is_blocked`] but returns the active entry instead of
    /// a bool. Useful for the gateway to attach the `reason` to the
    /// 451 response.
    pub fn lookup_active(&self, hash: &ContentHash) -> Option<BlocklistEntry> {
        let by_hash = self.by_hash.read();
        let id = by_hash.get(hash.as_hex()).copied()?;
        drop(by_hash);
        let entries = self.entries.read();
        let entry = entries.get(&id)?;
        if entry.is_active(now_unix()) {
            Some(entry.clone())
        } else {
            None
        }
    }

    /// Add a hash to the blocklist. Returns the assigned entry id.
    /// If the hash is already blocked, the existing entry is returned
    /// unchanged (with its original id) so audit logs are stable.
    pub fn add(
        &self,
        hash: ContentHash,
        reason: TakedownReason,
        source: BlocklistSource,
        evidence: impl Into<String>,
        operator: impl Into<String>,
        expires_unix: Option<i64>,
        publisher_node_hex: impl Into<String>,
    ) -> ModerationResult<u64> {
        let operator = operator.into();
        let evidence = evidence.into();
        let publisher_node_hex = publisher_node_hex.into();

        {
            let by_hash = self.by_hash.read();
            if let Some(existing_id) = by_hash.get(hash.as_hex()).copied() {
                let entries = self.entries.read();
                if let Some(existing) = entries.get(&existing_id)
                    && existing.is_active(now_unix())
                {
                    return Ok(existing.id);
                }
            }
        }

        let mut next_id = self.next_id.write();
        let id = *next_id;
        *next_id = next_id.saturating_add(1);
        drop(next_id);

        let entry = BlocklistEntry {
            id,
            hash: hash.clone(),
            reason,
            source,
            evidence,
            operator,
            issued_unix: now_unix(),
            expires_unix,
            publisher_node_hex,
            revoked: false,
        };

        let mut entries = self.entries.write();
        entries.insert(id, entry.clone());
        drop(entries);

        let mut by_hash = self.by_hash.write();
        by_hash.insert(hash.as_hex().to_string(), id);
        drop(by_hash);

        self.persist(&entry)?;
        Ok(id)
    }

    /// Revoke an entry by id. Revoked entries are kept on disk and
    /// in the in-memory map but no longer block reads / writes.
    pub fn revoke(&self, id: u64) -> ModerationResult<bool> {
        let mut entries = self.entries.write();
        let Some(entry) = entries.get_mut(&id) else {
            return Ok(false);
        };
        entry.revoked = true;
        let snapshot = entry.clone();
        drop(entries);

        // If the entry is still the "active" one for its hash, the
        // by_hash index is unchanged (a future add would replace
        // it). We deliberately do not re-point the by_hash index;
        // revocation is a soft flag, not a delete.
        self.persist(&snapshot)?;
        Ok(true)
    }

    /// Snapshot every entry in insertion order. Used by the CLI
    /// `a3net moderation list` command and by the test suite.
    pub fn list(&self) -> Vec<BlocklistEntry> {
        let entries = self.entries.read();
        entries.values().cloned().collect()
    }

    /// Snapshot only active entries.
    pub fn list_active(&self) -> Vec<BlocklistEntry> {
        let now = now_unix();
        let entries = self.entries.read();
        entries
            .values()
            .filter(|e| e.is_active(now))
            .cloned()
            .collect()
    }

    /// Aggregate stats for the CLI / dashboard.
    pub fn stats(&self) -> BlocklistStats {
        let now = now_unix();
        let entries = self.entries.read();
        let mut by_reason: IndexMap<String, usize> = IndexMap::new();
        let mut active = 0usize;
        for e in entries.values() {
            if e.is_active(now) {
                active += 1;
                *by_reason
                    .entry(format!("{:?}", e.reason).to_lowercase())
                    .or_insert(0) += 1;
            }
        }
        BlocklistStats {
            active,
            total: entries.len(),
            by_reason,
        }
    }

    /// Bulk-import entries from an external feed (NCMEC, IWF, …).
    /// Returns the number of entries that were newly added (entries
    /// already present are skipped to keep audit ids stable).
    pub fn import_feed(
        &self,
        entries: impl IntoIterator<Item = BlocklistEntry>,
    ) -> ModerationResult<usize> {
        let mut added = 0usize;
        for entry in entries {
            let hash = entry.hash.clone();
            let reason = entry.reason;
            let source = entry.source;
            let evidence = entry.evidence.clone();
            let operator = entry.operator.clone();
            let expires_unix = entry.expires_unix;
            let publisher_node_hex = entry.publisher_node_hex.clone();
            let prev = {
                let by_hash = self.by_hash.read();
                by_hash.get(hash.as_hex()).copied()
            };
            if prev.is_some() {
                continue;
            }
            self.add(
                hash,
                reason,
                source,
                evidence,
                operator,
                expires_unix,
                publisher_node_hex,
            )?;
            added += 1;
        }
        Ok(added)
    }

    /// Atomically rewrite the on-disk file. The intermediate file is
    /// written to `<path>.tmp` and then renamed, so a crash mid-write
    /// can never leave a truncated file.
    fn persist(&self, last_entry: &BlocklistEntry) -> ModerationResult<()> {
        let entries = self.entries.read();
        let next_id = *self.next_id.read();
        let document = BlocklistDocument {
            version: BLOCKLIST_FORMAT_VERSION,
            next_entry_id: next_id,
            entries: entries.values().cloned().collect(),
        };
        drop(entries);

        let serialized = serde_json::to_vec_pretty(&document)?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&serialized)?;
            f.flush()?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        let _ = last_entry;
        Ok(())
    }
}

/// On-disk shape. Kept private — callers use [`Blocklist::load`].
#[derive(Debug, Serialize, Deserialize)]
struct BlocklistDocument {
    /// Schema version. See [`BLOCKLIST_FORMAT_VERSION`].
    version: u32,
    /// Next entry id that will be assigned. Persisted so restored
    /// instances don't reuse ids.
    #[serde(default)]
    next_entry_id: u64,
    /// Insertion-ordered entries.
    entries: Vec<BlocklistEntry>,
}

/// Wall-clock helper. Kept crate-private so tests can stub it via
/// `now_unix` if needed; right now it's a thin wrapper around
/// `chrono::Utc::now().timestamp()`.
fn now_unix() -> i64 {
    Utc::now().timestamp()
}

/// Return the timestamp at which the blocklist was loaded, as an ISO
/// 8601 string. Exposed for the trail audit log.
pub fn loaded_at_iso(now_unix: i64) -> String {
    DateTime::<Utc>::from_timestamp(now_unix, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| "invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn hash(b: &[u8]) -> ContentHash {
        ContentHash::from_bytes(b)
    }

    #[test]
    fn empty_load_creates_fresh() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        assert_eq!(bl.list().len(), 0);
        assert!(!bl.is_blocked(&hash(b"x")));
    }

    #[test]
    fn add_then_is_blocked_returns_true() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        let h = hash(b"evil");
        let id = bl
            .add(
                h.clone(),
                TakedownReason::Csam,
                BlocklistSource::Ncmec,
                "case 12",
                "alice",
                None,
                "",
            )
            .unwrap();
        assert_eq!(id, 1);
        assert!(bl.is_blocked(&h));
        assert!(!bl.is_blocked(&hash(b"good")));
    }

    #[test]
    fn add_is_idempotent_when_hash_already_blocked() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        let h = hash(b"x");
        let id1 = bl
            .add(
                h.clone(),
                TakedownReason::Other,
                BlocklistSource::Operator,
                "",
                "alice",
                None,
                "",
            )
            .unwrap();
        let id2 = bl
            .add(
                h.clone(),
                TakedownReason::Other,
                BlocklistSource::Operator,
                "",
                "alice",
                None,
                "",
            )
            .unwrap();
        assert_eq!(id1, id2, "idempotent add retains the first id");
        assert_eq!(bl.list().len(), 1);
    }

    #[test]
    fn revoke_makes_hash_unblocked_but_keeps_audit() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        let h = hash(b"x");
        let id = bl
            .add(
                h.clone(),
                TakedownReason::Terrorism,
                BlocklistSource::LegalOrder,
                "court order",
                "alice",
                None,
                "",
            )
            .unwrap();
        assert!(bl.is_blocked(&h));
        assert!(bl.revoke(id).unwrap());
        assert!(!bl.is_blocked(&h));
        // Audit trail intact.
        let all = bl.list();
        assert_eq!(all.len(), 1);
        assert!(all[0].revoked);
    }

    #[test]
    fn expires_unix_disables_block_when_in_past() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        let h = hash(b"x");
        let past = now_unix() - 60;
        bl.add(
            h.clone(),
            TakedownReason::Copyright,
            BlocklistSource::Operator,
            "dmca",
            "alice",
            Some(past),
            "",
        )
        .unwrap();
        assert!(!bl.is_blocked(&h));
    }

    #[test]
    fn stats_breakdown_by_reason() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        for (b, reason) in [
            (b"a" as &[u8], TakedownReason::Csam),
            (b"b", TakedownReason::Csam),
            (b"c", TakedownReason::Copyright),
        ] {
            bl.add(
                hash(b),
                reason,
                BlocklistSource::Operator,
                "",
                "alice",
                None,
                "",
            )
            .unwrap();
        }
        let s = bl.stats();
        assert_eq!(s.total, 3);
        assert_eq!(s.active, 3);
        assert_eq!(s.by_reason.get("csam").copied(), Some(2));
        assert_eq!(s.by_reason.get("copyright").copied(), Some(1));
    }

    #[test]
    fn persist_round_trips() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        bl.add(
            hash(b"x"),
            TakedownReason::Malware,
            BlocklistSource::TrustedFeed,
            "bad exe",
            "alice",
            None,
            "deadbeef",
        )
        .unwrap();
        drop(bl);

        let bl2 = Blocklist::load(dir.path()).unwrap();
        assert!(bl2.is_blocked(&hash(b"x")));
        let entry = bl2.lookup_active(&hash(b"x")).unwrap();
        assert_eq!(entry.reason, TakedownReason::Malware);
        assert_eq!(entry.publisher_node_hex, "deadbeef");
    }

    #[test]
    fn next_id_advances_across_loads() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        bl.add(
            hash(b"a"),
            TakedownReason::Other,
            BlocklistSource::Operator,
            "",
            "alice",
            None,
            "",
        )
        .unwrap();
        bl.add(
            hash(b"b"),
            TakedownReason::Other,
            BlocklistSource::Operator,
            "",
            "alice",
            None,
            "",
        )
        .unwrap();
        drop(bl);

        let bl2 = Blocklist::load(dir.path()).unwrap();
        let id3 = bl2
            .add(
                hash(b"c"),
                TakedownReason::Other,
                BlocklistSource::Operator,
                "",
                "alice",
                None,
                "",
            )
            .unwrap();
        assert_eq!(id3, 3, "next_id must be > max existing id");
    }

    #[test]
    fn import_feed_skips_duplicates() {
        let dir = tempdir().unwrap();
        let bl = Blocklist::load(dir.path()).unwrap();
        bl.add(
            hash(b"a"),
            TakedownReason::Csam,
            BlocklistSource::Ncmec,
            "",
            "feed",
            None,
            "",
        )
        .unwrap();
        let added = bl
            .import_feed(vec![BlocklistEntry {
                id: 99,
                hash: hash(b"a"),
                reason: TakedownReason::Csam,
                source: BlocklistSource::Ncmec,
                evidence: String::new(),
                operator: "feed".to_string(),
                issued_unix: 0,
                expires_unix: None,
                publisher_node_hex: String::new(),
                revoked: false,
            }])
            .unwrap();
        assert_eq!(added, 0, "duplicate hash is skipped");
        let added = bl
            .import_feed(vec![BlocklistEntry {
                id: 99,
                hash: hash(b"b"),
                reason: TakedownReason::Csam,
                source: BlocklistSource::Ncmec,
                evidence: String::new(),
                operator: "feed".to_string(),
                issued_unix: 0,
                expires_unix: None,
                publisher_node_hex: String::new(),
                revoked: false,
            }])
            .unwrap();
        assert_eq!(added, 1);
    }

    #[test]
    fn severity_orders_correctly() {
        assert!(TakedownReason::Csam.severity() > TakedownReason::Copyright.severity());
        assert!(TakedownReason::Terrorism.severity() >= TakedownReason::Csam.severity());
    }
}
