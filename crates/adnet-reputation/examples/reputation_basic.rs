//! Tiny example: build a `PeerScoreTable`, apply a few events, and read
//! the per-peer score.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-reputation --example reputation_basic
//! ```

use adnet_reputation::{PeerScoreTable, ReputationEvent, ReputationParams};
use adnet_types::NodeId;

fn main() {
    let table = PeerScoreTable::new(ReputationParams::default());

    let good_peer = NodeId::random();
    let bad_peer = NodeId::random();

    // 1. A few valid messages from `good_peer` push the score into
    //    positive territory.
    for _ in 0..3 {
        let _ = table.apply(ReputationEvent::ValidMessage {
            peer: good_peer.clone(),
            topic: None,
            size_bytes: 1024,
        });
    }

    // 2. Invalid messages from `bad_peer` push the score negative.
    for _ in 0..4 {
        let _ = table.apply(ReputationEvent::InvalidMessage {
            peer: bad_peer.clone(),
            topic: None,
            reason: adnet_reputation::InvalidReason::BadSignature,
        });
    }

    let g = table.score(&good_peer).unwrap_or_default();
    let b = table.score(&bad_peer).unwrap_or_default();
    println!("good_peer ({}): {g:.3}", &good_peer.short());
    println!("bad_peer  ({}): {b:.3}", &bad_peer.short());
    assert!(g > 0.0);
    assert!(b < 0.0);
}
