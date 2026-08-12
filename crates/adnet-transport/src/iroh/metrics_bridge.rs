//! PR2 — adapter that bridges `DiscoveryDiagnostics` and
//! `EndpointDiagnosticsRecorder` into the
//! `adnet-observability` global registry.
//!
//! ## What this file does
//!
//! The V8 audit called for a "transport-diagnostics →
//! `adnet-observability`" bridge. The bridge trait and
//! alignment logic live in `adnet-observability::bridge`
//! (transport-agnostic). This file is the
//! `adnet-transport`-side adapter: it implements
//! [`adnet_observability::bridge::DiagnosticsSource`] for
//! the two existing diagnostic structs so they can be fed
//! into the bridge.
//!
//! Two adapters are provided:
//!
//! - [`DiscoverySourceAdapter`] — wraps a `&DiscoveryDiagnostics`
//!   and exposes its five fields (`publishes_total`,
//!   `publishes_filtered`, `resolutions_total`, `resolutions_hit`,
//!   `resolutions_miss`). Endpoint fields return `0` /
//!   `false` — see "Field partitioning" below.
//! - [`EndpointSourceAdapter`] — wraps an
//!   `&EndpointSnapshot` and exposes the three endpoint
//!   fields (`direct_addresses`, `relay_urls`,
//!   `endpoint_closed`). Discovery fields return `0`.
//!
//! ## Field partitioning
//!
//! The bridge trait has **all** eight fields on a single
//! type so the bridge function is one linear pass — but
//! the two real diagnostic structs only carry subsets of
//! those fields. We partition them across two adapters;
//! each adapter fills the fields that don't belong to
//! it with neutral values (0 / false). Operators that
//! want to update both sets at once call the bridge
//! twice — once per adapter.
//!
//! This is intentionally a tiny bit awkward. The
//! alternative — splitting the trait into two — would
//! double the bridge function count without saving any
//! real complexity. We keep one trait and two adapters.
//!
//! ## `Deref` cost
//!
//! `DiscoveryDiagnostics::snapshot()` is cheap (a few
//! `AtomicU64` loads + a small `Mutex<Vec>` clone) so
//! calling it from inside the adapter is fine even on
//! hot scrapes. `EndpointDiagnosticsRecorder::latest()`
//! is `async`; we expose an `async fn snapshot()` helper
//! on [`EndpointSourceAdapter::from_recorder`] so the
//! caller can `await` it and then run the bridge sync.

#![cfg(feature = "iroh")]

use adnet_observability::bridge::{
    DiagnosticsSource, DiscoveryMetrics, EndpointMetrics, update_endpoint_from_source_into,
    update_from_source_into,
};

use super::discovery::diagnostics::{DiscoveryDiagnostics, IrohDiscoverySnapshot};
use super::endpoint_diagnostics::{EndpointDiagnosticsRecorder, EndpointSnapshot};

/// Adapter that exposes a `DiscoveryDiagnostics` snapshot
/// as a [`DiagnosticsSource`] for the bridge.
///
/// **Allocation note**: [`DiscoverySourceAdapter::snapshot`]
/// captures an [`IrohDiscoverySnapshot`] by value (the
/// snapshot type is `Clone + Serialize`); the adapter
/// stores the snapshot and serves the bridge trait
/// methods from it. This is the simplest contract: the
/// adapter is a value, not a reference, so it can be
/// passed across `await` points if needed.
pub struct DiscoverySourceAdapter {
    snap: IrohDiscoverySnapshot,
}

impl DiscoverySourceAdapter {
    /// Capture a snapshot of `diag` and wrap it. Equivalent
    /// to `DiscoveryDiagnostics::snapshot()` — kept as a
    /// named constructor so callers don't have to import
    /// `IrohDiscoverySnapshot` directly.
    pub fn capture(diag: &DiscoveryDiagnostics) -> Self {
        Self {
            snap: diag.snapshot(),
        }
    }
}

impl DiagnosticsSource for DiscoverySourceAdapter {
    fn publishes_total(&self) -> u64 {
        self.snap.publishes_total
    }
    fn publishes_filtered(&self) -> u64 {
        self.snap.publishes_filtered
    }
    fn resolutions_total(&self) -> u64 {
        self.snap.resolutions_total
    }
    fn resolutions_hit(&self) -> u64 {
        self.snap.resolutions_hit
    }
    fn resolutions_miss(&self) -> u64 {
        self.snap.resolutions_miss
    }
    // Endpoint fields are not part of DiscoveryDiagnostics;
    // return neutral values so the bridge function still
    // writes the discovery metrics correctly.
    fn direct_addresses(&self) -> u64 {
        0
    }
    fn relay_urls(&self) -> u64 {
        0
    }
    fn endpoint_closed(&self) -> bool {
        false
    }
}

/// Adapter that exposes an `EndpointSnapshot` as a
/// [`DiagnosticsSource`] for the bridge.
pub struct EndpointSourceAdapter {
    snap: EndpointSnapshot,
}

impl EndpointSourceAdapter {
    /// Wrap an already-captured snapshot. Use this when
    /// the caller already has an `EndpointSnapshot` in
    /// hand (e.g. from `EndpointSnapshot::capture`).
    pub fn from_snapshot(snap: EndpointSnapshot) -> Self {
        Self { snap }
    }

    /// Capture a snapshot from a live
    /// [`EndpointDiagnosticsRecorder`] (the recorder's
    /// `latest()` is async). Returns `None` if the
    /// recorder has no snapshots yet — callers should
    /// treat that as "endpoint not yet bound" and skip
    /// the bridge call for this tick.
    pub async fn from_recorder(rec: &EndpointDiagnosticsRecorder) -> Option<Self> {
        rec.latest().await.map(|snap| Self { snap })
    }
}

impl DiagnosticsSource for EndpointSourceAdapter {
    fn publishes_total(&self) -> u64 {
        0
    }
    fn publishes_filtered(&self) -> u64 {
        0
    }
    fn resolutions_total(&self) -> u64 {
        0
    }
    fn resolutions_hit(&self) -> u64 {
        0
    }
    fn resolutions_miss(&self) -> u64 {
        0
    }
    fn direct_addresses(&self) -> u64 {
        self.snap.direct_addresses as u64
    }
    fn relay_urls(&self) -> u64 {
        self.snap.relay_urls as u64
    }
    fn endpoint_closed(&self) -> bool {
        self.snap.closed
    }
}

/// Update the **process-global** discovery metrics from
/// `diag`. Convenience wrapper over
/// [`adnet_observability::bridge::update_from_source`].
pub fn publish_discovery_metrics(diag: &DiscoveryDiagnostics) {
    let adapter = DiscoverySourceAdapter::capture(diag);
    update_from_source_into(&adnet_observability::bridge::DISCOVERY, &adapter);
}

/// Update the **process-global** discovery metrics using
/// a pre-built `DiscoveryMetrics` (e.g. for tests). Most
/// callers should use [`publish_discovery_metrics`].
pub fn publish_discovery_metrics_into(metrics: &DiscoveryMetrics, diag: &DiscoveryDiagnostics) {
    let adapter = DiscoverySourceAdapter::capture(diag);
    update_from_source_into(metrics, &adapter);
}

/// Update the **process-global** endpoint metrics from
/// `rec`'s latest snapshot. Returns `false` if the
/// recorder has no snapshot yet.
pub async fn publish_endpoint_metrics(rec: &EndpointDiagnosticsRecorder) -> bool {
    match EndpointSourceAdapter::from_recorder(rec).await {
        Some(adapter) => {
            update_endpoint_from_source_into(&adnet_observability::bridge::ENDPOINT, &adapter);
            true
        }
        None => false,
    }
}

/// Update the **process-global** endpoint metrics using
/// a pre-built `EndpointMetrics`.
pub async fn publish_endpoint_metrics_into(
    metrics: &EndpointMetrics,
    rec: &EndpointDiagnosticsRecorder,
) -> bool {
    match EndpointSourceAdapter::from_recorder(rec).await {
        Some(adapter) => {
            update_endpoint_from_source_into(metrics, &adapter);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn for_tests() -> (DiscoveryMetrics, EndpointMetrics) {
        let reg = adnet_observability::registry::Registry::default();
        (
            DiscoveryMetrics::register(&reg),
            EndpointMetrics::register(&reg),
        )
    }

    #[test]
    fn discovery_adapter_reads_snapshot_fields() {
        let d = DiscoveryDiagnostics::new();
        d.record_publish(true);
        d.record_publish(false);
        d.record_resolution_started();
        d.record_resolution("pkarr", true);

        let adapter = DiscoverySourceAdapter::capture(&d);
        assert_eq!(adapter.publishes_total(), 2);
        assert_eq!(adapter.publishes_filtered(), 1);
        assert_eq!(adapter.resolutions_total(), 1);
        assert_eq!(adapter.resolutions_hit(), 1);
        assert_eq!(adapter.resolutions_miss(), 0);
        // Endpoint fields are neutral:
        assert_eq!(adapter.direct_addresses(), 0);
        assert_eq!(adapter.relay_urls(), 0);
        assert!(!adapter.endpoint_closed());
    }

    #[test]
    fn endpoint_adapter_reads_snapshot_fields() {
        let snap = EndpointSnapshot {
            endpoint_id: "01aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            endpoint_id_short: "01aaaaaa".into(),
            closed: true,
            identity_path: None,
            direct_addresses: 2,
            relay_urls: 3,
            captured_at: std::time::SystemTime::UNIX_EPOCH,
        };
        let adapter = EndpointSourceAdapter::from_snapshot(snap);
        assert_eq!(adapter.direct_addresses(), 2);
        assert_eq!(adapter.relay_urls(), 3);
        assert!(adapter.endpoint_closed());
        // Discovery fields are neutral:
        assert_eq!(adapter.publishes_total(), 0);
        assert_eq!(adapter.resolutions_total(), 0);
    }

    #[test]
    fn publish_discovery_metrics_into_writes_counters() {
        let (m, _) = for_tests();
        let d = DiscoveryDiagnostics::new();
        d.record_publish(true);
        d.record_publish(false);
        d.record_publish(true);
        d.record_resolution_started();
        d.record_resolution("pkarr", true);
        d.record_resolution("pkarr", false);
        d.record_resolution("dns", true);
        publish_discovery_metrics_into(&m, &d);
        assert_eq!(m.publishes_total.get(), 3);
        assert_eq!(m.publishes_filtered.get(), 1);
        assert_eq!(m.resolutions_total.get(), 1);
        assert_eq!(m.resolutions_hit.get(), 2);
        assert_eq!(m.resolutions_miss.get(), 1);
        // 2/(2+1) = 66.66% → 66_666_666 (rounded micro-units).
        let rate = m.hit_rate_pct.get();
        assert!(
            (rate - 66_666_666).abs() < 5,
            "hit_rate_pct out of range: {rate}"
        );
    }

    #[tokio::test]
    async fn publish_endpoint_metrics_into_writes_gauges() {
        let (_, m) = for_tests();
        let rec = Arc::new(EndpointDiagnosticsRecorder::new(4));
        rec.record(EndpointSnapshot {
            endpoint_id: "01bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .into(),
            endpoint_id_short: "01bbbbbb".into(),
            closed: false,
            identity_path: None,
            direct_addresses: 5,
            relay_urls: 2,
            captured_at: std::time::SystemTime::UNIX_EPOCH,
        })
        .await;
        let ok = publish_endpoint_metrics_into(&m, &rec).await;
        assert!(ok);
        assert_eq!(m.direct_addresses.get(), 5);
        assert_eq!(m.relay_urls.get(), 2);
        assert_eq!(m.closed.get(), 0);
    }

    #[tokio::test]
    async fn publish_endpoint_metrics_into_returns_false_when_empty() {
        let (_, m) = for_tests();
        let rec = EndpointDiagnosticsRecorder::new(2);
        let ok = publish_endpoint_metrics_into(&m, &rec).await;
        assert!(!ok, "no snapshot recorded → no update");
        assert_eq!(m.direct_addresses.get(), 0);
        assert_eq!(m.relay_urls.get(), 0);
        assert_eq!(m.closed.get(), 0);
    }

    #[test]
    fn publish_discovery_metrics_uses_global_handle() {
        // Smoke test: the no-`into` variant must not panic.
        // Real-valued assertions live in the `into` variant
        // above where we control the registry.
        let d = DiscoveryDiagnostics::new();
        d.record_publish(true);
        publish_discovery_metrics(&d);
    }
}
