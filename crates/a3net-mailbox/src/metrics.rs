//! Metrics for the mailbox server.
//!
//! Mirrors `a3net-relay`'s `RelayMetrics` pattern: every metric is
//! registered eagerly into the global `a3net-observability` registry
//! on first access, and the per-field types are `Arc<Counter>` /
//! `Arc<Gauge>` so the [`MailboxMetrics`] handle is cheap to clone
//! and share across axum handlers.

use std::sync::Arc;

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::{Registry, GLOBAL as OBSERVABILITY_GLOBAL};
use once_cell::sync::Lazy;

/// Mailbox-layer metric handle.
#[derive(Debug, Clone)]
pub struct MailboxMetrics {
    /// `a3net_mailbox_enqueues_total` — total `enqueue` requests
    /// accepted by the server.
    pub enqueues: Arc<Counter>,
    /// `a3net_mailbox_enqueues_rejected_total` — total `enqueue`
    /// requests rejected by the policy layer (oversize, quota,
    /// invalid signature, ...).
    pub enqueues_rejected: Arc<Counter>,
    /// `a3net_mailbox_pulls_total` — total `pull` requests served.
    pub pulls: Arc<Counter>,
    /// `a3net_mailbox_acks_total` — total `ack` requests served.
    pub acks: Arc<Counter>,
    /// `a3net_mailbox_purged_total` — total envelopes purged by the
    /// background sweeper.
    pub purged: Arc<Counter>,
    /// `a3net_mailbox_queue_depth` — current count of envelopes held
    /// in the queue (across all recipients).
    pub queue_depth: Arc<Gauge>,
    /// `a3net_mailbox_active_recipients` — current number of
    /// recipients with at least one envelope.
    pub active_recipients: Arc<Gauge>,
}

impl MailboxMetrics {
    /// Register every metric into `registry`. Idempotent.
    pub fn register(registry: &Registry) -> Self {
        Self {
            enqueues: registry.register_counter(
                "a3net_mailbox_enqueues_total",
                "Total enqueue requests accepted by the mailbox server.",
            ),
            enqueues_rejected: registry.register_counter(
                "a3net_mailbox_enqueues_rejected_total",
                "Total enqueue requests rejected by the policy layer.",
            ),
            pulls: registry.register_counter(
                "a3net_mailbox_pulls_total",
                "Total pull requests served by the mailbox server.",
            ),
            acks: registry.register_counter(
                "a3net_mailbox_acks_total",
                "Total ack requests served by the mailbox server.",
            ),
            purged: registry.register_counter(
                "a3net_mailbox_purged_total",
                "Total envelopes purged by the background sweeper.",
            ),
            queue_depth: registry.register_gauge(
                "a3net_mailbox_queue_depth",
                "Current envelope count held in the queue (gauge).",
            ),
            active_recipients: registry.register_gauge(
                "a3net_mailbox_active_recipients",
                "Current number of recipients with at least one envelope (gauge).",
            ),
        }
    }

    /// Get the process-wide singleton handle.
    pub fn get() -> Self {
        MAILBOX_GLOBAL.clone()
    }
}

static MAILBOX_GLOBAL: Lazy<MailboxMetrics> =
    Lazy::new(|| MailboxMetrics::register(&OBSERVABILITY_GLOBAL));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_metrics_register_all() {
        let registry = Registry::default();
        let m = MailboxMetrics::register(&registry);
        let snap = registry.snapshot();
        let names: Vec<String> = snap
            .metrics
            .iter()
            .map(|x| x.name().to_string())
            .collect();
        assert!(names.contains(&"a3net_mailbox_enqueues_total".to_string()));
        assert!(names.contains(&"a3net_mailbox_enqueues_rejected_total".to_string()));
        assert!(names.contains(&"a3net_mailbox_pulls_total".to_string()));
        assert!(names.contains(&"a3net_mailbox_acks_total".to_string()));
        assert!(names.contains(&"a3net_mailbox_purged_total".to_string()));
        assert!(names.contains(&"a3net_mailbox_queue_depth".to_string()));
        assert!(names.contains(&"a3net_mailbox_active_recipients".to_string()));
        m.enqueues.inc_by(7);
        assert_eq!(m.enqueues.get(), 7);
    }

    #[test]
    fn mailbox_get_is_idempotent() {
        let a = MailboxMetrics::get();
        let b = MailboxMetrics::get();
        a.enqueues.inc();
        assert!(b.enqueues.get() >= 1);
    }
}
