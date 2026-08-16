//! Bandwidth Metrics and Monitoring
//!
//! Provides metrics collection for bandwidth usage monitoring and alerting.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::Registry;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::policy::BandwidthDirection;

/// Bandwidth metrics for observability.
pub struct BandwidthMetrics {
    /// Total bytes uploaded.
    pub total_bytes_uploaded: Arc<Counter>,
    /// Total bytes downloaded.
    pub total_bytes_downloaded: Arc<Counter>,

    /// Current upload rate (bytes/sec).
    pub current_upload_rate: Arc<Gauge>,
    /// Current download rate (bytes/sec).
    pub current_download_rate: Arc<Gauge>,

    /// Peak upload rate (bytes/sec).
    pub peak_upload_rate: Arc<Gauge>,
    /// Peak download rate (bytes/sec).
    pub peak_download_rate: Arc<Gauge>,

    /// Number of active uploads.
    pub active_uploads: Arc<Gauge>,
    /// Number of active downloads.
    pub active_downloads: Arc<Gauge>,

    /// Bandwidth permits acquired.
    pub permits_acquired: Arc<Counter>,
    /// Bandwidth permits denied.
    pub permits_denied: Arc<Counter>,

    /// Per-tenant metrics.
    per_tenant: Arc<RwLock<HashMap<String, TenantMetrics>>>,

    /// Last rate calculation.
    last_rate_calc: RwLock<Instant>,
}

// TenantMetrics doesn't implement Debug because Counter/Gauge don't
#[allow(missing_debug_implementations)]
#[derive(Debug)]
struct TenantMetrics {
    upload_bytes: Arc<Counter>,
    download_bytes: Arc<Counter>,
    upload_rate: Arc<Gauge>,
    download_rate: Arc<Gauge>,
}

impl Default for TenantMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl TenantMetrics {
    fn new() -> Self {
        Self {
            upload_bytes: Arc::new(Counter::new(
                "bandwidth_per_tenant_upload",
                "uploaded bytes per tenant",
            )),
            download_bytes: Arc::new(Counter::new(
                "bandwidth_per_tenant_download",
                "downloaded bytes per tenant",
            )),
            upload_rate: Arc::new(Gauge::new(
                "bandwidth_per_tenant_upload_rate",
                "upload rate per tenant",
            )),
            download_rate: Arc::new(Gauge::new(
                "bandwidth_per_tenant_download_rate",
                "download rate per tenant",
            )),
        }
    }
}

impl BandwidthMetrics {
    /// Create a new metrics instance (doesn't register with global registry).
    pub fn new() -> Self {
        let reg = Registry::default();
        Self::register(&reg)
    }

    /// Register metrics with a registry.
    pub fn register(registry: &Registry) -> Self {
        Self {
            total_bytes_uploaded: registry.register_counter("bandwidth", "bytes_uploaded_total"),
            total_bytes_downloaded: registry
                .register_counter("bandwidth", "bytes_downloaded_total"),
            current_upload_rate: registry.register_gauge("bandwidth", "upload_rate_bps"),
            current_download_rate: registry.register_gauge("bandwidth", "download_rate_bps"),
            peak_upload_rate: registry.register_gauge("bandwidth", "peak_upload_rate_bps"),
            peak_download_rate: registry.register_gauge("bandwidth", "peak_download_rate_bps"),
            active_uploads: registry.register_gauge("bandwidth", "active_uploads"),
            active_downloads: registry.register_gauge("bandwidth", "active_downloads"),
            permits_acquired: registry.register_counter("bandwidth", "permits_acquired"),
            permits_denied: registry.register_counter("bandwidth", "permits_denied"),
            per_tenant: Arc::new(RwLock::new(HashMap::new())),
            last_rate_calc: RwLock::new(Instant::now()),
        }
    }

    /// Record bytes transferred.
    pub fn record_transfer(&self, tenant_id: &str, direction: BandwidthDirection, bytes: u64) {
        match direction {
            BandwidthDirection::Upload => {
                self.total_bytes_uploaded.inc_by(bytes);
            }
            BandwidthDirection::Download => {
                self.total_bytes_downloaded.inc_by(bytes);
            }
        }

        // Per-tenant metrics
        let mut per_tenant = self.per_tenant.write();
        let metrics = per_tenant
            .entry(tenant_id.to_string())
            .or_insert_with(TenantMetrics::new);

        match direction {
            BandwidthDirection::Upload => {
                metrics.upload_bytes.inc_by(bytes);
            }
            BandwidthDirection::Download => {
                metrics.download_bytes.inc_by(bytes);
            }
        }
    }

    /// Record permit acquisition.
    pub fn record_permit_acquired(&self, _tenant_id: &str, _direction: BandwidthDirection) {
        self.permits_acquired.inc();
    }

    /// Record permit denial.
    pub fn record_permit_denied(
        &self,
        _tenant_id: &str,
        _direction: BandwidthDirection,
        _reason: &str,
    ) {
        self.permits_denied.inc();
    }

    /// Update rate calculations.
    ///
    /// Compares current rates with peak rates and updates peaks if exceeded.
    /// Note: Gauge values are stored in micro-units, so we need to convert.
    pub fn update_rates(&self, upload_bps: u64, download_bps: u64) {
        self.current_upload_rate.set_f64(upload_bps as f64);
        self.current_download_rate.set_f64(download_bps as f64);

        // Update peaks - get() returns micro-units, compare with bps * 1_000_000
        let current_peak_upload = self.peak_upload_rate.get();
        if upload_bps as f64 * 1_000_000.0 > current_peak_upload as f64 {
            self.peak_upload_rate.set_f64(upload_bps as f64);
        }

        let current_peak_download = self.peak_download_rate.get();
        if download_bps as f64 * 1_000_000.0 > current_peak_download as f64 {
            self.peak_download_rate.set_f64(download_bps as f64);
        }

        *self.last_rate_calc.write() = Instant::now();
    }

    /// Update active transfer counts.
    pub fn update_active(&self, uploads: usize, downloads: usize) {
        self.active_uploads.set(uploads as i64);
        self.active_downloads.set(downloads as i64);
    }

    /// Get summary for a tenant.
    pub fn get_tenant_summary(&self, tenant_id: &str) -> Option<TenantMetricsSummary> {
        let per_tenant = self.per_tenant.read();
        per_tenant.get(tenant_id).map(|m| TenantMetricsSummary {
            upload_bytes: m.upload_bytes.get(),
            download_bytes: m.download_bytes.get(),
            upload_rate_bps: m.upload_rate.get() as u64,
            download_rate_bps: m.download_rate.get() as u64,
        })
    }

    /// Get all tenant summaries.
    pub fn get_all_tenant_summaries(&self) -> HashMap<String, TenantMetricsSummary> {
        let per_tenant = self.per_tenant.read();
        per_tenant
            .iter()
            .map(|(id, m)| {
                (
                    id.clone(),
                    TenantMetricsSummary {
                        upload_bytes: m.upload_bytes.get(),
                        download_bytes: m.download_bytes.get(),
                        upload_rate_bps: m.upload_rate.get() as u64,
                        download_rate_bps: m.download_rate.get() as u64,
                    },
                )
            })
            .collect()
    }
}

impl Default for BandwidthMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of metrics for a tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantMetricsSummary {
    pub upload_bytes: u64,
    pub download_bytes: u64,
    pub upload_rate_bps: u64,
    pub download_rate_bps: u64,
}

/// Summary of global bandwidth metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalMetricsSummary {
    pub total_bytes_uploaded: u64,
    pub total_bytes_downloaded: u64,
    pub current_upload_rate_bps: u64,
    pub current_download_rate_bps: u64,
    pub peak_upload_rate_bps: u64,
    pub peak_download_rate_bps: u64,
    pub active_uploads: usize,
    pub active_downloads: usize,
    pub permits_acquired: u64,
    pub permits_denied: u64,
}
