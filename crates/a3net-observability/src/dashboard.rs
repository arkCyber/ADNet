//! Dashboard JSON — a single typed payload that Grafana /
//! status pages / `a3net status` can consume. The metrics
//! surface is the same data the Prometheus exporter renders,
//! but shaped as a **named section** rather than a flat label
//! soup:
//!
//! ```json
//! {
//!   "now_unix_ms": 1691740000000,
//!   "storage":   { "private_used_bytes": ..., "shared_used_bytes": ... },
//!   "replication": { "factor": 3, "sweeps_total": 17, "blocks_pushed_total": 1024 },
//!   "blobstore":  { "blobs_total": 409, "store_size_bytes": 1.2e9 },
//!   "share":      { "receive_bytes_total": 5.6e7, "receive_files_total": 8 },
//!   "alerts":     [ { "level": "warn", "code": "STOREFULL_PRIVATE", "message": "..." } ]
//! }
//! ```
//!
//! The export is operator-facing: values are human-readable
//! (bytes as `u64`, not exponential), timestamps are ISO-8601
//! in addition to Unix millis, and known threshold breaches
//! surface as `alerts: []` entries that a status page can
//! post to Slack / PagerDuty without further processing.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::labels::Label;
use crate::registry::Registry;
use crate::registry::RegistrySnapshot;

/// Well-known metric names this dashboard reads. Crates
/// upstream may register additional metrics; the dashboard
/// also surfaces them under `metrics.extra` so nothing is lost.
pub const NAME_BLOBSTORE_SIZE: &str = "a3net_blob_store_size_bytes";
pub const NAME_BLOBSTORE_BLOBS: &str = "a3net_blob_blobs_total";
pub const NAME_REPLICATOR_SWEEPS: &str = "a3net_replicator_sweeps_total";
pub const NAME_REPLICATOR_PUSHES: &str = "a3net_replicator_blocks_pushed_total";
pub const NAME_REPLICATOR_PUSH_ERRORS: &str = "a3net_replicator_push_errors_total";
pub const NAME_REPLICATOR_HASHES: &str = "a3net_replicator_hashes_verified_total";
pub const NAME_REPLICATOR_UNDER: &str = "a3net_replicator_under_replicated_blocks";
pub const NAME_REPLICATOR_FULL: &str = "a3net_replicator_fully_replicated_blocks";
pub const NAME_SHARE_RECEIVE_BYTES: &str = "a3net_share_receive_bytes_total";
pub const NAME_SHARE_RECEIVE_FILES: &str = "a3net_share_receive_files_total";
pub const NAME_SHARE_RECEIVE_ERRORS: &str = "a3net_share_receive_errors_total";
pub const NAME_BLOBSTORE_READ_HASH_MISMATCH: &str = "a3net_blob_read_hash_mismatch_total";
pub const NAME_BLOBSTORE_QUARANTINED: &str = "a3net_blob_quarantined_total";
pub const NAME_BLOBSTORE_IMPORT_PATH_REJECTED: &str = "a3net_blob_import_path_rejected_total";

/// Storage section of the dashboard.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct StorageSection {
    pub private_used_bytes: u64,
    pub private_budget_bytes: u64,
    pub private_hard_cap_bytes: u64,
    pub shared_used_bytes: u64,
    pub shared_budget_bytes: u64,
    pub shared_hard_cap_bytes: u64,
    pub private_blobs: u64,
    pub shared_blobs: u64,
    pub factor: u8,
    pub sweep_interval_seconds: u64,
    /// Audit fix (P0-W): whether the on-disk quota policy
    /// is sealed. `true` after the first-ever `StorageTopology::open`.
    /// Monitors can alert on `false` for nodes that have
    /// never sealed the policy (i.e. never joined the
    /// distributed network).
    #[serde(default)]
    pub quota_sealed: bool,
    /// Audit fix (P0-W): unix-ms timestamp when the
    /// quota policy was sealed, or `None` if not yet
    /// sealed.
    #[serde(default)]
    pub quota_sealed_at_unix_ms: Option<i64>,
    /// Audit fix (P0-W): the list of public write paths
    /// into the shared scope. Must always equal
    /// `["accept_replica"]`. Monitors can alert on any
    /// discrepancy (the type system makes this impossible
    /// today, but the field is here for safety).
    #[serde(default)]
    pub shared_write_paths: Vec<String>,
}

/// Replication section — observable per-sweep counters.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ReplicationSection {
    pub factor: u8,
    pub sweeps_total: u64,
    pub blocks_pushed_total: u64,
    pub push_errors_total: u64,
    pub hashes_verified_total: u64,
    pub under_replicated_blocks: i64,
    pub fully_replicated_blocks: i64,
}

/// Blob-store section.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BlobStoreSection {
    pub store_size_bytes: i64,
    pub blobs_total: i64,
    pub read_hash_mismatch_total: u64,
    pub quarantined_total: u64,
    pub import_path_rejected_total: u64,
}

/// Share section.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ShareSection {
    pub receive_bytes_total: u64,
    pub receive_files_total: u64,
    pub receive_errors_total: u64,
}

/// One alert in the dashboard JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub level: AlertLevel,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Info,
    Warn,
    Critical,
}

/// Top-level dashboard payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dashboard {
    pub now_unix_ms: i64,
    pub now_iso8601: String,
    pub storage: StorageSection,
    pub replication: ReplicationSection,
    pub blobstore: BlobStoreSection,
    pub share: ShareSection,
    pub alerts: Vec<Alert>,
    /// Every metric the registry knows about that this
    /// dashboard didn't carve into a section. Useful for
    /// debugging and to keep the dashboard forward-compatible
    /// with new metrics.
    pub extras: BTreeMap<String, serde_json::Value>,
}

/// Tunable thresholds for the alert rail. Defaults match
/// the operator-facing rules in `AUDIT_RECENT_CODE_V*.md`.
#[derive(Debug, Clone)]
pub struct AlertThresholds {
    /// `private_used_bytes / private_hard_cap_bytes` ≥ this
    /// fraction ⇒ `Warn` "Private store nearly full".
    pub private_full_warn: f64,
    /// `shared_used_bytes / shared_hard_cap_bytes` ≥ this
    /// fraction ⇒ `Warn` "Shared store nearly full".
    pub shared_full_warn: f64,
    /// `push_errors_total / pushes_total` ≥ this fraction
    /// ⇒ `Warn` "Replication push error rate high".
    pub push_error_ratio_warn: f64,
    /// `read_hash_mismatch_total > 0` ⇒ `Critical` "Data
    /// integrity breach detected".
    pub hash_mismatch_critical: bool,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            private_full_warn: 0.90,
            shared_full_warn: 0.90,
            push_error_ratio_warn: 0.10,
            hash_mismatch_critical: true,
        }
    }
}

/// Build the dashboard from the live registry. The
/// `storage` section is **only populated** when the caller
/// fills it in via [`DashboardBuilder::with_storage`]; the
/// same holds for `replication.factor` which is a config
/// value, not a metric.
pub struct DashboardBuilder {
    registry: Arc<Registry>,
    storage: Option<StorageSection>,
    replication_factor: u8,
    sweep_interval_seconds: u64,
    thresholds: AlertThresholds,
}

impl DashboardBuilder {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            storage: None,
            replication_factor: 3,
            sweep_interval_seconds: 300,
            thresholds: AlertThresholds::default(),
        }
    }

    pub fn with_storage(mut self, storage: StorageSection) -> Self {
        self.storage = Some(storage);
        self
    }

    pub fn with_replication(mut self, factor: u8, sweep_interval_seconds: u64) -> Self {
        self.replication_factor = factor;
        self.sweep_interval_seconds = sweep_interval_seconds;
        self
    }

    pub fn with_thresholds(mut self, thresholds: AlertThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    pub fn build(&self) -> Dashboard {
        let snap: RegistrySnapshot = self.registry.snapshot();
        let mut blobstore = BlobStoreSection::default();
        let mut replication = ReplicationSection::default();
        let mut share = ShareSection::default();
        let mut extras: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let mut alerts: Vec<Alert> = Vec::new();

        // Populate the typed sections.
        blobstore.store_size_bytes = read_gauge(&snap, NAME_BLOBSTORE_SIZE, &[]);
        blobstore.blobs_total = read_gauge(&snap, NAME_BLOBSTORE_BLOBS, &[]);
        blobstore.read_hash_mismatch_total =
            read_counter(&snap, NAME_BLOBSTORE_READ_HASH_MISMATCH, &[]);
        blobstore.quarantined_total = read_counter(&snap, NAME_BLOBSTORE_QUARANTINED, &[]);
        blobstore.import_path_rejected_total =
            read_counter(&snap, NAME_BLOBSTORE_IMPORT_PATH_REJECTED, &[]);

        replication.factor = self.replication_factor;
        replication.sweeps_total = read_counter(&snap, NAME_REPLICATOR_SWEEPS, &[]);
        replication.blocks_pushed_total = read_counter(&snap, NAME_REPLICATOR_PUSHES, &[]);
        replication.push_errors_total =
            read_counter(&snap, NAME_REPLICATOR_PUSH_ERRORS, &[]);
        replication.hashes_verified_total =
            read_counter(&snap, NAME_REPLICATOR_HASHES, &[]);
        replication.under_replicated_blocks =
            read_gauge(&snap, NAME_REPLICATOR_UNDER, &[]);
        replication.fully_replicated_blocks =
            read_gauge(&snap, NAME_REPLICATOR_FULL, &[]);

        share.receive_bytes_total = read_counter(&snap, NAME_SHARE_RECEIVE_BYTES, &[]);
        share.receive_files_total = read_counter(&snap, NAME_SHARE_RECEIVE_FILES, &[]);
        share.receive_errors_total = read_counter(&snap, NAME_SHARE_RECEIVE_ERRORS, &[]);

        // Storage section is caller-supplied (BlobStore lives in
        // a different crate). Default to all-zero if absent.
        let storage = self.storage.clone().unwrap_or_default();
        let mut storage = storage;
        storage.factor = self.replication_factor;
        storage.sweep_interval_seconds = self.sweep_interval_seconds;

        // Compute alerts.
        if self.thresholds.hash_mismatch_critical
            && blobstore.read_hash_mismatch_total > 0
        {
            alerts.push(Alert {
                level: AlertLevel::Critical,
                code: "DATA_TAMPER_DETECTED".to_string(),
                message: format!(
                    "{} read-hash-mismatch events since process start; \
                     check `.quarantine/` for affected blobs",
                    blobstore.read_hash_mismatch_total
                ),
            });
        }
        if storage.private_hard_cap_bytes > 0 {
            let ratio = storage.private_used_bytes as f64
                / storage.private_hard_cap_bytes as f64;
            if ratio >= self.thresholds.private_full_warn {
                alerts.push(Alert {
                    level: AlertLevel::Warn,
                    code: "STOREFULL_PRIVATE".to_string(),
                    message: format!(
                        "private storage at {:.1}% of hard cap ({} / {} bytes)",
                        ratio * 100.0,
                        storage.private_used_bytes,
                        storage.private_hard_cap_bytes
                    ),
                });
            }
        }
        if storage.shared_hard_cap_bytes > 0 {
            let ratio = storage.shared_used_bytes as f64
                / storage.shared_hard_cap_bytes as f64;
            if ratio >= self.thresholds.shared_full_warn {
                alerts.push(Alert {
                    level: AlertLevel::Warn,
                    code: "STOREFULL_SHARED".to_string(),
                    message: format!(
                        "shared storage at {:.1}% of hard cap ({} / {} bytes)",
                        ratio * 100.0,
                        storage.shared_used_bytes,
                        storage.shared_hard_cap_bytes
                    ),
                });
            }
        }
        if replication.blocks_pushed_total > 0 {
            let ratio = replication.push_errors_total as f64
                / replication.blocks_pushed_total as f64;
            if ratio >= self.thresholds.push_error_ratio_warn {
                alerts.push(Alert {
                    level: AlertLevel::Warn,
                    code: "REPLICATOR_PUSH_ERROR_RATE".to_string(),
                    message: format!(
                        "{:.1}% of push attempts failed ({}/{}). Check network / peer list.",
                        ratio * 100.0,
                        replication.push_errors_total,
                        replication.blocks_pushed_total
                    ),
                });
            }
        }

        // Forwards-compatible: every metric that did NOT match
        // a known name surfaces under `extras` with its name
        // and current value so a future operator can see
        // everything the registry knows about.
        let known = [
            NAME_BLOBSTORE_SIZE,
            NAME_BLOBSTORE_BLOBS,
            NAME_REPLICATOR_SWEEPS,
            NAME_REPLICATOR_PUSHES,
            NAME_REPLICATOR_PUSH_ERRORS,
            NAME_REPLICATOR_HASHES,
            NAME_REPLICATOR_UNDER,
            NAME_REPLICATOR_FULL,
            NAME_SHARE_RECEIVE_BYTES,
            NAME_SHARE_RECEIVE_FILES,
            NAME_SHARE_RECEIVE_ERRORS,
            NAME_BLOBSTORE_READ_HASH_MISMATCH,
            NAME_BLOBSTORE_QUARANTINED,
            NAME_BLOBSTORE_IMPORT_PATH_REJECTED,
        ];
        let known: std::collections::HashSet<&str> = known.iter().copied().collect();
        for m in snap.sorted() {
            if known.contains(m.name()) {
                continue;
            }
            let value = render_metric_as_json(&*m);
            extras.insert(m.name().to_string(), value);
        }

        let now = chrono::Utc::now();
        Dashboard {
            now_unix_ms: now.timestamp_millis(),
            now_iso8601: now.to_rfc3339(),
            storage,
            replication,
            blobstore,
            share,
            alerts,
            extras,
        }
    }
}

/// Read the unlabeled value of a counter from the snapshot.
fn read_counter(snap: &RegistrySnapshot, name: &str, _labels: &[Label]) -> u64 {
    snap.sorted()
        .iter()
        .find(|m| m.name() == name)
        .and_then(|m| m.as_any().downcast_ref::<crate::metrics::Counter>())
        .map(|c| c.get())
        .unwrap_or(0)
}

fn read_gauge(snap: &RegistrySnapshot, name: &str, _labels: &[Label]) -> i64 {
    snap.sorted()
        .iter()
        .find(|m| m.name() == name)
        .and_then(|m| m.as_any().downcast_ref::<crate::metrics::Gauge>())
        .map(|g| g.get())
        .unwrap_or(0)
}

fn render_metric_as_json(m: &dyn crate::metrics::Metric) -> serde_json::Value {
    use crate::histogram::Histogram;
    use crate::metrics::{Counter, Gauge};
    if let Some(c) = m.as_any().downcast_ref::<Counter>() {
        serde_json::json!({
            "kind": "counter",
            "help": m.help(),
            "value": c.get(),
        })
    } else if let Some(g) = m.as_any().downcast_ref::<Gauge>() {
        serde_json::json!({
            "kind": "gauge",
            "help": m.help(),
            "value": g.get(),
        })
    } else if let Some(h) = m.as_any().downcast_ref::<Histogram>() {
        let snap = h.snapshot();
        serde_json::json!({
            "kind": "histogram",
            "help": m.help(),
            "count": snap.count,
            "sum": snap.sum,
        })
    } else {
        serde_json::json!({
            "kind": "unknown",
            "help": m.help(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    #[test]
    fn dashboard_json_is_well_formed() {
        let reg = Arc::new(Registry::default());
        let c = reg.register_counter(NAME_REPLICATOR_SWEEPS, "sweeps");
        c.inc();
        c.inc();
        let dash = DashboardBuilder::new(Arc::clone(&reg)).build();
        assert_eq!(dash.replication.sweeps_total, 2);
        assert!(dash.now_unix_ms > 0);
        assert!(!dash.now_iso8601.is_empty());
    }

    #[test]
    fn dashboard_storage_section_is_respected() {
        let reg = Arc::new(Registry::default());
        let storage = StorageSection {
            private_used_bytes: 900,
            private_budget_bytes: 1000,
            private_hard_cap_bytes: 1000,
            shared_used_bytes: 0,
            shared_budget_bytes: 500,
            shared_hard_cap_bytes: 500,
            private_blobs: 0,
            shared_blobs: 0,
            factor: 3,
            sweep_interval_seconds: 300,
            // Audit fix P0-W: defaults for new fields.
            quota_sealed: true,
            quota_sealed_at_unix_ms: Some(0),
            shared_write_paths: vec!["accept_replica".into()],
        };
        let dash = DashboardBuilder::new(Arc::clone(&reg))
            .with_storage(storage)
            .build();
        assert_eq!(dash.storage.private_used_bytes, 900);
        assert_eq!(dash.storage.shared_hard_cap_bytes, 500);
        // Audit fix P0-W: the dashboard JSON must surface
        // the sealed-scope invariant so monitors can
        // enforce it.
        assert!(dash.storage.quota_sealed);
        assert_eq!(
            dash.storage.shared_write_paths,
            vec!["accept_replica".to_string()]
        );
    }

    #[test]
    fn dashboard_alerts_trigger_on_thresholds() {
        let reg = Arc::new(Registry::default());
        let c = reg.register_counter(NAME_BLOBSTORE_READ_HASH_MISMATCH, "mm");
        c.inc();
        let storage = StorageSection {
            private_used_bytes: 950,
            private_budget_bytes: 1000,
            private_hard_cap_bytes: 1000,
            ..Default::default()
        };
        let dash = DashboardBuilder::new(Arc::clone(&reg))
            .with_storage(storage)
            .build();
        let codes: Vec<&str> = dash.alerts.iter().map(|a| a.code.as_str()).collect();
        assert!(
            codes.contains(&"DATA_TAMPER_DETECTED"),
            "expected DATA_TAMPER_DETECTED, alerts={codes:?}"
        );
        assert!(
            codes.contains(&"STOREFULL_PRIVATE"),
            "expected STOREFULL_PRIVATE, alerts={codes:?}"
        );
    }

    #[test]
    fn dashboard_alerts_disabled_when_threshold_is_zero() {
        let reg = Arc::new(Registry::default());
        let c = reg.register_counter(NAME_BLOBSTORE_READ_HASH_MISMATCH, "mm");
        c.inc();
        let dash = DashboardBuilder::new(Arc::clone(&reg))
            .with_thresholds(AlertThresholds {
                hash_mismatch_critical: false,
                ..Default::default()
            })
            .build();
        let has_critical = dash
            .alerts
            .iter()
            .any(|a| a.code == "DATA_TAMPER_DETECTED");
        assert!(!has_critical, "DATA_TAMPER_DETECTED must be disabled");
    }

    #[test]
    fn dashboard_unknown_metric_lands_in_extras() {
        let reg = Arc::new(Registry::default());
        let c = reg.register_counter("a3net_test_widget_total", "demo");
        c.inc_by(7);
        let dash = DashboardBuilder::new(Arc::clone(&reg)).build();
        assert_eq!(
            dash.extras["a3net_test_widget_total"]["value"].as_u64(),
            Some(7)
        );
    }

    #[test]
    fn dashboard_serde_roundtrip() {
        let reg = Arc::new(Registry::default());
        let dash = DashboardBuilder::new(Arc::clone(&reg)).build();
        let s = serde_json::to_string(&dash).unwrap();
        let back: Dashboard = serde_json::from_str(&s).unwrap();
        assert_eq!(dash.now_unix_ms, back.now_unix_ms);
        assert_eq!(dash.replication.factor, back.replication.factor);
    }

    #[test]
    fn dashboard_includes_shared_full_alert() {
        let reg = Arc::new(Registry::default());
        let storage = StorageSection {
            shared_used_bytes: 950,
            shared_budget_bytes: 1000,
            shared_hard_cap_bytes: 1000,
            ..Default::default()
        };
        let dash = DashboardBuilder::new(Arc::clone(&reg))
            .with_storage(storage)
            .build();
        let codes: Vec<&str> = dash.alerts.iter().map(|a| a.code.as_str()).collect();
        assert!(codes.contains(&"STOREFULL_SHARED"), "alerts={codes:?}");
    }

    #[test]
    fn dashboard_push_error_rate_alert() {
        let reg = Arc::new(Registry::default());
        let pushes = reg.register_counter(NAME_REPLICATOR_PUSHES, "p");
        let errors = reg.register_counter(NAME_REPLICATOR_PUSH_ERRORS, "e");
        for _ in 0..10 {
            pushes.inc();
        }
        for _ in 0..5 {
            errors.inc();
        }
        let dash = DashboardBuilder::new(Arc::clone(&reg)).build();
        let codes: Vec<&str> = dash.alerts.iter().map(|a| a.code.as_str()).collect();
        assert!(
            codes.contains(&"REPLICATOR_PUSH_ERROR_RATE"),
            "alerts={codes:?}"
        );
    }
}
