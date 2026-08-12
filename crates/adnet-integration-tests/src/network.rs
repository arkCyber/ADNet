// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Network integration tests.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_tracing, temp_dir};
    use adnet_dht::{DhtNode, DhtConfig, RoutingTable, Contact};
    use adnet_gossip::{GossipBus, InProcessGossip};
    use adnet_types::NodeId;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::sync::Arc;

    fn make_node_id() -> NodeId {
        NodeId::random()
    }

    fn make_contact(node_id: &NodeId, port: u16) -> Contact {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        Contact::new(node_id.clone(), addr)
    }

    // ────────────────────────────────────────────────────────────────────
    // DHT Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_dht_single_node() {
        init_tracing();

        let node_id = make_node_id();
        let config = DhtConfig::default();
        let _node = DhtNode::new(node_id, config).expect("failed to create DHT node");
    }

    #[tokio::test]
    async fn test_dht_routing_table_population() {
        init_tracing();

        let mut nodes = Vec::new();

        // Create 10 nodes
        for i in 0..10 {
            let node_id = make_node_id();
            let config = DhtConfig::default();
            let node = DhtNode::new(node_id, config).expect("failed to create DHT node");
            nodes.push(node);
        }

        // Add contacts to first node's routing table
        let local_id = nodes[0].node_id().clone();
        let mut routing_table = RoutingTable::new(local_id);

        for (i, node) in nodes.iter().enumerate().skip(1) {
            let contact = make_contact(node.node_id(), 9000 + i as u16);
            routing_table.insert(contact).expect("failed to insert contact");
        }

        assert!(routing_table.size() > 0);
    }

    #[tokio::test]
    async fn test_dht_find_closest_peers() {
        init_tracing();

        let local_id = make_node_id();
        let mut routing_table = RoutingTable::new(local_id.clone());

        // Add 100 random contacts
        for i in 0..100 {
            let node_id = make_node_id();
            let contact = make_contact(&node_id, 9000 + i as u16);
            routing_table.insert(contact).expect("failed to insert contact");
        }

        // Find closest to a random target
        let target = make_node_id();
        let closest = routing_table.get_closest(&target, 20);

        assert!(!closest.is_empty());
        assert!(closest.len() <= 20);
    }

    #[tokio::test]
    async fn test_dht_multiple_nodes_connect() {
        init_tracing();

        let num_nodes = 5;
        let mut dht_nodes = Vec::new();

        // Create nodes
        for i in 0..num_nodes {
            let node_id = make_node_id();
            let config = DhtConfig::default();
            let node = DhtNode::new(node_id, config).expect("failed to create DHT node");
            dht_nodes.push(node);
        }

        // Connect nodes in a ring
        for i in 0..num_nodes {
            let next = (i + 1) % num_nodes;
            let contact = make_contact(dht_nodes[next].node_id(), 9000 + next as u16);
            dht_nodes[i].add_contact(contact).await.expect("failed to add contact");
        }

        // Verify routing tables are populated
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        for node in &dht_nodes {
            let size = node.routing_table_size();
            assert!(size > 0, "routing table should have contacts");
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Gossip Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_gossip_two_nodes() {
        init_tracing();

        let transport = Arc::new(InProcessGossip::new());

        let node1_id = make_node_id();
        let node2_id = make_node_id();

        let bus1 = GossipBus::new(node1_id.clone(), Arc::clone(&transport) as _);
        let bus2 = GossipBus::new(node2_id.clone(), Arc::clone(&transport) as _);

        let room: adnet_types::RoomId = "test-room".into();

        // Both nodes join the room
        bus1.join_room(&room).await.expect("node1 join failed");
        bus2.join_room(&room).await.expect("node2 join failed");

        // Subscribe node2
        let mut rx = bus2.subscribe(&room);

        // Node1 publishes
        let ann = adnet_types::Announcement {
            room_id: room.clone(),
            content_hash: adnet_types::ContentHash::from_bytes(b"test-content"),
            node_id: node1_id,
            title: "Test Announcement".into(),
            kind: adnet_types::CdnContentKind::Article,
            size_bytes: 1024,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };

        bus1.publish(&room, &ann).await.expect("publish failed");

        // Node2 should receive
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(received)) => {
                assert_eq!(received.title, "Test Announcement");
            }
            Ok(Err(e)) => panic!("receive error: {}", e),
            Err(_) => panic!("timeout waiting for message"),
        }
    }

    #[tokio::test]
    async fn test_gossip_multi_node_broadcast() {
        init_tracing();

        let num_nodes = 10;
        let transport = Arc::new(InProcessGossip::new());

        // Create buses
        let buses: Vec<_> = (0..num_nodes)
            .map(|_| {
                let id = make_node_id();
                GossipBus::new(id, Arc::clone(&transport) as _)
            })
            .collect();

        let room: adnet_types::RoomId = "broadcast-room".into();

        // All nodes join
        for bus in &buses {
            bus.join_room(&room).await.expect("join failed");
        }

        // Subscribe all except the publisher
        let mut receivers: Vec<_> = buses[1..]
            .iter()
            .map(|b| b.subscribe(&room))
            .collect();

        // Node 0 publishes
        let ann = adnet_types::Announcement {
            room_id: room.clone(),
            content_hash: adnet_types::ContentHash::from_bytes(b"broadcast-content"),
            node_id: buses[0].local_node().clone(),
            title: "Broadcast Test".into(),
            kind: adnet_types::CdnContentKind::Article,
            size_bytes: 2048,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };

        buses[0].publish(&room, &ann).await.expect("publish failed");

        // All subscribers should receive
        for (i, rx) in receivers.iter_mut().enumerate() {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(received)) => {
                    assert_eq!(received.title, "Broadcast Test");
                }
                Ok(Err(e)) => panic!("receiver {} error: {}", i, e),
                Err(_) => panic!("receiver {} timeout", i),
            }
        }
    }

    #[tokio::test]
    async fn test_gossip_room_isolation() {
        init_tracing();

        let transport = Arc::new(InProcessGossip::new());

        let node_id = make_node_id();
        let bus = GossipBus::new(node_id, Arc::clone(&transport) as _);

        let room1: adnet_types::RoomId = "room-1".into();
        let room2: adnet_types::RoomId = "room-2".into();

        bus.join_room(&room1).await.expect("join room1 failed");
        bus.join_room(&room2).await.expect("join room2 failed");

        let mut rx1 = bus.subscribe(&room1);
        let mut rx2 = bus.subscribe(&room2);

        // Publish to room1
        let ann1 = adnet_types::Announcement {
            room_id: room1.clone(),
            content_hash: adnet_types::ContentHash::from_bytes(b"room1-content"),
            node_id: make_node_id(),
            title: "Room 1 Message".into(),
            kind: adnet_types::CdnContentKind::Article,
            size_bytes: 512,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };

        bus.publish(&room1, &ann1).await.expect("publish to room1 failed");

        // Only room1 subscriber should receive
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx1.recv()).await {
            Ok(Ok(received)) => {
                assert_eq!(received.title, "Room 1 Message");
            }
            _ => panic!("room1 should have received message"),
        }

        // Room2 should not receive (using try_recv)
        match rx2.try_recv() {
            Ok(_) => panic!("room2 should not have received room1's message"),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                // Expected - no message available
            }
            Err(e) => panic!("unexpected error: {}", e),
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Transport Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_transport_connection() {
        init_tracing();

        // This test would use actual transport implementation
        // For now, just verify the transport trait exists
        use adnet_transport::Transport;

        let _ = std::any::type_name::<dyn Transport>();
    }

    // ────────────────────────────────────────────────────────────────────
    // Cross-protocol Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_dht_gossip_integration() {
        init_tracing();

        // Test DHT and Gossip working together
        let transport = Arc::new(InProcessGossip::new());
        let node_id = make_node_id();

        let gossip_bus = GossipBus::new(node_id.clone(), transport);
        let dht_node = DhtNode::new(node_id, DhtConfig::default())
            .expect("failed to create DHT node");

        let room: adnet_types::RoomId = "dht-gossip-room".into();

        // Both should work independently
        gossip_bus.join_room(&room).await.expect("gossip join failed");

        // Verify DHT is operational
        let target = make_node_id();
        let _closest = dht_node.find_closest(target, 5).await;

        // Both systems are operational
    }
}
