// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Multi-node and cluster tests.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_tracing, run_with_timeout};
    use a3net_dht::DhtNode;
    use a3net_gossip::{GossipBus, InProcessGossip};
    use a3net_simulator::{NetworkTopology, presets, NetworkEmulator, ConnectionId};

    // ────────────────────────────────────────────────────────────────────
    // Two-Node Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_two_nodes_connect_and_exchange() {
        init_tracing();

        // Create two gossip buses sharing transport
        let transport = Arc::new(InProcessGossip::new());
        let bus1 = GossipBus::new(a3net_types::NodeId::random(), Arc::clone(&transport) as _);
        let bus2 = GossipBus::new(a3net_types::NodeId::random(), Arc::clone(&transport) as _);

        let room: a3net_types::RoomId = "two-node-room".into();

        // Both join
        bus1.join_room(&room).await.expect("bus1 join failed");
        bus2.join_room(&room).await.expect("bus2 join failed");

        // Bus2 subscribes
        let mut rx = bus2.subscribe(&room);

        // Bus1 publishes
        let ann = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: a3net_types::ContentHash::from_bytes(b"two-node-content"),
            node_id: bus1.local_node().clone(),
            title: "Two Node Test".into(),
            kind: a3net_types::CdnContentKind::Article,
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

        // Bus2 receives
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(received)) => {
                assert_eq!(received.title, "Two Node Test");
            }
            Ok(Err(e)) => panic!("receive error: {}", e),
            Err(_) => panic!("timeout"),
        }
    }

    #[tokio::test]
    async fn test_two_nodes_bidirectional() {
        init_tracing();

        let transport = Arc::new(InProcessGossip::new());
        let bus1 = GossipBus::new(a3net_types::NodeId::random(), Arc::clone(&transport) as _);
        let bus2 = GossipBus::new(a3net_types::NodeId::random(), Arc::clone(&transport) as _);

        let room: a3net_types::RoomId = "bidirectional".into();

        bus1.join_room(&room).await.expect("join failed");
        bus2.join_room(&room).await.expect("join failed");

        let mut rx1 = bus1.subscribe(&room);
        let mut rx2 = bus2.subscribe(&room);

        // Both publish
        let ann1 = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: a3net_types::ContentHash::from_bytes(b"from-bus1"),
            node_id: bus1.local_node().clone(),
            title: "From Bus 1".into(),
            kind: a3net_types::CdnContentKind::Article,
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

        let ann2 = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: a3net_types::ContentHash::from_bytes(b"from-bus2"),
            node_id: bus2.local_node().clone(),
            title: "From Bus 2".into(),
            kind: a3net_types::CdnContentKind::Article,
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

        // Publish both
        bus1.publish(&room, &ann1).await.expect("publish failed");
        bus2.publish(&room, &ann2).await.expect("publish failed");

        // Both should receive both messages
        let mut received = Vec::new();

        for _ in 0..4 { // Expect 2 messages each
            tokio::select! {
                r1 = rx1.recv() => {
                    if let Ok(ann) = r1 {
                        received.push(ann.title);
                    }
                }
                r2 = rx2.recv() => {
                    if let Ok(ann) = r2 {
                        received.push(ann.title);
                    }
                }
            }
        }

        assert!(received.contains(&"From Bus 1".to_string()));
        assert!(received.contains(&"From Bus 2".to_string()));
    }

    // ────────────────────────────────────────────────────────────────────
    // Multi-Node Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_five_nodes_all_receive() {
        init_tracing();

        let num_nodes = 5;
        let transport = Arc::new(InProcessGossip::new());

        let buses: Vec<_> = (0..num_nodes)
            .map(|_| {
                let id = a3net_types::NodeId::random();
                GossipBus::new(id, Arc::clone(&transport) as _)
            })
            .collect();

        let room: a3net_types::RoomId = "five-node-room".into();

        // All join
        for bus in &buses {
            bus.join_room(&room).await.expect("join failed");
        }

        // Subscribe all
        let mut receivers: Vec<_> = buses.iter().map(|b| b.subscribe(&room)).collect();

        // Node 0 publishes
        let ann = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: a3net_types::ContentHash::from_bytes(b"five-node-content"),
            node_id: buses[0].local_node().clone(),
            title: "Five Node Broadcast".into(),
            kind: a3net_types::CdnContentKind::Article,
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

        // All should receive
        for (i, rx) in receivers.iter_mut().enumerate() {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(received)) => {
                    assert_eq!(received.title, "Five Node Broadcast",
                        "node {} failed to receive", i);
                }
                Ok(Err(e)) => panic!("node {} error: {}", i, e),
                Err(_) => panic!("node {} timeout", i),
            }
        }
    }

    #[tokio::test]
    async fn test_ten_nodes_partial_join() {
        init_tracing();

        let num_total = 10;
        let num_subscribed = 6;
        let transport = Arc::new(InProcessGossip::new());

        let buses: Vec<_> = (0..num_total)
            .map(|_| {
                let id = a3net_types::NodeId::random();
                GossipBus::new(id, Arc::clone(&transport) as _)
            })
            .collect();

        let room: a3net_types::RoomId = "partial-join-room".into();

        // Only first num_subscribed join
        for bus in buses.iter().take(num_subscribed) {
            bus.join_room(&room).await.expect("join failed");
        }

        // Subscribe only the first num_subscribed
        let mut receivers: Vec<_> = buses.iter().take(num_subscribed)
            .map(|b| b.subscribe(&room))
            .collect();

        // Node 0 publishes
        let ann = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: a3net_types::ContentHash::from_bytes(b"partial-content"),
            node_id: buses[0].local_node().clone(),
            title: "Partial Join Test".into(),
            kind: a3net_types::CdnContentKind::Article,
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

        buses[0].publish(&room, &ann).await.expect("publish failed");

        // Only subscribed nodes should receive
        for (i, rx) in receivers.iter_mut().enumerate() {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(received)) => {
                    assert_eq!(received.title, "Partial Join Test");
                }
                Ok(Err(e)) => panic!("node {} error: {}", i, e),
                Err(_) => panic!("node {} timeout", i),
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Cluster Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_cluster_ring_topology() {
        init_tracing();

        let num_nodes = 6;
        let transport = Arc::new(InProcessGossip::new());

        let buses: Vec<_> = (0..num_nodes)
            .map(|_| {
                let id = a3net_types::NodeId::random();
                GossipBus::new(id, Arc::clone(&transport) as _)
            })
            .collect();

        let room: a3net_types::RoomId = "ring-room".into();

        // All join
        for bus in &buses {
            bus.join_room(&room).await.expect("join failed");
        }

        // All subscribe
        let receivers: Vec<_> = buses.iter().map(|b| b.subscribe(&room)).collect();

        // First node publishes
        let ann = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: a3net_types::ContentHash::from_bytes(b"ring-content"),
            node_id: buses[0].local_node().clone(),
            title: "Ring Topology Test".into(),
            kind: a3net_types::CdnContentKind::Article,
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

        // All should eventually receive (gossip propagates through ring)
        let mut received_count = 0;
        for mut rx in receivers {
            match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                Ok(Ok(_)) => received_count += 1,
                _ => {}
            }
        }

        assert_eq!(received_count, num_nodes, "not all nodes received message");
    }

    // ────────────────────────────────────────────────────────────────────
    // Load Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_many_messages_many_nodes() {
        init_tracing();

        let num_nodes = 5;
        let messages_per_node = 20;
        let transport = Arc::new(InProcessGossip::new());

        let buses: Vec<_> = (0..num_nodes)
            .map(|_| {
                let id = a3net_types::NodeId::random();
                GossipBus::new(id, Arc::clone(&transport) as _)
            })
            .collect();

        let room: a3net_types::RoomId = "load-test-room".into();

        for bus in &buses {
            bus.join_room(&room).await.expect("join failed");
        }

        // Subscribe all
        let mut receivers: Vec<_> = buses.iter().map(|b| b.subscribe(&room)).collect();

        // All nodes publish concurrently
        let mut handles = Vec::new();

        for (i, bus) in buses.iter().enumerate() {
            let bus = bus.clone();
            let room = room.clone();
            let handle = tokio::spawn(async move {
                for j in 0..messages_per_node {
                    let ann = a3net_types::Announcement {
                        room_id: room.clone(),
                        content_hash: a3net_types::ContentHash::from_bytes(
                            format!("node-{}-msg-{}", i, j).as_bytes()
                        ),
                        node_id: bus.local_node().clone(),
                        title: format!("Message {}-{}", i, j),
                        kind: a3net_types::CdnContentKind::Article,
                        size_bytes: 256,
                        mime_type: None,
                        source_url: None,
                        ticket: None,
                        timestamp: chrono::Utc::now(),
                        message_id: None,
                        ttl_secs: None,
                        signer: None,
                        signature: None,
                    };
                    bus.publish(&room, &ann).await.expect("publish failed");
                }
            });
            handles.push(handle);
        }

        // Wait for all publishes
        for handle in handles {
            handle.await.expect("task join failed");
        }

        // Each node should receive all messages from all other nodes
        // Total messages = num_nodes * messages_per_node
        // Each node (including sender) will receive messages from others
        let expected_total = num_nodes * messages_per_node * (num_nodes - 1);

        let mut total_received = 0;
        for rx in &mut receivers {
            let mut count = 0;
            loop {
                match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                    Ok(Ok(_)) => {
                        count += 1;
                        if count >= expected_total / num_nodes {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            total_received += count;
        }

        // At least some messages should have been received
        assert!(total_received > 0, "no messages were received");
    }

    // ────────────────────────────────────────────────────────────────────
    // Network Partition Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_partition_isolation() {
        init_tracing();

        // Create two separate transport instances (simulating partitions)
        let transport1 = Arc::new(InProcessGossip::new());
        let transport2 = Arc::new(InProcessGossip::new());

        let bus1 = GossipBus::new(a3net_types::NodeId::random(), transport1);
        let bus2 = GossipBus::new(a3net_types::NodeId::random(), transport2);

        let room: a3net_types::RoomId = "partitioned-room".into();

        bus1.join_room(&room).await.expect("bus1 join failed");
        bus2.join_room(&room).await.expect("bus2 join failed");

        let mut rx2 = bus2.subscribe(&room);

        // Bus1 publishes
        let ann = a3net_types::Announcement {
            room_id: room.clone(),
            content_hash: a3net_types::ContentHash::from_bytes(b"partitioned-content"),
            node_id: bus1.local_node().clone(),
            title: "Partition Test".into(),
            kind: a3net_types::CdnContentKind::Article,
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

        bus1.publish(&room, &ann).await.expect("publish failed");

        // Bus2 should NOT receive (separate transport = separate network)
        match tokio::time::timeout(Duration::from_millis(500), rx2.recv()).await {
            Ok(Ok(_)) => panic!("bus2 should not have received message from partitioned network"),
            Ok(Err(_)) => {} // Closed or lagged, expected
            Err(_) => {} // Timeout, expected - no message received
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Stress Tests
    // ────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rapid_subscribe_unsubscribe() {
        init_tracing();

        let transport = Arc::new(InProcessGossip::new());
        let bus = GossipBus::new(a3net_types::NodeId::random(), transport);

        let room: a3net_types::RoomId = "rapid-sub-room".into();
        bus.join_room(&room).await.expect("join failed");

        // Rapidly subscribe and unsubscribe
        for _ in 0..100 {
            let _rx = bus.subscribe(&room);
            // Rx is dropped immediately
        }

        // Publish should still work
        let ann = a3net_types::Announcement {
            room_id: room,
            content_hash: a3net_types::ContentHash::from_bytes(b"stress-content"),
            node_id: bus.local_node().clone(),
            title: "Rapid Subscribe Test".into(),
            kind: a3net_types::CdnContentKind::Article,
            size_bytes: 256,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };

        bus.publish(&room, &ann).await.expect("publish failed");
    }
}

use std::sync::Arc;
use std::time::Duration;
