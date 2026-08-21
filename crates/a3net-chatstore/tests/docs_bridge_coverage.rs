//! Feature-specific (`iroh`) coverage for `a3net-chatstore`.
//!
//! Exercises every public function in [`crate::docs_bridge`] that is
//! NOT covered by the existing integration tests in
//! `iroh_docs_chat.rs` and `docs_bridge_aerospace.rs`.
//!
//! # DO-178C properties
//!
//! - **Idempotent**: each test allocates a fresh `TempDir`; no
//!   shared state across tests.
//! - **Deterministic**: messages use fixed payloads.
//! - **Self-contained**: no fixtures from outside this crate.

#![cfg(feature = "iroh")]

use std::sync::Arc;

use a3net_blobstore::IrohBlobStore;
use a3net_chatstore::docs_bridge::{
    DEFAULT_MESSAGE_LIMIT, DocsBridgeError, ErrorClass, IrohDocsChat, MAX_APPEND_RETRIES,
};
use a3net_chatstore::im::Message;
use chrono::Utc;
use iroh_docs::api::protocol::ShareMode;

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// Build a fresh chat bridge backed by an in-memory docs engine.
/// Uses `Minimal` endpoint preset to avoid real networking.
async fn fresh_bridge() -> (tempfile::TempDir, IrohDocsChat) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let blob_store = IrohBlobStore::open(dir.path()).await.expect("blobs");

    let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::Minimal)
        .await
        .expect("endpoint bind");
    let gossip = iroh_gossip::net::Gossip::builder().spawn(endpoint.clone());
    let fs: iroh_blobs::api::Store = (*blob_store.handle()).clone().into();
    let docs = iroh_docs::protocol::Docs::memory()
        .spawn(endpoint.clone(), fs, gossip)
        .await
        .expect("docs spawn");
    let api = docs.api().clone();
    let bridge = IrohDocsChat::new(Arc::new(api), blob_store)
        .await
        .expect("bridge");
    (dir, bridge)
}

fn sample_msg(sender: &str, content: &str) -> Message {
    Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: String::new(),
        sender_id: sender.to_string(),
        receiver_id: None,
        content: content.to_string(),
        timestamp: Utc::now(),
        sequence: None,
        reply_to: None,
        integrity_hash: None,
        is_edited: false,
        edited_at: None,
    }
}

// ─── Constants ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn constants_are_as_documented() {
    assert_eq!(MAX_APPEND_RETRIES, 32);
    assert_eq!(DEFAULT_MESSAGE_LIMIT, 1024);
}

// ─── Construction ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn new_returns_default_author() {
    let (_dir, bridge) = fresh_bridge().await;
    let author = bridge.default_author();
    assert_eq!(author.to_string().len(), 64); // hex(32 bytes)
    let api_author = bridge.api().author_default().await.unwrap();
    assert_eq!(author, api_author);
}

#[tokio::test]
async fn api_returns_arc() {
    let (_dir, bridge) = fresh_bridge().await;
    let a = bridge.api();
    let b = bridge.api();
    assert!(Arc::ptr_eq(&a, &b));
}

// ─── open_existing ────────────────────────────────────────────────────────────

#[tokio::test]
async fn open_existing_returns_err_for_unknown_namespace() {
    let (_dir, bridge) = fresh_bridge().await;
    let unknown = iroh_docs::NamespaceId::from([0u8; 32]);
    let result = bridge.open_existing("conv-x", unknown).await;
    assert!(result.is_err(), "unknown namespace must error");
}

// ─── share ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn share_produces_non_empty_ticket() {
    let (_dir, bridge) = fresh_bridge().await;
    bridge.open_conversation("conv-share").await.expect("open");

    let ticket = bridge
        .share("conv-share", ShareMode::Write)
        .await
        .expect("share");
    assert!(!ticket.to_string().is_empty());
}

// ─── start_sync ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn start_sync_with_empty_peer_list_is_noop() {
    // `start_sync` with no peers should not error.
    let (_dir, bridge) = fresh_bridge().await;
    bridge.open_conversation("conv-sync").await.expect("open");
    bridge
        .start_sync("conv-sync", vec![])
        .await
        .expect("start_sync empty peers");
}

// ─── get_messages ────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_messages_limit_zero_uses_default_cap() {
    let (_dir, bridge) = fresh_bridge().await;
    bridge.open_conversation("conv-limit").await.expect("open");

    for i in 0..10 {
        bridge
            .append_message("conv-limit", sample_msg("alice", &format!("msg {i}")))
            .await
            .expect("append");
    }

    // `limit=0` → DEFAULT_MESSAGE_LIMIT (1024) → all 10 returned.
    let msgs = bridge
        .get_messages("conv-limit", None, 0)
        .await
        .expect("get");
    assert_eq!(msgs.len(), 10);
}

#[tokio::test]
async fn get_messages_filters_by_after_sequence() {
    let (_dir, bridge) = fresh_bridge().await;
    bridge.open_conversation("conv-after").await.expect("open");

    let s1 = bridge
        .append_message("conv-after", sample_msg("alice", "first"))
        .await
        .expect("append 1");
    let _s2 = bridge
        .append_message("conv-after", sample_msg("alice", "second"))
        .await
        .expect("append 2");

    // After seq=s1 → only the second message.
    let msgs = bridge
        .get_messages("conv-after", Some(s1), 100)
        .await
        .expect("get after seq");
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "second");

    // After seq=s2 → no messages.
    let msgs = bridge
        .get_messages("conv-after", Some(s1 + 1), 100)
        .await
        .expect("get after seq 2");
    assert!(msgs.is_empty());
}

// ─── shutdown / close_all ────────────────────────────────────────────────────

#[tokio::test]
async fn shutdown_is_idempotent() {
    let (_dir, bridge) = fresh_bridge().await;
    bridge
        .open_conversation("conv-shutdown")
        .await
        .expect("open");
    bridge.shutdown().await;
    bridge.shutdown().await; // Idempotent.
}

#[tokio::test]
async fn close_all_clears_cache() {
    let (_dir, bridge) = fresh_bridge().await;
    bridge.open_conversation("conv-cache").await.expect("open");
    bridge.close_all().await;
    bridge.close_all().await; // Idempotent.

    // After close_all, re-opening the same conversation creates a new Doc.
    bridge
        .open_conversation("conv-cache")
        .await
        .expect("re-open");
}

// ─── DocsBridgeError recoverability ─────────────────────────────────────────

#[test]
fn docs_bridge_error_every_variant_has_classification() {
    use a3net_chatstore::ChatStoreError;

    let cases: Vec<(DocsBridgeError, ErrorClass)> = vec![
        // Chat variants → ErrorClass.
        (
            DocsBridgeError::Chat(ChatStoreError::Validation("x".into())),
            ErrorClass::UserError,
        ),
        (
            DocsBridgeError::Chat(ChatStoreError::Invalid("x".into())),
            ErrorClass::UserError,
        ),
        (
            DocsBridgeError::Chat(ChatStoreError::Constraint("x".into())),
            ErrorClass::UserError,
        ),
        (
            DocsBridgeError::Chat(ChatStoreError::ForeignKey("x".into())),
            ErrorClass::UserError,
        ),
        (
            DocsBridgeError::Chat(ChatStoreError::NotFound("x".into())),
            ErrorClass::Recoverable,
        ),
        // Sequence contention is recoverable.
        (DocsBridgeError::SequenceContention, ErrorClass::Recoverable),
        // Empty sender id → user error.
        (DocsBridgeError::EmptySenderId, ErrorClass::UserError),
        // NotOpen → user error.
        (DocsBridgeError::NotOpen("c".into()), ErrorClass::UserError),
        // Fatal variants.
        (
            DocsBridgeError::Chat(ChatStoreError::Lock),
            ErrorClass::Fatal,
        ),
        (
            DocsBridgeError::Chat(ChatStoreError::Sqlite(rusqlite::Error::InvalidQuery)),
            ErrorClass::Fatal,
        ),
        (
            DocsBridgeError::Iroh(anyhow::anyhow!("boom")),
            ErrorClass::Fatal,
        ),
        // Serde → Fatal (requires serde_json in scope).
        (
            DocsBridgeError::Serde(
                serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
            ),
            ErrorClass::Fatal,
        ),
    ];

    for (err, expected) in cases {
        let actual = err.recoverability();
        assert_eq!(actual, expected, "wrong class for {err:?}");
    }
}
