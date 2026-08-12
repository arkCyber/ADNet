//! Per-peer scoring table.
//!
//! The table is sharded by `blake3(peer_id) % N` so concurrent
//! gossip writes can hit different shards in parallel without
//! blocking each other. Read-side counts as a "scan all shards"
//! under the hood (still O(peers), not O(messages), and the shards
//! are bounded so a snapshot stays cheap).
//!
//! ## Decoupling score from rate
//!
//! A [`PeerScore`] is the **cumulative** signal: "how much do we
//! trust this peer overall?" — bounded by [`MIN_SCORE`] and
//! [`MAX_SCORE`].
//!
//! A [`TopicScore`] is the per-topic contribution — also bounded,
//! and additionally subject to per-topic mesh counters that
//! gossipsub semantics care about.

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use adnet_types::NodeId;
use indexmap::IndexMap;
use parking_lot::RwLock;

use crate::error::{ReputationError, ReputationResult};
use crate::event::{ReputationDelta, ReputationEvent, TopicId};
use crate::params::{ReputationParams, MAX_SCORE, MIN_SCORE};

/// Shard index — opaque to callers but exposed in the public API
/// for callers that want to do their own shard walks (e.g. test
/// harnesses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShardIndex(pub usize);

/// Per-peer cumulative score.
#[derive(Debug, Clone)]
pub struct PeerScore {
    /// Current score, clamped to `[MIN_SCORE, MAX_SCORE]`.
    pub score: f64,
    /// Unix timestamp (seconds) of the last `apply`.
    pub last_updated_unix: i64,
    /// Unix timestamp (seconds) of the last decay tick applied.
    pub last_decayed_unix: i64,
    /// Count of positive events seen (for diagnostics).
    pub positive_count: u64,
    /// Count of negative events seen.
    pub negative_count: u64,
    /// Recent deltas — ring buffer of `params.history_cap` items.
    pub history: Vec<ReputationDelta>,
}

impl Default for PeerScore {
    fn default() -> Self {
        let now = unix_now();
        Self {
            score: 0.0,
            last_updated_unix: now,
            last_decayed_unix: now,
            positive_count: 0,
            negative_count: 0,
            history: Vec::new(),
        }
    }
}

impl PeerScore {
    /// Construct with a specific starting score.
    pub fn with_initial_score(initial: f64, now_unix: i64, cap: usize) -> Self {
        Self {
            score: initial.clamp(MIN_SCORE, MAX_SCORE),
            last_updated_unix: now_unix,
            last_decayed_unix: now_unix,
            positive_count: 0,
            negative_count: 0,
            history: Vec::with_capacity(cap.min(64)),
        }
    }

    /// Push a delta into the history ring, evicting the oldest.
    pub fn push_delta(&mut self, delta: ReputationDelta, cap: usize) {
        if self.history.len() >= cap {
            let drop_n = self.history.len() + 1 - cap;
            self.history.drain(0..drop_n);
        }
        if delta.delta >= 0.0 {
            self.positive_count = self.positive_count.saturating_add(1);
        } else {
            self.negative_count = self.negative_count.saturating_add(1);
        }
        self.history.push(delta);
    }
}

/// Per-topic counters — used by gossipsub-style code paths.
#[derive(Debug, Clone, Default)]
pub struct TopicScore {
    /// Topic-specific score, clamped like the global score.
    pub score: f64,
    /// Times this peer was the first to deliver a message on this
    /// topic.
    pub first_deliveries: u64,
    /// Times this peer delivered inside the mesh.
    pub mesh_deliveries: u64,
    /// Times this peer delivered an invalid message on this topic.
    pub invalid_messages: u64,
    /// Times this peer delivered a duplicate.
    pub duplicate_messages: u64,
}

impl TopicScore {
    fn apply(&mut self, event: &ReputationEvent, params: &ReputationParams) -> f64 {
        let delta = event.delta(params);
        match event {
            ReputationEvent::FirstMessageDelivery { .. } => {
                self.first_deliveries = self.first_deliveries.saturating_add(1);
            }
            ReputationEvent::MeshMessageDelivery { .. } => {
                self.mesh_deliveries = self.mesh_deliveries.saturating_add(1);
            }
            ReputationEvent::InvalidMessage { .. } => {
                self.invalid_messages = self.invalid_messages.saturating_add(1);
            }
            ReputationEvent::DuplicateMessage { .. } => {
                self.duplicate_messages = self.duplicate_messages.saturating_add(1);
            }
            _ => {}
        }
        let before = self.score;
        let after = (before + delta).clamp(MIN_SCORE, MAX_SCORE);
        self.score = after;
        after - before
    }
}

/// Per-peer shard entry: the cumulative [`PeerScore`] plus an
/// index of topics that contributed to it.
#[derive(Debug, Clone)]
struct PeerEntry {
    peer_score: PeerScore,
    /// IndexMap preserves insertion order so iteration is stable
    /// in snapshots / debugging.
    topic_scores: IndexMap<String, TopicScore>,
}

impl PeerEntry {
    fn new(now_unix: i64, history_cap: usize) -> Self {
        Self {
            peer_score: PeerScore::with_initial_score(0.0, now_unix, history_cap),
            topic_scores: IndexMap::new(),
        }
    }
}

/// One shard of the table — guards both the per-peer scores and
/// the per-topic scores for its slice of peers.
#[derive(Debug)]
struct Shard {
    peers: RwLock<IndexMap<String, PeerEntry>>,
}

impl Shard {
    fn new() -> Self {
        Self { peers: RwLock::new(IndexMap::new()) }
    }
}

/// Thread-safe sharded reputation table.
#[derive(Debug, Clone)]
pub struct PeerScoreTable {
    inner: Arc<Shards>,
    params: Arc<ReputationParams>,
}

struct Shards {
    shards: Vec<Shard>,
    mask: usize,
}

impl fmt::Debug for Shards {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shards")
            .field("count", &self.shards.len())
            .finish()
    }
}

impl PeerScoreTable {
    /// Construct a new table using the supplied parameters.
    pub fn new(params: ReputationParams) -> Self {
        params.validate().expect("params must validate before construction");
        let n = params.shards;
        let shards = (0..n).map(|_| Shard::new()).collect::<Vec<_>>();
        Self {
            inner: Arc::new(Shards {
                shards,
                mask: n - 1,
            }),
            params: Arc::new(params),
        }
    }

    /// Borrow the active parameters.
    pub fn params(&self) -> &ReputationParams {
        &self.params
    }

    /// Borrow the active NodeId → global score mapping. Used by
    /// the gossip layer to decide whether to send to a peer.
    pub fn score(&self, peer: &NodeId) -> Option<f64> {
        let (shard_idx, _) = self.shard_for(peer);
        let shards = self.inner.shards[shard_idx].peers.read();
        shards.get(peer.as_hex()).map(|e| e.peer_score.score)
    }

    /// Return a snapshot of every (peer → score) pair. Cost: O(N)
    /// where N = total tracked peers. Used by [`crate::store`]
    /// snapshots and the CLI `reputation show` command.
    pub fn snapshot(&self) -> ScoreSnapshot {
        let mut out: Vec<(NodeId, f64)> = Vec::new();
        for shard in &self.inner.shards {
            let map = shard.peers.read();
            for (hex, entry) in map.iter() {
                let _ = hex; // already used for lookup
                if let Ok(node) = NodeId::from_hex(hex) {
                    out.push((node, entry.peer_score.score));
                }
            }
        }
        out.sort_by(|a, b| a.0.as_hex().cmp(b.0.as_hex()));
        ScoreSnapshot {
            params: self.params.as_ref().clone(),
            scores: out,
            unix_now: unix_now(),
        }
    }

    /// Return the number of peers currently tracked. O(shards).
    pub fn peer_count(&self) -> usize {
        self.inner
            .shards
            .iter()
            .map(|s| s.peers.read().len())
            .sum()
    }

    /// Apply a single [`ReputationEvent`]. Returns the
    /// [`ReputationDelta`] that was recorded (for tests, audit
    /// streams, and the persistence layer). Returns `Err` if the
    /// event produces a non-finite score (programming error).
    pub fn apply(&self, event: ReputationEvent) -> ReputationResult<ReputationDelta> {
        self.apply_with_count(event, None)
    }

    /// Apply an event with an explicit monotonically-increasing
    /// counter — used by [`crate::store`] during replay so the
    /// delta log is bit-identical to the original run.
    pub fn apply_with_count(
        &self,
        event: ReputationEvent,
        explicit_count: Option<u64>,
    ) -> ReputationResult<ReputationDelta> {
        let peer = event.peer().clone();
        let topic = event.topic();
        let now = unix_now();

        let (shard_idx, key) = self.shard_for(&peer);
        let mut shard = self.inner.shards[shard_idx].peers.write();
        let entry = shard
            .entry(key)
            .or_insert_with(|| PeerEntry::new(now, self.params.history_cap));

        let before = entry.peer_score.score;
        // `AbsoluteRestore` sets the absolute score, bypassing both
        // the manual-adjust cap and the `before + event_delta`
        // formula. The score-after value is the clamped target,
        // recorded into the delta so the audit trail shows the
        // intended value.
        if let ReputationEvent::AbsoluteRestore { score, .. } = &event {
            let target = score.clamp(MIN_SCORE, MAX_SCORE);
            entry.peer_score.score = target;
            entry.peer_score.last_updated_unix = now;
            let count = explicit_count.unwrap_or_else(|| {
                entry
                    .peer_score
                    .positive_count
                    .saturating_add(entry.peer_score.negative_count)
                    .saturating_add(1)
            });
            let delta = ReputationDelta::new(&peer, topic, &event, before, target, now, count);
            entry.peer_score.push_delta(delta.clone(), self.params.history_cap);
            return Ok(delta);
        }

        let topic_delta = match topic.as_ref() {
            Some(t) => {
                let topic_key = t.as_hex().to_string();
                let ts = entry
                    .topic_scores
                    .entry(topic_key)
                    .or_insert_with(TopicScore::default);
                ts.apply(&event, &self.params)
            }
            None => 0.0,
        };
        // Peer-level delta = event delta (already includes size
        // normalisation, weights, …). Topic delta is the *change in
        // the per-topic score* — separate signal that does not
        // double-count into the peer.
        let event_delta = event.delta(&self.params);
        let after = (before + event_delta).clamp(MIN_SCORE, MAX_SCORE);

        if !after.is_finite() {
            return Err(ReputationError::NonFiniteScore {
                peer: peer.as_hex().to_string(),
                event: event.kind_tag().to_string(),
            });
        }

        entry.peer_score.score = after;
        entry.peer_score.last_updated_unix = now;

        let count = explicit_count.unwrap_or_else(|| {
            entry
                .peer_score
                .positive_count
                .saturating_add(entry.peer_score.negative_count)
                .saturating_add(1)
        });

        let delta = ReputationDelta::new(&peer, topic, &event, before, after, now, count);
        entry.peer_score.push_delta(delta.clone(), self.params.history_cap);
        let _ = topic_delta; // recorded in TopicScore counters; not persisted per-topic in v1
        Ok(delta)
    }

    /// Apply a decay tick. Walks every shard; updates
    /// `last_decayed_unix` on each peer entry. Returns the number
    /// of peers decayed.
    pub fn decay_tick(&self) -> usize {
        let rate = self.params.decay_rate;
        if rate <= 0.0 {
            return 0;
        }
        let factor = 1.0 - rate.clamp(0.0, 1.0);
        let now = unix_now();
        let mut touched = 0usize;
        for shard in &self.inner.shards {
            let mut map = shard.peers.write();
            for entry in map.values_mut() {
                let before = entry.peer_score.score;
                if before.abs() < f64::EPSILON {
                    continue;
                }
                let after = (before * factor).clamp(MIN_SCORE, MAX_SCORE);
                entry.peer_score.score = after;
                entry.peer_score.last_decayed_unix = now;
                touched += 1;
            }
        }
        touched
    }

    /// Manual reset for a peer — used by the CLI's `reputation
    /// restore` command. Returns `true` if the peer existed.
    pub fn reset(&self, peer: &NodeId) -> bool {
        let (idx, key) = self.shard_for(peer);
        let mut map = self.inner.shards[idx].peers.write();
        map.swap_remove(&key).is_some()
    }

    /// Set a peer's score to an absolute value (snapshot replay).
    ///
    /// Bypasses the per-event cap that [`Self::apply`] would impose
    /// on `ManualAdjust`; instead the value is clamped to
    /// [`MIN_SCORE`] / [`MAX_SCORE`]. Returns the new (clamped)
    /// score, or `None` if the peer slot was newly created (caller
    /// can decide whether that's a fresh apply or a snapshot
    /// restore of a never-seen peer).
    ///
    /// Used by [`crate::store::ReputationStore::load_snapshot`] —
    /// see [`crate::event::ReputationEvent::AbsoluteRestore`] for
    /// the public-event counterpart which goes through `apply`.
    pub fn set_score(&self, peer: &NodeId, score: f64) -> f64 {
        let clamped = score.clamp(MIN_SCORE, MAX_SCORE);
        let now = unix_now();
        let (idx, key) = self.shard_for(peer);
        let mut map = self.inner.shards[idx].peers.write();
        let entry = map
            .entry(key)
            .or_insert_with(|| PeerEntry::new(now, self.params.history_cap));
        entry.peer_score.score = clamped;
        entry.peer_score.last_updated_unix = now;
        clamped
    }

    fn shard_for(&self, peer: &NodeId) -> (usize, String) {
        let h = blake3::hash(peer.as_hex().as_bytes());
        let bytes = h.as_bytes();
        // Use first 8 bytes as a u64; mod by power-of-two mask.
        let raw = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let idx = (raw as usize) & self.inner.mask;
        (idx, peer.as_hex().to_string())
    }
}

/// Opaque snapshot of the table — used by the persistence layer
/// and the CLI's `show` command.
#[derive(Debug, Clone)]
pub struct ScoreSnapshot {
    /// Parameters in effect when the snapshot was taken.
    pub params: ReputationParams,
    /// `(NodeId, score)` pairs sorted by NodeId.
    pub scores: Vec<(NodeId, f64)>,
    /// Unix time when the snapshot was captured.
    pub unix_now: i64,
}

impl ReputationEvent {
    /// Extract the optional topic from an event. Used by the table
    /// when deciding which TopicScore to update.
    pub(crate) fn topic(&self) -> TopicId {
        match self {
            Self::ValidMessage { topic, .. }
            | Self::InvalidMessage { topic, .. }
            | Self::DuplicateMessage { topic, .. }
            | Self::FirstMessageDelivery { topic, .. }
            | Self::MeshMessageDelivery { topic, .. } => topic.clone(),
            _ => None,
        }
    }
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
    use crate::event::InvalidReason;

    fn peer() -> NodeId {
        NodeId::random()
    }

    #[test]
    fn empty_score_is_zero() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        assert_eq!(t.score(&p), None);
    }

    #[test]
    fn valid_message_is_positive() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        t.apply(ReputationEvent::ValidMessage {
            peer: p.clone(),
            topic: None,
            size_bytes: 1024,
        })
        .unwrap();
        assert!(t.score(&p).unwrap() > 0.0);
    }

    #[test]
    fn invalid_is_asymmetric() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        t.apply(ReputationEvent::InvalidMessage {
            peer: p.clone(),
            topic: None,
            reason: InvalidReason::BadSignature,
        })
        .unwrap();
        // default weight_invalid_message = 1.0 ⇒ -1.0
        assert!(t.score(&p).unwrap() < -0.5);
    }

    #[test]
    fn clamping_holds() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        for _ in 0..1000 {
            t.apply(ReputationEvent::ValidMessage {
                peer: p.clone(),
                topic: None,
                size_bytes: 1024,
            })
            .unwrap();
        }
        assert!(t.score(&p).unwrap() <= MAX_SCORE);
    }

    #[test]
    fn decay_tick_pulls_toward_zero() {
        let t = PeerScoreTable::new(ReputationParams {
            decay_rate: 0.5, // aggressive
            ..Default::default()
        });
        let p = peer();
        t.apply(ReputationEvent::ValidMessage {
            peer: p.clone(),
            topic: None,
            size_bytes: 1024,
        })
        .unwrap();
        let before = t.score(&p).unwrap();
        t.decay_tick();
        let after = t.score(&p).unwrap();
        assert!(after < before);
        assert!(after > 0.0);
    }

    #[test]
    fn decay_does_nothing_at_zero() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        let n = t.decay_tick();
        assert_eq!(n, 0);
        assert_eq!(t.score(&p), None);
    }

    #[test]
    fn pairing_revokes_more_than_it_grants() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        let d1 = t
            .apply(ReputationEvent::PairingEstablished {
                peer: p.clone(),
                credential_id_short: "abcd".into(),
            })
            .unwrap();
        let d2 = t
            .apply(ReputationEvent::PairingRevoked {
                peer: p.clone(),
                credential_id_short: "abcd".into(),
            })
            .unwrap();
        assert!(d1.delta > 0.0);
        assert!(d2.delta < 0.0);
        assert!(d2.delta.abs() > d1.delta.abs());
    }

    #[test]
    fn history_is_capped() {
        let mut p = ReputationParams::default();
        p.history_cap = 4;
        let t = PeerScoreTable::new(p);
        let peer = peer();
        for _ in 0..10 {
            t.apply(ReputationEvent::ValidMessage {
                peer: peer.clone(),
                topic: None,
                size_bytes: 1024,
            })
            .unwrap();
        }
        // walk shards to find the entry's history
        let entry = {
            let (idx, key) = t.shard_for(&peer);
            t.inner.shards[idx].peers.read().get(&key).cloned()
        };
        assert!(entry.is_some());
        assert!(entry.unwrap().peer_score.history.len() <= 4);
    }

    #[test]
    fn manual_reset_clears_peer() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        t.apply(ReputationEvent::ValidMessage {
            peer: p.clone(),
            topic: None,
            size_bytes: 1024,
        })
        .unwrap();
        assert!(t.score(&p).is_some());
        assert!(t.reset(&p));
        assert!(t.score(&p).is_none());
    }

    #[test]
    fn topic_scores_track_counts() {
        let t = PeerScoreTable::new(ReputationParams::default());
        let p = peer();
        let topic = adnet_types::Topic::from_label("adnet-room-test");
        for _ in 0..3 {
            t.apply(ReputationEvent::FirstMessageDelivery {
                peer: p.clone(),
                topic: Some(topic.clone()),
            })
            .unwrap();
        }
        let (idx, key) = t.shard_for(&p);
        let map = t.inner.shards[idx].peers.read();
        let entry = map.get(&key).unwrap();
        let ts = entry.topic_scores.get(topic.as_hex()).unwrap();
        assert_eq!(ts.first_deliveries, 3);
    }
}
