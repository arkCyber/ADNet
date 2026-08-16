//! Relay-layer metrics — Counter / Gauge primitives backed
//! by the global `a3net-observability` registry.
//!
//! All metrics are **registered eagerly** on first access via
//! the `relay_metrics()` / `billing_metrics()` static helpers.
//!
//! ## Metric names
//!
//! All names are prefixed with `a3net_relay_*` so the
//! Prometheus output is namespaced.
//!
//! ## Cardinality
//!
//! Low-cardinality by design. The only label is
//! `{outcome="ok"|"filtered"|"error"}` on the forward counter
//! to show all outcome paths.

use std::sync::Arc;

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::{Registry, GLOBAL as OBSERVABILITY_GLOBAL};
use once_cell::sync::Lazy;

// ─────────────────────────── RelayMetrics ─────────────────────────────────

/// Relay-layer metric handle.
#[derive(Debug, Clone)]
pub struct RelayMetrics {
    /// `a3net_relay_requests_total` — total HTTP requests
    /// received at the `/exodus-mesh/fetch` endpoint.
    pub requests: Arc<Counter>,
    /// `a3net_relay_forwards_total` — total requests that
    /// passed the policy filter and were forwarded upstream.
    pub forwards: Arc<Counter>,
    /// `a3net_relay_bytes_sent_total` — total bytes sent to
    /// upstream (response body).
    pub bytes_sent: Arc<Counter>,
    /// `a3net_relay_bytes_received_total` — total bytes
    /// received from upstream (response body).
    pub bytes_received: Arc<Counter>,
    /// `a3net_relay_policy_filtered_total` — total requests
    /// blocked by the upstream policy.
    pub policy_filtered: Arc<Counter>,
    /// `a3net_relay_upstream_errors_total` — total requests
    /// that reached the upstream but returned an error.
    pub upstream_errors: Arc<Counter>,
    /// `a3net_relay_active_sessions` — number of HTTP requests
    /// currently being served (incremented on entry, decremented
    /// on response completion). Useful for spotting stuck or
    /// slow requests that are holding a connection open.
    pub active_sessions: Arc<Gauge>,
}

impl RelayMetrics {
    /// Register every metric into `registry`. Idempotent.
    pub fn register(registry: &Registry) -> Self {
        Self {
            requests: registry.register_counter(
                "a3net_relay_requests_total",
                "Total HTTP requests to /exodus-mesh/fetch.",
            ),
            forwards: registry.register_counter(
                "a3net_relay_forwards_total",
                "Total requests forwarded upstream (past the policy filter).",
            ),
            bytes_sent: registry.register_counter(
                "a3net_relay_bytes_sent_total",
                "Total bytes sent to upstream (response body).",
            ),
            bytes_received: registry.register_counter(
                "a3net_relay_bytes_received_total",
                "Total bytes received from upstream (response body).",
            ),
            policy_filtered: registry.register_counter(
                "a3net_relay_policy_filtered_total",
                "Total requests blocked by the upstream policy.",
            ),
            upstream_errors: registry.register_counter(
                "a3net_relay_upstream_errors_total",
                "Total upstream requests that returned a non-2xx response.",
            ),
            active_sessions: registry.register_gauge(
                "a3net_relay_active_sessions",
                "HTTP requests currently in flight at the relay (gauge).",
            ),
        }
    }

    /// Process-global handle.
    pub fn get() -> Self {
        RELAY_GLOBAL.clone()
    }
}

static RELAY_GLOBAL: Lazy<RelayMetrics> =
    Lazy::new(|| RelayMetrics::register(&OBSERVABILITY_GLOBAL));

// ─────────────────────────── BillingMetrics ────────────────────────────────

/// Billing-layer metric handle. Only available when the
/// `billing` feature is enabled.
#[cfg(feature = "billing")]
#[derive(Debug, Clone)]
pub struct BillingMetrics {
    /// `a3net_relay_pledges_total` — total pledge attempts
    /// received at `/relay/billing/pledge`.
    pub pledges: Arc<Counter>,
    /// `a3net_relay_pledge_errors_total` — total pledge
    /// attempts that returned an error (malformed, replay, …).
    pub pledge_errors: Arc<Counter>,
    /// `a3net_relay_receipts_issued_total` — total receipts
    /// signed and issued.
    pub receipts_issued: Arc<Counter>,
    /// `a3net_relay_receipts_redeemed_total` — total receipt
    /// redemption attempts.
    pub receipts_redeemed: Arc<Counter>,
    /// `a3net_relay_receipt_redeem_errors_total` — total
    /// receipt redemption attempts that returned an error
    /// (invalid sig, insufficient balance, …).
    pub receipt_redeem_errors: Arc<Counter>,
    /// `a3net_relay_receipt_redeem_success_total` — total
    /// receipt redemptions that succeeded.
    pub receipt_redeem_success: Arc<Counter>,
}

#[cfg(feature = "billing")]
impl BillingMetrics {
    /// Register every metric into `registry`. Idempotent.
    pub fn register(registry: &Registry) -> Self {
        Self {
            pledges: registry.register_counter(
                "a3net_relay_pledges_total",
                "Total pledge attempts received.",
            ),
            pledge_errors: registry.register_counter(
                "a3net_relay_pledge_errors_total",
                "Total pledge attempts that returned an error.",
            ),
            receipts_issued: registry.register_counter(
                "a3net_relay_receipts_issued_total",
                "Total receipts signed and issued.",
            ),
            receipts_redeemed: registry.register_counter(
                "a3net_relay_receipts_redeemed_total",
                "Total receipt redemption attempts.",
            ),
            receipt_redeem_errors: registry.register_counter(
                "a3net_relay_receipt_redeem_errors_total",
                "Total receipt redemptions that returned an error.",
            ),
            receipt_redeem_success: registry.register_counter(
                "a3net_relay_receipt_redeem_success_total",
                "Total receipt redemptions that succeeded.",
            ),
        }
    }

    /// Process-global handle.
    pub fn get() -> Self {
        BILLING_GLOBAL.clone()
    }
}

#[cfg(feature = "billing")]
static BILLING_GLOBAL: Lazy<BillingMetrics> =
    Lazy::new(|| BillingMetrics::register(&OBSERVABILITY_GLOBAL));

#[cfg(test)]
mod tests {
    use super::*;

    fn relay_for_tests() -> (RelayMetrics, std::sync::Arc<Registry>) {
        let registry = std::sync::Arc::new(Registry::default());
        (RelayMetrics::register(&registry), registry)
    }

    #[test]
    fn relay_metrics_register_all() {
        let (m, registry) = relay_for_tests();
        let snap = registry.snapshot();
        let names: Vec<String> =
            snap.metrics.iter().map(|x| x.name().to_string()).collect();
        assert!(names.contains(&"a3net_relay_requests_total".to_string()));
        assert!(names.contains(&"a3net_relay_forwards_total".to_string()));
        assert!(names.contains(&"a3net_relay_policy_filtered_total".to_string()));
        assert!(names.contains(&"a3net_relay_bytes_sent_total".to_string()));
        assert!(names.contains(&"a3net_relay_bytes_received_total".to_string()));
        assert!(names.contains(&"a3net_relay_upstream_errors_total".to_string()));
        assert!(names.contains(&"a3net_relay_active_sessions".to_string()));
        m.requests.inc_by(10);
        m.forwards.inc_by(8);
        m.policy_filtered.inc_by(2);
        assert_eq!(m.requests.get(), 10);
        assert_eq!(m.forwards.get(), 8);
        assert_eq!(m.policy_filtered.get(), 2);
    }

    #[test]
    fn relay_get_is_idempotent() {
        let a = RelayMetrics::get();
        let b = RelayMetrics::get();
        a.requests.inc();
        assert!(b.requests.get() >= 1);
    }

    #[cfg(feature = "billing")]
    fn billing_for_tests() -> (BillingMetrics, std::sync::Arc<Registry>) {
        let registry = std::sync::Arc::new(Registry::default());
        (BillingMetrics::register(&registry), registry)
    }

    #[cfg(feature = "billing")]
    #[test]
    fn billing_metrics_register_all() {
        let (m, registry) = billing_for_tests();
        let snap = registry.snapshot();
        let names: Vec<String> =
            snap.metrics.iter().map(|x| x.name().to_string()).collect();
        assert!(names.contains(&"a3net_relay_pledges_total".to_string()));
        assert!(names.contains(&"a3net_relay_pledge_errors_total".to_string()));
        assert!(names.contains(&"a3net_relay_receipts_issued_total".to_string()));
        assert!(names.contains(&"a3net_relay_receipts_redeemed_total".to_string()));
        assert!(names.contains(&"a3net_relay_receipt_redeem_errors_total".to_string()));
        assert!(names.contains(&"a3net_relay_receipt_redeem_success_total".to_string()));
        m.pledges.inc_by(5);
        m.receipt_redeem_success.inc_by(3);
        assert_eq!(m.pledges.get(), 5);
        assert_eq!(m.receipt_redeem_success.get(), 3);
    }
}
