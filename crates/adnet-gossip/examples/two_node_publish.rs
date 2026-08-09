//! Two `GossipBus` instances connected through an `InProcessGossip` transport.
//! One node publishes an [`Announcement`] into a room; the other subscribes
//! and decodes it.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-gossip --example two_node_publish
//! ```

use std::sync::Arc;

use adnet_gossip::{GossipBus, InProcessGossip};
use adnet_types::{Announcement, CdnContentKind, ContentHash, NodeId, RoomId};
use chrono::Utc;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("rt");

    rt.block_on(async {
        let alice = NodeId::random();
        let bob = NodeId::random();
        println!("alice: {alice}");
        println!("bob  : {bob}");

        // Both nodes share one in-process transport.
        let transport: Arc<InProcessGossip> = Arc::new(InProcessGossip::default());
        let alice_bus = GossipBus::new(alice.clone(), transport.clone());
        let bob_bus = GossipBus::new(bob.clone(), transport.clone());

        let room = RoomId::new("ai-models");
        alice_bus.join_room(&room).await.expect("alice join");
        bob_bus.join_room(&room).await.expect("bob join");

        // Bob subscribes BEFORE alice publishes.
        let mut rx = bob_bus.subscribe(&room);

        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"hello gossip"),
            node_id: alice.clone(),
            title: "Llama 8B GGUF".into(),
            kind: CdnContentKind::AiModel,
            size_bytes: 4_500_000_000,
            mime_type: Some("application/octet-stream".into()),
            source_url: Some("https://example.com/llama.gguf".into()),
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
        };

        // Allow subscribe to register before publishing.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        alice_bus.publish(&room, &ann).await.expect("publish");
        println!("alice published into {room:?}");

        // Wait for bob to receive.
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("bob should receive")
            .expect("no lag/error");
        assert_eq!(received.content_hash, ann.content_hash);
        assert_eq!(received.title, "Llama 8B GGUF");
        assert_eq!(received.kind, CdnContentKind::AiModel);
        assert_eq!(received.node_id, alice);
        println!("bob received : {}", received.title);
        println!("  kind        : {:?}", received.kind);
        println!("  size        : {} bytes", received.size_bytes);

        alice_bus.leave_room(&room).await.expect("leave");
        bob_bus.leave_room(&room).await.expect("leave");
        println!("\nALL OK");
    });
}
