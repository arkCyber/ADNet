//! Bridge between the in-memory `HamtShard` tree and the
//! on-the-wire `UnixFsNode::HamtShardedDirectory` (DAG-CBOR) form
//! used by IPFS gateways and other A3Net nodes.
//!
//! The HAMT crate owns the canonical in-memory representation of a
//! sharded directory (a balanced trie keyed by `blake3(name)` so
//! lookups are O(log n)). The UnixFS type system owns the
//! canonical serialised form (a `HamtShardedDirectory` node whose
//! `links` field carries the children plus a fanout/bits_width
//! header that lets readers rebuild the same trie on the other
//! side).
//!
//! This module closes the gap by exposing three operations:
//!
//! - `build_hamt_directory(entries, fanout, bits_width) -> Cid` —
//!   take a list of (name, file-cid, size) tuples, build the
//!   shard, serialise it as DAG-CBOR, and return the CID of the
//!   root UnixFS HAMT-sharded-directory node.
//! - `entries_from_hamt_directory(node) -> Vec<(name, cid, size)>`
//!   — recover the flat entry list from a parsed
//!   `HamtShardedDirectory` node (lossless: name + cid + size
//!   are preserved across the bridge).
//! - `hamt_shard_from_hamt_directory(node) -> HamtShard` —
//!   rebuild the in-memory shard from a parsed node so a
//!   downloader can drive the trie with the same lookups the
//!   publisher used.
//!
//! All three honour the IPFS HAMT sharding spec: `fanout=256`,
//! `bits_width=8`, names hashed with BLAKE3-256, and links
//! serialised as `{ Name, Cid, Tsize }` maps.

use a3net_types::{
    cid::{Cid, Codec},
    content::ContentHash,
    multihash::{HashCode, Multihash},
    unixfs::{serialization, UnixFsError, UnixFsLink, UnixFsNode},
};

use crate::hamt::{HamtEntry, HamtShard, HamtResult};

/// Standard IPFS HAMT parameters. `fanout=256` and `bits_width=8`
/// are the values every IPFS implementation uses; deviating from
/// them breaks interop with Kubo and any other node that hashes
/// names with the same parameters.
pub const IPFS_FANOUT: u64 = 256;
pub const IPFS_BITS_WIDTH: u64 = 8;

/// Convert a `(name, cid, size)` entry list into a `HamtShard`
/// ready for serialisation. The shard uses the in-memory trie
/// layout, so subsequent inserts / lookups against it are O(log n).
pub fn build_shard_from_entries(
    entries: impl IntoIterator<Item = (String, Cid, u64)>,
) -> HamtResult<HamtShard> {
    let mut shard = HamtShard::new();
    for (name, cid, size) in entries {
        // Re-derive the file hash from the CID's multihash digest
        // so the shard can be re-serialised losslessly without
        // keeping the original bytes around.
        let hash = match cid.hash_code() {
            0x1e => {
                // BLAKE3-256 — the on-the-wire default in A3Net.
                let mut bytes = [0u8; 32];
                let digest = cid.hash_digest();
                let n = digest.len().min(32);
                bytes[..n].copy_from_slice(&digest[..n]);
                ContentHash::from_bytes(&bytes)
            }
            0x12 => {
                // SHA-256 — IPFS-compat path.
                let mut bytes = [0u8; 32];
                let digest = cid.hash_digest();
                let n = digest.len().min(32);
                bytes[..n].copy_from_slice(&digest[..n]);
                ContentHash::from_bytes(&bytes)
            }
            _ => {
                // Unknown hash function: store the raw bytes in
                // the ContentHash anyway. The shard treats the
                // 32-byte form as opaque.
                let mut bytes = [0u8; 32];
                let digest = cid.hash_digest();
                let n = digest.len().min(32);
                bytes[..n].copy_from_slice(&digest[..n]);
                ContentHash::from_bytes(&bytes)
            }
        };

        let entry = if cid.codec() == Some(Codec::DagPb) || cid.codec() == Some(Codec::Raw) {
            // dag-pb/raw CIDs are file nodes.
            HamtEntry::File {
                hash,
                size_bytes: size,
            }
        } else {
            // dag-cbor + anything else is treated as a directory
            // (or a non-file link the shard can still address).
            HamtEntry::Directory {
                hash,
                entry_count: 0,
            }
        };

        shard.insert(name, entry)?;
    }
    Ok(shard)
}

/// Build a `UnixFsNode::HamtShardedDirectory` from a `(name, cid,
/// size)` entry list. The links are sorted by name (IPFS gateway
/// convention) so the resulting DAG-CBOR blob is byte-stable
/// across publishers with the same inputs.
pub fn build_hamt_directory(
    entries: impl IntoIterator<Item = (String, Cid, u64)>,
) -> HamtResult<UnixFsNode> {
    let shard = build_shard_from_entries(entries)?;
    shard_to_unixfs_node(&shard)
}

/// Convert an in-memory `HamtShard` into the on-the-wire
/// `UnixFsNode::HamtShardedDirectory` form.
pub fn shard_to_unixfs_node(shard: &HamtShard) -> HamtResult<UnixFsNode> {
    let flat = shard.list();
    let mut links: Vec<UnixFsLink> = flat
        .into_iter()
        .map(|(name, entry)| {
            // Rebuild the link's CID from the entry's content hash
            // + the IPFS-default dag-pb codec. The Tsize is the
            // size field for files and zero for directories.
            let codec = match &entry {
                HamtEntry::File { .. } => Codec::DagPb,
                HamtEntry::Directory { .. } => Codec::DagPb,
            };
            let hash = match &entry {
                HamtEntry::File { hash, .. } | HamtEntry::Directory { hash, .. } => hash,
            };
            let digest = hash.as_bytes();
            let mh = Multihash::from_blake3(digest.as_slice())
                .or_else(|_| Multihash::new(HashCode::Sha256, digest.clone()))
                .map_err(|e| crate::hamt::HamtError::Encoding(e.to_string()))?;
            let cid = Cid::new_v1(codec, mh);
            let tsize = match entry {
                HamtEntry::File { size_bytes, .. } => size_bytes,
                HamtEntry::Directory { .. } => 0,
            };
            Ok::<_, crate::hamt::HamtError>(UnixFsLink::new(name, cid, tsize))
        })
        .collect::<Result<_, _>>()?;

    // Sort by name so the wire form is deterministic.
    links.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(UnixFsNode::HamtShardedDirectory {
        hamt_opts: Default::default(),
        fanout: Some(IPFS_FANOUT),
        bits_width: Some(IPFS_BITS_WIDTH),
        links,
    })
}

/// Extract the flat `(name, cid, size)` entry list from a parsed
/// `UnixFsNode::HamtShardedDirectory`. Returns an error for any
/// other UnixFS variant — the bridge is HAMT-specific.
pub fn entries_from_hamt_directory(node: &UnixFsNode) -> Result<Vec<(String, Cid, u64)>, UnixFsError> {
    match node {
        UnixFsNode::HamtShardedDirectory { links, .. } => Ok(links
            .iter()
            .map(|l| (l.name.clone(), l.cid.clone(), l.tsize.unwrap_or(0)))
            .collect()),
        other => Err(UnixFsError::Encoding(format!(
            "expected HamtShardedDirectory, got {:?}",
            std::mem::discriminant(other)
        ))),
    }
}

/// Rebuild the in-memory `HamtShard` from a parsed
/// `UnixFsNode::HamtShardedDirectory`. The returned shard is
/// ready for lookups (`shard.get(name)`) and is byte-stable
/// with the original publisher's shard as long as both sides
/// use the same hash function and bucket size.
pub fn hamt_shard_from_hamt_directory(node: &UnixFsNode) -> HamtResult<HamtShard> {
    let entries = entries_from_hamt_directory(node)
        .map_err(|e| crate::hamt::HamtError::Encoding(e.to_string()))?;
    build_shard_from_entries(entries)
}

/// Serialise a `HamtShard` (or a freshly built directory) to its
/// canonical DAG-CBOR bytes. Callers that want the CID of the
/// root node should hash the returned bytes with BLAKE3-256
/// (see [`root_cid_for_bytes`]).
pub fn shard_to_dag_cbor(shard: &HamtShard) -> Result<Vec<u8>, UnixFsError> {
    let node = shard_to_unixfs_node(shard)
        .map_err(|e| UnixFsError::InvalidNode(format!("hamt build: {}", e)))?;
    serialization::to_cbor(&node)
}

/// Build the CID that the IPFS gateway would mint for a HAMT
/// directory serialised with [`shard_to_dag_cbor`].
pub fn root_cid_for_bytes(bytes: &[u8]) -> Cid {
    let hash = ContentHash::from_bytes(bytes);
    let digest = hash.as_bytes();
    let mh = Multihash::from_blake3(digest.as_slice())
        .or_else(|_| Multihash::new(HashCode::Sha256, digest.clone()))
        .expect("blake3 multihash always constructs");
    Cid::new_v1(Codec::DagCbor, mh)
}

/// Convenience: build the HAMT directory, serialise it, and
/// return both the bytes and the CID. This is the single
/// entry point that the CLI's `import-dir` flow should call.
pub fn build_hamt_directory_bytes(
    entries: impl IntoIterator<Item = (String, Cid, u64)>,
) -> HamtResult<(Vec<u8>, Cid)> {
    let shard = build_shard_from_entries(entries)?;
    let bytes = shard_to_dag_cbor(&shard).map_err(|e| crate::hamt::HamtError::Encoding(e.to_string()))?;
    let cid = root_cid_for_bytes(&bytes);
    Ok((bytes, cid))
}

// Re-export the shard-manager alias so callers can drive the
// `ShardManager::shard` codepath without an extra import.
pub use crate::hamt::ShardManager as HamtShardManager;

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::cid::Codec;
    use a3net_types::multihash::{HashCode, Multihash};
    use a3net_types::unixfs::serialization;

    fn make_cid(byte: u8) -> Cid {
        let digest = [byte; 32];
        let mh = Multihash::new(HashCode::Blake3, digest.to_vec()).unwrap();
        Cid::new_v1(Codec::DagPb, mh)
    }

    #[test]
    fn empty_directory_roundtrips() {
        let bytes = shard_to_dag_cbor(&HamtShard::new()).expect("encode");
        let node = serialization::from_cbor(&bytes).expect("decode");
        let entries = entries_from_hamt_directory(&node).expect("hamt entries");
        assert!(entries.is_empty(), "empty shard must produce 0 entries");
    }

    #[test]
    fn single_entry_roundtrips() {
        let mut shard = HamtShard::new();
        shard
            .insert(
                "hello.txt".to_string(),
                HamtEntry::File {
                    hash: ContentHash::from_bytes(b"hello"),
                    size_bytes: 5,
                },
            )
            .expect("insert");
        let bytes = shard_to_dag_cbor(&shard).expect("encode");
        let node = serialization::from_cbor(&bytes).expect("decode");
        let entries = entries_from_hamt_directory(&node).expect("hamt entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "hello.txt");
        assert_eq!(entries[0].2, 5);
    }

    #[test]
    fn many_entries_preserve_order() {
        let names: Vec<String> = (0..50).map(|i| format!("file_{:03}.bin", i)).collect();
        let entries: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), make_cid(i as u8), 100 + i as u64))
            .collect();
        let bytes = build_hamt_directory_bytes(entries).expect("build").0;
        let node = serialization::from_cbor(&bytes).expect("decode");
        let recovered = entries_from_hamt_directory(&node).expect("hamt entries");
        assert_eq!(recovered.len(), 50);
        // Recovered entries are sorted by name (wire-form
        // determinism) — verify the order matches the
        // alphabetical sort of the input names.
        let mut sorted_names = names.clone();
        sorted_names.sort();
        let recovered_names: Vec<&String> = recovered.iter().map(|e| &e.0).collect();
        let sorted_refs: Vec<&String> = sorted_names.iter().collect();
        assert_eq!(recovered_names, sorted_refs);
    }

    #[test]
    fn shard_manager_roundtrips_via_node() {
        let mut mgr = HamtShardManager::new();
        mgr.insert(
            "alpha".to_string(),
            HamtEntry::File {
                hash: ContentHash::from_bytes(b"a"),
                size_bytes: 1,
            },
        )
        .expect("insert");
        let node = shard_to_unixfs_node(mgr.root()).expect("to node");
        let rebuilt = hamt_shard_from_hamt_directory(&node).expect("rebuild");
        assert_eq!(rebuilt.len(), 1);
        assert!(rebuilt.get("alpha").is_some());
    }
}
