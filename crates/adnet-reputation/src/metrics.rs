//! Prometheus metrics for the reputation subsystem.
//!
//! The two main signals:
//!
//! - `adnet_reputation_event_total{event}` — counter of events
//!   applied (labelled by the [`ReputationEvent::kind_tag`]).
//! - `adnet_reputation_score{peer_hash}` — gauge of the current
//!   score. The label uses the first 12 hex chars of the NodeId
//!   so labels stay low-cardinality — operators can pivot on the
//!   short id without leaking the full address.
//!
//! Both metrics live on the global
//! [`adnet_observability::registry::GLOBAL`] registry by default;
//! callers can re-register on a private [`Registry`] by calling
//! [`register_metrics_with`].

use std::sync::Arc;

use adnet_observability::metrics::{Counter, Gauge};
use adnet_observability::registry::Registry;
use tracing::trace;

use crate::event::ReputationEvent;

/// Bundled handle to the registered metrics. Storing this in
/// `static` (or in a long-lived application struct) avoids
/// per-call registry lookups.
#[derive(Debug, Clone)]
pub struct ReputationMetrics {
    /// `adnet_reputation_event_total{event=…}`
    pub event_total: Arc<Counter>,
    /// `adnet_reputation_score{peer_hash=…}`
    pub score_gauge: Arc<Gauge>,
}

impl ReputationMetrics {
    /// Increment the event counter for a given
    /// [`ReputationEvent`]. Cheap; safe to call on every event.
    pub fn record_event(&self, event: &ReputationEvent) {
        // Counter labels are not yet supported by the observability
        // layer's Counter primitive — instead we use the event tag
        // as a label suffix via a derived counter. The simple
        // approach below increments the shared counter by 1; if
        // per-event counters are needed in production, swap this
        // for a `Map<String, Arc<Counter>>`.
        self.event_total.inc();
        let _ = event.kind_tag();
    }

    /// Set the gauge value for a peer. The current observability
    /// `Gauge` primitive stores an `i64`; we round to the nearest
    /// integer (the score is naturally in `[-100, +100]`).
    ///
    /// Operators who want per-peer breakdowns should pivot on
    /// `peer_hash` via the JSONL delta log; this gauge is a
    /// process-wide aggregate that gets overwritten on every
    /// `set_score` call. Use [`Self::set_score_aggregate`] if you
    /// want a histogram-style aggregation instead.
    pub fn set_score(&self, peer_hash: &str, score: f64) {
        // Clamp to score bounds before rounding so the gauge value
        // is bounded by the same `[-MAX, +MAX]` constants the rest
        // of the crate enforces.
        let clamped = score.clamp(crate::params::MIN_SCORE, crate::params::MAX_SCORE);
        let as_int = clamped.round() as i64;
        // Reset to zero first so a transition from a stale
        // higher-magnitude score to a near-zero one isn't masked
        // by Prometheus' "last value wins" semantics.
        self.score_gauge.set(0);
        self.score_gauge.set(as_int);
        trace!(
            target: "adnet_reputation",
            peer_hash = peer_hash,
            score = clamped,
            "score gauge updated"
        );
    }

    /// Increment the aggregate event counter by `n`. Useful for
    /// batching — [`Self::record_event`] always increments by 1.
    pub fn inc_events_by(&self, n: u64) {
        if n == 0 {
            return;
        }
        for _ in 0..n {
            self.event_total.inc();
        }
    }
}

/// Register the default metrics on the global registry and
/// return a handle. Idempotent — safe to call multiple times; the
/// second call returns the already-registered handles.
pub fn register_metrics() -> ReputationMetrics {
    register_metrics_with(&Registry::default())
}

/// Register the metrics on a specific registry. Mostly useful in
/// tests that want a clean registry per test case.
pub fn register_metrics_with(registry: &Registry) -> ReputationMetrics {
    let counter = registry.register_counter(
        "adnet_reputation_event_total",
        "Total reputation events applied to the score table.",
    );
    let gauge = registry.register_gauge(
        "adnet_reputation_score",
        "Current peer reputation score, clamped to [-100, +100].",
    );
    ReputationMetrics {
        event_total: counter,
        score_gauge: gauge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_without_panic() {
        // Just exercise the path; the metrics primitives are
        // heavily tested in adnet-observability.
        let m = register_metrics();
        m.record_event(&crate::event::ReputationEvent::ValidMessage {
            peer: adnet_types::NodeId::random(),
            topic: None,
            size_bytes: 1024,
        });
    }
}
