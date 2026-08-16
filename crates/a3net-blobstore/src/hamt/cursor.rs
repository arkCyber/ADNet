//! HAMT Cursor for navigating the directory tree.
//!
//! Provides a cursor-based interface for traversing and modifying
//! HAMT directory structures.

use super::{HamtEntry, HamtLink, HamtNode, HamtShard};

/// A cursor for navigating a HAMT directory.
#[derive(Debug, Clone)]
pub struct HamtCursor {
    /// Current position in the tree.
    path: Vec<CursorPosition>,
}

/// Current position in the tree.
#[derive(Debug, Clone)]
struct CursorPosition {
    /// The node we're currently in.
    node_index: usize,
    /// The link we're pointing at (if any).
    link_index: Option<usize>,
    /// Depth in the tree.
    depth: u8,
}

impl HamtCursor {
    /// Create a new cursor at the root.
    pub fn new() -> Self {
        Self {
            path: vec![CursorPosition {
                node_index: 0,
                link_index: None,
                depth: 0,
            }],
        }
    }

    /// Check if we're at the root.
    pub fn is_at_root(&self) -> bool {
        self.path.len() == 1 && self.path[0].depth == 0
    }

    /// Get the current depth.
    pub fn depth(&self) -> u8 {
        self.path.last().map(|p| p.depth).unwrap_or(0)
    }

    /// Move to the root.
    pub fn go_to_root(&mut self) {
        self.path.truncate(1);
        if let Some(pos) = self.path.first_mut() {
            pos.node_index = 0;
            pos.link_index = None;
            pos.depth = 0;
        }
    }

    /// Navigate to a child by name.
    /// Returns whether the navigation was successful.
    pub fn navigate_to(&mut self, name: &str, shard: &HamtShard) -> bool {
        // Find the entry by name
        let hash = blake3::hash(name.as_bytes());
        let hash_bytes = *hash.as_bytes();

        self.navigate_with_hash(name, &hash_bytes, &shard.root)
    }

    /// Navigate with a precomputed hash.
    fn navigate_with_hash(&mut self, name: &str, hash: &[u8; 32], node: &HamtNode) -> bool {
        use super::DEFAULT_FANOUT_BITS;

        let depth = self.depth();
        let index =
            super::HamtHasher::extract_bits(hash, depth * DEFAULT_FANOUT_BITS, DEFAULT_FANOUT_BITS);

        match node.get_child(index) {
            Some(HamtLink::Bucket { entries }) => {
                // Check if the entry exists in this bucket
                if entries.iter().any(|(n, _)| n == name) {
                    // Update cursor position
                    if let Some(pos) = self.path.last_mut() {
                        pos.link_index = Some(index);
                    }
                    true
                } else {
                    false
                }
            }
            Some(HamtLink::Shard { .. }) => {
                // Would need to descend into child shard
                false
            }
            None => false,
        }
    }

    /// Navigate up one level.
    pub fn navigate_up(&mut self) -> bool {
        if self.path.len() > 1 {
            self.path.pop();
            true
        } else {
            false
        }
    }

    /// Get the current entry if we're pointing at one.
    pub fn current_entry(&self, _shard: &HamtShard) -> Option<HamtEntry> {
        // Would need to implement based on current position
        None
    }

    /// Get the current path as a vector of indices.
    pub fn path_indices(&self) -> Vec<usize> {
        self.path.iter().filter_map(|p| p.link_index).collect()
    }
}

impl Default for HamtCursor {
    fn default() -> Self {
        Self::new()
    }
}

/// A builder for creating cursors at specific positions.
pub struct CursorBuilder {
    cursor: HamtCursor,
}

impl CursorBuilder {
    /// Create a new cursor builder.
    pub fn new() -> Self {
        Self {
            cursor: HamtCursor::new(),
        }
    }

    /// Add a level to the path.
    pub fn with_level(mut self, index: usize) -> Self {
        self.cursor.path.push(CursorPosition {
            node_index: self.cursor.path.len(),
            link_index: Some(index),
            depth: self.cursor.path.len() as u8,
        });
        self
    }

    /// Build the cursor.
    pub fn build(self) -> HamtCursor {
        self.cursor
    }
}

impl Default for CursorBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// A watched cursor that tracks changes.
#[derive(Debug)]
pub struct WatchedCursor {
    cursor: HamtCursor,
    generation: u64,
}

impl WatchedCursor {
    /// Create a new watched cursor.
    pub fn new() -> Self {
        Self {
            cursor: HamtCursor::new(),
            generation: 0,
        }
    }

    /// Get the underlying cursor.
    pub fn cursor(&self) -> &HamtCursor {
        &self.cursor
    }

    /// Get a mutable reference to the cursor.
    pub fn cursor_mut(&mut self) -> &mut HamtCursor {
        &mut self.cursor
    }

    /// Get the generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Increment the generation.
    pub fn invalidate(&mut self) {
        self.generation += 1;
    }

    /// Check if the cursor is still valid.
    pub fn is_valid(&self, current_generation: u64) -> bool {
        self.generation == current_generation
    }
}

impl Default for WatchedCursor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_creation() {
        let cursor = HamtCursor::new();
        assert!(cursor.is_at_root());
        assert_eq!(cursor.depth(), 0);
    }

    #[test]
    fn test_cursor_navigation() {
        use super::super::*;

        let mut shard = HamtShard::new();
        shard
            .insert(
                "file1.txt".to_string(),
                HamtEntry::File {
                    hash: a3net_types::ContentHash::from_bytes(b"content1"),
                    size_bytes: 8,
                },
            )
            .unwrap();

        let mut cursor = HamtCursor::new();
        assert!(cursor.is_at_root());

        // Navigate to the entry
        let found = cursor.navigate_to("file1.txt", &shard);
        assert!(found);
        assert_eq!(cursor.depth(), 0);
    }

    #[test]
    fn test_watched_cursor() {
        let mut watched = WatchedCursor::new();
        assert_eq!(watched.generation(), 0);
        assert!(watched.is_valid(0));

        watched.invalidate();
        assert_eq!(watched.generation(), 1);
        assert!(!watched.is_valid(0));
        assert!(watched.is_valid(1));
    }
}
