//! Chat-side trust levels and fusion with the global PeerScore.
//!
//! Trust in chat is a **per-user** signal: user A may consider
//! user B a friend while user C considers them a harasser. The
//! global [`crate::score::PeerScore`] is a per-peer signal shared
//! across all subsystems. [`TrustFusion`] reconciles the two:
//!
//! - chat-side trust levels are written to the `chat_trust` table
//!   in [`adnet-chatstore`] (see `TrustStore`).
//! - chat trust levels also produce [`crate::event::ReputationEvent`]
//!   entries that influence the global score.
//! - The fused value (chat trust × global score) is what the chat
//!   layer should consult when deciding whether to relay, push
//!   notifications, or accept invites from a peer.

use std::time::{SystemTime, UNIX_EPOCH};

use adnet_types::NodeId;
use serde::{Deserialize, Serialize};

use crate::score::PeerScoreTable;

/// Discrete trust levels. Mapping is preserved on disk so older
/// `chat_trust` rows are decoded correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrustLevel {
    /// Strong trust — paired device, family, or self-vouched.
    Trusted = 3,
    /// Friend.
    Friend = 2,
    /// Known contact.
    Known = 1,
    /// Neutral / unknown.
    Neutral = 0,
    /// Exercise caution.
    Caution = -1,
    /// Untrusted — drop non-essential traffic.
    Untrusted = -2,
    /// Blocked — refuse all interaction.
    Blocked = -3,
}

impl TrustLevel {
    /// Build from a raw `i8` in `[-3, +3]`. Out-of-range values
    /// clamp. `None` if the input is `0` and you want to express
    /// "no entry", use [`TrustLevel::Neutral`] explicitly instead.
    pub fn from_i8(v: i8) -> Self {
        match v {
            3 => Self::Trusted,
            2 => Self::Friend,
            1 => Self::Known,
            0 => Self::Neutral,
            -1 => Self::Caution,
            -2 => Self::Untrusted,
            -3 => Self::Blocked,
            v if v > 3 => Self::Trusted,
            _ => Self::Blocked,
        }
    }

    /// Numeric tag in `[-3, +3]`.
    pub fn as_i8(self) -> i8 {
        self as i8
    }

    /// Human-readable label for UI / logs.
    pub fn label(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Friend => "friend",
            Self::Known => "known",
            Self::Neutral => "neutral",
            Self::Caution => "caution",
            Self::Untrusted => "untrusted",
            Self::Blocked => "blocked",
        }
    }

    /// Should chat refuse to relay messages from this level?
    pub fn is_blocked(self) -> bool {
        matches!(self, Self::Blocked)
    }
}

impl Default for TrustLevel {
    fn default() -> Self {
        Self::Neutral
    }
}

/// Default half-life (in hours) of a chat-trust signal before it
/// starts decaying. Two days is the typical Signal/Delta-Chat
/// policy.
pub const DEFAULT_TRUST_HALFLIFE_HOURS: u64 = 48;

/// A single chat-trust signal. Produced by
/// [`crate::event::ReputationEvent::ChatTrustSet`] and
/// [`crate::event::ReputationEvent::ChatTrustReport`] and also
/// stored in the `chat_trust` table by the chat layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustSignal {
    /// The user issuing the trust signal.
    pub by_user: u64,
    /// The peer being trusted / distrusted.
    pub peer: NodeId,
    /// The trust level after the signal.
    pub level: TrustLevel,
    /// Optional reason for debugging / UI display.
    pub notes: Option<String>,
    /// Unix seconds.
    pub updated_unix: i64,
}

impl TrustSignal {
    /// Build a new signal with the current time stamp.
    pub fn new(by_user: u64, peer: NodeId, level: TrustLevel, notes: Option<String>) -> Self {
        Self {
            by_user,
            peer,
            level,
            notes,
            updated_unix: unix_now(),
        }
    }

    /// Age of this signal in hours.
    pub fn age_hours(&self) -> u64 {
        let now = unix_now();
        ((now - self.updated_unix).max(0) as u64) / 3600
    }

    /// Decay weight based on age, using an exponential half-life.
    /// Returns `1.0` for fresh signals, `0.5` after one half-life,
    /// `0.25` after two, etc.
    pub fn decay_factor(&self, half_life_hours: u64) -> f64 {
        if half_life_hours == 0 {
            return 1.0;
        }
        let age = self.age_hours();
        0.5_f64.powf((age as f64) / (half_life_hours as f64))
    }
}

/// Combine chat-side trust and global PeerScore into a single
/// decision value.
///
/// The fused value lives in `[-1.0, +1.0]`:
///
/// - `fused = sign(chat_trust) * chat_weight
///          + sign(global_score) * global_weight`
///
/// with each weight normalised by the maximum expected magnitude.
/// Callers compare the fused value against thresholds; the exact
/// thresholds are deliberately not exported from this module — the
/// chat layer owns its policy.
#[derive(Debug, Clone)]
pub struct TrustFusion {
    chat_weight: f64,
    global_weight: f64,
}

impl Default for TrustFusion {
    fn default() -> Self {
        Self {
            // Chat trust dominates because it carries user intent;
            // global score is a safety net.
            chat_weight: 0.7,
            global_weight: 0.3,
        }
    }
}

impl TrustFusion {
    /// Construct with custom weights. Both must be in `[0, 1]` and
    /// sum to `1.0`; otherwise defaults are used.
    pub fn new(chat_weight: f64, global_weight: f64) -> Self {
        let ok = (0.0..=1.0).contains(&chat_weight)
            && (0.0..=1.0).contains(&global_weight)
            && ((chat_weight + global_weight) - 1.0).abs() < 1e-6;
        if ok {
            Self { chat_weight, global_weight }
        } else {
            Self::default()
        }
    }

    /// Fuse a chat trust signal with the global score into a
    /// single decision value.
    ///
    /// - `global_score` should be the [`crate::score::PeerScoreTable`]
    ///   score, already in `[-100, +100]`. The fusion normalises
    ///   it to `[-1, +1]` using `tanh`-like scaling (linear for
    ///   `|x| < 50`, sign-clip beyond).
    /// - `chat_signal` is optional — if `None`, only the global
    ///   signal contributes.
    pub fn fused(&self, global_score: f64, chat_signal: Option<&TrustSignal>) -> f64 {
        let global_norm = normalise(global_score);
        let global_component = global_norm * self.global_weight;

        let chat_component = match chat_signal {
            Some(sig) => {
                let decay = sig.decay_factor(DEFAULT_TRUST_HALFLIFE_HOURS);
                let lv = sig.level.as_i8() as f64 / 3.0; // [-1, +1]
                lv * decay * self.chat_weight
            }
            None => 0.0,
        };

        (global_component + chat_component).clamp(-1.0, 1.0)
    }

    /// Convenience: compute the fused value directly from a
    /// [`PeerScoreTable`] and a chat signal.
    pub fn fused_with(
        &self,
        table: &PeerScoreTable,
        chat_signal: Option<&TrustSignal>,
    ) -> f64 {
        let global = chat_signal
            .as_ref()
            .map(|s| table.score(&s.peer).unwrap_or(0.0))
            .unwrap_or(0.0);
        self.fused(global, chat_signal)
    }

    /// Return `true` if the fused value is below the refusal
    /// threshold.
    pub fn should_refuse(&self, fused: f64, refusal: f64) -> bool {
        fused <= refusal
    }
}

fn normalise(score: f64) -> f64 {
    // Map `[-100, +100]` to `[-1, +1]`. Use linear scaling for
    // small magnitudes and a soft clip for outliers.
    let x = score / 50.0;
    x.clamp(-1.5, 1.5).tanh()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ReputationEvent;
    use crate::params::ReputationParams;

    #[test]
    fn level_round_trip() {
        for v in -3..=3 {
            let lv = TrustLevel::from_i8(v);
            assert_eq!(lv.as_i8(), v);
        }
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(TrustLevel::Trusted.label(), "trusted");
        assert_eq!(TrustLevel::Blocked.label(), "blocked");
    }

    #[test]
    fn out_of_range_clamps() {
        assert_eq!(TrustLevel::from_i8(99), TrustLevel::Trusted);
        assert_eq!(TrustLevel::from_i8(-99), TrustLevel::Blocked);
    }

    #[test]
    fn decay_halves_at_halflife() {
        let mut sig = TrustSignal::new(1, NodeId::random(), TrustLevel::Friend, None);
        // Pretend it was issued 48 h ago.
        sig.updated_unix -= 48 * 3600;
        let d = sig.decay_factor(DEFAULT_TRUST_HALFLIFE_HOURS);
        assert!((d - 0.5).abs() < 1e-6, "half-life should yield 0.5 (got {d})");
    }

    #[test]
    fn fusion_clamps_to_unit() {
        let f = TrustFusion::default();
        let v = f.fused(10_000.0, None);
        assert!(v <= 1.0 && v >= -1.0);
    }

    #[test]
    fn fusion_combines_signals() {
        let f = TrustFusion::default();
        let sig = TrustSignal::new(1, NodeId::random(), TrustLevel::Trusted, None);
        let with_chat = f.fused(0.0, Some(&sig));
        let without_chat = f.fused(0.0, None);
        assert!(with_chat > without_chat);
    }

    #[test]
    fn blocked_signal_drives_refusal() {
        let f = TrustFusion::default();
        let sig = TrustSignal::new(1, NodeId::random(), TrustLevel::Blocked, None);
        let v = f.fused(0.0, Some(&sig));
        assert!(v < 0.0);
        assert!(f.should_refuse(v, 0.0));
    }

    #[test]
    fn fused_with_uses_table() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let peer = NodeId::random();
        t.apply(ReputationEvent::PairingEstablished {
            peer: peer.clone(),
            credential_id_short: "abcd".into(),
        })
        .unwrap();
        let sig = TrustSignal::new(1, peer.clone(), TrustLevel::Trusted, None);
        let f = TrustFusion::default();
        let v = f.fused_with(&t, Some(&sig));
        assert!(v > 0.5, "trusted + pairing should be strongly positive (got {v})");
    }

    #[test]
    fn bad_weights_fall_back_to_default() {
        let f = TrustFusion::new(0.5, 0.0); // doesn't sum to 1
        // Should fall back to defaults (0.7/0.3).
        assert!((f.chat_weight - 0.7).abs() < 1e-6);
    }
}
