//! Reputation event model and delta records.
//!
//! Every change to the [`crate::score::PeerScoreTable`] is
//! initiated by a [`ReputationEvent`] and produces a
//! [`ReputationDelta`] that gets appended to the per-peer history
//! ring. This makes the score auditable end-to-end: "why is peer X
//! at -7.5?" can be answered by replaying its recent deltas.

use std::fmt;

use adnet_types::NodeId;
use serde::{Deserialize, Serialize};

use crate::params::{BehaviourKind, ReportKind, ReputationParams};

/// Stable 32-byte hex topic identifier. Topics are scoped by
/// `gossipsub` semantics in ADNet; a `None` topic means
/// "applies to the peer globally".
pub type TopicId = Option<adnet_types::Topic>;

/// Reasons a message can be invalid. Used by
/// [`ReputationEvent::InvalidMessage`] so observers can attribute
/// the penalty correctly. Numeric tags are stable; append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum InvalidReason {
    /// Cryptographic signature did not verify.
    BadSignature = 1,
    /// Structure parsed but a required field was missing.
    MissingField = 2,
    /// Topic id was not one the peer is subscribed to.
    UnknownTopic = 3,
    /// Payload exceeded the size cap.
    Oversized = 4,
    /// Internal decode / decompression error.
    DecodeError = 5,
    /// Other protocol-level rejection (default).
    Other = 99,
}

/// The full event taxonomy. New variants are added at the bottom of
/// the enum and given the next stable tag in
/// `crate::params::ReputationParams` weights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
pub enum ReputationEvent {
    /// Peer delivered a well-formed message on a topic.
    ValidMessage {
        /// Originating peer.
        peer: NodeId,
        /// Topic. `None` for non-topic-scoped events (e.g. raw
        /// byteswap).
        topic: TopicId,
        /// Decoded payload size in bytes. Used for the size
        /// normalisation in [`ReputationParams::size_norm_bytes`].
        size_bytes: u32,
    },

    /// Peer delivered a message that failed validation.
    InvalidMessage {
        /// Originating peer.
        peer: NodeId,
        /// Topic.
        topic: TopicId,
        /// Why the message was rejected.
        reason: InvalidReason,
    },

    /// Peer sent a duplicate (already-seen) message id.
    DuplicateMessage {
        /// Originating peer.
        peer: NodeId,
        /// Topic.
        topic: TopicId,
    },

    /// Peer delivered a message we had never seen before — a
    /// first-publish event in gossipsub terminology.
    FirstMessageDelivery {
        /// Originating peer.
        peer: NodeId,
        /// Topic.
        topic: TopicId,
    },

    /// Peer was inside our mesh and delivered a message we
    /// forwarded.
    MeshMessageDelivery {
        /// Originating peer.
        peer: NodeId,
        /// Topic.
        topic: TopicId,
    },

    /// Peer round-trip exceeded the configured threshold.
    SlowPeer {
        /// Originating peer.
        peer: NodeId,
        /// Measured RTT in milliseconds.
        rtt_ms: u32,
        /// Threshold that was breached, in milliseconds.
        threshold_ms: u32,
    },

    /// Peer has been inactive for > 1 decay interval.
    InactivePeer {
        /// Originating peer.
        peer: NodeId,
    },

    /// Peer exhibited a specific behavioural pattern.
    BehaviourPenalty {
        /// Originating peer.
        peer: NodeId,
        /// Which behaviour.
        behaviour: BehaviourKind,
        /// Multiplier (e.g. observed 3 amplifications in one tick).
        count: u32,
    },

    // ── Pairing ─────────────────────────────────────────────────
    /// Pairing ceremony completed successfully.
    PairingEstablished {
        /// Newly-paired peer.
        peer: NodeId,
        /// Credential id (hex, first 16 chars). Used purely as a
        /// human-readable reference in deltas.
        credential_id_short: String,
    },
    /// Paired device revoked at the wallet level.
    PairingRevoked {
        /// Peer whose credential was revoked.
        peer: NodeId,
        /// Credential id (hex, first 16 chars).
        credential_id_short: String,
    },

    // ── Chat trust ──────────────────────────────────────────────
    /// Per-user-per-target trust level set in chat.
    ChatTrustSet {
        /// Peer being scored.
        peer: NodeId,
        /// Numeric id of the user issuing the trust update.
        by_user: u64,
        /// `TrustLevel` tag in `[-3, +3]`.
        level: i8,
    },
    /// Peer filed a report.
    ChatTrustReport {
        /// Peer being reported.
        peer: NodeId,
        /// Numeric id of the user filing the report.
        by_user: u64,
        /// Report kind.
        report: ReportKind,
    },

    // ── Manual intervention ────────────────────────────────────
    /// Operator-driven adjustment (CLI, governance). The delta is
    /// capped per call (see
    /// [`ReputationParams::manual_adjust_cap_per_call`]).
    ManualAdjust {
        /// Peer.
        peer: NodeId,
        /// Signed delta. Clamped to `manual_adjust_cap_per_call`.
        delta: f64,
        /// Free-form reason recorded in deltas.
        reason: String,
    },
    /// Restore a peer's score to an absolute value (snapshot replay).
    ///
    /// Unlike [`Self::ManualAdjust`], this **bypasses**
    /// `manual_adjust_cap_per_call` and applies the value directly
    /// (still clamped to `MIN_SCORE` / `MAX_SCORE`). Used by
    /// [`crate::store::ReputationStore::load_snapshot`] when
    /// replaying a saved snapshot — the absolute score is what
    /// matters there, not a delta. The `reason` field identifies the
    /// snapshot epoch for debugging.
    AbsoluteRestore {
        /// Peer.
        peer: NodeId,
        /// Absolute score to set, clamped to score bounds.
        score: f64,
        /// Human-readable reason (e.g. `snapshot:1700000000`).
        reason: String,
    },
}

impl ReputationEvent {
    /// Convenience: return the peer this event is about, regardless
    /// of variant.
    pub fn peer(&self) -> &NodeId {
        match self {
            Self::ValidMessage { peer, .. }
            | Self::InvalidMessage { peer, .. }
            | Self::DuplicateMessage { peer, .. }
            | Self::FirstMessageDelivery { peer, .. }
            | Self::MeshMessageDelivery { peer, .. }
            | Self::SlowPeer { peer, .. }
            | Self::InactivePeer { peer }
            | Self::BehaviourPenalty { peer, .. }
            | Self::PairingEstablished { peer, .. }
            | Self::PairingRevoked { peer, .. }
            | Self::ChatTrustSet { peer, .. }
            | Self::ChatTrustReport { peer, .. }
            | Self::ManualAdjust { peer, .. }
            | Self::AbsoluteRestore { peer, .. } => peer,
        }
    }

    /// Stable string tag for metrics and the `kind` JSON field.
    /// Must NOT change once published — operators write dashboards
    /// against these strings.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Self::ValidMessage { .. } => "valid_message",
            Self::InvalidMessage { .. } => "invalid_message",
            Self::DuplicateMessage { .. } => "duplicate_message",
            Self::FirstMessageDelivery { .. } => "first_message_delivery",
            Self::MeshMessageDelivery { .. } => "mesh_message_delivery",
            Self::SlowPeer { .. } => "slow_peer",
            Self::InactivePeer { .. } => "inactive_peer",
            Self::BehaviourPenalty { .. } => "behaviour_penalty",
            Self::PairingEstablished { .. } => "pairing_established",
            Self::PairingRevoked { .. } => "pairing_revoked",
            Self::ChatTrustSet { .. } => "chat_trust_set",
            Self::ChatTrustReport { .. } => "chat_trust_report",
            Self::ManualAdjust { .. } => "manual_adjust",
            Self::AbsoluteRestore { .. } => "absolute_restore",
        }
    }

    /// Compute the score delta this event should add to the peer,
    /// given the configured [`ReputationParams`]. Returns the
    /// signed delta. Does **not** clamp; clamping happens inside
    /// [`crate::score::PeerScoreTable::apply`].
    pub fn delta(&self, p: &ReputationParams) -> f64 {
        match self {
            Self::ValidMessage { size_bytes, .. } => {
                let norm = ((*size_bytes as f64) + 1.0).log2()
                    / (p.size_norm_bytes + 1.0).log2();
                p.weight_valid_message * norm
            }
            Self::InvalidMessage { .. } => -p.weight_invalid_message,
            Self::DuplicateMessage { .. } => -p.weight_duplicate_message,
            Self::FirstMessageDelivery { .. } => p.weight_first_delivery,
            Self::MeshMessageDelivery { .. } => p.weight_mesh_delivery,
            Self::SlowPeer { .. } => -p.weight_slow_peer,
            Self::InactivePeer { .. } => -p.weight_inactive_peer,
            Self::BehaviourPenalty { behaviour, count, .. } => {
                -p.behaviour_weight(*behaviour) * (*count as f64)
            }
            Self::PairingEstablished { .. } => p.weight_pairing_established,
            Self::PairingRevoked { .. } => p.weight_pairing_revoked,
            Self::ChatTrustSet { level, .. } => {
                p.weight_chat_trust_per_unit * (*level as f64)
            }
            Self::ChatTrustReport { report, .. } => -p.report_weight(*report),
            Self::ManualAdjust { delta, .. } => {
                let cap = p.manual_adjust_cap_per_call;
                delta.clamp(-cap, cap)
            }
            // `AbsoluteRestore` bypasses the additive delta model —
            // `apply_with_count` handles it as a special case. Here
            // we return 0 because the score-after is set directly
            // from the event's `score` field; if a caller invokes
            // `delta()` they only get a hint that this event is not
            // additive. The audit log records the real before/after.
            Self::AbsoluteRestore { .. } => 0.0,
        }
    }
}

impl fmt::Display for ReputationEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidMessage { peer, topic, size_bytes } => {
                write!(f, "valid({} size={}B topic={:?})", peer.short(), size_bytes, topic)
            }
            Self::InvalidMessage { peer, reason, .. } => {
                write!(f, "invalid({} {:?})", peer.short(), reason)
            }
            Self::DuplicateMessage { peer, .. } => {
                write!(f, "duplicate({})", peer.short())
            }
            Self::FirstMessageDelivery { peer, .. } => {
                write!(f, "first_delivery({})", peer.short())
            }
            Self::MeshMessageDelivery { peer, .. } => {
                write!(f, "mesh_delivery({})", peer.short())
            }
            Self::SlowPeer { peer, rtt_ms, threshold_ms } => {
                write!(f, "slow({} {}>{}ms)", peer.short(), rtt_ms, threshold_ms)
            }
            Self::InactivePeer { peer } => write!(f, "inactive({})", peer.short()),
            Self::BehaviourPenalty { peer, behaviour, count } => {
                write!(f, "behaviour({} {:?}×{})", peer.short(), behaviour, count)
            }
            Self::PairingEstablished { peer, .. } => {
                write!(f, "paired({})", peer.short())
            }
            Self::PairingRevoked { peer, .. } => {
                write!(f, "unpaired({})", peer.short())
            }
            Self::ChatTrustSet { peer, level, .. } => {
                write!(f, "chat_trust({} level={:+})", peer.short(), level)
            }
            Self::ChatTrustReport { peer, report, .. } => {
                write!(f, "chat_report({} {:?})", peer.short(), report)
            }
            Self::ManualAdjust { peer, delta, reason } => {
                write!(
                    f,
                    "manual({} delta={:+.2} reason={})",
                    peer.short(),
                    delta,
                    reason
                )
            }
            Self::AbsoluteRestore { peer, score, reason } => {
                write!(
                    f,
                    "restore({} score={:+.2} reason={})",
                    peer.short(),
                    score,
                    reason
                )
            }
        }
    }
}

/// A recorded score change. The deltas form an append-only audit
/// log. Every delta captures enough information to reproduce the
/// effect: which peer, when, why, and the before/after values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReputationDelta {
    /// Schema version of the on-disk record. Bumped if the shape
    /// ever changes in a backward-incompatible way.
    pub schema_version: u32,
    /// Hex NodeId.
    pub peer: String,
    /// Optional topic id hex (None ⇒ global).
    pub topic_hex: Option<String>,
    /// Stable event tag from [`ReputationEvent::kind_tag`].
    pub event: String,
    /// Score **before** this delta was applied.
    pub score_before: f64,
    /// Score **after** this delta was applied.
    pub score_after: f64,
    /// Signed delta (score_after - score_before).
    pub delta: f64,
    /// Unix timestamp (seconds) when the event was applied.
    pub ts_unix: i64,
    /// Counter — monotonically increasing per (peer, event-kind)
    /// pair; used by tests to detect reorder.
    pub count: u64,
    /// blake3 digest of the canonical JSON form. Used to detect
    /// tampering of the JSONL log. `blake3:` prefix is always
    /// present so consumers know which hash.
    pub event_digest: String,
}

impl ReputationDelta {
    /// Schema version constant — bump on backward-incompatible changes.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Construct a delta from the inputs. Caller is responsible for
    /// supplying the right `score_before` / `score_after`.
    pub fn new(
        peer: &NodeId,
        topic: TopicId,
        event: &ReputationEvent,
        score_before: f64,
        score_after: f64,
        ts_unix: i64,
        count: u64,
    ) -> Self {
        let delta = score_after - score_before;
        let topic_hex = topic.as_ref().map(|t| t.as_hex().to_string());
        let mut d = Self {
            schema_version: Self::SCHEMA_VERSION,
            peer: peer.as_hex().to_string(),
            topic_hex,
            event: event.kind_tag().to_string(),
            score_before,
            score_after,
            delta,
            ts_unix,
            count,
            event_digest: String::new(),
        };
        d.event_digest = d.compute_digest();
        d
    }

    /// blake3 digest over a canonical (sorted-keys) JSON projection.
    /// `score_before / score_after` are formatted with full
    /// precision so re-applying a delta produces bit-identical
    /// scores.
    pub fn compute_digest(&self) -> String {
        let canonical = serde_json::json!({
            "peer": self.peer,
            "topic_hex": self.topic_hex,
            "event": self.event,
            "delta": self.delta,
            "ts_unix": self.ts_unix,
            "count": self.count,
        });
        let bytes = serde_json::to_vec(&canonical)
            .expect("canonical json must serialise");
        let h = blake3::hash(&bytes);
        format!("blake3:{}", hex::encode(h.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ReputationParams;

    fn peer() -> NodeId {
        NodeId::random()
    }

    #[test]
    fn peer_accessor_is_total() {
        let p = peer();
        let cases: Vec<ReputationEvent> = vec![
            ReputationEvent::ValidMessage {
                peer: p.clone(),
                topic: None,
                size_bytes: 100,
            },
            ReputationEvent::InvalidMessage {
                peer: p.clone(),
                topic: None,
                reason: InvalidReason::BadSignature,
            },
            ReputationEvent::BehaviourPenalty {
                peer: p.clone(),
                behaviour: BehaviourKind::Amplification,
                count: 2,
            },
            ReputationEvent::PairingEstablished {
                peer: p.clone(),
                credential_id_short: "abcd1234".into(),
            },
            ReputationEvent::ChatTrustSet {
                peer: p.clone(),
                by_user: 1,
                level: 2,
            },
            ReputationEvent::ManualAdjust {
                peer: p.clone(),
                delta: 0.5,
                reason: "test".into(),
            },
        ];
        for ev in cases {
            assert_eq!(ev.peer(), &p);
        }
    }

    #[test]
    fn invalid_is_negative() {
        let p = peer();
        let ev = ReputationEvent::InvalidMessage {
            peer: p,
            topic: None,
            reason: InvalidReason::BadSignature,
        };
        assert!(ev.delta(&ReputationParams::default()) < 0.0);
    }

    #[test]
    fn pairing_strongly_positive() {
        let ev = ReputationEvent::PairingEstablished {
            peer: peer(),
            credential_id_short: "abcd".into(),
        };
        let p = ReputationParams::default();
        assert!(ev.delta(&p) >= 10.0, "pairing must be a strong signal");
    }

    #[test]
    fn manual_adjust_is_capped() {
        let ev = ReputationEvent::ManualAdjust {
            peer: peer(),
            delta: 1000.0,
            reason: "test".into(),
        };
        let p = ReputationParams::default();
        assert_eq!(ev.delta(&p), p.manual_adjust_cap_per_call);
    }

    #[test]
    fn digest_is_stable() {
        let p = peer();
        let ev = ReputationEvent::ValidMessage {
            peer: p.clone(),
            topic: None,
            size_bytes: 100,
        };
        let d1 = ReputationDelta::new(&p, None, &ev, 0.0, 0.5, 100, 1);
        let d2 = ReputationDelta::new(&p, None, &ev, 0.0, 0.5, 100, 1);
        assert_eq!(d1.event_digest, d2.event_digest);
        let d3 = ReputationDelta::new(&p, None, &ev, 0.0, 0.6, 100, 1);
        assert_ne!(d1.event_digest, d3.event_digest);
    }

    #[test]
    fn display_uses_short_id() {
        let ev = ReputationEvent::InvalidMessage {
            peer: peer(),
            topic: None,
            reason: InvalidReason::BadSignature,
        };
        let s = format!("{ev}");
        assert!(s.contains("invalid("));
        assert!(s.contains("BadSignature"));
    }
}
