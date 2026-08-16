//! Persistent, thread-safe wrapper around [`ContactsList`].
//!
//! ## Responsibilities
//!
//! - On-disk persistence to `<data_dir>/contacts.json`. Atomic writes
//!   (write-to-temp + rename) so a crash never leaves the file half-
//!   written.
//! - Concurrent access via [`parking_lot::RwLock`] so the
//!   gossip-driven "auto-discover" path and the operator-driven
//!   "manual add" path do not contend.
//! - Schema-version migration: the on-disk file carries a
//!   `version` field; future versions will add a `from_json_v1`
//!   migration step.
//!
//! ## API shape
//!
//! ```no_run
//! # use std::path::Path;
//! # use a3net_node::contacts_manager::ContactsManager;
//! let mgr = ContactsManager::open(Path::new("/tmp/a3net")).unwrap();
//! mgr.upsert_manual(node_id, "alice", 100).unwrap();
//! let list = mgr.snapshot();
//! assert_eq!(list.len(), 1);
//! ```

#![forbid(unsafe_code)]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use a3net_types::{
    ContactEntry, ContactSource, ContactsList, ContactsListError, NodeId, ReputationTier,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Filename for the on-disk contacts JSON document.
pub const CONTACTS_FILE_NAME: &str = "contacts.json";

/// Filename used during atomic-rename writes.
const CONTACTS_FILE_TMP: &str = "contacts.json.tmp";

/// Persistent contacts-list manager. Cheap to clone — the inner
/// state lives behind an [`RwLock`].
#[derive(Debug, Clone)]
pub struct ContactsManager {
    path: PathBuf,
    inner: std::sync::Arc<RwLock<ContactsList>>,
}

impl ContactsManager {
    /// Open (or create) the contacts list backed by `<dir>/contacts.json`.
    ///
    /// - If the file exists, deserialise it. A malformed file is
    ///   treated as a fatal error (the operator must inspect it
    ///   manually — silently overwriting would lose data).
    /// - If the file does not exist, create an empty list and
    ///   persist it.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(CONTACTS_FILE_NAME);
        let list = if path.exists() {
            let bytes = fs::read(&path)?;
            match serde_json::from_slice::<ContactsList>(&bytes) {
                Ok(l) => {
                    info!(
                        count = l.len(),
                        "loaded contacts list from disk"
                    );
                    l
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "failed to parse {}: {e}. \
                             Move the file aside and restart to rebuild.",
                            path.display()
                        ),
                    ));
                }
            }
        } else {
            debug!(
                path = %path.display(),
                "no contacts.json on disk; starting fresh"
            );
            ContactsList::new(now_secs())
        };

        let mgr = Self {
            path,
            inner: std::sync::Arc::new(RwLock::new(list)),
        };
        // Persist the freshly-created empty list so the file exists
        // for subsequent readers.
        if !mgr.path.exists() {
            mgr.persist()?;
        }
        Ok(mgr)
    }

    /// Snapshot the full list. Cheap O(N) clone.
    pub fn snapshot(&self) -> Vec<ContactEntry> {
        self.inner.read().snapshot()
    }

    /// Number of stored contacts.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// `true` when no contacts are stored.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// `true` when the list has reached the 5 000-entry cap.
    pub fn is_full(&self) -> bool {
        self.inner.read().is_full()
    }

    /// Look up a contact by their [`NodeId`].
    pub fn get(&self, node_id: &NodeId) -> Option<ContactEntry> {
        self.inner.read().get(node_id).cloned()
    }

    /// Look up a contact by their DNS-assigned 12-digit numeric id.
    pub fn get_by_dns(
        &self,
        dns: a3net_types::DnsNodeId,
    ) -> Option<ContactEntry> {
        self.inner.read().get_by_dns(dns).cloned()
    }

    /// Insert a contact the user just typed into the CLI.
    ///
    /// Errors when:
    /// - the local nickname is too long / contains NUL;
    /// - the list is already full ([`ContactsListError::Full`]).
    pub fn upsert_manual(
        &self,
        node_id: NodeId,
        nickname: impl Into<String>,
    ) -> Result<ContactEntry, ContactsListError> {
        let now = now_secs();
        let entry =
            ContactEntry::new_manual(node_id, nickname, now).map_err(|e| {
                ContactsListError::InvalidNickname(e.to_string())
            })?;
        let replaced = {
            let mut guard = self.inner.write();
            guard.upsert(entry.clone(), now)?
        };
        self.persist().map_err(map_persist_err)?;
        info!(
            node_id = %entry.node_id,
            nickname = %entry.nickname,
            replaced = replaced.is_some(),
            "contact added manually"
        );
        Ok(entry)
    }

    /// Insert or update a contact discovered via gossip profile
    /// announcements. Updates `last_seen_at` if the contact already
    /// exists; otherwise creates a new entry with the given
    /// nickname (which the user can later rename).
    pub fn upsert_from_gossip(
        &self,
        node_id: NodeId,
        nickname: impl Into<String>,
        dns_node_id: Option<a3net_types::DnsNodeId>,
    ) -> Result<ContactEntry, ContactsListError> {
        self.upsert_from_source(
            node_id,
            nickname,
            dns_node_id,
            ContactSource::Gossip,
        )
    }

    /// Generic helper for any auto-discovery source. Used by
    /// `upsert_from_gossip` and the future DHT / invite paths.
    pub fn upsert_from_source(
        &self,
        node_id: NodeId,
        nickname: impl Into<String>,
        dns_node_id: Option<a3net_types::DnsNodeId>,
        source: ContactSource,
    ) -> Result<ContactEntry, ContactsListError> {
        let now = now_secs();
        let mut entry =
            ContactEntry::new_manual(node_id, nickname, now).map_err(|e| {
                ContactsListError::InvalidNickname(e.to_string())
            })?;
        entry.source = source;
        if let Some(d) = dns_node_id {
            entry.set_dns_node_id(d);
        }
        entry.mark_seen(now);

        let replaced = {
            let mut guard = self.inner.write();
            guard.upsert(entry.clone(), now)?
        };
        self.persist().map_err(map_persist_err)?;
        debug!(
            node_id = %entry.node_id,
            source = source.as_str(),
            replaced = replaced.is_some(),
            "contact auto-discovered"
        );
        Ok(entry)
    }

    /// Rename an existing contact (local nickname only).
    pub fn rename(
        &self,
        node_id: &NodeId,
        nickname: impl Into<String>,
    ) -> Result<(), ContactsListError> {
        let now = now_secs();
        {
            let mut guard = self.inner.write();
            let entry = guard.get_mut(node_id).ok_or_else(|| {
                ContactsListError::NotFound(node_id.to_string())
            })?;
            entry.set_nickname(nickname).map_err(|e| {
                ContactsListError::InvalidNickname(e.to_string())
            })?;
            guard.touch(now);
        }
        self.persist().map_err(map_persist_err)?;
        Ok(())
    }

    /// Block / unblock a contact. Blocked contacts stay in the list
    /// (so the user can unblock them later) but the gossip / social
    /// layers suppress their content.
    pub fn set_blocked(
        &self,
        node_id: &NodeId,
        blocked: bool,
    ) -> Result<(), ContactsListError> {
        let now = now_secs();
        {
            let mut guard = self.inner.write();
            let entry = guard.get_mut(node_id).ok_or_else(|| {
                ContactsListError::NotFound(node_id.to_string())
            })?;
            entry.set_blocked(blocked);
            guard.touch(now);
        }
        self.persist().map_err(map_persist_err)?;
        Ok(())
    }

    /// Read the current reputation score for a contact. Returns
    /// `None` when the contact isn't in the list (rather than
    /// synthesizing a [`DEFAULT_REPUTATION`] — the caller needs to
    /// distinguish "unknown contact" from "neutral reputation").
    pub fn get_reputation(&self, node_id: &NodeId) -> Option<u32> {
        self.inner.read().get(node_id).map(|c| c.reputation())
    }

    /// Assign a new reputation value to a contact. Validates the
    /// value is in `[MIN_REPUTATION, MAX_REPUTATION]`. Persists
    /// synchronously. Returns the new value.
    pub fn set_reputation(
        &self,
        node_id: &NodeId,
        rep: u32,
    ) -> Result<u32, ContactsListError> {
        let now = now_secs();
        let new_value = {
            let mut guard = self.inner.write();
            let entry = guard.get_mut(node_id).ok_or_else(|| {
                ContactsListError::NotFound(node_id.to_string())
            })?;
            entry.set_reputation(rep)?;
            let value = entry.reputation();
            guard.touch(now);
            value
        };
        self.persist().map_err(map_persist_err)?;
        info!(
            node = %node_id,
            reputation = new_value,
            "contact reputation set"
        );
        Ok(new_value)
    }

    /// Increase a contact's reputation by `delta`, saturating at
    /// [`MAX_REPUTATION`]. Persists synchronously. Returns the
    /// resulting reputation score.
    ///
    /// `delta` is `u32` because the design intent is "increase
    /// trust"; lowering a reputation should be done via
    /// [`ContactsManager::set_reputation`] so the operator's intent
    /// is explicit and auditable in the logs.
    pub fn bump_reputation(
        &self,
        node_id: &NodeId,
        delta: u32,
    ) -> Result<u32, ContactsListError> {
        let now = now_secs();
        let (new_value, old_value) = {
            let mut guard = self.inner.write();
            let entry = guard.get_mut(node_id).ok_or_else(|| {
                ContactsListError::NotFound(node_id.to_string())
            })?;
            let old = entry.reputation();
            entry.bump_reputation(delta);
            let new = entry.reputation();
            guard.touch(now);
            (new, old)
        };
        self.persist().map_err(map_persist_err)?;
        if new_value != old_value {
            info!(
                node = %node_id,
                from = old_value,
                to = new_value,
                delta,
                "contact reputation bumped"
            );
        }
        Ok(new_value)
    }

    /// Snapshot of reputation tiers across the whole contacts
    /// list. Useful for the profile page (e.g. "Trusted by 12
    /// contacts, 3 of them highly-trusted") and for CLI health
    /// output.
    pub fn reputation_summary(&self) -> ReputationSummary {
        let guard = self.inner.read();
        let mut s = ReputationSummary::default();
        for entry in guard.entries.values() {
            match entry.reputation_tier() {
                ReputationTier::Untrusted => s.untrusted += 1,
                ReputationTier::Neutral => s.neutral += 1,
                ReputationTier::Trusted => s.trusted += 1,
                ReputationTier::HighlyTrusted => s.highly_trusted += 1,
            }
            s.total_score = s
                .total_score
                .saturating_add(u64::from(entry.reputation()));
            s.contacts += 1;
        }
        s
    }

    /// Mark the contact as seen *now* (no-op if the contact isn't
    /// in the list). Does NOT persist — `last_seen_at` updates are
    /// noisy under gossip load; we batch them via
    /// [`ContactsManager::persist_seen_updates`] instead.
    pub fn mark_seen(&self, node_id: &NodeId) -> bool {
        let now = now_secs();
        let mut guard = self.inner.write();
        if let Some(entry) = guard.get_mut(node_id) {
            entry.mark_seen(now);
            true
        } else {
            false
        }
    }

    /// Persist the current on-disk state after a batch of
    /// `mark_seen` calls.
    pub fn persist_seen_updates(&self) -> std::io::Result<()> {
        self.persist()
    }

    /// Remove a contact by [`NodeId`].
    pub fn remove(&self, node_id: &NodeId) -> Result<ContactEntry, ContactsListError> {
        let removed = {
            let mut guard = self.inner.write();
            guard.remove(node_id, now_secs())?
        };
        self.persist().map_err(map_persist_err)?;
        info!(
            node_id = %removed.node_id,
            "contact removed"
        );
        Ok(removed)
    }

    /// Bulk-import contacts from a Vec. Existing entries are
    /// overwritten; new entries that would exceed the cap are
    /// silently skipped (a soft policy: we don't want a single
    /// bulk import to fail entirely).
    ///
    /// Returns `(imported, skipped)` counts so the caller can
    /// report them to the user.
    pub fn bulk_import(
        &self,
        entries: Vec<ContactEntry>,
    ) -> (usize, usize) {
        let now = now_secs();
        let mut imported = 0usize;
        let mut skipped = 0usize;
        {
            let mut guard = self.inner.write();
            for entry in entries {
                let key = entry.node_id.as_hex().to_string();
                let already_present = guard.entries.contains_key(&key);
                if !already_present && guard.is_full() {
                    skipped += 1;
                    continue;
                }
                guard.entries.insert(key, entry);
                imported += 1;
            }
            guard.touch(now);
        }
        if imported > 0 {
            if let Err(e) = self.persist() {
                warn!(error = %e, "bulk_import: persist failed");
            }
        }
        (imported, skipped)
    }

    /// BLAKE3 digest of the current list. See
    /// [`ContactsList::digest`].
    pub fn digest(&self) -> [u8; 32] {
        self.inner.read().digest()
    }

    /// Approximate serialised size in bytes.
    pub fn approx_size(&self) -> usize {
        self.inner.read().approx_size()
    }

    /// Force a write of the in-memory state to disk. Called after
    /// every mutation by the public API; rarely needed by callers.
    pub fn persist(&self) -> std::io::Result<()> {
        let snapshot = {
            let guard = self.inner.read();
            serde_json::to_vec(&*guard).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("serialise contacts: {e}"),
                )
            })?
        };
        let tmp = self.path.with_file_name(CONTACTS_FILE_TMP);
        // Write to tmp file, fsync, then atomically rename. The
        // rename is atomic on POSIX (and `MoveFileEx` with the
        // replace-existing flag on Windows), so a crash never
        // leaves a half-written file visible.
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&snapshot)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        debug!(
            path = %self.path.display(),
            bytes = snapshot.len(),
            "contacts list persisted"
        );
        Ok(())
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn map_persist_err(e: std::io::Error) -> ContactsListError {
    ContactsListError::Serialization(e.to_string())
}

/// Aggregate reputation summary across the whole contacts list.
/// Returned by [`ContactsManager::reputation_summary`] and consumed
/// by the profile page and the `a3net contacts reputation` CLI
/// command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReputationSummary {
    /// Total number of contacts considered (excluding the local
    /// node itself, which is never in its own contacts list).
    pub contacts: u32,
    /// Sum of every contact's reputation score. Saturating — a
    /// huge contacts list cannot overflow.
    pub total_score: u64,
    /// Number of contacts in the
    /// [`ReputationTier::Untrusted`] bucket.
    pub untrusted: u32,
    /// Number of contacts in the
    /// [`ReputationTier::Neutral`] bucket.
    pub neutral: u32,
    /// Number of contacts in the
    /// [`ReputationTier::Trusted`] bucket.
    pub trusted: u32,
    /// Number of contacts in the
    /// [`ReputationTier::HighlyTrusted`] bucket.
    pub highly_trusted: u32,
}

impl ReputationSummary {
    /// Average reputation score across all contacts. Returns `0.0`
    /// when there are no contacts — this is intentional; a
    /// brand-new node has no notion of "average peer reputation".
    pub fn average_score(&self) -> f64 {
        if self.contacts == 0 {
            return 0.0;
        }
        // `total_score` is `u64`, but `f64` only has 53 bits of
        // mantissa — past 2^53 the conversion loses precision. With
        // a 5k-contact cap and a per-contact max of 1_000, the max
        // possible sum is 5_000_000, well under that bound.
        (self.total_score as f64) / (self.contacts as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::{DEFAULT_REPUTATION, DnsNodeId, MAX_REPUTATION};

    fn fresh() -> (tempfile::TempDir, ContactsManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ContactsManager::open(dir.path()).unwrap();
        (dir, mgr)
    }

    #[test]
    fn open_creates_file() {
        let (dir, mgr) = fresh();
        assert!(dir.path().join(CONTACTS_FILE_NAME).exists());
        assert_eq!(mgr.len(), 0);
    }

    #[test]
    fn open_loads_existing() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ContactsManager::open(dir.path()).unwrap();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        // Re-open and verify the entry survived.
        let mgr2 = ContactsManager::open(dir.path()).unwrap();
        assert_eq!(mgr2.len(), 1);
        let e = mgr2.get(&id).unwrap();
        assert_eq!(e.nickname, "alice");
    }

    #[test]
    fn open_rejects_malformed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONTACTS_FILE_NAME),
            b"this is not json",
        )
        .unwrap();
        let err = ContactsManager::open(dir.path()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn upsert_manual_validates_nickname() {
        let (_dir, mgr) = fresh();
        let err = mgr
            .upsert_manual(NodeId::random(), "ali\0ce")
            .unwrap_err();
        assert!(matches!(err, ContactsListError::InvalidNickname(_)));
    }

    #[test]
    fn upsert_manual_replaces_existing() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        mgr.upsert_manual(id.clone(), "alice-2").unwrap();
        let e = mgr.get(&id).unwrap();
        assert_eq!(e.nickname, "alice-2");
        assert_eq!(mgr.len(), 1);
    }

    #[test]
    fn upsert_from_gossip_sets_source() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        let dns = DnsNodeId::parse("483726150931").unwrap();
        let e = mgr
            .upsert_from_gossip(id.clone(), "bob", Some(dns))
            .unwrap();
        assert_eq!(e.source, ContactSource::Gossip);
        assert_eq!(e.dns_node_id, Some(dns));
        assert!(e.last_seen_at.is_some());
    }

    #[test]
    fn rename_updates_nickname() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        mgr.rename(&id, "alice-2").unwrap();
        assert_eq!(mgr.get(&id).unwrap().nickname, "alice-2");
        let err = mgr.rename(&NodeId::random(), "x").unwrap_err();
        assert!(matches!(err, ContactsListError::NotFound(_)));
    }

    #[test]
    fn rename_rejects_bad_nickname() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        let err = mgr.rename(&id, "ali\0ce").unwrap_err();
        assert!(matches!(err, ContactsListError::InvalidNickname(_)));
    }

    #[test]
    fn set_blocked_toggles() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        mgr.set_blocked(&id, true).unwrap();
        assert!(mgr.get(&id).unwrap().blocked);
        mgr.set_blocked(&id, false).unwrap();
        assert!(!mgr.get(&id).unwrap().blocked);
    }

    #[test]
    fn mark_seen_noop_for_unknown() {
        let (_dir, mgr) = fresh();
        assert!(!mgr.mark_seen(&NodeId::random()));
    }

    #[test]
    fn mark_seen_updates_existing() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        assert!(mgr.mark_seen(&id));
        let e = mgr.get(&id).unwrap();
        assert!(e.last_seen_at.is_some());
    }

    #[test]
    fn remove_returns_entry() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        let removed = mgr.remove(&id).unwrap();
        assert_eq!(removed.nickname, "alice");
        assert!(mgr.is_empty());
    }

    #[test]
    fn remove_unknown_not_found() {
        let (_dir, mgr) = fresh();
        let err = mgr.remove(&NodeId::random()).unwrap_err();
        assert!(matches!(err, ContactsListError::NotFound(_)));
    }

    #[test]
    fn bulk_import_creates_entries() {
        let (_dir, mgr) = fresh();
        let entries: Vec<_> = (0..5)
            .map(|i| {
                ContactEntry::new_manual(NodeId::random(), format!("u{i}"), 0).unwrap()
            })
            .collect();
        let (imported, skipped) = mgr.bulk_import(entries);
        assert_eq!(imported, 5);
        assert_eq!(skipped, 0);
        assert_eq!(mgr.len(), 5);
    }

    #[test]
    fn bulk_import_replaces_existing() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        let updated =
            ContactEntry::new_manual(id.clone(), "alice-2", 0).unwrap();
        let (imported, skipped) = mgr.bulk_import(vec![updated]);
        assert_eq!(imported, 1);
        assert_eq!(skipped, 0);
        assert_eq!(mgr.get(&id).unwrap().nickname, "alice-2");
    }

    #[test]
    fn digest_changes_on_mutation() {
        let (_dir, mgr) = fresh();
        let d0 = mgr.digest();
        mgr.upsert_manual(NodeId::random(), "x").unwrap();
        let d1 = mgr.digest();
        assert_ne!(d0, d1);
    }

    #[test]
    fn approx_size_grows() {
        let (_dir, mgr) = fresh();
        let s0 = mgr.approx_size();
        mgr.upsert_manual(NodeId::random(), "alice").unwrap();
        assert!(mgr.approx_size() > s0);
    }

    #[test]
    fn is_empty_and_full() {
        let (_dir, mgr) = fresh();
        assert!(mgr.is_empty());
        assert!(!mgr.is_full());
    }

    #[test]
    fn snapshot_is_ordered() {
        let (_dir, mgr) = fresh();
        mgr.upsert_manual(NodeId::random(), "a").unwrap();
        mgr.upsert_manual(NodeId::random(), "b").unwrap();
        let snap = mgr.snapshot();
        let it: Vec<_> = mgr
            .inner
            .read()
            .iter()
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(snap, it);
    }

    #[test]
    fn get_by_dns_finds() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        let dns = DnsNodeId::parse("483726150931").unwrap();
        mgr.upsert_from_gossip(id.clone(), "alice", Some(dns)).unwrap();
        let found = mgr.get_by_dns(dns).unwrap();
        assert_eq!(found.node_id, id);
    }

    #[test]
    fn persist_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ContactsManager::open(dir.path()).unwrap();
        mgr.upsert_manual(NodeId::random(), "alice").unwrap();
        // Re-open — must succeed even after many writes.
        for _ in 0..50 {
            let id = NodeId::random();
            mgr.upsert_manual(id, "x").unwrap();
        }
        let mgr2 = ContactsManager::open(dir.path()).unwrap();
        assert_eq!(mgr2.len(), 51);
    }

    #[test]
    fn reputation_get_returns_none_for_unknown() {
        let (_dir, mgr) = fresh();
        assert_eq!(mgr.get_reputation(&NodeId::random()), None);
    }

    #[test]
    fn reputation_set_persists_and_validates() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        // DEFAULT_REPUTATION is what new contacts start at.
        assert_eq!(mgr.get_reputation(&id), Some(DEFAULT_REPUTATION));
        let v = mgr.set_reputation(&id, 250).unwrap();
        assert_eq!(v, 250);
        assert_eq!(mgr.get_reputation(&id), Some(250));
        // Above MAX is rejected.
        assert!(matches!(
            mgr.set_reputation(&id, MAX_REPUTATION + 1),
            Err(ContactsListError::InvalidReputation(_))
        ));
        // NotFound for unknown id.
        let other = NodeId::random();
        assert!(matches!(
            mgr.set_reputation(&other, 100),
            Err(ContactsListError::NotFound(_))
        ));
    }

    #[test]
    fn reputation_bump_increases_with_saturate() {
        let (_dir, mgr) = fresh();
        let id = NodeId::random();
        mgr.upsert_manual(id.clone(), "alice").unwrap();
        let v = mgr.bump_reputation(&id, 50).unwrap();
        assert_eq!(v, DEFAULT_REPUTATION + 50);
        let v = mgr.bump_reputation(&id, MAX_REPUTATION).unwrap();
        assert_eq!(v, MAX_REPUTATION);
        // Further bumps stay at MAX.
        let v = mgr.bump_reputation(&id, 1_000).unwrap();
        assert_eq!(v, MAX_REPUTATION);
    }

    #[test]
    fn reputation_bump_unknown_contact_errors() {
        let (_dir, mgr) = fresh();
        assert!(matches!(
            mgr.bump_reputation(&NodeId::random(), 10),
            Err(ContactsListError::NotFound(_))
        ));
    }

    #[test]
    fn reputation_summary_empty() {
        let (_dir, mgr) = fresh();
        let s = mgr.reputation_summary();
        assert_eq!(s.contacts, 0);
        assert_eq!(s.total_score, 0);
        assert_eq!(s.untrusted, 0);
        assert_eq!(s.neutral, 0);
        assert_eq!(s.trusted, 0);
        assert_eq!(s.highly_trusted, 0);
        assert_eq!(s.average_score(), 0.0);
    }

    #[test]
    fn reputation_summary_buckets() {
        let (_dir, mgr) = fresh();
        let mut ids = Vec::new();
        for i in 0..5 {
            let id = NodeId::random();
            mgr.upsert_manual(id.clone(), &format!("user{i}"), ).unwrap();
            ids.push(id);
        }
        mgr.set_reputation(&ids[0], 50).unwrap(); // untrusted
        mgr.set_reputation(&ids[1], 100).unwrap(); // neutral
        mgr.set_reputation(&ids[2], 500).unwrap(); // trusted
        mgr.set_reputation(&ids[3], 900).unwrap(); // highly-trusted
        // ids[4] stays at DEFAULT_REPUTATION (neutral).
        let s = mgr.reputation_summary();
        assert_eq!(s.contacts, 5);
        assert_eq!(s.untrusted, 1);
        assert_eq!(s.neutral, 2); // ids[1] and ids[4]
        assert_eq!(s.trusted, 1);
        assert_eq!(s.highly_trusted, 1);
        assert_eq!(s.total_score, 50 + 100 + 500 + 900 + DEFAULT_REPUTATION as u64);
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ContactsManager>();
    }

    // ── Property tests ──────────────────────────────────────────────────
    //
    // Probes the persistence and reputation invariants across
    // arbitrary input. Catches the sort of "edge case past 4
    // billion" bug that hand-written tests miss.

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        // Keep proptest's default case count low so the test
        // suite stays under a minute on slow hardware. 16 cases
        // per property is enough to catch the saturation /
        // overflow bugs.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(16))]

            #[test]
            fn bump_clamps_at_max(start in 0u32..=MAX_REPUTATION, delta in 0u32..5000) {
                let (_dir, mgr) = fresh();
                let id = NodeId::random();
                mgr.upsert_manual(id.clone(), "x").unwrap();
                mgr.set_reputation(&id, start).unwrap();
                let after = mgr.bump_reputation(&id, delta).unwrap();
                prop_assert!(after >= start);
                prop_assert!(after <= MAX_REPUTATION);
            }

            #[test]
            fn set_then_get_round_trip(rep in 0u32..=MAX_REPUTATION) {
                let (_dir, mgr) = fresh();
                let id = NodeId::random();
                mgr.upsert_manual(id.clone(), "x").unwrap();
                mgr.set_reputation(&id, rep).unwrap();
                prop_assert_eq!(mgr.get_reputation(&id), Some(rep));
            }
        }
    }
}
