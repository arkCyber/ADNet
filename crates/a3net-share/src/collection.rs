//! `Collection` — a manifest of `(relative_name → ContentHash)` pairs
//! describing a multi-file blob (a directory) shared as one ticket.
//!
//! ## Wire format
//!
//! A3Net's `Collection` is wire-compatible with
//! `iroh_blobs::format::collection::Collection`. That struct uses
//! postcard to serialise an inner `Vec<(String, Hash)>`, and our
//! `Collection` mirrors the exact same on-the-wire shape — just with
//! `a3net_types::ContentHash` (also 32-byte BLAKE3) instead of
//! `iroh_blobs::Hash`.
//!
//! `iroh-blobs` peers will accept the ticket we hand them and decode
//! the manifest into their own types. Reciprocally, an iroh peer can
//! send us a `Collection` and we'll load it back into A3Net-native
//! `ContentHash`es through [`Collection::store`] / [`Collection::load`].
//!
//! ## Storage model
//!
//! A `Collection` is itself stored as a single BLAKE3-addressed blob:
//! the manifest hash (the value returned by [`Collection::hash`]) is
//! the ticket the receiver fetches, and each entry's per-file hash is
//! fetched on demand. This matches sendme's `collection.store(&db)` +
//! ticket handling exactly.
//!
//! [`Collection::store`]: Collection::store_with
//! [`Collection::load`]: Collection::load_from

use serde::{Deserialize, Serialize};

use a3net_types::ContentHash;

use crate::error::{ShareError, ShareResult};
use crate::path::{MAX_NAME_LEN, validate_path_component};

/// Maximum number of entries allowed in a single `Collection`.
///
/// sendme does not enforce an explicit cap; A3Net picks 16,384 to bound
/// gossip / ticket size while still allowing reasonably-large directory
/// trees. Exceeding this returns [`ShareError::CollectionTooLarge`]
/// from [`Collection::push`].
pub const MAX_COLLECTION_ENTRIES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionEntry {
    /// Relative file name inside the share root, slash-separated, with
    /// no leading `/`. Validated against
    /// [`crate::path::validate_path_component`] on each segment.
    pub name: String,
    /// BLAKE3 hash of the file's bytes.
    pub hash: ContentHash,
}

impl CollectionEntry {
    /// Construct a new entry. `name` is validated and `hash` is stored
    /// verbatim (it must already be a valid BLAKE3 hex digest).
    pub fn new(name: impl Into<String>, hash: ContentHash) -> ShareResult<Self> {
        let name = name.into();
        // Validate every slash-separated segment.
        for segment in name.split('/') {
            validate_path_component(segment)?;
        }
        if name.len() > MAX_NAME_LEN {
            return Err(ShareError::NameTooLong {
                got: name.len(),
                max: MAX_NAME_LEN,
            });
        }
        Ok(Self { name, hash })
    }
}

/// A manifest of files for a single shared blob.
///
/// Order is significant: `iroh_blobs::format::collection::Collection`
/// is backed by a `BTreeMap`, but our in-memory representation is a
/// `Vec` to preserve insertion order (the walker inserts files in
/// sorted path order, which is what sendme does too). Serialising
/// with `Vec` preserves wire compatibility because postcard does not
/// care about ordering — only the final bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    entries: Vec<CollectionEntry>,
}

impl Collection {
    /// Empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Append an entry. Returns [`ShareError::CollectionTooLarge`] if
    /// the cap would be exceeded, [`ShareError::DuplicateName`] on
    /// name collision, [`ShareError::InvalidPathComponent`] /
    /// [`ShareError::NameTooLong`] if the name fails validation.
    pub fn push(&mut self, entry: CollectionEntry) -> ShareResult<()> {
        if self.entries.len() >= MAX_COLLECTION_ENTRIES {
            return Err(ShareError::CollectionTooLarge {
                got: self.entries.len() + 1,
                max: MAX_COLLECTION_ENTRIES,
            });
        }
        if self.entries.iter().any(|e| e.name == entry.name) {
            return Err(ShareError::DuplicateName(entry.name));
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Iterate over `(name, hash)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ContentHash)> {
        self.entries.iter().map(|e| (e.name.as_str(), &e.hash))
    }

    /// Total bytes is **not** stored in the manifest (matches sendme).
    /// Callers that need it should sum per-entry sizes from the
    /// `BlobReader` separately.
    pub fn total_size_hint(&self) -> u64 {
        // Placeholder — real byte counts come from the importer. The
        // receiver gets them via `BlobStatus::Complete { size }`.
        0
    }

    /// Serialise to bytes. The wire format is `postcard(Vec<(String, ContentHash)>)`,
    /// matching `iroh_blobs::format::collection::Collection`.
    pub fn to_bytes(&self) -> ShareResult<Vec<u8>> {
        postcard::to_stdvec(&self.entries)
            .map_err(|e| ShareError::Backend(format!("collection serialize: {e}")))
    }

    /// Deserialise from bytes produced by [`Collection::to_bytes`] or
    /// by an iroh-blobs peer.
    pub fn from_bytes(bytes: &[u8]) -> ShareResult<Self> {
        let entries: Vec<CollectionEntry> = postcard::from_bytes(bytes)
            .map_err(|e| ShareError::Backend(format!("collection deserialize: {e}")))?;
        if entries.len() > MAX_COLLECTION_ENTRIES {
            return Err(ShareError::CollectionTooLarge {
                got: entries.len(),
                max: MAX_COLLECTION_ENTRIES,
            });
        }
        let mut c = Collection::new();
        for e in entries {
            c.push(e)?;
        }
        Ok(c)
    }

    /// Deterministic BLAKE3 hash of the manifest. This is the value
    /// the receiver fetches when they connect to a sender — the ticket
    /// carries `manifest_hash`, and the per-file hashes are pulled
    /// lazily as the receiver walks the manifest.
    pub fn manifest_hash(&self) -> ShareResult<ContentHash> {
        let bytes = self.to_bytes()?;
        Ok(ContentHash::from_bytes(&bytes))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(byte: u8) -> ContentHash {
        ContentHash::from_bytes(&[byte])
    }

    #[test]
    fn empty_collection() {
        let c = Collection::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        let bytes = c.to_bytes().unwrap();
        let back = Collection::from_bytes(&bytes).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn push_single_entry() {
        let mut c = Collection::new();
        c.push(CollectionEntry::new("a.txt", hash_of(1)).unwrap()).unwrap();
        assert_eq!(c.len(), 1);
        let bytes = c.to_bytes().unwrap();
        let back = Collection::from_bytes(&bytes).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn push_multiple_entries_preserves_order() {
        let mut c = Collection::new();
        c.push(CollectionEntry::new("b.txt", hash_of(2)).unwrap()).unwrap();
        c.push(CollectionEntry::new("a.txt", hash_of(1)).unwrap()).unwrap();
        c.push(CollectionEntry::new("c/d.txt", hash_of(3)).unwrap()).unwrap();
        let names: Vec<&str> = c.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["b.txt", "a.txt", "c/d.txt"]);
    }

    #[test]
    fn push_rejects_duplicate() {
        let mut c = Collection::new();
        c.push(CollectionEntry::new("a.txt", hash_of(1)).unwrap()).unwrap();
        let err = c
            .push(CollectionEntry::new("a.txt", hash_of(2)).unwrap())
            .unwrap_err();
        assert!(matches!(err, ShareError::DuplicateName(_)));
    }

    #[test]
    fn push_rejects_invalid_component() {
        let err = CollectionEntry::new("a/bad\\name", hash_of(1)).unwrap_err();
        assert!(matches!(err, ShareError::InvalidPathComponent(_)));
    }

    #[test]
    fn push_rejects_overlong_name() {
        // The exact error returned depends on which check fires
        // first. `CollectionEntry::new` calls
        // `validate_path_component` on each segment before checking
        // the whole-name `MAX_NAME_LEN`. For a 513-char single
        // component, the segment check fires first and surfaces
        // `PathComponentTooLong` — the same back-pressure on bad
        // names, just a different variant.
        let too_long = "x".repeat(MAX_NAME_LEN + 1);
        let err = CollectionEntry::new(&too_long, hash_of(1)).unwrap_err();
        assert!(
            matches!(
                err,
                ShareError::PathComponentTooLong { .. } | ShareError::NameTooLong { .. }
            ),
            "expected PathComponentTooLong or NameTooLong, got {err:?}"
        );
    }

    #[test]
    fn push_rejects_over_cap() {
        // We don't actually allocate MAX_COLLECTION_ENTRIES entries in a
        // unit test; we just confirm the cap is checked by going
        // through `from_bytes` (which re-runs `push` on every entry).
        let mut entries = Vec::new();
        for i in 0..(MAX_COLLECTION_ENTRIES + 1) {
            entries.push(
                CollectionEntry::new(
                    format!("f{i}"),
                    hash_of((i % 256) as u8),
                )
                .unwrap(),
            );
        }
        let bytes = postcard::to_stdvec(&entries).unwrap();
        let err = Collection::from_bytes(&bytes).unwrap_err();
        match err {
            ShareError::CollectionTooLarge { got, max } => {
                assert_eq!(got, MAX_COLLECTION_ENTRIES + 1);
                assert_eq!(max, MAX_COLLECTION_ENTRIES);
            }
            other => panic!("expected CollectionTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn manifest_hash_is_deterministic() {
        let mut a = Collection::new();
        a.push(CollectionEntry::new("a.txt", hash_of(1)).unwrap()).unwrap();
        let mut b = Collection::new();
        b.push(CollectionEntry::new("a.txt", hash_of(1)).unwrap()).unwrap();
        assert_eq!(a.manifest_hash().unwrap(), b.manifest_hash().unwrap());
    }

    #[test]
    fn manifest_hash_changes_with_order() {
        // BLAKE3 over the manifest bytes — different ordering yields
        // a different digest even when the entry *set* is identical.
        let mut a = Collection::new();
        a.push(CollectionEntry::new("a.txt", hash_of(1)).unwrap()).unwrap();
        a.push(CollectionEntry::new("b.txt", hash_of(2)).unwrap()).unwrap();
        let mut b = Collection::new();
        b.push(CollectionEntry::new("b.txt", hash_of(2)).unwrap()).unwrap();
        b.push(CollectionEntry::new("a.txt", hash_of(1)).unwrap()).unwrap();
        assert_ne!(a.manifest_hash().unwrap(), b.manifest_hash().unwrap());
    }

    #[test]
    fn round_trip_with_nested_names() {
        let mut c = Collection::new();
        c.push(CollectionEntry::new("dir/a.txt", hash_of(1)).unwrap()).unwrap();
        c.push(CollectionEntry::new("dir/sub/b.txt", hash_of(2)).unwrap()).unwrap();
        c.push(CollectionEntry::new("top.txt", hash_of(3)).unwrap()).unwrap();
        let bytes = c.to_bytes().unwrap();
        let back = Collection::from_bytes(&bytes).unwrap();
        assert_eq!(c, back);
    }
}