//! Prometheus metrics for `a3net-dns-server`.
//!
//! The DNS server is a long-running component. Operators need
//! observability into the traffic it serves and the resolution path
//! each query took (local cache hit vs. HTTP relay vs. mainline DHT).
//!
//! We expose a single [`Metrics`] instance that owns a static
//! [`prometheus::Registry`]. Every counter / histogram below is
//! registered into that registry at construction time, and the
//! renderer serialises the registry in the standard text exposition
//! format (with no need for the `protobuf` feature).
//!
//! ## Counters
//!
//! - `a3net_dns_queries_total{result="hit|miss|error"}` — total queries
//!   served by the DNS protocol handler, partitioned by outcome.
//! - `a3net_dns_doh_queries_total{method="get|post",status="2xx|4xx|5xx"}` —
//!   total DoH requests, partitioned by HTTP method and response
//!   status.
//! - `a3net_dns_pkarr_publishes_total{result="ok|invalid|error"}`
//! - `a3net_dns_pkarr_resolves_total{source="local|http|dht",result="ok|miss|error"}`
//!
//! ## Gauges
//!
//! - `a3net_dns_zone_records` — number of records currently stored
//!   (refreshed whenever [`crate::zone::ZoneStore::put`] /
//!   [`crate::zone::ZoneStore::delete`] /
//!   [`crate::zone::ZoneStore::evict_expired`] runs).
//!
//! ## Histograms
//!
//! - `a3net_dns_query_duration_seconds` — wall-clock time spent
//!   answering a single DNS query.
use prometheus::{
    Encoder, Histogram, HistogramOpts, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};
use std::sync::Arc;

/// All Prometheus metrics exposed by the DNS server.
///
/// Cloning a `Metrics` handle is cheap — every field is an `Arc` over
/// the registry entries, so the gauge / counter state is shared.
#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Registry>,

    pub queries: IntCounterVec,
    pub doh_queries: IntCounterVec,
    pub pkarr_publishes: IntCounterVec,
    pub pkarr_resolves: IntCounterVec,
    pub zone_records: IntGauge,
    pub query_duration: Histogram,
}

impl Metrics {
    /// Build a fresh registry and register every counter / histogram.
    pub fn new() -> Self {
        let registry = Registry::new();

        let queries = IntCounterVec::new(
            Opts::new(
                "a3net_dns_queries_total",
                "Total DNS queries handled, partitioned by result (hit|miss|error)",
            ),
            &["result"],
        )
        .expect("counter");
        registry
            .register(Box::new(queries.clone()))
            .expect("register queries");

        let doh_queries = IntCounterVec::new(
            Opts::new(
                "a3net_dns_doh_queries_total",
                "Total DoH requests, partitioned by HTTP method and response status",
            ),
            &["method", "status"],
        )
        .expect("counter");
        registry
            .register(Box::new(doh_queries.clone()))
            .expect("register doh_queries");

        let pkarr_publishes = IntCounterVec::new(
            Opts::new(
                "a3net_dns_pkarr_publishes_total",
                "Total pkarr publish operations and their result",
            ),
            &["result"],
        )
        .expect("counter");
        registry
            .register(Box::new(pkarr_publishes.clone()))
            .expect("register pkarr_publishes");

        let pkarr_resolves = IntCounterVec::new(
            Opts::new(
                "a3net_dns_pkarr_resolves_total",
                "Total pkarr resolve attempts by source and result",
            ),
            &["source", "result"],
        )
        .expect("counter");
        registry
            .register(Box::new(pkarr_resolves.clone()))
            .expect("register pkarr_resolves");

        let zone_records = IntGauge::new(
            "a3net_dns_zone_records",
            "Current number of records stored in the local zone store",
        )
        .expect("gauge");
        registry
            .register(Box::new(zone_records.clone()))
            .expect("register zone_records");

        let query_duration = Histogram::with_opts(
            HistogramOpts::new(
                "a3net_dns_query_duration_seconds",
                "Wall-clock seconds spent answering a DNS query",
            )
            .buckets(vec![
                0.000_05, 0.000_1, 0.000_5, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0,
            ]),
        )
        .expect("histogram");
        registry
            .register(Box::new(query_duration.clone()))
            .expect("register query_duration");

        Self {
            registry: Arc::new(registry),
            queries,
            doh_queries,
            pkarr_publishes,
            pkarr_resolves,
            zone_records,
            query_duration,
        }
    }

    /// Render the registry in Prometheus text exposition format.
    pub fn render(&self) -> Vec<u8> {
        let metric_families = self.registry.gather();
        let mut buf = Vec::with_capacity(4096);
        let encoder = TextEncoder::new();
        if let Err(e) = encoder.encode(&metric_families, &mut buf) {
            tracing::warn!(error = %e, "metrics encode");
        }
        buf
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_increment() {
        let m = Metrics::new();
        m.queries.with_label_values(&["hit"]).inc();
        m.queries.with_label_values(&["miss"]).inc_by(3);
        m.doh_queries
            .with_label_values(&["post", "2xx"])
            .inc_by(2);
        m.pkarr_publishes.with_label_values(&["ok"]).inc();
        m.pkarr_resolves.with_label_values(&["dht", "miss"]).inc();
        m.zone_records.set(42);
        m.query_duration.observe(0.0123);

        let text = String::from_utf8(m.render()).expect("utf8");
        assert!(text.contains("a3net_dns_queries_total{result=\"hit\"} 1"), "{text}");
        assert!(text.contains("a3net_dns_queries_total{result=\"miss\"} 3"), "{text}");
        assert!(
            text.contains("a3net_dns_doh_queries_total{method=\"post\",status=\"2xx\"} 2"),
            "{text}"
        );
        assert!(
            text.contains("a3net_dns_pkarr_publishes_total{result=\"ok\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("a3net_dns_pkarr_resolves_total{result=\"miss\",source=\"dht\"} 1"),
            "{text}"
        );
        assert!(text.contains("a3net_dns_zone_records 42"), "{text}");
        assert!(
            text.contains("a3net_dns_query_duration_seconds_count 1"),
            "{text}"
        );
    }

    #[test]
    fn render_is_idempotent() {
        let m = Metrics::new();
        let a = m.render();
        let b = m.render();
        assert_eq!(a, b);
    }
}
