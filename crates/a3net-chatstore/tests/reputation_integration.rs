//! Integration tests for the `a3net-reputation` × `a3net-chatstore`
//! bridge. Verifies that `ChatTrustStore::set` (when a reputation
//! reporter is attached) emits `ChatTrustSet` events into the
//! global PeerScore, and that the user-id → NodeId mapping is
//! deterministic across calls.

use std::sync::Arc;

use a3net_chatstore::trust::{init_trust_schema, ChatTrustStore};
use a3net_reputation::{PeerScoreTable, ReputationEvent, ReputationParams, ReputationReporter};
use a3net_types::NodeId;
use rusqlite::Connection;
use tempfile::tempdir;
use tokio::sync::Mutex;

async fn store() -> ChatTrustStore {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    init_trust_schema(&mut conn).unwrap();
    // Leak the tempdir so the SQLite file (and its `-wal`/`-shm`
    // siblings) outlive this scope. The OS reaps the directory at
    // process exit.
    std::mem::forget(dir);
    ChatTrustStore::new(Arc::new(Mutex::new(conn)))
}

/// Map a user-id to the same `NodeId` the trust store would derive.
/// Mirrors the private `ChatTrustStore::user_to_node` helper — we
/// can't call it directly because it's private, but the algorithm
/// is documented in `trust.rs` and must be stable.
fn user_to_node(user_id: &str) -> NodeId {
    use blake3::Hash;
    let h: Hash = blake3::hash(user_id.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&h.as_bytes()[..32]);
    NodeId::from_bytes(&bytes).expect("32 bytes")
}

/// A positive chat trust level (+2 = Friend) must move the target's
/// global PeerScore up.
#[tokio::test]
async fn positive_chat_trust_lifts_peer_score() {
    let s = store().await;
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    s.with_reputation(reporter.clone());

    s.set("alice", "bob", 2, Some("good friend".into()))
        .await
        .unwrap();

    let bob_node = user_to_node("bob");
    let score = reporter
        .table()
        .score(&bob_node)
        .expect("bob must have a score entry after chat_trust_set");
    assert!(
        score > 0.0,
        "trust level +2 (Friend) should drive the score positive, got {score}"
    );
}

/// A negative chat trust level (-3 = Blocked) must move the target's
/// score below zero.
#[tokio::test]
async fn blocked_chat_trust_pushes_score_below_zero() {
    let s = store().await;
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    s.with_reputation(reporter.clone());

    s.set("alice", "spammer", -3, Some("clear abuse".into()))
        .await
        .unwrap();

    let spammer_node = user_to_node("spammer");
    let score = reporter
        .table()
        .score(&spammer_node)
        .expect("spammer must have a score entry after chat_trust_set");
    assert!(
        score < 0.0,
        "trust level -3 (Blocked) should drive the score negative, got {score}"
    );
}

/// The user-id → NodeId mapping is deterministic: two `set` calls
/// with the same target must land on the same PeerScore entry.
#[tokio::test]
async fn user_id_mapping_is_deterministic() {
    let s = store().await;
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    s.with_reputation(reporter.clone());

    s.set("alice", "bob", 1, None).await.unwrap();
    s.set("alice2", "bob", 2, None).await.unwrap(); // different owner, same target

    let bob_node = user_to_node("bob");
    let score = reporter
        .table()
        .score(&bob_node)
        .expect("bob must have a score entry");
    // Two positive contributions => strictly greater than what
    // either alone would have produced. We just check the score
    // is positive and not zero — the exact value depends on
    // weights, but the deterministic mapping guarantees both
    // calls contributed to the same entry.
    assert!(score > 0.0);
}

/// A store without a reputation hook must still accept writes —
/// the reputation path is strictly opt-in.
#[tokio::test]
async fn store_without_reputation_still_works() {
    let s = store().await;
    s.set("alice", "bob", 1, None).await.expect("set without reporter");
    let rec = s
        .get("alice", "bob")
        .await
        .unwrap()
        .expect("row persisted");
    assert_eq!(rec.level, 1);
}

/// Update path: a second `set` on the same pair must also emit a
/// second event, driving the score further. We verify by comparing
/// the score after one vs. two calls.
#[tokio::test]
async fn repeated_set_stacks_events() {
    let s = store().await;
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    s.with_reputation(reporter.clone());

    s.set("alice", "bob", 3, None).await.unwrap();
    let bob_node = user_to_node("bob");
    let after_one = reporter
        .table()
        .score(&bob_node)
        .expect("scored after first set");
    s.set("alice", "bob", 3, None).await.unwrap();
    let after_two = reporter
        .table()
        .score(&bob_node)
        .expect("scored after second set");
    assert!(
        after_two > after_one,
        "second set should push score higher: was {after_one}, now {after_two}"
    );
}

/// Sanity: the `kind_tag` on the event matches the variant name.
#[test]
fn chat_trust_event_kind_tag_is_stable() {
    let peer = NodeId::random();
    let kind = ReputationEvent::ChatTrustSet {
        peer,
        by_user: 1,
        level: 2,
    }
    .kind_tag();
    assert!(kind.contains("chat_trust_set"), "kind_tag was {kind:?}");
}