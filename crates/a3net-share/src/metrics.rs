//! Share-layer metrics — counters and a histogram for the
//! `share receive` path.
//!
//! All metrics are registered eagerly on first access via the
//! `share_metrics()` static helper. The Prometheus exporter
//! picks them up automatically; no wiring is needed at the call
//! site (the only integration point is the
//! `receive_progress` trait the `receive()` implementation
//! drives, and `a3net-cli::share` is free to ignore it).
//!
//! ## Metric names
//!
//! All names are prefixed with `a3net_share_receive_*` so the
//! Prometheus output is namespaced away from the blobstore /
//! gossip / transport layer metrics.
//!
//! ## Cardinality
//!
//! Low-cardinality by design. There are no per-hash / per-peer
//! labels today; the receive progress is process-wide. A future
//! PR may add `sender_node_id` once we have a curated
//! allow-list (otherwise the cardinality explodes).
//!
//! ## Why a histogram, not a counter
//!
//! `share_receive_seconds` is a histogram so an operator can
//! query the receive p50 / p90 / p99 from the Prometheus
//! `/metrics` endpoint. The bucket layout is in seconds and
//! follows `DEFAULT_BUCKETS` from `a3net-observability` (1 ms
//! to 10 s, plus `+Inf`). Receives that take longer than 10 s
//! are common for large directory pulls; the operator can
//! pair the `+Inf` count against the explicit bucket counts to
//! see how often that happens.

use std::sync::Arc;
use std::time::Duration;

use a3net_observability::histogram::Histogram;
use a3net_observability::metrics::Counter;
use a3net_observability::registry::{
    GLOBAL as OBSERVABILITY_GLOBAL, Registry,
};
use once_cell::sync::Lazy;

/// Per-layer metric handle. Constructed once per process via
/// [`ShareMetrics::get`]. The fields are `Arc`-shared so
/// cloning the handle is cheap.
#[derive(Debug, Clone)]
pub struct ShareMetrics {
    /// `a3net_share_receive_bytes_total` — cumulative
    /// **expected** bytes across every share receive (sum of
    /// every manifest entry's size). Updated once per
    /// receive when the manifest is known.
    pub receive_bytes_total: Arc<Counter>,
    /// `a3net_share_receive_bytes_done` — cumulative
    /// bytes successfully written to the local output
    /// directory. Updated incrementally as each file lands
    /// on disk (or in one shot at the end of the receive,
    /// depending on the source of the bytes).
    pub receive_bytes_done: Arc<Counter>,
    /// `a3net_share_receive_files_total` — cumulative
    /// manifest entry count across every receive.
    pub receive_files_total: Arc<Counter>,
    /// `a3net_share_receive_files_done` — cumulative
    /// number of files successfully written.
    pub receive_files_done: Arc<Counter>,
    /// `a3net_share_receive_errors_total` — every error
    /// surfaced from the local-receive path (read failure,
    /// IO error, target collision without `--overwrite`,
    /// etc.). Operators should alert on a non-zero rate.
    pub receive_errors: Arc<Counter>,
    /// `a3net_share_receive_seconds` — wall-clock duration
    /// of each `receive()` call, observed as a histogram
    /// sample. The bucket layout is the standard
    /// `DEFAULT_BUCKETS` (1 ms .. 10 s .. +Inf).
    pub receive_seconds: Arc<Histogram>,
}

impl ShareMetrics {
    /// Register every metric into `registry`. Idempotent —
    /// the caller (`ShareMetrics::get`) gates registration
    /// behind a `Lazy` so this runs once per process.
    pub fn register(registry: &Registry) -> Self {
        Self {
            receive_bytes_total: registry.register_counter(
                "a3net_share_receive_bytes_total",
                "Cumulative expected bytes across every share receive (sum of manifest entry sizes).",
            ),
            receive_bytes_done: registry.register_counter(
                "a3net_share_receive_bytes_done",
                "Cumulative bytes successfully written by share receive to the local output directory.",
            ),
            receive_files_total: registry.register_counter(
                "a3net_share_receive_files_total",
                "Cumulative manifest entry count across every share receive.",
            ),
            receive_files_done: registry.register_counter(
                "a3net_share_receive_files_done",
                "Cumulative number of files successfully written by share receive.",
            ),
            receive_errors: registry.register_counter(
                "a3net_share_receive_errors_total",
                "Cumulative error count from the share receive path (IO, overwrite refusal, backend failure).",
            ),
            receive_seconds: registry.register_histogram(
                "a3net_share_receive_seconds",
                "Wall-clock duration of share receive calls (seconds, bucketed by DEFAULT_BUCKETS).",
            ),
        }
    }

    /// Process-global handle. First access registers the
    /// metrics into the global registry via the module-level
    /// `GLOBAL` static; subsequent access returns a clone.
    pub fn get() -> Self {
        GLOBAL.clone()
    }
}

/// Module-level lazy global handle.
static GLOBAL: Lazy<ShareMetrics> =
    Lazy::new(|| ShareMetrics::register(&OBSERVABILITY_GLOBAL));

/// Convenience function to get the global share metrics.
///
/// Equivalent to [`ShareMetrics::get`] — kept as a free
/// function so callers can `use a3net_share::share_metrics;`
/// without naming the handle type.
pub fn share_metrics() -> ShareMetrics {
    ShareMetrics::get()
}

/// Record a single receive completion: increments the
/// `receive_seconds` histogram by `elapsed` and bumps the
/// per-file / per-byte counters by `files` and `bytes`.
///
/// A free function (rather than a method on [`ShareMetrics`])
/// so callers don't have to import the handle just to record
/// one observation — the receiving code already has the
/// `ShareStats` in hand.
pub fn record_receive_complete(
    elapsed: Duration,
    files: usize,
    bytes: u64,
    had_error: bool,
) {
    let m = share_metrics();
    m.receive_seconds.observe(elapsed.as_secs_f64());
    m.receive_files_done.inc_by(files as u64);
    m.receive_bytes_done.inc_by(bytes);
    if had_error {
        m.receive_errors.inc();
    }
}

/// Record the manifest expectation: bumps
/// `receive_files_total` and `receive_bytes_total` by `files`
/// and `bytes`. Called once per receive when the manifest is
/// fully known but BEFORE the per-file loop runs (so the
/// `_total` series are observed even on early-failure paths
/// where the receive never produces any files).
pub fn record_receive_expected(files: usize, bytes: u64) {
    let m = share_metrics();
    m.receive_files_total.inc_by(files as u64);
    m.receive_bytes_total.inc_by(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn for_tests() -> (ShareMetrics, Arc<Registry>) {
        let registry = Arc::new(Registry::default());
        (ShareMetrics::register(&registry), registry)
    }

    #[test]
    fn register_creates_all_metrics() {
        let (m, registry) = for_tests();
        let snap = registry.snapshot();
        let names: Vec<String> =
            snap.metrics.iter().map(|x| x.name().to_string()).collect();
        assert!(names.contains(&"a3net_share_receive_bytes_total".to_string()));
        assert!(names.contains(&"a3net_share_receive_bytes_done".to_string()));
        assert!(names.contains(&"a3net_share_receive_files_total".to_string()));
        assert!(names.contains(&"a3net_share_receive_files_done".to_string()));
        assert!(names.contains(&"a3net_share_receive_errors_total".to_string()));
        assert!(names.contains(&"a3net_share_receive_seconds".to_string()));
        // Round-trip a couple of observations to confirm
        // the handles actually increment.
        m.receive_bytes_total.inc_by(1024);
        m.receive_bytes_done.inc_by(512);
        m.receive_files_total.inc_by(3);
        m.receive_files_done.inc_by(2);
        m.receive_errors.inc();
        m.receive_seconds.observe(0.123);
        assert_eq!(m.receive_bytes_total.get(), 1024);
        assert_eq!(m.receive_bytes_done.get(), 512);
        assert_eq!(m.receive_files_total.get(), 3);
        assert_eq!(m.receive_files_done.get(), 2);
        assert_eq!(m.receive_errors.get(), 1);
        assert_eq!(m.receive_seconds.count(), 1);
    }

    #[test]
    fn get_is_idempotent() {
        let a = ShareMetrics::get();
        let b = ShareMetrics::get();
        a.receive_bytes_done.inc_by(10);
        // The two handles share the same underlying counter
        // — a write through `a` is visible through `b`.
        assert!(b.receive_bytes_done.get() >= 10);
    }

    #[test]
    fn record_receive_expected_increments_totals() {
        let m = share_metrics();
        let before_total_files = m.receive_files_total.get();
        let before_total_bytes = m.receive_bytes_total.get();
        record_receive_expected(5, 12345);
        assert!(m.receive_files_total.get() >= before_total_files + 5);
        assert!(m.receive_bytes_total.get() >= before_total_bytes + 12345);
    }

    #[test]
    fn record_receive_complete_increments_done_and_histogram() {
        let m = share_metrics();
        let before_files = m.receive_files_done.get();
        let before_bytes = m.receive_bytes_done.get();
        let before_count = m.receive_seconds.count();
        record_receive_complete(Duration::from_millis(250), 3, 4096, false);
        assert!(
            m.receive_files_done.get() >= before_files + 3,
            "files_done should advance by >=3 ({} → {})",
            before_files,
            m.receive_files_done.get(),
        );
        assert!(
            m.receive_bytes_done.get() >= before_bytes + 4096,
            "bytes_done should advance by >=4096 ({} → {})",
            before_bytes,
            m.receive_bytes_done.get(),
        );
        assert!(
            m.receive_seconds.count() >= before_count + 1,
            "histogram should have at least one new observation",
        );
        // Error path: same helper, with the `had_error` flag
        // set. The error counter increments but the done
        // counters also advance (partial progress is still
        // progress).
        let before_errors = m.receive_errors.get();
        record_receive_complete(Duration::from_millis(10), 1, 0, true);
        assert!(
            m.receive_errors.get() >= before_errors + 1,
            "errors counter should advance by >=1",
        );
    }

    #[test]
    fn record_receive_complete_handles_empty_manifest() {
        // An empty manifest (zero files, zero bytes) is the
        // edge case `walk_import` produces for an empty
        // directory. The histogram still records the
        // observation; the counters don't move.
        let m = share_metrics();
        let before_files = m.receive_files_done.get();
        let before_bytes = m.receive_bytes_done.get();
        let before_count = m.receive_seconds.count();
        record_receive_complete(Duration::from_millis(0), 0, 0, false);
        // `>=` because other tests may have run in parallel
        // and incremented the same process-global counters.
        assert!(m.receive_files_done.get() >= before_files);
        assert!(m.receive_bytes_done.get() >= before_bytes);
        assert!(m.receive_seconds.count() >= before_count + 1);
    }
}