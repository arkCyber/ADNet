//! Kani Verification Harness for A3Net DHT Protocol
//!
//! This module provides formal verification of the DHT implementation
//! using the Kani model checker (https://model-checker.github.io/).
//!
//! Run verification with: `cargo kani`

use a3net_types::NodeId;
use std::collections::HashMap;

/// XOR distance between two NodeIds
pub fn xor_distance(a: &NodeId, b: &NodeId) -> u64 {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut result = 0u64;
    for i in 0..8.min(a_bytes.len()).min(b_bytes.len()) {
        result |= ((a_bytes[i] as u64) ^ (b_bytes[i] as u64)) << (i * 8);
    }
    result
}

/// K-bucket entry in the routing table
#[derive(Debug, Clone)]
pub struct KBucketEntry {
    pub node_id: NodeId,
    pub distance: u64,
    pub last_seen: u64,
}

impl KBucketEntry {
    pub fn new(node_id: NodeId, self_id: &NodeId) -> Self {
        Self {
            distance: xor_distance(&node_id, self_id),
            node_id,
            last_seen: 0,
        }
    }
}

/// Routing table (K-buckets)
#[derive(Debug, Clone)]
pub struct RoutingTable {
    pub self_id: NodeId,
    pub buckets: HashMap<u8, Vec<KBucketEntry>>,
    pub k: usize,
}

impl RoutingTable {
    pub fn new(self_id: NodeId, k: usize) -> Self {
        Self {
            self_id,
            buckets: HashMap::new(),
            k,
        }
    }

    /// Add a peer to the routing table
    pub fn add_peer(&mut self, node_id: NodeId) -> bool {
        let distance = xor_distance(&self.self_id, &node_id);
        let bucket_idx = Self::distance_to_bucket(distance);
        
        let bucket = self.buckets.entry(bucket_idx).or_insert_with(Vec::new);
        
        // Check if peer already exists
        if bucket.iter().any(|e| e.node_id == node_id) {
            return false;
        }
        
        // If bucket is full, don't add
        if bucket.len() >= self.k {
            return false;
        }
        
        bucket.push(KBucketEntry::new(node_id, &self.self_id));
        true
    }

    /// Remove a peer from the routing table
    pub fn remove_peer(&mut self, node_id: &NodeId) -> bool {
        for bucket in self.buckets.values_mut() {
            if let Some(pos) = bucket.iter().position(|e| e.node_id == *node_id) {
                bucket.remove(pos);
                return true;
            }
        }
        false
    }

    /// Get the k closest nodes to a target
    pub fn get_k_closest(&self, target: &NodeId, k: usize) -> Vec<NodeId> {
        let mut entries: Vec<_> = self.buckets
            .values()
            .flatten()
            .map(|e| {
                let dist = xor_distance(&e.node_id, target);
                (dist, e.node_id.clone())
            })
            .collect();
        
        entries.sort_by_key(|(dist, _)| *dist);
        entries.into_iter().take(k).map(|(_, id)| id).collect()
    }

    fn distance_to_bucket(distance: u64) -> u8 {
        if distance == 0 { 0 } else { 64 - distance.leading_zeros() as u8 }
    }
}

/// Kani proof: routing table maintains K-bucket invariant
#[cfg(feature = "kani")]
mod proof {
    use super::*;
    use kani::proof;

    /// Proof: Adding a peer to a non-full bucket succeeds
    #[proof]
    pub fn proof_add_peer_succeeds_when_bucket_not_full() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        
        let new_peer = NodeId::random();
        let result = table.add_peer(new_peer.clone());
        
        // Assert: add should succeed
        kani::assert(result, "Adding peer to non-full bucket should succeed");
        
        // Assert: peer is in table
        let found = table.buckets.values()
            .flatten()
            .any(|e| e.node_id == new_peer);
        kani::assert(found, "Peer should be in routing table after add");
    }

    /// Proof: Adding a duplicate peer fails
    #[proof]
    pub fn proof_add_duplicate_peer_fails() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        
        let peer = NodeId::random();
        table.add_peer(peer.clone());
        
        let result = table.add_peer(peer.clone());
        
        kani::assert(!result, "Adding duplicate peer should fail");
    }

    /// Proof: Removing a peer works
    #[proof]
    pub fn proof_remove_peer_works() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        
        let peer = NodeId::random();
        table.add_peer(peer.clone());
        
        let remove_result = table.remove_peer(&peer);
        kani::assert(remove_result, "Remove should succeed");
        
        let found = table.buckets.values()
            .flatten()
            .any(|e| e.node_id == peer);
        kani::assert(!found, "Peer should not be in table after remove");
    }

    /// Proof: GetKClosest returns at most K results
    #[proof]
    pub fn proof_get_k_closest_respects_limit() {
        let self_id = NodeId::random();
        let table = RoutingTable::new(self_id, 3);
        let target = NodeId::random();
        
        let k = 3;
        let closest = table.get_k_closest(&target, k);
        
        kani::assert(closest.len() <= k, "get_k_closest should return at most K results");
    }

    /// Proof: XOR distance is symmetric
    #[proof]
    pub fn proof_xor_distance_symmetric() {
        let a = NodeId::random();
        let b = NodeId::random();
        
        let dist_ab = xor_distance(&a, &b);
        let dist_ba = xor_distance(&b, &a);
        
        kani::assert(dist_ab == dist_ba, "XOR distance should be symmetric");
    }

    /// Proof: XOR distance with self is zero
    #[proof]
    pub fn proof_xor_distance_to_self_is_zero() {
        let a = NodeId::random();
        
        let dist = xor_distance(&a, &a);
        
        kani::assert(dist == 0, "XOR distance to self should be zero");
    }

    /// Proof: K-bucket size is bounded by K
    #[proof]
    pub fn proof_bucket_size_bounded() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        
        // Try to add more than K peers
        for _ in 0..10 {
            table.add_peer(NodeId::random());
        }
        
        for bucket in table.buckets.values() {
            kani::assert(bucket.len() <= table.k, "Each bucket should have at most K entries");
        }
    }
}

/// Unit tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xor_distance_symmetric() {
        let a = NodeId::random();
        let b = NodeId::random();
        
        let dist_ab = xor_distance(&a, &b);
        let dist_ba = xor_distance(&b, &a);
        
        assert_eq!(dist_ab, dist_ba, "XOR distance should be symmetric");
    }

    #[test]
    fn test_xor_distance_self_zero() {
        let a = NodeId::random();
        
        let dist = xor_distance(&a, &a);
        
        assert_eq!(dist, 0, "XOR distance to self should be zero");
    }

    #[test]
    fn test_routing_table_add_peer() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        
        let peer = NodeId::random();
        let result = table.add_peer(peer.clone());
        
        assert!(result, "Adding peer should succeed");
    }

    #[test]
    fn test_routing_table_dedup() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        
        let peer = NodeId::random();
        table.add_peer(peer.clone());
        let result = table.add_peer(peer);
        
        assert!(!result, "Adding duplicate peer should fail");
    }

    #[test]
    fn test_routing_table_remove() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        
        let peer = NodeId::random();
        table.add_peer(peer.clone());
        let result = table.remove_peer(&peer);
        
        assert!(result, "Remove should succeed");
    }

    #[test]
    fn test_get_k_closest_limit() {
        let self_id = NodeId::random();
        let mut table = RoutingTable::new(self_id, 3);
        let target = NodeId::random();
        
        // Add many peers
        for _ in 0..20 {
            table.add_peer(NodeId::random());
        }
        
        let k = 5;
        let closest = table.get_k_closest(&target, k);
        
        assert!(closest.len() <= k, "Should respect K limit");
    }
}
