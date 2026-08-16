//! Bandwidth Policy Configuration
//!
//! Defines the bandwidth allocation policies for tenants and the global system limits.

use serde::{Deserialize, Serialize};

/// Direction of bandwidth usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BandwidthDirection {
    /// Upload bandwidth (serving blobs to peers)
    Upload,
    /// Download bandwidth (fetching blobs from peers)
    Download,
}

/// Tenant priority levels for bandwidth allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantPriority {
    /// Lowest priority - first to be throttled when system is saturated
    Low = 0,
    /// Normal priority - standard bandwidth allocation
    Normal = 1,
    /// High priority - gets bandwidth before lower priority tenants
    High = 2,
    /// Critical priority - nearly unaffected by system limits
    Critical = 3,
}

impl Default for TenantPriority {
    fn default() -> Self {
        Self::Normal
    }
}

/// Per-tenant bandwidth policy.
///
/// Each tenant gets dedicated bandwidth allocations that are independent
/// of other tenants, subject to global system limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BandwidthPolicy {
    /// Maximum upload bandwidth in bytes per second.
    /// `0` means unlimited (subject to global limits).
    pub max_upload_bps: u64,

    /// Maximum download bandwidth in bytes per second.
    /// `0` means unlimited (subject to global limits).
    pub max_download_bps: u64,

    /// Priority level for bandwidth allocation.
    pub priority: TenantPriority,

    /// Whether this tenant is allowed to use reserved bandwidth
    /// when global bandwidth is saturated.
    #[serde(default)]
    pub can_use_reserved: bool,

    /// Burst allowance multiplier (1.0 = no burst, 2.0 = 2x for short periods).
    #[serde(default = "default_burst_multiplier")]
    pub burst_multiplier: f64,
}

fn default_burst_multiplier() -> f64 {
    1.0
}

impl Default for BandwidthPolicy {
    fn default() -> Self {
        Self {
            max_upload_bps: 0,
            max_download_bps: 0,
            priority: TenantPriority::default(),
            can_use_reserved: false,
            burst_multiplier: 1.0,
        }
    }
}

impl BandwidthPolicy {
    /// Create a new policy with explicit limits.
    pub fn new(max_upload_bps: u64, max_download_bps: u64) -> Self {
        Self {
            max_upload_bps,
            max_download_bps,
            ..Default::default()
        }
    }

    /// Set the priority level.
    pub fn with_priority(mut self, priority: TenantPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Allow using reserved bandwidth.
    pub fn with_reserved(mut self) -> Self {
        self.can_use_reserved = true;
        self
    }

    /// Set burst multiplier.
    ///
    /// The multiplier is clamped to a reasonable range [0.1, 10.0] to prevent
    /// extreme burst allowances that could defeat the purpose of rate limiting.
    pub fn with_burst(mut self, multiplier: f64) -> Self {
        self.burst_multiplier = multiplier.clamp(0.1, 10.0);
        self
    }

    /// Get the effective upload limit for a given global remaining capacity.
    pub fn effective_upload_limit(&self, global_remaining: u64, reserved_available: bool) -> u64 {
        let mut limit = self.max_upload_bps;

        // Apply global remaining capacity
        if limit == 0 || limit > global_remaining {
            limit = global_remaining;
        }

        // Add reserved capacity if available and allowed
        if reserved_available && self.can_use_reserved {
            limit = limit.saturating_add(global_remaining);
        }

        // Apply burst
        if self.burst_multiplier > 1.0 {
            limit = (limit as f64 * self.burst_multiplier) as u64;
        }

        limit
    }

    /// Get the effective download limit.
    pub fn effective_download_limit(&self, global_remaining: u64, reserved_available: bool) -> u64 {
        let mut limit = self.max_download_bps;

        if limit == 0 || limit > global_remaining {
            limit = global_remaining;
        }

        if reserved_available && self.can_use_reserved {
            limit = limit.saturating_add(global_remaining);
        }

        if self.burst_multiplier > 1.0 {
            limit = (limit as f64 * self.burst_multiplier) as u64;
        }

        limit
    }
}

/// Global bandwidth limits for the entire system.
///
/// These limits ensure that P2P transfers don't consume all available bandwidth,
/// leaving room for normal NAS operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalBandwidthLimits {
    /// Maximum total upload bandwidth for all P2P transfers (bytes/sec).
    pub max_upload_bps: u64,

    /// Maximum total download bandwidth for all P2P transfers (bytes/sec).
    pub max_download_bps: u64,

    /// Bandwidth reserved for system operations (bytes/sec).
    /// This is subtracted from the usable bandwidth pool.
    pub reserved_for_system_bps: u64,

    /// Whether to enable per-tenant fairness when system is saturated.
    #[serde(default = "default_fair_share")]
    pub enable_fair_share: bool,

    /// Minimum bandwidth guaranteed per tenant (bytes/sec).
    /// Only applies when fair_share is enabled.
    #[serde(default)]
    pub min_guaranteed_per_tenant_bps: u64,
}

fn default_fair_share() -> bool {
    true
}

impl Default for GlobalBandwidthLimits {
    fn default() -> Self {
        Self {
            max_upload_bps: 100 * 1024 * 1024,         // 100 MB/s
            max_download_bps: 200 * 1024 * 1024,       // 200 MB/s
            reserved_for_system_bps: 50 * 1024 * 1024, // 50 MB/s
            enable_fair_share: true,
            min_guaranteed_per_tenant_bps: 1024 * 1024, // 1 MB/s
        }
    }
}

impl GlobalBandwidthLimits {
    /// Create new limits with specified values.
    pub fn new(max_upload_bps: u64, max_download_bps: u64, reserved_bps: u64) -> Self {
        Self {
            max_upload_bps,
            max_download_bps,
            reserved_for_system_bps: reserved_bps,
            ..Default::default()
        }
    }

    /// Usable upload bandwidth (total - reserved).
    pub fn usable_upload_bps(&self) -> u64 {
        self.max_upload_bps
            .saturating_sub(self.reserved_for_system_bps)
    }

    /// Usable download bandwidth (total - reserved).
    pub fn usable_download_bps(&self) -> u64 {
        self.max_download_bps
            .saturating_sub(self.reserved_for_system_bps)
    }
}

/// A named tenant identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(pub String);

impl TenantId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl AsRef<str> for TenantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tenant({})", self.0)
    }
}

/// Summary of a tenant's current bandwidth usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantBandwidthStatus {
    pub tenant_id: String,
    pub policy: BandwidthPolicy,
    pub current_upload_bps: u64,
    pub current_download_bps: u64,
    pub queued_upload_bytes: u64,
    pub queued_download_bytes: u64,
    pub tokens_available: bool,
}

/// Summary of global bandwidth usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalBandwidthStatus {
    pub limits: GlobalBandwidthLimits,
    pub total_upload_bps: u64,
    pub total_download_bps: u64,
    pub remaining_upload_bps: u64,
    pub remaining_download_bps: u64,
    pub active_tenants: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_default() {
        let policy = BandwidthPolicy::default();
        assert_eq!(policy.max_upload_bps, 0);
        assert_eq!(policy.max_download_bps, 0);
        assert_eq!(policy.priority, TenantPriority::Normal);
    }

    #[test]
    fn test_policy_builder() {
        let policy = BandwidthPolicy::new(1024, 2048)
            .with_priority(TenantPriority::High)
            .with_reserved()
            .with_burst(1.5);

        assert_eq!(policy.max_upload_bps, 1024);
        assert_eq!(policy.max_download_bps, 2048);
        assert_eq!(policy.priority, TenantPriority::High);
        assert!(policy.can_use_reserved);
        assert!((policy.burst_multiplier - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_effective_limit() {
        let policy = BandwidthPolicy::new(10_000_000, 20_000_000); // 10MB/s upload, 20MB/s download

        // Within global capacity
        assert_eq!(
            policy.effective_upload_limit(100_000_000, false),
            10_000_000
        );
        assert_eq!(
            policy.effective_download_limit(100_000_000, false),
            20_000_000
        );

        // Global capacity smaller than policy
        assert_eq!(policy.effective_upload_limit(5_000_000, false), 5_000_000);
        assert_eq!(
            policy.effective_download_limit(10_000_000, false),
            10_000_000
        );
    }

    #[test]
    fn test_global_limits_usable() {
        let limits = GlobalBandwidthLimits::new(
            100_000_000, // 100 MB/s
            200_000_000, // 200 MB/s
            50_000_000,  // 50 MB/s reserved
        );

        assert_eq!(limits.usable_upload_bps(), 50_000_000);
        assert_eq!(limits.usable_download_bps(), 150_000_000);
    }

    #[test]
    fn test_tenant_id() {
        let id = TenantId::new("user@example.com");
        assert_eq!(id.as_ref(), "user@example.com");
        assert_eq!(id.to_string(), "Tenant(user@example.com)");
    }
}
