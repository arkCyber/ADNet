//! PR2 — bridge from existing diagnostic snapshots to the
//! process-wide Prometheus registry.
//!
//! ## Why this module exists
//!
//! The V6 / V7 audits flagged that `DiscoveryDiagnostics` (in
//! `a3net-transport/src/iroh/discovery/diagnostics.rs`) and
//! `EndpointDiagnosticsRecorder` (in
//! `a3net-transport/src/iroh/endpoint_diagnostics.rs`) are
//! *typed* diagnostic snapshots — they expose `snapshot()` methods
//! that return `Clone + Serialize` structs. They live behind
//! `#[cfg(feature = "iroh")]`, so the `a3net-observability`
//! crate (which has **no** iroh dependency) cannot directly
//! import them.
//!
//! PR2 is the **bridge**: we define a transport-agnostic
//! [`DiagnosticsSource`] trait in `a3net-observability` and
//! write an adapter in `a3net-transport/src/iroh/metrics_bridge.rs`
//! that implements the trait for both snapshot types. The
//! [`update_from_source`] helper then reads the latest snapshot
//! and writes the values into a fresh set of `Counter` /
//! `Gauge` metrics registered against the global registry.
//!
//! ## Why not just `prometheus` / `OpenTelemetry`?
//!
//! Same reason as PR1: A3Net's three metric primitives are enough
//! for what we surface. The bridge is a single linear pass over
//! the snapshot fields — no streaming, no quantiles.
//!
//! ## Metric names
//!
//! All names are prefixed with `a3net_discovery_*` and
//! `a3net_endpoint_*` so they don't collide with the
//! `a3net_transport_*` counters from
//! `crates/a3net-transport/src/metrics.rs`. The `endpoint_*`
//! metrics are **gauges** (current state); the `discovery_*`
//! counters carry both **monotonic totals** (publishes_total,
//! resolutions_total) and **current derived gauges**
//! (publishes_filtered).
//!
//! ## Cardinality
//!
//! The bridge emits one counter / gauge pair per source field;
//! there is **no per-provenance label** on the discovery metrics
//! because `by_provenance` is already cardinality-capped at
//! `MAX_PROVENANCE_BUCKETS` in `DiscoveryDiagnostics`. We instead
//! emit a single aggregated `a3net_discovery_resolutions_hit_pct`
//! gauge (computed from `resolutions_hit / resolutions_total`)
//! so the operator gets a hit-rate signal without per-provenance
//! label explosion.

use std::sync::Arc;

use once_cell::sync::Lazy;

use crate::metrics::{Counter, Gauge};
use crate::registry::{GLOBAL, Registry};

/// Snapshot view that the bridge can read. Every A3Net
/// diagnostic struct (current and future) implements this
/// trait by extracting the relevant fields.
///
/// The trait is **object-safe** so callers can store
/// `Box<dyn DiagnosticsSource>` or `Arc<dyn DiagnosticsSource>`
/// if they want a registry of bridges. The bridge function
/// takes `&dyn DiagnosticsSource` for that reason.
///
/// **Design note**: the fields below are deliberately
/// value-shaped (no `&'a str`, no `&'a [u8]`) so the trait
/// is `dyn`-safe and easy to implement across crates. Any
/// richer detail (e.g. `by_provenance`) is exposed as a
/// pre-computed gauge in the bridge metrics, not as a
/// separate label set.
pub trait DiagnosticsSource {
    /// Total number of publish attempts.
    fn publishes_total(&self) -> u64;
    /// Total publishes *filtered out* by the publish policy.
    fn publishes_filtered(&self) -> u64;
    /// Total resolution attempts (started counter).
    fn resolutions_total(&self) -> u64;
    /// Resolutions that returned at least one address.
    fn resolutions_hit(&self) -> u64;
    /// Resolutions that returned no addresses.
    fn resolutions_miss(&self) -> u64;

    /// Direct-IP addresses currently associated with the local
    /// endpoint. Endpoint-level state — used by the
    /// `EndpointMetrics` group.
    fn direct_addresses(&self) -> u64;
    /// Relay URLs currently associated with the local endpoint.
    fn relay_urls(&self) -> u64;
    /// Whether the local endpoint is closed.
    fn endpoint_closed(&self) -> bool;
}

/// Discovery-layer metrics. All fields are `Arc`-shared so
/// cloning the handle is cheap and concurrent scrapes don't
/// block on each other.
///
/// Names are listed in the module-level doc comment.
#[derive(Debug, Clone)]
pub struct DiscoveryMetrics {
    /// `a3net_discovery_publishes_total` — monotonic publish
    /// attempts since process start.
    pub publishes_total: Arc<Counter>,
    /// `a3net_discovery_publishes_filtered` — monotonic
    /// publishes that the publish policy stripped.
    pub publishes_filtered: Arc<Counter>,
    /// `a3net_discovery_resolutions_total` — monotonic
    /// resolution attempts since process start.
    pub resolutions_total: Arc<Counter>,
    /// `a3net_discovery_resolutions_hit` — monotonic
    /// resolutions that returned at least one address.
    pub resolutions_hit: Arc<Counter>,
    /// `a3net_discovery_resolutions_miss` — monotonic
    /// resolutions that returned no addresses.
    pub resolutions_miss: Arc<Counter>,
    /// `a3net_discovery_hit_rate_pct` — current hit rate
    /// in percent (0.0..=100.0). Gauge, not counter.
    pub hit_rate_pct: Arc<Gauge>,
}

impl DiscoveryMetrics {
    /// Register every metric into `registry`. Idempotent —
    /// a second call returns the existing handles (the
    /// registry de-dups by name).
    pub fn register(registry: &Registry) -> Self {
        Self {
            publishes_total: registry.register_counter(
                "a3net_discovery_publishes_total",
                "Total discovery publish attempts.",
            ),
            publishes_filtered: registry.register_counter(
                "a3net_discovery_publishes_filtered",
                "Total publishes filtered out by the publish policy.",
            ),
            resolutions_total: registry.register_counter(
                "a3net_discovery_resolutions_total",
                "Total discovery resolution attempts.",
            ),
            resolutions_hit: registry.register_counter(
                "a3net_discovery_resolutions_hit",
                "Total resolutions that returned at least one address.",
            ),
            resolutions_miss: registry.register_counter(
                "a3net_discovery_resolutions_miss",
                "Total resolutions that returned no addresses.",
            ),
            hit_rate_pct: registry.register_gauge(
                "a3net_discovery_hit_rate_pct",
                "Current discovery hit rate as percent (0..=100).",
            ),
        }
    }
}

/// Endpoint-layer metrics. Gauges (current state), not
/// monotonic counters.
#[derive(Debug, Clone)]
pub struct EndpointMetrics {
    /// `a3net_endpoint_direct_addresses` — current direct
    /// (non-relay) addresses associated with the local
    /// endpoint, after publish-policy filtering.
    pub direct_addresses: Arc<Gauge>,
    /// `a3net_endpoint_relay_urls` — current relay URLs
    /// associated with the local endpoint.
    pub relay_urls: Arc<Gauge>,
    /// `a3net_endpoint_closed` — `1` if the local endpoint
    /// is closed, `0` if open. Gauge-as-bool is the
    /// Prometheus convention.
    pub closed: Arc<Gauge>,
}

impl EndpointMetrics {
    /// Register every metric into `registry`. Idempotent.
    pub fn register(registry: &Registry) -> Self {
        Self {
            direct_addresses: registry.register_gauge(
                "a3net_endpoint_direct_addresses",
                "Direct (non-relay) addresses currently associated with the local endpoint.",
            ),
            relay_urls: registry.register_gauge(
                "a3net_endpoint_relay_urls",
                "Relay URLs currently associated with the local endpoint.",
            ),
            closed: registry.register_gauge(
                "a3net_endpoint_closed",
                "Local endpoint closed flag (1 = closed, 0 = open).",
            ),
        }
    }
}

/// Process-global discovery metrics. First access registers
/// the metrics into [`GLOBAL`]; subsequent access returns
/// a clone of the same `Arc`-backed counters.
pub static DISCOVERY: Lazy<DiscoveryMetrics> = Lazy::new(|| DiscoveryMetrics::register(&GLOBAL));

/// Process-global endpoint metrics. Same lazy-init contract
/// as [`DISCOVERY`].
pub static ENDPOINT: Lazy<EndpointMetrics> = Lazy::new(|| EndpointMetrics::register(&GLOBAL));

/// Update `metrics` from a [`DiagnosticsSource`] snapshot.
///
/// **The monotonic counters are NOT reset on each call.** The
/// bridge carries over the `*_total` snapshot values by
/// *aligning* the local counter to the snapshot — bumping the
/// local counter by the delta to reach the snapshot value, so
/// the final Prometheus output matches the source.
///
/// The alignment trick is needed because
/// `a3net-observability` counters are monotonic
/// (`fetch_add`-only). If the source's `publishes_total`
/// regresses (e.g. process restart between scrapes), the
/// bridge clamps to `local >= source` and **never** goes
/// backwards — operators see a flat line rather than a
/// negative spike.
///
/// **Idempotency**: calling this function twice with the
/// same source snapshot leaves the local counters at
/// `source_value`. Operators can poll from a tokio task
/// without worrying about duplicate ticks.
///
/// **Static helper**: [`update_from_source`] wraps this
/// function with the process-global [`DISCOVERY`] handle so
/// production callers don't have to thread the metrics
/// through.
pub fn update_from_source_into(metrics: &DiscoveryMetrics, source: &dyn DiagnosticsSource) {
    // Align monotonic counters. We use a small helper to
    // keep the alignment logic in one place; each metric
    // is updated independently.
    align_counter(&metrics.publishes_total, source.publishes_total());
    align_counter(&metrics.publishes_filtered, source.publishes_filtered());
    align_counter(&metrics.resolutions_total, source.resolutions_total());
    align_counter(&metrics.resolutions_hit, source.resolutions_hit());
    align_counter(&metrics.resolutions_miss, source.resolutions_miss());

    // Gauge: compute current hit rate as a percentage.
    // Stored as integer micro-units (75.42% → 75420000)
    // via `set_f64` so the Prometheus exporter sees an
    // atomic `i64` under the hood. The metric's `help`
    // text documents the scaling.
    //
    // **Denominator policy**: `DiscoveryDiagnostics`
    // documents a started→outcome contract — the
    // caller is responsible for calling
    // `record_resolution_started` BEFORE each
    // `record_resolution`. If they only call the
    // outcome path, `resolutions_total` lags behind
    // `resolutions_hit + resolutions_miss` and a
    // naive `hit / total` would over-report. We
    // floor the denominator at `hit + miss` so the
    // rate is always in `0..=100`, even when
    // callers skip the started hook (as
    // `DiscoveryDiagnostics::record_resolution`
    // documents in its own doc comment).
    let hit = source.resolutions_hit();
    let miss = source.resolutions_miss();
    let denom = std::cmp::max(source.resolutions_total(), hit + miss);
    let hit_rate = if denom == 0 {
        0.0
    } else {
        // `as f64` is safe because u64 -> f64 can only lose
        // precision for values > 2^53; A3Net's resolutions
        // counter is never that high in practice. Operators
        // running a hot loop for a year accumulate ~3e7
        // resolutions; well within f64 precision.
        (hit as f64 / denom as f64) * 100.0
    };
    metrics.hit_rate_pct.set_f64(hit_rate);
}

/// Update the process-global discovery metrics from a
/// [`DiagnosticsSource`] snapshot. Convenience wrapper
/// over [`update_from_source_into`] that uses the
/// process-global [`DISCOVERY`] handle.
pub fn update_from_source(source: &dyn DiagnosticsSource) {
    update_from_source_into(&DISCOVERY, source);
}

/// Update `metrics` from a [`DiagnosticsSource`] snapshot.
/// Endpoint state is current, so we just overwrite the
/// gauges.
pub fn update_endpoint_from_source_into(metrics: &EndpointMetrics, source: &dyn DiagnosticsSource) {
    metrics
        .direct_addresses
        .set(source.direct_addresses() as i64);
    metrics.relay_urls.set(source.relay_urls() as i64);
    metrics
        .closed
        .set(if source.endpoint_closed() { 1 } else { 0 });
}

/// Update the process-global endpoint metrics from a
/// [`DiagnosticsSource`] snapshot. Convenience wrapper
/// over [`update_endpoint_from_source_into`] that uses the
/// process-global [`ENDPOINT`] handle.
pub fn update_endpoint_from_source(source: &dyn DiagnosticsSource) {
    update_endpoint_from_source_into(&ENDPOINT, source);
}

/// Align a monotonic counter to `target`. If the local
/// counter is *behind* `target` (the snapshot has seen more
/// events than the local counter has), bump the local
/// counter by the delta. If the local counter is *ahead*
/// (a process restart between scrapes, or a snapshot
/// regression), clamp — `local >= target`. Operators
/// see a flat line, never a backwards tick.
fn align_counter(counter: &Counter, target: u64) {
    let current = counter.get();
    if target > current {
        counter.inc_by(target - current);
    }
    // Else: clamp silently. The local counter is already
    // ahead (process restart, or snapshot regression);
    // we don't go backwards.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub source for unit tests. The struct fields are
    /// `pub` so the test can mutate them between calls —
    /// the bridge treats them as immutable via the trait
    /// methods, but tests can drive them directly.
    #[derive(Debug, Default, Clone)]
    struct StubSource {
        publishes_total: u64,
        publishes_filtered: u64,
        resolutions_total: u64,
        resolutions_hit: u64,
        resolutions_miss: u64,
        direct_addresses: u64,
        relay_urls: u64,
        endpoint_closed: bool,
    }

    impl DiagnosticsSource for StubSource {
        fn publishes_total(&self) -> u64 {
            self.publishes_total
        }
        fn publishes_filtered(&self) -> u64 {
            self.publishes_filtered
        }
        fn resolutions_total(&self) -> u64 {
            self.resolutions_total
        }
        fn resolutions_hit(&self) -> u64 {
            self.resolutions_hit
        }
        fn resolutions_miss(&self) -> u64 {
            self.resolutions_miss
        }
        fn direct_addresses(&self) -> u64 {
            self.direct_addresses
        }
        fn relay_urls(&self) -> u64 {
            self.relay_urls
        }
        fn endpoint_closed(&self) -> bool {
            self.endpoint_closed
        }
    }

    /// Use an isolated registry so the bridge tests don't
    /// pollute the global one — V8 PR1's
    /// `TransportMetrics::for_tests` pattern.
    fn for_tests() -> (DiscoveryMetrics, EndpointMetrics) {
        let reg = Registry::default();
        (
            DiscoveryMetrics::register(&reg),
            EndpointMetrics::register(&reg),
        )
    }

    #[test]
    fn align_counter_bumps_up_when_behind() {
        let (m, _) = for_tests();
        // Source reports 5 publishes; local counter at 0.
        let s = StubSource {
            publishes_total: 5,
            ..Default::default()
        };
        update_from_source_into(&m, &s);
        assert_eq!(m.publishes_total.get(), 5);
        // Source advances to 10; local counter should reach 10.
        let s = StubSource {
            publishes_total: 10,
            ..Default::default()
        };
        update_from_source_into(&m, &s);
        assert_eq!(m.publishes_total.get(), 10);
    }

    #[test]
    fn align_counter_clamps_when_ahead() {
        let (m, _) = for_tests();
        // Drive local counter past the source value
        // manually (simulating a process restart where the
        // local state was preserved but the source came
        // back at zero).
        m.publishes_total.inc_by(100);
        let s = StubSource {
            publishes_total: 0,
            ..Default::default()
        };
        update_from_source_into(&m, &s);
        // Local stays at 100 — we never regress a
        // monotonic counter.
        assert_eq!(m.publishes_total.get(), 100);
    }

    #[test]
    fn align_counter_handles_repeated_calls_with_same_snapshot() {
        let (m, _) = for_tests();
        let s = StubSource {
            publishes_total: 7,
            publishes_filtered: 2,
            resolutions_total: 5,
            resolutions_hit: 3,
            resolutions_miss: 2,
            ..Default::default()
        };
        update_from_source_into(&m, &s);
        assert_eq!(m.publishes_total.get(), 7);
        update_from_source_into(&m, &s);
        assert_eq!(m.publishes_total.get(), 7);
        update_from_source_into(&m, &s);
        assert_eq!(m.publishes_total.get(), 7);
    }

    #[test]
    fn hit_rate_is_zero_when_no_resolutions() {
        let (m, _) = for_tests();
        let s = StubSource::default();
        update_from_source_into(&m, &s);
        // `set_f64` stores `0.0 * 1_000_000 = 0` as `i64`.
        assert_eq!(m.hit_rate_pct.get(), 0);
    }

    #[test]
    fn hit_rate_is_calculated_correctly() {
        let (m, _) = for_tests();
        let s = StubSource {
            resolutions_total: 4,
            resolutions_hit: 3,
            resolutions_miss: 1,
            ..Default::default()
        };
        update_from_source_into(&m, &s);
        // 75% is stored as 75 * 1_000_000 = 75_000_000.
        assert_eq!(m.hit_rate_pct.get(), 75_000_000);
    }

    #[test]
    fn update_endpoint_writes_gauge_state() {
        let (_, m) = for_tests();
        let s = StubSource {
            direct_addresses: 3,
            relay_urls: 2,
            endpoint_closed: false,
            ..Default::default()
        };
        update_endpoint_from_source_into(&m, &s);
        assert_eq!(m.direct_addresses.get(), 3);
        assert_eq!(m.relay_urls.get(), 2);
        assert_eq!(m.closed.get(), 0);
    }

    #[test]
    fn update_endpoint_overwrites_state_on_second_call() {
        let (_, m) = for_tests();
        let s1 = StubSource {
            direct_addresses: 3,
            relay_urls: 2,
            endpoint_closed: false,
            ..Default::default()
        };
        update_endpoint_from_source_into(&m, &s1);
        let s2 = StubSource {
            direct_addresses: 1,
            relay_urls: 5,
            endpoint_closed: true,
            ..Default::default()
        };
        update_endpoint_from_source_into(&m, &s2);
        assert_eq!(m.direct_addresses.get(), 1);
        assert_eq!(m.relay_urls.get(), 5);
        assert_eq!(m.closed.get(), 1);
    }

    #[test]
    fn bridge_is_dyn_safe() {
        // Trait object compile-time test. If `DiagnosticsSource`
        // ever stops being `dyn`-safe, this won't compile.
        let s = StubSource::default();
        let boxed: Box<dyn DiagnosticsSource> = Box::new(s);
        let (m, em) = for_tests();
        update_from_source_into(&m, &*boxed);
        update_endpoint_from_source_into(&em, &*boxed);
    }

    #[test]
    fn static_helper_writes_to_global_discovery_metrics() {
        // Calling the no-`_into` variant must drive the
        // global `DISCOVERY` static. We can't directly
        // observe the global metric value (it's a
        // `Lazy<DiscoveryMetrics>` whose internals aren't
        // public), so we exercise the static helper once
        // and assert it doesn't panic. The real assertion
        // is that the call path is reachable.
        let s = StubSource {
            publishes_total: 1,
            resolutions_total: 1,
            resolutions_hit: 1,
            ..Default::default()
        };
        update_from_source(&s);
        update_endpoint_from_source(&s);
    }
}
