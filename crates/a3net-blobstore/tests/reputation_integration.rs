//! Integration tests for the `a3net-reputation` × `a3net-blobstore`
//! bitswap bridge. Requires the `reputation` feature on
//! `a3net-blobstore` so that `BitswapEngine::with_reputation` is in
//! scope and `process_message` feeds `BitswapSignal` events into
//! the global PeerScore.

#![cfg(feature = "reputation")]

use a3net_blobstore::bitswap::{
    BitswapEngine, BitswapMessage,
};
use a3net_reputation::{PeerScoreTable, ReputationParams, ReputationReporter};
use a3net_types::ContentHash;

fn hash(b: &[u8]) -> ContentHash {
    ContentHash::from_bytes(b)
}

/// Block delivery must record a positive `BitswapSignal::valid`
/// event. The peer that delivered the block must see its score
/// rise above zero.
#[test]
fn block_delivery_lifts_peer_score() {
    let mut engine = BitswapEngine::new();
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    engine = engine.with_reputation(reporter.clone());

    let peer_id = "peer-A";
    engine
        .add_peer(peer_id)
        .expect("peer id validates");

    let block = hash(b"hello-world");
    let data = b"payload-bytes-here".to_vec();
    engine.process_message(
        peer_id,
        BitswapMessage::Block {
            block: block.clone(),
            data,
        },
    );

    let node = blake3_node(peer_id);
    let score = reporter
        .table()
        .score(&node)
        .expect("peer-A must have a score entry after block delivery");
    assert!(
        score > 0.0,
        "score should be positive after a successful block delivery, got {score}"
    );
}

/// `DontHave` for an unsolicited block must not penalise the peer.
/// Only attributed when there is a pending request from us.
#[test]
fn unsolicited_donthave_is_neutral() {
    let mut engine = BitswapEngine::new();
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    engine = engine.with_reputation(reporter.clone());

    let peer_id = "peer-B";
    engine.add_peer(peer_id).unwrap();

    let block = hash(b"never-requested");
    // No pending request — DontHave is unsolicited.
    engine.process_message(
        peer_id,
        BitswapMessage::DontHave {
            block: block.clone(),
        },
    );

    let node = blake3_node(peer_id);
    let score = reporter.table().score(&node).unwrap_or(0.0);
    assert_eq!(
        score, 0.0,
        "unsolicited DontHave must not move the score (got {score})"
    );
}

/// `DontHave` after a real outbound request must drive the score
/// down — the peer is responsive but doesn't have what we want.
/// Outbound requests are tracked via `PeerState::start_request`
/// (the `process_message` path serves incoming `WantBlock`s).
#[test]
fn donthave_after_want_penalises_peer() {
    let mut engine = BitswapEngine::new();
    let table = PeerScoreTable::new(ReputationParams::default());
    let reporter = ReputationReporter::in_memory(table);
    engine = engine.with_reputation(reporter.clone());

    let peer_id = "peer-C";
    engine.add_peer(peer_id).unwrap();

    let block = hash(b"wanted-block");
    // Mark a pending outbound request to peer-C for this block.
    engine.start_request_for_peer(peer_id, &block);
    // Now the peer responds with DontHave.
    engine.process_message(
        peer_id,
        BitswapMessage::DontHave {
            block: block.clone(),
        },
    );

    let node = blake3_node(peer_id);
    let score = reporter
        .table()
        .score(&node)
        .expect("peer-C must have a score entry after DontHave");
    assert!(
        score < 0.0,
        "score should be negative after a wanted DontHave, got {score}"
    );
}

/// Without a reputation hook the engine must still work — the
/// `BitswapSignal` path is strictly opt-in.
#[test]
fn engine_without_reputation_still_works() {
    let mut engine = BitswapEngine::new();
    engine.add_peer("peer-X").unwrap();
    let block = hash(b"unhooked-block");
    let responses = engine.process_message(
        "peer-X",
        BitswapMessage::Block {
            block: block.clone(),
            data: b"data".to_vec(),
        },
    );
    // No responses because we don't have the block.
    assert!(responses.is_empty());
}

/// `with_reputation` must be idempotent: a second call replaces
/// the reporter (so callers can swap reporters in tests).
#[test]
fn with_reputation_replaces_existing_reporter() {
    let table1 = PeerScoreTable::new(ReputationParams::default());
    let table2 = PeerScoreTable::new(ReputationParams::default());
    let rep1 = ReputationReporter::in_memory(table1);
    let rep2 = ReputationReporter::in_memory(table2);

    let mut engine = BitswapEngine::new().with_reputation(rep1).with_reputation(rep2.clone());
    engine.add_peer("peer-Y").unwrap();
    let block = hash(b"replace-test");
    engine.process_message(
        "peer-Y",
        BitswapMessage::Block {
            block: block.clone(),
            data: b"x".to_vec(),
        },
    );
    // Only the second reporter should have an entry.
    let node = blake3_node("peer-Y");
    assert!(rep2.table().score(&node).is_some());
}

/// Map a bitswap peer-id string to the deterministic `NodeId` the
/// engine uses internally. Mirrors the private helper.
fn blake3_node(peer_id: &str) -> a3net_types::NodeId {
    let h = blake3::hash(peer_id.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&h.as_bytes()[..32]);
    a3net_types::NodeId::from_bytes(&bytes).expect("32 bytes")
}