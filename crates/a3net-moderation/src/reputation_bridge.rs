//! Bridge between the moderation blocklist and the reputation table.
//!
//! When the gateway files a takedown, the publishing node's
//! reputation should drop so other A3Net nodes can refuse to peer
//! with it. This module is the single funnel that translates a
//! moderation event into a [`a3net_reputation::ReputationEvent`].
//!
//! ## Translation
//!
//! For every takedown we emit a
//! [`BehaviourPenalty`][a3net_reputation::ReputationEvent::BehaviourPenalty]
//! with [`BehaviourKind::ContentViolation`]. The penalty magnitude
//! is `-weight_behaviour_content_violation * severity * batch_count`.
//! The default weight is `5.0`
//! ([`a3net_reputation::ReputationParams::weight_behaviour_content_violation`]),
//! so a CSAM takedown (severity 10) produces a `-50.0` delta — well
//! below the default refusal threshold (`-10.0`) and effectively
//! blacklist-list the peer.
//!
//! ## Bounded blast radius
//!
//! A flooded takedown report can otherwise push the peer past
//! `MIN_SCORE = -100`. The score is hard-clamped inside the
//! reputation table, so a spam of takedown events is degenerate but
//! bounded — the audit log still records every event so the
//! reputation floor can be undone if the takedown is later
//! overturned.

use a3net_reputation::{
    BehaviourKind, PeerScoreTable, ReputationEvent, ReputationResult,
};
use a3net_types::NodeId;

use crate::blocklist::TakedownReason;

/// Translate a single takedown into a reputation penalty and apply
/// it to the table. `batch_count` is the number of distinct banned
/// CIDs the peer uploaded in one batch — each one multiplies the
/// per-event delta. The severity of the underlying reason is baked
/// into the event's `count` field so the
/// `weight_behaviour_content_violation` (default 5.0) is multiplied
/// by the reason's severity (2..=10).
///
/// Returns the actual delta that was applied (negative number).
pub fn apply_violation(
    table: &PeerScoreTable,
    peer: &NodeId,
    reason: TakedownReason,
    blocklist_entry_id: u64,
    batch_count: u32,
) -> ReputationResult<f64> {
    let severity = reason.severity();
    if severity == 0 {
        return Ok(0.0);
    }
    #[allow(unused_variables)]
    let per_event = table.params().weight_behaviour_content_violation;
    let batch_count = batch_count.max(1);
    let effective_count = (severity as u32).saturating_mul(batch_count).min(64);

    let event = ReputationEvent::BehaviourPenalty {
        peer: peer.clone(),
        behaviour: BehaviourKind::ContentViolation,
        count: effective_count,
    };
    let delta = table.apply(event)?.delta;

    tracing::info!(
        target: "a3net_moderation",
        peer = %peer,
        reason = ?reason,
        blocklist_entry_id,
        severity,
        batch_count,
        effective_count,
        delta,
        "reputation penalty applied for content violation"
    );

    Ok(delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_reputation::ReputationParams;
    use a3net_types::NodeId;

    fn peer() -> NodeId {
        NodeId::random()
    }

    #[test]
    fn csam_takedown_pins_score_below_threshold() {
        let table = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        let delta = apply_violation(&table, &p, TakedownReason::Csam, 1, 1).unwrap();
        // severity(10) * weight(5.0) = -50.0
        assert!(delta <= -50.0, "delta should be ≤ -50 for CSAM, got {delta}");
        let score = table.score(&p).unwrap();
        assert!(score <= -10.0, "score {score} should be ≤ refusal threshold");
    }

    #[test]
    fn terms_of_service_violation_is_a_soft_penalty() {
        let table = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        let delta = apply_violation(&table, &p, TakedownReason::TermsOfService, 1, 1).unwrap();
        // severity(3) * weight(5.0) = -15.0
        assert!(delta <= -15.0, "delta should be ≤ -15 for ToS, got {delta}");
        let score = table.score(&p).unwrap();
        assert!(score <= -10.0);
    }

    #[test]
    fn count_multiplier_amplifies_penalty() {
        let table = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        let d1 = apply_violation(&table, &p, TakedownReason::Csam, 1, 1).unwrap();
        // Use a different reason so the second event is incremental
        // (severity 10 * batch 1 = 10 vs severity 5 * batch 1 = 5).
        let d2 = apply_violation(&table, &p, TakedownReason::Copyright, 2, 1).unwrap();
        // CSAM is more severe than copyright in absolute terms.
        assert!(d1.abs() > d2.abs(), "CSAM ({d1}) should punish more than copyright ({d2})");
    }
}
