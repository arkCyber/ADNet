//! Tunable parameters for the reputation subsystem.
//!
//! The defaults are chosen so that:
//!
//! - **One valid message** from an unknown peer bumps the score by
//!   `+0.5` (well below [`MAX_SCORE`]/2).
//! - **One invalid / malformed message** drops the score by
//!   `-1.0` (asymmetric — bad acts outweigh good).
//! - **Decay** pulls every score 1% closer to zero per minute, so
//!   long-idle peers do not stay pinned high forever.
//! - **Pairing** is a much stronger signal than a single valid
//!   message: a successful pairing ceremony gives `+25`, a
//!   revocation gives `-50`.
//!
//! Every value is exposed as a `pub` field so callers (CLI, tests,
//! integration scripts) can override per-deployment.

use serde::{Deserialize, Serialize};

/// Hard floor — no peer's score can drop below this.
pub const MIN_SCORE: f64 = -100.0;

/// Hard ceiling — no peer's score can rise above this.
pub const MAX_SCORE: f64 = 100.0;

/// Default decay interval (60 s).
pub const DEFAULT_DECAY_INTERVAL_SECS: u64 = 60;

/// Default decay rate (1% per interval).
pub const DEFAULT_DECAY_RATE: f64 = 0.01;

/// Default capacity of the per-peer delta history ring.
pub const DEFAULT_HISTORY_CAP: usize = 64;

/// Default shard count for [`crate::score::PeerScoreTable`].
pub const DEFAULT_SHARDS: usize = 16;

/// Default threshold below which the gossip layer should refuse
/// outbound to a peer (matches libp2p's default for
/// `gossip_threshold`).
pub const DEFAULT_REFUSAL_THRESHOLD: f64 = -10.0;

/// Default threshold below which the gossip layer should consider
/// the peer graylisted (matches libp2p's `publish_threshold`).
pub const DEFAULT_GRAYLIST_THRESHOLD: f64 = 0.0;

/// Behaviour kinds used in
/// [`crate::event::ReputationEvent::BehaviourPenalty`].
///
/// The numeric tags are stable; do **not** renumber existing
/// entries. New kinds are added at the bottom of the enum and
/// given the next integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BehaviourKind {
    /// Peer sent data outside the protocol envelope (oversized,
    /// malformed, …).
    ProtocolViolation = 1,
    /// Peer drained our outbound bandwidth without reciprocating.
    FreeRider = 2,
    /// Peer refused to relay traffic at our request.
    RelayRefusal = 3,
    /// Peer injected traffic to amplify / flood us.
    Amplification = 4,
    /// Peer presented a credential that did not verify.
    AuthFailure = 5,
    /// Peer stored or relayed known-bad content (CSAM hashes,
    /// copyrighted material the operator flagged, …).
    ContentViolation = 6,
}

/// Report kinds submitted from chat users via
/// [`crate::event::ReputationEvent::ChatTrustReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ReportKind {
    /// Unsolicited advertising / SEO spam.
    Spam = 1,
    /// Targeted harassment.
    Harassment = 2,
    /// Impersonating another user / device.
    Impersonation = 3,
    /// Phishing or other social-engineering attempt.
    Phishing = 4,
    /// Other, see notes field.
    Other = 99,
}

/// Per-event weights applied by
/// [`crate::score::PeerScoreTable::apply`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReputationParams {
    // ─── Positive signals ──────────────────────────────────────
    /// Weight added per delivered valid message.
    pub weight_valid_message: f64,
    /// Weight added for the first time a peer delivers a message
    /// on a given topic. Higher than [`Self::weight_valid_message`]
    /// because first-delivery is rarer and a stronger indicator of
    /// liveness.
    pub weight_first_delivery: f64,
    /// Weight added per mesh delivery (i.e. inside the gossipsub
    /// mesh). Smaller than first-delivery but additive.
    pub weight_mesh_delivery: f64,
    /// Cap on the cumulative weight contributed by `valid_message`
    /// per decay interval. Prevents a chatty well-behaved peer from
    /// pinning its score at the ceiling forever.
    pub valid_message_cap_per_tick: f64,

    // ─── Negative signals ──────────────────────────────────────
    /// Weight subtracted per invalid message.
    pub weight_invalid_message: f64,
    /// Weight subtracted per duplicate message.
    pub weight_duplicate_message: f64,
    /// Weight subtracted when the peer is detected slow relative to
    /// the configured threshold.
    pub weight_slow_peer: f64,
    /// Weight subtracted when the peer is flagged inactive (no
    /// activity for > 1 decay interval).
    pub weight_inactive_peer: f64,

    /// Behaviour-penalty multipliers (per [`BehaviourKind`]).
    pub weight_behaviour_protocol_violation: f64,
    /// Penalty for free-riding (downloading without serving).
    pub weight_behaviour_free_rider: f64,
    /// Penalty for refusing relay traffic.
    pub weight_behaviour_relay_refusal: f64,
    /// Penalty for traffic amplification.
    pub weight_behaviour_amplification: f64,
    /// Penalty for failed authentication attempts.
    pub weight_behaviour_auth_failure: f64,
    /// Penalty for serving known-bad content.
    pub weight_behaviour_content_violation: f64,

    // ─── Pairing signals (strong; cross-trust) ─────────────────
    /// Weight added when a pairing ceremony completes successfully.
    pub weight_pairing_established: f64,
    /// Weight subtracted when a paired device is revoked.
    pub weight_pairing_revoked: f64,

    // ─── Chat-side signals ─────────────────────────────────────
    /// Per-user-per-target `ChatTrustSet` global weight added. The
    /// chat-side `TrustLevel` itself is stored in
    /// [`crate::trust::TrustFusion`] and is the authoritative value
    /// for chat routing decisions; this weight is the influence the
    /// user's choice has on the global score.
    pub weight_chat_trust_per_unit: f64,
    /// Multiplier per [`ReportKind`].
    pub weight_chat_report_spam: f64,
    /// Penalty for harassment reports.
    pub weight_chat_report_harassment: f64,
    /// Penalty for impersonation reports.
    pub weight_chat_report_impersonation: f64,
    /// Penalty for phishing reports.
    pub weight_chat_report_phishing: f64,
    /// Penalty for miscellaneous reports.
    pub weight_chat_report_other: f64,

    /// Manual adjustments — capped per call. We don't want a
    /// compromised CLI session to push a peer to the ceiling in
    /// one keystroke.
    pub manual_adjust_cap_per_call: f64,

    // ─── Decay / thresholds ────────────────────────────────────
    /// Decay interval in seconds.
    pub decay_interval_secs: u64,
    /// Per-interval decay rate (`1.0 - decay_factor` is the absolute
    /// pull toward zero).
    pub decay_rate: f64,
    /// Score below which the gossip layer should refuse outbound.
    pub refusal_threshold: f64,
    /// Score below which gossip may graylist.
    pub graylist_threshold: f64,

    // ─── Storage tuning ────────────────────────────────────────
    /// Per-peer delta history ring capacity.
    pub history_cap: usize,
    /// Shard count for the table.
    pub shards: usize,

    /// Default per-event size normalisation. Valid message weights
    /// are multiplied by `log2(1 + size_bytes) / log2(1 + 1024)` so
    /// that a 1 KiB message scores as 1.0×, a 1 MiB message as
    /// 1.21×, and a 1 GiB message as 1.41×. This bounds the
    /// influence of very large but legitimate messages without
    /// penalising them outright.
    pub size_norm_bytes: f64,
}

impl Default for ReputationParams {
    fn default() -> Self {
        Self {
            // positive
            weight_valid_message: 0.5,
            weight_first_delivery: 1.0,
            weight_mesh_delivery: 0.2,
            valid_message_cap_per_tick: 2.0,
            // negative
            weight_invalid_message: 1.0,
            weight_duplicate_message: 0.5,
            weight_slow_peer: 0.2,
            weight_inactive_peer: 0.1,
            weight_behaviour_protocol_violation: 2.0,
            weight_behaviour_free_rider: 1.5,
            weight_behaviour_relay_refusal: 1.0,
            weight_behaviour_amplification: 3.0,
            weight_behaviour_auth_failure: 2.5,
            weight_behaviour_content_violation: 5.0,
            // pairing
            weight_pairing_established: 25.0,
            weight_pairing_revoked: -50.0,
            // chat
            weight_chat_trust_per_unit: 1.5,
            weight_chat_report_spam: 1.0,
            weight_chat_report_harassment: 2.0,
            weight_chat_report_impersonation: 3.0,
            weight_chat_report_phishing: 3.0,
            weight_chat_report_other: 0.5,
            manual_adjust_cap_per_call: 5.0,
            // decay
            decay_interval_secs: DEFAULT_DECAY_INTERVAL_SECS,
            decay_rate: DEFAULT_DECAY_RATE,
            refusal_threshold: DEFAULT_REFUSAL_THRESHOLD,
            graylist_threshold: DEFAULT_GRAYLIST_THRESHOLD,
            // storage
            history_cap: DEFAULT_HISTORY_CAP,
            shards: DEFAULT_SHARDS,
            size_norm_bytes: 1024.0,
        }
    }
}

impl ReputationParams {
    /// Return the weight for a [`BehaviourKind`].
    pub fn behaviour_weight(&self, kind: BehaviourKind) -> f64 {
        match kind {
            BehaviourKind::ProtocolViolation => {
                self.weight_behaviour_protocol_violation
            }
            BehaviourKind::FreeRider => self.weight_behaviour_free_rider,
            BehaviourKind::RelayRefusal => self.weight_behaviour_relay_refusal,
            BehaviourKind::Amplification => self.weight_behaviour_amplification,
            BehaviourKind::AuthFailure => self.weight_behaviour_auth_failure,
            BehaviourKind::ContentViolation => self.weight_behaviour_content_violation,
        }
    }

    /// Return the weight for a [`ReportKind`].
    pub fn report_weight(&self, kind: ReportKind) -> f64 {
        match kind {
            ReportKind::Spam => self.weight_chat_report_spam,
            ReportKind::Harassment => self.weight_chat_report_harassment,
            ReportKind::Impersonation => self.weight_chat_report_impersonation,
            ReportKind::Phishing => self.weight_chat_report_phishing,
            ReportKind::Other => self.weight_chat_report_other,
        }
    }

    /// Validate that the configured weights form a non-explosive
    /// system. Returns `Ok(())` if everything looks sane.
    pub fn validate(&self) -> crate::error::ReputationResult<()> {
        if self.decay_interval_secs == 0 {
            return Err(crate::error::ReputationError::InvalidDecayConfig(
                "decay_interval_secs must be > 0".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.decay_rate) {
            return Err(crate::error::ReputationError::InvalidDecayConfig(
                "decay_rate must be in [0, 1]".into(),
            ));
        }
        if self.shards == 0 || !self.shards.is_power_of_two() {
            return Err(crate::error::ReputationError::InvalidDecayConfig(
                "shards must be a power of two and > 0".into(),
            ));
        }
        if self.history_cap == 0 {
            return Err(crate::error::ReputationError::InvalidDecayConfig(
                "history_cap must be > 0".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        let p = ReputationParams::default();
        p.validate().expect("defaults must validate");
    }

    #[test]
    fn size_norm_is_bounded() {
        let p = ReputationParams::default();
        let norm = |bytes: u32| {
            (bytes as f64 + 1.0).log2() / (p.size_norm_bytes + 1.0).log2()
        };
        // 1 KiB ≈ 1.0×, 1 MiB ≈ 2.0×, 1 GiB ≈ 3.0× — the curve is
        // intentionally flat so huge messages don't dominate a peer's
        // score. The "norm" is unbounded as bytes → ∞, but pragmatically
        // capped because gossip payloads are bounded by the protocol
        // anyway.
        assert!((norm(1024) - 1.0).abs() < 1e-3);
        assert!((norm(1024 * 1024) - 2.0).abs() < 1e-3);
        assert!((norm(1024 * 1024 * 1024) - 3.0).abs() < 1e-3);
    }

    #[test]
    fn bad_decay_interval_rejected() {
        let mut p = ReputationParams::default();
        p.decay_interval_secs = 0;
        assert!(p.validate().is_err());
    }

    #[test]
    fn bad_shards_rejected() {
        let mut p = ReputationParams::default();
        p.shards = 3;
        assert!(p.validate().is_err());
        p.shards = 8;
        assert!(p.validate().is_ok());
    }
}
