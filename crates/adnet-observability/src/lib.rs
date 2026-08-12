//! `adnet-observability` — Counter / Gauge / Histogram metrics for ADNet.
//!
//! This crate is the **PR1** deliverable of the observability gap
//! analysis (see `AUDIT_RECENT_CODE_V7.md` §3 "诊断 / 观测 50%"). It
//! introduces:
//!
//! - Three metric primitives ([`Counter`], [`Gauge`], [`Histogram`])
//!   backed by `std::sync::atomic` for hot-path increments and
//!   `parking_lot::RwLock` for slow-path label set bookkeeping.
//! - A global [`registry::Registry`] keyed by metric name.
//! - A [`prometheus::PrometheusExporter`] that renders the registry
//!   to the standard Prometheus text exposition format
//!   (`text/plain; version=0.0.4`).
//!
//! ## Design constraints
//!
//! 1. **No external metrics crate**. We deliberately do not depend
//!    on `prometheus` / `metrics` / `OpenTelemetry` — they are
//!    large and pull in a runtime/SDK we don't need. The text
//!    format is small enough to hand-roll and the API surface
//!    exactly fits ADNet's three primitive types.
//! 2. **No `unsafe`**. Same rule as the rest of the workspace
//!    (`#![forbid(unsafe_code)]`).
//! 3. **Recover from lock poisoning**. A writer panic in a metrics
//!    path must never crash the process; the same `recover_lock`
//!    convention used in
//!    `adnet-transport/src/iroh/discovery/diagnostics.rs` is
//!    applied here.
//! 4. **No new dependencies on iroh / quinn / heavy runtime
//!    crates**. This crate must compile in the *default* build
//!    (no `--features iroh`).
//!
//! ## Layer-specific metrics
//!
//! Each layer's `*Metrics` struct (transport / gossip / blobs /
//! docs / chat / relay / billing / discovery) is **not** in this
//! PR. They are follow-up PRs (PR3+). This PR lays the foundation.
//!
//! ## Quick start
//!
//! ```no_run
//! use adnet_observability::registry::Registry;
//! use adnet_observability::prometheus::PrometheusExporter;
//!
//! let registry = Registry::default();
//! let counter = registry.register_counter(
//!     "adnet_demo_requests_total",
//!     "Total demo requests received",
//! );
//! counter.inc();
//! counter.inc_by(2);
//!
//! let exporter = PrometheusExporter::new(&registry);
//! let output = exporter.render();
//! assert!(output.text().contains("adnet_demo_requests_total 3"));
//! ```
//!
//! [`Counter`]: crate::metrics::Counter
//! [`Gauge`]: crate::metrics::Gauge
//! [`Histogram`]: crate::metrics::Histogram

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod bridge;
pub mod dashboard;
pub mod health;
pub mod histogram;
pub mod labels;
pub mod metrics;
pub mod prometheus;
pub mod registry;

#[cfg(feature = "http-server")]
pub mod http;

/// Convenience re-exports. Most callers want
/// `use adnet_observability::prelude::*;` and not the full module
/// tree.
pub mod prelude {
    pub use crate::bridge::{
        DISCOVERY, DiagnosticsSource, DiscoveryMetrics, ENDPOINT, EndpointMetrics,
        update_endpoint_from_source, update_endpoint_from_source_into, update_from_source,
        update_from_source_into,
    };
    pub use crate::dashboard::{
        Alert, AlertLevel, AlertThresholds, BlobStoreSection, Dashboard, DashboardBuilder,
        ReplicationSection, ShareSection, StorageSection,
        NAME_BLOBSTORE_BLOBS, NAME_BLOBSTORE_IMPORT_PATH_REJECTED, NAME_BLOBSTORE_QUARANTINED,
        NAME_BLOBSTORE_READ_HASH_MISMATCH, NAME_BLOBSTORE_SIZE, NAME_REPLICATOR_FULL,
        NAME_REPLICATOR_HASHES, NAME_REPLICATOR_PUSH_ERRORS, NAME_REPLICATOR_PUSHES,
        NAME_REPLICATOR_SWEEPS, NAME_REPLICATOR_UNDER, NAME_SHARE_RECEIVE_BYTES,
        NAME_SHARE_RECEIVE_FILES, NAME_SHARE_RECEIVE_ERRORS,
    };
    pub use crate::histogram::{DEFAULT_BUCKETS, Histogram, HistogramSnapshot};
    pub use crate::labels::{Label, LabelSet};
    pub use crate::metrics::{Counter, Gauge, MetricKind};
    pub use crate::prometheus::{PrometheusExporter, PrometheusOutput};
    pub use crate::registry::{GLOBAL, Registry, RegistrySnapshot};
    #[cfg(feature = "http-server")]
    pub use crate::http::{MetricsServer, MetricsServerConfig, health_handler, metrics_handler};
    #[cfg(feature = "http-server")]
    pub use crate::health::{
        CheckResult, HealthCheck, HealthCheckError, HealthStatus,
        clear_health_checks, register_health_check, run_checks,
    };
}
