//! Metric primitives — Counter, Gauge, and the [`MetricKind`] tag.
//!
//! Histogram is a separate type ([`crate::histogram::Histogram`])
//! because it has substantially more state (a vector of bucket
//! counters) and a different exporter shape.
//!
//! ## Atomicity
//!
//! All increment / decrement operations use
//! [`Ordering::Relaxed`]. Cross-metric ordering (e.g. "dial
//! attempts" must be observed before "dial success") is **not**
//! guaranteed — the metrics surface is best-effort observability,
//! not a transactional log. The same convention is used in
//! `a3net-transport/src/iroh/discovery/diagnostics.rs`.
//!
//! ## Label storage
//!
//! Labeled variants of a counter are stored in a
//! `parking_lot::RwLock<HashMap<LabelSet, Arc<Inner>>>`. Hot path
//! is `lock().entry(set).or_insert_with(...)`; the read lock is
//! taken only on first observation of a new label set, and the
//! subsequent increments hit the `Arc<Inner>` directly without
//! touching the map. The map grows as new label sets are
//! observed; we do **not** cap cardinality here — that policy
//! belongs to the caller (see `DiscoveryDiagnostics` for the
//! existing `MAX_PROVENANCE_BUCKETS` cap).
//!
//! [`MetricKind`]: crate::metrics::MetricKind

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use parking_lot::RwLock;

use crate::labels::LabelSet;

/// Discriminator for the three metric kinds. Used by the
/// Prometheus exporter to choose the `# TYPE` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

impl MetricKind {
    /// Prometheus type string.
    pub fn as_prometheus_str(&self) -> &'static str {
        match self {
            MetricKind::Counter => "counter",
            MetricKind::Gauge => "gauge",
            MetricKind::Histogram => "histogram",
        }
    }
}

/// Common interface implemented by every metric primitive.
///
/// The exporter only needs [`Metric::kind`], [`Metric::name`],
/// [`Metric::help`], and [`Metric::render_prometheus`]. The
/// `Any` plumbing lets us put `Counter`, `Gauge`, and `Histogram`
/// behind the same `dyn Metric` trait object in the
/// [`Registry`](crate::registry::Registry).
///
/// [`as_any`](Metric::as_any) is used by the JSON exporter to
/// recover the concrete type (`Counter` / `Gauge` / `Histogram`)
/// for typed sample generation. It is a separate method rather
/// than a default-implemented `Any` supertrait so we don't
/// impose the `Any` bound on every implementor.
pub trait Metric: std::fmt::Debug + Send + Sync + 'static {
    fn kind(&self) -> MetricKind;
    fn name(&self) -> &str;
    fn help(&self) -> &str;

    /// Render the metric in Prometheus text format. Each
    /// implementation is responsible for emitting the `# HELP`
    /// and `# TYPE` lines plus the per-variant sample lines.
    fn render_prometheus(&self) -> String;

    /// Downcast to `&dyn Any` for typed access. Required
    /// because the JSON exporter needs to recover the concrete
    /// primitive type from a `dyn Metric` trait object.
    fn as_any(&self) -> &dyn std::any::Any;
}

// ─── Counter ────────────────────────────────────────────────────────────

/// Atomic, monotonically non-decreasing 64-bit counter.
///
/// Construct via [`Registry::register_counter`](crate::registry::Registry::register_counter)
/// or directly via [`Counter::new`] for tests / one-off metrics.
/// Direct construction does **not** register with the global
/// [`Registry`](crate::registry::Registry); the exporter won't
/// see it.
#[derive(Debug)]
pub struct Counter {
    name: String,
    help: String,
    /// Value for the unlabeled (or single, label-less) variant.
    inner: AtomicU64,
    /// Per-label-set value, populated lazily as new label sets
    /// are observed. The `RwLock` is taken only on a cache miss
    /// (first observation of a given `LabelSet`); subsequent
    /// increments go through the inner `AtomicU64` directly.
    labeled: RwLock<HashMap<LabelSet, Arc<AtomicU64>>>,
}

impl Counter {
    /// Construct a counter without registering it. The metric
    /// is **not** visible to the global exporter; this is mostly
    /// useful for unit tests. Production code goes through
    /// [`Registry::register_counter`](crate::registry::Registry::register_counter).
    pub fn new(name: impl Into<String>, help: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            help: help.into(),
            inner: AtomicU64::new(0),
            labeled: RwLock::new(HashMap::new()),
        }
    }

    /// Increment the unlabeled variant by 1.
    pub fn inc(&self) {
        self.inner.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the unlabeled variant by `n`.
    pub fn inc_by(&self, n: u64) {
        self.inner.fetch_add(n, Ordering::Relaxed);
    }

    /// Current value of the unlabeled variant.
    pub fn get(&self) -> u64 {
        self.inner.load(Ordering::Relaxed)
    }

    /// Increment the labeled variant for `labels` by 1.
    ///
    /// First observation of a given `LabelSet` inserts a new
    /// `AtomicU64` into the labeled map. Subsequent calls hit
    /// the inner atomic without touching the map.
    pub fn inc_labels(&self, labels: &LabelSet) {
        let inner = self.get_or_insert_labels(labels);
        inner.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the labeled variant by `n`.
    pub fn inc_labels_by(&self, labels: &LabelSet, n: u64) {
        let inner = self.get_or_insert_labels(labels);
        inner.fetch_add(n, Ordering::Relaxed);
    }

    /// Snapshot the labeled values for the Prometheus / JSON
    /// exporter. Returns `(LabelSet, count)` pairs sorted by
    /// the label set's `render()` form — this gives the
    /// exporter a stable, lexicographic order so the output
    /// is byte-deterministic across runs.
    pub fn labeled_snapshot(&self) -> Vec<(LabelSet, u64)> {
        let mut entries: Vec<(LabelSet, u64)> = self
            .labeled
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect();
        entries.sort_by_key(|a| a.0.render());
        entries
    }

    fn get_or_insert_labels(&self, labels: &LabelSet) -> Arc<AtomicU64> {
        // Fast path: read lock + clone of the inner Arc.
        if let Some(v) = self.labeled.read().get(labels) {
            return Arc::clone(v);
        }
        // Slow path: take the write lock, double-check after
        // (classic HashMap entry pattern), then insert.
        let mut guard = self.labeled.write();
        if let Some(v) = guard.get(labels) {
            return Arc::clone(v);
        }
        let arc = Arc::new(AtomicU64::new(0));
        guard.insert(labels.clone(), Arc::clone(&arc));
        arc
    }
}

impl Metric for Counter {
    fn kind(&self) -> MetricKind {
        MetricKind::Counter
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn help(&self) -> &str {
        &self.help
    }
    fn render_prometheus(&self) -> String {
        let mut out = String::with_capacity(128);
        out.push_str("# HELP ");
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(&self.help);
        out.push('\n');
        out.push_str("# TYPE ");
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(MetricKind::Counter.as_prometheus_str());
        out.push('\n');
        // Unlabeled sample line. Prometheus convention: when
        // labels are present the unlabeled sample is *omitted*
        // (you cannot mix a labeled and unlabeled value of the
        // same metric name). We emit it because we allow both
        // paths and the unlabeled value is meaningful on its
        // own; the exporter does not enforce that only one
        // path is used.
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(&self.inner.load(Ordering::Relaxed).to_string());
        out.push('\n');
        for (labels, value) in self.labeled_snapshot() {
            out.push_str(&self.name);
            out.push_str(&labels.render());
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        out
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─── Gauge ──────────────────────────────────────────────────────────────

/// Atomic, signed 64-bit gauge. Can increase, decrease, or be
/// set to an absolute value.
///
/// Use gauges for: current connection count, in-flight
/// subscriptions, active workers, anything that has a meaningful
/// "now" value that can go up or down.
#[derive(Debug)]
pub struct Gauge {
    name: String,
    help: String,
    inner: AtomicI64,
    labeled: RwLock<HashMap<LabelSet, Arc<AtomicI64>>>,
}

impl Gauge {
    /// Construct a gauge without registering it.
    pub fn new(name: impl Into<String>, help: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            help: help.into(),
            inner: AtomicI64::new(0),
            labeled: RwLock::new(HashMap::new()),
        }
    }

    /// Set the unlabeled value to `v`.
    pub fn set(&self, v: i64) {
        self.inner.store(v, Ordering::Relaxed);
    }

    /// Set the unlabeled value to `v` (float, scaled to
    /// micro-units). Implemented as `(v * 1_000_000) as i64`
    /// rounded to the nearest micro-unit (1e-6), stored as
    /// the integer part. This trades precision for
    /// atomicity — the alternative is CAS-loop bit-casting
    /// like `Histogram`'s `sum`, but gauge values are
    /// typically used for ratios / percentages where 6
    /// decimal places is more than enough.
    ///
    /// The integer-only atomic store is the **same** as
    /// [`set`](Self::set); the only difference is that
    /// callers that need fractional values (e.g. hit rate
    /// 75.42%) multiply by 1e6 before storing. The
    /// Prometheus exporter renders the value as an `i64`,
    /// so the operator needs to know the scaling factor —
    /// this is documented in the metric `help` text.
    pub fn set_f64(&self, v: f64) {
        let scaled = (v * 1_000_000.0).round() as i64;
        self.inner.store(scaled, Ordering::Relaxed);
    }

    /// Increment the unlabeled value by 1.
    pub fn inc(&self) {
        self.inner.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the unlabeled value by `n`. Saturates at `i64::MAX`
    /// rather than overflowing.
    ///
    /// DO-178C DAL-B (SR-4 storage quota): the blobstore used to
    /// do `for _ in 0..bytes { gauge.inc() }` which is O(N) and
    /// effectively hangs the runtime for multi-GB imports. This
    /// O(1) primitive is the only call site that should be used
    /// for bulk-size accounting.
    pub fn inc_by_u64(&self, n: u64) {
        // Saturating add at i64::MAX; never overflows. Compute in
        // `i128` so large `n` values don't get truncated to
        // `i64::MAX` before the add — matches the symmetric
        // `dec_by_u64` implementation.
        let cur = self.inner.load(Ordering::Relaxed);
        let next = (cur as i128)
            .saturating_add(n as i128)
            .clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        self.inner.store(next, Ordering::Relaxed);
    }

    /// Decrement the unlabeled value by `n`. Saturates at `i64::MIN`.
    pub fn dec_by_u64(&self, n: u64) {
        let cur = self.inner.load(Ordering::Relaxed);
        // Compute in `i128` to avoid wraparound when `n` exceeds
        // `i64::MAX`. Without the widening, the previous
        // implementation truncated `n` to `i64::MAX` before
        // subtracting — so `Gauge::set(20)` followed by
        // `dec_by_u64(u64::MAX)` produced `i64::MIN + 20` instead
        // of `i64::MIN`. Saturating `i128` arithmetic gives the
        // spec-correct answer for every input.
        let next = (cur as i128)
            .saturating_sub(n as i128)
            .clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        self.inner.store(next, Ordering::Relaxed);
    }

    /// Decrement the unlabeled value by 1.
    pub fn dec(&self) {
        self.inner.fetch_sub(1, Ordering::Relaxed);
    }

    /// Current value of the unlabeled variant.
    pub fn get(&self) -> i64 {
        self.inner.load(Ordering::Relaxed)
    }

    /// Set the labeled variant for `labels` to `v`.
    pub fn set_labels(&self, labels: &LabelSet, v: i64) {
        let inner = self.get_or_insert_labels(labels);
        inner.store(v, Ordering::Relaxed);
    }

    /// Increment the labeled variant.
    pub fn inc_labels(&self, labels: &LabelSet) {
        let inner = self.get_or_insert_labels(labels);
        inner.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the labeled variant.
    pub fn dec_labels(&self, labels: &LabelSet) {
        let inner = self.get_or_insert_labels(labels);
        inner.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn labeled_snapshot(&self) -> Vec<(LabelSet, i64)> {
        let mut entries: Vec<(LabelSet, i64)> = self
            .labeled
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect();
        entries.sort_by_key(|a| a.0.render());
        entries
    }

    fn get_or_insert_labels(&self, labels: &LabelSet) -> Arc<AtomicI64> {
        if let Some(v) = self.labeled.read().get(labels) {
            return Arc::clone(v);
        }
        let mut guard = self.labeled.write();
        if let Some(v) = guard.get(labels) {
            return Arc::clone(v);
        }
        let arc = Arc::new(AtomicI64::new(0));
        guard.insert(labels.clone(), Arc::clone(&arc));
        arc
    }
}

impl Metric for Gauge {
    fn kind(&self) -> MetricKind {
        MetricKind::Gauge
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn help(&self) -> &str {
        &self.help
    }
    fn render_prometheus(&self) -> String {
        let mut out = String::with_capacity(128);
        out.push_str("# HELP ");
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(&self.help);
        out.push('\n');
        out.push_str("# TYPE ");
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(MetricKind::Gauge.as_prometheus_str());
        out.push('\n');
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(&self.inner.load(Ordering::Relaxed).to_string());
        out.push('\n');
        for (labels, value) in self.labeled_snapshot() {
            out.push_str(&self.name);
            out.push_str(&labels.render());
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        out
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_inc_and_get() {
        let c = Counter::new("c", "help");
        assert_eq!(c.get(), 0);
        c.inc();
        c.inc();
        c.inc_by(5);
        assert_eq!(c.get(), 7);
    }

    #[test]
    fn counter_labels_first_observation_inserts() {
        let c = Counter::new("c", "help");
        let l1 = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
        let l2 = LabelSet::new([("topic".into(), "files".into())]).unwrap();
        c.inc_labels(&l1);
        c.inc_labels(&l1);
        c.inc_labels(&l2);
        c.inc_labels(&l1);
        let snap = c.labeled_snapshot();
        let lobby = snap
            .iter()
            .find(|(k, _)| k.render().contains("lobby"))
            .unwrap();
        let files = snap
            .iter()
            .find(|(k, _)| k.render().contains("files"))
            .unwrap();
        assert_eq!(lobby.1, 3);
        assert_eq!(files.1, 1);
    }

    #[test]
    fn gauge_set_inc_dec() {
        let g = Gauge::new("g", "help");
        assert_eq!(g.get(), 0);
        g.set(10);
        assert_eq!(g.get(), 10);
        g.inc();
        g.inc();
        g.dec();
        assert_eq!(g.get(), 11);
        g.set(-5);
        assert_eq!(g.get(), -5);
    }

    /// SR-4 storage quota: bulk size updates must be O(1).
    /// Regression: `for _ in 0..N { inc() }` was O(N) and effectively
    /// hung the runtime on multi-GB imports.
    #[test]
    fn gauge_inc_by_u64_saturates_at_max() {
        let g = Gauge::new("g", "help");
        g.inc_by_u64(100);
        assert_eq!(g.get(), 100);
        g.inc_by_u64(2_000_000_000);
        assert_eq!(g.get(), 2_000_000_100);
        // Saturate at i64::MAX — no overflow.
        g.inc_by_u64(u64::MAX);
        assert_eq!(g.get(), i64::MAX);
    }

    #[test]
    fn gauge_dec_by_u64_saturates_at_min() {
        let g = Gauge::new("g", "help");
        g.set(50);
        g.dec_by_u64(30);
        assert_eq!(g.get(), 20);
        // Saturate at i64::MIN.
        g.dec_by_u64(u64::MAX);
        assert_eq!(g.get(), i64::MIN);
    }

    #[test]
    fn gauge_inc_by_u64_zero_is_noop() {
        let g = Gauge::new("g", "help");
        g.set(42);
        g.inc_by_u64(0);
        assert_eq!(g.get(), 42);
    }

    #[test]
    fn gauge_labels_track_independently() {
        let g = Gauge::new("g", "help");
        let a = LabelSet::new([("k".into(), "a".into())]).unwrap();
        let b = LabelSet::new([("k".into(), "b".into())]).unwrap();
        g.set_labels(&a, 5);
        g.set_labels(&b, -2);
        g.inc_labels(&a);
        g.dec_labels(&b);
        let snap = g.labeled_snapshot();
        let av = snap
            .iter()
            .find(|(k, _)| k.render().contains("\"a\""))
            .unwrap();
        let bv = snap
            .iter()
            .find(|(k, _)| k.render().contains("\"b\""))
            .unwrap();
        assert_eq!(av.1, 6);
        assert_eq!(bv.1, -3);
    }

    #[test]
    fn counter_render_prometheus_includes_help_type_and_samples() {
        let c = Counter::new("a3net_test_total", "test counter help");
        c.inc_by(3);
        let l = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
        c.inc_labels(&l);
        c.inc_labels(&l);
        let out = c.render_prometheus();
        assert!(out.contains("# HELP a3net_test_total test counter help"));
        assert!(out.contains("# TYPE a3net_test_total counter"));
        assert!(out.contains("a3net_test_total 3"));
        // Labeled variant rendered with sorted label order and
        // Prometheus escaping.
        assert!(out.contains(r#"a3net_test_total{topic="lobby"} 2"#));
    }

    #[test]
    fn gauge_render_prometheus_includes_negative_value() {
        let g = Gauge::new("g", "help");
        g.set(-7);
        let out = g.render_prometheus();
        assert!(out.contains("g -7"));
    }

    #[test]
    fn metric_kind_prom_str() {
        assert_eq!(MetricKind::Counter.as_prometheus_str(), "counter");
        assert_eq!(MetricKind::Gauge.as_prometheus_str(), "gauge");
        assert_eq!(MetricKind::Histogram.as_prometheus_str(), "histogram");
    }
}
