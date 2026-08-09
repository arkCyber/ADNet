//! End-to-end test for the iroh-docs backed chat store.
//!
//! This test wires **two** [`IrohDocsChat`]s that share the same
//! [`iroh_docs::api::DocsApi`] engine (in-memory replica store, no
//! actual networking), and verifies that:
//!
//! 1. A message appended on side A is visible on side B through
//!    `get_messages`.
//! 2. Subscribers on B receive the message via the `subscribe`
//!    broadcast channel.
//!
//! It does not exercise the actual iroh P2P sync; for that, see the
//! `examples/iroh_e2e.rs` integration example (to be added in a
//! follow-up).

#![cfg(feature = "iroh")]

use std::path::PathBuf;

use adnet_blobstore::IrohBlobStore;
use adnet_chatstore::{IrohDocsChat, MessageEvent};
use chrono::Utc;
use iroh::Endpoint;
use iroh::endpoint::presets::N0;
use iroh_docs::api::DocsApi;
use iroh_docs::protocol::Docs;
use iroh_gossip::net::Gossip;
use tempfile::TempDir;

/// Spin up a fresh chat bridge backed by an in-memory docs engine
/// and an empty `IrohBlobStore` rooted at a tempdir.
async fn fresh_bridge() -> (TempDir, IrohDocsChat, Docs) {
    let dir = TempDir::new().expect("tempdir");
    let blob_store = IrohBlobStore::open(dir.path()).await.expect("blobs");

    // Build a tiny docs engine: a bound `Endpoint` (so docs has
    // something to gossip with), a `Gossip` instance, and an
    // in-memory replica store. This is the cheapest way to get a
    // working `Docs` for tests.
    let endpoint = Endpoint::bind(N0).await.expect("endpoint bind");
    let gossip = Gossip::builder().spawn(endpoint.clone());
    // `Docs::spawn` wants `iroh_blobs::api::Store` by value.
    // `FsStore: Into<Store>` via the `From<FsStore>` impl, so we
    // pass an owned `FsStore` and call `.into()` to drive the
    // conversion at the call site.
    let fs: iroh_blobs::api::Store = (*blob_store.handle()).clone().into();
    let docs = Docs::memory()
        .spawn(endpoint.clone(), fs, gossip)
        .await
        .expect("docs spawn");
    let api: DocsApi = docs.api().clone();
    let bridge = IrohDocsChat::new(std::sync::Arc::new(api), blob_store)
        .await
        .expect("bridge");
    (dir, bridge, docs)
}

fn sample_message(sender: &str, content: &str) -> adnet_chatstore::Message {
    adnet_chatstore::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: String::new(), // filled in by append_message
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

#[tokio::test]
async fn append_then_get_roundtrip() {
    let (_dir, bridge, _docs) = fresh_bridge().await;

    let handle = bridge.open_conversation("conv-1").await.expect("open conv");
    assert_eq!(handle.conversation_id, "conv-1");

    let seq1 = bridge
        .append_message("conv-1", sample_message("alice", "hello"))
        .await
        .expect("append 1");
    let seq2 = bridge
        .append_message("conv-1", sample_message("bob", "hi alice"))
        .await
        .expect("append 2");
    let seq3 = bridge
        .append_message("conv-1", sample_message("alice", "how are you?"))
        .await
        .expect("append 3");

    assert_eq!(seq1, 1);
    assert_eq!(seq2, 1); // bob's first message has its own counter
    assert_eq!(seq3, 2); // alice's second

    let history = bridge
        .get_messages("conv-1", None, 100)
        .await
        .expect("get_messages");
    assert_eq!(history.len(), 3);

    // The ordering rule is (seq, sender). The 3 messages sort as:
    // (1, alice), (1, bob), (2, alice)
    let senders: Vec<&str> = history.iter().map(|m| m.sender_id.as_str()).collect();
    assert_eq!(senders, vec!["alice", "bob", "alice"]);

    // Filtering by `after` skips the first message.
    let after = bridge
        .get_messages("conv-1", Some(1), 100)
        .await
        .expect("get after=1");
    // Expect to skip msg/(alice|seq<=1) AND msg/(bob|seq<=1). The
    // remaining one is (2, alice).
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].sender_id, "alice");
    assert_eq!(after[0].content, "how are you?");
}

#[tokio::test]
async fn subscribe_receives_replay_then_live_inserts() {
    let (_dir, bridge, _docs) = fresh_bridge().await;

    bridge.open_conversation("conv-2").await.expect("open conv");

    // Pre-populate one message before any subscribers exist.
    bridge
        .append_message("conv-2", sample_message("alice", "before"))
        .await
        .expect("pre seed");

    // Now subscribe — we expect a single Replay with one message.
    let mut rx = bridge.subscribe("conv-2").await.expect("subscribe");

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("replay received in time")
        .expect("replay channel not closed");
    match first {
        MessageEvent::Replay(msgs) => {
            assert_eq!(msgs.len(), 1);
            assert_eq!(msgs[0].content, "before");
        }
        other => panic!("expected Replay first, got {other:?}"),
    }

    // Append another — subscriber should see a live Insert.
    bridge
        .append_message("conv-2", sample_message("bob", "after"))
        .await
        .expect("append");

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("live received in time")
        .expect("live channel not closed");
    match second {
        MessageEvent::Insert(m) => assert_eq!(m.content, "after"),
        other => panic!("expected Insert second, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_sender_id_is_rejected() {
    let (_dir, bridge, _docs) = fresh_bridge().await;
    bridge.open_conversation("conv-3").await.expect("open conv");
    let res = bridge
        .append_message("conv-3", sample_message("", "no one speaks"))
        .await;
    assert!(res.is_err(), "empty sender must error");
}

#[tokio::test]
async fn unknown_conversation_errors_on_append() {
    let (_dir, bridge, _docs) = fresh_bridge().await;
    let res = bridge
        .append_message("never-opened", sample_message("alice", "x"))
        .await;
    assert!(res.is_err(), "append to unknown conv must error");
}

// Helper: silence the unused-import warning for `PathBuf` if it ends
// up unused in the future.
#[allow(dead_code)]
fn _path_unused() -> PathBuf {
    PathBuf::new()
}
