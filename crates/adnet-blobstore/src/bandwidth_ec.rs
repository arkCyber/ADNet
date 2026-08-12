//! Bandwidth-Aware EC Transfer Service
//!
//! This module provides multi-tenant bandwidth control for P2P transfers,
//! ensuring that uploads and downloads respect per-tenant bandwidth limits
//! and global system constraints.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adnet_types::ContentHash;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use super::bandwidth::{
    BandwidthDirection, BandwidthPermit, BandwidthPolicy, GlobalBandwidthLimits,
    TenantBandwidthManager, TenantBandwidthStatus, TenantId, TenantPriority,
};

/// Errors specific to bandwidth-aware transfers.
#[derive(Debug, Error)]
pub enum BandwidthECError {
    #[error("bandwidth limit exceeded: {0}")]
    LimitExceeded(String),

    #[error("bandwidth timeout after {0:?}")]
    Timeout(Duration),

    #[error("tenant not registered: {0}")]
    TenantNotFound(String),

    #[error("bandwidth error: {0}")]
    Bandwidth(#[from] super::bandwidth::BandwidthError),
}

/// Bandwidth-aware configuration for EC transfers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BandwidthECConfig {
    pub global_limits: GlobalBandwidthLimits,
    pub default_upload_policy: BandwidthPolicy,
    pub default_download_policy: BandwidthPolicy,
    #[serde(default = "default_transfer_timeout")]
    pub transfer_timeout: Duration,
}

fn default_transfer_timeout() -> Duration {
    Duration::from_secs(300)
}

impl Default for BandwidthECConfig {
    fn default() -> Self {
        Self {
            global_limits: GlobalBandwidthLimits::default(),
            default_upload_policy: BandwidthPolicy {
                max_upload_bps: 10 * 1024 * 1024,
                max_download_bps: 50 * 1024 * 1024,
                priority: TenantPriority::Normal,
                can_use_reserved: false,
                burst_multiplier: 1.0,
            },
            default_download_policy: BandwidthPolicy {
                max_upload_bps: 50 * 1024 * 1024,
                max_download_bps: 100 * 1024 * 1024,
                priority: TenantPriority::Normal,
                can_use_reserved: false,
                burst_multiplier: 1.0,
            },
            transfer_timeout: default_transfer_timeout(),
        }
    }
}

/// Bandwidth-aware EC Transfer Service.
#[allow(dead_code)]
pub struct BandwidthECService {
    bandwidth_manager: Arc<TenantBandwidthManager>,
    default_upload_policy: BandwidthPolicy,
    default_download_policy: BandwidthPolicy,
    transfer_timeout: Duration,
    active_transfers: RwLock<HashMap<ContentHash, BandwidthPermit>>,
}

impl BandwidthECService {
    /// Create a new service.
    pub fn new(config: BandwidthECConfig) -> Self {
        Self {
            bandwidth_manager: Arc::new(TenantBandwidthManager::new(config.global_limits.clone())),
            default_upload_policy: config.default_upload_policy,
            default_download_policy: config.default_download_policy,
            transfer_timeout: config.transfer_timeout,
            active_transfers: RwLock::new(HashMap::new()),
        }
    }

    /// Get the bandwidth manager.
    pub fn bandwidth_manager(&self) -> &Arc<TenantBandwidthManager> {
        &self.bandwidth_manager
    }

    /// Register a new tenant.
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
            .add_tenant(tenant_id, self.default_upload_policy.clone())
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

    /// Acquire bandwidth for upload.
    pub async fn acquire_upload(
        &self,
        tenant_id: &TenantId,
        bytes: u64,
    ) -> Result<BandwidthPermit, BandwidthECError> {
        let permit = self
            .bandwidth_manager
            .acquire_with_timeout(
                tenant_id,
                BandwidthDirection::Upload,
                bytes,
                self.transfer_timeout,
            )
            .await?;

        debug!(tenant_id = %tenant_id, bytes = bytes, "bandwidth permit acquired for EC upload");
        Ok(permit)
    }

    /// Acquire bandwidth for download.
    pub async fn acquire_download(
        &self,
        tenant_id: &TenantId,
        bytes: u64,
    ) -> Result<BandwidthPermit, BandwidthECError> {
        let permit = self
            .bandwidth_manager
            .acquire_with_timeout(
                tenant_id,
                BandwidthDirection::Download,
                bytes,
                self.transfer_timeout,
            )
            .await?;

        debug!(tenant_id = %tenant_id, bytes = bytes, "bandwidth permit acquired for EC download");
        Ok(permit)
    }

    /// Get tenant status.
    pub fn get_tenant_status(
        &self,
        tenant_id: &TenantId,
    ) -> Result<TenantBandwidthStatus, BandwidthECError> {
        Ok(self.bandwidth_manager.get_tenant_status(tenant_id)?)
    }

    /// Get global status.
    pub fn get_global_status(&self) -> super::bandwidth::GlobalBandwidthStatus {
        self.bandwidth_manager.get_global_status()
    }

    /// List all registered tenants.
    pub fn list_tenants(&self) -> Vec<TenantId> {
        self.bandwidth_manager.list_tenants()
    }
}

/// Predefined bandwidth profiles.
pub mod profiles {
    use super::*;

    /// Family user profile - limited bandwidth for home NAS.
    pub fn family_profile() -> BandwidthECConfig {
        BandwidthECConfig {
            global_limits: GlobalBandwidthLimits::new(
                20 * 1024 * 1024,
                50 * 1024 * 1024,
                10 * 1024 * 1024,
            ),
            default_upload_policy: BandwidthPolicy::new(5 * 1024 * 1024, 20 * 1024 * 1024)
                .with_priority(TenantPriority::Normal),
            default_download_policy: BandwidthPolicy::new(20 * 1024 * 1024, 50 * 1024 * 1024)
                .with_priority(TenantPriority::Normal),
            ..Default::default()
        }
    }

    /// Enterprise profile - higher bandwidth for business use.
    pub fn enterprise_profile() -> BandwidthECConfig {
        BandwidthECConfig {
            global_limits: GlobalBandwidthLimits::new(
                100 * 1024 * 1024,
                200 * 1024 * 1024,
                50 * 1024 * 1024,
            ),
            default_upload_policy: BandwidthPolicy::new(20 * 1024 * 1024, 100 * 1024 * 1024)
                .with_priority(TenantPriority::Normal),
            default_download_policy: BandwidthPolicy::new(100 * 1024 * 1024, 200 * 1024 * 1024)
                .with_priority(TenantPriority::Normal),
            ..Default::default()
        }
    }

    /// Guest profile - very limited bandwidth for visitors.
    pub fn guest_profile() -> BandwidthECConfig {
        BandwidthECConfig {
            global_limits: GlobalBandwidthLimits::new(
                5 * 1024 * 1024,
                20 * 1024 * 1024,
                2 * 1024 * 1024,
            ),
            default_upload_policy: BandwidthPolicy::new(512 * 1024, 2 * 1024 * 1024)
                .with_priority(TenantPriority::Low),
            default_download_policy: BandwidthPolicy::new(2 * 1024 * 1024, 10 * 1024 * 1024)
                .with_priority(TenantPriority::Low),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BandwidthECConfig::default();
        assert!(config.global_limits.max_upload_bps > 0);
    }

    #[tokio::test]
    async fn test_register_tenant() {
        let service = BandwidthECService::new(BandwidthECConfig::default());
        assert!(
            service
                .register_tenant(TenantId::new("test"), BandwidthPolicy::default())
                .is_ok()
        );
    }
}
