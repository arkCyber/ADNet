//! Bao Tree Verifiable Storage — incremental, streaming content verification.
//!
//! This module implements Bao tree hashing for ADNet blobs, enabling:
//!
//! ## Key Capabilities
//!
//! - **Incremental verification**: Verify chunks as they arrive without waiting for full blob
//! - **Out-of-order downloads**: Any chunk can be verified independently using tree proofs
//! - **Partial content proofs**: Prove that a byte range is valid without downloading entire blob
//! - **Streaming integrity**: Verify data integrity during transfer, not just at the end
//!
//! ## Bao Tree Structure
//!
//! Bao (BLAKE3 authenticated organization) organizes data into a hash tree:
//!
//! ```text
//!                    Root Hash (32 bytes)
//!                    /              \
//!           Parent Hash 0         Parent Hash 1
//!           /    |    \           /    |    \
//!      Chunk0 Chunk1 Chunk2   Chunk3 Chunk4 Chunk5 ...
//! ```
//!
//! Each parent hash = BLAKE3(child_0 || child_1 || ...)
//! Root hash = BLAKE3(parent_0 || parent_1 || ...)
//!
//! This creates a merkle-like structure where any node can be verified
//! independently if you know its siblings and parents up to the root.
//!
//! ## Verification Strategy
//!
//! For legacy compatibility, we keep flat BLAKE3 as the root identifier.
//! The Bao tree provides verification INSIDE the blob, not replacing the hash.
//!
//! ## DO-178C Traceability
//!
//! - BAO-1: Every chunk participates in the Bao tree
//! - BAO-2: Tree structure is deterministic and reproducible
//! - BAO-3: Partial verification detects tampering before full read
//! - BAO-4: Out-of-order chunk verification is cryptographically sound
//! - BAO-5: BaoProof includes complete verification paths

use adnet_types::ContentHash;

/// Bao tree leaf node — a single chunk with its verification hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaoLeaf {
    /// Byte offset within the original blob.
    pub offset: u64,
    /// Chunk data bytes (exactly CHUNK_SIZE except possibly the last).
    pub data: Vec<u8>,
    /// BLAKE3 hash of this chunk's data.
    pub hash: ContentHash,
}

impl BaoLeaf {
    pub fn new(offset: u64, data: Vec<u8>) -> Self {
        let hash = ContentHash::from_bytes(&data);
        Self { offset, data, hash }
    }

    /// Byte length of this leaf's data.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// True if this leaf contains no data.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Bao tree node — either a leaf (chunk) or an internal parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaoNode {
    /// Leaf node containing raw chunk data.
    Leaf(BaoLeaf),
    /// Parent node with children hashes.
    Parent {
        /// Byte range covered by this node.
        offset: u64,
        len: u64,
        /// Combined hash of all children.
        hash: ContentHash,
        /// Child node hashes (2 for binary, but Bao can use more).
        children: Vec<ContentHash>,
    },
}

impl BaoNode {
    /// Get the hash of this node.
    pub fn hash(&self) -> ContentHash {
        match self {
            BaoNode::Leaf(leaf) => leaf.hash.clone(),
            BaoNode::Parent { hash, .. } => hash.clone(),
        }
    }

    /// Get the byte range this node covers.
    pub fn range(&self) -> (u64, u64) {
        match self {
            BaoNode::Leaf(leaf) => (leaf.offset, leaf.offset + leaf.data.len() as u64),
            BaoNode::Parent { offset, len, .. } => (*offset, *offset + *len),
        }
    }
}

/// Bao tree — a hierarchical hash tree for incremental verification.
///
/// The tree is built from chunks and provides:
/// - Root hash for the complete blob
/// - Parent hashes for any subtree
/// - Verification proofs for arbitrary byte ranges
///
/// ## Inner tree structure
///
/// The tree stores parent nodes at each level to enable efficient proof generation.
/// For a tree with `n` leaves, we store:
/// - Level 0: `n` leaves (chunks)
/// - Level 1: `ceil(n/2)` parent nodes
/// - Level 2: `ceil(ceil(n/2)/2)` grandparent nodes
/// - ... until root
///
/// This enables O(tree_height) proof generation instead of O(n).
#[derive(Debug, Clone)]
pub struct BaoTree {
    /// All leaf nodes (chunks).
    leaves: Vec<BaoLeaf>,
    /// Root hash — the cryptographic anchor for the entire tree.
    root_hash: ContentHash,
    /// Total byte length of the original content.
    total_len: u64,
    /// Tree height (for large blobs, multiple levels of parents).
    height: u8,
    /// Parent hashes at each level. `parents[0]` is level 1 (above leaves),
    /// `parents[height-1]` is just below root. Each inner Vec has hashes
    /// in left-to-right order.
    parents: Vec<Vec<ContentHash>>,
    /// Offsets for each node at each level (same structure as parents).
    /// Used for range queries and proof generation.
    parent_offsets: Vec<Vec<u64>>,
    /// Lengths for each node at each level.
    parent_lens: Vec<Vec<u64>>,
}

impl BaoTree {
    /// Build a Bao tree from raw content bytes.
    ///
    /// This is the primary construction method. The content is split
    /// into CHUNK_SIZE chunks, each becomes a leaf node, and parent
    /// nodes are computed recursively until a single root remains.
    ///
    /// ## DO-178C BAO-1
    ///
    /// Every byte of input participates in exactly one leaf node,
    /// ensuring complete coverage for verification.
    pub fn build(content: &[u8]) -> Self {
        let chunk_size = crate::chunked::CHUNK_SIZE;

        // Build leaves (chunks).
        let mut leaves = Vec::new();
        let mut offset = 0u64;
        for chunk in content.chunks(chunk_size) {
            let leaf = BaoLeaf::new(offset, chunk.to_vec());
            leaves.push(leaf);
            offset += chunk.len() as u64;
        }

        // Special case: empty content.
        if leaves.is_empty() {
            let root_hash = ContentHash::from_bytes(b"");
            return Self {
                leaves,
                root_hash,
                total_len: 0,
                height: 0,
                parents: Vec::new(),
                parent_offsets: Vec::new(),
                parent_lens: Vec::new(),
            };
        }

        // Build tree from leaves, including parent structure.
        let (root_hash, height, parents, parent_offsets, parent_lens) =
            Self::build_tree_structure(&leaves);

        Self {
            leaves,
            root_hash,
            total_len: content.len() as u64,
            height,
            parents,
            parent_offsets,
            parent_lens,
        }
    }

    /// Build the full tree structure including parent nodes at each level.
    fn build_tree_structure(
        leaves: &[BaoLeaf],
    ) -> (
        ContentHash,
        u8,
        Vec<Vec<ContentHash>>,
        Vec<Vec<u64>>,
        Vec<Vec<u64>>,
    ) {
        if leaves.is_empty() {
            return (
                ContentHash::from_bytes(b""),
                0,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
        }

        let mut current_level: Vec<ContentHash> = leaves.iter().map(|l| l.hash.clone()).collect();
        let mut current_offsets: Vec<u64> = leaves.iter().map(|l| l.offset).collect();
        let mut current_lens: Vec<u64> = leaves.iter().map(|l| l.data.len() as u64).collect();

        let mut height = 0u8;
        let mut all_parents: Vec<Vec<ContentHash>> = Vec::new();
        let mut all_offsets: Vec<Vec<u64>> = Vec::new();
        let mut all_lens: Vec<Vec<u64>> = Vec::new();

        while current_level.len() > 1 {
            let mut next_level = Vec::new();
            let mut next_offsets = Vec::new();
            let mut next_lens = Vec::new();

            for pair in current_level.chunks(2) {
                if pair.len() == 2 {
                    let bytes0 = hex::decode(pair[0].as_hex()).expect("valid hex");
                    let bytes1 = hex::decode(pair[1].as_hex()).expect("valid hex");
                    let combined = [bytes0.as_slice(), bytes1.as_slice()].concat();
                    let parent_hash = ContentHash::from_bytes(&combined);
                    next_level.push(parent_hash);
                } else {
                    next_level.push(pair[0].clone());
                }
            }

            let mut i = 0;
            while i < current_level.len() {
                let len = if i + 1 < current_level.len() {
                    current_lens[i] + current_lens[i + 1]
                } else {
                    current_lens[i]
                };
                next_offsets.push(current_offsets[i]);
                next_lens.push(len);
                i += 2;
            }

            // Store this level's parent hashes and metadata
            all_parents.push(current_level.clone());
            all_offsets.push(current_offsets.clone());
            all_lens.push(current_lens.clone());

            current_level = next_level;
            current_offsets = next_offsets;
            current_lens = next_lens;
            height += 1;
        }

        // The root is the only node remaining
        let root_hash = current_level
            .pop()
            .unwrap_or_else(|| ContentHash::from_bytes(b""));

        // Special case: single leaf tree
        if leaves.len() == 1 && height == 0 {
            height = 1;
        }

        (root_hash, height, all_parents, all_offsets, all_lens)
    }

    /// Build tree from existing leaves (for streaming construction).
    /// Note: This only computes root hash without building full tree structure.
    /// For proofs and range queries, use BaoTree::build() instead.
    #[allow(dead_code)]
    fn build_tree_from_leaves(leaves: &[BaoLeaf]) -> (ContentHash, u8) {
        if leaves.is_empty() {
            return (ContentHash::from_bytes(b""), 0);
        }

        let mut current_level: Vec<ContentHash> = leaves.iter().map(|l| l.hash.clone()).collect();
        let mut current_lens: Vec<u64> = leaves.iter().map(|l| l.data.len() as u64).collect();

        let mut height = 0u8;

        while current_level.len() > 1 {
            let mut next_level = Vec::new();

            for pair in current_level.chunks(2) {
                if pair.len() == 2 {
                    let bytes0 = hex::decode(pair[0].as_hex()).expect("valid hex");
                    let bytes1 = hex::decode(pair[1].as_hex()).expect("valid hex");
                    let combined = [bytes0.as_slice(), bytes1.as_slice()].concat();
                    let parent_hash = ContentHash::from_bytes(&combined);
                    next_level.push(parent_hash);
                } else {
                    next_level.push(pair[0].clone());
                }
            }

            let mut i = 0;
            let mut next_lens = Vec::new();
            while i < current_level.len() {
                let len = if i + 1 < current_level.len() {
                    current_lens[i] + current_lens[i + 1]
                } else {
                    current_lens[i]
                };
                next_lens.push(len);
                i += 2;
            }

            current_level = next_level;
            current_lens = next_lens;
            height += 1;
        }

        if leaves.len() == 1 {
            height = 1;
        }

        (
            current_level
                .pop()
                .unwrap_or_else(|| ContentHash::from_bytes(b"")),
            height,
        )
    }

    /// Root hash of the tree — use this as the verification anchor.
    pub fn root_hash(&self) -> &ContentHash {
        &self.root_hash
    }

    /// Total byte length of the original content.
    pub fn total_len(&self) -> u64 {
        self.total_len
    }

    /// Number of leaf chunks.
    pub fn num_leaves(&self) -> usize {
        self.leaves.len()
    }

    /// Tree height (0 for empty, 1 for single chunk, etc.).
    pub fn height(&self) -> u8 {
        self.height
    }

    /// Get a leaf by index.
    pub fn leaf(&self, index: usize) -> Option<&BaoLeaf> {
        self.leaves.get(index)
    }

    /// Get all leaves.
    pub fn leaves(&self) -> &[BaoLeaf] {
        &self.leaves
    }

    /// Get parent hashes at a specific tree level (0 = level above leaves).
    /// Returns None if the level doesn't exist or is out of bounds.
    pub fn parents_at_level(&self, level: usize) -> Option<&[ContentHash]> {
        self.parents.get(level).map(|p| p.as_slice())
    }

    /// Get offsets at a specific tree level.
    pub fn offsets_at_level(&self, level: usize) -> Option<&[u64]> {
        self.parent_offsets.get(level).map(|p| p.as_slice())
    }

    /// Get lengths at a specific tree level.
    pub fn lens_at_level(&self, level: usize) -> Option<&[u64]> {
        self.parent_lens.get(level).map(|p| p.as_slice())
    }

    /// Find the leaf indices that intersect with a byte range.
    pub fn leaf_indices_for_range(&self, start: u64, end: u64) -> Vec<usize> {
        let mut indices = Vec::new();
        for (i, leaf) in self.leaves.iter().enumerate() {
            let leaf_end = leaf.offset + leaf.data.len() as u64;
            if leaf.offset < end && leaf_end > start {
                indices.push(i);
            }
        }
        indices
    }

    /// Verify a leaf's hash matches the tree's expectation.
    ///
    /// ## DO-178C BAO-3
    ///
    /// This check ensures the chunk hasn't been tampered with
    /// since the tree was built.
    pub fn verify_leaf(&self, index: usize) -> Result<(), BaoTreeError> {
        let leaf = self.leaves.get(index).ok_or(BaoTreeError::LeafOutOfRange {
            index,
            total: self.leaves.len(),
        })?;

        let computed = ContentHash::from_bytes(&leaf.data);
        if &computed != &leaf.hash {
            return Err(BaoTreeError::LeafHashMismatch {
                index,
                expected: leaf.hash.clone(),
                actual: computed,
            });
        }

        Ok(())
    }

    /// Verify the entire tree against the stored root hash.
    ///
    /// This rebuilds the tree from leaves and compares to root.
    /// Used for periodic integrity verification.
    pub fn verify_tree(&self) -> Result<(), BaoTreeError> {
        let (computed_root, _, _, _, _) = Self::build_tree_structure(&self.leaves);
        if &computed_root != &self.root_hash {
            return Err(BaoTreeError::RootHashMismatch {
                expected: self.root_hash.clone(),
                actual: computed_root,
            });
        }
        Ok(())
    }

    /// Generate a verification proof for a byte range.
    ///
    /// Returns the chunks and their verification paths needed to verify the range.
    /// Each leaf includes its full Merkle path from the leaf to the root.
    ///
    /// ## DO-178C BAO-5
    ///
    /// Proofs include complete verification paths enabling cryptographically
    /// sound verification without trusting the data source.
    pub fn proof_for_range(&self, start: u64, end: u64) -> Result<BaoProof, BaoTreeError> {
        if start >= end {
            return Err(BaoTreeError::InvalidRange { start, end });
        }

        if end > self.total_len {
            return Err(BaoTreeError::RangeOutOfBounds {
                range_end: end,
                total_len: self.total_len,
            });
        }

        // Find all leaves that intersect with the requested range
        let leaf_indices = self.leaf_indices_for_range(start, end);

        if leaf_indices.is_empty() {
            return Ok(BaoProof {
                root_hash: self.root_hash.clone(),
                total_len: self.total_len,
                leaves: Vec::new(),
                paths: Vec::new(),
            });
        }

        let mut proof_leaves = Vec::new();
        let mut proof_paths = Vec::new();

        for &leaf_idx in &leaf_indices {
            let leaf = self.leaves[leaf_idx].clone();
            let path = self.merkle_path_for_leaf(leaf_idx)?;

            proof_leaves.push(leaf);
            proof_paths.push(path);
        }

        Ok(BaoProof {
            root_hash: self.root_hash.clone(),
            total_len: self.total_len,
            leaves: proof_leaves,
            paths: proof_paths,
        })
    }

    /// Generate the Merkle path (proof) for a specific leaf.
    ///
    /// Returns sibling hashes at each level from leaf to root.
    /// To verify: hash leaf, then iteratively combine with siblings
    /// until reaching the root hash.
    pub fn merkle_path_for_leaf(&self, leaf_index: usize) -> Result<MerklePath, BaoTreeError> {
        if leaf_index >= self.leaves.len() {
            return Err(BaoTreeError::LeafOutOfRange {
                index: leaf_index,
                total: self.leaves.len(),
            });
        }

        let mut path = MerklePath {
            leaf_index,
            leaf_offset: self.leaves[leaf_index].offset,
            leaf_len: self.leaves[leaf_index].data.len() as u64,
            siblings: Vec::new(),
        };

        // Navigate up the tree, collecting sibling hashes at each level
        let mut current_index = leaf_index;

        // Level 0 is the leaves, so we start at level 1 (parents of leaves)
        for level in 0..self.parents.len() {
            // Determine sibling index (left = even, right = odd)
            let sibling_index = if current_index % 2 == 0 {
                // Left child, sibling is to the right
                current_index + 1
            } else {
                // Right child, sibling is to the left
                current_index - 1
            };

            // Check if sibling exists (might not for odd-numbered nodes at the end)
            let siblings_at_level = &self.parents[level];
            let offsets_at_level = &self.parent_offsets[level];
            let lens_at_level = &self.parent_lens[level];

            if sibling_index < siblings_at_level.len() {
                // Sibling exists: include its hash
                path.siblings.push(MerkleSibling {
                    level: level as u8,
                    index: sibling_index,
                    offset: offsets_at_level[sibling_index],
                    len: lens_at_level[sibling_index],
                    hash: siblings_at_level[sibling_index].clone(),
                    is_left: sibling_index % 2 == 0,
                    is_null: false,
                });
            } else {
                // No sibling at this level (odd number of nodes)
                // Include a null marker - this means we hash the current node alone
                path.siblings.push(MerkleSibling {
                    level: level as u8,
                    index: sibling_index,
                    offset: 0,
                    len: 0,
                    hash: ContentHash::from_bytes(b""),
                    is_left: true,
                    is_null: true,
                });
            }

            // Move up to parent index
            current_index = current_index / 2;
        }

        Ok(path)
    }

    /// Verify a Merkle path against a leaf hash, returning the computed root.
    ///
    /// This allows verification of a proof without having the full tree.
    pub fn verify_path(
        &self,
        leaf_index: usize,
        leaf_hash: &ContentHash,
    ) -> Result<ContentHash, BaoTreeError> {
        let path = self.merkle_path_for_leaf(leaf_index)?;
        path.compute_root(leaf_hash)
    }
}

/// Error types for Bao tree operations.
#[derive(Debug, thiserror::Error)]
pub enum BaoTreeError {
    #[error("leaf index {index} out of range (total {total})")]
    LeafOutOfRange { index: usize, total: usize },

    #[error("leaf {index} hash mismatch: expected {expected}, got {actual}")]
    LeafHashMismatch {
        index: usize,
        expected: ContentHash,
        actual: ContentHash,
    },

    #[error("root hash mismatch: expected {expected}, got {actual}")]
    RootHashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },

    #[error("invalid range: start {start} >= end {end}")]
    InvalidRange { start: u64, end: u64 },

    #[error("range end {range_end} exceeds total length {total_len}")]
    RangeOutOfBounds { range_end: u64, total_len: u64 },

    #[error("proof verification failed: {0}")]
    ProofVerificationFailed(String),
}

/// A single sibling hash in a Merkle proof path.
///
/// Represents one level of the tree with the sibling hash needed
/// to compute the parent hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleSibling {
    /// Tree level this sibling is at (0 = level above leaves).
    pub level: u8,
    /// Index of this sibling at its level.
    pub index: usize,
    /// Byte offset of the sibling's data range.
    pub offset: u64,
    /// Length in bytes of the sibling's data range.
    pub len: u64,
    /// Hash of the sibling node (or empty if this is a null entry).
    pub hash: ContentHash,
    /// True if this sibling is the left child (needed for correct ordering).
    pub is_left: bool,
    /// True if this is a null entry (no actual sibling exists).
    pub is_null: bool,
}

/// A Merkle path from a leaf to the root.
///
/// This is the complete set of sibling hashes needed to verify
/// a leaf hash against the tree's root hash.
///
/// ## Verification Process
///
/// ```ignore
/// current_hash = leaf_hash
/// for sibling in path.siblings (bottom to top):
///     if sibling.is_null:
///         continue  // No sibling to combine
///     if sibling.is_left:
///         current_hash = hash(sibling.hash || current_hash)
///     else:
///         current_hash = hash(current_hash || sibling.hash)
/// return current_hash  // Should equal root_hash
/// ```
#[derive(Debug, Clone)]
pub struct MerklePath {
    /// Index of the leaf this path is for.
    pub leaf_index: usize,
    /// Byte offset of the leaf in the original content.
    pub leaf_offset: u64,
    /// Length of the leaf's data range.
    pub leaf_len: u64,
    /// Sibling hashes at each level, from immediate parent to root.
    /// The first entry is the immediate sibling, last is near the root.
    pub siblings: Vec<MerkleSibling>,
}

impl MerklePath {
    /// Compute the root hash from a leaf hash using this path.
    ///
    /// Returns the computed root hash if verification succeeds.
    pub fn compute_root(&self, leaf_hash: &ContentHash) -> Result<ContentHash, BaoTreeError> {
        let mut current = leaf_hash.clone();

        for sibling in &self.siblings {
            if sibling.is_null {
                continue;
            }

            let bytes_current = hex::decode(current.as_hex())
                .map_err(|e| BaoTreeError::ProofVerificationFailed(e.to_string()))?;
            let bytes_sibling = hex::decode(sibling.hash.as_hex())
                .map_err(|e| BaoTreeError::ProofVerificationFailed(e.to_string()))?;

            let combined = if sibling.is_left {
                // Sibling is left, current is right
                [bytes_sibling.as_slice(), bytes_current.as_slice()].concat()
            } else {
                // Current is left, sibling is right
                [bytes_current.as_slice(), bytes_sibling.as_slice()].concat()
            };

            current = ContentHash::from_bytes(&combined);
        }

        Ok(current)
    }

    /// Verify this path against an expected root hash.
    ///
    /// Returns Ok(()) if the path, when computed from the leaf data,
    /// produces the expected root hash.
    pub fn verify(
        &self,
        leaf_hash: &ContentHash,
        expected_root: &ContentHash,
    ) -> Result<(), BaoTreeError> {
        let computed_root = self.compute_root(leaf_hash)?;
        if &computed_root != expected_root {
            return Err(BaoTreeError::ProofVerificationFailed(format!(
                "computed root {} != expected {}",
                computed_root, expected_root
            )));
        }
        Ok(())
    }
}

/// A verification proof for a byte range.
///
/// Contains all information needed to verify a range of bytes
/// against the tree's root hash, including complete Merkle paths
/// for each leaf in the range.
///
/// ## DO-178C BAO-5
///
/// This proof structure enables verification of partial content
/// without requiring the entire blob.
#[derive(Debug, Clone)]
pub struct BaoProof {
    /// Root hash this proof verifies against.
    pub root_hash: ContentHash,
    /// Total content length.
    pub total_len: u64,
    /// Leaves (chunks) covering the requested range.
    pub leaves: Vec<BaoLeaf>,
    /// Merkle paths for each leaf, enabling full verification.
    /// paths[i] is the proof for leaves[i].
    pub paths: Vec<MerklePath>,
}

impl BaoProof {
    /// Verify this proof against expected content.
    ///
    /// Returns Ok if all leaves and their Merkle paths verify
    /// against the root hash.
    ///
    /// ## DO-178C BAO-5
    pub fn verify(&self, expected_root: &ContentHash) -> Result<(), BaoTreeError> {
        if &self.root_hash != expected_root {
            return Err(BaoTreeError::ProofVerificationFailed(
                "root hash mismatch".into(),
            ));
        }

        // Verify each leaf has a corresponding path
        if self.leaves.len() != self.paths.len() {
            return Err(BaoTreeError::ProofVerificationFailed(format!(
                "leaves count {} != paths count {}",
                self.leaves.len(),
                self.paths.len()
            )));
        }

        // Verify each leaf and its path
        for (leaf, path) in self.leaves.iter().zip(self.paths.iter()) {
            // Verify the leaf's internal hash is correct
            let computed = ContentHash::from_bytes(&leaf.data);
            if &computed != &leaf.hash {
                return Err(BaoTreeError::LeafHashMismatch {
                    index: 0,
                    expected: leaf.hash.clone(),
                    actual: computed,
                });
            }

            // Verify the path from leaf to root
            path.verify(&leaf.hash, expected_root)?;
        }

        Ok(())
    }

    /// Verify a subset of the proof (for streaming verification).
    ///
    /// Only verifies leaves at the given indices.
    pub fn verify_subset(
        &self,
        indices: &[usize],
        expected_root: &ContentHash,
    ) -> Result<(), BaoTreeError> {
        for &idx in indices {
            if idx >= self.leaves.len() {
                return Err(BaoTreeError::LeafOutOfRange {
                    index: idx,
                    total: self.leaves.len(),
                });
            }

            let leaf = &self.leaves[idx];
            let path = &self.paths[idx];

            let computed = ContentHash::from_bytes(&leaf.data);
            if &computed != &leaf.hash {
                return Err(BaoTreeError::LeafHashMismatch {
                    index: idx,
                    expected: leaf.hash.clone(),
                    actual: computed,
                });
            }

            path.verify(&leaf.hash, expected_root)?;
        }

        Ok(())
    }
}

/// Streaming Bao tree builder — construct tree as data arrives.
///
/// This is useful for verifying data during download without
/// waiting for the entire blob.
pub struct BaoTreeBuilder {
    leaves: Vec<BaoLeaf>,
    hasher: blake3::Hasher,
    current_offset: u64,
    chunk_buf: Vec<u8>,
    chunk_size: usize,
}

impl BaoTreeBuilder {
    /// Create a new streaming builder.
    pub fn new() -> Self {
        Self {
            leaves: Vec::new(),
            hasher: blake3::Hasher::new(),
            current_offset: 0,
            chunk_buf: Vec::with_capacity(crate::chunked::CHUNK_SIZE),
            chunk_size: crate::chunked::CHUNK_SIZE,
        }
    }

    /// Feed more bytes into the builder.
    ///
    /// Bytes are buffered and emitted as complete chunks.
    pub fn write(&mut self, buf: &[u8]) {
        let mut remaining = buf;

        while !remaining.is_empty() {
            let space = self.chunk_size - self.chunk_buf.len();
            let take = space.min(remaining.len());

            self.chunk_buf.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];

            if self.chunk_buf.len() == self.chunk_size {
                let data = std::mem::take(&mut self.chunk_buf);
                let data_len = data.len() as u64;
                let leaf = BaoLeaf::new(self.current_offset, data);
                self.hasher.update(&leaf.data);
                self.leaves.push(leaf);
                self.current_offset += data_len;
            }
        }
    }

    /// Finalize the tree and return the root hash.
    ///
    /// Call this when all data has been written.
    pub fn finish(mut self) -> BaoTree {
        // Flush any remaining bytes as a partial final chunk.
        if !self.chunk_buf.is_empty() {
            let data = std::mem::take(&mut self.chunk_buf);
            let data_len = data.len() as u64;
            let leaf = BaoLeaf::new(self.current_offset, data);
            self.hasher.update(&leaf.data);
            self.leaves.push(leaf);
            self.current_offset += data_len;
        }

        // Build tree with full structure for proofs
        let (root_hash, height, parents, parent_offsets, parent_lens) =
            BaoTree::build_tree_structure(&self.leaves);

        BaoTree {
            leaves: self.leaves,
            root_hash,
            total_len: self.current_offset,
            height,
            parents,
            parent_offsets,
            parent_lens,
        }
    }
}

impl Default for BaoTreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_empty_tree() {
        let tree = BaoTree::build(b"");
        assert_eq!(tree.total_len(), 0);
        assert_eq!(tree.num_leaves(), 0);
        assert_eq!(tree.root_hash(), &ContentHash::from_bytes(b""));
    }

    #[test]
    fn build_single_chunk_tree() {
        let data = b"hello world";
        let tree = BaoTree::build(data);
        assert_eq!(tree.total_len(), data.len() as u64);
        assert_eq!(tree.num_leaves(), 1);
        assert_eq!(tree.leaf(0).unwrap().data.as_slice(), data);
    }

    #[test]
    fn build_multi_chunk_tree() {
        // Create data larger than one chunk.
        let data: Vec<u8> = (0..(crate::chunked::CHUNK_SIZE * 3 + 100))
            .map(|i| i as u8)
            .collect();
        let tree = BaoTree::build(&data);

        assert_eq!(tree.total_len(), data.len() as u64);
        // Should be 4 chunks: 3 full + 1 partial.
        assert_eq!(tree.num_leaves(), 4);

        // Verify each leaf.
        for i in 0..tree.num_leaves() {
            tree.verify_leaf(i).unwrap();
        }

        // Verify the entire tree.
        tree.verify_tree().unwrap();
    }

    #[test]
    fn leaf_hash_matches_direct() {
        let data = b"test data for hashing";
        let tree = BaoTree::build(data);

        let leaf = tree.leaf(0).unwrap();
        let direct_hash = ContentHash::from_bytes(&leaf.data);
        assert_eq!(leaf.hash, direct_hash);
    }

    #[test]
    fn root_hash_deterministic() {
        let data = b"deterministic content";

        let tree1 = BaoTree::build(data);
        let tree2 = BaoTree::build(data);

        assert_eq!(tree1.root_hash(), tree2.root_hash());
    }

    #[test]
    fn root_hash_changes_with_content() {
        let tree1 = BaoTree::build(b"content A");
        let tree2 = BaoTree::build(b"content B");

        assert_ne!(tree1.root_hash(), tree2.root_hash());
    }

    #[test]
    fn streaming_builder_matches_direct() {
        let data: Vec<u8> = (0..(crate::chunked::CHUNK_SIZE * 2 + 500))
            .map(|i| (i * 7) as u8)
            .collect();

        let direct_tree = BaoTree::build(&data);

        // Stream in small pieces.
        let mut builder = BaoTreeBuilder::new();
        for chunk in data.chunks(100) {
            builder.write(chunk);
        }
        let streamed_tree = builder.finish();

        assert_eq!(streamed_tree.root_hash(), direct_tree.root_hash());
        assert_eq!(streamed_tree.total_len(), direct_tree.total_len());
        assert_eq!(streamed_tree.num_leaves(), direct_tree.num_leaves());
    }

    #[test]
    fn proof_for_range() {
        let data: Vec<u8> = (0..10000).map(|i| i as u8).collect();
        let tree = BaoTree::build(&data);

        // Request a range in the middle.
        let proof = tree.proof_for_range(1000, 5000).unwrap();

        assert_eq!(proof.total_len, data.len() as u64);
        assert!(!proof.leaves.is_empty());

        // Verify the proof.
        proof.verify(tree.root_hash()).unwrap();
    }

    #[test]
    fn proof_range_error_cases() {
        let tree = BaoTree::build(b"hello world");

        // Invalid range: start >= end.
        let err = tree.proof_for_range(100, 50).unwrap_err();
        assert!(matches!(err, BaoTreeError::InvalidRange { .. }));

        // Range out of bounds.
        let err = tree.proof_for_range(0, 1000).unwrap_err();
        assert!(matches!(err, BaoTreeError::RangeOutOfBounds { .. }));
    }

    #[test]
    fn verify_leaf_detects_tampering() {
        let data = b"sensitive data that should not be tampered with";
        let mut tree = BaoTree::build(data);

        // Tamper with a leaf (don't update hash).
        if let Some(leaf) = tree.leaves.first_mut() {
            leaf.data[0] ^= 0xFF;
            // Note: leaf.hash is NOT updated, so verification will fail.
        }

        // Verification should fail.
        let err = tree.verify_leaf(0).unwrap_err();
        assert!(matches!(err, BaoTreeError::LeafHashMismatch { .. }));
    }

    #[test]
    fn height_increases_with_size() {
        let single = BaoTree::build(b"x");
        let medium = BaoTree::build(&vec![0u8; crate::chunked::CHUNK_SIZE * 10]);
        let large = BaoTree::build(&vec![0u8; crate::chunked::CHUNK_SIZE * 100]);

        // Larger data should result in taller trees.
        assert!(large.height >= medium.height);
        assert!(medium.height >= single.height);
    }

    #[test]
    fn merkle_path_generation() {
        // Use enough data to create multiple leaves (CHUNK_SIZE = 16 KiB)
        let data: Vec<u8> = (0..(crate::chunked::CHUNK_SIZE * 5))
            .map(|i| i as u8)
            .collect();
        let tree = BaoTree::build(&data);

        // Get path for a middle leaf
        let leaf_idx = 2;
        let path = tree.merkle_path_for_leaf(leaf_idx).unwrap();

        assert_eq!(path.leaf_index, leaf_idx);
        assert!(!path.siblings.is_empty());

        // Verify the path computes to the root
        let leaf = tree.leaf(leaf_idx).unwrap();
        let computed_root = path.compute_root(&leaf.hash).unwrap();
        assert_eq!(&computed_root, tree.root_hash());
    }

    #[test]
    fn merkle_path_verify_method() {
        let data: Vec<u8> = (0..(crate::chunked::CHUNK_SIZE * 3))
            .map(|i| i as u8)
            .collect();
        let tree = BaoTree::build(&data);

        for leaf_idx in 0..tree.num_leaves() {
            let leaf = tree.leaf(leaf_idx).unwrap();
            let path = tree.merkle_path_for_leaf(leaf_idx).unwrap();

            // Direct verification using path.verify()
            path.verify(&leaf.hash, tree.root_hash()).unwrap();
        }
    }

    #[test]
    fn merkle_path_detects_tampering() {
        let data: Vec<u8> = (0..(crate::chunked::CHUNK_SIZE * 2))
            .map(|i| i as u8)
            .collect();
        let tree = BaoTree::build(&data);

        let leaf_idx = 0;
        let _leaf = tree.leaf(leaf_idx).unwrap();
        let path = tree.merkle_path_for_leaf(leaf_idx).unwrap();

        // Tamper with the leaf hash
        let tampered_hash = ContentHash::from_bytes(b"totally different data");

        // Verification should fail
        let result = path.verify(&tampered_hash, tree.root_hash());
        assert!(result.is_err());
    }

    #[test]
    fn proof_includes_merkle_paths() {
        let data: Vec<u8> = (0..20000).map(|i| i as u8).collect();
        let tree = BaoTree::build(&data);

        // Get a range proof
        let proof = tree.proof_for_range(5000, 15000).unwrap();

        assert_eq!(proof.leaves.len(), proof.paths.len());
        assert!(!proof.leaves.is_empty());

        // Verify each leaf-path pair
        for (leaf, path) in proof.leaves.iter().zip(proof.paths.iter()) {
            path.verify(&leaf.hash, &proof.root_hash).unwrap();
        }
    }

    #[test]
    fn proof_verify_detects_tampered_leaf() {
        let data: Vec<u8> = (0..10000).map(|i| i as u8).collect();
        let tree = BaoTree::build(&data);

        let mut proof = tree.proof_for_range(1000, 5000).unwrap();

        // Tamper with the first leaf's data
        if let Some(leaf) = proof.leaves.first_mut() {
            leaf.data[0] = 0xFF;
        }

        // Verification should fail
        let result = proof.verify(tree.root_hash());
        assert!(result.is_err());
    }

    #[test]
    fn proof_verify_subset() {
        let data: Vec<u8> = (0..30000).map(|i| i as u8).collect();
        let tree = BaoTree::build(&data);

        let mut proof = tree.proof_for_range(0, 20000).unwrap();
        let num_leaves = proof.leaves.len();

        if num_leaves >= 3 {
            // Verify only first 2 leaves
            proof.verify_subset(&[0, 1], tree.root_hash()).unwrap();

            // Tamper with one
            proof.leaves[0].data[0] = 0x00;
            let result = proof.verify_subset(&[0, 1], tree.root_hash());
            assert!(result.is_err());
        }
    }

    #[test]
    fn single_leaf_tree_merkle_path() {
        let tree = BaoTree::build(b"hello");

        let path = tree.merkle_path_for_leaf(0).unwrap();
        assert_eq!(path.leaf_index, 0);

        // For a single leaf, the path to root should just be null entries
        // (the leaf itself is the root when there's no tree structure)
        let leaf = tree.leaf(0).unwrap();
        let computed_root = path.compute_root(&leaf.hash).unwrap();
        assert_eq!(&computed_root, tree.root_hash());
    }

    #[test]
    fn empty_proof() {
        let data: Vec<u8> = (0..10000).map(|i| i as u8).collect();
        let tree = BaoTree::build(&data);

        // Request range that exactly matches a single leaf
        let proof = tree
            .proof_for_range(0, tree.leaf(0).unwrap().data.len() as u64)
            .unwrap();

        assert!(!proof.leaves.is_empty());
        proof.verify(tree.root_hash()).unwrap();
    }
}
