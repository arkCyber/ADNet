//! End-to-end walk-through of the chatstore crate.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p adnet-chatstore --example chatstore_demo
//! ```
//!
//! The example creates a temporary SQLite database, exercises both
//! the per-user `ChatStorage` and the hub-server `ImManager`, prints
//! a short report, and exits with status 0.

use adnet_chatstore::im::{ChatType, ImManager};
use adnet_chatstore::storage::{ChatStorage, ChatStorageConfig, Friend};
use adnet_types::group_chat::{DirectMessage, GroupMessage, MessageAttachment, MessageReceipt};
use adnet_types::invariants::{AttachmentKind, MessageType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // -----------------------------------------------------------------
    // 1. Open (or create) the per-user chat store
    // -----------------------------------------------------------------
    let dir = tempfile::tempdir()?;
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: dir.path().to_path_buf(),
    })?;
    println!("[1/5] per-user database opened at {}", dir.path().display());

    // -----------------------------------------------------------------
    // 2. Save a friend, send a 1-to-1 message and a group message
    // -----------------------------------------------------------------
    storage.save_friend(
        "alice",
        Friend {
            friend_id: "bob".into(),
            name: "Bob".into(),
            avatar_url: None,
            status: Some("online".into()),
            last_seen: Some(1_700_000_000),
            created_at: None,
            updated_at: None,
        },
    )?;
    println!("[2/5] alice added bob as a friend");

    let mut direct = DirectMessage {
        message_id: "m1".into(),
        chat_id: "dm:alice:bob".into(),
        sender_id: "alice".into(),
        receiver_id: "bob".into(),
        content: "hello bob".into(),
        message_type: MessageType::Text,
        attachments: vec![MessageAttachment {
            attachment_id: "att1".into(),
            file_type: AttachmentKind::Image,
            blob_hash: "a".repeat(64),
            file_name: "hello.png".into(),
            file_size: 4096,
            thumbnail_hash: None,
        }],
        reply_to: None,
        sequence: 1,
        timestamp: 1_700_000_001,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    direct.stamp_integrity_hash();
    storage.save_direct_message("alice", direct.clone())?;
    storage.save_direct_message("bob", direct.clone())?;
    println!("[3/5] direct message roundtripped for alice + bob");

    let mut group = GroupMessage {
        message_id: "g1".into(),
        group_id: "team".into(),
        sender_id: "alice".into(),
        sender_name: "Alice".into(),
        content: "standup at 10".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        mentions: vec!["bob".into()],
        sequence: 1,
        timestamp: 1_700_000_010,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    group.stamp_integrity_hash();
    for user in ["alice", "bob", "carol"] {
        storage.save_group_message(user, group.clone())?;
    }
    println!("[4/5] group message replicated across 3 users");

    storage.save_receipt(
        "bob",
        MessageReceipt {
            receipt_id: "r1".into(),
            message_id: "m1".into(),
            receiver_id: "bob".into(),
            sequence: 1,
            received_at: 1_700_000_500,
        },
    )?;

    // -----------------------------------------------------------------
    // 3. Drive the hub-server side
    // -----------------------------------------------------------------
    let db_path = dir.path().join("hub.db");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let mgr = ImManager::new(db_path)?;
        let alice = mgr.create_user("alice", "Alice").await?;
        let bob = mgr.create_user("bob", "Bob").await?;
        let conv = mgr
            .create_conversation(ChatType::OneOnOne, "alice<->bob")
            .await?;
        mgr.add_group_member(&conv.id, &alice.id, "member").await?;
        mgr.add_group_member(&conv.id, &bob.id, "member").await?;

        for i in 1..=3 {
            mgr.send_message(
                &conv.id,
                &alice.id,
                Some(&bob.id),
                &format!("hub msg #{i}"),
                None,
            )
            .await?;
        }
        let sync = mgr.get_messages_for_sync(&conv.id, None, 10).await?;
        let blob = mgr
            .get_compressed_messages_for_sync(&conv.id, None, 10)
            .await?;
        let decoded = ImManager::decompress_messages(&blob)?;
        println!(
            "[5/5] hub: 3 messages stored, sync returned {}, compressed blob {} bytes, decoded {}",
            sync.messages.len(),
            blob.len(),
            decoded.len()
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    // -----------------------------------------------------------------
    // 4. Print a tiny report
    // -----------------------------------------------------------------
    println!();
    println!("=== per-user ===");
    println!("  alice friends: {}", storage.get_friends("alice")?.len());
    println!(
        "  alice direct msgs in dm:alice:bob: {}",
        storage.get_direct_messages("alice", "dm:alice:bob")?.len()
    );
    println!(
        "  alice group msgs in team: {}",
        storage.get_group_messages("alice", "team")?.len()
    );
    println!(
        "  bob receipts for m1: {}",
        storage.get_message_receipts("bob", "m1")?.len()
    );

    // -----------------------------------------------------------------
    // 5. Demonstrate batch writes + schema metadata
    // -----------------------------------------------------------------
    let batch: Vec<DirectMessage> = (1..=5)
        .map(|i| {
            let mut m = DirectMessage {
                message_id: format!("batch-{i}"),
                chat_id: "dm:alice:bob".into(),
                sender_id: "alice".into(),
                receiver_id: "bob".into(),
                content: format!("batch message {i}"),
                message_type: MessageType::Text,
                attachments: vec![],
                reply_to: None,
                sequence: 10 + i as u32,
                timestamp: 1_700_000_100 + i as u64,
                integrity_hash: None,
                is_edited: false,
                edited_at: None,
            };
            m.stamp_integrity_hash();
            m
        })
        .collect();
    let saved = storage.save_direct_messages("alice", batch.iter().cloned())?;
    println!();
    println!("=== batch + maintenance ===");
    println!("  batched {saved} direct messages in one tx");
    println!(
        "  count_direct_messages: {}",
        storage.count_direct_messages("alice", "dm:alice:bob")?
    );
    println!("  schema_version: {}", storage.schema_version()?);
    storage.check_integrity()?;
    println!("  integrity_check: ok");
    Ok(())
}
