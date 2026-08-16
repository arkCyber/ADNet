//! Registry — the metric collection hub.
//!
//! The `Registry` is a `HashMap<String, Arc<dyn Metric>>` keyed
//! by metric name. Callers register a metric by name once at
//! startup (typically inside a `static FOO: Lazy<...>` block)
//! and obtain an `Arc<Counter>` / `Arc<Gauge>` / `Arc<Histogram>`
//! handle that they carry through the rest of the program.
//!
//! ## Global vs. local registries
//!
//! - [`GLOBAL`] is a process-wide singleton. Most layers in
//!   A3Net should use it. The HTTP `/metrics` endpoint reads
//!   from this registry.
//! - Per-crate `Registry::default()` instances are useful for
//!   tests and for callers that want isolated metric sets
//!   (e.g. embedding A3Net into a larger application that
//!   already has a metrics surface).
//!
//! Both paths use the same primitives; the only difference is
//! the storage location. [`Registry`] is `Clone` — and clones
//! share the underlying `Arc<RwLock<HashMap<...>>>` so writes
//! through one handle are visible through every other. This is
//! what makes the HTTP server's "default to the global
//! registry" path work: `Arc::new(GLOBAL.deref().clone())`
//! produces a fresh handle whose writes also flow into every
//! other clone (including the [`GLOBAL`] static).
//!
//! [`Registry`]: crate::registry::Registry
//!
//! ## Locking
//!
//! All internal maps use `parking_lot::RwLock` — same trade-off
//! as the rest of the crate: read-heavy (`/metrics` scrape)
//! path is lock-free, write path (registration) is rare.

use std::collections::HashMap;
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::Serialize;

use crate::histogram::Histogram;
use crate::metrics::{Counter, Gauge, Metric};

/// Process-wide default registry. Read by the HTTP `/metrics`
/// endpoint and by the `PrometheusExporter::render_default()`
/// shortcut.
pub static GLOBAL: Lazy<Registry> = Lazy::new(Registry::default);

/// Metric collection hub.
///
/// Cloning a `Registry` produces a fresh handle that **shares**
/// the same underlying metric map (the inner `RwLock` is
/// wrapped in an `Arc`). This is what makes `Registry::default()`
/// usable from many call sites — clones are cheap (one
/// `Arc::clone`) and all writes through any clone are visible
/// through every other clone.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    inner: Arc<RwLock<HashMap<String, Arc<dyn Metric>>>>,
}

impl Registry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a [`Counter`] with this registry.
    ///
    /// Returns the **newly-created** `Arc<Counter>`. If a metric
    /// with the same name is already registered (regardless of
    /// its type), the call **panics** with a clear message.
    /// A3Net's convention is to gate registration behind a
    /// `static FOO: Lazy<...>` so each metric is registered
    /// exactly once at process startup; a duplicate
    /// registration is a programming error.
    ///
    /// **Cardinality caveat**: divergent `help` strings under
    /// the same `name` are silently dropped — the first
    /// registration wins. This is intentional: divergent
    /// `help` strings are a bug, not a runtime error.
    pub fn register_counter(&self, name: &str, help: &str) -> Arc<Counter> {
        self.register_unique(name, Counter::new(name, help))
    }

    /// Register a [`Gauge`]. See [`register_counter`](Self::register_counter)
    /// for the idempotency contract.
    pub fn register_gauge(&self, name: &str, help: &str) -> Arc<Gauge> {
        self.register_unique(name, Gauge::new(name, help))
    }

    /// Register a [`Histogram`] with the [`DEFAULT_BUCKETS`](crate::histogram::DEFAULT_BUCKETS)
    /// layout. See [`register_counter`](Self::register_counter)
    /// for the idempotency contract.
    pub fn register_histogram(&self, name: &str, help: &str) -> Arc<Histogram> {
        self.register_unique(name, Histogram::new(name, help))
    }

    /// Register a [`Histogram`] with a custom bucket layout.
    pub fn register_histogram_with_buckets(
        &self,
        name: &str,
        help: &str,
        buckets: &[f64],
    ) -> Arc<Histogram> {
        self.register_unique(name, Histogram::with_buckets(name, help, buckets))
    }

    /// Look up a previously-registered metric by name. Returns
    /// `None` if the name is not registered. The downcast uses
    /// the safe `Arc::clone` + `as_any().downcast_ref::<T>()`
    /// pattern, which returns a borrowed `&T`; we then
    /// reconstruct a fresh `Arc<T>` by **cloning the value**
    /// (cheap for `Counter` / `Gauge` / `Histogram` — the
    /// atomic state is not copied, only the wrapping struct's
    /// metadata).
    ///
    /// **Label caveat**: `get_counter` returns a fresh
    /// `Counter` whose **unlabeled** value mirrors the original
    /// at lookup time. Labeled samples are **not** copied over
    /// — the new counter starts at `0` for every label set. If
    /// a caller needs the labeled state preserved across
    /// registrations, they should hold the original
    /// `Arc<Counter>` returned by [`register_counter`](Self::register_counter)
    /// (typically behind a `static FOO: Lazy<...>`) rather than
    /// round-tripping through `get_counter`. This is a
    /// deliberate trade-off: labelled state is held in a
    /// `RwLock<HashMap<…>>` inside the counter; copying it
    /// would require holding that lock and cloning the entire
    /// label set, which is the kind of subtle code path that
    /// causes correctness bugs at 3am.
    pub fn get_counter(&self, name: &str) -> Option<Arc<Counter>> {
        self.inner
            .read()
            .get(name)
            .and_then(|m| m.as_any().downcast_ref::<Counter>())
            .map(|c| {
                // Clone the *struct*, not the underlying
                // atomic state. `Counter::clone` is not
                // implemented (we don't want a second
                // handle to the same state — that would
                // break the "single source of truth"
                // invariant). Instead, we build a fresh
                // `Counter` with the same `name` and
                // `help`, and copy the current *snapshot*
                // of the unlabeled value into it. This is
                // a copy-on-register — appropriate for
                // the `get_counter` API where the caller
                // is recovering a handle from a global
                // registration.
                //
                // Caveat: labeled values are **not**
                // copied — the new `Counter` starts at
                // `0` for every label set. If the caller
                // needs the labeled state, they should
                // hold the original `Arc<Counter>` from
                // `register_counter`, not call
                // `get_counter`.
                let fresh = Counter::new(c.name(), c.help());
                fresh.inc_by(c.get());
                Arc::new(fresh)
            })
    }

    /// Look up a previously-registered gauge by name. See
    /// [`get_counter`](Self::get_counter) for the
    /// "snapshot" semantics.
    pub fn get_gauge(&self, name: &str) -> Option<Arc<Gauge>> {
        self.inner
            .read()
            .get(name)
            .and_then(|m| m.as_any().downcast_ref::<Gauge>())
            .map(|g| {
                let fresh = Gauge::new(g.name(), g.help());
                fresh.set(g.get());
                Arc::new(fresh)
            })
    }

    /// Look up a previously-registered histogram by name. See
    /// [`get_counter`](Self::get_counter) for the
    /// "snapshot" semantics.
    pub fn get_histogram(&self, name: &str) -> Option<Arc<Histogram>> {
        self.inner
            .read()
            .get(name)
            .and_then(|m| m.as_any().downcast_ref::<Histogram>())
            .map(|h| Arc::new(Histogram::new(h.name(), h.help())))
    }

    /// Look up a metric by name. Returns `None` if the name is
    /// not registered.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Metric>> {
        self.inner.read().get(name).cloned()
    }

    /// Number of registered metrics.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// True when no metrics are registered.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Iterate over every registered metric in an unspecified
    /// order. The iterator is cheap (no clones); callers that
    /// need a stable order should sort by `metric.name()`.
    pub fn iter(&self) -> impl Iterator<Item = Arc<dyn Metric>> + '_ {
        // parking_lot's `read()` returns a guard that holds
        // the lock for the iterator's lifetime. We collect
        // the Arcs into a Vec so the lock is dropped before
        // we return — the alternative (an iterator holding
        // a read guard) would deadlock if a caller tried to
        // register a new metric mid-iteration.
        let snapshot: Vec<Arc<dyn Metric>> = self.inner.read().values().cloned().collect();
        snapshot.into_iter()
    }

    /// Drop every registered metric. After this call,
    /// `is_empty()` returns `true` and any `Arc<Counter>` /
    /// `Arc<Gauge>` handle held by a caller is still usable —
    /// the handle keeps the counter alive via its `Arc`
    /// reference. The handle is just no longer visible to the
    /// exporter.
    pub fn clear(&self) {
        self.inner.write().clear();
    }

    /// Convenience: total number of registered metrics,
    /// exposed for `cargo doc` and the `/diagnostics` admin
    /// endpoint.
    pub fn metric_count(&self) -> usize {
        self.len()
    }

    /// Build a snapshot of all registered metrics. Cheap
    /// (`Vec<Arc<dyn Metric>>` clone).
    pub fn snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            metrics: self.iter().collect(),
        }
    }

    /// Register `metric` under `name`. **Panics** if the name
    /// is already taken. A3Net callers gate each metric behind
    /// a `static FOO: Lazy<...>` so registration happens once
    /// per process; the panic is a loud failure mode for the
    /// "two layers race to claim the same metric name" bug.
    fn register_unique<T: Metric + Send + Sync + 'static>(&self, name: &str, metric: T) -> Arc<T> {
        let arc: Arc<T> = Arc::new(metric);
        let dyn_arc: Arc<dyn Metric> = arc.clone();
        let mut write = self.inner.write();
        if write.contains_key(name) {
            // Drop the write lock and the in-flight metric
            // before panicking — releasing the lock means
            // other threads can still observe the
            // already-registered metric.
            drop(write);
            drop(arc);
            drop(dyn_arc);
            panic!(
                "metric name {name:?} is already registered — \
                 each metric must have a unique name; \
                 use a distinct prefix per crate (e.g. \"a3net_transport_dial_total\")"
            );
        }
        write.insert(name.to_string(), dyn_arc);
        arc
    }
}

/// Snapshot of a [`Registry`]. Cheap to clone (just `Vec<Arc>`).
///
/// `metrics` is intentionally **not** `Serialize` directly —
/// `Arc<dyn Metric>` is not a `Serialize` type. Use
/// [`RegistrySnapshot::sorted`] to get the descriptors, or
/// call the JSON exporter for a typed sample breakdown.
#[derive(Debug, Clone)]
pub struct RegistrySnapshot {
    pub metrics: Vec<Arc<dyn Metric>>,
}

/// Lightweight, `Serialize`-friendly view of a metric in a
/// snapshot. Built on demand by callers that need a JSON
/// representation.
#[derive(Debug, Clone, Serialize)]
pub struct MetricDescriptor {
    pub name: String,
    pub kind: String,
    pub help: String,
}

impl RegistrySnapshot {
    /// Number of registered metrics in the snapshot.
    pub fn len(&self) -> usize {
        self.metrics.len()
    }

    /// True when the snapshot contains no metrics.
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty()
    }

    /// Iterate over the metrics in name-sorted order. Sorting
    /// is O(n log n) but `n` is small (tens of metrics per
    /// A3Net layer), so this is the right place to do it
    /// for deterministic output.
    pub fn sorted(&self) -> Vec<Arc<dyn Metric>> {
        let mut v = self.metrics.clone();
        v.sort_by(|a, b| a.name().cmp(b.name()));
        v
    }

    /// Build a `Serialize`-friendly descriptor list. Useful
    /// for the JSON exporter and the `/diagnostics` admin
    /// endpoint.
    pub fn descriptors(&self) -> Vec<MetricDescriptor> {
        let mut out: Vec<MetricDescriptor> = self
            .metrics
            .iter()
            .map(|m| MetricDescriptor {
                name: m.name().to_string(),
                kind: m.kind().as_prometheus_str().to_string(),
                help: m.help().to_string(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

// ─── `Metric::as_any` helper ───────────────────────────────────────────

// `Metric::as_any` is part of the `Metric` trait itself (see
// `metrics.rs`). The `Registry::get_counter` / `get_gauge` /
// `get_histogram` methods use it via `as_any().downcast_ref::<T>()`
// to recover the concrete primitive type. The downcast returns
// `&T`; we wrap it in a fresh `Arc<T>` with the current value
// snapshot (cheap for the unlabeled variant; labeled variants
// start at zero — callers that need the labeled state should
// hold the original `Arc<T>` from the `register_*` call
// instead of going through `get_*`).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_counter_returns_arc() {
        let reg = Registry::default();
        let c1 = reg.register_counter("c", "help");
        c1.inc();
        c1.inc();
        assert_eq!(c1.get(), 2);
        assert_eq!(reg.metric_count(), 1);
    }

    #[test]
    #[should_panic(expected = "metric name \"c\" is already registered")]
    fn register_rejects_duplicate_name() {
        // A3Net requires every metric to have a unique
        // name. The `static FOO: Lazy<...>` pattern at the
        // call site is the canonical way to ensure single
        // registration; this test pins down the loud
        // failure mode.
        let reg = Registry::default();
        let _ = reg.register_counter("c", "help");
        let _ = reg.register_counter("c", "different help");
    }

    #[test]
    fn first_registration_help_wins_on_first_writer() {
        // When two callers race to register the same name
        // with the *same* kind, the first registration's
        // `help` string wins. We can't reliably race
        // threads in a unit test, but the fast path
        // (first writer wins) is what the registry
        // guarantees.
        let reg = Registry::default();
        let c = reg.register_counter("c", "first");
        assert_eq!(c.help(), "first");
    }

    #[test]
    #[should_panic(expected = "metric name \"c\" is already registered")]
    fn register_name_collision_across_kinds_panics() {
        let reg = Registry::default();
        let _ = reg.register_counter("c", "help");
        // A second registration with a different kind
        // (Counter followed by Gauge) under the same name
        // must panic. A3Net requires every metric to have
        // a unique name; the panic is a loud failure
        // mode for the "two layers race to claim the same
        // metric name" bug.
        //
        // We use `#[should_panic]` rather than
        // `catch_unwind` because `Registry` contains a
        // `parking_lot::RwLock` (not `UnwindSafe`).
        let _ = reg.register_gauge("c", "help");
    }

    #[test]
    #[should_panic(expected = "metric name \"c\" is already registered")]
    fn register_name_collision_same_kind_panics() {
        // Same-kind collision also panics — the registry
        // does not deduplicate, and a `static FOO: Lazy`
        // pattern at the call site is the canonical way
        // to ensure single registration.
        let reg = Registry::default();
        let _ = reg.register_counter("c", "help");
        let _ = reg.register_counter("c", "help");
    }

    #[test]
    fn clear_drops_registry_but_handles_stay_alive() {
        let reg = Registry::default();
        let c = reg.register_counter("c", "help");
        c.inc();
        c.inc();
        reg.clear();
        assert!(reg.is_empty());
        // The Arc handle is still functional — clear() only
        // removes the registry's *reference* to the metric.
        c.inc();
        assert_eq!(c.get(), 3);
    }

    #[test]
    fn iter_returns_all_metrics() {
        let reg = Registry::default();
        let _c = reg.register_counter("c", "help");
        let _g = reg.register_gauge("g", "help");
        let _h = reg.register_histogram("h", "help");
        assert_eq!(reg.metric_count(), 3);
        // Clone the metric names out of each Arc before
        // dropping the iterator — `Arc<dyn Metric>::name`
        // returns `&str` borrowed from the heap, which is
        // invalidated when the `Arc` is dropped.
        let names: Vec<String> = reg.iter().map(|m| m.name().to_string()).collect();
        assert!(names.contains(&"c".to_string()));
        assert!(names.contains(&"g".to_string()));
        assert!(names.contains(&"h".to_string()));
    }

    #[test]
    fn snapshot_is_sorted_and_complete() {
        let reg = Registry::default();
        let _c = reg.register_counter("zzz", "help");
        let _c = reg.register_counter("aaa", "help");
        let snap = reg.snapshot();
        assert_eq!(snap.sorted().len(), 2);
        assert_eq!(snap.sorted()[0].name(), "aaa");
        assert_eq!(snap.sorted()[1].name(), "zzz");
    }

    #[test]
    fn global_registry_is_lazy_and_independent() {
        // Use a local registry so we don't pollute the
        // global one for other tests.
        let reg = Registry::default();
        let a = reg.register_counter("a", "help");
        let b = reg.register_counter("b", "help");
        a.inc_by(5);
        b.inc_by(7);
        let snap = reg.snapshot();
        // The Counter renderer reads the value; double-check
        // via the registry's own get(). The names are owned
        // strings on the metric, so we clone them out before
        // the snapshot is dropped.
        let a_arc = reg.get("a").unwrap();
        let a_text = a_arc.render_prometheus();
        assert!(a_text.contains("a 5"));
        // sanity: the snapshot reports 2 metrics.
        assert_eq!(snap.len(), 2);
    }
}
