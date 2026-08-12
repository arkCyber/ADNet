//! Histogram metric — distribution of observed values into
//! pre-defined buckets.
//!
//! ADNet's histogram is intentionally **simple**:
//!
//! - Fixed bucket layout defined by [`DEFAULT_BUCKETS`]
//!   (8 buckets: 1ms, 10ms, 100ms, 500ms, 1s, 5s, 10s, +Inf).
//! - One `AtomicU64` per bucket + a `count` counter + a `sum`
//!   stored as the bits of an `f64`.
//! - Lock-free observation (`observe` is the hot path).
//!
//! We deliberately do **not** use a streaming algorithm (HDR,
//! t-digest) — those are required when bucket layout is unknown
//! at construction or when memory is at a premium. ADNet's
//! histograms all have well-known latency / size distributions
//! (handshake RTT, message size, etc.) so a fixed layout gives
//! the operator the most predictable Prometheus output.
//!
//! ## When to extend
//!
//! PR1 keeps the bucket set fixed. A future PR2/3 may add a
//! `Histogram::with_buckets(&[...])` constructor for callers
//! that need a different layout; the `count` and `sum` fields
//! stay the same.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::labels::LabelSet;
use crate::metrics::{Metric, MetricKind};

/// Default histogram bucket layout (Prometheus-style cumulative
/// `le` semantics). Values in **seconds**.
///
/// ```text
///   0.001, 0.01, 0.1, 0.5, 1, 5, 10, +Inf
/// ```
pub const DEFAULT_BUCKETS: &[f64] = &[0.001, 0.01, 0.1, 0.5, 1.0, 5.0, 10.0, f64::INFINITY];

/// Distribution histogram. One bucket set per `LabelSet`.
#[derive(Debug)]
pub struct Histogram {
    name: String,
    help: String,
    /// Bucket layout used for both the unlabeled and labeled
    /// variants. We share the layout across variants rather
    /// than storing it per-inner because every ADNet histogram
    /// uses the same layout for all label combinations — a
    /// per-label-set layout would be over-engineering.
    buckets: Vec<f64>,
    /// Unlabeled histogram: pre-allocated bucket counters +
    /// `count` + `sum`.
    inner: HistogramInner,
    /// Per-label-set bucket arrays. Allocated lazily on first
    /// observation; the inner `RwLock` is only touched on the
    /// cache-miss path. Each entry is an `Arc<HistogramInner>`
    /// so `get_or_insert_labels` can hand out a long-lived
    /// reference to the inner state without holding the lock
    /// for the duration of `observe`.
    labeled: parking_lot::RwLock<Vec<(LabelSet, Arc<HistogramInner>)>>,
}

/// Per-histogram state shared between the labeled and unlabeled
/// paths. Cheap to construct (8 buckets + 2 atomics = 10 words).
#[derive(Debug)]
struct HistogramInner {
    /// One `AtomicU64` per bucket. The bucket's value is the
    /// number of observations whose sample was `<= bucket_le`.
    /// Buckets are cumulative — see [`Histogram::observe`].
    buckets: Vec<AtomicU64>,
    /// Total number of observations.
    count: AtomicU64,
    /// Sum of all observations, stored as the bits of an `f64`.
    /// We use the bit-cast trick to keep the field
    /// `AtomicU64`-shaped while still representing an `f64`; the
    /// load + bit-cast is a single atomic op on every platform
    /// we care about.
    sum_bits: AtomicU64,
}

impl HistogramInner {
    fn new(buckets: &[f64]) -> Self {
        Self {
            buckets: (0..buckets.len()).map(|_| AtomicU64::new(0)).collect(),
            count: AtomicU64::new(0),
            sum_bits: AtomicU64::new(0.0_f64.to_bits()),
        }
    }

    fn observe(&self, value: f64) {
        // Cumulative bucket semantics: walk the bucket array
        // and increment every bucket whose `le` is `>= value`.
        // We walk *all* buckets (8 entries) — cheap, and the
        // alternative (binary search) is slower for 8 elements.
        for bucket in &self.buckets {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
        // Decrement the buckets whose `le` is below the value,
        // so only buckets with `le >= value` end up with a
        // count. The "increment then decrement" pattern is the
        // standard trick for atomic cumulative histograms.
        //   * Per the Prometheus spec, the bucket is the number
        //     of observations with `value <= le`, so for value
        //     0.05s the buckets 0.001, 0.01, 0.1 all carry the
        //     sample (0.05 <= 0.1).
        // We use the **le** from DEFAULT_BUCKETS by index —
        // see `Histogram::observe_indexed` for the only public
        // entry point.
        // The decrement loop is below in `Histogram::observe`.
        self.count.fetch_add(1, Ordering::Relaxed);
        // `sum_bits` holds an `f64` via bit-cast. We do a CAS
        // loop to atomically add `value` to the current sum.
        // A naive `load + add + store` loses updates under
        // contention — multiple threads can read the same
        // old value, compute the same new value, and store
        // it, dropping one thread's contribution. The CAS
        // loop detects the lost-update and retries.
        loop {
            let current_bits = self.sum_bits.load(Ordering::Relaxed);
            let current = f64::from_bits(current_bits);
            let new = current + value;
            // Allow NaN to fall through — the final stored
            // value may be NaN under extreme contention
            // (catastrophic cancellation), but we never
            // observe NaN on a single-threaded path.
            let new_bits = new.to_bits();
            if self
                .sum_bits
                .compare_exchange_weak(current_bits, new_bits, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    fn snapshot(&self, buckets: &[f64]) -> HistogramSnapshot {
        HistogramSnapshot {
            buckets: self
                .buckets
                .iter()
                .zip(buckets.iter())
                .map(|(c, le)| (*le, c.load(Ordering::Relaxed)))
                .collect(),
            count: self.count.load(Ordering::Relaxed),
            sum: f64::from_bits(self.sum_bits.load(Ordering::Relaxed)),
        }
    }
}

impl Histogram {
    /// Construct a histogram with the [`DEFAULT_BUCKETS`] layout.
    /// Does **not** register with the global registry.
    pub fn new(name: impl Into<String>, help: impl Into<String>) -> Self {
        Self::with_buckets(name, help, DEFAULT_BUCKETS)
    }

    /// Construct a histogram with a custom bucket layout (le
    /// values, seconds). The last bucket is implicitly
    /// `+Inf` and does not need to be supplied.
    pub fn with_buckets(name: impl Into<String>, help: impl Into<String>, buckets: &[f64]) -> Self {
        let mut owned: Vec<f64> = buckets.to_vec();
        if !owned.iter().any(|b| b.is_infinite()) {
            owned.push(f64::INFINITY);
        }
        let inner = HistogramInner::new(&owned);
        Self {
            name: name.into(),
            help: help.into(),
            buckets: owned,
            inner,
            labeled: parking_lot::RwLock::new(Vec::new()),
        }
    }

    /// Record a single observation on the unlabeled variant.
    pub fn observe(&self, value: f64) {
        self.observe_into(&self.inner, value);
    }

    /// Record a single observation on the labeled variant.
    pub fn observe_labels(&self, labels: &LabelSet, value: f64) {
        let inner = self.get_or_insert_labels(labels);
        self.observe_into(&inner, value);
    }

    /// Snapshot of one histogram variant (unlabeled or labeled).
    pub fn snapshot(&self) -> HistogramSnapshot {
        self.inner.snapshot(&self.buckets)
    }

    /// Snapshot of all labeled variants. Returned as `(label_set,
    /// snapshot)` pairs in lexicographic order of the label
    /// set's `render()` form. The unlabeled variant is
    /// **not** included — use [`Histogram::snapshot`] for
    /// that.
    pub fn labeled_snapshots(&self) -> Vec<(LabelSet, HistogramSnapshot)> {
        let mut entries: Vec<(LabelSet, HistogramSnapshot)> = self
            .labeled
            .read()
            .iter()
            .map(|(k, inner)| (k.clone(), inner.snapshot(&self.buckets)))
            .collect();
        entries.sort_by_key(|a| a.0.render());
        entries
    }

    /// Number of observations on the unlabeled variant.
    pub fn count(&self) -> u64 {
        self.inner.count.load(Ordering::Relaxed)
    }

    /// Sum of all unlabeled observations.
    pub fn sum(&self) -> f64 {
        f64::from_bits(self.inner.sum_bits.load(Ordering::Relaxed))
    }

    fn observe_into(&self, inner: &HistogramInner, value: f64) {
        inner.observe(value);
        // Apply the "decrement buckets below le" rule: walk
        // the bucket array, comparing the cumulative `le` to
        // `value`, and decrementing the buckets whose `le` is
        // strictly less than `value`. Buckets are sorted in
        // ascending order, so we can stop at the first
        // bucket with `le >= value`.
        for (i, le) in self.buckets.iter().enumerate() {
            if *le < value {
                inner.buckets[i].fetch_sub(1, Ordering::Relaxed);
            } else {
                break;
            }
        }
    }

    fn get_or_insert_labels(&self, labels: &LabelSet) -> Arc<HistogramInner> {
        // Fast path: read lock + Arc::clone (cheap).
        {
            let guard = self.labeled.read();
            if let Some((_, inner)) = guard.iter().find(|(k, _)| k == labels) {
                return Arc::clone(inner);
            }
        }
        // Slow path: take the write lock, double-check
        // (the second caller might have inserted between
        // our read unlock and write lock), then insert
        // a fresh `Arc<HistogramInner>` and return a
        // clone of it.
        let mut guard = self.labeled.write();
        if let Some((_, inner)) = guard.iter().find(|(k, _)| k == labels) {
            return Arc::clone(inner);
        }
        let fresh = Arc::new(HistogramInner::new(&self.buckets));
        guard.push((labels.clone(), Arc::clone(&fresh)));
        fresh
    }
}

/// Point-in-time view of one histogram variant. Used by the
/// Prometheus exporter.
#[derive(Debug, Clone, Serialize)]
pub struct HistogramSnapshot {
    /// `(le, count)` pairs in ascending `le` order.
    pub buckets: Vec<(f64, u64)>,
    pub count: u64,
    pub sum: f64,
}

impl HistogramSnapshot {
    /// Render as the Prometheus histogram lines, including the
    /// `_count` and `_sum` lines. The histogram name passed in
    /// is the **base** name; this method emits:
    ///
    /// - `name_bucket{le="0.001"} N`
    /// - `name_bucket{le="0.01"} N`
    /// - ...
    /// - `name_bucket{le="+Inf"} N`
    /// - `name_count N`
    /// - `name_sum S`
    pub fn render_prometheus(&self, name: &str, labels: &LabelSet) -> String {
        // For each bucket line we emit
        // `<name>_bucket{<inner_labels>,le="<le>"} N`. The
        // `<inner_labels>` is the caller's label set without
        // the outer `{` / `}` — see `LabelSet::render_inner`.
        let inner = labels.render_inner();
        let mut out = String::with_capacity(256);
        for (le, count) in &self.buckets {
            out.push_str(name);
            out.push_str("_bucket{");
            if !inner.is_empty() {
                out.push_str(&inner);
                out.push(',');
            }
            out.push_str("le=\"");
            if le.is_infinite() {
                out.push_str("+Inf");
            } else {
                out.push_str(&format_float(*le));
            }
            out.push_str("\"} ");
            out.push_str(&count.to_string());
            out.push('\n');
        }
        // `_count` and `_sum` use the full `LabelSet::render`
        // form (with the outer `{...}`), but only when the
        // label set is non-empty — for the unlabeled variant
        // we emit `name_count N` without a labels block.
        out.push_str(name);
        out.push_str("_count");
        if !labels.is_empty() {
            out.push_str(&labels.render());
        }
        out.push(' ');
        out.push_str(&self.count.to_string());
        out.push('\n');
        out.push_str(name);
        out.push_str("_sum");
        if !labels.is_empty() {
            out.push_str(&labels.render());
        }
        out.push(' ');
        out.push_str(&format_float(self.sum));
        out.push('\n');
        out
    }
}

/// Format an `f64` for the Prometheus text format. We avoid the
/// `{}` formatter because it produces "1.5" for `1.5` but "1"
/// for `1.0` — Prometheus is happy with both, but consistent
/// formatting makes the snapshot tests deterministic.
fn format_float(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 {
            "+Inf".into()
        } else {
            "-Inf".into()
        }
    } else if v == v.trunc() && v.abs() < 1e15 {
        // Whole number — render without a trailing `.0` so
        // `1.0` becomes `"1"`, matching Prometheus convention.
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

impl Metric for Histogram {
    fn kind(&self) -> MetricKind {
        MetricKind::Histogram
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn help(&self) -> &str {
        &self.help
    }
    fn render_prometheus(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("# HELP ");
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(&self.help);
        out.push('\n');
        out.push_str("# TYPE ");
        out.push_str(&self.name);
        out.push(' ');
        out.push_str(MetricKind::Histogram.as_prometheus_str());
        out.push('\n');
        // Unlabeled variant.
        let snap = self.snapshot();
        out.push_str(&snap.render_prometheus(&self.name, &LabelSet::EMPTY));
        // Labeled variants.
        for (labels, snap) in self.labeled_snapshots() {
            out.push_str(&snap.render_prometheus(&self.name, &labels));
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
    use crate::labels::LabelSet;

    #[test]
    fn observe_increments_count_and_sum() {
        let h = Histogram::new("h", "help");
        h.observe(0.05);
        h.observe(0.5);
        h.observe(2.0);
        assert_eq!(h.count(), 3);
        assert!((h.sum() - 2.55).abs() < 1e-9);
    }

    #[test]
    fn bucket_layout_is_cumulative() {
        // Three observations: 0.0005 (under 0.001), 0.05
        // (between 0.01 and 0.1), 5.0 (between 1 and 5).
        // Buckets 0.001, 0.01, 0.1, 0.5, 1, 5, 10, +Inf.
        // Cumulative counts:
        //   0.001: 1 (0.0005)
        //   0.01:  1
        //   0.1:   2 (0.0005, 0.05)
        //   0.5:   2
        //   1:     2
        //   5:     3 (0.0005, 0.05, 5.0)
        //   10:    3
        //   +Inf:  3
        let h = Histogram::new("h", "help");
        h.observe(0.0005);
        h.observe(0.05);
        h.observe(5.0);
        let snap = h.snapshot();
        assert_eq!(snap.buckets[0], (0.001, 1));
        assert_eq!(snap.buckets[1], (0.01, 1));
        assert_eq!(snap.buckets[2], (0.1, 2));
        assert_eq!(snap.buckets[3], (0.5, 2));
        assert_eq!(snap.buckets[4], (1.0, 2));
        assert_eq!(snap.buckets[5], (5.0, 3));
        assert_eq!(snap.buckets[6], (10.0, 3));
        assert_eq!(snap.buckets[7], (f64::INFINITY, 3));
    }

    #[test]
    fn labeled_observations_track_independently() {
        let h = Histogram::new("h", "help");
        let a = LabelSet::new([("topic".into(), "a".into())]).unwrap();
        let b = LabelSet::new([("topic".into(), "b".into())]).unwrap();
        h.observe_labels(&a, 0.1);
        h.observe_labels(&a, 0.2);
        h.observe_labels(&b, 5.0);
        // `a`: 2 observations in (0.1, 0.5] bucket range.
        // `b`: 1 observation in (1, 5] bucket range.
        let snaps = h.labeled_snapshots();
        assert_eq!(snaps.len(), 2);
        let a_snap = snaps
            .iter()
            .find(|(k, _)| k.render().contains("\"a\""))
            .unwrap();
        let b_snap = snaps
            .iter()
            .find(|(k, _)| k.render().contains("\"b\""))
            .unwrap();
        assert_eq!(a_snap.1.count, 2);
        assert!((a_snap.1.sum - 0.3).abs() < 1e-9);
        assert_eq!(b_snap.1.count, 1);
        assert!((b_snap.1.sum - 5.0).abs() < 1e-9);
    }

    #[test]
    fn render_prometheus_includes_help_type_buckets_count_sum() {
        let h = Histogram::new("adnet_test_seconds", "test histogram");
        h.observe(0.05);
        h.observe(5.0);
        let out = h.render_prometheus();
        assert!(out.contains("# HELP adnet_test_seconds test histogram"));
        assert!(out.contains("# TYPE adnet_test_seconds histogram"));
        // Bucket lines (cumulative).
        assert!(out.contains(r#"adnet_test_seconds_bucket{le="0.1"} 1"#));
        assert!(out.contains(r#"adnet_test_seconds_bucket{le="5"} 2"#));
        assert!(out.contains(r#"adnet_test_seconds_bucket{le="+Inf"} 2"#));
        assert!(out.contains("adnet_test_seconds_count 2"));
        assert!(out.contains("adnet_test_seconds_sum 5.05"));
    }

    #[test]
    fn render_prometheus_for_labeled_variant_includes_labels() {
        let h = Histogram::new("h", "help");
        let l = LabelSet::new([("topic".into(), "lobby".into())]).unwrap();
        h.observe_labels(&l, 0.5);
        let out = h.render_prometheus();
        assert!(out.contains(r#"h_bucket{topic="lobby",le="0.5"} 1"#));
        assert!(out.contains(r#"h_count{topic="lobby"} 1"#));
        assert!(out.contains(r#"h_sum{topic="lobby"} 0.5"#));
    }

    #[test]
    fn custom_buckets_observed_correctly() {
        // Use a custom layout with one tiny bucket + +Inf.
        let h = Histogram::with_buckets("h", "help", &[0.0001]);
        h.observe(0.0005);
        let snap = h.snapshot();
        // 2 buckets: 0.0001 and +Inf.
        assert_eq!(snap.buckets.len(), 2);
        // 0.0005 > 0.0001, so only +Inf counts the observation.
        assert_eq!(snap.buckets[0], (0.0001, 0));
        assert_eq!(snap.buckets[1], (f64::INFINITY, 1));
    }

    #[test]
    fn format_float_handles_whole_numbers_and_special_values() {
        assert_eq!(format_float(1.0), "1");
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(format_float(0.0), "0");
        assert_eq!(format_float(f64::INFINITY), "+Inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-Inf");
        assert_eq!(format_float(f64::NAN), "NaN");
    }
}
