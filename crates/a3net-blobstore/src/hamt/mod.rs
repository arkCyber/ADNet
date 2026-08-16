//! HAMT (Hash Array Mapped Prefix Tree) Directory Sharding implementation.
//!
//! This module provides efficient directory sharding for large directories,
//! similar to IPFS's HAMT-based directory sharding.
//!
//! ## Overview
//!
//! HAMT is a persistent hash-based trie data structure that provides:
//! - O(log_k N) lookup, insert, and delete operations
//! - Efficient distribution of entries across shards
//! - Content-addressable storage for distributed P2P systems
//!
//! ## Design Parameters
//!
//! | Parameter | Default | Description |
//! |-----------|---------|-------------|
//! | `fanout_bits` | 8 | Bits per level (fanout = 2^fanout_bits = 256) |
//! | `bucket_size` | 1 | Max entries per leaf bucket before overflow |
//! | `hash_bits` | 256 | BLAKE3 output bits used for indexing |
//!
//! ## Example Usage
//!
//! ```rust
//! use a3net_blobstore::{HamtShard, HamtEntry, ContentHash};
//!
//! let mut shard = HamtShard::new();
//!
//! // Insert entries
//! shard.insert("file.txt".to_string(), HamtEntry::File {
//!     hash: ContentHash::from_bytes(b"content"),
//!     size_bytes: 7,
//! }).unwrap();
//!
//! // Query entries
//! let entry = shard.get("file.txt");
//! assert!(entry.is_some());
//!
//! // List all entries
//! for (name, entry) in shard.iter() {
//!     println!("{}: {:?}", name, entry);
//! }
//! ```

pub mod builder;
pub mod cursor;
pub mod iter;

use std::collections::BTreeMap;
use std::fmt::Debug;

use a3net_types::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// bincode for serialization
use bincode;

// Re-export for convenience
pub use builder::{BulkImporter, HamtBuilder, ParallelBulkImporter};
pub use cursor::{CursorBuilder, HamtCursor, WatchedCursor};
pub use iter::{
    DirHamtIter, FileHamtIter, HamtIter, HamtPathIter, PagedHamtIter, PrefixHamtIter, RevHamtIter,
    SortedHamtIter,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default fanout: 2^8 = 256 children per internal node.
/// This matches IPFS's default HAMT fanout.
pub const DEFAULT_FANOUT_BITS: u8 = 8;

/// Default fanout value (2^DEFAULT_FANOUT_BITS).
pub const DEFAULT_FANOUT: usize = 1 << DEFAULT_FANOUT_BITS;

/// Maximum depth of the HAMT tree.
/// Limits maximum number of entries to fanout^max_depth.
pub const MAX_HAMT_DEPTH: u8 = 32;

/// Default bucket size - each leaf bucket holds at most this many entries.
pub const DEFAULT_BUCKET_SIZE: usize = 1;

/// Maximum entries in a leaf bucket.
pub const MAX_BUCKET_SIZE: usize = 256;

/// Size threshold for triggering directory sharding (256 KiB).
pub const DEFAULT_SHARDING_THRESHOLD: u64 = 256 * 1024;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during HAMT operations.
#[derive(Debug, Error)]
pub enum HamtError {
    #[error("entry not found: {0}")]
    NotFound(String),

    #[error("hash collision at max depth: {0}")]
    MaxDepthExceeded(String),

    #[error("invalid HAMT structure: {0}")]
    InvalidStructure(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("bucket overflow: {0} entries exceeds maximum {1}")]
    BucketOverflow(usize, usize),
}

/// Result type for HAMT operations.
pub type HamtResult<T> = Result<T, HamtError>;

// ---------------------------------------------------------------------------
// Hash utilities
// ---------------------------------------------------------------------------

/// Wrapper around BLAKE3 hasher for consistent hashing.
#[derive(Debug, Clone, Default)]
pub struct HamtHasher(blake3::Hasher);

impl HamtHasher {
    /// Create a new hasher.
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    /// Update the hasher with key bytes.
    pub fn update(&mut self, key: &[u8]) {
        self.0.update(key);
    }

    /// Finalize and return the hash as bytes.
    pub fn finalize(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }

    /// Extract `bits` bits starting at `position` from the hash.
    /// Returns the index in [0, 2^bits).
    pub fn extract_bits(hash: &[u8; 32], position: u8, bits: u8) -> usize {
        debug_assert!(
            position as u16 + bits as u16 <= 256,
            "bit extraction out of range"
        );

        let byte_pos = (position / 8) as usize;
        let bit_offset = position % 8;

        // Need up to 4 bytes for bit ranges that cross byte boundaries
        let mut value: u32 = 0;
        let bytes_needed = ((bit_offset + bits + 7) / 8) as usize;

        for i in 0..bytes_needed {
            if byte_pos + i < 32 {
                value = (value << 8) | (hash[byte_pos + i] as u32);
            }
        }

        // Shift right to align and mask
        let shift = if bit_offset > 0 {
            8 - bit_offset - (bits - 1)
        } else {
            0
        };
        let aligned = if shift > 0 && shift < 8 {
            value >> shift
        } else {
            value
        };
        let mask = (1u32 << bits) - 1;
        (aligned & mask) as usize
    }
}

// ---------------------------------------------------------------------------
// Directory Entry (what we store in the HAMT)
// ---------------------------------------------------------------------------

/// A directory entry stored in the HAMT.
/// This can be either a file or a subdirectory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HamtEntry {
    /// Leaf file with content hash and size.
    File { hash: ContentHash, size_bytes: u64 },
    /// Subdirectory link to another HAMT shard.
    Directory {
        /// Content hash of the child HAMT shard.
        hash: ContentHash,
        /// Number of entries in this subdirectory.
        entry_count: u64,
    },
}

impl HamtEntry {
    /// Check if this is a directory entry.
    pub fn is_dir(&self) -> bool {
        matches!(self, HamtEntry::Directory { .. })
    }

    /// Check if this is a file entry.
    pub fn is_file(&self) -> bool {
        matches!(self, HamtEntry::File { .. })
    }

    /// Get the content hash for this entry.
    pub fn content_hash(&self) -> Option<&ContentHash> {
        match self {
            HamtEntry::File { hash, .. } => Some(hash),
            HamtEntry::Directory { hash, .. } => Some(hash),
        }
    }

    /// Get the size in bytes (for files) or entry count (for directories).
    pub fn size_or_count(&self) -> u64 {
        match self {
            HamtEntry::File { size_bytes, .. } => *size_bytes,
            HamtEntry::Directory { entry_count, .. } => *entry_count,
        }
    }
}

// ---------------------------------------------------------------------------
// HAMT Links
// ---------------------------------------------------------------------------

/// A link in the HAMT can point to either:
/// - A child shard (by content hash)
/// - A leaf bucket with one or more entries
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HamtLink {
    /// Points to a child HAMT shard.
    Shard {
        /// Content hash of the child shard.
        hash: ContentHash,
    },
    /// A leaf bucket containing one or more entries.
    Bucket {
        /// The entries in this bucket.
        entries: Vec<(String, HamtEntry)>,
    },
}

impl HamtLink {
    /// Create a shard link.
    pub fn shard(hash: ContentHash) -> Self {
        HamtLink::Shard { hash }
    }

    /// Create a bucket link with a single entry.
    pub fn bucket(name: String, entry: HamtEntry) -> Self {
        HamtLink::Bucket {
            entries: vec![(name, entry)],
        }
    }

    /// Check if this is a shard link.
    pub fn is_shard(&self) -> bool {
        matches!(self, HamtLink::Shard { .. })
    }

    /// Check if this is a bucket link.
    pub fn is_bucket(&self) -> bool {
        matches!(self, HamtLink::Bucket { .. })
    }

    /// Get the hash if this is a shard link.
    pub fn as_shard_hash(&self) -> Option<&ContentHash> {
        match self {
            HamtLink::Shard { hash } => Some(hash),
            _ => None,
        }
    }

    /// Get entries if this is a bucket.
    pub fn as_bucket_entries(&self) -> Option<&[(String, HamtEntry)]> {
        match self {
            HamtLink::Bucket { entries } => Some(entries),
            _ => None,
        }
    }

    /// Add an entry to a bucket.
    /// Returns error if the bucket would exceed MAX_BUCKET_SIZE.
    pub fn bucket_push(&mut self, name: String, entry: HamtEntry) -> HamtResult<()> {
        match self {
            HamtLink::Bucket { entries } => {
                if entries.len() >= MAX_BUCKET_SIZE {
                    return Err(HamtError::BucketOverflow(
                        entries.len() + 1,
                        MAX_BUCKET_SIZE,
                    ));
                }
                entries.push((name, entry));
                Ok(())
            }
            HamtLink::Shard { .. } => Err(HamtError::InvalidStructure(
                "cannot push to shard link".into(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// HAMT Node (Internal structure)
// ---------------------------------------------------------------------------

/// A single node in the HAMT tree.
/// Can be either an internal node (with children) or a leaf node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HamtNode {
    /// Bitfield indicating which child slots are occupied.
    /// For fanout = 256, this is a 256-bit (32-byte) bitfield.
    pub bitfield: Vec<u8>,

    /// Child links indexed by position in the bitfield.
    #[serde(default)]
    pub links: Vec<HamtLink>,

    /// For tracking estimated size in serialized form.
    #[serde(default)]
    pub estimated_size: u64,
}

impl HamtNode {
    /// Create a new empty HAMT node.
    pub fn new() -> Self {
        Self {
            bitfield: vec![0u8; DEFAULT_FANOUT / 8],
            links: Vec::new(),
            estimated_size: 0,
        }
    }

    /// Get the number of children (set bits in bitfield).
    pub fn child_count(&self) -> usize {
        self.bitfield.iter().map(|&b| b.count_ones() as usize).sum()
    }

    /// Check if slot `index` is occupied.
    pub fn has_slot(&self, index: usize) -> bool {
        let byte_idx = index / 8;
        let bit_idx = index % 8;
        byte_idx < self.bitfield.len() && (self.bitfield[byte_idx] & (1 << bit_idx)) != 0
    }

    /// Set slot `index` as occupied.
    pub fn set_slot(&mut self, index: usize) {
        let byte_idx = index / 8;
        let bit_idx = index % 8;
        if byte_idx < self.bitfield.len() {
            self.bitfield[byte_idx] |= 1 << bit_idx;
        }
    }

    /// Clear slot `index`.
    pub fn clear_slot(&mut self, index: usize) {
        let byte_idx = index / 8;
        let bit_idx = index % 8;
        if byte_idx < self.bitfield.len() {
            self.bitfield[byte_idx] &= !(1 << bit_idx);
        }
    }

    /// Get the child link at a specific index.
    /// Returns None if the slot is empty.
    pub fn get_child(&self, index: usize) -> Option<&HamtLink> {
        if !self.has_slot(index) {
            return None;
        }

        // Count how many set bits come before this index
        let mut rank = 0usize;
        for i in 0..index {
            if self.has_slot(i) {
                rank += 1;
            }
        }

        self.links.get(rank)
    }

    /// Get a mutable reference to the child link at a specific index.
    pub fn get_child_mut(&mut self, index: usize) -> Option<&mut HamtLink> {
        if !self.has_slot(index) {
            return None;
        }

        let mut rank = 0usize;
        for i in 0..index {
            if self.has_slot(i) {
                rank += 1;
            }
        }

        self.links.get_mut(rank)
    }

    /// Set a child link at a specific index.
    pub fn set_child(&mut self, index: usize, link: HamtLink) {
        let was_present = self.has_slot(index);

        if was_present {
            // Find and replace existing link
            let mut rank = 0usize;
            for i in 0..index {
                if self.has_slot(i) {
                    rank += 1;
                }
            }
            if rank < self.links.len() {
                self.links[rank] = link;
            }
        } else {
            self.set_slot(index);
            let mut insert_pos = 0usize;
            for i in 0..index {
                if self.has_slot(i) {
                    insert_pos += 1;
                }
            }
            self.links.insert(insert_pos, link);
        }
    }

    /// Remove a child at a specific index.
    pub fn remove_child(&mut self, index: usize) -> Option<HamtLink> {
        if !self.has_slot(index) {
            return None;
        }

        let mut rank = 0usize;
        for i in 0..index {
            if self.has_slot(i) {
                rank += 1;
            }
        }

        self.clear_slot(index);
        Some(self.links.remove(rank))
    }

    /// Estimate the serialized size of this node.
    pub fn estimate_size(&self) -> u64 {
        let links_size: u64 = self
            .links
            .iter()
            .map(|l| match l {
                HamtLink::Shard { hash: _ } => 32 + 4,
                HamtLink::Bucket { entries } => {
                    4 + entries
                        .iter()
                        .map(|(k, v)| 4 + k.len() as u64 + estimate_entry_size(v))
                        .sum::<u64>()
                }
            })
            .sum();
        (32 + 8 + 8) as u64 + self.bitfield.len() as u64 + links_size
    }

    /// Get the total entry count across all buckets in this node.
    pub fn total_entries(&self) -> usize {
        self.links
            .iter()
            .map(|l| match l {
                HamtLink::Shard { .. } => 0,
                HamtLink::Bucket { entries } => entries.len(),
            })
            .sum()
    }
}

impl Default for HamtNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Estimate the serialized size of a directory entry.
fn estimate_entry_size(entry: &HamtEntry) -> u64 {
    match entry {
        HamtEntry::File { size_bytes, .. } => 32 + 8 + *size_bytes,
        HamtEntry::Directory { entry_count, .. } => 32 + 8 + (*entry_count * 8),
    }
}

// ---------------------------------------------------------------------------
// HAMT Shard (complete sharded directory)
// ---------------------------------------------------------------------------

/// A complete HAMT directory shard.
/// This is the top-level structure that can be serialized and stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HamtShard {
    /// The root node.
    pub root: HamtNode,

    /// Total number of entries in this shard.
    pub entry_count: u64,

    /// Estimated serialized size in bytes.
    pub estimated_size: u64,

    /// Hash of this shard (computed on finalize).
    #[serde(skip)]
    pub hash: Option<ContentHash>,
}

impl HamtShard {
    /// Create a new empty HAMT shard.
    pub fn new() -> Self {
        Self {
            root: HamtNode::new(),
            entry_count: 0,
            estimated_size: 0,
            hash: None,
        }
    }

    /// Create a shard from a single node.
    pub fn from_node(root: HamtNode) -> Self {
        let entry_count = root.total_entries() as u64;
        let estimated_size = root.estimate_size();
        Self {
            root,
            entry_count,
            estimated_size,
            hash: None,
        }
    }

    /// Check if this shard is empty.
    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// Get the total entry count.
    pub fn len(&self) -> u64 {
        self.entry_count
    }

    /// Check if this shard should be sharded further.
    pub fn should_shard(&self) -> bool {
        self.estimated_size > DEFAULT_SHARDING_THRESHOLD
    }

    /// Insert an entry into this shard.
    pub fn insert(&mut self, name: String, entry: HamtEntry) -> HamtResult<()> {
        let hash = blake3::hash(name.as_bytes());
        let hash_bytes = *hash.as_bytes();
        self.insert_with_hash(&name, entry, &hash_bytes, 0)
    }

    /// Insert with precomputed hash.
    fn insert_with_hash(
        &mut self,
        name: &str,
        entry: HamtEntry,
        hash: &[u8; 32],
        depth: u8,
    ) -> HamtResult<()> {
        let index =
            HamtHasher::extract_bits(hash, depth * DEFAULT_FANOUT_BITS, DEFAULT_FANOUT_BITS);

        if self.root.has_slot(index) {
            let existing = self.root.get_child(index);

            match existing {
                Some(HamtLink::Bucket { entries }) if entries.len() < MAX_BUCKET_SIZE => {
                    let new_entry_size = estimate_entry_size(&entry);
                    self.estimated_size += 4 + name.len() as u64 + new_entry_size;
                    self.entry_count += 1;
                    let mut link = self.root.remove_child(index).unwrap();
                    if let HamtLink::Bucket { entries: e } = &mut link {
                        e.push((name.to_string(), entry));
                    }
                    self.root.set_child(index, link);
                    Ok(())
                }
                _ => Err(HamtError::InvalidStructure(
                    "collision handling not yet implemented".into(),
                )),
            }
        } else {
            let new_entry_size = estimate_entry_size(&entry);
            self.estimated_size += 4 + name.len() as u64 + new_entry_size;
            self.entry_count += 1;
            self.root
                .set_child(index, HamtLink::bucket(name.to_string(), entry));
            Ok(())
        }
    }

    /// Look up an entry by name.
    pub fn get(&self, name: &str) -> Option<HamtEntry> {
        let hash = blake3::hash(name.as_bytes());
        let hash_bytes = *hash.as_bytes();
        self.get_with_hash(name, &hash_bytes, 0)
    }

    /// Look up with precomputed hash.
    fn get_with_hash(&self, name: &str, hash: &[u8; 32], depth: u8) -> Option<HamtEntry> {
        let index =
            HamtHasher::extract_bits(hash, depth * DEFAULT_FANOUT_BITS, DEFAULT_FANOUT_BITS);

        let link = self.root.get_child(index)?;

        match link {
            HamtLink::Bucket { entries } => entries
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, e)| e.clone()),
            HamtLink::Shard { .. } => None,
        }
    }

    /// Remove an entry by name.
    pub fn remove(&mut self, name: &str) -> HamtResult<Option<HamtEntry>> {
        let hash = blake3::hash(name.as_bytes());
        let hash_bytes = *hash.as_bytes();
        self.remove_with_hash(name, &hash_bytes, 0)
    }

    /// Remove with precomputed hash.
    fn remove_with_hash(
        &mut self,
        name: &str,
        hash: &[u8; 32],
        depth: u8,
    ) -> HamtResult<Option<HamtEntry>> {
        let index =
            HamtHasher::extract_bits(hash, depth * DEFAULT_FANOUT_BITS, DEFAULT_FANOUT_BITS);

        if !self.root.has_slot(index) {
            return Ok(None);
        }

        let link = self.root.get_child(index).cloned();

        match link {
            Some(HamtLink::Bucket { mut entries }) => {
                if let Some(pos) = entries.iter().position(|(n, _)| n == name) {
                    let removed = entries.remove(pos);
                    self.entry_count = self.entry_count.saturating_sub(1);
                    self.estimated_size = self.root.estimate_size();

                    if entries.is_empty() {
                        self.root.remove_child(index);
                    } else {
                        self.root.set_child(index, HamtLink::Bucket { entries });
                    }

                    Ok(Some(removed.1))
                } else {
                    Ok(None)
                }
            }
            Some(HamtLink::Shard { .. }) => Err(HamtError::InvalidStructure(
                "shard removal requires recursive handling".into(),
            )),
            None => Ok(None),
        }
    }

    /// List all entries in this shard.
    pub fn list(&self) -> Vec<(String, HamtEntry)> {
        let mut results = Vec::with_capacity(self.entry_count as usize);
        self.collect_entries(&mut results);
        results
    }

    fn collect_entries(&self, results: &mut Vec<(String, HamtEntry)>) {
        for link in &self.root.links {
            match link {
                HamtLink::Bucket { entries } => {
                    results.extend(entries.iter().cloned());
                }
                HamtLink::Shard { .. } => {}
            }
        }
    }

    /// Compute the content hash of this shard.
    pub fn compute_hash(&mut self) -> ContentHash {
        let bytes = self.to_bytes();
        let hash = ContentHash::from_bytes(&bytes);
        self.hash = Some(hash.clone());
        hash
    }

    /// Serialize to bytes using JSON.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes.
    pub fn from_bytes(bytes: &[u8]) -> HamtResult<Self> {
        serde_json::from_slice(bytes).map_err(|e| HamtError::Serialization(e.to_string()))
    }

    /// Iterate over entries.
    pub fn iter(&self) -> HamtIter<'_> {
        HamtIter::new(self)
    }
}

impl Default for HamtShard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Shard Manager (for managing multiple shards)
// ---------------------------------------------------------------------------

/// Manages multiple HAMT shards and handles automatic sharding.
#[derive(Debug, Clone)]
pub struct ShardManager {
    /// Root shard (may be a single node or contain sub-shards).
    root: HamtShard,

    /// Whether sharding is enabled.
    sharding_enabled: bool,

    /// Threshold for triggering sharding.
    sharding_threshold: u64,
}

impl ShardManager {
    /// Create a new shard manager.
    pub fn new() -> Self {
        Self {
            root: HamtShard::new(),
            sharding_enabled: true,
            sharding_threshold: DEFAULT_SHARDING_THRESHOLD,
        }
    }

    /// Create a shard manager with custom settings.
    pub fn with_threshold(threshold: u64) -> Self {
        Self {
            root: HamtShard::new(),
            sharding_enabled: true,
            sharding_threshold: threshold,
        }
    }

    /// Enable or disable automatic sharding.
    pub fn set_sharding(&mut self, enabled: bool) {
        self.sharding_enabled = enabled;
    }

    /// Check if sharding is enabled.
    pub fn is_sharding_enabled(&self) -> bool {
        self.sharding_enabled
    }

    /// Get the root shard.
    pub fn root(&self) -> &HamtShard {
        &self.root
    }

    /// Get a mutable reference to the root shard.
    pub fn root_mut(&mut self) -> &mut HamtShard {
        &mut self.root
    }

    /// Insert an entry.
    pub fn insert(&mut self, name: String, entry: HamtEntry) -> HamtResult<()> {
        if self.sharding_enabled && self.root.should_shard() {
            tracing::warn!(
                size = self.root.estimated_size,
                "HAMT shard exceeds threshold, sharding not yet implemented"
            );
        }
        self.root.insert(name, entry)
    }

    /// Look up an entry.
    pub fn get(&self, name: &str) -> Option<HamtEntry> {
        self.root.get(name)
    }

    /// Remove an entry.
    pub fn remove(&mut self, name: &str) -> HamtResult<Option<HamtEntry>> {
        self.root.remove(name)
    }

    /// List all entries.
    pub fn list(&self) -> Vec<(String, HamtEntry)> {
        self.root.list()
    }

    /// Get the total entry count.
    pub fn entry_count(&self) -> u64 {
        self.root.len()
    }

    /// Check if sharding should be triggered.
    pub fn should_shard(&self) -> bool {
        self.sharding_enabled && self.root.should_shard()
    }

    /// Trigger sharding of the root shard.
    pub fn shard(&mut self) -> HamtResult<()> {
        if !self.should_shard() {
            return Ok(());
        }

        let entries: Vec<(String, HamtEntry)> = self.root.list();
        let mut new_root = HamtShard::new();

        for (name, entry) in entries {
            new_root.insert(name, entry)?;
        }

        self.root = new_root;
        Ok(())
    }

    /// Serialize the shard manager.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.root.to_bytes()
    }

    /// Deserialize a shard manager.
    pub fn from_bytes(bytes: &[u8]) -> HamtResult<Self> {
        let root = HamtShard::from_bytes(bytes)?;
        Ok(Self {
            root,
            sharding_enabled: true,
            sharding_threshold: DEFAULT_SHARDING_THRESHOLD,
        })
    }
}

impl Default for ShardManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Conversion from/to namespace Entry
// ---------------------------------------------------------------------------

/// Convert from namespace Entry type.
pub fn from_namespace_entry(entry: crate::namespace::Entry) -> HamtEntry {
    match entry {
        crate::namespace::Entry::File { hash, size_bytes } => HamtEntry::File { hash, size_bytes },
        crate::namespace::Entry::Directory { children } => HamtEntry::Directory {
            hash: ContentHash::from_bytes(&[]),
            entry_count: children.len() as u64,
        },
    }
}

/// Convert to namespace Entry type.
pub fn to_namespace_entry(entry: HamtEntry) -> crate::namespace::Entry {
    match entry {
        HamtEntry::File { hash, size_bytes } => crate::namespace::Entry::File { hash, size_bytes },
        HamtEntry::Directory {
            hash: _,
            entry_count: _,
        } => crate::namespace::Entry::Directory {
            children: std::collections::BTreeMap::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitfield_operations() {
        let mut node = HamtNode::new();

        assert!(!node.has_slot(0));
        assert!(!node.has_slot(255));
        assert_eq!(node.child_count(), 0);

        node.set_slot(0);
        node.set_slot(10);
        node.set_slot(255);

        assert!(node.has_slot(0));
        assert!(node.has_slot(10));
        assert!(node.has_slot(255));
        assert!(!node.has_slot(1));
        assert_eq!(node.child_count(), 3);

        node.clear_slot(10);
        assert!(!node.has_slot(10));
        assert_eq!(node.child_count(), 2);
    }

    #[test]
    fn test_hash_bit_extraction() {
        let hash: [u8; 32] = [
            0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x05, 0x06, 0x07, 0x08,
        ];

        let idx = HamtHasher::extract_bits(&hash, 0, 8);
        assert_eq!(idx, 0x12 as usize);

        let idx = HamtHasher::extract_bits(&hash, 8, 8);
        assert_eq!(idx, 0x34 as usize);
    }

    #[test]
    fn test_hamt_shard_insert_get() {
        let mut shard = HamtShard::new();

        let hash1 = ContentHash::from_bytes(b"content1");
        let hash2 = ContentHash::from_bytes(b"content2");

        shard
            .insert(
                "file1.txt".to_string(),
                HamtEntry::File {
                    hash: hash1,
                    size_bytes: 8,
                },
            )
            .unwrap();

        shard
            .insert(
                "file2.txt".to_string(),
                HamtEntry::File {
                    hash: hash2,
                    size_bytes: 8,
                },
            )
            .unwrap();

        assert_eq!(shard.len(), 2);

        let entry = shard.get("file1.txt");
        assert!(entry.is_some());

        assert!(shard.get("nonexistent").is_none());
    }

    #[test]
    fn test_hamt_shard_remove() {
        let mut shard = HamtShard::new();

        let hash = ContentHash::from_bytes(b"content");
        shard
            .insert(
                "file.txt".to_string(),
                HamtEntry::File {
                    hash,
                    size_bytes: 7,
                },
            )
            .unwrap();

        assert_eq!(shard.len(), 1);

        let removed = shard.remove("file.txt").unwrap();
        assert!(removed.is_some());
        assert_eq!(shard.len(), 0);

        assert!(shard.get("file.txt").is_none());
    }

    #[test]
    fn test_hamt_shard_serialization() {
        let mut shard = HamtShard::new();

        shard
            .insert(
                "test.txt".to_string(),
                HamtEntry::File {
                    hash: ContentHash::from_bytes(b"test"),
                    size_bytes: 4,
                },
            )
            .unwrap();

        let bytes = shard.to_bytes();
        assert!(!bytes.is_empty());

        let loaded = HamtShard::from_bytes(&bytes).unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn test_large_insertion() {
        let mut shard = HamtShard::new();

        for i in 0..1000 {
            let name = format!("file{:04}.txt", i);
            shard
                .insert(
                    name,
                    HamtEntry::File {
                        hash: ContentHash::from_bytes(format!("content{}", i).as_bytes()),
                        size_bytes: (i % 100) as u64,
                    },
                )
                .unwrap();
        }

        assert_eq!(shard.len(), 1000);

        let entry = shard.get("file0500.txt");
        assert!(entry.is_some());
    }
}
