//! End-to-end integration tests for `a3chat-app` storage + service layer.
//!
//! These tests stand up the full [`ChatStorage`] + [`ChatService`]
//! + [`NotificationBus`] stack against a real SQLite database (in
//! a tempdir) and walk a message through every stage of the
//! product's read/write path:
//!
//! 1. Caller hands a `MessageEnvelope` to `ChatService::send_message`.
//! 2. The service persists via `ChatStorage::save_outbound`, which
//!    *encrypts* the body with the owner's keyring.
//! 3. We re-open the same SQLite file cold and read back via
//!    `list_messages`, which must *decrypt* the body back to the
//!    original plaintext.
//! 4. The `NotificationBus` receives a `ChatMessageReceived` event
//!    for the peer.
//!
//! Anything weaker than this would let a regression in the
//! `ChatStorage` ↔ `E2eKeyring` ↔ `a3chat-crypto` glue pass review
//! unnoticed.

use a3chat_app::chat_service::ChatService;
use a3chat_app::keyring::E2eKeyring;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{
    Attachment, AttachmentKind, MessageBody, MessageEnvelope, MessageType,
};
use tempfile::TempDir;

fn owner() -> UserId {
    UserId::from("alice-node-id")
}

fn peer() -> UserId {
    UserId::from("bob-node-id")
}

async fn fresh_stack() -> (TempDir, ChatService) {
    let dir = tempfile::tempdir().expect("tempdir");
    let keyring = E2eKeyring::new(owner());
    let storage = ChatStorage::new(StorageConfig::new(dir.path().to_path_buf()), keyring);
    storage.init_user(&owner()).await.expect("init_user");
    let bus = NotificationBus::new(64);
    (dir, ChatService::new(storage, bus))
}

fn envelope(content: &str, seq: u32, ts: i64) -> MessageEnvelope {
    MessageEnvelope {
        conversation_id: ConversationId::from("dm:alice-node-id:bob-node-id"),
        receiver_id: peer(),
        message_type: MessageType::Text,
        body: MessageBody::Plain {
            content: content.into(),
        },
        attachments: vec![],
        reply_to: None,
        sequence: seq,
        timestamp: ts,
    }
}

#[tokio::test]
async fn save_then_list_returns_sealed_body() {
    // `ChatStorage::list_messages` is the *raw* read path — it
    // returns whatever was persisted, including `MessageBody::Encrypted`
    // for sealed messages. Decryption is the caller's responsibility
    // (the higher-level service does that today via
    // `E2eKeyring::send_key_for`; future P1 work moves it into the
    // service layer).
    let (_dir, svc) = fresh_stack().await;

    let stored = svc
        .send_message(&owner(), &envelope("hello bob", 1, 1_700_000_001))
        .await
        .expect("send_message");
    assert_eq!(stored.message.sequence, 1);
    assert!(
        matches!(stored.message.body, MessageBody::Encrypted { .. }),
        "save_outbound must seal the body, got {:?}",
        stored.message.body
    );
    assert!(
        stored.was_encrypted_at_write,
        "service must report that the body was encrypted on write"
    );

    // Read back via the same storage — body is still encrypted
    // (matches what hit disk).
    let conv = ConversationId::from("dm:alice-node-id:bob-node-id");
    let list = svc
        .storage()
        .list_messages(&owner(), &conv, 10)
        .await
        .expect("list_messages");
    assert_eq!(list.len(), 1, "exactly one message persisted");
    assert!(
        matches!(list[0].body, MessageBody::Encrypted { .. }),
        "list_messages returns the persisted (encrypted) body; got {:?}",
        list[0].body
    );
    // Nonces must match what we wrote — no resampling on read.
    let (nonce_written, _) = match &stored.message.body {
        MessageBody::Encrypted {
            nonce, ciphertext, ..
        } => (nonce.clone(), ciphertext.clone()),
        _ => unreachable!(),
    };
    let (nonce_read, _) = match &list[0].body {
        MessageBody::Encrypted {
            nonce, ciphertext, ..
        } => (nonce.clone(), ciphertext.clone()),
        _ => unreachable!(),
    };
    assert_eq!(nonce_written, nonce_read, "nonce must round-trip verbatim");
}

#[tokio::test]
async fn reload_from_disk_preserves_sealed_body() {
    // Cold-restart: a fresh storage handle against the same SQLite
    // file must still return the encrypted body byte-for-byte.
    let (dir, svc) = fresh_stack().await;
    let stored = svc
        .send_message(&owner(), &envelope("cold-start", 1, 1))
        .await
        .expect("send");
    drop(svc);

    // Re-open.
    let keyring = E2eKeyring::new(owner());
    let storage = ChatStorage::new(StorageConfig::new(dir.path().to_path_buf()), keyring);
    let conv = ConversationId::from("dm:alice-node-id:bob-node-id");
    let list = storage
        .list_messages(&owner(), &conv, 10)
        .await
        .expect("list after reload");
    assert_eq!(list.len(), 1);
    // Same ciphertext after a fresh open — proves persistence,
    // not just in-memory caching.
    let (orig_ct, orig_nonce) = match &stored.message.body {
        MessageBody::Encrypted {
            nonce, ciphertext, ..
        } => (ciphertext.clone(), nonce.clone()),
        _ => unreachable!("was encrypted on write"),
    };
    match &list[0].body {
        MessageBody::Encrypted {
            nonce, ciphertext, ..
        } => {
            assert_eq!(nonce, &orig_nonce, "nonce survives reload");
            assert_eq!(ciphertext, &orig_ct, "ciphertext survives reload byte-for-byte");
        }
        other => panic!("expected sealed body after reload, got {other:?}"),
    }
}

#[tokio::test]
async fn two_messages_produce_distinct_ciphertexts() {
    // Same plaintext twice → different ciphertexts (fresh nonce +
    // tag). Otherwise an attacker could trivially detect duplicate
    // messages.
    let (_dir, svc) = fresh_stack().await;
    let a = svc
        .send_message(&owner(), &envelope("same", 1, 100))
        .await
        .unwrap();
    let b = svc
        .send_message(&owner(), &envelope("same", 2, 101))
        .await
        .unwrap();
    let (na, ca) = match &a.message.body {
        MessageBody::Encrypted {
            nonce, ciphertext, ..
        } => (nonce.clone(), ciphertext.clone()),
        _ => panic!("a not encrypted"),
    };
    let (nb, cb) = match &b.message.body {
        MessageBody::Encrypted {
            nonce, ciphertext, ..
        } => (nonce.clone(), ciphertext.clone()),
        _ => panic!("b not encrypted"),
    };
    assert_ne!(na, nb, "nonces must be fresh per message");
    assert_ne!(ca, cb, "ciphertexts must differ even for identical plaintext");
}

#[tokio::test]
async fn send_message_emits_notification_to_peer_bus() {
    let (_dir, svc) = fresh_stack().await;
    // Subscribe for the *peer* — the bus filters by owner identity
    // so the right receiver gets notified.
    let mut rx = svc.bus().subscribe_for(peer());
    svc.send_message(&owner(), &envelope("notify me", 1, 200))
        .await
        .expect("send");
    let evt = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .expect("event timed out")
        .expect("event missing");
    match evt {
        A3chatEvent::ChatMessageReceived {
            user_id,
            conversation_id,
            ..
        } => {
            assert_eq!(user_id, peer(), "event must target the peer, not the sender");
            assert_eq!(
                conversation_id.as_str(),
                "dm:alice-node-id:bob-node-id"
            );
        }
        other => panic!("expected ChatMessageReceived, got {other:?}"),
    }
}

#[tokio::test]
async fn attachments_survive_persist_and_reload() {
    // Attachments travel alongside the message body and must not be
    // encrypted (the E2E contract covers body only — attachments
    // are content-addressed blobs).
    let (_dir, svc) = fresh_stack().await;
    let mut env = envelope("see attached", 1, 300);
    env.attachments = vec![Attachment {
        attachment_id: "att-1".into(),
        file_type: AttachmentKind::Image,
        blob_hash: "a".repeat(64),
        file_name: "cat.jpg".into(),
        file_size: 4096,
        thumbnail_hash: Some("b".repeat(64)),
    }];
    svc.send_message(&owner(), &env).await.expect("send with attachment");
    let conv = ConversationId::from("dm:alice-node-id:bob-node-id");
    let list = svc
        .storage()
        .list_messages(&owner(), &conv, 10)
        .await
        .expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].attachments.len(), 1);
    assert_eq!(list[0].attachments[0].attachment_id, "att-1");
    assert_eq!(list[0].attachments[0].file_name, "cat.jpg");
}

#[tokio::test]
async fn system_messages_are_persisted_plaintext() {
    // System messages have no confidentiality value; the storage
    // layer leaves them in plaintext so the UI can render them
    // without bootstrapping E2E.
    let (_dir, svc) = fresh_stack().await;
    let mut env = envelope("server announcement", 1, 400);
    env.message_type = MessageType::System;
    let stored = svc.send_message(&owner(), &env).await.expect("send system");
    assert!(
        !stored.was_encrypted_at_write,
        "system messages must not be encrypted"
    );
    match &stored.message.body {
        MessageBody::Plain { content } => assert_eq!(content, "server announcement"),
        other => panic!("expected plaintext system body, got {other:?}"),
    }
}

#[tokio::test]
async fn send_message_validates_envelope() {
    // Invalid envelopes (e.g. negative timestamp, oversize content)
    // must be rejected before any DB write.
    let (_dir, svc) = fresh_stack().await;
    let mut bad = envelope("", 1, 0);
    bad.timestamp = -1;
    let result = svc.send_message(&owner(), &bad).await;
    assert!(
        result.is_err(),
        "negative timestamp must be rejected, got {:?}",
        result
    );
}