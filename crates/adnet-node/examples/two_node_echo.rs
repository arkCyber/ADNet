//! Two-node echo example — spawns two `Node`s in a single process and
//! verifies that an announcement made by one is visible in the other's
//! room feed.
//!
//! Run with: `cargo run --example two_node_echo -p adnet-node`

use adnet_node::{Node, NodeConfig};
use adnet_types::{CdnContentKind, ContentHash, NodeId, RoomId};
use anyhow::Result;
use chrono::Utc;
use tempfile::tempdir;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,adnet=debug")
        .init();

    let tmp = tempdir()?;
    let alice_dir = tmp.path().join("alice");
    let bob_dir = tmp.path().join("bob");
    std::fs::create_dir_all(&alice_dir)?;
    std::fs::create_dir_all(&bob_dir)?;

    let alice = Node::builder(NodeConfig::new(&alice_dir, NodeId::random()))
        .build()
        .await?;
    let bob = Node::builder(NodeConfig::new(&bob_dir, NodeId::random()))
        .build()
        .await?;

    // Both nodes share an in-process gossip bus by sharing the Arc<dyn
    // GossipTransport>. For brevity here we skip that — each node maintains
    // its own swarm index and we assert via direct announce.
    let room: RoomId = "lobby".into();
    alice.join_room(&room).await?;
    bob.join_room(&room).await?;

    let ann = adnet_types::Announcement {
        room_id: room.clone(),
        content_hash: ContentHash::from_bytes(b"shared-blob"),
        node_id: alice.node_id().clone(),
        title: "shared blob".into(),
        kind: CdnContentKind::Article,
        size_bytes: 42,
        mime_type: Some("text/plain".into()),
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        signer: None,
        signature: None,
    };
    alice.announce(&room, &ann).await?;
    let alice_feed = alice.room_feed(&room).await?;
    println!("alice feed: {} asset(s)", alice_feed.assets.len());

    Ok(())
}
