//! Retry & per-peer backoff policy for DHT network queries.
//!
//! Transient QUIC failures (timeouts, mid-handshake connection drops,
//! server-side resets during a still-valid TLS session) are
//! recoverable and benefit from a transparent retry with jittered
//! exponential backoff. Persistent failures (peer not found,
//! decode errors) are surfaced immediately so the caller can pick
//! the next peer or give up.
//!
//! In addition to per-attempt backoff, the [`PeerFailureTracker`]
//! implements a **per-peer cooldown** that suppresses rapid retries
//! to a peer that has just failed. The cooldown duration follows the
//! same exponential schedule but is keyed on `NodeId`; once a peer
//! succeeds, the failure record is cleared so a future lookup can
//! re-query it without delay.
//!
//! This is intentionally small and dependency-free so the rest of
//! `a3net-dht` can stay zero-cost-when-unused. The transport layer
//! is responsible for its own retry budget — this policy is purely
//! for **DHT-level** retries (FindNode / GetProviders).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use a3net_types::NodeId;

/// Retry policy applied to a single DHT query attempt.
///
/// Defaults mirror libp2p-kad: 3 retries, 500 ms initial backoff,
/// 2× multiplier capped at 30 s, ±20 % jitter.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum total attempts (the original attempt + retries).
    /// `max_attempts == 1` disables retry entirely.
    pub max_attempts: u32,
    /// Backoff applied **before** attempt #2 (i.e. the first retry).
    pub initial_backoff: Duration,
    /// Maximum per-attempt backoff. The exponential schedule clamps
    /// here so a flapping peer can't push the backoff into minutes.
    pub max_backoff: Duration,
    /// Multiplier applied to the previous backoff to get the next.
    /// `2.0` is the canonical exponential schedule.
    pub backoff_multiplier: f64,
    /// Jitter ratio. `0.2` means each backoff is uniformly perturbed
    /// by ±20 % to avoid thundering-herd retry storms.
    pub jitter_ratio: f64,
    /// Per-peer cooldown cap. Once a peer hits
    /// `peer_cooldown_threshold` failures it is suppressed for at
    /// least this long before any subsequent query even attempts.
    pub peer_cooldown_threshold: u32,
    pub peer_cooldown_min: Duration,
    pub peer_cooldown_max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            jitter_ratio: 0.2,
            peer_cooldown_threshold: 2,
            peer_cooldown_min: Duration::from_secs(5),
            peer_cooldown_max: Duration::from_secs(300),
        }
    }
}

impl RetryPolicy {
    /// `true` when the policy would still attempt another call after
    /// the current attempt failed.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.max_attempts
    }

    /// Backoff duration for the retry that follows `attempt`
    /// (1-indexed: `attempt == 1` returns the delay before attempt
    /// #2).
    ///
    /// Applies the exponential schedule then uniformly perturbs the
    /// result by ±`jitter_ratio`.
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        // `attempt` is the just-failed attempt number; the upcoming
        // retry's delay is schedule[attempt - 1].
        let schedule_index = (attempt - 1).min(20) as i32; // guard overflow
        let base_ms = self.initial_backoff.as_millis() as f64;
        let factor = self.backoff_multiplier.powi(schedule_index);
        let raw_ms = base_ms * factor;
        let max_ms = self.max_backoff.as_millis() as f64;
        let clamped_ms = raw_ms.min(max_ms);
        // Apply jitter uniformly in [clamped * (1 - jitter), clamped * (1 + jitter)].
        // Bounded below at 1 ms so we never sleep for zero.
        let low = clamped_ms * (1.0 - self.jitter_ratio);
        let high = clamped_ms * (1.0 + self.jitter_ratio);
        let jittered_ms = jitter_uniform(low, high).max(1.0);
        Duration::from_millis(jittered_ms as u64)
    }

    /// Backoff applied by the **per-peer** failure tracker once a
    /// peer has crossed `peer_cooldown_threshold` failures.
    ///
    /// The first cooldown equals `peer_cooldown_min`; subsequent
    /// cooldowns grow by `backoff_multiplier` (capped at
    /// `peer_cooldown_max`).
    pub fn peer_cooldown(&self, failure_count: u32) -> Duration {
        if failure_count < self.peer_cooldown_threshold {
            return Duration::ZERO;
        }
        let exponent = failure_count - self.peer_cooldown_threshold;
        let base_ms = self.peer_cooldown_min.as_millis() as f64;
        let factor = self.backoff_multiplier.powi(exponent.min(16) as i32);
        let raw_ms = base_ms * factor;
        let max_ms = self.peer_cooldown_max.as_millis() as f64;
        Duration::from_millis(raw_ms.min(max_ms) as u64)
    }
}

/// Decide whether a [`crate::query::QueryError`] is worth retrying.
///
/// Permanent errors (`PeerNotFound`, `InvalidResponse`) are returned
/// immediately because retrying them can't change the outcome.
/// Transient errors (`Timeout`, `Network`) are retried per the
/// policy.
pub fn is_transient(err: &crate::query::QueryError) -> bool {
    use crate::query::QueryError;
    matches!(err, QueryError::Timeout | QueryError::Network(_))
}

/// Cheap, dependency-free jitter.
///
/// `low` and `high` are non-negative; result lies in `[low, high]`
/// (inclusive on both ends for small ranges, the integer round
/// governs otherwise).
fn jitter_uniform(low: f64, high: f64) -> f64 {
    use std::cell::Cell;
    // Per-thread PRNG state to keep this function `no_std`-friendly
    // without pulling `rand` in (which is already a workspace
    // dep, but keeping the surface narrow lets unit tests run
    // without its thread-local initialisation).
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0x9E3779B97F4A7C15) };
    }
    let (low, high) = if high < low { (high, low) } else { (low, high) };
    if (high - low).abs() < f64::EPSILON {
        return low;
    }
    STATE.with(|s| {
        // xorshift64*
        let mut x = s.get();
        if x == 0 {
            x = 0x9E3779B97F4A7C15;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        // Take top 53 bits to fill a double in [1, 2).
        let unit = ((x >> 11) as f64) / ((1u64 << 53) as f64);
        low + unit * (high - low)
    })
}

/// Per-peer failure tracker.
///
/// Records a recent `Instant` + cumulative count per `NodeId`. On
/// [`try_acquire`](Self::try_acquire) it returns `None` if the peer
/// is still inside a cooldown window; otherwise it records an
/// attempt and returns `Some(attempt_number)`. Callers must invoke
/// [`record_success`](Self::record_success) on every successful
/// response to clear the failure state for that peer.
#[derive(Debug, Default)]
pub struct PeerFailureTracker {
    state: HashMap<NodeId, PeerState>,
    policy: RetryPolicy,
}

#[derive(Debug, Clone)]
struct PeerState {
    /// Total consecutive failures since the last success.
    consecutive_failures: u32,
    /// End of the current cooldown window. `None` means the peer
    /// isn't in cooldown.
    cooldown_until: Option<Instant>,
}

impl PeerFailureTracker {
    /// Build a tracker with the given policy.
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            state: HashMap::new(),
            policy,
        }
    }

    /// Number of distinct peers that have any recorded state.
    pub fn tracked_peers(&self) -> usize {
        self.state.len()
    }

    /// Try to reserve a query slot against `peer`.
    ///
    /// Returns `Ok(attempt_number)` if the peer is allowed to be
    /// queried. `attempt_number` starts at 1 and increments every
    /// time the same logical query hits the same peer after a
    /// failure (i.e. attempt #1 is the first try, attempt #2 is the
    /// first retry, etc.). Returns `Err(cooldown_remaining)` if the
    /// peer is currently in cooldown; the caller should pick a
    /// different peer or abort.
    pub fn try_acquire(&mut self, peer: &NodeId) -> Result<u32, Duration> {
        let now = Instant::now();
        let entry = self.state.entry(peer.clone()).or_insert(PeerState {
            consecutive_failures: 0,
            cooldown_until: None,
        });
        if let Some(until) = entry.cooldown_until {
            if until > now {
                return Err(until - now);
            }
            entry.cooldown_until = None;
        }
        Ok(entry.consecutive_failures + 1)
    }

    /// Record that a query against `peer` failed with a transient
    /// error. Bumps the counter and, if the threshold is crossed,
    /// schedules a cooldown window.
    pub fn record_failure(&mut self, peer: &NodeId) {
        let now = Instant::now();
        let entry = self.state.entry(peer.clone()).or_insert(PeerState {
            consecutive_failures: 0,
            cooldown_until: None,
        });
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        let cooldown = self.policy.peer_cooldown(entry.consecutive_failures);
        if !cooldown.is_zero() {
            entry.cooldown_until = Some(now + cooldown);
        }
    }

    /// Record that a query against `peer` succeeded. Clears the
    /// failure record entirely so the peer is immediately
    /// available for subsequent queries.
    pub fn record_success(&mut self, peer: &NodeId) {
        self.state.remove(peer);
    }

    /// Test-only: read out the cooldown for a peer without
    /// consuming it.
    #[cfg(test)]
    pub fn cooldown_for(&self, peer: &NodeId) -> Option<Duration> {
        let now = Instant::now();
        self.state
            .get(peer)
            .and_then(|s| s.cooldown_until)
            .map(|until| until.saturating_duration_since(now))
    }

    /// Test-only: read the failure count for a peer.
    #[cfg(test)]
    pub fn failure_count(&self, peer: &NodeId) -> u32 {
        self.state
            .get(peer)
            .map(|s| s.consecutive_failures)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::QueryError;

    #[test]
    fn backoff_grows_then_clamps() {
        let p = RetryPolicy::default();
        // attempt=1 → initial 500 ms (within ±20%)
        let b1 = p.backoff_for(1);
        assert!(
            b1 >= Duration::from_millis(400) && b1 <= Duration::from_millis(600),
            "got {b1:?}"
        );
        // attempt=2 → 2x
        let b2 = p.backoff_for(2);
        assert!(
            b2 >= Duration::from_millis(800) && b2 <= Duration::from_millis(1200),
            "got {b2:?}"
        );
        // Very high attempt should clamp at max_backoff.
        let b_high = p.backoff_for(20);
        assert!(
            b_high <= Duration::from_secs(36),
            "expected clamp to ~30s+jitter, got {b_high:?}"
        );
    }

    #[test]
    fn should_retry_respects_max_attempts() {
        let mut p = RetryPolicy::default();
        p.max_attempts = 3;
        assert!(p.should_retry(1));
        assert!(p.should_retry(2));
        assert!(!p.should_retry(3));
        assert!(!p.should_retry(4));
    }

    #[test]
    fn peer_cooldown_scales_with_failures() {
        let mut p = RetryPolicy::default();
        p.peer_cooldown_threshold = 2;
        p.peer_cooldown_min = Duration::from_secs(5);
        p.peer_cooldown_max = Duration::from_secs(60);
        p.backoff_multiplier = 2.0;
        assert_eq!(p.peer_cooldown(0), Duration::ZERO);
        assert_eq!(p.peer_cooldown(1), Duration::ZERO);
        assert_eq!(p.peer_cooldown(2), Duration::from_secs(5));
        assert_eq!(p.peer_cooldown(3), Duration::from_secs(10));
        assert_eq!(p.peer_cooldown(4), Duration::from_secs(20));
        // Should clamp at peer_cooldown_max.
        assert_eq!(p.peer_cooldown(20), Duration::from_secs(60));
    }

    #[test]
    fn is_transient_classifies_errors() {
        assert!(is_transient(&QueryError::Timeout));
        assert!(is_transient(&QueryError::Network("boom".into())));
        assert!(!is_transient(&QueryError::PeerNotFound));
        assert!(!is_transient(&QueryError::InvalidResponse));
    }

    #[test]
    fn tracker_allows_first_query_then_cools_down() {
        let mut p = RetryPolicy::default();
        p.peer_cooldown_threshold = 2;
        p.peer_cooldown_min = Duration::from_secs(60);
        let mut t = PeerFailureTracker::new(p);
        let peer = NodeId::random();
        assert_eq!(t.try_acquire(&peer).unwrap(), 1);
        t.record_failure(&peer);
        t.record_failure(&peer);
        // Now we should be in cooldown.
        let err = t.try_acquire(&peer).unwrap_err();
        assert!(err > Duration::ZERO);
        assert!(t.cooldown_for(&peer).unwrap() > Duration::ZERO);
        // Success clears the state.
        t.record_success(&peer);
        assert_eq!(t.try_acquire(&peer).unwrap(), 1);
        assert_eq!(t.failure_count(&peer), 0);
    }

    #[test]
    fn tracker_is_per_peer() {
        let p = RetryPolicy::default();
        let mut t = PeerFailureTracker::new(p);
        let a = NodeId::random();
        let b = NodeId::random();
        for _ in 0..5 {
            t.record_failure(&a);
        }
        // `a` should be cooled down, `b` unaffected.
        assert!(t.try_acquire(&a).is_err());
        assert!(t.try_acquire(&b).is_ok());
    }

    #[test]
    fn jitter_is_bounded() {
        // Run many samples and confirm they all stay in the
        // theoretical bounds (with ±1ms slack for the float round).
        let p = RetryPolicy::default();
        let b1 = p.backoff_for(1);
        let low = b1.as_millis() as i64 - 100;
        let high = b1.as_millis() as i64 + 100;
        for _ in 0..200 {
            let b = p.backoff_for(1);
            let ms = b.as_millis() as i64;
            // Initial backoff is 500 ms ±20% = [400, 600].
            assert!(ms >= 400, "got {ms}");
            assert!(ms <= 600, "got {ms}");
            // And bounds should be a sensible sub-range.
            assert!(ms >= low.min(400));
            assert!(ms <= high.max(600));
        }
    }
}