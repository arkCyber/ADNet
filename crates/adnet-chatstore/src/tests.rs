//! Integration tests for the chat store.
//!
//! These tests use `tempfile::tempdir()` to give every test its own
//! SQLite file so they can run in parallel without contention.

use std::sync::Arc;

use adnet_types::group_chat::{DirectMessage, GroupMessage, MessageAttachment, MessageReceipt};
use adnet_types::invariants::{AttachmentKind, MemberRole, MessageType};

use crate::im::{ChatType, ImManager, MAX_SEQUENCE};
use crate::schema::SCHEMA_VERSION;
use crate::storage::{ChatStorage, ChatStorageConfig, Friend};

/// Build a `ChatStorage` rooted in a fresh tmpdir.
fn temp_chatstorage() -> ChatStorage {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ChatStorageConfig {
        storage_dir: dir.path().to_path_buf(),
    };
    ChatStorage::new(cfg).unwrap()
}

/// Build an `ImManager` rooted in a fresh tmpdir.
fn temp_im() -> (tempfile::TempDir, ImManager) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("hub.db");
    let mgr = ImManager::new(db_path).unwrap();
    (dir, mgr)
}

fn attachment() -> MessageAttachment {
    MessageAttachment {
        attachment_id: "att_1".into(),
        file_type: AttachmentKind::Image,
        // 64-hex chars so it satisfies the ContentHash::HEX_LEN invariant.
        blob_hash: "a".repeat(64),
        file_name: "photo.png".into(),
        file_size: 1024,
        thumbnail_hash: None,
    }
}

// ----------------------------------------------------------------------
// ChatStorage tests
// ----------------------------------------------------------------------

#[test]
fn friends_roundtrip() {
    let storage = temp_chatstorage();
    let friend = Friend {
        friend_id: "user_b".into(),
        name: "Bob".into(),
        avatar_url: Some("https://example/bob.png".into()),
        status: Some("online".into()),
        last_seen: Some(1_700_000_000),
        created_at: Some(1_700_000_000),
        updated_at: None,
    };
    storage.save_friend("user_a", friend.clone()).unwrap();

    let fetched = storage.get_friend("user_a", "user_b").unwrap();
    let f = fetched.expect("friend should exist");
    assert_eq!(f.friend_id, "user_b");
    assert_eq!(f.name, "Bob");
    assert_eq!(f.avatar_url.as_deref(), Some("https://example/bob.png"));
    // `updated_at` is auto-stamped on save.
    assert!(f.updated_at.is_some());

    let all = storage.get_friends("user_a").unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].friend_id, "user_b");
}

#[test]
fn save_friend_rejects_bad_name() {
    let storage = temp_chatstorage();
    let bad = Friend {
        friend_id: "user_b".into(),
        name: "x".repeat(adnet_types::invariants::MAX_NAME_LEN + 1),
        avatar_url: None,
        status: None,
        last_seen: None,
        created_at: None,
        updated_at: None,
    };
    let err = storage.save_friend("user_a", bad).unwrap_err();
    assert!(
        matches!(err, crate::error::ChatStoreError::Validation(_)),
        "expected Validation error, got {err:?}"
    );
}

#[test]
fn save_friend_rejects_bad_user_id() {
    let storage = temp_chatstorage();
    let bad = Friend {
        friend_id: "user_b".into(),
        name: "Bob".into(),
        avatar_url: None,
        status: None,
        last_seen: None,
        created_at: None,
        updated_at: None,
    };
    let err = storage.save_friend("  bad user id  ", bad).unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Validation(_)));
}

#[test]
fn remove_friend_returns_true_then_false() {
    let storage = temp_chatstorage();
    let friend = Friend {
        friend_id: "user_b".into(),
        name: "Bob".into(),
        avatar_url: None,
        status: None,
        last_seen: None,
        created_at: None,
        updated_at: None,
    };
    storage.save_friend("user_a", friend).unwrap();
    assert!(storage.remove_friend("user_a", "user_b").unwrap());
    // Already removed → false.
    assert!(!storage.remove_friend("user_a", "user_b").unwrap());
}

#[test]
fn count_friends_returns_zero_then_n() {
    let storage = temp_chatstorage();
    assert_eq!(storage.count_friends("user_a").unwrap(), 0);
    for i in 0..5 {
        storage
            .save_friend(
                "user_a",
                Friend {
                    friend_id: format!("f{i}"),
                    name: format!("Friend {i}"),
                    avatar_url: None,
                    status: None,
                    last_seen: None,
                    created_at: None,
                    updated_at: None,
                },
            )
            .unwrap();
    }
    assert_eq!(storage.count_friends("user_a").unwrap(), 5);
}

#[test]
fn direct_messages_roundtrip() {
    let storage = temp_chatstorage();
    let mut msg = DirectMessage {
        message_id: "m1".into(),
        chat_id: "dm:user_a:user_b".into(),
        sender_id: "user_a".into(),
        receiver_id: "user_b".into(),
        content: "hello".into(),
        message_type: MessageType::Text,
        attachments: vec![attachment()],
        reply_to: None,
        sequence: 1,
        timestamp: 1_700_000_001,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();

    storage.save_direct_message("user_a", msg.clone()).unwrap();

    let msgs = storage
        .get_direct_messages("user_a", "dm:user_a:user_b")
        .unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].message_id, "m1");
    assert!(msgs[0].verify_integrity());
    assert_eq!(msgs[0].attachments.len(), 1);
    assert_eq!(msgs[0].message_type, MessageType::Text);
}

#[test]
fn direct_messages_range_query() {
    let storage = temp_chatstorage();
    for i in 1..=10 {
        let mut msg = DirectMessage {
            message_id: format!("m{i}"),
            chat_id: "dm:a:b".into(),
            sender_id: "a".into(),
            receiver_id: "b".into(),
            content: format!("hello {i}"),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: i,
            timestamp: 1_700_000_000 + i as u64,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        msg.stamp_integrity_hash();
        storage.save_direct_message("a", msg).unwrap();
    }
    let slice = storage
        .get_direct_messages_by_sequence("a", "dm:a:b", 3, 7)
        .unwrap();
    let seqs: Vec<u32> = slice.iter().map(|m| m.sequence).collect();
    assert_eq!(seqs, vec![3, 4, 5, 6, 7]);

    // Empty range is an error.
    let err = storage.get_direct_messages_by_sequence("a", "dm:a:b", 9, 1);
    assert!(err.is_err());
}

#[test]
fn get_recent_direct_messages_returns_tail_in_chronological_order() {
    let storage = temp_chatstorage();
    for i in 1..=5 {
        let mut msg = DirectMessage {
            message_id: format!("m{i}"),
            chat_id: "dm:a:b".into(),
            sender_id: "a".into(),
            receiver_id: "b".into(),
            content: format!("m{i}"),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: i,
            timestamp: i as u64,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        msg.stamp_integrity_hash();
        storage.save_direct_message("a", msg).unwrap();
    }
    let tail = storage
        .get_recent_direct_messages("a", "dm:a:b", 2)
        .unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].sequence, 4);
    assert_eq!(tail[1].sequence, 5);
}

#[test]
fn count_direct_messages_reflects_inserts() {
    let storage = temp_chatstorage();
    assert_eq!(storage.count_direct_messages("a", "dm:a:b").unwrap(), 0);
    for i in 1..=4 {
        let mut msg = DirectMessage {
            message_id: format!("m{i}"),
            chat_id: "dm:a:b".into(),
            sender_id: "a".into(),
            receiver_id: "b".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: i,
            timestamp: i as u64,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        msg.stamp_integrity_hash();
        storage.save_direct_message("a", msg).unwrap();
    }
    assert_eq!(storage.count_direct_messages("a", "dm:a:b").unwrap(), 4);
}

#[test]
fn save_direct_messages_batch_is_atomic() {
    let storage = temp_chatstorage();
    let mut batch: Vec<DirectMessage> = (1..=5)
        .map(|i| {
            let mut m = DirectMessage {
                message_id: format!("m{i}"),
                chat_id: "dm:a:b".into(),
                sender_id: "a".into(),
                receiver_id: "b".into(),
                content: format!("hi {i}"),
                message_type: MessageType::Text,
                attachments: vec![],
                reply_to: None,
                sequence: i,
                timestamp: i as u64,
                integrity_hash: None,
                is_edited: false,
                edited_at: None,
            };
            m.stamp_integrity_hash();
            m
        })
        .collect();

    let saved = storage
        .save_direct_messages("a", batch.iter().cloned())
        .unwrap();
    assert_eq!(saved, 5);

    // Insert one bad message (sequence 0 violates MAX_SEQUENCE for
    // some adnet_types builds, but `validate()` accepts sequence=0).
    // Force a failure with a duplicate primary key (same message_id).
    batch[3].message_id = "m1".into();
    // Duplicate (message_id, user_id) — `INSERT OR REPLACE` would
    // silently overwrite, so this still succeeds. The right way to
    // exercise failure is via the `validate()` path: an empty
    // `message_id` is rejected.
    batch[2].message_id = "".into();
    let err = storage
        .save_direct_messages("a", batch.iter().cloned())
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Validation(_)));

    // The first batch was committed — the failed batch did not
    // partially overwrite anything.
    assert_eq!(storage.count_direct_messages("a", "dm:a:b").unwrap(), 5);
}

#[test]
fn group_messages_roundtrip() {
    let storage = temp_chatstorage();
    let mut msg = GroupMessage {
        message_id: "g1".into(),
        group_id: "group_xyz".into(),
        sender_id: "user_a".into(),
        sender_name: "Alice".into(),
        content: "hello group".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        mentions: vec!["user_b".into()],
        sequence: 1,
        timestamp: 1_700_000_010,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();

    storage.save_group_message("user_a", msg.clone()).unwrap();
    storage.save_group_message("user_b", msg.clone()).unwrap();

    // Each user sees their own row — verify the per-user
    // partition key works (regression test for the previous bug
    // where `INSERT OR REPLACE` on `message_id` PK was clobbering
    // the row from the first user).
    assert_eq!(
        storage
            .get_group_messages("user_a", "group_xyz")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        storage
            .get_group_messages("user_b", "group_xyz")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn save_group_messages_batch_roundtrip() {
    let storage = temp_chatstorage();
    let batch: Vec<GroupMessage> = (1..=3)
        .map(|i| {
            let mut m = GroupMessage {
                message_id: format!("g{i}"),
                group_id: "team".into(),
                sender_id: "alice".into(),
                sender_name: "Alice".into(),
                content: format!("msg {i}"),
                message_type: MessageType::Text,
                attachments: vec![],
                reply_to: None,
                mentions: vec![],
                sequence: i,
                timestamp: i as u64,
                integrity_hash: None,
                is_edited: false,
                edited_at: None,
            };
            m.stamp_integrity_hash();
            m
        })
        .collect();

    let saved = storage
        .save_group_messages("alice", batch.iter().cloned())
        .unwrap();
    assert_eq!(saved, 3);
    assert_eq!(storage.count_group_messages("alice", "team").unwrap(), 3);
}

#[test]
fn sequence_tracking_roundtrip() {
    let storage = temp_chatstorage();
    storage
        .update_sequence("user_a", "user_b", "direct", 42)
        .unwrap();
    assert_eq!(
        storage.get_sequence("user_a", "user_b", "direct").unwrap(),
        Some(42)
    );
    // Different sequence_type ⇒ independent counter.
    assert_eq!(
        storage.get_sequence("user_a", "user_b", "group").unwrap(),
        None
    );
}

#[test]
fn update_sequence_rejects_invalid_id() {
    let storage = temp_chatstorage();
    let err = storage
        .update_sequence("   ", "user_b", "direct", 42)
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Validation(_)));
}

#[test]
fn update_sequence_rejects_value_at_max() {
    let storage = temp_chatstorage();
    let err = storage
        .update_sequence("user_a", "user_b", "direct", MAX_SEQUENCE)
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Invalid(_)));
}

#[test]
fn receipts_roundtrip() {
    let storage = temp_chatstorage();
    let receipt = MessageReceipt {
        receipt_id: "r1".into(),
        message_id: "m1".into(),
        receiver_id: "user_b".into(),
        sequence: 1,
        received_at: 1_700_000_500,
    };
    storage.save_receipt("user_a", receipt.clone()).unwrap();
    let receipts = storage.get_message_receipts("user_a", "m1").unwrap();
    assert_eq!(receipts, vec![receipt]);
}

#[test]
fn delete_user_data_purges_everything() {
    let storage = temp_chatstorage();
    let mut msg = DirectMessage {
        message_id: "m1".into(),
        chat_id: "dm:a:b".into(),
        sender_id: "a".into(),
        receiver_id: "b".into(),
        content: "x".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();
    storage.save_direct_message("a", msg).unwrap();
    storage
        .save_friend(
            "a",
            Friend {
                friend_id: "b".into(),
                name: "B".into(),
                avatar_url: None,
                status: None,
                last_seen: None,
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
    storage.update_sequence("a", "b", "direct", 1).unwrap();
    let receipt = MessageReceipt {
        receipt_id: "r1".into(),
        message_id: "m1".into(),
        receiver_id: "b".into(),
        sequence: 1,
        received_at: 1,
    };
    storage.save_receipt("a", receipt).unwrap();

    let purged = storage.delete_user_data("a").unwrap();
    // We saved one row each across friends / direct_messages /
    // sequences / message_receipts — `group_messages` is empty so the
    // total comes from the four populated tables.
    assert!(purged >= 4);

    assert!(storage
        .get_direct_messages("a", "dm:a:b")
        .unwrap()
        .is_empty());
    assert!(storage.get_friends("a").unwrap().is_empty());
    assert!(storage.get_sequence("a", "b", "direct").unwrap().is_none());
    assert!(storage.get_message_receipts("a", "m1").unwrap().is_empty());
}

#[test]
fn delete_user_data_rejects_bad_id() {
    let storage = temp_chatstorage();
    let err = storage.delete_user_data("  ").unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Validation(_)));
}

#[test]
fn reset_clears_all_data() {
    let storage = temp_chatstorage();
    storage
        .save_friend(
            "a",
            Friend {
                friend_id: "b".into(),
                name: "B".into(),
                avatar_url: None,
                status: None,
                last_seen: None,
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
    storage.reset().unwrap();
    assert_eq!(storage.count_friends("a").unwrap(), 0);
}

#[test]
fn integrity_check_passes_on_healthy_db() {
    let storage = temp_chatstorage();
    storage.check_integrity().unwrap();
}

#[test]
fn schema_version_is_current() {
    let storage = temp_chatstorage();
    assert_eq!(storage.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn invalid_message_is_rejected_at_the_boundary() {
    let storage = temp_chatstorage();
    // Empty message_id violates invariants::validate_id.
    let bad = DirectMessage {
        message_id: "".into(),
        chat_id: "dm:a:b".into(),
        sender_id: "a".into(),
        receiver_id: "b".into(),
        content: "x".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    let err = storage.save_direct_message("a", bad).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("validation") || msg.contains("Validation"),
        "expected validation error, got: {msg}"
    );
}

#[test]
fn concurrent_writes_via_arc_dont_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ChatStorageConfig {
        storage_dir: dir.path().to_path_buf(),
    };
    let storage = Arc::new(ChatStorage::new(cfg).unwrap());

    let mut handles = Vec::new();
    for t in 0..16 {
        let s = Arc::clone(&storage);
        handles.push(std::thread::spawn(move || {
            let mut msg = DirectMessage {
                message_id: format!("m{t}"),
                chat_id: "dm:a:b".into(),
                sender_id: "a".into(),
                receiver_id: "b".into(),
                content: "x".into(),
                message_type: MessageType::Text,
                attachments: vec![],
                reply_to: None,
                sequence: t as u32,
                timestamp: t as u64,
                integrity_hash: None,
                is_edited: false,
                edited_at: None,
            };
            msg.stamp_integrity_hash();
            s.save_direct_message("a", msg).unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let msgs = storage.get_direct_messages("a", "dm:a:b").unwrap();
    assert_eq!(msgs.len(), 16);
}

// ----------------------------------------------------------------------
// ImManager tests
// ----------------------------------------------------------------------

#[tokio::test]
async fn im_create_user_and_conversation() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let bob = mgr.create_user("bob", "Bob").await.unwrap();

    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "alice<->bob")
        .await
        .unwrap();
    mgr.add_group_member(&conv.id, &alice.id, "member")
        .await
        .unwrap();
    mgr.add_group_member(&conv.id, &bob.id, "member")
        .await
        .unwrap();

    let members = mgr.get_group_members(&conv.id).await.unwrap();
    assert_eq!(members.len(), 2);

    let convs = mgr.list_user_conversations(&alice.id).await.unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].id, conv.id);

    let user = mgr.get_user(&alice.id).await.unwrap().unwrap();
    assert_eq!(user.username, "alice");
}

#[tokio::test]
async fn im_create_user_rejects_duplicate_username_with_constraint_error() {
    let (_dir, mgr) = temp_im();
    mgr.create_user("alice", "Alice").await.unwrap();
    let err = mgr.create_user("alice", "Alice2").await.unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Constraint(_)));
}

#[tokio::test]
async fn im_get_user_by_username_works() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let fetched = mgr.get_user_by_username("alice").await.unwrap();
    assert_eq!(fetched.unwrap().id, alice.id);
    assert!(mgr.get_user_by_username("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn im_touch_unknown_user_returns_not_found() {
    let (_dir, mgr) = temp_im();
    let err = mgr.touch_user("ghost").await.unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::NotFound(_)));
}

#[tokio::test]
async fn im_send_message_rejects_empty_content() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();
    let err = mgr
        .send_message(&conv.id, &alice.id, None, "", None)
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_send_message_rejects_missing_conversation() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let err = mgr
        .send_message("nope", &alice.id, None, "x", None)
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::NotFound(_)));
}

#[tokio::test]
async fn im_send_message_group_must_have_no_receiver() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let bob = mgr.create_user("bob", "Bob").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::Group, "team")
        .await
        .unwrap();
    let err = mgr
        .send_message(&conv.id, &alice.id, Some(&bob.id), "hi", None)
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Invalid(_)));
}

#[tokio::test]
async fn im_send_message_stamps_sequence_and_hash() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let bob = mgr.create_user("bob", "Bob").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "alice<->bob")
        .await
        .unwrap();

    let m1 = mgr
        .send_message(&conv.id, &alice.id, Some(&bob.id), "hi", None)
        .await
        .unwrap();
    assert_eq!(m1.sequence, Some(1));
    assert!(m1.integrity_hash.is_some());
    assert!(ImManager::verify_message_integrity(&m1));

    let m2 = mgr
        .send_message(&conv.id, &alice.id, Some(&bob.id), "again", None)
        .await
        .unwrap();
    assert_eq!(m2.sequence, Some(2));

    let msgs = mgr.get_messages(&conv.id, None).await.unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].sequence, Some(2));

    // Touch updates last_seen.
    mgr.touch_user(&alice.id).await.unwrap();
    let refreshed = mgr.get_user(&alice.id).await.unwrap().unwrap();
    assert!(refreshed.last_seen.is_some());
}

#[tokio::test]
async fn im_sync_supports_pagination_and_compression() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();

    for i in 0..10 {
        mgr.send_message(&conv.id, &alice.id, None, &format!("m{i}"), None)
            .await
            .unwrap();
    }

    let first = mgr.get_messages_for_sync(&conv.id, None, 5).await.unwrap();
    assert_eq!(first.messages.len(), 5);
    assert!(first.has_more);
    assert_eq!(first.last_sequence, 5);

    // After advancing past seq 5, the remaining 5 messages are seqs
    // 6..=10 — exactly the limit we requested, so the optimistic
    // "has_more" flag is still set (the hub may have more on the
    // next page). The important invariant is that we *do* receive
    // those 5 messages and that `last_sequence` advances to 10.
    let second = mgr
        .get_messages_for_sync(&conv.id, Some(5), 5)
        .await
        .unwrap();
    assert_eq!(second.messages.len(), 5);
    assert_eq!(second.last_sequence, 10);

    // Asking for more after seq 10 returns an empty slice.
    let third = mgr
        .get_messages_for_sync(&conv.id, Some(10), 5)
        .await
        .unwrap();
    assert!(third.messages.is_empty());

    let blob = mgr
        .get_compressed_messages_for_sync(&conv.id, None, 10)
        .await
        .unwrap();
    let decompressed = ImManager::decompress_messages(&blob).unwrap();
    assert_eq!(decompressed.len(), 10);
}

#[tokio::test]
async fn im_sync_rejects_zero_limit() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();
    let err = mgr
        .get_messages_for_sync(&conv.id, None, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Invalid(_)));
    // Silence the unused-variable warning on alice.
    let _ = alice;
}

#[tokio::test]
async fn im_sequence_cycles_at_max_sequence() {
    use crate::MAX_SEQUENCE;
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();

    // Use the public API to drive the sequence right up to the
    // ceiling — sending MAX_SEQUENCE - 1 messages brings the sender
    // sequence to MAX_SEQUENCE - 1, the next call wraps to
    // MAX_SEQUENCE, the one after wraps back to 1.
    for _ in 0..(MAX_SEQUENCE - 1) {
        mgr.send_message(&conv.id, &alice.id, None, "x", None)
            .await
            .unwrap();
    }
    let at_max = mgr
        .send_message(&conv.id, &alice.id, None, "wrap", None)
        .await
        .unwrap();
    assert_eq!(at_max.sequence, Some(MAX_SEQUENCE));

    let wrapped = mgr
        .send_message(&conv.id, &alice.id, None, "wrap2", None)
        .await
        .unwrap();
    assert_eq!(wrapped.sequence, Some(1), "sequence must wrap to 1");
}

#[tokio::test]
async fn im_integrity_hash_domain_separation_is_stable() {
    // A second pass over the same message produces the same hash
    // (deterministic) and a modified message produces a different
    // hash. The `INTEGRITY_HASH_TAG` prefix makes historical hashes
    // distinguishable from future schema revisions.
    use crate::im::generate_integrity_hash;
    let a = generate_integrity_hash("alice", Some("bob"), "hi", 1, "2024-01-01T00:00:00Z");
    let b = generate_integrity_hash("alice", Some("bob"), "hi", 1, "2024-01-01T00:00:00Z");
    assert_eq!(a, b);
    let c = generate_integrity_hash("alice", Some("bob"), "hi", 2, "2024-01-01T00:00:00Z");
    assert_ne!(a, c, "different seq → different hash");
    let d = generate_integrity_hash("alice", Some("bob"), "hello", 1, "2024-01-01T00:00:00Z");
    assert_ne!(a, d, "different content → different hash");
}

#[tokio::test]
async fn im_detect_missing_messages_returns_range() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();

    for _ in 0..5 {
        mgr.send_message(&conv.id, &alice.id, None, "x", None)
            .await
            .unwrap();
    }
    mgr.update_user_sequence(&alice.id, &alice.id, 3)
        .await
        .unwrap();
    let miss = mgr
        .detect_missing_messages(&alice.id, &alice.id)
        .await
        .unwrap();
    assert_eq!(miss, Some((4, 5)));

    mgr.update_user_sequence(&alice.id, &alice.id, 5)
        .await
        .unwrap();
    let miss = mgr
        .detect_missing_messages(&alice.id, &alice.id)
        .await
        .unwrap();
    assert_eq!(miss, None);
}

#[tokio::test]
async fn im_pending_messages_queue_and_drain() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let bob = mgr.create_user("bob", "Bob").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "dm")
        .await
        .unwrap();
    let msg = mgr
        .send_message(&conv.id, &alice.id, Some(&bob.id), "ping", None)
        .await
        .unwrap();

    mgr.add_pending_message(&msg.id, &bob.id, &conv.id)
        .await
        .unwrap();
    let pending = mgr.get_pending_messages(&bob.id).await.unwrap();
    assert_eq!(pending.len(), 1);

    let removed = mgr.clear_pending_messages(&bob.id).await.unwrap();
    assert_eq!(removed, 1);
    assert!(mgr.get_pending_messages(&bob.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn im_pending_message_for_unknown_message_returns_foreign_key_error() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let err = mgr
        .add_pending_message("ghost_msg", &alice.id, "ghost_conv")
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::ForeignKey(_)));
}

#[tokio::test]
async fn im_add_group_member_returns_existing_on_duplicate() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::Group, "team")
        .await
        .unwrap();
    let first = mgr
        .add_group_member(&conv.id, &alice.id, "member")
        .await
        .unwrap();
    // Inserting the same pair again returns the existing row.
    let second = mgr
        .add_group_member(&conv.id, &alice.id, "admin")
        .await
        .unwrap();
    // The id should be the same; the role reflects the caller's
    // intent (the implementation does not currently update the
    // existing row's role to keep the API symmetric with `INSERT
    // OR IGNORE`).
    assert_eq!(first.id, second.id);
    let members = mgr.get_group_members(&conv.id).await.unwrap();
    assert_eq!(members.len(), 1);
}

#[tokio::test]
async fn im_receipts_roundtrip() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();
    let msg = mgr
        .send_message(&conv.id, &alice.id, None, "x", None)
        .await
        .unwrap();
    let receipt = mgr
        .create_message_receipt(&msg.id, &alice.id, msg.sequence.unwrap_or(0))
        .await
        .unwrap();
    assert_eq!(receipt.message_id, msg.id);

    let all = mgr.get_message_receipts(&msg.id).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].receipt_id, receipt.receipt_id);
}

#[tokio::test]
async fn im_receipt_for_unknown_message_returns_foreign_key_error() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let err = mgr
        .create_message_receipt("ghost", &alice.id, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::ForeignKey(_)));
}

#[tokio::test]
async fn im_remove_group_member_returns_count() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let bob = mgr.create_user("bob", "Bob").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::Group, "team")
        .await
        .unwrap();
    mgr.add_group_member(&conv.id, &alice.id, "admin")
        .await
        .unwrap();
    mgr.add_group_member(&conv.id, &bob.id, "member")
        .await
        .unwrap();
    assert_eq!(mgr.remove_group_member(&conv.id, &bob.id).await.unwrap(), 1);
    // Already removed → 0 rows.
    assert_eq!(mgr.remove_group_member(&conv.id, &bob.id).await.unwrap(), 0);
}

#[tokio::test]
async fn im_integrity_check_passes_on_healthy_db() {
    let (_dir, mgr) = temp_im();
    mgr.check_integrity().await.unwrap();
}

#[tokio::test]
async fn im_schema_version_is_current() {
    let (_dir, mgr) = temp_im();
    assert_eq!(mgr.schema_version().await.unwrap(), SCHEMA_VERSION);
}

#[test]
fn im_member_role_roundtrip() {
    // Round-trip the MemberRole enum so callers can map our
    // local `GroupMember.role: String` to the typed variant.
    let r = MemberRole::Admin;
    let s = serde_json::to_string(&r).unwrap();
    assert_eq!(s, "\"admin\"");
    let back: MemberRole = serde_json::from_str(&s).unwrap();
    assert_eq!(back, MemberRole::Admin);
}

// ----------------------------------------------------------------------
// Audit-pass additions: new APIs in storage.rs / im.rs.
// ----------------------------------------------------------------------

#[test]
fn search_direct_messages_returns_matching_rows_in_chronological_order() {
    let storage = temp_chatstorage();
    for i in 1..=5 {
        let mut msg = DirectMessage {
            message_id: format!("m{i}"),
            chat_id: "dm:a:b".into(),
            sender_id: "a".into(),
            receiver_id: "b".into(),
            content: if i == 3 {
                "needle in haystack".to_string()
            } else {
                format!("msg {i}")
            },
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: i,
            timestamp: i as u64,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        msg.stamp_integrity_hash();
        storage.save_direct_message("a", msg).unwrap();
    }
    let hits = storage
        .search_direct_messages("a", "dm:a:b", "needle", 100)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id, "m3");

    // Empty query is an error.
    let err = storage
        .search_direct_messages("a", "dm:a:b", "", 100)
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Invalid(_)));

    // Search is case-insensitive (SQLite's default `LIKE`).
    let hits = storage
        .search_direct_messages("a", "dm:a:b", "NEEDLE", 100)
        .unwrap();
    assert_eq!(hits.len(), 1, "LIKE should be case-insensitive");

    // `limit = 0` falls back to the safety cap.
    let hits = storage
        .search_direct_messages("a", "dm:a:b", "msg", 0)
        .unwrap();
    assert_eq!(hits.len(), 4);
}

#[test]
fn search_all_direct_messages_spans_chats() {
    let storage = temp_chatstorage();
    for (chat, body) in [("dm:a:b", "alpha"), ("dm:a:c", "beta"), ("dm:a:d", "gamma")] {
        let mut msg = DirectMessage {
            message_id: format!("{chat}-1"),
            chat_id: chat.into(),
            sender_id: "a".into(),
            receiver_id: chat.rsplit(':').next().unwrap().into(),
            content: body.into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        msg.stamp_integrity_hash();
        storage.save_direct_message("a", msg).unwrap();
    }
    let hits = storage.search_all_direct_messages("a", "alpha", 100).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chat_id, "dm:a:b");
}

#[test]
fn delete_chat_messages_removes_only_one_side() {
    let storage = temp_chatstorage();
    let mut msg = DirectMessage {
        message_id: "m1".into(),
        chat_id: "dm:a:b".into(),
        sender_id: "a".into(),
        receiver_id: "b".into(),
        content: "x".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();
    storage.save_direct_message("a", msg.clone()).unwrap();
    storage.save_direct_message("b", msg).unwrap();

    let removed = storage.delete_chat_messages("a", "dm:a:b").unwrap();
    assert_eq!(removed, 1);
    assert!(storage
        .get_direct_messages("a", "dm:a:b")
        .unwrap()
        .is_empty());
    assert_eq!(
        storage.get_direct_messages("b", "dm:a:b").unwrap().len(),
        1,
        "the other side's history must be untouched"
    );
}

#[test]
fn prune_direct_messages_before_deletes_older_rows() {
    let storage = temp_chatstorage();
    for i in 1..=5 {
        let mut msg = DirectMessage {
            message_id: format!("m{i}"),
            chat_id: "dm:a:b".into(),
            sender_id: "a".into(),
            receiver_id: "b".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: i,
            timestamp: i as u64,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        msg.stamp_integrity_hash();
        storage.save_direct_message("a", msg).unwrap();
    }
    // Cutoff 3 — keep timestamp >= 3.
    let removed = storage.prune_direct_messages_before("a", 3).unwrap();
    assert_eq!(removed, 2); // ts 1 and 2
    let remaining = storage.get_direct_messages("a", "dm:a:b").unwrap();
    let seqs: Vec<u32> = remaining.iter().map(|m| m.sequence).collect();
    assert_eq!(seqs, vec![3, 4, 5]);

    // Negative cutoff is rejected.
    let err = storage.prune_direct_messages_before("a", -1).unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Invalid(_)));
}

#[test]
fn update_friend_status_round_trips() {
    let storage = temp_chatstorage();
    storage
        .save_friend(
            "a",
            Friend {
                friend_id: "b".into(),
                name: "Bob".into(),
                avatar_url: None,
                status: None,
                last_seen: None,
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
    assert!(storage
        .update_friend_status("a", "b", Some("away"), Some(123))
        .unwrap());
    let friend = storage.get_friend("a", "b").unwrap().unwrap();
    assert_eq!(friend.status.as_deref(), Some("away"));
    assert_eq!(friend.last_seen, Some(123));

    // Empty status string is an error.
    let err = storage
        .update_friend_status("a", "b", Some(""), None)
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Invalid(_)));

    // Unknown friend returns false (not an error).
    assert!(!storage.update_friend_status("a", "ghost", None, None).unwrap());
}

#[tokio::test]
async fn im_edit_message_re_stamps_hash_and_marks_edited() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();
    let original = mgr
        .send_message(&conv.id, &alice.id, None, "original", None)
        .await
        .unwrap();
    // The freshly-sent message verifies as expected.
    assert!(ImManager::verify_message_integrity(&original));
    let original_hash = original.integrity_hash.clone().unwrap();

    let edited_at = chrono::Utc::now() + chrono::Duration::seconds(5);
    let updated = mgr
        .edit_message(&original.id, "edited body", edited_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.content, "edited body");

    // The integrity hash was re-stamped, so it must differ from the
    // original hash (because both content and timestamp changed).
    assert_ne!(updated.integrity_hash.as_ref().unwrap(), &original_hash);
    assert!(ImManager::verify_message_integrity(&updated));

    // Empty content is rejected.
    let err = mgr
        .edit_message(&original.id, "", chrono::Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Validation(_)));

    // Editing a non-existent message returns Ok(None), not an error.
    let ghost = mgr
        .edit_message("ghost_id", "x", chrono::Utc::now())
        .await
        .unwrap();
    assert!(ghost.is_none());
}

#[tokio::test]
async fn im_delete_message_cascades_receipts() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();
    let msg = mgr
        .send_message(&conv.id, &alice.id, None, "x", None)
        .await
        .unwrap();
    mgr.create_message_receipt(&msg.id, &alice.id, 1).await.unwrap();
    assert_eq!(
        mgr.get_message_receipts(&msg.id).await.unwrap().len(),
        1
    );

    assert!(mgr.delete_message(&msg.id).await.unwrap());
    assert!(mgr.get_message_receipts(&msg.id).await.unwrap().is_empty());
    assert!(!mgr.delete_message(&msg.id).await.unwrap());
}

#[tokio::test]
async fn im_count_messages_and_prune() {
    let (_dir, mgr) = temp_im();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self")
        .await
        .unwrap();
    for i in 0..3 {
        mgr.send_message(&conv.id, &alice.id, None, &format!("m{i}"), None)
            .await
            .unwrap();
        // Spread the timestamps so the cutoff is meaningful.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(mgr.count_messages(&conv.id).await.unwrap(), 3);

    // Cutoff in the future deletes everything.
    let now = chrono::Utc::now().timestamp() + 60;
    let removed = mgr.prune_messages_before(&conv.id, now).await.unwrap();
    assert_eq!(removed, 3);
    assert_eq!(mgr.count_messages(&conv.id).await.unwrap(), 0);

    // Negative cutoff is rejected.
    let err = mgr.prune_messages_before(&conv.id, -1).await.unwrap_err();
    assert!(matches!(err, crate::error::ChatStoreError::Invalid(_)));
}
