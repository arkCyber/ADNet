//! Integration tests for Bandwidth Management.
//!
//! These tests verify the bandwidth management API.

#[cfg(test)]
mod bandwidth_integration_tests {
    use adnet_blobstore::bandwidth::{
        BandwidthDirection, BandwidthPolicy, GlobalBandwidthLimits, TenantBandwidthManager,
        TenantId, TenantPriority,
    };

    // ─────────────────────────────────────────────────────────────────
    // Helper: Create test bandwidth policies
    // ─────────────────────────────────────────────────────────────────

    fn make_unlimited_policy() -> BandwidthPolicy {
        BandwidthPolicy::new(0, 0)
    }

    fn make_rate_limited_policy(upload_bps: u64, download_bps: u64) -> BandwidthPolicy {
        BandwidthPolicy::new(upload_bps, download_bps)
    }

    fn make_global_limits(max_upload: u64, max_download: u64) -> GlobalBandwidthLimits {
        GlobalBandwidthLimits::new(max_upload, max_download, 0)
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Tenant Bandwidth Manager Lifecycle
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_tenant_manager_creation() {
        let limits = make_global_limits(100_000_000, 100_000_000); // 100 MB/s
        let manager = TenantBandwidthManager::new(limits);

        let status = manager.get_global_status();
        assert_eq!(status.total_upload_bps, 0);
        assert_eq!(status.total_download_bps, 0);
    }

    #[test]
    fn test_tenant_registration() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant_id = TenantId::new("test-tenant");
        let policy = make_unlimited_policy();

        manager.add_tenant(tenant_id.clone(), policy).unwrap();

        let status = manager.get_tenant_status(&tenant_id);
        assert!(status.is_ok());
        let status = status.unwrap();
        assert_eq!(status.tenant_id, "test-tenant");
    }

    #[test]
    fn test_tenant_unregistration() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant_id = TenantId::new("test-tenant");
        let policy = make_unlimited_policy();

        manager.add_tenant(tenant_id.clone(), policy).unwrap();
        manager.remove_tenant(&tenant_id).unwrap();

        let status = manager.get_tenant_status(&tenant_id);
        assert!(status.is_err());
    }

    #[test]
    fn test_duplicate_tenant_fails() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant_id = TenantId::new("test-tenant");
        let policy = make_unlimited_policy();

        manager
            .add_tenant(tenant_id.clone(), policy.clone())
            .unwrap();
        let result = manager.add_tenant(tenant_id.clone(), policy);

        assert!(result.is_err());
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Bandwidth Policy Types
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_unlimited_policy() {
        let policy = make_unlimited_policy();
        assert_eq!(policy.max_upload_bps, 0);
        assert_eq!(policy.max_download_bps, 0);
    }

    #[test]
    fn test_rate_limited_policy() {
        let policy = make_rate_limited_policy(1_000_000, 2_000_000);
        assert_eq!(policy.max_upload_bps, 1_000_000);
        assert_eq!(policy.max_download_bps, 2_000_000);
    }

    #[test]
    fn test_policy_with_priority() {
        let policy =
            make_rate_limited_policy(1_000_000, 2_000_000).with_priority(TenantPriority::High);
        assert_eq!(policy.priority, TenantPriority::High);
    }

    #[test]
    fn test_policy_with_burst() {
        let policy = make_rate_limited_policy(1_000_000, 2_000_000).with_burst(2.0);
        assert_eq!(policy.burst_multiplier, 2.0);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Fair Share Calculation
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_fair_share_with_equal_priority_tenants() {
        let limits = make_global_limits(100_000_000, 100_000_000); // 100 MB/s
        let manager = TenantBandwidthManager::new(limits);

        // Register two tenants with same priority
        let tenant1 = TenantId::new("tenant1");
        let tenant2 = TenantId::new("tenant2");

        let policy = make_rate_limited_policy(50_000_000, 50_000_000);
        manager.add_tenant(tenant1.clone(), policy.clone()).unwrap();
        manager.add_tenant(tenant2.clone(), policy).unwrap();

        // Get fair share
        let (share1, _share2) = manager.calculate_fair_share(&tenant1).unwrap();

        // Fair share should be positive
        assert!(share1 > 0);
    }

    #[test]
    fn test_fair_share_with_different_priorities() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant_high = TenantId::new("high-priority");
        let tenant_low = TenantId::new("low-priority");

        let high_policy =
            make_rate_limited_policy(50_000_000, 50_000_000).with_priority(TenantPriority::High);

        let low_policy =
            make_rate_limited_policy(50_000_000, 50_000_000).with_priority(TenantPriority::Low);

        manager
            .add_tenant(tenant_high.clone(), high_policy)
            .unwrap();
        manager.add_tenant(tenant_low.clone(), low_policy).unwrap();

        let (high_share, _) = manager.calculate_fair_share(&tenant_high).unwrap();
        let (_, low_share) = manager.calculate_fair_share(&tenant_low).unwrap();

        // High priority should get at least as much as low priority
        assert!(high_share >= low_share);
    }

    #[test]
    fn test_fair_share_nonexistent_tenant() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant = TenantId::new("nonexistent");
        let result = manager.calculate_fair_share(&tenant);

        assert!(result.is_err());
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Global Status
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_global_status_limits() {
        let limits = make_global_limits(1_000_000_000, 1_000_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let status = manager.get_global_status();

        assert_eq!(status.limits.usable_upload_bps(), 1_000_000_000);
        assert_eq!(status.limits.usable_download_bps(), 1_000_000_000);
        assert_eq!(status.active_tenants, 0);
    }

    #[test]
    fn test_global_status_with_tenants() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant = TenantId::new("test-tenant");
        let policy = make_unlimited_policy();
        manager.add_tenant(tenant, policy).unwrap();

        let status = manager.get_global_status();
        assert_eq!(status.active_tenants, 1);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Tenant Priority Levels
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_tenant_priority_ordering() {
        // Test that priorities can be compared
        assert!(TenantPriority::Critical > TenantPriority::High);
        assert!(TenantPriority::High > TenantPriority::Normal);
        assert!(TenantPriority::Normal > TenantPriority::Low);
    }

    #[test]
    fn test_tenant_priority_default() {
        let priority = TenantPriority::default();
        assert_eq!(priority, TenantPriority::Normal);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Tenant Status
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_tenant_status_active() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant_id = TenantId::new("active-tenant");
        let policy = make_unlimited_policy();
        manager.add_tenant(tenant_id.clone(), policy).unwrap();

        let status = manager.get_tenant_status(&tenant_id);
        assert!(status.is_ok());

        let status = status.unwrap();
        assert_eq!(status.tenant_id, "active-tenant");
    }

    #[test]
    fn test_tenant_status_inactive() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant_id = TenantId::new("inactive-tenant");
        let status = manager.get_tenant_status(&tenant_id);

        // Non-existent tenant should return error
        assert!(status.is_err());
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: List Tenants
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_list_tenants() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);
        let policy = make_unlimited_policy();

        // Register multiple tenants
        for i in 0..5 {
            let tenant_id = TenantId::new(&format!("tenant-{}", i));
            manager.add_tenant(tenant_id, policy.clone()).unwrap();
        }

        let tenants = manager.list_tenants();
        assert_eq!(tenants.len(), 5);
    }

    #[test]
    fn test_list_tenants_empty() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenants = manager.list_tenants();
        assert!(tenants.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Bandwidth Direction
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_bandwidth_direction() {
        let upload = BandwidthDirection::Upload;
        let download = BandwidthDirection::Download;

        assert_ne!(upload, download);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: TenantId
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_tenant_id_creation() {
        let id = TenantId::new("test-tenant");
        assert_eq!(id.0, "test-tenant");
    }

    #[test]
    fn test_tenant_id_equality() {
        let id1 = TenantId::new("tenant1");
        let id2 = TenantId::new("tenant1");
        let id3 = TenantId::new("tenant2");

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Global Bandwidth Limits
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_global_bandwidth_limits() {
        let limits = GlobalBandwidthLimits::new(1_000_000_000, 500_000_000, 0);

        assert_eq!(limits.usable_upload_bps(), 1_000_000_000);
        assert_eq!(limits.usable_download_bps(), 500_000_000);
    }

    #[test]
    fn test_global_bandwidth_limits_with_reserved() {
        let limits = GlobalBandwidthLimits::new(
            1_000_000_000, // total upload
            500_000_000,   // total download
            200_000_000,   // reserved
        );

        assert_eq!(limits.usable_upload_bps(), 800_000_000); // 1000 - 200
        assert_eq!(limits.usable_download_bps(), 300_000_000); // 500 - 200
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Update Limits
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_update_limits() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let new_limits = make_global_limits(200_000_000, 150_000_000);
        manager.update_limits(new_limits);

        let status = manager.get_global_status();
        assert_eq!(status.limits.usable_upload_bps(), 200_000_000);
        assert_eq!(status.limits.usable_download_bps(), 150_000_000);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Total Bytes Tracking
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_total_bytes_initially_zero() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let (upload, download) = manager.get_total_bytes();
        assert_eq!(upload, 0);
        assert_eq!(download, 0);
    }

    #[test]
    fn test_active_transfers_initially_zero() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let count = manager.get_active_transfers();
        assert_eq!(count, 0);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Update Policy
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_update_policy() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant = TenantId::new("test-tenant");
        let policy = make_unlimited_policy();
        manager.add_tenant(tenant.clone(), policy).unwrap();

        let new_policy = make_rate_limited_policy(50_000_000, 50_000_000);
        manager.update_policy(&tenant, new_policy).unwrap();

        let status = manager.get_policy(&tenant).unwrap();
        assert_eq!(status.max_upload_bps, 50_000_000);
    }

    // ─────────────────────────────────────────────────────────────────
    // Test: Get Policy
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_get_policy() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant = TenantId::new("test-tenant");
        let policy = make_rate_limited_policy(75_000_000, 60_000_000);
        manager.add_tenant(tenant.clone(), policy).unwrap();

        let retrieved = manager.get_policy(&tenant).unwrap();
        assert_eq!(retrieved.max_upload_bps, 75_000_000);
        assert_eq!(retrieved.max_download_bps, 60_000_000);
    }

    #[test]
    fn test_get_policy_nonexistent() {
        let limits = make_global_limits(100_000_000, 100_000_000);
        let manager = TenantBandwidthManager::new(limits);

        let tenant = TenantId::new("nonexistent");
        let result = manager.get_policy(&tenant);

        assert!(result.is_err());
    }
}
