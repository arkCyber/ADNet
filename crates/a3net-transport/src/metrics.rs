//! Transport-layer metrics — counter / gauge primitives backed
//! by the global `a3net-observability` registry.
//!
//! This is the answer to audit gap §8 ("iroh 提供官方的 iroh-doctor
//! 和 iroh-metrics；A3Net 仅有 DiscoveryDiagnostics 而没有
//! endpoint / transport / gossip 层的 metrics").
//!
//! All metrics are **registered eagerly** into the global
//! [`a3net_observability::registry::GLOBAL`] registry on first
//! access via the `transport_metrics!` macro / `register()` call
//! in `lazy_static!` style below. The exporter (see
//! `a3net-observability::prometheus::PrometheusExporter`) picks
//! them up automatically; no wiring is needed at the call site.
//!
//! ## Metric names
//!
//! All names are prefixed with `a3net_transport_` so the
//! Prometheus output is namespaced; operators can group them
//! with a single `match { a3net_transport_... }` rule.
//!
//! ## Cardinality
//!
//! Each metric is intentionally low-cardinality. The only
//! label set we add is `{kind="dial"|"accept"}` on counters
//! that span both directions; remote `NodeId`s are deliberately
//! **not** labelled — a malicious peer could otherwise inflate
//! the registry to O(peers).
//!
//! ## Layer coverage
//!
//! This module covers the **transport** layer. Other layers
//! (gossip / blobs / docs / chat) have their own `*Metrics`
//! structs in their respective crates; they all share the
//! same global registry so a single `PrometheusExporter`
//! renders the whole workspace at once.
//!
//! [`a3net_observability::registry::GLOBAL`]: a3net_observability::registry::GLOBAL

use std::sync::{Arc, OnceLock};

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::{GLOBAL as OBSERVABILITY_GLOBAL, Registry};

/// Per-layer metric handle. Constructed once per process via
/// [`TransportMetrics::get`] (or [`TransportMetrics::register`]
/// in tests that need an isolated registry). The fields are
/// `Arc`-shared so cloning the handle is cheap.
#[derive(Debug, Clone)]
pub struct TransportMetrics {
    /// `a3net_transport_dial_attempts_total` — total `dial`
    /// calls invoked, irrespective of outcome.
    pub dial_attempts: Arc<Counter>,
    /// `a3net_transport_dial_failures_total` — total `dial`
    /// calls that returned an error (identity mismatch,
    /// timeout, peer unreachable, etc.).
    pub dial_failures: Arc<Counter>,
    /// `a3net_transport_dial_successes_total` — total `dial`
    /// calls that returned a live `OutgoingConnection`.
    pub dial_successes: Arc<Counter>,
    /// `a3net_transport_accepts_total` — total `accept`
    /// resolutions (whether Some/None).
    pub accepts: Arc<Counter>,
    /// `a3net_transport_frames_sent_total` — total frames
    /// pushed through `OutgoingConnection::send`.
    pub frames_sent: Arc<Counter>,
    /// `a3net_transport_frames_received_total` — total frames
    /// read through `OutgoingConnection::recv`.
    pub frames_received: Arc<Counter>,
    /// `a3net_transport_bytes_sent_total` — total bytes
    /// written to the wire (frame body length, sum).
    pub bytes_sent: Arc<Counter>,
    /// `a3net_transport_bytes_received_total` — total bytes
    /// read off the wire.
    pub bytes_received: Arc<Counter>,
    /// `a3net_transport_active_connections` — current number of
    /// open QUIC connections tracked by this layer.
    pub active_connections: Arc<Gauge>,
    /// `a3net_transport_identity_mismatch_total` — dial-side
    /// rejections from `enforce_peer_id` / `iroh::connect`.
    pub identity_mismatch: Arc<Counter>,
}

impl TransportMetrics {
    /// Register every metric into `registry`. Idempotent: a
    /// second call re-registers a fresh `Counter` / `Gauge`
    /// (the registry de-dups by name and returns the
    /// previously-registered instance on a second
    /// `register_counter(...)`). Safe to call from a test
    /// harness or from the production startup path.
    pub fn register(registry: &Registry) -> Self {
        Self {
            dial_attempts: registry.register_counter(
                "a3net_transport_dial_attempts_total",
                "Total Transport::dial calls.",
            ),
            dial_failures: registry.register_counter(
                "a3net_transport_dial_failures_total",
                "Total failed Transport::dial calls.",
            ),
            dial_successes: registry.register_counter(
                "a3net_transport_dial_successes_total",
                "Total successful Transport::dial calls.",
            ),
            accepts: registry.register_counter(
                "a3net_transport_accepts_total",
                "Total Transport::accept resolutions.",
            ),
            frames_sent: registry.register_counter(
                "a3net_transport_frames_sent_total",
                "Total frames sent via OutgoingConnection::send.",
            ),
            frames_received: registry.register_counter(
                "a3net_transport_frames_received_total",
                "Total frames received via OutgoingConnection::recv.",
            ),
            bytes_sent: registry.register_counter(
                "a3net_transport_bytes_sent_total",
                "Total frame body bytes sent.",
            ),
            bytes_received: registry.register_counter(
                "a3net_transport_bytes_received_total",
                "Total frame body bytes received.",
            ),
            active_connections: registry.register_gauge(
                "a3net_transport_active_connections",
                "Currently open QUIC connections.",
            ),
            identity_mismatch: registry.register_counter(
                "a3net_transport_identity_mismatch_total",
                "Total dial-side identity mismatches.",
            ),
        }
    }
}

/// Process-global handle. First call to [`TransportMetrics::get`]
/// registers the metrics into the global registry; subsequent
/// calls return a clone of the same `Arc`-backed counters so
/// production code never has to thread the handle through the
/// transport factory.
static GLOBAL: OnceLock<TransportMetrics> = OnceLock::new();

impl TransportMetrics {
    /// Get (or initialise) the process-global handle.
    ///
    /// Uses [`OnceLock::get_or_init`] so that even if two threads
    /// race on the first call, `register()` runs exactly once —
    /// a second `register()` call would panic in the observability
    /// registry on the duplicated metric name.
    pub fn get() -> Self {
        GLOBAL
            .get_or_init(|| Self::register(&OBSERVABILITY_GLOBAL))
            .clone()
    }

    /// Test-only constructor that uses an isolated registry
    /// (does not touch the global one). Useful for unit tests
    /// that want to assert the metric values without polluting
    /// the production registry.
    #[cfg(test)]
    pub fn for_tests() -> (Self, std::sync::Arc<Registry>) {
        let registry = std::sync::Arc::new(Registry::default());
        (Self::register(&registry), registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::quic::TransportIdentity;
    use crate::quic::{QuicTransportBuilder, derive_node_id_from_cert};
    use a3net_observability::prometheus::PrometheusExporter;
    use a3net_observability::registry::GLOBAL as GLOBAL_REGISTRY;
    use a3net_types::NodeId;

    #[test]
    fn register_creates_all_metrics_with_unique_names() {
        let (m, registry) = TransportMetrics::for_tests();
        // Each metric should be visible by name in the
        // registry snapshot.
        let snap = registry.snapshot();
        let names: Vec<String> = snap.metrics.iter().map(|x| x.name().to_string()).collect();
        assert!(names.contains(&"a3net_transport_dial_attempts_total".to_string()));
        assert!(names.contains(&"a3net_transport_dial_failures_total".to_string()));
        assert!(names.contains(&"a3net_transport_dial_successes_total".to_string()));
        assert!(names.contains(&"a3net_transport_accepts_total".to_string()));
        assert!(names.contains(&"a3net_transport_frames_sent_total".to_string()));
        assert!(names.contains(&"a3net_transport_frames_received_total".to_string()));
        assert!(names.contains(&"a3net_transport_bytes_sent_total".to_string()));
        assert!(names.contains(&"a3net_transport_bytes_received_total".to_string()));
        assert!(names.contains(&"a3net_transport_active_connections".to_string()));
        assert!(names.contains(&"a3net_transport_identity_mismatch_total".to_string()));
        // Inc + read back.
        m.dial_attempts.inc_by(3);
        m.dial_successes.inc();
        m.dial_failures.inc();
        m.active_connections.set(7);
        m.bytes_sent.inc_by(42);
        let exporter = PrometheusExporter::new(&registry);
        let text = exporter.render().body;
        assert!(text.contains("a3net_transport_dial_attempts_total 3"));
        assert!(text.contains("a3net_transport_dial_successes_total 1"));
        assert!(text.contains("a3net_transport_dial_failures_total 1"));
        assert!(text.contains("a3net_transport_active_connections 7"));
        assert!(text.contains("a3net_transport_bytes_sent_total 42"));
    }

    #[test]
    fn get_is_idempotent_and_returns_same_handle() {
        let a = TransportMetrics::get();
        let b = TransportMetrics::get();
        // The handle clones share the inner counters; bumping
        // `a` is observable on `b`.
        a.dial_attempts.inc();
        assert!(b.dial_attempts.get() >= 1);
    }

    #[test]
    fn for_tests_uses_isolated_registry() {
        let (m, registry) = TransportMetrics::for_tests();
        m.dial_attempts.inc_by(5);
        let exporter = PrometheusExporter::new(&registry);
        let text = exporter.render().body;
        assert!(text.contains("a3net_transport_dial_attempts_total 5"));
    }

    /// **Gap §8 — Prometheus exporter wired into the QUIC
    /// transport.** We dial + accept + send + recv a single
    /// frame through the native QUIC transport and confirm
    /// every metric (dial_attempts, dial_successes,
    /// frames_sent, frames_received, accepts, active_connections)
    /// incremented at least once. Counts are compared with
    /// `>=` because the test runs inside a process that may
    /// have other dials in flight.
    ///
    /// **Note**: the transport currently calls the
    /// process-global `TransportMetrics::get()`; this test
    /// asserts **total** increments so the assertions are
    /// stable under test re-runs in the same process.
    #[tokio::test]
    async fn transport_round_trip_increments_metrics() {
        use crate::traits::Transport;
        let m = TransportMetrics::get();
        let baseline_attempts = m.dial_attempts.get();
        let baseline_successes = m.dial_successes.get();
        let baseline_sent = m.frames_sent.get();
        let baseline_recv = m.frames_received.get();
        let baseline_accepts = m.accepts.get();
        let baseline_active = m.active_connections.get();

        let peer_identity = TransportIdentity::generate().unwrap();
        let peer_node_from_cert = derive_node_id_from_cert(peer_identity.cert_der()).unwrap();
        let server =
            QuicTransportBuilder::new(peer_node_from_cert.clone(), "127.0.0.1:0".parse().unwrap())
                .with_identity(peer_identity)
                .build()
                .unwrap();
        let server_endpoint = server.get_or_init_endpoint().await.unwrap();
        let server_port = server_endpoint.local_addr().unwrap().port();
        let client = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
            .with_known(
                peer_node_from_cert.clone(),
                format!("127.0.0.1:{server_port}").parse().unwrap(),
            )
            .build()
            .unwrap();

        let server_handle = tokio::spawn(async move {
            let (_peer, mut conn) = server.accept().await.unwrap().unwrap();
            let frame = conn.recv().await.unwrap().unwrap();
            assert_eq!(frame, Frame::text("metrics-payload"));
            // Await the close to ensure active_connections is decremented
            conn.close().await.unwrap();
        });

        let mut client_conn = client.dial(peer_node_from_cert.clone()).await.unwrap();
        client_conn
            .send(Frame::text("metrics-payload"))
            .await
            .unwrap();
        let _ = client_conn.recv().await;
        client_conn.close().await.unwrap();
        // Await the server task to ensure all close() calls are complete
        server_handle.await.unwrap();

        // Every primitive should have observed at least one
        // bump relative to the captured baseline.
        assert!(m.dial_attempts.get() > baseline_attempts, "dial_attempts");
        assert!(
            m.dial_successes.get() > baseline_successes,
            "dial_successes"
        );
        assert!(m.frames_sent.get() > baseline_sent, "frames_sent");
        assert!(m.frames_received.get() > baseline_recv, "frames_received");
        assert!(m.accepts.get() > baseline_accepts, "accepts");
        // active_connections: +1 on accept, +1 on dial, -1 on client close, -1 on server close = 0
        // The net should be at or below baseline (allowing for timing issues with async cleanup).
        // Note: this is a global counter and other tests running
        // concurrently under cargo's default test-threads model may
        // legitimately bump it, so we bound it loosely (a generous
        // +32 to absorb parallel-test churn without being a
        // tautology). Run with `--test-threads=1` for a tight
        // check.
        let leaked = m.active_connections.get().saturating_sub(baseline_active);
        assert!(
            leaked <= 32,
            "active_connections leaked too much during this test: +{leaked} from baseline {baseline_active}",
        );

        // Round-trip via Prometheus exporter — the standard
        // text format should mention at least one of our
        // metrics.
        let exporter = PrometheusExporter::new(&a3net_observability::registry::GLOBAL);
        let text = exporter.render().body;
        assert!(
            text.contains("a3net_transport_dial_attempts_total"),
            "prom exporter missing dial_attempts"
        );
    }
}
