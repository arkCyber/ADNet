//! Ergonomic adapters for cross-subsystem reputation signals.
//!
//! Each subsystem (gossip, bitswap, pairing, chat, manual) calls
//! into a [`ReputationReporter`]. The reporter is a thin facade
//! over a [`PeerScoreTable`] (and optionally a [`ReputationStore`])
//! that turns subsystem-specific signals into the unified
//! [`ReputationEvent`] model.
//!
//! The reporters are kept **as small as possible** — they must
//! not pull heavy dependencies into the call site. They live here
//! (and not in `a3net-gossip` / `a3net-blobstore`) so the call
//! sites don't have to depend on each other.

use std::sync::Arc;

use a3net_types::NodeId;
use tracing::trace;

use crate::event::{InvalidReason, ReputationDelta, ReputationEvent};
use crate::metrics::ReputationMetrics;
use crate::params::ReportKind;
use crate::score::PeerScoreTable;
use crate::store::ReputationStore;

/// Bundle of all the things a subsystem needs to feed reputation.
#[derive(Clone)]
pub struct ReputationReporter {
    table: PeerScoreTable,
    store: Option<ReputationStore>,
    metrics: Option<ReputationMetrics>,
}

impl std::fmt::Debug for ReputationReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReputationReporter")
            .field("has_store", &self.store.is_some())
            .field("has_metrics", &self.metrics.is_some())
            .finish()
    }
}

impl ReputationReporter {
    /// Construct a reporter that operates only on the in-memory
    /// table (no persistence, no metrics). Useful in tests.
    pub fn in_memory(table: PeerScoreTable) -> Self {
        Self { table, store: None, metrics: None }
    }

    /// Construct a reporter that persists to `store` and records
    /// metrics.
    pub fn persistent(
        table: PeerScoreTable,
        store: ReputationStore,
        metrics: ReputationMetrics,
    ) -> Self {
        Self { table, store: Some(store), metrics: Some(metrics) }
    }

    /// Borrow the underlying table.
    pub fn table(&self) -> &PeerScoreTable {
        &self.table
    }

    /// Apply an event end-to-end: score → persist → metric.
    pub fn record(&self, event: ReputationEvent) -> crate::error::ReputationResult<()> {
        trace!(target: "a3net_reputation", event = %event.kind_tag(), peer = %event.peer().short(), "recording");
        if let Some(ref store) = self.store {
            store.apply(event)?;
        } else {
            self.table.apply(event)?;
        }
        if let Some(ref m) = self.metrics {
            // event payload is consumed by the table / store path
            // already; we only have the tag here for the counter.
            m.event_total.inc();
        }
        Ok(())
    }

    /// Apply and return the delta. Useful for callers that want to
    /// inspect the score change.
    pub fn record_with_delta(
        &self,
        event: ReputationEvent,
    ) -> crate::error::ReputationResult<ReputationDelta> {
        let delta = if let Some(ref store) = self.store {
            store.apply(event)?
        } else {
            self.table.apply(event)?
        };
        if let Some(ref m) = self.metrics {
            m.event_total.inc();
        }
        Ok(delta)
    }
}

/// Convenience constructors for the gossip layer.
#[derive(Debug, Clone)]
pub struct GossipSignal<'a>(pub &'a ReputationReporter);

impl<'a> GossipSignal<'a> {
    /// A well-formed message was received.
    pub fn valid(&self, peer: NodeId, topic: Option<a3net_types::Topic>, size_bytes: u32) {
        let _ = self.0.record(ReputationEvent::ValidMessage {
            peer,
            topic,
            size_bytes,
        });
    }

    /// A message failed validation.
    pub fn invalid(
        &self,
        peer: NodeId,
        topic: Option<a3net_types::Topic>,
        reason: InvalidReason,
    ) {
        let _ = self.0.record(ReputationEvent::InvalidMessage {
            peer,
            topic,
            reason,
        });
    }

    /// First-delivery of a message id.
    pub fn first_delivery(&self, peer: NodeId, topic: Option<a3net_types::Topic>) {
        let _ = self.0.record(ReputationEvent::FirstMessageDelivery {
            peer,
            topic,
        });
    }

    /// Mesh delivery (gossip-forwarded to subscribers).
    pub fn mesh_delivery(&self, peer: NodeId, topic: Option<a3net_types::Topic>) {
        let _ = self.0.record(ReputationEvent::MeshMessageDelivery {
            peer,
            topic,
        });
    }

    /// Duplicate message id.
    pub fn duplicate(&self, peer: NodeId, topic: Option<a3net_types::Topic>) {
        let _ = self.0.record(ReputationEvent::DuplicateMessage {
            peer,
            topic,
        });
    }

    /// RTT exceeded the threshold.
    pub fn slow(&self, peer: NodeId, rtt_ms: u32, threshold_ms: u32) {
        let _ = self.0.record(ReputationEvent::SlowPeer {
            peer,
            rtt_ms,
            threshold_ms,
        });
    }
}

/// Convenience constructors for the bitswap layer.
#[derive(Debug, Clone)]
pub struct BitswapSignal<'a>(pub &'a ReputationReporter);

impl<'a> BitswapSignal<'a> {
    /// Bitswap request succeeded with the given payload size.
    pub fn valid(&self, peer: NodeId, size_bytes: u32) {
        let _ = self.0.record(ReputationEvent::ValidMessage {
            peer,
            topic: None,
            size_bytes,
        });
    }

    /// Bitswap request was malformed.
    pub fn invalid(&self, peer: NodeId, reason: InvalidReason) {
        let _ = self.0.record(ReputationEvent::InvalidMessage {
            peer,
            topic: None,
            reason,
        });
    }

    /// Peer did not respond to a want.
    pub fn slow(&self, peer: NodeId, rtt_ms: u32) {
        let _ = self.0.record(ReputationEvent::SlowPeer {
            peer,
            rtt_ms,
            threshold_ms: rtt_ms.saturating_mul(2),
        });
    }
}

/// Convenience constructors for the pairing layer.
#[derive(Debug, Clone)]
pub struct PairingSignal<'a>(pub &'a ReputationReporter);

impl<'a> PairingSignal<'a> {
    /// A pairing ceremony completed successfully.
    pub fn established(&self, peer: NodeId, credential_id_short: String) {
        let _ = self.0.record(ReputationEvent::PairingEstablished {
            peer,
            credential_id_short,
        });
    }

    /// A paired device was revoked.
    pub fn revoked(&self, peer: NodeId, credential_id_short: String) {
        let _ = self.0.record(ReputationEvent::PairingRevoked {
            peer,
            credential_id_short,
        });
    }
}

/// Convenience constructors for the chat-trust layer. Chat-trust is
/// the **user-attributed** signal that the global PeerScore fuses
/// with gossip / bitswap evidence; every write to the
/// `chat_trust` table should also call into this facade so the
/// score can move immediately.
#[derive(Debug, Clone)]
pub struct ChatSignal<'a>(pub &'a ReputationReporter);

impl<'a> ChatSignal<'a> {
    /// Record a user setting their trust level for `peer` to
    /// `level ∈ [-3, +3]`. Translates to a
    /// [`ReputationEvent::ChatTrustSet`] event with the configured
    /// weight (`params.trust_weight(level)`).
    ///
    /// `by_user` is a local numeric id for the issuing user (the
    /// chat store maps each user to a stable u64; we don't need the
    /// full identity at this layer because chat trust is owned by
    /// the chat store, not by reputation).
    pub fn set_trust(&self, peer: NodeId, by_user: u64, level: i8) {
        let _ = self.0.record(ReputationEvent::ChatTrustSet {
            peer,
            by_user,
            level,
        });
    }

    /// Record a user filing a moderation report against `peer`.
    /// Translates to a [`ReputationEvent::ChatTrustReport`] event.
    pub fn report(&self, peer: NodeId, by_user: u64, report: ReportKind) {
        let _ = self.0.record(ReputationEvent::ChatTrustReport {
            peer,
            by_user,
            report,
        });
    }
}

// `ReportKind` is re-exported under an alias so external crates can
// talk about "chat reports" without reaching into `params`.
pub use crate::params::ReportKind as ChatReportKind;

/// Single entry-point that returns the reporter wrapped in all
/// four adapter structs. Equivalent to constructing them by hand
/// but more ergonomic at the call site.
pub fn signals(
    reporter: &ReputationReporter,
) -> (
    GossipSignal<'_>,
    BitswapSignal<'_>,
    PairingSignal<'_>,
    ChatSignal<'_>,
) {
    (
        GossipSignal(reporter),
        BitswapSignal(reporter),
        PairingSignal(reporter),
        ChatSignal(reporter),
    )
}

/// Lift a [`ReputationReporter`] into an `Arc` for shared
/// ownership across subsystems.
pub fn shared(reporter: ReputationReporter) -> Arc<ReputationReporter> {
    Arc::new(reporter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ReputationParams;

    fn reporter() -> ReputationReporter {
        ReputationReporter::in_memory(PeerScoreTable::new(ReputationParams::default()))
    }

    #[test]
    fn gossip_valid_is_positive() {
        let r = reporter();
        let p = NodeId::random();
        GossipSignal(&r).valid(p.clone(), None, 1024);
        assert!(r.table().score(&p).unwrap() > 0.0);
    }

    #[test]
    fn bitswap_invalid_is_negative() {
        let r = reporter();
        let p = NodeId::random();
        BitswapSignal(&r).invalid(p.clone(), InvalidReason::BadSignature);
        assert!(r.table().score(&p).unwrap() < 0.0);
    }

    #[test]
    fn pairing_round_trip() {
        let r = reporter();
        let p = NodeId::random();
        PairingSignal(&r).established(p.clone(), "abcd".into());
        let s1 = r.table().score(&p).unwrap();
        PairingSignal(&r).revoked(p.clone(), "abcd".into());
        let s2 = r.table().score(&p).unwrap();
        assert!(s1 > 0.0);
        assert!(s2 < 0.0);
    }

    #[test]
    fn signals_helper_returns_all_four() {
        let r = reporter();
        let (g, b, p, c) = signals(&r);
        let peer = NodeId::random();
        g.valid(peer.clone(), None, 1024);
        b.valid(peer.clone(), 2048);
        p.established(peer.clone(), "abcd".into());
        c.set_trust(peer.clone(), 1, 2);
        assert!(r.table().score(&peer).unwrap() > 0.0);
    }

    #[test]
    fn chat_signal_set_trust_moves_score() {
        let r = reporter();
        let peer = NodeId::random();
        ChatSignal(&r).set_trust(peer.clone(), 7, -3);
        let after_neg = r.table().score(&peer).unwrap();
        ChatSignal(&r).set_trust(peer.clone(), 7, 3);
        let after_pos = r.table().score(&peer).unwrap();
        assert!(
            after_neg < 0.0,
            "level=-3 should drive the score negative (got {after_neg})"
        );
        assert!(
            after_pos > after_neg,
            "level=+3 should drive the score above the level=-3 baseline \
             (got neg={after_neg}, pos={after_pos})"
        );
    }

    #[test]
    fn chat_signal_report_moves_score_down() {
        let r = reporter();
        let peer = NodeId::random();
        ChatSignal(&r).report(peer.clone(), 9, ReportKind::Spam);
        assert!(r.table().score(&peer).unwrap() < 0.0);
    }
}
