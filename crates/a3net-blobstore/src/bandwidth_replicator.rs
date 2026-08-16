//! Bandwidth-Aware Replicator Service
//!
//! This module provides multi-tenant bandwidth control for replication operations.

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

/// Errors specific to bandwidth-aware replication.
#[derive(Debug, Error)]
pub enum BandwidthReplicatorError {
    #[error("bandwidth limit exceeded: {0}")]
    LimitExceeded(String),

    #[error("bandwidth timeout after {0:?}")]
    Timeout(Duration),

    #[error("tenant not registered: {0}")]
    TenantNotFound(String),

    #[error("bandwidth error: {0}")]
    Bandwidth(#[from] super::bandwidth::BandwidthError),
}

/// Bandwidth-aware configuration for replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BandwidthReplicatorConfig {
    pub global_limits: GlobalBandwidthLimits,
    pub default_replication_policy: BandwidthPolicy,
    #[serde(default = "default_replication_timeout")]
    pub transfer_timeout: Duration,
    #[serde(default = "default_max_concurrent_pushes")]
    pub max_concurrent_pushes: usize,
}

fn default_replication_timeout() -> Duration {
    Duration::from_secs(600)
}

fn default_max_concurrent_pushes() -> usize {
    4
}

impl Default for BandwidthReplicatorConfig {
    fn default() -> Self {
        Self {
            global_limits: GlobalBandwidthLimits::default(),
            default_replication_policy: BandwidthPolicy {
                max_upload_bps: 10 * 1024 * 1024,
                max_download_bps: 0,
                priority: TenantPriority::Normal,
                can_use_reserved: false,
                burst_multiplier: 1.0,
            },
            transfer_timeout: default_replication_timeout(),
            max_concurrent_pushes: default_max_concurrent_pushes(),
        }
    }
}

/// Bandwidth-aware replicator service.
pub struct BandwidthReplicatorService {
    bandwidth_manager: Arc<TenantBandwidthManager>,
    default_replication_policy: BandwidthPolicy,
    transfer_timeout: Duration,
    max_concurrent_pushes: usize,
    active_pushes: RwLock<HashMap<ContentHash, BandwidthPermit>>,
}

impl BandwidthReplicatorService {
    /// Create a new service.
    pub fn new(config: BandwidthReplicatorConfig) -> Self {
        Self {
            bandwidth_manager: Arc::new(TenantBandwidthManager::new(config.global_limits.clone())),
            default_replication_policy: config.default_replication_policy,
            transfer_timeout: config.transfer_timeout,
            max_concurrent_pushes: config.max_concurrent_pushes,
            active_pushes: RwLock::new(HashMap::new()),
        }
    }

    /// Get the bandwidth manager.
    pub fn bandwidth_manager(&self) -> &Arc<TenantBandwidthManager> {
        &self.bandwidth_manager
    }

    /// Register a tenant.
    pub fn register_tenant(
        &self,
        tenant_id: TenantId,
        policy: BandwidthPolicy,
    ) -> super::bandwidth::BandwidthResult<()> {
        self.bandwidth_manager.add_tenant(tenant_id, policy)
    }

    /// Register a tenant using default policy.
    pub fn register_tenant_default(
        &self,
        tenant_id: TenantId,
    ) -> super::bandwidth::BandwidthResult<()> {
        self.bandwidth_manager
            .add_tenant(tenant_id, self.default_replication_policy.clone())
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

    /// Acquire bandwidth for a push.
    pub async fn acquire_push_bandwidth(
        &self,
        tenant_id: &TenantId,
        block_size: u64,
    ) -> Result<BandwidthPermit, BandwidthReplicatorError> {
        let permit = self
            .bandwidth_manager
            .acquire_with_timeout(
                tenant_id,
                BandwidthDirection::Upload,
                block_size,
                self.transfer_timeout,
            )
            .await?;

        debug!(tenant_id = %tenant_id, block_size = block_size, "bandwidth permit acquired for replication push");
        Ok(permit)
    }

    /// Release push bandwidth.
    pub fn release_push_bandwidth(&self, _tenant_id: &TenantId, permit: BandwidthPermit) {
        drop(permit);
    }

    /// Check if we can push.
    pub fn can_push(&self) -> bool {
        self.active_pushes.read().len() < self.max_concurrent_pushes
    }

    /// Track an active push.
    pub fn track_push(&self, content_hash: ContentHash, permit: BandwidthPermit) {
        self.active_pushes.write().insert(content_hash, permit);
    }

    /// Untrack a push.
    pub fn untrack_push(&self, content_hash: &ContentHash) {
        self.active_pushes.write().remove(content_hash);
    }

    /// Get active push count.
    pub fn active_push_count(&self) -> usize {
        self.active_pushes.read().len()
    }

    /// Get tenant status.
    pub fn get_tenant_status(
        &self,
        tenant_id: &TenantId,
    ) -> Result<TenantBandwidthStatus, BandwidthReplicatorError> {
        Ok(self.bandwidth_manager.get_tenant_status(tenant_id)?)
    }

    /// Get global status.
    pub fn get_global_status(&self) -> super::bandwidth::GlobalBandwidthStatus {
        self.bandwidth_manager.get_global_status()
    }

    /// List tenants.
    pub fn list_tenants(&self) -> Vec<TenantId> {
        self.bandwidth_manager.list_tenants()
    }
}

/// Predefined profiles.
pub mod profiles {
    use super::*;

    /// Family profile.
    pub fn family_profile() -> BandwidthReplicatorConfig {
        BandwidthReplicatorConfig {
            global_limits: GlobalBandwidthLimits::new(5 * 1024 * 1024, 0, 2 * 1024 * 1024),
            default_replication_policy: BandwidthPolicy::new(1 * 1024 * 1024, 0)
                .with_priority(TenantPriority::Normal),
            max_concurrent_pushes: 2,
            ..Default::default()
        }
    }

    /// Enterprise profile.
    pub fn enterprise_profile() -> BandwidthReplicatorConfig {
        BandwidthReplicatorConfig {
            global_limits: GlobalBandwidthLimits::new(50 * 1024 * 1024, 0, 10 * 1024 * 1024),
            default_replication_policy: BandwidthPolicy::new(10 * 1024 * 1024, 0)
                .with_priority(TenantPriority::Normal),
            max_concurrent_pushes: 8,
            ..Default::default()
        }
    }

    /// Guest profile.
    pub fn guest_profile() -> BandwidthReplicatorConfig {
        BandwidthReplicatorConfig {
            global_limits: GlobalBandwidthLimits::new(512 * 1024, 0, 256 * 1024),
            default_replication_policy: BandwidthPolicy::new(128 * 1024, 0)
                .with_priority(TenantPriority::Low),
            max_concurrent_pushes: 1,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BandwidthReplicatorConfig::default();
        assert!(config.global_limits.max_upload_bps > 0);
    }

    #[tokio::test]
    async fn test_can_push() {
        let service = BandwidthReplicatorService::new(BandwidthReplicatorConfig::default());
        assert!(service.can_push());
    }
}
