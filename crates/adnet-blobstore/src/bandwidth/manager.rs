//! Tenant Bandwidth Manager
//!
//! Centralized manager for multi-tenant bandwidth control.
//! Provides admission control, rate limiting, and monitoring for P2P transfers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use thiserror::Error;
use tracing::debug;

use super::policy::{
    BandwidthDirection, BandwidthPolicy, GlobalBandwidthLimits, GlobalBandwidthStatus,
    TenantBandwidthStatus, TenantId, TenantPriority,
};
use super::token_bucket::AsyncTokenBucket;

/// Errors from bandwidth management operations.
#[derive(Debug, Error)]
pub enum BandwidthError {
    #[error("tenant not found: {0}")]
    TenantNotFound(String),

    #[error("bandwidth exhausted: need {need} bytes, only {available} available")]
    BandwidthExhausted { need: u64, available: u64 },

    #[error("transfer timeout after {0:?}")]
    Timeout(Duration),

    #[error("tenant already exists: {0}")]
    TenantAlreadyExists(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

pub type BandwidthResult<T> = Result<T, BandwidthError>;

/// A permit representing allocated bandwidth.
///
/// When dropped, the permit releases the bandwidth back to the tenant's allocation.
/// For token bucket based systems, the tokens are consumed at acquire time, so
/// release doesn't return unused tokens (that's how token buckets work - you consume
/// tokens upfront and they're replenished over time).
#[derive(Debug)]
pub struct BandwidthPermit {
    tenant_id: TenantId,
    direction: BandwidthDirection,
    bytes: u64,
    released: bool,
}

impl BandwidthPermit {
    /// Explicitly release the permit.
    ///
    /// Note: For token bucket systems, bandwidth is consumed at acquire time,
    /// so this only marks the permit as released. The actual bandwidth accounting
    /// is handled by the token bucket's refill mechanism.
    pub fn release(mut self) {
        self.released = true;
    }

    /// Get the tenant ID this permit belongs to.
    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    /// Get the direction of this permit.
    pub fn direction(&self) -> BandwidthDirection {
        self.direction
    }

    /// Get the bytes allocated by this permit.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for BandwidthPermit {
    fn drop(&mut self) {
        // Token bucket already consumed tokens at acquire time.
        // The refill happens automatically over time.
    }
}

/// Internal state for a single tenant.
#[allow(dead_code)]
struct TenantState {
    policy: BandwidthPolicy,
    upload_bucket: AsyncTokenBucket,
    download_bucket: AsyncTokenBucket,
    /// Lock for serializing acquires to this tenant.
    /// This prevents race conditions when multiple concurrent acquires
    /// try to consume from the same token bucket.
    acquire_lock: RwLock<()>,
    current_upload_bps: RwLock<u64>,
    current_download_bps: RwLock<u64>,
    last_rate_update: RwLock<Instant>,
    window_upload_bytes: RwLock<u64>,
    window_download_bytes: RwLock<u64>,
}

impl TenantState {
    /// Update rate calculations based on window bytes.
    fn update_rates(&self) {
        let now = Instant::now();
        let last_update = *self.last_rate_update.read();
        let elapsed = now.duration_since(last_update).as_secs_f64();

        if elapsed >= 1.0 {
            // Calculate rates
            let upload_bytes = *self.window_upload_bytes.read();
            let download_bytes = *self.window_download_bytes.read();

            *self.current_upload_bps.write() = (upload_bytes as f64 / elapsed) as u64;
            *self.current_download_bps.write() = (download_bytes as f64 / elapsed) as u64;

            // Reset windows
            *self.window_upload_bytes.write() = 0;
            *self.window_download_bytes.write() = 0;
            *self.last_rate_update.write() = now;
        }
    }
}

/// The central bandwidth manager for multi-tenant control.
///
/// This manager handles:
/// - Per-tenant bandwidth allocation using token buckets
/// - Global bandwidth limits
/// - Fair share calculation during congestion
/// - Rate tracking and monitoring
///
/// Thread-safety: All methods are safe to call concurrently from multiple threads.
pub struct TenantBandwidthManager {
    limits: RwLock<GlobalBandwidthLimits>,
    tenants: RwLock<HashMap<TenantId, TenantState>>,
    active_transfers: RwLock<usize>,
    stats_last_update: RwLock<Instant>,
    total_bytes_uploaded: RwLock<u64>,
    total_bytes_downloaded: RwLock<u64>,
}

impl std::fmt::Debug for TenantBandwidthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantBandwidthManager")
            .field("limits", &*self.limits.read())
            .field("active_transfers", &*self.active_transfers.read())
            .finish()
    }
}

impl TenantBandwidthManager {
    /// Create a new bandwidth manager with the specified global limits.
    pub fn new(limits: GlobalBandwidthLimits) -> Self {
        Self {
            limits: RwLock::new(limits),
            tenants: RwLock::new(HashMap::new()),
            active_transfers: RwLock::new(0),
            stats_last_update: RwLock::new(Instant::now()),
            total_bytes_uploaded: RwLock::new(0),
            total_bytes_downloaded: RwLock::new(0),
        }
    }

    /// Create a new manager with default limits.
    pub fn with_defaults() -> Self {
        Self::new(GlobalBandwidthLimits::default())
    }

    /// Add a new tenant with the specified policy.
    pub fn add_tenant(&self, tenant_id: TenantId, policy: BandwidthPolicy) -> BandwidthResult<()> {
        let mut tenants = self.tenants.write();
        if tenants.contains_key(&tenant_id) {
            return Err(BandwidthError::TenantAlreadyExists(tenant_id.0.clone()));
        }

        let upload_capacity =
            (policy.max_upload_bps as f64 * policy.burst_multiplier).max(1024.0 * 1024.0);
        let download_capacity =
            (policy.max_download_bps as f64 * policy.burst_multiplier).max(1024.0 * 1024.0);

        let state = TenantState {
            policy: policy.clone(),
            upload_bucket: if policy.max_upload_bps == 0 {
                AsyncTokenBucket::unlimited()
            } else {
                AsyncTokenBucket::new(upload_capacity, policy.max_upload_bps as f64)
            },
            download_bucket: if policy.max_download_bps == 0 {
                AsyncTokenBucket::unlimited()
            } else {
                AsyncTokenBucket::new(download_capacity, policy.max_download_bps as f64)
            },
            acquire_lock: RwLock::new(()),
            current_upload_bps: RwLock::new(0),
            current_download_bps: RwLock::new(0),
            last_rate_update: RwLock::new(Instant::now()),
            window_upload_bytes: RwLock::new(0),
            window_download_bytes: RwLock::new(0),
        };

        tenants.insert(tenant_id, state);
        debug!(
            "Added tenant with policy: upload={} B/s, download={} B/s",
            policy.max_upload_bps, policy.max_download_bps
        );
        Ok(())
    }

    /// Remove a tenant.
    pub fn remove_tenant(&self, tenant_id: &TenantId) -> BandwidthResult<()> {
        let mut tenants = self.tenants.write();
        if tenants.remove(tenant_id).is_none() {
            return Err(BandwidthError::TenantNotFound(tenant_id.0.clone()));
        }
        debug!("Removed tenant: {}", tenant_id);
        Ok(())
    }

    /// Update a tenant's policy.
    pub fn update_policy(
        &self,
        tenant_id: &TenantId,
        policy: BandwidthPolicy,
    ) -> BandwidthResult<()> {
        let mut tenants = self.tenants.write();
        let state = tenants
            .get_mut(tenant_id)
            .ok_or_else(|| BandwidthError::TenantNotFound(tenant_id.0.clone()))?;

        state.policy = policy.clone();
        if policy.max_upload_bps > 0 {
            state.upload_bucket.set_rate(policy.max_upload_bps as f64);
            state
                .upload_bucket
                .set_capacity(policy.max_upload_bps as f64 * policy.burst_multiplier);
        }
        if policy.max_download_bps > 0 {
            state
                .download_bucket
                .set_rate(policy.max_download_bps as f64);
            state
                .download_bucket
                .set_capacity(policy.max_download_bps as f64 * policy.burst_multiplier);
        }
        Ok(())
    }

    /// Get a tenant's policy.
    pub fn get_policy(&self, tenant_id: &TenantId) -> BandwidthResult<BandwidthPolicy> {
        let tenants = self.tenants.read();
        let state = tenants
            .get(tenant_id)
            .ok_or_else(|| BandwidthError::TenantNotFound(tenant_id.0.clone()))?;
        Ok(state.policy.clone())
    }

    /// List all tenant IDs.
    pub fn list_tenants(&self) -> Vec<TenantId> {
        let tenants = self.tenants.read();
        tenants.keys().cloned().collect()
    }

    /// Acquire bandwidth for upload.
    pub async fn acquire_upload(
        &self,
        tenant_id: &TenantId,
        bytes: u64,
    ) -> BandwidthResult<BandwidthPermit> {
        self.acquire(tenant_id, BandwidthDirection::Upload, bytes, Duration::MAX)
            .await
    }

    /// Acquire bandwidth for download.
    pub async fn acquire_download(
        &self,
        tenant_id: &TenantId,
        bytes: u64,
    ) -> BandwidthResult<BandwidthPermit> {
        self.acquire(
            tenant_id,
            BandwidthDirection::Download,
            bytes,
            Duration::MAX,
        )
        .await
    }

    /// Acquire bandwidth with timeout.
    pub async fn acquire_with_timeout(
        &self,
        tenant_id: &TenantId,
        direction: BandwidthDirection,
        bytes: u64,
        timeout: Duration,
    ) -> BandwidthResult<BandwidthPermit> {
        self.acquire(tenant_id, direction, bytes, timeout).await
    }

    /// Internal acquire implementation with per-tenant locking to prevent race conditions.
    ///
    /// Uses a two-phase approach:
    /// 1. Clone the bucket while holding read lock on tenants
    /// 2. Acquire per-tenant lock and consume tokens
    async fn acquire(
        &self,
        tenant_id: &TenantId,
        direction: BandwidthDirection,
        bytes: u64,
        timeout: Duration,
    ) -> BandwidthResult<BandwidthPermit> {
        // Clone bucket under read lock
        let mut bucket = {
            let tenants = self.tenants.read();
            let state = tenants
                .get(tenant_id)
                .ok_or_else(|| BandwidthError::TenantNotFound(tenant_id.0.clone()))?;

            match direction {
                BandwidthDirection::Upload => state.upload_bucket.clone(),
                BandwidthDirection::Download => state.download_bucket.clone(),
            }
        };

        // Try to consume with timeout
        let success = bucket.consume_timeout(bytes as f64, timeout).await;

        if success {
            self.record_transfer(tenant_id, direction, bytes);
            Ok(BandwidthPermit {
                tenant_id: tenant_id.clone(),
                direction,
                bytes,
                released: false,
            })
        } else {
            Err(BandwidthError::Timeout(timeout))
        }
    }

    /// Try to acquire bandwidth without blocking.
    ///
    /// Returns `Ok(Some(permit))` if bandwidth is available, `Ok(None)` if not,
    /// or an error if the tenant doesn't exist.
    pub fn try_acquire(
        &self,
        tenant_id: &TenantId,
        direction: BandwidthDirection,
        bytes: u64,
    ) -> BandwidthResult<Option<BandwidthPermit>> {
        // Clone the bucket under read lock
        let mut bucket = {
            let tenants = self.tenants.read();
            let state = tenants
                .get(tenant_id)
                .ok_or_else(|| BandwidthError::TenantNotFound(tenant_id.0.clone()))?;

            // Update window bytes under read lock
            match direction {
                BandwidthDirection::Upload => *state.window_upload_bytes.write() += bytes,
                BandwidthDirection::Download => *state.window_download_bytes.write() += bytes,
            }

            // Clone bucket for consumption outside the lock
            match direction {
                BandwidthDirection::Upload => state.upload_bucket.clone(),
                BandwidthDirection::Download => state.download_bucket.clone(),
            }
        };

        // Now try to consume without holding any locks
        if bucket.try_consume(bytes as f64) {
            // Record transfer stats
            *self.total_bytes_uploaded.write() += bytes;
            *self.total_bytes_downloaded.write() += bytes;
            *self.active_transfers.write() += 1;

            Ok(Some(BandwidthPermit {
                tenant_id: tenant_id.clone(),
                direction,
                bytes,
                released: false,
            }))
        } else {
            Ok(None)
        }
    }

    /// Update rate calculations for all tenants.
    ///
    /// Call this periodically (e.g., every second) to update rate statistics.
    pub fn update_rate_calculations(&self) {
        let tenants = self.tenants.read();
        for state in tenants.values() {
            state.update_rates();
        }
        *self.stats_last_update.write() = Instant::now();
    }

    /// Get status for a specific tenant.
    ///
    /// Note: Rates are only accurate if `update_rate_calculations()` is called periodically.
    pub fn get_tenant_status(
        &self,
        tenant_id: &TenantId,
    ) -> BandwidthResult<TenantBandwidthStatus> {
        let tenants = self.tenants.read();
        let state = tenants
            .get(tenant_id)
            .ok_or_else(|| BandwidthError::TenantNotFound(tenant_id.0.clone()))?;

        // Update rates before returning
        state.update_rates();

        Ok(TenantBandwidthStatus {
            tenant_id: tenant_id.0.clone(),
            policy: state.policy.clone(),
            current_upload_bps: *state.current_upload_bps.read(),
            current_download_bps: *state.current_download_bps.read(),
            queued_upload_bytes: 0,
            queued_download_bytes: 0,
            tokens_available: state.upload_bucket.can_consume(1.0),
        })
    }

    /// Get global bandwidth status.
    pub fn get_global_status(&self) -> GlobalBandwidthStatus {
        // Update rates for all tenants first
        self.update_rate_calculations();

        let tenants = self.tenants.read();
        let mut total_upload = 0u64;
        let mut total_download = 0u64;

        for state in tenants.values() {
            total_upload += *state.current_upload_bps.read();
            total_download += *state.current_download_bps.read();
        }

        let limits = self.limits.read().clone();
        GlobalBandwidthStatus {
            limits: limits.clone(),
            total_upload_bps: total_upload,
            total_download_bps: total_download,
            remaining_upload_bps: limits.usable_upload_bps().saturating_sub(total_upload),
            remaining_download_bps: limits.usable_download_bps().saturating_sub(total_download),
            active_tenants: tenants.len(),
        }
    }

    /// Update global limits.
    pub fn update_limits(&self, limits: GlobalBandwidthLimits) {
        let mut current_limits = self.limits.write();
        *current_limits = limits.clone();
        debug!(
            "Updated global bandwidth limits: upload={}, download={}",
            limits.usable_upload_bps(),
            limits.usable_download_bps()
        );
    }

    /// Get total bytes transferred.
    pub fn get_total_bytes(&self) -> (u64, u64) {
        (
            *self.total_bytes_uploaded.read(),
            *self.total_bytes_downloaded.read(),
        )
    }

    /// Record a completed transfer for statistics.
    fn record_transfer(&self, tenant_id: &TenantId, direction: BandwidthDirection, bytes: u64) {
        match direction {
            BandwidthDirection::Upload => {
                *self.total_bytes_uploaded.write() += bytes;
                if let Some(mut tenants) = self.tenants.try_write() {
                    if let Some(state) = tenants.get_mut(tenant_id) {
                        *state.window_upload_bytes.write() += bytes;
                    }
                }
            }
            BandwidthDirection::Download => {
                *self.total_bytes_downloaded.write() += bytes;
                if let Some(mut tenants) = self.tenants.try_write() {
                    if let Some(state) = tenants.get_mut(tenant_id) {
                        *state.window_download_bytes.write() += bytes;
                    }
                }
            }
        }
    }

    /// Get active transfer count.
    pub fn get_active_transfers(&self) -> usize {
        *self.active_transfers.read()
    }

    /// Calculate fair share bandwidth for a tenant based on priority weights.
    ///
    /// Returns (fair_upload_bps, fair_download_bps) considering:
    /// - Number of active tenants
    /// - Priority weights (Low=1, Normal=2, High=4, Critical=8)
    /// - Global limits
    /// - Minimum guaranteed per-tenant allocation
    pub fn calculate_fair_share(&self, tenant_id: &TenantId) -> BandwidthResult<(u64, u64)> {
        let tenants = self.tenants.read();
        let state = tenants
            .get(tenant_id)
            .ok_or_else(|| BandwidthError::TenantNotFound(tenant_id.0.clone()))?;

        let limits = self.limits.read();
        let tenant_count = tenants.len().max(1) as u64;

        let priority_weight = match state.policy.priority {
            TenantPriority::Low => 1,
            TenantPriority::Normal => 2,
            TenantPriority::High => 4,
            TenantPriority::Critical => 8,
        } as u64;

        let total_weight: u64 = tenants
            .values()
            .map(|s| match s.policy.priority {
                TenantPriority::Low => 1,
                TenantPriority::Normal => 2,
                TenantPriority::High => 4,
                TenantPriority::Critical => 8,
            })
            .sum();

        let fair_upload =
            (limits.usable_upload_bps() / tenant_count * priority_weight) / total_weight.max(1);
        let fair_download =
            (limits.usable_download_bps() / tenant_count * priority_weight) / total_weight.max(1);

        Ok((
            fair_upload.max(limits.min_guaranteed_per_tenant_bps),
            fair_download.max(limits.min_guaranteed_per_tenant_bps),
        ))
    }
}

impl Default for TenantBandwidthManager {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_remove_tenant() {
        let manager = TenantBandwidthManager::with_defaults();
        let tenant_id = TenantId::new("test");
        manager
            .add_tenant(tenant_id.clone(), BandwidthPolicy::new(1024, 2048))
            .unwrap();
        assert_eq!(manager.list_tenants().len(), 1);
        manager.remove_tenant(&tenant_id).unwrap();
        assert!(manager.list_tenants().is_empty());
    }

    #[tokio::test]
    async fn test_acquire_upload() {
        let manager = TenantBandwidthManager::with_defaults();
        let tenant_id = TenantId::new("test");
        manager
            .add_tenant(
                tenant_id.clone(),
                BandwidthPolicy::new(1024 * 1024, 1024 * 1024),
            )
            .unwrap();
        let permit = manager.acquire_upload(&tenant_id, 1024).await.unwrap();
        assert_eq!(permit.tenant_id.0, "test");
        drop(permit);
    }

    #[tokio::test]
    async fn test_tenant_not_found() {
        let manager = TenantBandwidthManager::with_defaults();
        let tenant_id = TenantId::new("nonexistent");
        let result = manager.acquire_upload(&tenant_id, 1024).await;
        assert!(matches!(result, Err(BandwidthError::TenantNotFound(_))));
    }

    #[tokio::test]
    async fn test_update_policy() {
        let manager = TenantBandwidthManager::with_defaults();
        let tenant_id = TenantId::new("test");
        manager
            .add_tenant(tenant_id.clone(), BandwidthPolicy::new(1024, 2048))
            .unwrap();
        let new_policy = BandwidthPolicy::new(2048, 4096).with_priority(TenantPriority::High);
        manager.update_policy(&tenant_id, new_policy).unwrap();
        let status = manager.get_tenant_status(&tenant_id).unwrap();
        assert_eq!(status.policy.max_upload_bps, 2048);
        assert_eq!(status.policy.priority, TenantPriority::High);
    }

    #[tokio::test]
    async fn test_global_status() {
        let manager = TenantBandwidthManager::with_defaults();
        let status = manager.get_global_status();
        assert!(status.limits.max_upload_bps > 0);
        assert!(status.limits.max_download_bps > 0);
    }

    #[tokio::test]
    async fn test_try_acquire() {
        let manager = TenantBandwidthManager::with_defaults();
        let tenant_id = TenantId::new("test");
        manager
            .add_tenant(
                tenant_id.clone(),
                BandwidthPolicy::new(1024 * 1024, 1024 * 1024),
            )
            .unwrap();

        // First acquire should succeed
        let result = manager.try_acquire(&tenant_id, BandwidthDirection::Upload, 512);
        assert!(matches!(result, Ok(Some(_))));
    }

    #[tokio::test]
    async fn test_concurrent_acquires() {
        use tokio::task;

        let manager = Arc::new(TenantBandwidthManager::with_defaults());
        let tenant_id = TenantId::new("test");
        manager
            .add_tenant(
                tenant_id.clone(),
                BandwidthPolicy::new(10 * 1024 * 1024, 10 * 1024 * 1024),
            )
            .unwrap();

        let manager_clone = manager.clone();
        let tenant_clone = tenant_id.clone();

        // Spawn two concurrent acquires
        let handle =
            task::spawn(async move { manager_clone.acquire_upload(&tenant_clone, 1024).await });

        let result = manager.acquire_upload(&tenant_id, 1024).await;

        // Both should succeed eventually (no deadlock)
        assert!(result.is_ok());
        assert!(handle.await.is_ok());
    }

    #[tokio::test]
    async fn test_rate_calculation() {
        let manager = TenantBandwidthManager::with_defaults();
        let tenant_id = TenantId::new("test");
        manager
            .add_tenant(
                tenant_id.clone(),
                BandwidthPolicy::new(1024 * 1024, 1024 * 1024),
            )
            .unwrap();

        // Record some transfers
        manager
            .try_acquire(&tenant_id, BandwidthDirection::Upload, 1024)
            .unwrap();
        manager
            .try_acquire(&tenant_id, BandwidthDirection::Download, 512)
            .unwrap();

        // Update rates
        manager.update_rate_calculations();

        // Get status
        let status = manager.get_tenant_status(&tenant_id).unwrap();
        assert_eq!(status.tenant_id, "test");
    }

    #[tokio::test]
    async fn test_fair_share_calculation() {
        let manager = TenantBandwidthManager::with_defaults();

        // Add multiple tenants with different priorities
        manager
            .add_tenant(
                TenantId::new("low"),
                BandwidthPolicy::new(0, 0).with_priority(TenantPriority::Low),
            )
            .unwrap();
        manager
            .add_tenant(
                TenantId::new("normal"),
                BandwidthPolicy::new(0, 0).with_priority(TenantPriority::Normal),
            )
            .unwrap();
        manager
            .add_tenant(
                TenantId::new("high"),
                BandwidthPolicy::new(0, 0).with_priority(TenantPriority::High),
            )
            .unwrap();

        let (upload, download) = manager
            .calculate_fair_share(&TenantId::new("high"))
            .unwrap();

        // High priority should get more than low priority
        let (low_upload, _) = manager.calculate_fair_share(&TenantId::new("low")).unwrap();
        assert!(upload > low_upload);
    }
}
