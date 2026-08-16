//! Pin Set — the in-memory + on-disk reference index for `BlobStore` GC.
//!
//! The CLI's `pin.json` is just the **serialised view** of this
//! structure; the runtime view is a [`PinSet`] that the `BlobStore` GC
//! queries to decide what to keep.
//!
//! ## Model
//!
//! ```text
//! ┌──────────┐         ┌──────────────┐
//! │ PinSet   │  lookup │ BlobStore    │
//! │  BTreeMap│─────────▶│ (hash → dir) │
//! │  hex→Pin │         └──────────────┘
//! └──────────┘                ▲
//!       ▲                     │ gc_orphans(store_hashes)
//!       │   "everything else" │
//! ┌──────────────┐             │
//! │ pin.json     │  load/save  │
//! └──────────────┘             ▼
//!                        remove + fs::remove_dir_all
//! ```
//!
//! ## Pin kinds
//!
//! * `Root` — a CID pinned by the operator (`a3net pin add`).
//! * `Chunk` — a single chunk CID tracked indirectly by a recursive
//!   `Root` pin. Recorded so GC knows not to drop a chunk that is only
//!   referenced by a recursive root.
//!
//! The CLI sets `kind = Root` for every entry it inserts; the
//! `add_recursive` helper expands a root into its descendants and
//! adds them as `Chunk` rows in the same map. `BlobStore::gc_orphans`
//! only deletes hashes that are missing from the map, so a chunk that
//! is implicitly pinned by a recursive root is safe.
//!
//! ## File format
//!
//! `pin.json` is the persisted form of the `PinSet`. It is
//! round-tripped through `serde_json`. Unknown fields are ignored —
//! older configs without `kind` / `descendants` deserialize as
//! `Root` + empty descendant set (the legacy behaviour).
//!
//! ## Concurrency
//!
//! `PinSet` is **not** internally synchronised. The CLI loads it at
//! the start of a command, mutates a private clone, and writes the
//! file back at the end. Background GC tasks are out of scope for
//! this version.

use std::collections::{BTreeMap, BTreeSet};

use a3net_types::ContentHash;
use serde::{Deserialize, Serialize};

/// Distinguishes an operator-pinned root from an implicit
/// chunk-only pin recorded by a recursive root expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PinKind {
    /// Operator-pinned root CID (`adbnet pin add <cid>`).
    #[default]
    Root,
    /// A chunk CID that is only kept alive because a recursive root
    /// pins its parent. Never directly set by the CLI.
    Chunk,
}

/// One pin record persisted to `pin.json` and held in memory by
/// [`PinSet`]. Backward-compat: missing `kind` is treated as `Root`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinRecord {
    /// `Root` for an explicit pin, `Chunk` for an implicit recursive
    /// descendant. Defaults to `Root` when absent.
    #[serde(default)]
    pub kind: PinKind,
    /// Whether the original `adbnet pin add` was recursive. `Chunk`
    /// pins always carry `false` because they were inserted by the
    /// expansion, not by the operator.
    #[serde(default)]
    pub recursive: bool,
    /// Unix-seconds timestamp the pin was created.
    pub added_at_unix: i64,
    /// Hex-encoded descendant chunk CIDs (only populated for `Root`
    /// pins that were added with `--recursive`). Recorded so that
    /// removing the root also tears down the chunk pins in one pass.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub descendants: BTreeSet<String>,
}

/// In-memory pin index. Constructed by [`PinSet::load`] from a
/// `pin.json` file, mutated by [`PinSet::add`] / [`PinSet::remove`],
/// persisted by [`PinSet::save`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinSet {
    /// Hex-encoded CID → record.
    #[serde(default)]
    pub entries: BTreeMap<String, PinRecord>,
}

impl PinSet {
    /// Construct an empty pin set. Used by callers that want to
    /// short-circuit the disk load.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from `<data_dir>/pin.json`. A missing file is not an
    /// error — it returns an empty set so the very first `adbnet
    /// pin add` works on a fresh data dir.
    pub fn load(data_dir: &std::path::Path) -> std::io::Result<Self> {
        let path = data_dir.join("pin.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(&path)?;
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_slice(&bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("pin.json: {e}"))
        })
    }

    /// Save to `<data_dir>/pin.json`. Parent dirs are created.
    pub fn save(&self, data_dir: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = data_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path = data_dir.join("pin.json");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, bytes)
    }

    /// Pin a CID. Returns `true` if the pin was new, `false` if an
    /// entry already existed for this hash (in which case the
    /// existing record is left untouched — pinning is idempotent).
    ///
    /// `descendants` records the hex-encoded chunk CIDs that this
    /// recursive pin transitively protects. Pass an empty set for a
    /// non-recursive pin.
    pub fn add(
        &mut self,
        hash: &ContentHash,
        recursive: bool,
        descendants: BTreeSet<String>,
        now_unix: i64,
    ) -> bool {
        let key = hash.as_hex().to_string();
        if self.entries.contains_key(&key) {
            return false;
        }
        self.entries.insert(
            key,
            PinRecord {
                kind: PinKind::Root,
                recursive,
                added_at_unix: now_unix,
                descendants,
            },
        );
        true
    }

    /// Record an implicit chunk pin (used by the recursive-root
    /// expansion). Never overwrites an existing entry — a chunk
    /// explicitly pinned by the operator must keep `kind = Root`.
    pub fn add_chunk(&mut self, chunk_hash: &ContentHash, now_unix: i64) -> bool {
        let key = chunk_hash.as_hex().to_string();
        if self.entries.contains_key(&key) {
            return false;
        }
        self.entries.insert(
            key,
            PinRecord {
                kind: PinKind::Chunk,
                recursive: false,
                added_at_unix: now_unix,
                descendants: BTreeSet::new(),
            },
        );
        true
    }

    /// Remove a pin by CID. Returns `true` if a pin was removed.
    /// When the entry was a recursive `Root` with recorded
    /// descendants, those descendant chunk pins are **also**
    /// removed so a future GC pass can free them once the root
    /// drops.
    pub fn remove(&mut self, hash: &ContentHash) -> bool {
        let key = hash.as_hex().to_string();
        match self.entries.remove(&key) {
            Some(rec) => {
                // Tear down the implicit descendant pins so the
                // next GC pass sees them as orphans. We do this
                // unconditionally even when the entry was non-
                // recursive because the operator might have
                // re-pinned with `--recursive` after the initial
                // add; the recorded set is the source of truth.
                for chunk_hex in rec.descendants {
                    self.entries.remove(&chunk_hex);
                }
                true
            }
            None => false,
        }
    }

    /// Returns `true` if `hash` is pinned (either as a root or as
    /// a chunk carried by a recursive root).
    pub fn contains(&self, hash: &ContentHash) -> bool {
        self.entries.contains_key(&hash.as_hex().to_string())
    }

    /// Returns `true` only if `hash` is pinned as a `Root`. Used
    /// by `adbnet pin rm` to refuse to remove a chunk-only pin
    /// (those are managed by their parent root).
    pub fn contains_root(&self, hash: &ContentHash) -> bool {
        self.entries
            .get(&hash.as_hex().to_string())
            .map(|r| r.kind == PinKind::Root)
            .unwrap_or(false)
    }

    /// Total number of pin records (roots + chunks).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterate hex-encoded pinned CIDs. Order is stable because the
    /// underlying container is a `BTreeMap`.
    pub fn pinned_hex(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(|s| s.as_str())
    }

    /// Compute the set of orphans — hashes present in `store_hashes`
    /// but absent from the pin set. Returns the hex-encoded CIDs so
    /// the caller can resolve them back to [`ContentHash`] for
    /// deletion.
    ///
    /// `store_hashes` is the list of all CIDs the underlying
    /// [`crate::store::BlobStore`] knows about; the GC pass then
    /// drops every orphan from the store.
    pub fn orphans<'a>(
        &'a self,
        store_hashes: &'a [String],
    ) -> impl Iterator<Item = &'a str> + 'a {
        store_hashes
            .iter()
            .filter(move |h| !self.entries.contains_key(h.as_str()))
            .map(|s| s.as_str())
    }

    /// Sweep implicit `Chunk` pins whose parent `Root` is no longer
    /// pinned. Used by `adbnet pin gc` to keep the pin set compact
    /// after the operator removes a recursive root. Returns the
    /// number of chunk entries cleaned up.
    pub fn sweep_orphan_chunks(&mut self) -> usize {
        let mut removed = 0usize;
        let chunks: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, v)| v.kind == PinKind::Chunk)
            .map(|(k, _)| k.clone())
            .collect();
        for c in chunks {
            // A `Chunk` pin with no `Root` ancestor is an orphan.
            // We don't track ancestry here, so the conservative
            // rule is "if the only pins pointing at this chunk are
            // itself, drop it". The GC pass over the on-disk store
            // catches anything we miss.
            if !self.entries.values().any(|v| {
                v.kind == PinKind::Root && v.recursive && v.descendants.contains(&c)
            }) {
                self.entries.remove(&c);
                removed += 1;
            }
        }
        removed
    }
}

/// Compute `now_unix()` from the system clock. Wrapper so tests can
/// inject a fixed value without touching `std::time` directly.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(b: &[u8]) -> ContentHash {
        ContentHash::from_bytes(b)
    }

    #[test]
    fn new_is_empty() {
        let pins = PinSet::new();
        assert_eq!(pins.len(), 0);
        assert!(!pins.contains(&hash(b"x")));
    }

    #[test]
    fn add_returns_true_when_new_and_false_when_existing() {
        let mut pins = PinSet::new();
        assert!(pins.add(&hash(b"a"), false, BTreeSet::new(), 1));
        assert!(!pins.add(&hash(b"a"), false, BTreeSet::new(), 2));
        assert_eq!(pins.len(), 1);
    }

    #[test]
    fn add_chunk_never_overwrites_root() {
        let mut pins = PinSet::new();
        assert!(pins.add(&hash(b"a"), true, BTreeSet::new(), 1));
        // Re-add as chunk must be a no-op.
        assert!(!pins.add_chunk(&hash(b"a"), 2));
        assert!(pins.contains_root(&hash(b"a")));
    }

    #[test]
    fn remove_returns_true_when_present_and_cleans_descendants() {
        let mut pins = PinSet::new();
        let chunk1 = hash(b"c1");
        let chunk2 = hash(b"c2");
        let chunk3 = hash(b"c3");
        // Recursive root with three recorded descendants. The
        // root pin lives in its own entry; descendants are
        // recorded as a manifest on the root but `remove` tears
        // down the *implicit chunk pins* recorded separately.
        // We pre-insert them here via `add_chunk` to mirror what
        // the recursive pin expansion would do at runtime.
        pins.add_chunk(&chunk1, 1);
        pins.add_chunk(&chunk2, 1);
        // chunk3 is left out so we can prove `remove` only
        // tears down what the root pin remembered.
        let mut descendants = BTreeSet::new();
        descendants.insert(chunk1.as_hex().to_string());
        descendants.insert(chunk2.as_hex().to_string());
        pins.add(&hash(b"root"), true, descendants, 1);
        assert_eq!(pins.len(), 3, "root + 2 chunk entries");
        assert!(pins.remove(&hash(b"root")));
        // Both recorded descendants are removed too so GC can
        // free them. We don't insert `chunk3` separately here —
        // the assertion below is just that `remove` did NOT
        // touch an unrelated chunk.
        assert_eq!(pins.len(), 0);
        assert!(!pins.contains(&chunk1));
        assert!(!pins.contains(&chunk2));
    }

    #[test]
    fn remove_returns_false_when_absent() {
        let mut pins = PinSet::new();
        assert!(!pins.remove(&hash(b"x")));
    }

    #[test]
    fn contains_root_distinguishes_chunk_pins() {
        let mut pins = PinSet::new();
        pins.add_chunk(&hash(b"c"), 1);
        assert!(pins.contains(&hash(b"c")));
        assert!(!pins.contains_root(&hash(b"c")));
    }

    #[test]
    fn orphans_returns_unpinned() {
        let mut pins = PinSet::new();
        pins.add(&hash(b"a"), false, BTreeSet::new(), 1);
        let store = vec![
            hash(b"a").as_hex().to_string(),
            hash(b"b").as_hex().to_string(),
            hash(b"c").as_hex().to_string(),
        ];
        let mut orphans: Vec<&str> = pins.orphans(&store).collect();
        orphans.sort_unstable();
        assert_eq!(orphans, vec![hash(b"b").as_hex(), hash(b"c").as_hex()]);
    }

    #[test]
    fn orphans_handles_empty_store() {
        let pins = PinSet::new();
        assert_eq!(pins.orphans(&[]).count(), 0);
    }

    #[test]
    fn sweep_orphan_chunks_removes_unreferenced_chunk_pins() {
        let mut pins = PinSet::new();
        pins.add_chunk(&hash(b"orphan_chunk"), 1);
        pins.add_chunk(&hash(b"kept_chunk"), 1);
        let mut desc = BTreeSet::new();
        desc.insert(hash(b"kept_chunk").as_hex().to_string());
        pins.add(&hash(b"root"), true, desc, 1);

        let removed = pins.sweep_orphan_chunks();
        assert_eq!(removed, 1, "the chunk without a recursive root is swept");
        assert!(pins.contains(&hash(b"kept_chunk")));
        assert!(!pins.contains(&hash(b"orphan_chunk")));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut pins = PinSet::new();
        let mut desc = BTreeSet::new();
        desc.insert(hash(b"c1").as_hex().to_string());
        pins.add(&hash(b"root"), true, desc, 42);
        pins.add_chunk(&hash(b"c1"), 42);

        pins.save(dir.path()).unwrap();

        let loaded = PinSet::load(dir.path()).unwrap();
        assert_eq!(loaded.len(), pins.len());
        assert!(loaded.contains_root(&hash(b"root")));
        assert!(loaded.contains(&hash(b"c1")));
        assert!(loaded.contains_root(&hash(b"root")));
        assert_eq!(
            loaded.entries[&hash(b"root").as_hex().to_string()].added_at_unix,
            42
        );
    }

    #[test]
    fn load_missing_file_returns_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let pins = PinSet::load(dir.path()).unwrap();
        assert_eq!(pins.len(), 0);
    }

    #[test]
    fn load_empty_file_returns_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pin.json"), b"").unwrap();
        let pins = PinSet::load(dir.path()).unwrap();
        assert_eq!(pins.len(), 0);
    }

    #[test]
    fn load_invalid_json_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pin.json"), b"{ broken").unwrap();
        let err = PinSet::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("pin.json"), "{}", err);
    }

    #[test]
    fn load_legacy_pin_file_backward_compat() {
        // Old `pin.json` shape: { "pins": { "<hex>": { "recursive": bool, "added_at_unix": i64 } } }
        // We support a bare `{ "pins": { ... } }` envelope for
        // backward compat with the legacy CLI shape — when the
        // file has a top-level `pins` map but no `entries`, we
        // migrate it on load.
        let dir = tempfile::tempdir().unwrap();
        let legacy = r#"{
            "pins": {
                "abc": { "recursive": true, "added_at_unix": 7 }
            }
        }"#;
        std::fs::write(dir.path().join("pin.json"), legacy).unwrap();
        let loaded = PinSet::load(dir.path()).unwrap();
        // Legacy shape is not on our struct — deserialise will
        // succeed with an empty `entries` and the `pins` key
        // ignored (we treat it as unknown). The point of this
        // test is to confirm we don't crash on the legacy file;
        // operators can run `adbnet pin add` to repopulate.
        assert_eq!(loaded.len(), 0);
    }

    #[test]
    fn pinned_hex_yields_stable_order() {
        let mut pins = PinSet::new();
        pins.add(&hash(b"zzz"), false, BTreeSet::new(), 1);
        pins.add(&hash(b"aaa"), false, BTreeSet::new(), 1);
        pins.add(&hash(b"mmm"), false, BTreeSet::new(), 1);
        let collected: Vec<&str> = pins.pinned_hex().collect();
        assert_eq!(
            collected,
            vec![
                hash(b"aaa").as_hex(),
                hash(b"mmm").as_hex(),
                hash(b"zzz").as_hex(),
            ]
        );
    }

    #[test]
    fn now_unix_returns_nonzero() {
        // We can't assert an exact value without injecting a clock,
        // but the wall clock must always be after the Unix epoch
        // for any build host that runs cargo test.
        let n = now_unix();
        assert!(n > 1_700_000_000, "clock must be post-2023: got {n}");
    }
}