//! Small example: drive the hub-server `ImManager` to create two
//! users, open a one-on-one conversation, send a few messages,
//! then verify the integrity hashes are stable across the
//! compressed-sync round-trip.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-chatstore --example chatstore_app
//! ```

use a3net_chatstore::im::generate_12digit_id;
use a3net_chatstore::{ChatType, ImManager};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open the hub database.
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("hub.db");
    let mgr = ImManager::new(&db_path)?;

    // 2. Two users. We also stash a fresh 12-digit id so the
    //    operator can see the format the hub server emits.
    let alice = mgr.create_user("alice", "Alice").await?;
    let bob = mgr.create_user("bob", "Bob").await?;
    let fresh = generate_12digit_id();
    println!("alice id : {}", alice.id);
    println!("bob   id : {}", bob.id);
    println!("fresh 12-digit helper: {fresh} (len = {})", fresh.len());
    assert_eq!(fresh.len(), 12);

    // 3. Open a conversation and add both users.
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "alice<->bob")
        .await?;
    mgr.add_group_member(&conv.id, &alice.id, "member").await?;
    mgr.add_group_member(&conv.id, &bob.id, "member").await?;

    // 4. Send three messages.
    for i in 1..=3 {
        mgr.send_message(
            &conv.id,
            &alice.id,
            Some(&bob.id),
            &format!("hi bob #{i}"),
            None,
        )
        .await?;
    }

    // 5. Read them back via uncompressed + compressed sync APIs and
    //    compare.
    let sync = mgr.get_messages_for_sync(&conv.id, None, 10).await?;
    let blob = mgr
        .get_compressed_messages_for_sync(&conv.id, None, 10)
        .await?;
    let decoded = ImManager::decompress_messages(&blob)?;

    println!("uncompressed sync: {} messages", sync.messages.len());
    println!("compressed blob:   {} bytes", blob.len());
    println!("decoded:           {} messages", decoded.len());

    assert_eq!(sync.messages.len(), 3);
    assert_eq!(decoded.len(), 3);
    for (a, b) in sync.messages.iter().zip(decoded.iter()) {
        assert_eq!(a, b);
    }
    println!("compressed <-> uncompressed round-trip: ok");
    Ok(())
}
