//! `MemStore` — pure in-memory blob storage (Gap §11).
//!
//! iroh exposes `iroh_blobs::store::mem::MemStore` which keeps
//! every blob in a `HashMap<Hash, Bytes>`. A3Net used to only have
//! [`BlobStore`](crate::store::BlobStore) (filesystem-backed). This
//! module adds an in-memory variant so test harnesses, ephemeral
//! node setups, and in-process `chat_via_gossip` style flows
//! don't need a tempdir.
//!
//! The store implements the same sync trait as [`BlobStore`] so
//! callers can call `store.has()`, `store.read_all()`, etc.
//! directly without the async layer.

use std::collections::HashMap;
use std::path::Path as StdPath;

use a3net_types::{ContentHash, RangeSpec};
use parking_lot::RwLock;

use crate::chunked::{CHUNK_SIZE, ChunkError, chunk_count_for};
use crate::metrics::{BlobMetrics, blob_metrics};

/// In-memory blob store — mirrors the sync interface of
/// [`BlobStore`](crate::store::BlobStore) so callers can swap the
/// two without changing their call pattern.
#[derive(Debug, Clone)]
pub struct MemStore {
    inner: std::sync::Arc<RwLock<HashMap<ContentHash, Vec<u8>>>>,
    /// Metrics handle. Defaults to the global singleton so existing
    /// callers (which use `MemStore::new`) continue to record into
    /// the process-global Prometheus registry.
    metrics: std::sync::Arc<BlobMetrics>,
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemStore {
    /// Build an empty store backed by the process-global metrics
    /// registry.
    pub fn new() -> Self {
        Self::with_metrics(blob_metrics())
    }

    /// Build a store that records into `metrics`. Used by tests
    /// that want to read the counters back without conflicting
    /// with other tests sharing the global registry.
    pub fn with_metrics(metrics: BlobMetrics) -> Self {
        Self {
            inner: std::sync::Arc::new(RwLock::new(HashMap::new())),
            metrics: std::sync::Arc::new(metrics),
        }
    }

    /// Number of blobs currently held.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// True if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Total bytes across every blob.
    pub fn total_bytes(&self) -> u64 {
        self.inner.read().values().map(|v| v.len() as u64).sum()
    }

    /// True if the backend has a complete local copy.
    pub fn has(&self, hash: &ContentHash) -> bool {
        self.inner.read().contains_key(hash)
    }

    /// Total bytes for the blob (after import).
    pub fn size(&self, hash: &ContentHash) -> Result<u64, ChunkError> {
        self.inner
            .read()
            .get(hash)
            .map(|v| v.len() as u64)
            .ok_or_else(|| {
                ChunkError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("blob {} not found", hash),
                ))
            })
    }

    /// Number of 16 KiB chunks.
    pub fn chunk_count(&self, hash: &ContentHash) -> Result<u32, ChunkError> {
        Ok(chunk_count_for(self.size(hash)?))
    }

    /// Read the full blob into a freshly-allocated buffer.
    pub fn read_all(&self, hash: &ContentHash) -> Result<Vec<u8>, ChunkError> {
        self.inner.read().get(hash).cloned().ok_or_else(|| {
            ChunkError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("blob {} not found", hash),
            ))
        })
    }

    /// Read a sub-range of the blob.
    pub fn read_range(&self, hash: &ContentHash, range: RangeSpec) -> Result<Vec<u8>, ChunkError> {
        let buf = self.read_all(hash)?;
        match range {
            RangeSpec::All => Ok(buf),
            RangeSpec::Single(r) => {
                let len = buf.len() as u64;
                let start = r.start.min(len);
                let end = r.end.min(len).max(start);
                Ok(buf[start as usize..end as usize].to_vec())
            }
            RangeSpec::Multi(rs) => {
                let len = buf.len() as u64;
                let mut out = Vec::new();
                for r in rs {
                    let start = r.start.min(len);
                    let end = r.end.min(len).max(start);
                    out.extend_from_slice(&buf[start as usize..end as usize]);
                }
                Ok(out)
            }
        }
    }

    /// Read a single 16 KiB chunk by index (0-based).
    pub fn read_chunk(
        &self,
        hash: &ContentHash,
        index: u32,
    ) -> Result<Option<Vec<u8>>, ChunkError> {
        let buf = match self.inner.read().get(hash) {
            Some(v) => v.clone(),
            None => return Ok(None),
        };
        let total = buf.len() as u64;
        let nchunks = chunk_count_for(total);
        if index >= nchunks {
            return Ok(None);
        }
        // The blob is guaranteed complete (memstore invariant), so
        // every chunk up to `nchunks - 1` is present.
        let start = index as usize * CHUNK_SIZE;
        let end = (start + CHUNK_SIZE).min(buf.len());
        Ok(Some(buf[start..end].to_vec()))
    }

    /// Export the blob to a destination file.
pub fn export_to_file(&self, hash: &ContentHash, dest: &StdPath) -> Result<u64, ChunkError> {
    let result = (|| -> Result<u64, ChunkError> {
        let buf = self.read_all(hash)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ChunkError::Io(std::io::Error::other(e.to_string())))?;
        }
        std::fs::write(dest, &buf)
            .map_err(|e| ChunkError::Io(std::io::Error::other(e.to_string())))?;
        Ok(buf.len() as u64)
    })();
    match result {
        Ok(n) => {
            self.metrics.exports.inc();
            Ok(n)
        }
        Err(e) => {
            self.metrics.import_errors.inc();
            Err(e)
        }
    }
}

/// Store `bytes` as a single-chunk blob, returning the hash.
pub fn put_bytes(&self, bytes: &[u8]) -> std::io::Result<ContentHash> {
    let hash = ContentHash::from_bytes(bytes);
    let mut guard = self.inner.write();
    let was_new = guard.insert(hash.clone(), bytes.to_vec()).is_none();
    if was_new {
        self.metrics.put_bytes.inc();
        // Update gauges for the fresh blob. Loop because
        // `Gauge` has no `inc_by`.
        self.metrics.blobs_total.inc();
        for _ in 0..bytes.len() {
            self.metrics.store_size_bytes.inc();
        }
    }
    Ok(hash)
}
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_then_read_round_trips() {
        let store = MemStore::new();
        let bytes = b"hello, memstore";
        let hash = store.put_bytes(bytes).unwrap();
        assert!(store.has(&hash));
        assert_eq!(store.size(&hash).unwrap(), bytes.len() as u64);
        assert_eq!(store.read_all(&hash).unwrap(), bytes.as_slice());
    }

    #[tokio::test]
    async fn chunk_boundary_16k() {
        let store = MemStore::new();
        // Build 49152 bytes by repeating a 16-byte pattern (no u8 overflow).
        let pattern = b"0123456789ABCDEF";
        let bytes: Vec<u8> = pattern.iter().copied().cycle().take(3 * 16384).collect();
        assert_eq!(bytes.len(), 3 * 16384);
        let hash = store.put_bytes(&bytes).unwrap();
        let nchunks = store.chunk_count(&hash).unwrap();
        assert_eq!(nchunks, 3u32);
        // Index 0 → first 16384 bytes
        let c0 = store.read_chunk(&hash, 0).unwrap().unwrap();
        assert_eq!(c0.len(), 16384);
        // Index 1 → bytes 16384..32768
        let c1 = store.read_chunk(&hash, 1).unwrap().unwrap();
        assert_eq!(c1.len(), 16384);
        // Index 2 → bytes 32768..49152
        let c2 = store.read_chunk(&hash, 2).unwrap().unwrap();
        assert_eq!(c2.len(), 16384);
        // Out-of-range → None
        assert!(store.read_chunk(&hash, 3).unwrap().is_none());
    }

    #[tokio::test]
    async fn range_clamped_to_blob_size() {
        let store = MemStore::new();
        // vec![val; count] is the canonical way to build an
        // all-same-byte buffer in Rust — avoid iterator overflow
        // issues with `.cycle()`.
        let bytes = vec![0xAAu8; 100];
        let hash = store.put_bytes(&bytes).unwrap();
        let r = store
            .read_range(&hash, RangeSpec::single(10, 500).unwrap())
            .unwrap();
        assert_eq!(r.len(), 90);
        assert!(r.iter().all(|&b| b == 0xAA));
    }

    #[tokio::test]
    async fn export_writes_file() {
        let store = MemStore::new();
        let bytes: Vec<u8> = b"export-test".to_vec();
        let hash = store.put_bytes(&bytes).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("blob");
        let n = store.export_to_file(&hash, &dest).unwrap();
        assert_eq!(n, bytes.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    }

    #[tokio::test]
    async fn not_found_is_io_error() {
        let store = MemStore::new();
        let bogus = ContentHash::from_bytes(b"never-seen");
        assert!(!store.has(&bogus));
        let err = store.size(&bogus).unwrap_err();
        let io_err = match err {
            ChunkError::Io(e) => e,
            other => panic!("expected Io, got {other:?}"),
        };
        assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn total_bytes_and_len() {
        let store = MemStore::new();
        assert!(store.is_empty());
        let _h1 = store.put_bytes(b"a").unwrap();
        let _h2 = store.put_bytes(b"bcdef").unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.total_bytes(), 6);
    }
}
