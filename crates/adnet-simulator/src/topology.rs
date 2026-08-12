// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Network topology generator and simulator.

use crate::conditions::NetworkCondition;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Node role in the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeRole {
    /// Bootstrap node - always available.
    Bootstrap,
    /// Relay node - helps NAT traversal.
    Relay,
    /// Regular peer node.
    Peer,
    /// Exit node - provides internet access.
    Exit,
    /// Mobile node - often offline.
    Mobile,
}

/// Connection configuration between nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Latency in milliseconds.
    pub latency_ms: u64,
    /// Jitter in milliseconds.
    pub jitter_ms: u64,
    /// Packet loss rate (0.0 to 1.0).
    pub packet_loss: f64,
    /// Whether this is a high-latency link.
    pub high_latency: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            latency_ms: 50,
            jitter_ms: 5,
            packet_loss: 0.001,
            high_latency: false,
        }
    }
}

/// A node in the network topology.
#[derive(Debug, Clone)]
pub struct TopologyNode {
    pub id: String,
    pub role: NodeRole,
    pub connections: HashMap<String, ConnectionConfig>,
    pub condition: NetworkCondition,
}

impl TopologyNode {
    /// Create a new topology node.
    pub fn new(id: String, role: NodeRole) -> Self {
        let condition = match role {
            NodeRole::Mobile => {
                let mut cond = NetworkCondition::default();
                // Mobile nodes have higher latency and packet loss
                cond.packet_loss = Some(crate::conditions::PacketLoss::new(0.02));
                cond
            }
            NodeRole::Bootstrap | NodeRole::Relay => {
                // Infrastructure nodes have low latency
                let mut cond = NetworkCondition::default();
                cond.latency = Some(crate::conditions::Latency::new(10));
                cond
            }
            _ => NetworkCondition::default(),
        };

        Self {
            id,
            role,
            connections: HashMap::new(),
            condition,
        }
    }

    /// Add a connection to another node.
    pub fn add_connection(&mut self, peer_id: String, config: ConnectionConfig) {
        self.connections.insert(peer_id, config);
    }
}

/// Network topology representing the simulated network.
#[derive(Debug, Clone, Default)]
pub struct NetworkTopology {
    nodes: HashMap<String, TopologyNode>,
    partitions: Vec<Vec<String>>, // Groups of nodes that can talk to each other
}

impl NetworkTopology {
    /// Create a new empty topology.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node to the topology.
    pub fn add_node(&mut self, node: TopologyNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: &str) -> Option<&TopologyNode> {
        self.nodes.get(id)
    }

    /// Get a mutable node by ID.
    pub fn get_node_mut(&mut self, id: &str) -> Option<&mut TopologyNode> {
        self.nodes.get_mut(id)
    }

    /// Get all nodes.
    pub fn nodes(&self) -> &HashMap<String, TopologyNode> {
        &self.nodes
    }

    /// Check if two nodes are connected.
    pub fn are_connected(&self, id1: &str, id2: &str) -> bool {
        if let (Some(n1), Some(n2)) = (self.nodes.get(id1), self.nodes.get(id2)) {
            n1.connections.contains_key(id2) || n2.connections.contains_key(id1)
        } else {
            false
        }
    }

    /// Get the connection config between two nodes.
    pub fn connection_config(&self, id1: &str, id2: &str) -> Option<ConnectionConfig> {
        if let (Some(n1), Some(n2)) = (self.nodes.get(id1), self.nodes.get(id2)) {
            n1.connections.get(id2)
                .cloned()
                .or_else(|| n2.connections.get(id1).cloned())
        } else {
            None
        }
    }

    /// Create a complete graph topology (all nodes connected to all).
    pub fn complete_graph(node_ids: Vec<String>, base_config: ConnectionConfig) -> Self {
        let mut topology = Self::new();
        for id in &node_ids {
            let mut node = TopologyNode::new(id.clone(), NodeRole::Peer);
            for other in &node_ids {
                if other != id {
                    node.add_connection(other.clone(), base_config.clone());
                }
            }
            topology.add_node(node);
        }
        topology
    }

    /// Create a star topology (one hub connected to all spokes).
    pub fn star(hub_id: String, spoke_ids: Vec<String>, hub_config: ConnectionConfig, spoke_config: ConnectionConfig) -> Self {
        let mut topology = Self::new();

        // Create hub
        let mut hub = TopologyNode::new(hub_id.clone(), NodeRole::Relay);
        for spoke in &spoke_ids {
            hub.add_connection(spoke.clone(), hub_config.clone());
        }
        topology.add_node(hub);

        // Create spokes
        for spoke in spoke_ids {
            let mut node = TopologyNode::new(spoke.clone(), NodeRole::Peer);
            node.add_connection(hub_id.clone(), spoke_config.clone());
            topology.add_node(node);
        }

        topology
    }

    /// Create a mesh topology with specified connections.
    pub fn mesh(node_ids: Vec<String>, connections: Vec<(String, String, ConnectionConfig)>) -> Self {
        let mut topology = Self::new();

        for id in &node_ids {
            topology.add_node(TopologyNode::new(id.clone(), NodeRole::Peer));
        }

        for (id1, id2, config) in connections {
            if let Some(node) = topology.get_node_mut(&id1) {
                node.add_connection(id2.clone(), config.clone());
            }
            if let Some(node) = topology.get_node_mut(&id2) {
                node.add_connection(id1, config);
            }
        }

        topology
    }

    /// Create a partition in the network.
    pub fn create_partition(&mut self, partition: Vec<String>) {
        self.partitions.push(partition);
    }

    /// Check if two nodes are in the same partition.
    pub fn in_same_partition(&self, id1: &str, id2: &str) -> bool {
        for partition in &self.partitions {
            if partition.iter().any(|s| s == id1) && partition.iter().any(|s| s == id2) {
                return true;
            }
        }
        // If no partitions defined, all nodes can talk
        self.partitions.is_empty()
    }

    /// Generate a random topology for testing.
    pub fn random(num_nodes: usize, connection_probability: f64) -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        let mut topology = Self::new();

        for i in 0..num_nodes {
            let role = if i < 3 {
                NodeRole::Bootstrap
            } else if i < 6 {
                NodeRole::Relay
            } else {
                NodeRole::Peer
            };

            let node = TopologyNode::new(format!("node-{}", i), role);
            topology.add_node(node);
        }

        let node_ids: Vec<_> = topology.nodes.keys().cloned().collect();
        for (i, id1) in node_ids.iter().enumerate() {
            for id2 in &node_ids[i+1..] {
                if rng.gen_bool(connection_probability) {
                    let latency_ms = if rng.gen_bool(0.1) {
                        // 10% high latency
                        rng.gen_range(100..500)
                    } else {
                        rng.gen_range(10..100)
                    };

                    let config = ConnectionConfig {
                        latency_ms,
                        jitter_ms: latency_ms / 10,
                        packet_loss: rng.gen_range(0.0..0.02),
                        high_latency: latency_ms > 100,
                    };

                    if let Some(node) = topology.get_node_mut(id1) {
                        node.add_connection(id2.clone(), config.clone());
                    }
                    if let Some(node) = topology.get_node_mut(id2) {
                        node.add_connection(id1.clone(), config);
                    }
                }
            }
        }

        topology
    }

    /// Get topology statistics.
    pub fn stats(&self) -> TopologyStats {
        let num_nodes = self.nodes.len();
        let mut total_connections = 0;
        let mut avg_latency = 0u64;
        let mut max_latency = 0u64;

        for node in self.nodes.values() {
            for conn in node.connections.values() {
                total_connections += 1;
                avg_latency += conn.latency_ms;
                max_latency = max_latency.max(conn.latency_ms);
            }
        }

        let avg_latency = if total_connections > 0 {
            avg_latency / total_connections as u64
        } else {
            0
        };

        TopologyStats {
            num_nodes,
            total_connections: total_connections / 2, // Count each edge once
            avg_latency_ms: avg_latency,
            max_latency_ms: max_latency,
            num_partitions: self.partitions.len(),
        }
    }
}

/// Topology statistics.
#[derive(Debug, Clone, Default)]
pub struct TopologyStats {
    pub num_nodes: usize,
    pub total_connections: usize,
    pub avg_latency_ms: u64,
    pub max_latency_ms: u64,
    pub num_partitions: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complete_graph() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let topology = NetworkTopology::complete_graph(ids.clone(), ConnectionConfig::default());

        assert_eq!(topology.nodes.len(), 3);
        assert!(topology.are_connected("a", "b"));
        assert!(topology.are_connected("b", "c"));
        assert!(topology.are_connected("a", "c"));
    }

    #[test]
    fn test_star_topology() {
        let hub = "hub".to_string();
        let spokes = vec!["s1".to_string(), "s2".to_string(), "s3".to_string()];
        let topology = NetworkTopology::star(hub.clone(), spokes, ConnectionConfig::default(), ConnectionConfig::default());

        assert_eq!(topology.nodes.len(), 4);

        // All spokes connected to hub
        for spoke in &["s1", "s2", "s3"] {
            assert!(topology.are_connected(&hub, spoke));
        }

        // Spokes not connected to each other
        assert!(!topology.are_connected("s1", "s2"));
    }

    #[test]
    fn test_random_topology() {
        let topology = NetworkTopology::random(10, 0.5);
        let stats = topology.stats();

        assert_eq!(stats.num_nodes, 10);
        // With 50% probability, expect roughly 45% of possible edges
        assert!(stats.total_connections > 0);
    }
}
