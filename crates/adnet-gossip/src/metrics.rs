//! Gossip-layer metrics — Counter / Gauge primitives backed
//! by the global `adnet-observability` registry.
//!
//! All metrics are **registered eagerly** into the global
//! registry on first access via the `gossip_metrics()` static
//! helper. The exporter picks them up automatically; no wiring
//! is needed at the call site.
//!
//! ## Metric names
//!
//! All names are prefixed with `adnet_gossip_*` so the
//! Prometheus output is namespaced.
//!
//! ## Cardinality
//!
//! Each metric is intentionally low-cardinality. The only
//! label set we add is `{outcome="ok"|"err"}` on counters that
//! span both success and failure paths; room-id or topic-id
//! labels are deliberately **not** used — a malicious peer
//! could otherwise inflate the registry to O(rooms).

use std::sync::Arc;

use adnet_observability::metrics::Counter;
use adnet_observability::registry::{GLOBAL as OBSERVABILITY_GLOBAL, Registry};
use once_cell::sync::Lazy;

/// Per-layer metric handle. Constructed once per process via
/// [`GossipMetrics::get`]. The fields are `Arc`-shared so
/// cloning the handle is cheap.
#[derive(Debug, Clone)]
pub struct GossipMetrics {
    /// `adnet_gossip_publishes_total` — total local
    /// announcements pushed into the transport. Outcome is
    /// not labelled; we have a separate `publish_errors_total`
    /// counter for the failure path.
    pub publishes: Arc<Counter>,
    /// `adnet_gossip_publish_errors_total` — total `publish`
    /// calls that returned `Err`.
    pub publish_errors: Arc<Counter>,
    /// `adnet_gossip_deliveries_total` — total announcements
    /// successfully delivered to a local subscriber (the
    /// `decode_stream` wrapper sent a value downstream).
    pub deliveries: Arc<Counter>,
    /// `adnet_gossip_decode_failures_total` — total
    /// payloads that `bridge::unwrap` rejected (bad JSON,
    /// missing fields, …).
    pub decode_failures: Arc<Counter>,
    /// `adnet_gossip_subscribes_total` — total `subscribe`
    /// calls (one per `Receiver` handed out).
    pub subscribes: Arc<Counter>,
    /// `adnet_gossip_unsubscribes_total` — total `Receiver`s
    /// dropped (close).
    pub unsubscribes: Arc<Counter>,
    /// `adnet_gossip_joins_total` — total `join_room` calls.
    pub joins: Arc<Counter>,
    /// `adnet_gossip_leaves_total` — total `leave_room` calls.
    pub leaves: Arc<Counter>,
}

/// Module-level lazy global handle — ensures only one
/// `register()` call, even under concurrent first access.
static GLOBAL: Lazy<GossipMetrics> = Lazy::new(|| GossipMetrics::register(&OBSERVABILITY_GLOBAL));

impl GossipMetrics {
    /// Register every metric into `registry`. Idempotent —
    /// a second call re-uses the existing metric handles.
    pub fn register(registry: &Registry) -> Self {
        Self {
            publishes: registry.register_counter(
                "adnet_gossip_publishes_total",
                "Total gossip publishes by the local node.",
            ),
            publish_errors: registry.register_counter(
                "adnet_gossip_publish_errors_total",
                "Total gossip publishes that returned an error.",
            ),
            deliveries: registry.register_counter(
                "adnet_gossip_deliveries_total",
                "Total gossip announcements delivered to a local subscriber.",
            ),
            decode_failures: registry.register_counter(
                "adnet_gossip_decode_failures_total",
                "Total gossip payloads that failed to decode into a typed Announcement.",
            ),
            subscribes: registry.register_counter(
                "adnet_gossip_subscribes_total",
                "Total subscribe calls (one per Receiver handed out).",
            ),
            unsubscribes: registry.register_counter(
                "adnet_gossip_unsubscribes_total",
                "Total subscribe receivers dropped.",
            ),
            joins: registry.register_counter("adnet_gossip_joins_total", "Total join_room calls."),
            leaves: registry
                .register_counter("adnet_gossip_leaves_total", "Total leave_room calls."),
        }
    }

    /// Process-global handle. First access registers the
    /// metrics into the global registry via the module-level
    /// `GLOBAL` static; subsequent access returns a clone of
    /// the same handle.
    ///
    /// Uses `Lazy` so `get()` is idempotent — the `Lazy`
    /// guarantees only one call to `register`, even across
    /// concurrent first-access. This is the same pattern
    /// `TransportMetrics::get()` uses in `adnet-transport`.
    pub fn get() -> Self {
        GLOBAL.clone()
    }

    /// Test-only constructor that uses an isolated registry.
    #[cfg(test)]
    pub fn for_tests() -> (Self, Arc<Registry>) {
        let registry = Arc::new(Registry::default());
        (Self::register(&registry), registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_creates_all_metrics_with_unique_names() {
        let (m, registry) = GossipMetrics::for_tests();
        let snap = registry.snapshot();
        let names: Vec<String> = snap.metrics.iter().map(|x| x.name().to_string()).collect();
        assert!(names.contains(&"adnet_gossip_publishes_total".to_string()));
        assert!(names.contains(&"adnet_gossip_publish_errors_total".to_string()));
        assert!(names.contains(&"adnet_gossip_deliveries_total".to_string()));
        assert!(names.contains(&"adnet_gossip_decode_failures_total".to_string()));
        assert!(names.contains(&"adnet_gossip_subscribes_total".to_string()));
        assert!(names.contains(&"adnet_gossip_unsubscribes_total".to_string()));
        assert!(names.contains(&"adnet_gossip_joins_total".to_string()));
        assert!(names.contains(&"adnet_gossip_leaves_total".to_string()));

        m.publishes.inc_by(3);
        m.publish_errors.inc();
        m.deliveries.inc_by(5);
        m.subscribes.inc();
        m.joins.inc();
        assert_eq!(m.publishes.get(), 3);
        assert_eq!(m.publish_errors.get(), 1);
        assert_eq!(m.deliveries.get(), 5);
        assert_eq!(m.subscribes.get(), 1);
        assert_eq!(m.joins.get(), 1);
    }

    #[test]
    fn get_is_idempotent_and_returns_same_handle() {
        let a = GossipMetrics::get();
        let b = GossipMetrics::get();
        a.publishes.inc();
        assert!(b.publishes.get() >= 1);
    }
}
