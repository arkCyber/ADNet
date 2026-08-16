//! Per-attempt and per-call diagnostics.
//!
//! Two levels:
//!
//! - [`StrategyAttempt`] — one entry per strategy the planner
//!   spawned for a single `punch()` call. Captures the outcome
//!   (hit / miss / error / cancelled), elapsed time, and a
//!   short error message.
//!
//! - [`HolePunchOutcome`] — the whole call's result. Wraps the
//!   winning `ResolvedEndpoint` (if any), the per-attempt trace,
//!   and the overall wall-clock elapsed time.
//!
//! The planner accumulates a [`HolePunchDiagnostics`] (Arc-shared
//! counter block) so callers can monitor long-running discovery
//! behaviour without owning the planner.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::error::HolePunchError;
use crate::strategy::{ResolvedEndpoint, ResolverCapabilities};

/// Per-strategy outcome of a single `punch()` call.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyAttempt {
    /// The strategy's provenance label (matches
    /// [`HolePunchResolver::label`]).
    pub strategy: String,
    /// What the resolver advertised. The planner populates this
    /// from the strategy's static capability mask.
    pub capabilities: ResolverCapabilities,
    /// High-level outcome. The discriminator is the `kind` field
    /// in the serialised JSON; the other fields are populated
    /// based on the outcome.
    pub outcome: AttemptOutcome,
    /// Wall-clock duration the attempt took (cancelled attempts
    /// may be short).
    pub elapsed: Duration,
}

/// Discriminated outcome for a single strategy attempt.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// The resolver returned a non-empty `ResolvedEndpoint`.
    Hit {
        /// Number of addresses the resolver returned (relay + direct).
        addresses: usize,
    },
    /// The resolver returned an empty `ResolvedEndpoint` — the
    /// target exists but no addressing info is reachable.
    Empty,
    /// The resolver returned an error. `error` is the
    /// `HolePunchError::classify()` short code; `message` is the
    /// full `Display`.
    Error {
        error: String,
        message: String,
    },
    /// The resolver was cancelled by the planner because a sibling
    /// strategy already won.
    Cancelled,
}

impl AttemptOutcome {
    /// Short tag — useful for log aggregation.
    pub fn tag(&self) -> &'static str {
        match self {
            AttemptOutcome::Hit { .. } => "hit",
            AttemptOutcome::Empty => "empty",
            AttemptOutcome::Error { .. } => "error",
            AttemptOutcome::Cancelled => "cancelled",
        }
    }

    /// `true` when this attempt represents a winning strategy.
    pub fn is_winner(&self) -> bool {
        matches!(self, AttemptOutcome::Hit { .. })
    }
}

/// Outcome of a single `punch()` call. The planner returns this
/// from every call so the caller knows which strategy won (or
/// why none of them did) and how long the race took.
#[derive(Debug, Clone, Serialize)]
pub struct HolePunchOutcome {
    /// The resolved endpoint, if any strategy hit.
    pub endpoint: Option<ResolvedEndpoint>,
    /// The label of the strategy that produced `endpoint`, if any.
    /// `None` when no strategy succeeded.
    pub winning_strategy: Option<String>,
    /// Per-strategy attempts. The same order as the input
    /// `HolePunchConfig::strategies` list — the planner appends
    /// an entry per strategy it actually spawned.
    pub attempts: Vec<StrategyAttempt>,
    /// Wall-clock elapsed time for the full `punch()` call.
    pub elapsed: Duration,
    /// When the call started. Useful for log correlation.
    pub started_at: SystemTime,
    /// High-level terminal error, if the planner surfaces one
    /// (currently: cancellation or exhaustion). `None` when the
    /// call produced a hit OR when [`TimeoutPolicy::SilentEmpty`]
    /// is in effect and the call simply timed out.
    pub error: Option<HolePunchError>,
    /// Config snapshot that produced this outcome. The planner
    /// serialises only the labels (not the resolver impls) so the
    /// JSON stays small.
    pub enabled_strategies: Vec<String>,
    /// Number of strategies that actually ran (after the
    /// capability filter / concurrency cap).
    pub effective_strategy_count: usize,
}

impl HolePunchOutcome {
    /// Stable `true` when the call produced a usable endpoint.
    pub fn is_hit(&self) -> bool {
        self.endpoint.is_some()
    }

    /// The winning `ResolvedEndpoint`, if any. Convenience over
    /// `outcome.endpoint.clone()`.
    pub fn resolved_endpoint(&self) -> Option<ResolvedEndpoint> {
        self.endpoint.clone()
    }

    /// The label of the winning strategy, if any.
    pub fn winning_strategy(&self) -> Option<&str> {
        self.winning_strategy.as_deref()
    }

    /// Convert into the `ResolvedEndpoint` (consuming the outcome).
    /// The operator-facing equivalent of "did we find the peer?"
    pub fn into_endpoint_addr(self) -> Option<ResolvedEndpoint> {
        let _ = self.attempts; // Drop the trace; the caller can keep it via `outcome`.
        self.endpoint
    }

    /// Number of strategies that hit (typically 0 or 1 in a
    /// `RaceAll` race; higher when multiple strategies returned
    /// the same address).
    pub fn hits(&self) -> usize {
        self.attempts.iter().filter(|a| a.outcome.is_winner()).count()
    }

    /// Number of strategies that returned an empty endpoint.
    pub fn empties(&self) -> usize {
        self.attempts
            .iter()
            .filter(|a| matches!(a.outcome, AttemptOutcome::Empty))
            .count()
    }

    /// Number of strategies that errored.
    pub fn errors(&self) -> usize {
        self.attempts
            .iter()
            .filter(|a| matches!(a.outcome, AttemptOutcome::Error { .. }))
            .count()
    }

    /// Number of strategies that were cancelled.
    pub fn cancelled(&self) -> usize {
        self.attempts
            .iter()
            .filter(|a| matches!(a.outcome, AttemptOutcome::Cancelled))
            .count()
    }

    /// Pretty single-line summary, useful for log lines.
    pub fn summary(&self) -> String {
        let winner = self
            .winning_strategy
            .clone()
            .unwrap_or_else(|| "none".to_string());
        format!(
            "hole_punch elapsed={:?} winner={} attempts={} hits={} empty={} err={} cancel={}",
            self.elapsed,
            winner,
            self.attempts.len(),
            self.hits(),
            self.empties(),
            self.errors(),
            self.cancelled(),
        )
    }
}

/// Shared counter block. The planner increments every field
/// across all `punch()` calls; callers can read the snapshot at
/// any time. Mirrors the `DiscoveryDiagnostics` pattern used by
/// `a3net-transport::iroh::discovery`.
#[derive(Debug, Default)]
pub struct HolePunchDiagnostics {
    inner: Arc<DiagnosticsInner>,
}

#[derive(Debug, Default)]
struct DiagnosticsInner {
    /// Total `punch()` calls.
    calls_total: AtomicU64,
    /// `punch()` calls that produced a non-empty endpoint.
    calls_hit: AtomicU64,
    /// `punch()` calls that exhausted all strategies.
    calls_exhausted: AtomicU64,
    /// `punch()` calls cancelled before completion (e.g. by the
    /// parent task).
    calls_cancelled: AtomicU64,
    /// Per-strategy counters. Stamped by the planner on every
    /// attempt outcome.
    by_strategy: Mutex<Vec<StrategyCount>>,
    /// Last outcome, kept for the `/discovery` admin snapshot.
    last_outcome: Mutex<Option<HolePunchOutcome>>,
    /// Last call start time.
    last_call_at: Mutex<Option<SystemTime>>,
}

/// Per-strategy aggregate counter.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StrategyCount {
    pub strategy: String,
    pub attempts: u64,
    pub hits: u64,
    pub empties: u64,
    pub errors: u64,
    pub cancelled: u64,
}

impl HolePunchDiagnostics {
    /// Build a fresh `HolePunchDiagnostics`. The planner constructs
    /// one internally when the caller doesn't supply one, so most
    /// usages don't need to call this directly.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a single `punch()` call's outcome. The planner
    /// calls this exactly once per `punch()` invocation.
    pub fn record_outcome(&self, outcome: &HolePunchOutcome) {
        self.inner.calls_total.fetch_add(1, Ordering::Relaxed);
        if outcome.is_hit() {
            self.inner.calls_hit.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(
            outcome.error,
            Some(HolePunchError::StrategiesExhausted { .. })
        ) {
            self.inner
                .calls_exhausted
                .fetch_add(1, Ordering::Relaxed);
        }
        if matches!(outcome.error, Some(HolePunchError::Cancelled)) {
            self.inner
                .calls_cancelled
                .fetch_add(1, Ordering::Relaxed);
        }
        *recover_lock(self.inner.last_outcome.lock()) = Some(outcome.clone());
        *recover_lock(self.inner.last_call_at.lock()) = Some(SystemTime::now());

        let mut guard = recover_lock(self.inner.by_strategy.lock());
        for att in &outcome.attempts {
            if let Some(existing) = guard.iter_mut().find(|s| s.strategy == att.strategy) {
                existing.attempts += 1;
                match &att.outcome {
                    AttemptOutcome::Hit { .. } => existing.hits += 1,
                    AttemptOutcome::Empty => existing.empties += 1,
                    AttemptOutcome::Error { .. } => existing.errors += 1,
                    AttemptOutcome::Cancelled => existing.cancelled += 1,
                }
            } else {
                let mut sc = StrategyCount {
                    strategy: att.strategy.clone(),
                    attempts: 1,
                    hits: 0,
                    empties: 0,
                    errors: 0,
                    cancelled: 0,
                };
                match &att.outcome {
                    AttemptOutcome::Hit { .. } => sc.hits = 1,
                    AttemptOutcome::Empty => sc.empties = 1,
                    AttemptOutcome::Error { .. } => sc.errors = 1,
                    AttemptOutcome::Cancelled => sc.cancelled = 1,
                }
                guard.push(sc);
            }
        }
    }

    /// Snapshot for `/discovery` admin output.
    pub fn snapshot(&self) -> HolePunchDiagnosticsSnapshot {
        let by_strategy = recover_lock(self.inner.by_strategy.lock()).clone();
        let last_outcome = recover_lock(self.inner.last_outcome.lock()).clone();
        let last_call_at = *recover_lock(self.inner.last_call_at.lock());
        HolePunchDiagnosticsSnapshot {
            calls_total: self.inner.calls_total.load(Ordering::Relaxed),
            calls_hit: self.inner.calls_hit.load(Ordering::Relaxed),
            calls_exhausted: self.inner.calls_exhausted.load(Ordering::Relaxed),
            calls_cancelled: self.inner.calls_cancelled.load(Ordering::Relaxed),
            by_strategy,
            last_outcome,
            last_call_at,
        }
    }

    /// Borrow the inner `Arc` so the caller can clone the
    /// diagnostics into a long-running subsystem.
    pub fn shared(&self) -> Arc<Self> {
        Arc::new(HolePunchDiagnostics {
            inner: Arc::clone(&self.inner),
        })
    }
}

/// Snapshot of the diagnostics counters. The struct is `Clone`
/// and serialisable so it can be embedded in JSON for `/discovery`
/// or sent over IPC.
#[derive(Debug, Clone, Serialize)]
pub struct HolePunchDiagnosticsSnapshot {
    pub calls_total: u64,
    pub calls_hit: u64,
    pub calls_exhausted: u64,
    pub calls_cancelled: u64,
    pub by_strategy: Vec<StrategyCount>,
    pub last_outcome: Option<HolePunchOutcome>,
    pub last_call_at: Option<SystemTime>,
}

impl HolePunchDiagnosticsSnapshot {
    /// Hit rate as a percentage (0.0 ..= 100.0). `0.0` when no
    /// calls have been recorded.
    pub fn hit_rate_pct(&self) -> f64 {
        if self.calls_total == 0 {
            0.0
        } else {
            (self.calls_hit as f64 / self.calls_total as f64) * 100.0
        }
    }

    /// Empty snapshot — useful when no calls have happened yet.
    pub fn empty() -> Self {
        Self {
            calls_total: 0,
            calls_hit: 0,
            calls_exhausted: 0,
            calls_cancelled: 0,
            by_strategy: Vec::new(),
            last_outcome: None,
            last_call_at: None,
        }
    }
}

/// Recover from a poisoned mutex without panicking. Mirrors the
/// helper used by `a3net-transport::iroh::discovery::diagnostics`.
fn recover_lock<T>(res: std::sync::LockResult<T>) -> T {
    match res {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::NodeId;

    fn nid() -> NodeId {
        NodeId::from_bytes(&[0u8; 32]).expect("32 bytes is valid")
    }

    fn attempt(outcome: AttemptOutcome) -> StrategyAttempt {
        StrategyAttempt {
            strategy: "test-strategy".into(),
            capabilities: ResolverCapabilities::all(),
            outcome,
            elapsed: Duration::from_millis(5),
        }
    }

    fn outcome_with(attempts: Vec<StrategyAttempt>) -> HolePunchOutcome {
        let winner = attempts.iter().find(|a| a.outcome.is_winner()).map(|a| a.strategy.clone());
        let endpoint = if winner.is_some() {
            Some(ResolvedEndpoint::empty(nid()))
        } else {
            None
        };
        HolePunchOutcome {
            endpoint,
            winning_strategy: winner,
            attempts,
            elapsed: Duration::from_millis(10),
            started_at: SystemTime::now(),
            error: None,
            enabled_strategies: vec!["test-strategy".into()],
            effective_strategy_count: 1,
        }
    }

    #[test]
    fn attempt_outcome_tag_is_stable() {
        assert_eq!(AttemptOutcome::Hit { addresses: 1 }.tag(), "hit");
        assert_eq!(AttemptOutcome::Empty.tag(), "empty");
        assert_eq!(
            AttemptOutcome::Error {
                error: "x".into(),
                message: "y".into(),
            }
            .tag(),
            "error"
        );
        assert_eq!(AttemptOutcome::Cancelled.tag(), "cancelled");
    }

    #[test]
    fn outcome_summary_includes_winner() {
        let o = outcome_with(vec![attempt(AttemptOutcome::Hit { addresses: 1 })]);
        let s = o.summary();
        assert!(s.contains("winner=test-strategy"));
        assert!(s.contains("hits=1"));
    }

    #[test]
    fn outcome_counters_segregate_hit_empty_error_cancel() {
        let o = outcome_with(vec![
            attempt(AttemptOutcome::Hit { addresses: 1 }),
            attempt(AttemptOutcome::Empty),
            attempt(AttemptOutcome::Error {
                error: "x".into(),
                message: "y".into(),
            }),
            attempt(AttemptOutcome::Cancelled),
        ]);
        assert_eq!(o.hits(), 1);
        assert_eq!(o.empties(), 1);
        assert_eq!(o.errors(), 1);
        assert_eq!(o.cancelled(), 1);
    }

    #[test]
    fn diagnostics_record_outcome_updates_counters() {
        let d = HolePunchDiagnostics::new();
        let o = outcome_with(vec![attempt(AttemptOutcome::Hit { addresses: 2 })]);
        d.record_outcome(&o);
        let snap = d.snapshot();
        assert_eq!(snap.calls_total, 1);
        assert_eq!(snap.calls_hit, 1);
        assert!((snap.hit_rate_pct() - 100.0).abs() < f64::EPSILON);
        let sc = snap
            .by_strategy
            .iter()
            .find(|s| s.strategy == "test-strategy")
            .expect("strategy bucket");
        assert_eq!(sc.attempts, 1);
        assert_eq!(sc.hits, 1);
    }

    #[test]
    fn diagnostics_record_outcome_aggregates_across_calls() {
        let d = HolePunchDiagnostics::new();
        d.record_outcome(&outcome_with(vec![attempt(AttemptOutcome::Hit { addresses: 1 })]));
        d.record_outcome(&outcome_with(vec![attempt(AttemptOutcome::Empty)]));
        d.record_outcome(&outcome_with(vec![attempt(AttemptOutcome::Error {
            error: "x".into(),
            message: "y".into(),
        })]));
        let snap = d.snapshot();
        assert_eq!(snap.calls_total, 3);
        assert_eq!(snap.calls_hit, 1);
        let sc = snap
            .by_strategy
            .iter()
            .find(|s| s.strategy == "test-strategy")
            .expect("strategy bucket");
        assert_eq!(sc.attempts, 3);
        assert_eq!(sc.hits, 1);
        assert_eq!(sc.empties, 1);
        assert_eq!(sc.errors, 1);
    }

    #[test]
    fn empty_snapshot_returns_zero_hit_rate() {
        let s = HolePunchDiagnosticsSnapshot::empty();
        assert!(s.hit_rate_pct().abs() < f64::EPSILON);
    }
}
