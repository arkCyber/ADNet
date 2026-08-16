//! Blob-layer metrics — Counter / Gauge primitives backed
//! by the global `a3net-observability` registry.
//!
//! All metrics are **registered eagerly** on first access via
//! the `blob_metrics()` static helper. The exporter picks
//! them up automatically; no wiring is needed at the call site.
//!
//! ## Metric names
//!
//! All names are prefixed with `a3net_blob_*` so the
//! Prometheus output is namespaced.
//!
//! ## Cardinality
//!
//! Low-cardinality by design. The only label is
//! `{outcome="ok"|"err"}` on the import counter to show
//! both success and failure paths; no per-hash labels.

use std::sync::Arc;

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::{GLOBAL as OBSERVABILITY_GLOBAL, Registry};
use once_cell::sync::Lazy;

/// Per-layer metric handle. Constructed once per process via
/// [`BlobMetrics::get`]. The fields are `Arc`-shared so
/// cloning the handle is cheap.
#[derive(Debug, Clone)]
pub struct BlobMetrics {
    /// `a3net_blob_imports_total` — total successful
    /// `import_file_sync` calls.
    pub imports: Arc<Counter>,
    /// `a3net_blob_import_errors_total` — total blob-layer
    /// operations that returned an `Err` (covers imports,
    /// `put_bytes_sync`, `export_to_file_sync`, and refusal
    /// to `remove` an incomplete blob).
    pub import_errors: Arc<Counter>,
    /// `a3net_blob_put_bytes_total` — total successful
    /// `put_bytes_sync` calls.
    pub put_bytes: Arc<Counter>,
    /// `a3net_blob_exports_total` — total
    /// `export_to_file_sync` calls.
    pub exports: Arc<Counter>,
    /// `a3net_blob_removes_total` — total `remove` calls
    /// that returned `Ok(true)`.
    pub removes: Arc<Counter>,
    /// `a3net_blob_store_size_bytes` — total bytes used
    /// by all complete blobs, as reported by
    /// `BlobStore::total_size`. This is a gauge (current
    /// state, not cumulative).
    pub store_size_bytes: Arc<Gauge>,
    /// `a3net_blob_blobs_total` — count of complete blobs
    /// in the store.
    pub blobs_total: Arc<Gauge>,
    /// `a3net_blob_read_hash_mismatch_total` — total read
    /// attempts that failed integrity verification (per
    /// SR-1). Each mismatch triggers a quarantine action.
    /// (DO-178C DAL-B SR-4.)
    pub read_hash_mismatch: Arc<Counter>,
    /// `a3net_blob_remove_rejected_total` — total `remove`
    /// attempts that were refused (partial or corrupt blob).
    /// (SR-2.)
    pub remove_rejected: Arc<Counter>,
    /// `a3net_blob_quarantined_total` — total blobs moved
    /// to `.quarantine/` after integrity failure.
    /// (SR-4.)
    pub quarantined: Arc<Counter>,
    /// `a3net_blob_bytes_deleted_total` — cumulative bytes
    /// removed via `remove_verified`. Counter (not gauge)
    /// so a long-running node can graph deletion churn.
    pub bytes_deleted: Arc<Counter>,
    /// `a3net_blob_import_path_rejected_total` — total
    /// imports that were refused at the path-safety layer
    /// (SR-5).
    pub import_path_rejected: Arc<Counter>,
}

impl BlobMetrics {
    /// Register every metric into `registry`. Idempotent.
    pub fn register(registry: &Registry) -> Self {
        Self {
            imports: registry.register_counter(
                "a3net_blob_imports_total",
                "Total successful blob imports (import_file_sync).",
            ),
            import_errors: registry.register_counter(
                "a3net_blob_import_errors_total",
                "Total blob-layer operations that returned an error (import, put, export, remove).",
            ),
            put_bytes: registry
                .register_counter("a3net_blob_put_bytes_total", "Total put_bytes_sync calls."),
            exports: registry.register_counter(
                "a3net_blob_exports_total",
                "Total export_to_file_sync calls.",
            ),
            removes: registry
                .register_counter("a3net_blob_removes_total", "Total successful blob removes."),
            remove_rejected: registry.register_counter(
                "a3net_blob_remove_rejected_total",
                "Total remove attempts refused (partial or corrupt blob).",
            ),
            quarantined: registry.register_counter(
                "a3net_blob_quarantined_total",
                "Total blobs moved to .quarantine/ after integrity failure.",
            ),
            bytes_deleted: registry.register_counter(
                "a3net_blob_bytes_deleted_total",
                "Cumulative bytes removed via remove_verified.",
            ),
            import_path_rejected: registry.register_counter(
                "a3net_blob_import_path_rejected_total",
                "Total imports refused at the path-safety layer.",
            ),
            read_hash_mismatch: registry.register_counter(
                "a3net_blob_read_hash_mismatch_total",
                "Total reads where the on-disk content hash did not match the requested hash.",
            ),
            store_size_bytes: registry.register_gauge(
                "a3net_blob_store_size_bytes",
                "Total bytes used by all complete blobs (current state).",
            ),
            blobs_total: registry.register_gauge(
                "a3net_blob_blobs_total",
                "Count of complete blobs in the store.",
            ),
        }
    }

    /// Process-global handle. First access registers the
    /// metrics into the global registry via the module-level
    /// `GLOBAL` static; subsequent access returns a clone.
    pub fn get() -> Self {
        GLOBAL.clone()
    }
}

/// Module-level lazy global handle.
static GLOBAL: Lazy<BlobMetrics> = Lazy::new(|| BlobMetrics::register(&OBSERVABILITY_GLOBAL));

/// Convenience function to get the global blob metrics.
pub fn blob_metrics() -> BlobMetrics {
    BlobMetrics::get()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn for_tests() -> (BlobMetrics, std::sync::Arc<Registry>) {
        let registry = std::sync::Arc::new(Registry::default());
        (BlobMetrics::register(&registry), registry)
    }

    #[test]
    fn register_creates_all_metrics() {
        let (m, registry) = for_tests();
        let snap = registry.snapshot();
        let names: Vec<String> = snap.metrics.iter().map(|x| x.name().to_string()).collect();
        assert!(names.contains(&"a3net_blob_imports_total".to_string()));
        assert!(names.contains(&"a3net_blob_import_errors_total".to_string()));
        assert!(names.contains(&"a3net_blob_put_bytes_total".to_string()));
        assert!(names.contains(&"a3net_blob_exports_total".to_string()));
        assert!(names.contains(&"a3net_blob_removes_total".to_string()));
        assert!(names.contains(&"a3net_blob_store_size_bytes".to_string()));
        assert!(names.contains(&"a3net_blob_blobs_total".to_string()));
        // DO-178C DAL-B metrics — SR-1..SR-5.
        assert!(names.contains(&"a3net_blob_read_hash_mismatch_total".to_string()));
        assert!(names.contains(&"a3net_blob_remove_rejected_total".to_string()));
        assert!(names.contains(&"a3net_blob_quarantined_total".to_string()));
        assert!(names.contains(&"a3net_blob_bytes_deleted_total".to_string()));
        assert!(names.contains(&"a3net_blob_import_path_rejected_total".to_string()));
        m.imports.inc_by(2);
        m.import_errors.inc();
        m.put_bytes.inc();
        m.exports.inc_by(3);
        m.removes.inc();
        m.read_hash_mismatch.inc();
        m.remove_rejected.inc();
        m.quarantined.inc();
        m.bytes_deleted.inc_by(4096);
        m.import_path_rejected.inc();
        assert_eq!(m.imports.get(), 2);
        assert_eq!(m.import_errors.get(), 1);
        assert_eq!(m.put_bytes.get(), 1);
        assert_eq!(m.exports.get(), 3);
        assert_eq!(m.removes.get(), 1);
        assert_eq!(m.read_hash_mismatch.get(), 1);
        assert_eq!(m.remove_rejected.get(), 1);
        assert_eq!(m.quarantined.get(), 1);
        assert_eq!(m.bytes_deleted.get(), 4096);
        assert_eq!(m.import_path_rejected.get(), 1);
    }

    #[test]
    fn get_is_idempotent() {
        let a = BlobMetrics::get();
        let b = BlobMetrics::get();
        a.imports.inc();
        assert!(b.imports.get() >= 1);
    }
}
