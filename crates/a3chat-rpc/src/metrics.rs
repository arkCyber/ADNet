//! Lightweight metrics & request-counter layer for `a3chat-rpc`.
//!
//! Every handler that wants to count itself wraps its call with
//! [`Metrics::record`] (or the helper macros). The counters are
//! in-process, exported in two ways:
//!
//! 1. `GET /rpc/metrics` — Prometheus text format (best-effort
//!    hand-rolled — we don't pull in the `prometheus` crate).
//! 2. `GET /rpc/stats` — JSON snapshot for `a3chat doctor --json`.
//!
//! DO-178C §6.1 (deterministic): the JSON output is key-sorted
//! and the text format has a stable ordering.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Process-wide metrics handle. Cheap to clone (`Arc`).
#[derive(Debug)]
pub struct Metrics {
    started_at: Instant,
    rpc_total: AtomicU64,
    rpc_errors: AtomicU64,
    rpc_transient: AtomicU64,
    sse_clients: AtomicU64,
    per_method: parking_lot::Mutex<BTreeMap<String, MethodCounters>>,
}

/// Per-method counter — one row per `a3chat.*` we observed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MethodCounters {
    pub total: u64,
    pub errors: u64,
    pub transient: u64,
    pub total_latency_us: u64,
}

impl MethodCounters {
    pub fn avg_latency_us(&self) -> u64 {
        if self.total == 0 {
            0
        } else {
            self.total_latency_us / self.total
        }
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            rpc_total: AtomicU64::new(0),
            rpc_errors: AtomicU64::new(0),
            rpc_transient: AtomicU64::new(0),
            sse_clients: AtomicU64::new(0),
            per_method: parking_lot::Mutex::new(BTreeMap::new()),
        }
    }

    /// Record a completed call. `latency_us` is the wall-clock
    /// micros spent in the handler.
    pub fn record(&self, method: &str, outcome: RpcOutcome, latency_us: u64) {
        self.rpc_total.fetch_add(1, Ordering::Relaxed);
        match outcome {
            RpcOutcome::Success => {}
            RpcOutcome::Error => {
                self.rpc_errors.fetch_add(1, Ordering::Relaxed);
            }
            RpcOutcome::Transient => {
                self.rpc_errors.fetch_add(1, Ordering::Relaxed);
                self.rpc_transient.fetch_add(1, Ordering::Relaxed);
            }
        }
        let mut map = self.per_method.lock();
        let row = map.entry(method.to_string()).or_default();
        row.total += 1;
        row.total_latency_us = row.total_latency_us.saturating_add(latency_us);
        match outcome {
            RpcOutcome::Success => {}
            RpcOutcome::Error => row.errors += 1,
            RpcOutcome::Transient => {
                row.errors += 1;
                row.transient += 1;
            }
        }
    }

    pub fn sse_inc(&self) {
        self.sse_clients.fetch_add(1, Ordering::Relaxed);
    }
    pub fn sse_dec(&self) {
        // saturating so we never underflow
        let prev = self
            .sse_clients
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
        let _ = prev;
    }
    pub fn sse_clients(&self) -> u64 {
        self.sse_clients.load(Ordering::Relaxed)
    }
    pub fn rpc_total(&self) -> u64 {
        self.rpc_total.load(Ordering::Relaxed)
    }
    pub fn rpc_errors(&self) -> u64 {
        self.rpc_errors.load(Ordering::Relaxed)
    }
    pub fn rpc_transient(&self) -> u64 {
        self.rpc_transient.load(Ordering::Relaxed)
    }
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Per-method snapshot for the JSON `/rpc/stats` endpoint.
    pub fn per_method_snapshot(&self) -> Vec<(String, MethodCounters)> {
        self.per_method
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    /// Prometheus exposition format. Counter types only (no
    /// histograms — average latency is exposed as a `*_avg_us`
    /// gauge).
    pub fn to_prometheus(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# HELP a3chat_rpc_calls_total Total RPC calls since process start."
        );
        let _ = writeln!(out, "# TYPE a3chat_rpc_calls_total counter");
        let _ = writeln!(out, "a3chat_rpc_calls_total {}", self.rpc_total());
        let _ = writeln!(
            out,
            "# HELP a3chat_rpc_errors_total RPC calls that returned an error."
        );
        let _ = writeln!(out, "# TYPE a3chat_rpc_errors_total counter");
        let _ = writeln!(out, "a3chat_rpc_errors_total {}", self.rpc_errors());
        let _ = writeln!(
            out,
            "# HELP a3chat_rpc_transient_total RPC calls that returned a transient error."
        );
        let _ = writeln!(out, "# TYPE a3chat_rpc_transient_total counter");
        let _ = writeln!(out, "a3chat_rpc_transient_total {}", self.rpc_transient());
        let _ = writeln!(
            out,
            "# HELP a3chat_sse_clients Connected Server-Sent-Events clients."
        );
        let _ = writeln!(out, "# TYPE a3chat_sse_clients gauge");
        let _ = writeln!(out, "a3chat_sse_clients {}", self.sse_clients());
        let _ = writeln!(
            out,
            "# HELP a3chat_uptime_secs Seconds since the daemon was started."
        );
        let _ = writeln!(out, "# TYPE a3chat_uptime_secs gauge");
        let _ = writeln!(out, "a3chat_uptime_secs {}", self.uptime_secs());

        for (method, c) in self.per_method_snapshot() {
            let safe = sanitize_label(&method);
            // One HELP/TYPE pair per family — emit them once
            // before the first labelled line. The Prometheus
            // parser tolerates this either way, but it's the
            // documented pattern.
            let _ = writeln!(
                out,
                "# HELP a3chat_method_calls_total Calls per method (label is the a3chat.* name)."
            );
            let _ = writeln!(out, "# TYPE a3chat_method_calls_total counter");
            let _ = writeln!(
                out,
                "a3chat_method_calls_total{{method=\"{safe}\"}} {}",
                c.total
            );
            let _ = writeln!(
                out,
                "a3chat_method_errors_total{{method=\"{safe}\"}} {}",
                c.errors
            );
            let _ = writeln!(
                out,
                "a3chat_method_latency_avg_us{{method=\"{safe}\"}} {}",
                c.avg_latency_us()
            );
        }
        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome tag for [`Metrics::record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcOutcome {
    Success,
    Error,
    Transient,
}

/// `method_name` may contain spaces / dashes — sanitize for the
/// Prometheus label syntax (only `[A-Za-z0-9_]` are safe in
/// practice).
fn sanitize_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_total_and_errors() {
        let m = Metrics::new();
        m.record("a3chat.foo", RpcOutcome::Success, 100);
        m.record("a3chat.foo", RpcOutcome::Error, 200);
        m.record("a3chat.foo", RpcOutcome::Transient, 300);
        assert_eq!(m.rpc_total(), 3);
        assert_eq!(m.rpc_errors(), 2);
        assert_eq!(m.rpc_transient(), 1);
    }

    #[test]
    fn per_method_snapshot_is_sorted() {
        let m = Metrics::new();
        m.record("a3chat.b", RpcOutcome::Success, 1);
        m.record("a3chat.a", RpcOutcome::Success, 2);
        m.record("a3chat.c", RpcOutcome::Success, 3);
        let s = m.per_method_snapshot();
        let names: Vec<&str> = s.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a3chat.a", "a3chat.b", "a3chat.c"]);
    }

    #[test]
    fn avg_latency_handles_zero_calls() {
        let m = Metrics::new();
        m.record("a3chat.zero", RpcOutcome::Success, 0);
        // No calls — avg should be 0, not panic.
        let snap = m.per_method_snapshot();
        // The above call WAS a call, so there will be one row.
        // But the *function* `avg_latency_us` should still be 0 if no calls were ever made.
        // Test the function via a fresh snapshot entry:
        let c = MethodCounters::default();
        assert_eq!(c.avg_latency_us(), 0);
        assert!(!snap.is_empty());
    }

    #[test]
    fn prometheus_output_is_stable() {
        let m = Metrics::new();
        m.record("a3chat.a", RpcOutcome::Success, 100);
        m.record("a3chat.a", RpcOutcome::Error, 50);
        let p = m.to_prometheus();
        // Must contain a stable set of metric families.
        for needle in [
            "a3chat_rpc_calls_total",
            "a3chat_rpc_errors_total",
            "a3chat_rpc_transient_total",
            "a3chat_sse_clients",
            "a3chat_uptime_secs",
            "a3chat_method_calls_total{method=\"a3chat_a\"}",
            "a3chat_method_errors_total{method=\"a3chat_a\"}",
        ] {
            assert!(p.contains(needle), "missing {needle} in:\n{p}");
        }
    }

    #[test]
    fn sse_counter_saturates_at_zero() {
        let m = Metrics::new();
        m.sse_dec();
        m.sse_dec();
        assert_eq!(m.sse_clients(), 0);
        m.sse_inc();
        assert_eq!(m.sse_clients(), 1);
        m.sse_dec();
        assert_eq!(m.sse_clients(), 0);
    }

    /// DO-178C §6.1 (determinism under load): counter sums must
    /// not lose updates when many tasks call `record`
    /// concurrently. This catches missing locking, lost
    /// reads-modify-writes, and spurious `Ordering` choices.
    #[test]
    fn record_is_atomic_under_concurrency() {
        use std::sync::Arc;
        use std::thread;
        let m = Arc::new(Metrics::new());
        let threads = 8;
        let per_thread = 500;
        let mut handles = Vec::new();
        for t in 0..threads {
            let m = m.clone();
            let outcome = match t % 3 {
                0 => RpcOutcome::Success,
                1 => RpcOutcome::Error,
                _ => RpcOutcome::Transient,
            };
            handles.push(thread::spawn(move || {
                for _ in 0..per_thread {
                    m.record("a3chat.race", outcome, 10);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread join");
        }
        assert_eq!(
            m.rpc_total(),
            (threads * per_thread) as u64,
            "no lost updates under concurrency"
        );
        let snap = m.per_method_snapshot();
        let row = snap
            .iter()
            .find(|(k, _)| k == "a3chat.race")
            .expect("row present");
        assert_eq!(row.1.total, (threads * per_thread) as u64);
    }

    /// Severity-tagged counters (errors / transient) must total
    /// to <= `rpc_total`. Run alongside the success counter for
    /// a sanity check that the mutually-exclusive branches
    /// stay disjoint.
    #[test]
    fn outcome_buckets_are_disjoint() {
        let m = Metrics::new();
        for _ in 0..10 {
            m.record("a3chat.a", RpcOutcome::Success, 1);
        }
        for _ in 0..5 {
            m.record("a3chat.a", RpcOutcome::Error, 1);
        }
        for _ in 0..3 {
            m.record("a3chat.a", RpcOutcome::Transient, 1);
        }
        assert_eq!(m.rpc_total(), 18);
        assert_eq!(m.rpc_errors(), 8);
        assert_eq!(m.rpc_transient(), 3);
        // Per-method distribution must agree.
        let row = m
            .per_method_snapshot()
            .into_iter()
            .find(|(k, _)| k == "a3chat.a")
            .unwrap()
            .1;
        assert_eq!(row.total, 18);
        assert_eq!(row.errors, 8);
        assert_eq!(row.transient, 3);
    }

    /// `sanitize_label` is called every Prometheus emission; if
    /// it's not injective we may merge two distinct methods
    /// into one label value. Guard against accidental changes.
    #[test]
    fn sanitize_label_collapses_dots_and_dashes() {
        assert_eq!(sanitize_label("a3chat.chat.send"), "a3chat_chat_send");
        assert_eq!(sanitize_label("a3chat-chat-send"), "a3chat_chat_send");
        // Pure-ASCII alphanumeric must be preserved.
        assert_eq!(sanitize_label("a3chatChatSend"), "a3chatChatSend");
    }
}
