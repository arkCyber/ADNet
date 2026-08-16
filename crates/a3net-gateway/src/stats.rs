//! Stats API for IPFS statistics.
//!
//! This module provides IPFS statistics endpoints including:
//! - Repository statistics
//! - Bandwidth statistics
//! - DHT statistics
//! - Block statistics

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use a3net_blobstore::BlobStore;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Repository statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoStats {
    #[serde(rename = "RepoSize")]
    pub repo_size: u64,
    #[serde(rename = "StorageMax")]
    pub storage_max: u64,
    #[serde(rename = "NumObjects")]
    pub num_objects: u64,
    #[serde(rename = "RepoPath")]
    pub repo_path: String,
    #[serde(rename = "Version")]
    pub version: String,
}

/// Bandwidth statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandwidthStats {
    #[serde(rename = "TotalIn")]
    pub total_in: u64,
    #[serde(rename = "TotalOut")]
    pub total_out: u64,
    #[serde(rename = "RateIn")]
    pub rate_in: f64,
    #[serde(rename = "RateOut")]
    pub rate_out: f64,
}

/// DHT statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtStats {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "NumPeers")]
    pub num_peers: u32,
    #[serde(rename = "SuccessRatio")]
    pub success_ratio: f64,
}

/// Bandwidth rate tracker using exponential moving average.
#[derive(Debug, Clone, Default)]
pub struct BandwidthRate {
    bytes: u64,
    samples: u64,
    rate: f64,
}

impl BandwidthRate {
    /// Record bytes transferred.
    pub fn record(&mut self, bytes: u64) {
        self.bytes += bytes;
        self.samples += 1;
        // Simple rate calculation: bytes per sample
        if self.samples > 0 {
            self.rate = self.bytes as f64 / self.samples as f64;
        }
    }

    /// Get current rate.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Reset counters.
    pub fn reset(&mut self) {
        self.bytes = 0;
        self.samples = 0;
        self.rate = 0.0;
    }
}

/// Stats service using atomic counters for thread-safe statistics.
#[derive(Clone)]
pub struct StatsService {
    blob_store: Arc<BlobStore>,
    repo_path: String,
    storage_max: u64,
    /// Total bytes received (cumulative).
    bytes_in: Arc<AtomicU64>,
    /// Total bytes sent (cumulative).
    bytes_out: Arc<AtomicU64>,
    /// Input bandwidth rate tracker.
    rate_in: Arc<RwLock<BandwidthRate>>,
    /// Output bandwidth rate tracker.
    rate_out: Arc<RwLock<BandwidthRate>>,
    /// Per-endpoint statistics.
    endpoint_stats: Arc<RwLock<HashMap<String, EndpointStats>>>,
    /// Start time for uptime calculation.
    start_time: SystemTime,
}

impl StatsService {
    /// Create a new stats service.
    pub fn new(blob_store: Arc<BlobStore>, repo_path: String, storage_max: u64) -> Self {
        Self {
            blob_store,
            repo_path,
            storage_max,
            bytes_in: Arc::new(AtomicU64::new(0)),
            bytes_out: Arc::new(AtomicU64::new(0)),
            rate_in: Arc::new(RwLock::new(BandwidthRate::default())),
            rate_out: Arc::new(RwLock::new(BandwidthRate::default())),
            endpoint_stats: Arc::new(RwLock::new(HashMap::new())),
            start_time: SystemTime::now(),
        }
    }

    /// Record bytes received.
    pub fn record_in(&self, bytes: u64) {
        self.bytes_in.fetch_add(bytes, Ordering::Relaxed);
        let rate_in = self.rate_in.clone();
        tokio::spawn(async move {
            let mut rate = rate_in.write().await;
            rate.record(bytes);
        });
    }

    /// Record bytes sent.
    pub fn record_out(&self, bytes: u64) {
        self.bytes_out.fetch_add(bytes, Ordering::Relaxed);
        let rate_out = self.rate_out.clone();
        tokio::spawn(async move {
            let mut rate = rate_out.write().await;
            rate.record(bytes);
        });
    }

    /// Record an endpoint request.
    pub async fn record_request(&self, endpoint: &str, status: u16, duration_ms: u64) {
        let mut stats = self.endpoint_stats.write().await;
        let entry = stats.entry(endpoint.to_string()).or_insert_with(EndpointStats::default);
        entry.total_requests += 1;
        if (200..300).contains(&status) {
            entry.success_requests += 1;
        } else if status >= 400 {
            entry.error_requests += 1;
        }
        entry.total_duration_ms += duration_ms;
    }

    /// Get repository statistics.
    pub async fn repo(&self) -> Result<RepoStats, StatsError> {
        let repo_size = self.calculate_repo_size().await;
        let num_objects = self.count_objects().await;

        Ok(RepoStats {
            repo_size,
            storage_max: self.storage_max,
            num_objects,
            repo_path: self.repo_path.clone(),
            version: "10".to_string(),
        })
    }

    /// Get bandwidth statistics.
    pub async fn bandwidth(&self) -> Result<BandwidthStats, StatsError> {
        let total_in = self.bytes_in.load(Ordering::Relaxed);
        let total_out = self.bytes_out.load(Ordering::Relaxed);

        let rate_in = self.rate_in.read().await;
        let rate_out = self.rate_out.read().await;

        Ok(BandwidthStats {
            total_in,
            total_out,
            rate_in: rate_in.rate(),
            rate_out: rate_out.rate(),
        })
    }

    /// Get DHT statistics.
    pub fn dht(&self) -> DhtStats {
        DhtStats {
            name: "kademlia".to_string(),
            num_peers: 0,
            success_ratio: 0.0,
        }
    }

    /// Get endpoint statistics.
    pub async fn endpoint_stats(&self) -> HashMap<String, EndpointStats> {
        self.endpoint_stats.read().await.clone()
    }

    /// Get uptime in seconds.
    pub fn uptime(&self) -> Duration {
        SystemTime::now()
            .duration_since(self.start_time)
            .unwrap_or_default()
    }

    async fn calculate_repo_size(&self) -> u64 {
        self.blob_store.total_size().unwrap_or(0)
    }

    async fn count_objects(&self) -> u64 {
        self.blob_store.list_complete().map(|v| v.len() as u64).unwrap_or(0)
    }

    /// Reset bandwidth counters.
    pub async fn reset_counters(&self) {
        self.bytes_in.store(0, Ordering::Relaxed);
        self.bytes_out.store(0, Ordering::Relaxed);
        self.rate_in.write().await.reset();
        self.rate_out.write().await.reset();
    }
}

/// Per-endpoint statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointStats {
    pub total_requests: u64,
    pub success_requests: u64,
    pub error_requests: u64,
    pub total_duration_ms: u64,
}

impl EndpointStats {
    /// Get average request duration in milliseconds.
    pub fn avg_duration_ms(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        self.total_duration_ms as f64 / self.total_requests as f64
    }

    /// Get success rate.
    pub fn success_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        self.success_requests as f64 / self.total_requests as f64
    }
}

/// Stats errors.
#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error("internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bandwidth_tracking() {
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            a3net_blobstore::BlobStore::new(temp_dir.path()).unwrap()
        );
        let stats = StatsService::new(
            blob_store,
            temp_dir.path().to_string_lossy().to_string(),
            1024 * 1024 * 1024,
        );

        // Record some bandwidth
        stats.record_in(1000);
        stats.record_out(500);

        // Give time for async updates
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Check bandwidth stats
        let bw = stats.bandwidth().await.unwrap();
        assert_eq!(bw.total_in, 1000);
        assert_eq!(bw.total_out, 500);
    }

    #[tokio::test]
    async fn test_repo_stats() {
        let temp_dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            a3net_blobstore::BlobStore::new(temp_dir.path()).unwrap()
        );
        let stats = StatsService::new(
            blob_store,
            temp_dir.path().to_string_lossy().to_string(),
            1024 * 1024 * 1024,
        );

        let repo = stats.repo().await.unwrap();
        assert_eq!(repo.version, "10");
        assert_eq!(repo.storage_max, 1024 * 1024 * 1024);
    }
}
