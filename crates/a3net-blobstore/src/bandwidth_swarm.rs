//! Bandwidth-Aware Swarm Download Service
//!
//! This module wraps the SwarmDownloadService with multi-tenant bandwidth control,
//! ensuring that P2P downloads respect per-tenant bandwidth limits.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use a3net_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use super::bandwidth::{
    BandwidthDirection, BandwidthPermit, BandwidthPolicy, GlobalBandwidthLimits,
    TenantBandwidthManager, TenantBandwidthStatus, TenantId, TenantPriority,
};
use super::bao_tree::BaoTree;
use super::chunked::CHUNK_SIZE;
use super::swarm_download::{ChunkFetcher, SwarmDownloadService, SwarmError};

/// Errors specific to bandwidth-aware swarm downloads.
#[derive(Debug, Error)]
pub enum BandwidthSwarmError {
    #[error("bandwidth limit exceeded: {0}")]
    LimitExceeded(String),

    #[error("bandwidth timeout after {0:?}")]
    Timeout(Duration),

    #[error("tenant not registered: {0}")]
    TenantNotFound(String),

    #[error("bandwidth error: {0}")]
    Bandwidth(#[from] super::bandwidth::BandwidthError),

    #[error("swarm error: {0}")]
    Swarm(#[from] SwarmError),
}

/// Bandwidth-aware configuration for swarm downloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BandwidthSwarmConfig {
    /// Global bandwidth limits.
    pub global_limits: GlobalBandwidthLimits,

    /// Default download policy per tenant.
    pub default_download_policy: BandwidthPolicy,

    /// Transfer timeout when waiting for bandwidth.
    #[serde(default = "default_download_timeout")]
    pub transfer_timeout: Duration,

    /// Maximum concurrent chunk downloads per tenant.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_chunks: usize,

    /// Chunk size for bandwidth accounting.
    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,
}

fn default_download_timeout() -> Duration {
    Duration::from_secs(300)
}

fn default_max_concurrent() -> usize {
    16
}

fn default_chunk_size() -> usize {
    CHUNK_SIZE
}

impl Default for BandwidthSwarmConfig {
    fn default() -> Self {
        Self {
            global_limits: GlobalBandwidthLimits::default(),
            default_download_policy: BandwidthPolicy {
                max_upload_bps: 50 * 1024 * 1024,
                max_download_bps: 100 * 1024 * 1024,
                priority: TenantPriority::Normal,
                can_use_reserved: false,
                burst_multiplier: 1.0,
            },
            transfer_timeout: default_download_timeout(),
            max_concurrent_chunks: default_max_concurrent(),
            chunk_size: default_chunk_size(),
        }
    }
}

/// Bandwidth-aware swarm download service.
///
/// This service wraps SwarmDownloadService with bandwidth control, ensuring:
/// - Per-tenant bandwidth limits are respected
/// - Downloads are throttled according to tenant allocation
/// - Bandwidth usage is tracked per tenant
#[allow(dead_code)]
pub struct BandwidthSwarmService<F: ChunkFetcher> {
    /// The underlying swarm download service.
    inner: Arc<SwarmDownloadService<F>>,
    /// Bandwidth manager.
    bandwidth_manager: Arc<TenantBandwidthManager>,
    /// Default download policy.
    default_download_policy: BandwidthPolicy,
    /// Transfer timeout.
    transfer_timeout: Duration,
    /// Max concurrent chunks.
    max_concurrent_chunks: usize,
    /// Chunk size.
    chunk_size: usize,
    /// Active permits tracking.
    active_downloads: RwLock<HashMap<ContentHash, BandwidthPermit>>,
}

impl<F: ChunkFetcher + 'static> BandwidthSwarmService<F> {
    /// Create a new bandwidth-aware swarm service.
    pub fn new(inner: Arc<SwarmDownloadService<F>>, config: BandwidthSwarmConfig) -> Self {
        let bandwidth_manager = Arc::new(TenantBandwidthManager::new(config.global_limits.clone()));

        Self {
            inner,
            bandwidth_manager,
            default_download_policy: config.default_download_policy,
            transfer_timeout: config.transfer_timeout,
            max_concurrent_chunks: config.max_concurrent_chunks,
            chunk_size: config.chunk_size,
            active_downloads: RwLock::new(HashMap::new()),
        }
    }

    /// Create with explicit bandwidth manager.
    pub fn with_bandwidth_manager(
        inner: Arc<SwarmDownloadService<F>>,
        bandwidth_manager: Arc<TenantBandwidthManager>,
        default_download_policy: BandwidthPolicy,
        transfer_timeout: Duration,
        max_concurrent_chunks: usize,
    ) -> Self {
        Self {
            inner,
            bandwidth_manager,
            default_download_policy,
            transfer_timeout,
            max_concurrent_chunks,
            chunk_size: CHUNK_SIZE,
            active_downloads: RwLock::new(HashMap::new()),
        }
    }

    /// Get the bandwidth manager.
    pub fn bandwidth_manager(&self) -> &Arc<TenantBandwidthManager> {
        &self.bandwidth_manager
    }

    /// Register a new tenant with a specific policy.
    pub fn register_tenant(
        &self,
        tenant_id: TenantId,
        policy: BandwidthPolicy,
    ) -> super::bandwidth::BandwidthResult<()> {
        self.bandwidth_manager.add_tenant(tenant_id, policy)
    }

    /// Register a tenant using the default policy.
    pub fn register_tenant_default(
        &self,
        tenant_id: TenantId,
    ) -> super::bandwidth::BandwidthResult<()> {
        self.bandwidth_manager
            .add_tenant(tenant_id, self.default_download_policy.clone())
    }

    /// Remove a tenant.
    pub fn unregister_tenant(&self, tenant_id: &TenantId) -> super::bandwidth::BandwidthResult<()> {
        self.bandwidth_manager.remove_tenant(tenant_id)
    }

    /// Update a tenant's policy.
    pub fn update_tenant_policy(
        &self,
        tenant_id: &TenantId,
        policy: BandwidthPolicy,
    ) -> super::bandwidth::BandwidthResult<()> {
        self.bandwidth_manager.update_policy(tenant_id, policy)
    }

    /// Download content with bandwidth control.
    ///
    /// This method:
    /// 1. Acquires bandwidth permit for the estimated download size
    /// 2. Performs the swarm download through the underlying service
    /// 3. Releases the permit when complete
    pub async fn download(
        &self,
        tenant_id: &TenantId,
        content_hash: &ContentHash,
        size: u64,
        chunk_count: u32,
        peers: Vec<(String, std::collections::HashSet<u32>)>,
        bao_tree: Option<Arc<BaoTree>>,
    ) -> Result<Vec<u8>, BandwidthSwarmError> {
        let estimated_bytes = size;

        // Acquire bandwidth permit
        let permit = self
            .bandwidth_manager
            .acquire_with_timeout(
                tenant_id,
                BandwidthDirection::Download,
                estimated_bytes,
                self.transfer_timeout,
            )
            .await?;

        debug!(
            tenant_id = %tenant_id,
            content_hash = %content_hash,
            bytes = estimated_bytes,
            "bandwidth permit acquired for swarm download"
        );

        // Track the permit
        {
            let mut downloads = self.active_downloads.write();
            downloads.insert(content_hash.clone(), permit);
        }

        // Perform the download
        let inner = self.inner.clone();
        let result = inner
            .download_parallel(content_hash, size, chunk_count, peers, bao_tree)
            .await;

        // Release the permit
        {
            let mut downloads = self.active_downloads.write();
            if let Some(p) = downloads.remove(content_hash) {
                drop(p);
            }
        }

        result.map_err(BandwidthSwarmError::Swarm)
    }

    /// Download content using default tenant.
    pub async fn download_default(
        &self,
        content_hash: &ContentHash,
        size: u64,
        chunk_count: u32,
        peers: Vec<(String, std::collections::HashSet<u32>)>,
        bao_tree: Option<Arc<BaoTree>>,
    ) -> Result<Vec<u8>, BandwidthSwarmError> {
        self.download(
            &TenantId::new("default"),
            content_hash,
            size,
            chunk_count,
            peers,
            bao_tree,
        )
        .await
    }

    /// Get current bandwidth status for a tenant.
    pub fn get_tenant_status(
        &self,
        tenant_id: &TenantId,
    ) -> Result<TenantBandwidthStatus, BandwidthSwarmError> {
        Ok(self.bandwidth_manager.get_tenant_status(tenant_id)?)
    }

    /// Get global bandwidth status.
    pub fn get_global_status(&self) -> super::bandwidth::GlobalBandwidthStatus {
        self.bandwidth_manager.get_global_status()
    }

    /// Get all registered tenants.
    pub fn list_tenants(&self) -> Vec<TenantId> {
        self.bandwidth_manager.list_tenants()
    }

    /// Get active download count.
    pub fn active_download_count(&self) -> usize {
        self.active_downloads.read().len()
    }
}

/// Predefined bandwidth profiles for swarm downloads.
pub mod profiles {
    use super::*;

    /// Family user profile.
    pub fn family_profile() -> BandwidthSwarmConfig {
        BandwidthSwarmConfig {
            global_limits: GlobalBandwidthLimits::new(
                10 * 1024 * 1024,
                50 * 1024 * 1024,
                5 * 1024 * 1024,
            ),
            default_download_policy: BandwidthPolicy::new(5 * 1024 * 1024, 20 * 1024 * 1024)
                .with_priority(TenantPriority::Normal),
            ..Default::default()
        }
    }

    /// Enterprise profile.
    pub fn enterprise_profile() -> BandwidthSwarmConfig {
        BandwidthSwarmConfig {
            global_limits: GlobalBandwidthLimits::new(
                50 * 1024 * 1024,
                200 * 1024 * 1024,
                25 * 1024 * 1024,
            ),
            default_download_policy: BandwidthPolicy::new(20 * 1024 * 1024, 100 * 1024 * 1024)
                .with_priority(TenantPriority::Normal),
            ..Default::default()
        }
    }

    /// Guest profile with strict limits.
    pub fn guest_profile() -> BandwidthSwarmConfig {
        BandwidthSwarmConfig {
            global_limits: GlobalBandwidthLimits::new(
                2 * 1024 * 1024,
                10 * 1024 * 1024,
                1 * 1024 * 1024,
            ),
            default_download_policy: BandwidthPolicy::new(256 * 1024, 1 * 1024 * 1024)
                .with_priority(TenantPriority::Low),
            max_concurrent_chunks: 4,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BandwidthSwarmConfig::default();
        assert!(config.global_limits.max_download_bps > 0);
        assert!(config.chunk_size > 0);
    }

    #[test]
    fn test_family_profile() {
        let config = profiles::family_profile();
        assert!(config.global_limits.max_download_bps < 100 * 1024 * 1024);
    }
}
