//! Multi-Tenant Bandwidth Control for NAS
//!
//! This module provides bandwidth isolation and control for multi-tenant NAS
//! environments, ensuring that P2P transfers don't affect normal business operations.
//!
//! ## Key Concepts
//!
//! - **Tenant**: A distinct user or user group with dedicated bandwidth allocation
//! - **Bandwidth Policy**: Per-tenant upload/download limits
//! - **Token Bucket**: Efficient rate limiting algorithm for smooth bandwidth control
//! - **Global Limits**: System-wide bandwidth caps that apply to all tenants
//!
//! ## Usage
//!
//! ```rust,ignore
//! use a3net_blobstore::bandwidth::{TenantBandwidthManager, BandwidthPolicy};
//!
//! // Create a bandwidth manager with global limits
//! let mut manager = TenantBandwidthManager::new(
//!     GlobalBandwidthLimits {
//!         max_upload_bps: 100 * 1024 * 1024,  // 100 MB/s
//!         max_download_bps: 200 * 1024 * 1024, // 200 MB/s
//!         reserved_for_system_bps: 50 * 1024 * 1024, // 50 MB/s reserved
//!     }
//! );
//!
//! // Register tenants
//! manager.add_tenant(TenantId::new("tenant_a"), BandwidthPolicy {
//!     max_upload_bps: 10 * 1024 * 1024,
//!     max_download_bps: 50 * 1024 * 1024,
//!     priority: TenantPriority::Normal,
//! });
//!
//! // Acquire bandwidth before transfer
//! let permit = manager.acquire_upload(&TenantId::new("tenant_a"), 1024 * 1024).await?;
//! // ... perform transfer ...
//! permit.release();
//! ```

pub mod manager;
pub mod metrics;
pub mod policy;
pub mod token_bucket;

pub use manager::{BandwidthError, BandwidthPermit, BandwidthResult, TenantBandwidthManager};
pub use metrics::BandwidthMetrics;
pub use policy::{
    BandwidthDirection, BandwidthPolicy, GlobalBandwidthLimits, GlobalBandwidthStatus,
    TenantBandwidthStatus, TenantId, TenantPriority,
};
pub use token_bucket::TokenBucket;
