//! Bitswap Wantlist Management and Synchronization
//!
//! This module implements the complete Wantlist protocol for Bitswap,
//! including:
//! - Full wantlist tracking per peer
//! - Periodic wantlist synchronization
//! - Wantlist broadcast on connection
//!
//! ## IPFS Bitswap Wantlist Protocol
//!
//! When a peer connects via Bitswap, both sides should:
//! 1. Exchange their current wantlists
//! 2. Process incoming wants and update local state
//! 3. Broadcast local wants to other peers (optional optimization)
//!
//! ## DO-178C Traceability
//!
//! - BITSWAP-6: Wantlist synchronization ensures peer state consistency
//! - BITSWAP-7: Batch operations for bandwidth optimization

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use thiserror::Error;

use crate::BitswapMessage;
use a3net_types::ContentHash;

// ─────────────────────────────────────────────────────────────────
// Error Types
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum WantlistError {
    #[error("wantlist full: {0} entries (max: {1})")]
    Full(usize, usize),

    #[error("want not found: {0}")]
    NotFound(ContentHash),

    #[error("peer not found: {0}")]
    PeerNotFound(String),

    #[error("invalid priority: {0}")]
    InvalidPriority(i32),
}

pub type Result<T> = std::result::Result<T, WantlistError>;

// ─────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────

/// Maximum entries per peer's wantlist.
pub const MAX_WANTLIST_SIZE: usize = 1024;

/// Maximum total entries across all peers.
pub const MAX_TOTAL_WANTS: usize = 10240;

/// Default wantlist broadcast interval.
pub const DEFAULT_WANTLIST_SYNC_INTERVAL: Duration = Duration::from_secs(30);

// ─────────────────────────────────────────────────────────────────
// Want Entry
// ─────────────────────────────────────────────────────────────────

/// A single entry in a peer's wantlist.
#[derive(Debug, Clone)]
pub struct WantEntry {
    /// The content hash being wanted.
    pub block: ContentHash,
    /// Priority (higher = more urgent).
    pub priority: i32,
    /// Whether this is a full block request or just want-have.
    pub want_type: WantType,
    /// Whether to send DONT_HAVE if not found.
    pub send_dont_have: bool,
    /// When this want was added.
    created_at: Instant,
    /// When this want should expire (if applicable).
    expires_at: Option<Instant>,
    /// The session this want belongs to (if any).
    pub session_id: Option<u64>,
}

/// Type of want request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantType {
    /// Full block requested.
    Block,
    /// Only want to know if peer has the block.
    Have,
}

impl Default for WantType {
    fn default() -> Self {
        Self::Block
    }
}

impl WantEntry {
    /// Create a new want-block entry.
    pub fn want_block(block: ContentHash, priority: i32) -> Self {
        Self {
            block,
            priority,
            want_type: WantType::Block,
            send_dont_have: false,
            created_at: Instant::now(),
            expires_at: None,
            session_id: None,
        }
    }

    /// Create a new want-have entry.
    pub fn want_have(block: ContentHash, priority: i32) -> Self {
        Self {
            block,
            priority,
            want_type: WantType::Have,
            send_dont_have: true,
            created_at: Instant::now(),
            expires_at: None,
            session_id: None,
        }
    }

    /// Check if this entry is expired.
    pub fn is_expired(&self) -> bool {
        if let Some(expires) = self.expires_at {
            Instant::now() > expires
        } else {
            false
        }
    }

    /// Set expiration time.
    pub fn with_expiry(mut self, duration: Duration) -> Self {
        self.expires_at = Some(Instant::now() + duration);
        self
    }

    /// Set session ID.
    pub fn with_session(mut self, session_id: u64) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Get creation time.
    pub fn created_at(&self) -> Instant {
        self.created_at
    }

    /// Get expiration time.
    pub fn expires_at(&self) -> Option<Instant> {
        self.expires_at
    }
}

// ─────────────────────────────────────────────────────────────────
// Per-Peer Wantlist
// ─────────────────────────────────────────────────────────────────

/// A peer's complete wantlist with full tracking.
#[derive(Debug, Clone)]
pub struct PeerWantlist {
    /// Peer ID.
    peer_id: String,
    /// Want entries indexed by content hash.
    wants: HashMap<ContentHash, WantEntry>,
    /// Pending wants (sent but not yet responded).
    pending: HashSet<ContentHash>,
    /// Last full sync timestamp.
    last_sync: Instant,
    /// Whether the wantlist has pending changes.
    dirty: bool,
    /// Total bytes wanted.
    want_bytes: u64,
}

impl PeerWantlist {
    /// Create a new empty wantlist for a peer.
    pub fn new(peer_id: String) -> Self {
        Self {
            peer_id,
            wants: HashMap::new(),
            pending: HashSet::new(),
            last_sync: Instant::now(),
            dirty: false,
            want_bytes: 0,
        }
    }

    /// Get the peer ID.
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// Number of entries in the wantlist.
    pub fn len(&self) -> usize {
        self.wants.len()
    }

    /// Check if the wantlist is empty.
    pub fn is_empty(&self) -> bool {
        self.wants.is_empty()
    }

    /// Check if the wantlist has changes since last sync.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Get the last sync time.
    pub fn last_sync(&self) -> Instant {
        self.last_sync
    }

    /// Get a want entry by block hash.
    pub fn get(&self, block: &ContentHash) -> Option<&WantEntry> {
        self.wants.get(block)
    }

    /// Check if we want a specific block.
    pub fn contains(&self, block: &ContentHash) -> bool {
        self.wants.contains_key(block)
    }

    /// Check if we're waiting for a response for a block.
    pub fn is_pending(&self, block: &ContentHash) -> bool {
        self.pending.contains(block)
    }

    /// Get total bytes wanted.
    pub fn want_bytes(&self) -> u64 {
        self.want_bytes
    }

    /// Get all want entries sorted by priority.
    pub fn entries_by_priority(&self) -> Vec<&WantEntry> {
        let mut entries: Vec<_> = self.wants.values().collect();
        entries.sort_by(|a, b| b.priority.cmp(&a.priority));
        entries
    }

    /// Get all pending blocks.
    pub fn pending_blocks(&self) -> Vec<ContentHash> {
        self.pending.iter().cloned().collect()
    }

    /// Add a want entry.
    pub fn add_want(&mut self, entry: WantEntry) -> Result<()> {
        if self.wants.len() >= MAX_WANTLIST_SIZE {
            return Err(WantlistError::Full(self.wants.len(), MAX_WANTLIST_SIZE));
        }

        let block = entry.block.clone();
        let old_priority = self.wants.get(&block).map(|e| e.priority);
        self.wants.insert(block.clone(), entry);

        // Update pending set
        self.pending.insert(block.clone());

        // Update dirty flag
        self.dirty = true;

        // Log priority change
        if let Some(old) = old_priority {
            tracing::debug!(
                "updated priority for {}: {} -> {}",
                block.short(),
                old,
                self.wants.get(&block).map(|e| e.priority).unwrap_or(0)
            );
        }

        Ok(())
    }

    /// Add a full block want.
    pub fn add_want_block(&mut self, block: ContentHash, priority: i32) -> Result<()> {
        self.add_want(WantEntry::want_block(block, priority))
    }

    /// Add a want-have.
    pub fn add_want_have(&mut self, block: ContentHash, priority: i32) -> Result<()> {
        self.add_want(WantEntry::want_have(block, priority))
    }

    /// Remove a want.
    pub fn remove_want(&mut self, block: &ContentHash) -> Option<WantEntry> {
        self.dirty = true;
        self.pending.remove(block);
        self.wants.remove(block)
    }

    /// Mark a want as no longer pending (received response).
    pub fn mark_received(&mut self, block: &ContentHash) {
        self.pending.remove(block);
    }

    /// Mark the wantlist as synced.
    pub fn mark_synced(&mut self) {
        self.last_sync = Instant::now();
        self.dirty = false;
    }

    /// Clean up expired entries.
    pub fn cleanup_expired(&mut self) -> Vec<ContentHash> {
        let expired: Vec<_> = self
            .wants
            .iter()
            .filter(|(_, entry)| entry.is_expired())
            .map(|(hash, _)| hash.clone())
            .collect();

        for hash in &expired {
            self.wants.remove(hash);
            self.pending.remove(hash);
        }

        if !expired.is_empty() {
            self.dirty = true;
        }

        expired
    }

    /// Generate cancel messages for all entries.
    pub fn to_cancel_messages(&self) -> Vec<BitswapMessage> {
        self.wants
            .keys()
            .map(|block| BitswapMessage::Cancel { block: block.clone() })
            .collect()
    }

    /// Generate want messages for all entries (for full sync).
    pub fn to_want_messages(&self) -> Vec<BitswapMessage> {
        self.wants
            .values()
            .map(|entry| {
                if entry.want_type == WantType::Have {
                    BitswapMessage::WantHave {
                        block: entry.block.clone(),
                        priority: entry.priority,
                        send_dont_have: entry.send_dont_have,
                    }
                } else {
                    BitswapMessage::WantBlock {
                        block: entry.block.clone(),
                        priority: entry.priority,
                    }
                }
            })
            .collect()
    }

    /// Export all entries for serialization.
    pub fn entries(&self) -> Vec<&WantEntry> {
        self.wants.values().collect()
    }
}

// ─────────────────────────────────────────────────────────────────
// Global Wantlist Manager
// ─────────────────────────────────────────────────────────────────

/// Manages wantlists across all connected peers.
pub struct WantlistManager {
    /// Per-peer wantlists.
    peer_wantlists: RwLock<HashMap<String, Arc<RwLock<PeerWantlist>>>>,
    /// Global set of all wanted blocks.
    global_wants: RwLock<HashSet<ContentHash>>,
    /// Max entries per peer.
    max_per_peer: usize,
    /// Cleanup interval.
    cleanup_interval: Duration,
}

impl WantlistManager {
    /// Create a new wantlist manager.
    pub fn new() -> Self {
        Self {
            peer_wantlists: RwLock::new(HashMap::new()),
            global_wants: RwLock::new(HashSet::new()),
            max_per_peer: MAX_WANTLIST_SIZE,
            cleanup_interval: DEFAULT_WANTLIST_SYNC_INTERVAL,
        }
    }

    /// Create with custom limits.
    pub fn with_limits(max_per_peer: usize, cleanup_interval: Duration) -> Self {
        Self {
            peer_wantlists: RwLock::new(HashMap::new()),
            global_wants: RwLock::new(HashSet::new()),
            max_per_peer,
            cleanup_interval,
        }
    }

    /// Get or create a peer's wantlist.
    pub fn get_or_create(&self, peer_id: &str) -> Arc<RwLock<PeerWantlist>> {
        let guard = self.peer_wantlists.read();
        if let Some(wantlist) = guard.get(peer_id) {
            return wantlist.clone();
        }
        drop(guard);

        let mut guard = self.peer_wantlists.write();
        // Double-check after acquiring write lock
        if let Some(wantlist) = guard.get(peer_id) {
            return wantlist.clone();
        }

        let wantlist = Arc::new(RwLock::new(PeerWantlist::new(peer_id.to_string())));
        guard.insert(peer_id.to_string(), wantlist.clone());
        wantlist
    }

    /// Get a peer's wantlist if it exists.
    pub fn get(&self, peer_id: &str) -> Option<Arc<RwLock<PeerWantlist>>> {
        self.peer_wantlists.read().get(peer_id).cloned()
    }

    /// Remove a peer's wantlist.
    pub fn remove(&self, peer_id: &str) -> Option<Arc<RwLock<PeerWantlist>>> {
        let removed = self.peer_wantlists.write().remove(peer_id);

        // Update global wants
        if let Some(wantlist) = &removed {
            let mut global = self.global_wants.write();
            for entry in wantlist.read().entries() {
                // Only remove from global if no other peer wants it
                if !self.is_wanted_elsewhere(peer_id, &entry.block) {
                    global.remove(&entry.block);
                }
            }
        }

        removed
    }

    /// Check if a block is wanted by any peer.
    pub fn is_wanted(&self, block: &ContentHash) -> bool {
        self.global_wants.read().contains(block)
    }

    /// Check if a block is wanted by any peer except the given one.
    fn is_wanted_elsewhere(&self, exclude_peer: &str, block: &ContentHash) -> bool {
        let wantlists = self.peer_wantlists.read();
        for (peer_id, wantlist) in wantlists.iter() {
            if peer_id != exclude_peer && wantlist.read().contains(block) {
                return true;
            }
        }
        false
    }

    /// Get all peers that want a specific block.
    pub fn get_wanters(&self, block: &ContentHash) -> Vec<String> {
        self.peer_wantlists
            .read()
            .iter()
            .filter(|(_, wantlist)| wantlist.read().contains(block))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Add a want for a peer.
    pub fn add_want(&self, peer_id: &str, entry: WantEntry) -> Result<()> {
        let wantlist = self.get_or_create(peer_id);
        let block = entry.block.clone();

        wantlist.write().add_want(entry)?;

        // Update global wants
        self.global_wants.write().insert(block);

        Ok(())
    }

    /// Add a want-block.
    pub fn add_want_block(&self, peer_id: &str, block: ContentHash, priority: i32) -> Result<()> {
        self.add_want(peer_id, WantEntry::want_block(block, priority))
    }

    /// Add a want-have.
    pub fn add_want_have(&self, peer_id: &str, block: ContentHash, priority: i32) -> Result<()> {
        self.add_want(peer_id, WantEntry::want_have(block, priority))
    }

    /// Remove a want for a peer.
    pub fn remove_want(&self, peer_id: &str, block: &ContentHash) -> Option<WantEntry> {
        let wantlist = self.get(peer_id)?;
        let entry = wantlist.write().remove_want(block);

        // Update global wants
        if entry.is_some() && !self.is_wanted_elsewhere(peer_id, block) {
            self.global_wants.write().remove(block);
        }

        entry
    }

    /// Mark a want as received.
    pub fn mark_received(&self, peer_id: &str, block: &ContentHash) {
        if let Some(wantlist) = self.get(peer_id) {
            wantlist.write().mark_received(block);
        }
    }

    /// Clean up all expired entries.
    pub fn cleanup_expired(&self) -> HashMap<String, Vec<ContentHash>> {
        let mut result = HashMap::new();
        let mut guard = self.peer_wantlists.write();

        for (peer_id, wantlist) in guard.iter_mut() {
            let expired = wantlist.write().cleanup_expired();
            if !expired.is_empty() {
                result.insert(peer_id.clone(), expired);
            }
        }

        // Clean up global wants
        let mut global = self.global_wants.write();
        global.retain(|block| {
            guard.values().any(|w| w.read().contains(block))
        });

        result
    }

    /// Get all dirty wantlists (need syncing).
    pub fn dirty_wantlists(&self) -> Vec<(String, Arc<RwLock<PeerWantlist>>)> {
        self.peer_wantlists
            .read()
            .iter()
            .filter(|(_, w)| w.read().is_dirty())
            .map(|(id, w)| (id.clone(), w.clone()))
            .collect()
    }

    /// Mark a wantlist as synced.
    pub fn mark_synced(&self, peer_id: &str) {
        if let Some(wantlist) = self.get(peer_id) {
            wantlist.write().mark_synced();
        }
    }

    /// Get statistics.
    pub fn stats(&self) -> WantlistStats {
        let guard = self.peer_wantlists.read();
        let mut total_entries = 0;
        let mut total_pending = 0;
        let mut dirty_count = 0;

        for wantlist in guard.values() {
            let wl = wantlist.read();
            total_entries += wl.len();
            total_pending += wl.pending_blocks().len();
            if wl.is_dirty() {
                dirty_count += 1;
            }
        }

        WantlistStats {
            peer_count: guard.len(),
            total_entries,
            total_pending,
            dirty_count,
            global_wants: self.global_wants.read().len(),
        }
    }

    /// Number of peers.
    pub fn peer_count(&self) -> usize {
        self.peer_wantlists.read().len()
    }

    /// Get all peer IDs.
    pub fn peer_ids(&self) -> Vec<String> {
        self.peer_wantlists.read().keys().cloned().collect()
    }
}

impl Default for WantlistManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the wantlist manager.
#[derive(Debug, Clone)]
pub struct WantlistStats {
    /// Number of peers with wantlists.
    pub peer_count: usize,
    /// Total want entries across all peers.
    pub total_entries: usize,
    /// Total pending wants.
    pub total_pending: usize,
    /// Number of dirty wantlists.
    pub dirty_count: usize,
    /// Number of globally wanted blocks.
    pub global_wants: usize,
}

// ─────────────────────────────────────────────────────────────────
// Serialization for Persistence
// ─────────────────────────────────────────────────────────────────

/// Serialized wantlist for persistence.
#[derive(Debug, Clone)]
pub struct SerializedWantlist {
    pub peer_id: String,
    pub wants: Vec<SerializedWantEntry>,
}

#[derive(Debug, Clone)]
pub struct SerializedWantEntry {
    pub block: String, // Hex-encoded content hash
    pub priority: i32,
    pub want_type: String,
    pub created_at_secs: u64,
    pub expires_at_secs: Option<u64>,
}

impl PeerWantlist {
    /// Serialize for persistence.
    pub fn serialize(&self) -> SerializedWantlist {
        SerializedWantlist {
            peer_id: self.peer_id.clone(),
            wants: self
                .wants
                .values()
                .map(|entry| SerializedWantEntry {
                    block: entry.block.to_string(),
                    priority: entry.priority,
                    want_type: match entry.want_type {
                        WantType::Block => "block".to_string(),
                        WantType::Have => "have".to_string(),
                    },
                    created_at_secs: entry.created_at.elapsed().as_secs(),
                    expires_at_secs: entry.expires_at.map(|t| t.elapsed().as_secs()),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_want_entry_creation() {
        let block = ContentHash::from_bytes(b"test");
        let entry = WantEntry::want_block(block.clone(), 10);

        assert_eq!(entry.priority, 10);
        assert_eq!(entry.want_type, WantType::Block);
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_want_entry_with_expiry() {
        let block = ContentHash::from_bytes(b"test");
        let entry = WantEntry::want_have(block, 5).with_expiry(Duration::from_secs(1));

        assert!(!entry.is_expired());

        std::thread::sleep(Duration::from_millis(1100));
        assert!(entry.is_expired());
    }

    #[test]
    fn test_peer_wantlist_basic_operations() {
        let mut wantlist = PeerWantlist::new("peer1".to_string());

        let block1 = ContentHash::from_bytes(b"block1");
        let block2 = ContentHash::from_bytes(b"block2");

        assert!(wantlist.is_empty());
        assert_eq!(wantlist.len(), 0);

        wantlist.add_want_block(block1.clone(), 10).unwrap();
        assert!(!wantlist.is_empty());
        assert_eq!(wantlist.len(), 1);
        assert!(wantlist.contains(&block1));
        assert!(wantlist.is_pending(&block1));

        wantlist.add_want_have(block2.clone(), 5).unwrap();
        assert_eq!(wantlist.len(), 2);

        // Priority ordering
        let entries = wantlist.entries_by_priority();
        assert_eq!(entries[0].block, block1); // priority 10
        assert_eq!(entries[1].block, block2); // priority 5

        // Remove
        let removed = wantlist.remove_want(&block1);
        assert!(removed.is_some());
        assert!(!wantlist.contains(&block1));
    }

    #[test]
    fn test_peer_wantlist_max_size() {
        let mut wantlist = PeerWantlist::new("peer1".to_string());

        // Fill to max
        for i in 0..MAX_WANTLIST_SIZE {
            let block = ContentHash::from_bytes(format!("block{}", i).as_bytes());
            wantlist.add_want_block(block, i as i32).unwrap();
        }

        assert_eq!(wantlist.len(), MAX_WANTLIST_SIZE);

        // Should fail to add more
        let block = ContentHash::from_bytes(b"overflow");
        let result = wantlist.add_want_block(block, 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_wantlist_manager_basic() {
        let manager = WantlistManager::new();

        let block = ContentHash::from_bytes(b"test");

        // Initially not wanted
        assert!(!manager.is_wanted(&block));

        // Add want
        manager.add_want_block("peer1", block.clone(), 10).unwrap();
        assert!(manager.is_wanted(&block));

        // Check wanters
        let wanters = manager.get_wanters(&block);
        assert_eq!(wanters, vec!["peer1".to_string()]);

        // Remove want
        manager.remove_want("peer1", &block);
        assert!(!manager.is_wanted(&block));
    }

    #[test]
    fn test_wantlist_manager_peer_tracking() {
        let manager = WantlistManager::new();

        assert_eq!(manager.peer_count(), 0);

        // Create wantlists
        manager.add_want_block("peer1", ContentHash::from_bytes(b"a"), 1).unwrap();
        manager.add_want_block("peer2", ContentHash::from_bytes(b"b"), 1).unwrap();

        assert_eq!(manager.peer_count(), 2);

        // Get existing
        let wl = manager.get("peer1");
        assert!(wl.is_some());

        // Remove peer
        manager.remove("peer1");
        assert_eq!(manager.peer_count(), 1);
    }

    #[test]
    fn test_wantlist_manager_stats() {
        let manager = WantlistManager::new();

        manager.add_want_block("peer1", ContentHash::from_bytes(b"a"), 10).unwrap();
        manager.add_want_block("peer2", ContentHash::from_bytes(b"b"), 5).unwrap();

        let stats = manager.stats();

        assert_eq!(stats.peer_count, 2);
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.global_wants, 2);
    }

    #[test]
    fn test_cancel_messages_generation() {
        let mut wantlist = PeerWantlist::new("peer1".to_string());

        wantlist.add_want_block(ContentHash::from_bytes(b"a"), 1).unwrap();
        wantlist.add_want_block(ContentHash::from_bytes(b"b"), 2).unwrap();

        let cancels = wantlist.to_cancel_messages();
        assert_eq!(cancels.len(), 2);

        for cancel in cancels {
            match cancel {
                BitswapMessage::Cancel { block: _ } => {}
                _ => panic!("expected Cancel message"),
            }
        }
    }

    #[test]
    fn test_dirty_flag() {
        let mut wantlist = PeerWantlist::new("peer1".to_string());

        assert!(!wantlist.is_dirty());

        wantlist.add_want_block(ContentHash::from_bytes(b"a"), 1).unwrap();
        assert!(wantlist.is_dirty());

        wantlist.mark_synced();
        assert!(!wantlist.is_dirty());
    }
}
