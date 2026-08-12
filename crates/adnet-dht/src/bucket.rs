//! K-Bucket implementation for Kademlia DHT routing table.
//!
//! KBuckets maintain sorted lists of contacts by XOR distance from the local node.
//! Each bucket covers a range of distances (0-255 for 256-bit IDs).
//! Buckets are filled lazily and have a maximum size (K).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use adnet_types::NodeId;

/// Maximum number of contacts per bucket (K in Kademlia).
pub const KBUCKET_SIZE: usize = 20;

/// Contact in the DHT routing table.
#[derive(Debug, Clone)]
pub struct Contact {
    /// Node ID of the peer.
    pub id: NodeId,
    /// Known addresses for this peer.
    pub addrs: Vec<SocketAddr>,
    /// Last time we successfully contacted this peer.
    pub last_contacted: Instant,
    /// Last time this peer was seen online.
    pub last_seen: Instant,
    /// Whether this contact is trusted (e.g., our own node, bootstraps).
    pub trusted: bool,
}

impl Contact {
    /// Create a new contact.
    pub fn new(id: NodeId, addr: SocketAddr) -> Self {
        let now = Instant::now();
        Self {
            id,
            addrs: vec![addr],
            last_contacted: now,
            last_seen: now,
            trusted: false,
        }
    }

    /// Add a new address to this contact.
    pub fn add_addr(&mut self, addr: SocketAddr) {
        if !self.addrs.contains(&addr) {
            self.addrs.push(addr);
        }
    }

    /// Update last_seen timestamp.
    pub fn mark_seen(&mut self) {
        self.last_seen = Instant::now();
    }

    /// Check if this contact has been seen recently.
    pub fn is_alive(&self, timeout: Duration) -> bool {
        self.last_seen.elapsed() < timeout
    }
}

/// A single KBucket (one distance range).
#[derive(Debug, Clone)]
pub struct KBucket {
    /// Contacts in this bucket, sorted by last seen (oldest first).
    contacts: VecDeque<Contact>,
    /// Whether this bucket is "pending" (waiting to replace a failed contact).
    pending: Option<NodeId>,
    /// Last time this bucket was refreshed.
    last_refresh: Instant,
}

impl Default for KBucket {
    fn default() -> Self {
        Self::new()
    }
}

impl KBucket {
    /// Create a new empty bucket.
    pub fn new() -> Self {
        Self {
            contacts: VecDeque::new(),
            pending: None,
            last_refresh: Instant::now(),
        }
    }

    /// Number of contacts in this bucket.
    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    /// Whether the bucket is empty.
    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }

    /// Whether the bucket is full.
    pub fn is_full(&self) -> bool {
        self.contacts.len() >= KBUCKET_SIZE
    }

    /// Check if this bucket has a pending replacement.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Get the pending contact ID.
    pub fn pending_id(&self) -> Option<&NodeId> {
        self.pending.as_ref()
    }

    /// Set the pending contact.
    pub fn set_pending(&mut self, id: NodeId) {
        self.pending = Some(id);
    }

    /// Clear the pending contact.
    pub fn clear_pending(&mut self) {
        self.pending = None;
    }

    /// Get all contacts, sorted by last seen (oldest first).
    pub fn contacts(&self) -> impl Iterator<Item = &Contact> {
        self.contacts.iter()
    }

    /// Get all contacts mutably.
    pub fn contacts_mut(&mut self) -> impl Iterator<Item = &mut Contact> {
        self.contacts.iter_mut()
    }

    /// Find a contact by ID.
    pub fn find(&self, id: &NodeId) -> Option<&Contact> {
        self.contacts.iter().find(|c| &c.id == id)
    }

    /// Find a contact by ID mutably.
    pub fn find_mut(&mut self, id: &NodeId) -> Option<&mut Contact> {
        self.contacts.iter_mut().find(|c| &c.id == id)
    }

    /// Check if this bucket contains a contact.
    pub fn contains(&self, id: &NodeId) -> bool {
        self.find(id).is_some()
    }

    /// Insert a new contact into this bucket.
    /// Returns Ok(()) if successful, or an error if the bucket is full.
    pub fn insert(&mut self, contact: Contact) -> Result<(), InsertError> {
        // Check if contact already exists
        if let Some(existing) = self.find(&contact.id) {
            // Update existing contact
            let mut c = contact.clone();
            c.last_seen = existing.last_seen;
            c.last_contacted = existing.last_contacted;
            *self.find_mut(&contact.id).unwrap() = c;
            return Ok(());
        }

        // Check if bucket is full
        if self.contacts.len() >= KBUCKET_SIZE {
            return Err(InsertError::BucketFull);
        }

        self.contacts.push_back(contact);
        Ok(())
    }

    /// Mark a contact as seen (updates last_seen).
    pub fn mark_seen(&mut self, id: &NodeId) {
        if let Some(c) = self.find_mut(id) {
            c.mark_seen();
            // Move to back (most recently seen)
            let pos = self.contacts.iter().position(|x| &x.id == id).unwrap();
            let c = self.contacts.remove(pos).unwrap();
            self.contacts.push_back(c);
        }
    }

    /// Remove a contact.
    pub fn remove(&mut self, id: &NodeId) -> bool {
        let pos = self.contacts.iter().position(|c| &c.id == id);
        if let Some(pos) = pos {
            self.contacts.remove(pos);
            true
        } else {
            false
        }
    }

    /// Whether the bucket needs refresh.
    pub fn needs_refresh(&self, interval: Duration) -> bool {
        self.last_refresh.elapsed() >= interval
    }

    /// Get the least recently seen contact (for eviction).
    pub fn oldest(&self) -> Option<&Contact> {
        self.contacts.front()
    }

    /// Evict the least recently seen contact.
    pub fn evict_oldest(&mut self) -> Option<Contact> {
        self.contacts.pop_front()
    }

    /// Get contacts sorted by distance to a target key.
    pub fn closest_to(&self, target: &NodeId) -> Vec<&Contact> {
        let mut contacts: Vec<_> = self.contacts.iter().collect();
        contacts.sort_by(|a, b| {
            let da = a.id.xor_distance(target);
            let db = b.id.xor_distance(target);
            da.cmp(&db)
        });
        contacts
    }

    /// Mark this bucket as refreshed.
    pub fn mark_refreshed(&mut self) {
        self.last_refresh = std::time::Instant::now();
    }
}

/// Error when inserting a contact into a bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertError {
    BucketFull,
    SelfContact,
}

/// The routing table containing all KBuckets.
#[derive(Debug, Clone)]
pub struct RoutingTable {
    /// Local node ID (our identity).
    local_id: NodeId,
    /// All 256 buckets (for 256-bit node IDs, log distance 0-255).
    buckets: [KBucket; 256],
    /// Bootstrap nodes (never evicted).
    bootstrap_nodes: Vec<Contact>,
    /// Time before a contact is considered dead.
    contact_timeout: Duration,
    /// Refresh interval for buckets.
    refresh_interval: Duration,
}

impl RoutingTable {
    /// Create a new routing table for a local node.
    pub fn new(local_id: NodeId) -> Self {
        Self {
            local_id,
            buckets: [(); 256].map(|_| KBucket::new()),
            bootstrap_nodes: Vec::new(),
            contact_timeout: Duration::from_secs(600), // 10 minutes
            refresh_interval: Duration::from_secs(300), // 5 minutes
        }
    }

    /// Set the local node ID.
    pub fn set_local_id(&mut self, id: NodeId) {
        self.local_id = id;
    }

    /// Get the local node ID.
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

    /// Add a bootstrap node (never evicted).
    pub fn add_bootstrap_node(&mut self, contact: Contact) {
        let mut c = contact;
        c.trusted = true;
        self.bootstrap_nodes.push(c);
    }

    /// Get all bootstrap nodes.
    pub fn bootstrap_nodes(&self) -> impl Iterator<Item = &Contact> {
        self.bootstrap_nodes.iter()
    }

    /// Get the bucket index for a node ID (XOR log distance).
    pub fn bucket_index(node_a: &NodeId, node_b: &NodeId) -> usize {
        let distance = node_a.xor_distance(node_b);
        // Find the most significant set bit
        for (i, &byte) in distance.iter().enumerate() {
            if byte != 0 {
                for bit in (0..8).rev() {
                    if (byte >> bit) & 1 == 1 {
                        return 255 - (i * 8 + (7 - bit));
                    }
                }
            }
        }
        255 // Same node, use last bucket
    }

    /// Get the bucket for a node ID.
    pub fn bucket_for(&self, id: &NodeId) -> &KBucket {
        let idx = Self::bucket_index(&self.local_id, id);
        &self.buckets[idx.min(255)]
    }

    /// Get the bucket for a node ID mutably.
    pub fn bucket_for_mut(&mut self, id: &NodeId) -> &mut KBucket {
        let idx = Self::bucket_index(&self.local_id, id);
        &mut self.buckets[idx.min(255)]
    }

    /// Check if we know about a node.
    pub fn contains(&self, id: &NodeId) -> bool {
        self.bucket_for(id).find(id).is_some() || self.bootstrap_nodes.iter().any(|c| &c.id == id)
    }

    /// Insert a contact into the routing table.
    pub fn insert(&mut self, contact: Contact) -> Result<(), InsertError> {
        if contact.id == self.local_id {
            return Err(InsertError::SelfContact);
        }
        self.bucket_for_mut(&contact.id).insert(contact)
    }

    /// Mark a contact as seen.
    pub fn mark_seen(&mut self, id: &NodeId) {
        self.bucket_for_mut(id).mark_seen(id);
    }

    /// Remove a contact.
    pub fn remove(&mut self, id: &NodeId) -> bool {
        self.bucket_for_mut(id).remove(id)
    }

    /// Get k closest contacts to a target node ID.
    pub fn closest(&self, target: &NodeId, k: usize) -> Vec<Contact> {
        let mut all_contacts: Vec<Contact> = self
            .buckets
            .iter()
            .flat_map(|b| b.contacts().cloned())
            .chain(self.bootstrap_nodes.iter().cloned())
            .collect();

        // Don't include self
        all_contacts.retain(|c| c.id != self.local_id);

        // Sort by XOR distance
        all_contacts.sort_by(|a, b| {
            let da = a.id.xor_distance(target);
            let db = b.id.xor_distance(target);
            da.cmp(&db)
        });

        all_contacts.truncate(k);
        all_contacts
    }

    /// Get all contacts in the routing table.
    pub fn all_contacts(&self) -> impl Iterator<Item = &Contact> {
        self.buckets
            .iter()
            .flat_map(|b| b.contacts())
            .chain(self.bootstrap_nodes.iter())
    }

    /// Number of contacts in the routing table.
    pub fn num_contacts(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum::<usize>() + self.bootstrap_nodes.len()
    }

    /// Get buckets that need refresh.
    pub fn buckets_needing_refresh(&self) -> impl Iterator<Item = usize> + '_ {
        self.buckets.iter().enumerate().filter_map(|(i, b)| {
            if b.needs_refresh(self.refresh_interval) {
                Some(i)
            } else {
                None
            }
        })
    }

    /// Remove dead contacts from all buckets.
    pub fn remove_dead_contacts(&mut self) -> Vec<NodeId> {
        let mut removed = Vec::new();
        for bucket in &mut self.buckets {
            let dead: Vec<NodeId> = bucket
                .contacts()
                .filter(|c| !c.is_alive(self.contact_timeout))
                .map(|c| c.id.clone())
                .collect();
            for id in &dead {
                bucket.remove(id);
            }
            removed.extend(dead);
        }
        removed
    }

    /// Mark a specific bucket as refreshed.
    pub fn mark_bucket_refreshed(&mut self, bucket_idx: usize) {
        if bucket_idx < 256 {
            self.buckets[bucket_idx].mark_refreshed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_insert() {
        let mut bucket = KBucket::new();
        let peer = NodeId::random();
        let contact = Contact::new(peer.clone(), "127.0.0.1:8080".parse().unwrap());

        assert!(bucket.insert(contact.clone()).is_ok());
        assert!(bucket.contains(&peer));
        assert_eq!(bucket.len(), 1);
    }

    #[test]
    fn test_bucket_eviction() {
        let mut bucket = KBucket::new();

        // Fill the bucket
        for i in 0..KBUCKET_SIZE {
            let peer = NodeId::random();
            let contact = Contact::new(peer, format!("127.0.0.{}:8080", i + 1).parse().unwrap());
            let result = bucket.insert(contact);
            assert!(result.is_ok());
        }

        assert!(bucket.is_full());

        // Try to add one more - should fail
        let new_peer = NodeId::random();
        let new_contact = Contact::new(new_peer, "127.0.0.254:8080".parse().unwrap());
        assert!(bucket.insert(new_contact).is_err());
    }

    #[test]
    fn test_routing_table_insert() {
        let local_id = NodeId::random();
        let mut table = RoutingTable::new(local_id.clone());

        let peer = NodeId::random();
        let contact = Contact::new(peer.clone(), "127.0.0.1:8080".parse().unwrap());

        assert!(table.insert(contact).is_ok());
        assert!(table.contains(&peer));
    }

    #[test]
    fn test_closest_nodes() {
        let local_id = NodeId::random();
        let table = RoutingTable::new(local_id.clone());

        let target = NodeId::random();
        let closest = table.closest(&target, 10);
        assert!(closest.is_empty());
    }
}
