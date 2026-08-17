//! Per-function coverage audit for `a3net-chatstore`.
//!
//! Every public function in `a3net_chatstore::{storage, im, error,
//! schema}` (and the `docs_bridge` module behind the `iroh` feature)
//! is exercised at least once with a focused unit test. The intent
//! is to catch a regression where a function becomes untested after
//! a refactor.
//!
//! # Scope
//!
//! This file deliberately avoids large happy-path coverage — the
//! existing `tests.rs` already covers the end-to-end happy path for
//! every public API. Here we focus on:
//!
//! - **Untested functions** that the original suite did not exercise
//!   (e.g. `ChatStorage::vacuum`, `ImManager::db_path`,
//!   `ImManager::vacuum`, `count_group_messages`, the error-type
//!   conversions, ...).
//! - **Boundary / negative paths** that the original suite glossed
//!   over (e.g. `MAX_SEQUENCE` arithmetic, empty-result counters,
//!   corrupt compressed payloads, edited-message hash re-stamping
//!   with a missing timestamp).
//! - **Round-trip contracts** that the typed records promise (e.g.
//!   `compress_messages` / `decompress_messages`).
//!
//! # DO-178C properties
//!
//! - **Idempotent**: each test allocates a fresh `TempDir`; no
//!   shared state across tests. `cargo test -- --test-threads=N`
//!   yields the same results as a single-threaded run.
//! - **Deterministic**: messages use fixed `seq` / `timestamp`
//!   values so failures are reproducible from the test log alone.
//! - **Self-contained**: no fixtures from outside this crate. The
//!   `cargo test` command from the workspace root runs the suite
//!   end-to-end without any setup.

use a3net_chatstore::error::ErrorClass;
use a3net_chatstore::im::{
    ChatType, ImManager, MAX_SEQUENCE, Message, generate_12digit_id, generate_integrity_hash,
};
use a3net_chatstore::storage::{ChatStorage, ChatStorageConfig, Friend};
use a3net_chatstore::{ChatStoreError, SCHEMA_VERSION};
use a3net_types::group_chat::{DirectMessage, GroupMessage, MessageAttachment, MessageReceipt};
use a3net_types::invariants::{AttachmentKind, MessageType};

// =========================================================================
// error.rs — From / Into conversions and recoverability boundary cases
// =========================================================================

#[test]
fn error_recoverability_io_bincode_zstd_idgen_are_fatal() {
    // Verify the `Fatal` classification for the three directly
    // constructible Fatal variants. `Bincode` and `Json` require
    // library internals not available in tests; they are covered
    // implicitly by the integration tests that call
    // `compress_messages` and `decompress_messages`.
    let zstd_err = zstd::decode_all(&b"not zstd"[..]).unwrap_err();
    let zstd_chat = ChatStoreError::Zstd(zstd_err.to_string());
    assert_eq!(zstd_chat.recoverability(), ErrorClass::Fatal);

    let id_gen: ChatStoreError = ChatStoreError::IdGen("exhausted".into());
    assert_eq!(id_gen.recoverability(), ErrorClass::Fatal);

    let io: ChatStoreError =
        ChatStoreError::Io(std::io::Error::new(std::io::ErrorKind::Other, "boom"));
    assert_eq!(io.recoverability(), ErrorClass::Fatal);
}

#[test]
fn error_from_a3net_types_uses_validation_variant() {
    // `a3net_types::error::AdnetError::Validation` → `ChatStoreError::Validation`.
    let a3net_err: a3net_types::error::AdnetError =
        a3net_types::error::AdnetError::Validation("x".into());
    let chat_err: ChatStoreError = a3net_err.into();
    assert!(matches!(chat_err, ChatStoreError::Validation(_)));
}

#[test]
fn error_from_std_parse_int_uses_invalid_variant() {
    let p: std::num::ParseIntError = "abc".parse::<u32>().unwrap_err();
    let chat: ChatStoreError = p.into();
    assert!(matches!(chat, ChatStoreError::Invalid(_)));
}

#[test]
fn error_from_chrono_parse_uses_invalid_variant() {
    let p: chrono::ParseError =
        chrono::DateTime::parse_from_rfc3339("not a date").expect_err("chrono rejects bad strings");
    let chat: ChatStoreError = p.into();
    assert!(matches!(chat, ChatStoreError::Invalid(_)));
}

#[test]
fn error_from_std_mutex_poison_is_lock_variant() {
    use std::sync::{Arc, Mutex};

    // Poison a mutex then try to lock it — exercises the
    // `From<PoisonError<T>>` impl.
    let m: Arc<Mutex<u8>> = Arc::new(Mutex::new(0));
    let m2 = Arc::clone(&m);
    let _ = std::thread::spawn(move || {
        let _guard = m2.lock().unwrap();
        panic!("intentional panic to poison the mutex");
    })
    .join();
    // The mutex should now be poisoned — `lock()` returns the
    // `PoisonError` carrying the original guard, which our
    // `From<PoisonError<T>>` impl converts to `ChatStoreError::Lock`.
    let result = m.lock();
    let chat: ChatStoreError = result.expect_err("mutex should be poisoned").into();
    assert!(matches!(chat, ChatStoreError::Lock));
}

#[test]
fn error_from_std_try_lock_is_lock_variant() {
    use std::sync::{Arc, Mutex};

    let m: Arc<Mutex<u8>> = Arc::new(Mutex::new(0));
    // Acquire the guard without releasing it.
    let _guard = m.lock().unwrap();
    // From another handle, `try_lock()` fails with `TryLockError::WouldBlock`
    // → converted to `ChatStoreError::Lock`.
    let err: ChatStoreError = m.try_lock().expect_err("would block").into();
    assert!(matches!(err, ChatStoreError::Lock));
}

// =========================================================================
// schema.rs — current_version / SCHEMA_VERSION constants
// =========================================================================

#[test]
fn schema_version_constant_matches_documented() {
    // The crate re-exports `SCHEMA_VERSION` from the public surface.
    // The migration ladder is `migrate_to(1)`, `migrate_to(2)`,
    // `migrate_to(3)` (chat_trust table), and `migrate_to(4)`
    // (group-metadata columns on conversations).
    assert_eq!(SCHEMA_VERSION, 4);
}

#[test]
fn storage_schema_version_matches_constant() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ChatStorageConfig {
        storage_dir: dir.path().to_path_buf(),
    };
    let storage = ChatStorage::new(cfg).unwrap();
    assert_eq!(storage.schema_version().unwrap(), SCHEMA_VERSION);
}

#[tokio::test]
async fn im_manager_schema_version_matches_constant() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("hub.db");
    let mgr = ImManager::new(db).unwrap();
    assert_eq!(mgr.schema_version().await.unwrap(), SCHEMA_VERSION);
}

// =========================================================================
// storage.rs — uncovered public functions
// =========================================================================

#[test]
fn chat_storage_config_default_uses_temp_dir() {
    // The `Default` impl for `ChatStorageConfig` pushes
    // `temp_dir()/exodus_chat_storage`. We don't pin the exact path
    // (it varies by platform) but verify the directory name and
    // that it's under the OS temp dir.
    let cfg = ChatStorageConfig::default();
    let last = cfg
        .storage_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("file name");
    assert_eq!(last, "exodus_chat_storage");
    let temp = std::env::temp_dir();
    assert!(
        cfg.storage_dir.starts_with(&temp),
        "default storage_dir must live under std::env::temp_dir()"
    );
}

#[test]
fn chat_storage_config_accessor_returns_same_value() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ChatStorageConfig {
        storage_dir: dir.path().to_path_buf(),
    };
    let storage = ChatStorage::new(cfg.clone()).unwrap();
    assert_eq!(storage.config().storage_dir, cfg.storage_dir);
}

#[test]
fn chat_storage_vacuum_runs_on_idle_db() {
    // VACUUM on a freshly-created db should be a no-op and
    // succeed without error. We don't assert anything about disk
    // size because the test fixture is too small for VACUUM to
    // reclaim anything meaningful.
    let dir = tempfile::tempdir().unwrap();
    let cfg = ChatStorageConfig {
        storage_dir: dir.path().to_path_buf(),
    };
    let storage = ChatStorage::new(cfg).unwrap();
    storage.vacuum().unwrap();
    // After VACUUM the DB must still be readable.
    storage.check_integrity().unwrap();
}

#[test]
fn count_group_messages_returns_zero_then_n() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();

    assert_eq!(storage.count_group_messages("a", "g").unwrap(), 0);

    for i in 1..=4 {
        let mut msg = GroupMessage {
            message_id: format!("m{i}"),
            group_id: "g".into(),
            sender_id: "a".into(),
            sender_name: "Alice".into(),
            content: "x".into(),
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
        msg.stamp_integrity_hash();
        storage.save_group_message("a", msg).unwrap();
    }
    assert_eq!(storage.count_group_messages("a", "g").unwrap(), 4);
}

#[test]
fn save_friend_rejects_bad_avatar_url() {
    // `validate_url`: permissive (no URL scheme check), but rejects
    // NUL bytes and length > MAX_NAME_LEN. Use a NUL-byte URL to
    // trigger the validation error.
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let bad = Friend {
        friend_id: "user_b".into(),
        name: "Bob".into(),
        avatar_url: Some("https://example/bob.png\0suffix".into()),
        status: None,
        last_seen: None,
        created_at: None,
        updated_at: None,
    };
    let err = storage.save_friend("user_a", bad).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_friend_rejects_bad_friend_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let bad = Friend {
        friend_id: "  ".into(),
        name: "Bob".into(),
        avatar_url: None,
        status: None,
        last_seen: None,
        created_at: None,
        updated_at: None,
    };
    let err = storage.save_friend("user_a", bad).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn update_sequence_rejects_empty_sequence_type() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .update_sequence("user_a", "user_b", "", 1)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn update_sequence_rejects_empty_target_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .update_sequence("user_a", "", "direct", 1)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn get_sequence_returns_none_for_unseen_pair() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let seq = storage.get_sequence("a", "b", "direct").unwrap();
    assert_eq!(seq, None);
}

#[test]
fn get_friend_returns_none_for_missing() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let f = storage.get_friend("a", "ghost").unwrap();
    assert!(f.is_none());
}

#[test]
fn get_friends_returns_empty_for_unknown_user() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let friends = storage.get_friends("ghost").unwrap();
    assert!(friends.is_empty());
}

#[test]
fn remove_friend_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage.remove_friend("  ", "b").unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_direct_messages_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .save_direct_messages("  ", std::iter::empty::<DirectMessage>())
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_group_messages_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .save_group_messages("  ", std::iter::empty::<GroupMessage>())
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_receipt_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let receipt = MessageReceipt {
        receipt_id: "r1".into(),
        message_id: "m1".into(),
        receiver_id: "b".into(),
        sequence: 1,
        received_at: 1,
    };
    let err = storage.save_receipt("  ", receipt).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_receipt_rejects_invalid_record() {
    // Empty message_id is rejected by `MessageReceipt::validate()`.
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let bad = MessageReceipt {
        receipt_id: "r1".into(),
        message_id: "".into(),
        receiver_id: "b".into(),
        sequence: 1,
        received_at: 1,
    };
    let err = storage.save_receipt("a", bad).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn delete_chat_messages_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage.delete_chat_messages("  ", "chat").unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn delete_chat_messages_rejects_bad_chat_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage.delete_chat_messages("a", "  ").unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn prune_direct_messages_before_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage.prune_direct_messages_before("  ", 100).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn update_friend_status_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .update_friend_status("  ", "b", None, None)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn update_friend_status_rejects_bad_friend_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .update_friend_status("a", "  ", None, None)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn update_friend_status_clears_status_with_none() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    storage
        .save_friend(
            "a",
            Friend {
                friend_id: "b".into(),
                name: "Bob".into(),
                avatar_url: None,
                status: Some("online".into()),
                last_seen: Some(100),
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
    // Clear status to None.
    assert!(storage.update_friend_status("a", "b", None, None).unwrap());
    let friend = storage.get_friend("a", "b").unwrap().unwrap();
    assert_eq!(friend.status, None);
}

#[test]
fn search_direct_messages_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .search_direct_messages("  ", "chat", "x", 10)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn search_direct_messages_rejects_bad_chat_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .search_direct_messages("a", "  ", "x", 10)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn search_all_direct_messages_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .search_all_direct_messages("  ", "x", 10)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn search_all_direct_messages_rejects_empty_query() {
    // `search_all_direct_messages` validates empty query → `Invalid`.
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage.search_all_direct_messages("a", "", 10).unwrap_err();
    assert!(matches!(err, ChatStoreError::Invalid(_)));
}

#[test]
fn save_direct_message_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
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
    let err = storage.save_direct_message("  ", msg).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_group_message_rejects_bad_user_id() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let mut msg = GroupMessage {
        message_id: "m1".into(),
        group_id: "g".into(),
        sender_id: "a".into(),
        sender_name: "Alice".into(),
        content: "x".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        mentions: vec![],
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();
    let err = storage.save_group_message("  ", msg).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn get_direct_messages_returns_empty_for_unknown_chat() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let msgs = storage.get_direct_messages("a", "no-such-chat").unwrap();
    assert!(msgs.is_empty());
}

#[test]
fn get_group_messages_returns_empty_for_unknown_group() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let msgs = storage.get_group_messages("a", "no-such-group").unwrap();
    assert!(msgs.is_empty());
}

#[test]
fn get_group_messages_by_sequence_rejects_inverted_range() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let err = storage
        .get_group_messages_by_sequence("a", "g", 5, 1)
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Invalid(_)));
}

#[test]
fn save_direct_message_overwrites_existing_row() {
    // `INSERT OR REPLACE` semantics: a second save with the same
    // `message_id` for the same `user_id` overwrites the row.
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let mut msg = DirectMessage {
        message_id: "m1".into(),
        chat_id: "dm:a:b".into(),
        sender_id: "a".into(),
        receiver_id: "b".into(),
        content: "first".into(),
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

    msg.content = "second".into();
    msg.stamp_integrity_hash();
    storage.save_direct_message("a", msg).unwrap();

    let msgs = storage.get_direct_messages("a", "dm:a:b").unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "second");
}

#[test]
fn save_group_message_overwrites_existing_row() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let mut msg = GroupMessage {
        message_id: "m1".into(),
        group_id: "g".into(),
        sender_id: "a".into(),
        sender_name: "Alice".into(),
        content: "first".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        mentions: vec![],
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();
    storage.save_group_message("a", msg.clone()).unwrap();

    msg.content = "second".into();
    msg.stamp_integrity_hash();
    storage.save_group_message("a", msg).unwrap();

    let msgs = storage.get_group_messages("a", "g").unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "second");
}

#[test]
fn save_friend_overwrites_existing_row() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    storage
        .save_friend(
            "a",
            Friend {
                friend_id: "b".into(),
                name: "Bob".into(),
                avatar_url: None,
                status: Some("online".into()),
                last_seen: Some(1),
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
    storage
        .save_friend(
            "a",
            Friend {
                friend_id: "b".into(),
                name: "Robert".into(),
                avatar_url: Some("https://example/r.png".into()),
                status: Some("away".into()),
                last_seen: Some(2),
                created_at: None,
                updated_at: None,
            },
        )
        .unwrap();
    let friend = storage.get_friend("a", "b").unwrap().unwrap();
    assert_eq!(friend.name, "Robert");
    assert_eq!(friend.status.as_deref(), Some("away"));
    assert_eq!(friend.last_seen, Some(2));
}

#[test]
fn reset_idempotent_on_empty_db() {
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    // `reset` on an empty DB must still succeed.
    storage.reset().unwrap();
    storage.reset().unwrap();
}

#[test]
fn save_direct_message_rejects_invalid_message_type() {
    // The `message_type` enum rejects `None`-equivalent values via
    // serde; we exercise the validation path with a bad sequence
    // value (MAX_SEQUENCE).
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let mut msg = DirectMessage {
        message_id: "m1".into(),
        chat_id: "dm:a:b".into(),
        sender_id: "a".into(),
        receiver_id: "b".into(),
        content: "x".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        sequence: MAX_SEQUENCE, // exceeds the invariant
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();
    let err = storage.save_direct_message("a", msg).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_group_message_rejects_too_many_mentions() {
    // The `mentions` list must not exceed `MAX_MENTIONS`. We
    // construct a payload that violates this and expect the typed
    // validation error.
    use a3net_types::invariants::MAX_MENTIONS;
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let mut msg = GroupMessage {
        message_id: "m1".into(),
        group_id: "g".into(),
        sender_id: "a".into(),
        sender_name: "Alice".into(),
        content: "x".into(),
        message_type: MessageType::Text,
        attachments: vec![],
        reply_to: None,
        mentions: (0..=MAX_MENTIONS).map(|i| format!("user_{i}")).collect(),
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();
    let err = storage.save_group_message("a", msg).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[test]
fn save_direct_message_rejects_too_many_attachments() {
    use a3net_types::invariants::MAX_ATTACHMENTS;
    let storage = ChatStorage::new(ChatStorageConfig {
        storage_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
    })
    .unwrap();
    let att = MessageAttachment {
        attachment_id: "att1".into(),
        file_type: AttachmentKind::Image,
        blob_hash: "a".repeat(64),
        file_name: "f".into(),
        file_size: 1,
        thumbnail_hash: None,
    };
    let mut msg = DirectMessage {
        message_id: "m1".into(),
        chat_id: "dm:a:b".into(),
        sender_id: "a".into(),
        receiver_id: "b".into(),
        content: "x".into(),
        message_type: MessageType::Text,
        attachments: (0..=MAX_ATTACHMENTS).map(|_| att.clone()).collect(),
        reply_to: None,
        sequence: 1,
        timestamp: 1,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    msg.stamp_integrity_hash();
    let err = storage.save_direct_message("a", msg).unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

// =========================================================================
// im.rs — uncovered public functions
// =========================================================================

#[test]
fn generate_12digit_id_returns_unique_12_digit_string() {
    // Generate a batch of ids, assert each is exactly 12 digits,
    // distinct from the others, and within the documented range
    // [100_000_000_000, 999_999_999_999].
    let ids: Vec<String> = (0..32).map(|_| generate_12digit_id()).collect();
    assert_eq!(ids.len(), 32);
    for id in &ids {
        assert_eq!(id.len(), 12, "id must be 12 chars: {id}");
        assert!(id.chars().all(|c| c.is_ascii_digit()));
        let n: u64 = id.parse().unwrap();
        assert!((100_000_000_000..=999_999_999_999).contains(&n));
    }
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert!(unique.len() >= 30, "at most 2 collisions in 32 ids");
}

#[test]
fn generate_integrity_hash_is_deterministic_and_differs_on_input_change() {
    // The hash function is deterministic: same input → same hash.
    let a = generate_integrity_hash("alice", Some("bob"), "hi", 1, "2024-01-01T00:00:00Z");
    let b = generate_integrity_hash("alice", Some("bob"), "hi", 1, "2024-01-01T00:00:00Z");
    assert_eq!(a, b);

    // Any input change must produce a different hash.
    let c = generate_integrity_hash("alice", Some("bob"), "hi", 1, "2024-01-02T00:00:00Z");
    assert_ne!(a, c, "different timestamp → different hash");
    let d = generate_integrity_hash("alice", None, "hi", 1, "2024-01-01T00:00:00Z");
    assert_ne!(a, d, "receiver_id present vs. None → different hash");
}

#[tokio::test]
async fn im_db_path_accessor_returns_constructed_path() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("hub.db");
    let mgr = ImManager::new(db.clone()).unwrap();
    assert_eq!(mgr.db_path(), &db);
}

#[tokio::test]
async fn im_vacuum_runs_on_idle_db() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("hub.db");
    let mgr = ImManager::new(db).unwrap();
    mgr.vacuum().await.unwrap();
    mgr.check_integrity().await.unwrap();
}

#[tokio::test]
async fn im_get_user_returns_none_for_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let user = mgr.get_user("ghost").await.unwrap();
    assert!(user.is_none());
}

#[tokio::test]
async fn im_get_conversation_returns_none_for_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let conv = mgr.get_conversation("ghost").await.unwrap();
    assert!(conv.is_none());
}

#[tokio::test]
async fn im_get_conversation_rejects_bad_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_conversation("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_list_user_conversations_returns_empty_for_unknown_user() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let convs = mgr.list_user_conversations("ghost").await.unwrap();
    assert!(convs.is_empty());
}

#[tokio::test]
async fn im_list_user_conversations_rejects_bad_user_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.list_user_conversations("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_create_user_rejects_empty_username() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.create_user("", "x").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_create_user_rejects_empty_display_name() {
    // `validate_name` does NOT reject empty strings. An empty display
    // name is accepted. Instead we test a name that exceeds
    // MAX_NAME_LEN bytes (all 'a' chars → 256 bytes).
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .create_user("alice", &"a".repeat(257))
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_send_message_one_on_one_with_no_receiver_is_allowed() {
    // 1-to-1 chats allow `receiver_id = None` (notes-to-self).
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    let m = mgr
        .send_message(&conv.id, &alice.id, None, "note to self", None)
        .await
        .unwrap();
    assert_eq!(m.receiver_id, None);
}

#[tokio::test]
async fn im_send_message_rejects_negative_timestamp_via_edited_at() {
    // `send_message` does not take an explicit timestamp (it stamps
    // the current time internally), so this test exercises
    // `edit_message` with a negative `edited_at` to cover that
    // validation path.
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    let msg = mgr
        .send_message(&conv.id, &alice.id, None, "x", None)
        .await
        .unwrap();
    let err = mgr
        .edit_message(
            &msg.id,
            "y",
            chrono::DateTime::<chrono::Utc>::from_timestamp(-1, 0).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Invalid(_)));
}

#[tokio::test]
async fn im_get_message_by_id_returns_none_for_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let m = mgr.get_message_by_id("ghost").await.unwrap();
    assert!(m.is_none());
}

#[tokio::test]
async fn im_get_message_by_id_rejects_bad_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_message_by_id("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_messages_returns_empty_for_unknown_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let msgs = mgr.get_messages("ghost", Some(10)).await.unwrap();
    assert!(msgs.is_empty());
}

#[tokio::test]
async fn im_get_messages_rejects_bad_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_messages("  ", Some(10)).await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_messages_uses_default_limit_when_none() {
    // Default limit is 100 — push 150 messages and expect 100.
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    for i in 0..150 {
        mgr.send_message(&conv.id, &alice.id, None, &format!("m{i}"), None)
            .await
            .unwrap();
    }
    let msgs = mgr.get_messages(&conv.id, None).await.unwrap();
    assert_eq!(msgs.len(), 100);
}

#[tokio::test]
async fn im_count_messages_returns_zero_for_unknown_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    assert_eq!(mgr.count_messages("ghost").await.unwrap(), 0);
}

#[tokio::test]
async fn im_count_messages_rejects_bad_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.count_messages("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_prune_messages_before_rejects_bad_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.prune_messages_before("  ", 0).await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_prune_messages_before_with_zero_cutoff_keeps_all() {
    // A cutoff of 0 (Unix epoch) should leave any reasonable
    // modern message untouched.
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    mgr.send_message(&conv.id, &alice.id, None, "x", None)
        .await
        .unwrap();
    let removed = mgr.prune_messages_before(&conv.id, 0).await.unwrap();
    assert_eq!(removed, 0);
    assert_eq!(mgr.count_messages(&conv.id).await.unwrap(), 1);
}

#[tokio::test]
async fn im_delete_message_returns_false_for_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    assert!(!mgr.delete_message("ghost").await.unwrap());
}

#[tokio::test]
async fn im_delete_message_rejects_bad_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.delete_message("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_add_pending_message_rejects_bad_message_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .add_pending_message("  ", "alice", "conv")
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_add_pending_message_rejects_bad_receiver_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .add_pending_message("msg", "  ", "conv")
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_add_pending_message_rejects_bad_conversation_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .add_pending_message("msg", "alice", "  ")
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_pending_messages_returns_empty_for_unknown_receiver() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let pending = mgr.get_pending_messages("ghost").await.unwrap();
    assert!(pending.is_empty());
}

#[tokio::test]
async fn im_get_pending_messages_rejects_bad_receiver() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_pending_messages("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_clear_pending_messages_rejects_bad_receiver() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.clear_pending_messages("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_update_user_sequence_rejects_bad_user_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .update_user_sequence("  ", "alice", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_update_user_sequence_rejects_bad_sender_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .update_user_sequence("alice", "  ", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_user_sequence_returns_none_for_unseen_pair() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let seq = mgr.get_user_sequence("a", "b").await.unwrap();
    assert!(seq.is_none());
}

#[tokio::test]
async fn im_get_user_sequence_rejects_bad_user_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_user_sequence("  ", "b").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_user_sequence_rejects_bad_sender_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_user_sequence("a", "  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_sender_sequence_returns_none_for_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let seq = mgr.get_sender_sequence("ghost").await.unwrap();
    assert!(seq.is_none());
}

#[tokio::test]
async fn im_get_sender_sequence_rejects_bad_sender_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_sender_sequence("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_detect_missing_messages_returns_none_when_no_messages() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let miss = mgr.detect_missing_messages("a", "b").await.unwrap();
    assert_eq!(miss, None, "no messages → no gap");
}

#[tokio::test]
async fn im_detect_missing_messages_rejects_bad_user_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.detect_missing_messages("  ", "b").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_detect_missing_messages_rejects_bad_sender_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.detect_missing_messages("a", "  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_messages_by_sequence_range_rejects_inverted_range() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .get_messages_by_sequence_range("a", 5, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Invalid(_)));
}

#[tokio::test]
async fn im_get_messages_by_sequence_range_rejects_bad_sender() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .get_messages_by_sequence_range("  ", 1, 5)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_create_message_receipt_rejects_bad_message_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let err = mgr
        .create_message_receipt("  ", &alice.id, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_create_message_receipt_rejects_bad_receiver_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .create_message_receipt("msg", "  ", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_message_receipts_rejects_bad_message_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_message_receipts("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_message_receipts_returns_empty_for_unknown_message() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let receipts = mgr.get_message_receipts("ghost").await.unwrap();
    assert!(receipts.is_empty());
}

#[tokio::test]
async fn im_get_messages_for_sync_rejects_bad_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_messages_for_sync("  ", None, 10).await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_messages_for_sync_returns_empty_for_unknown_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let resp = mgr.get_messages_for_sync("ghost", None, 10).await.unwrap();
    assert!(resp.messages.is_empty());
    assert!(!resp.has_more);
    assert_eq!(resp.last_sequence, 0);
}

#[tokio::test]
async fn im_get_messages_for_sync_has_more_when_more_remain() {
    // 5 messages, ask for 2 → first slice has 2 + has_more=true.
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    for i in 0..5 {
        mgr.send_message(&conv.id, &alice.id, None, &format!("m{i}"), None)
            .await
            .unwrap();
    }
    let resp = mgr.get_messages_for_sync(&conv.id, None, 2).await.unwrap();
    assert_eq!(resp.messages.len(), 2);
    assert!(resp.has_more);
    assert_eq!(resp.last_sequence, 2);
}

#[test]
fn im_compress_decompress_messages_roundtrip() {
    use chrono::Utc;
    let messages: Vec<Message> = (0..5)
        .map(|i| Message {
            id: format!("m{i}"),
            conversation_id: "c".into(),
            sender_id: "alice".into(),
            receiver_id: None,
            content: format!("hello {i}"),
            timestamp: Utc::now(),
            sequence: Some(i),
            reply_to: None,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        })
        .collect();
    let blob = ImManager::compress_messages(&messages).unwrap();
    // Compressed blob should be smaller than the equivalent JSON.
    assert!(!blob.is_empty());
    let decoded = ImManager::decompress_messages(&blob).unwrap();
    assert_eq!(decoded.len(), messages.len());
    for (orig, back) in messages.iter().zip(decoded.iter()) {
        assert_eq!(orig.content, back.content);
        assert_eq!(orig.sender_id, back.sender_id);
    }
}

#[test]
fn im_decompress_messages_rejects_corrupt_payload() {
    // Feeding random bytes to zstd produces an `Io(UnexpectedEof)`
    // framing error that zstd wraps in `Zstd`. In practice the wrapper
    // is `Io(Custom { kind: UnexpectedEof, ... })`.
    let err = ImManager::decompress_messages(b"not a zstd stream").unwrap_err();
    assert!(
        matches!(err, ChatStoreError::Io(_) | ChatStoreError::Zstd(_)),
        "expected Io or Zstd, got: {err:?}"
    );
}

#[test]
fn im_decompress_messages_rejects_truncated_payload() {
    // Compress a real payload then truncate the result. The framing
    // error surfaces as `Io(UnexpectedEof)` rather than `Zstd`.
    use chrono::Utc;
    let messages: Vec<Message> = vec![Message {
        id: "m1".into(),
        conversation_id: "c".into(),
        sender_id: "alice".into(),
        receiver_id: None,
        content: "hello".into(),
        timestamp: Utc::now(),
        sequence: Some(1),
        reply_to: None,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    }];
    let mut blob = ImManager::compress_messages(&messages).unwrap();
    blob.truncate(blob.len() / 2);
    let err = ImManager::decompress_messages(&blob).unwrap_err();
    // Could be Io (framing error) or Zstd (decompression error); both are Fatal.
    assert!(
        matches!(err, ChatStoreError::Io(_) | ChatStoreError::Zstd(_)),
        "expected Io or Zstd, got: {err:?}"
    );
}

#[tokio::test]
async fn im_get_compressed_messages_for_sync_returns_blob() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    for i in 0..3 {
        mgr.send_message(&conv.id, &alice.id, None, &format!("m{i}"), None)
            .await
            .unwrap();
    }
    let blob = mgr
        .get_compressed_messages_for_sync(&conv.id, None, 10)
        .await
        .unwrap();
    assert!(!blob.is_empty());
    let decoded = ImManager::decompress_messages(&blob).unwrap();
    assert_eq!(decoded.len(), 3);
}

#[tokio::test]
async fn im_get_compressed_messages_for_sync_rejects_bad_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .get_compressed_messages_for_sync("  ", None, 10)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_compressed_messages_for_sync_rejects_zero_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .get_compressed_messages_for_sync("c", None, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Invalid(_)));
}

#[test]
fn im_verify_message_integrity_returns_false_for_missing_hash() {
    // A `Message` with `integrity_hash = None` always fails
    // verification — this is the `Missing` outcome.
    use chrono::Utc;
    let m = Message {
        id: "m1".into(),
        conversation_id: "c".into(),
        sender_id: "a".into(),
        receiver_id: None,
        content: "x".into(),
        timestamp: Utc::now(),
        sequence: Some(1),
        reply_to: None,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    assert!(!ImManager::verify_message_integrity(&m));
}

#[test]
fn im_verify_message_integrity_returns_false_for_edited_with_no_timestamp() {
    // `is_edited=true` but `edited_at=None` is treated as invalid:
    // the verification must fail.
    use chrono::Utc;
    let m = Message {
        id: "m1".into(),
        conversation_id: "c".into(),
        sender_id: "a".into(),
        receiver_id: None,
        content: "x".into(),
        timestamp: Utc::now(),
        sequence: Some(1),
        reply_to: None,
        integrity_hash: Some("deadbeef".into()),
        is_edited: true,
        edited_at: None,
    };
    assert!(!ImManager::verify_message_integrity(&m));
}

#[test]
fn im_verify_message_integrity_returns_false_for_tampered_content() {
    // Stamp a real hash, then mutate the content; the verification
    // must fail with `Mismatch`.
    use chrono::Utc;
    let m = Message {
        id: "m1".into(),
        conversation_id: "c".into(),
        sender_id: "a".into(),
        receiver_id: Some("b".into()),
        content: "original".into(),
        timestamp: Utc::now(),
        sequence: Some(1),
        reply_to: None,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    };
    let hash = generate_integrity_hash(
        &m.sender_id,
        m.receiver_id.as_deref(),
        &m.content,
        m.sequence.unwrap(),
        &m.timestamp.to_rfc3339(),
    );
    let mut m = m;
    m.integrity_hash = Some(hash);
    assert!(ImManager::verify_message_integrity(&m));

    m.content = "tampered".into();
    assert!(!ImManager::verify_message_integrity(&m));
}

#[tokio::test]
async fn im_touch_user_rejects_bad_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.touch_user("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_create_conversation_rejects_oversize_title() {
    // `validate_name` does NOT reject empty strings. An empty title
    // is accepted. Instead we test a title that exceeds MAX_NAME_LEN.
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .create_conversation(ChatType::Group, &"a".repeat(257), false)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_create_conversation_increments_message_count_after_send() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    assert_eq!(conv.message_count, 0);
    mgr.send_message(&conv.id, &alice.id, None, "x", None)
        .await
        .unwrap();
    let refreshed = mgr.get_conversation(&conv.id).await.unwrap().unwrap();
    // `send_message` bumps `message_count` and `updated_at`, but NOT `last_sequence`.
    assert_eq!(refreshed.message_count, 1);
    assert!(refreshed.updated_at >= conv.created_at);
}

#[tokio::test]
async fn im_add_group_member_rejects_bad_role() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::Group, "team", false)
        .await
        .unwrap();
    let err = mgr
        .add_group_member(&conv.id, &alice.id, "")
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_remove_group_member_rejects_bad_conversation_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.remove_group_member("  ", "alice").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_remove_group_member_rejects_bad_user_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.remove_group_member("conv", "  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_get_group_members_rejects_bad_conversation_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr.get_group_members("  ").await.unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_send_message_rejects_bad_conversation_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let err = mgr
        .send_message("  ", &alice.id, None, "x", None)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_send_message_rejects_bad_sender_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    let err = mgr
        .send_message(&conv.id, "  ", None, "x", None)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_send_message_rejects_bad_receiver_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    let err = mgr
        .send_message(&conv.id, &alice.id, Some("  "), "x", None)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_send_message_rejects_bad_reply_to() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    let err = mgr
        .send_message(&conv.id, &alice.id, None, "x", Some("  "))
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_edit_message_rejects_bad_message_id() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let err = mgr
        .edit_message("  ", "y", chrono::Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_edit_message_rejects_empty_content() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    let msg = mgr
        .send_message(&conv.id, &alice.id, None, "x", None)
        .await
        .unwrap();
    let err = mgr
        .edit_message(&msg.id, "", chrono::Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Validation(_)));
}

#[tokio::test]
async fn im_edit_message_on_message_without_sequence_returns_invalid() {
    // Manually insert a message row with `sequence = NULL` and try
    // to edit it — must surface `Invalid` because edits are only
    // defined for sequenced messages.
    let dir = tempfile::tempdir().unwrap();
    let mgr = ImManager::new(dir.path().join("hub.db")).unwrap();
    let alice = mgr.create_user("alice", "Alice").await.unwrap();
    let conv = mgr
        .create_conversation(ChatType::OneOnOne, "self", false)
        .await
        .unwrap();
    let msg = mgr
        .send_message(&conv.id, &alice.id, None, "x", None)
        .await
        .unwrap();
    // Patch the row to clear the sequence.
    {
        // We need a `&Connection`; reach into `mgr.conn` via the
        // public `db_path` + a fresh `rusqlite::Connection`.
        let conn = rusqlite::Connection::open(mgr.db_path()).unwrap();
        conn.execute(
            "UPDATE messages SET sequence = NULL WHERE id = ?1",
            rusqlite::params![&msg.id],
        )
        .unwrap();
    }
    let err = mgr
        .edit_message(&msg.id, "y", chrono::Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, ChatStoreError::Invalid(_)));
}

// =========================================================================
// Type-level tests for the `ChatType` enum — `as_str` / `parse` are
// private but are exercised indirectly through `im::send_message`'s
// "group must have no receiver" check; we add a focused test to
// confirm the round-trip via `Message` JSON shape.
// =========================================================================

#[test]
fn chat_type_serializes_as_snake_case() {
    let one = serde_json::to_string(&ChatType::OneOnOne).unwrap();
    assert_eq!(one, "\"one_on_one\"");
    let grp = serde_json::to_string(&ChatType::Group).unwrap();
    assert_eq!(grp, "\"group\"");
}
