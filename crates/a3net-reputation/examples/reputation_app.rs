//! Realistic example: simulate a gossip mesh where most peers behave
//! well and one peer starts spamming invalid messages. The reputation
//! table is the same the gossip layer would maintain; we read the
//! score back to decide whether to keep the misbehaving peer in the
//! mesh.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-reputation --example reputation_app
//! ```

use a3net_reputation::{
    BitswapSignal, InvalidReason, MAX_SCORE, MIN_SCORE, PeerScoreTable, ReputationEvent,
    ReputationParams, ReputationReporter,
};
use a3net_types::NodeId;

fn main() {
    let params = ReputationParams::default();
    let table = PeerScoreTable::new(params);
    let reporter = ReputationReporter::in_memory(table);

    let well_behaved: Vec<NodeId> = (0..4).map(|_| NodeId::random()).collect();
    let spammer = NodeId::random();

    // 1. Well-behaved peers each contribute a few valid messages via
    //    the bitswap signal facade.
    let bitswap = BitswapSignal(&reporter);
    for peer in &well_behaved {
        for _ in 0..3 {
            let _ = bitswap.valid(peer.clone(), 2048);
        }
    }

    // 2. The spammer floods the bus with invalid messages — the
    //    penalty weights are tuned by `ReputationParams`.
    for _ in 0..20 {
        let _ = bitswap.invalid(spammer.clone(), InvalidReason::Oversized);
    }

    // 3. Read back the scores and decide membership.
    let mut keep = Vec::new();
    let mut drop = Vec::new();
    for peer in &well_behaved {
        let s = reporter.table().score(peer).unwrap_or(0.0);
        if s >= 0.0 {
            keep.push((peer, s));
        } else {
            drop.push((peer, s));
        }
    }
    let spammer_score = reporter.table().score(&spammer).unwrap_or(0.0);
    if spammer_score < 0.0 {
        drop.push((&spammer, spammer_score));
    } else {
        keep.push((&spammer, spammer_score));
    }

    println!("acceptable peers (score >= 0):");
    for (p, s) in &keep {
        println!("  {}  score={:.3}", p.short(), s);
    }
    println!("\npeers to drop (score < 0):");
    for (p, s) in &drop {
        println!("  {}  score={:.3}", p.short(), s);
    }

    let avg_well = well_behaved
        .iter()
        .map(|p| reporter.table().score(p).unwrap_or(0.0))
        .sum::<f64>() / well_behaved.len() as f64;
    println!("\nwell-behaved average: {avg_well:.3}");
    println!("spammer:              {spammer_score:.3}");
    println!("score bounds:         [{MIN_SCORE}, {MAX_SCORE}]");

    assert!(avg_well > 0.0);
    assert!(spammer_score < 0.0);

    // 4. A direct `ReputationEvent` (e.g. one that doesn't have a
    //    signal facade yet) is also accepted.
    let new_spammer = NodeId::random();
    let _ = reporter.record(ReputationEvent::InvalidMessage {
        peer: new_spammer.clone(),
        topic: None,
        reason: InvalidReason::BadSignature,
    });
    println!("\nafter direct event, new_spammer score: {:.3}",
        reporter.table().score(&new_spammer).unwrap_or(0.0));
}
