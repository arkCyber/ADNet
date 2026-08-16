//! Storage topology and scope management.
//!
//! This module provides the public API for storage scoping, quota
//! management, and topology-level operations that the `a3net-cli`
//! storage subcommands depend on.
//!
//! ## Features
//!
//! - **Scope Management**: Private and shared storage scopes
//! - **Quota Policy**: Configurable storage quotas per scope
//! - **Persistence**: Quota and scope metadata are persisted to disk
//! - **Usage Tracking**: Real-time storage usage statistics

use std::path::Path;
use serde::{Deserialize, Serialize};

use a3net_types::ContentHash;

use crate::store::BlobStore;

/// A byte range with start and end bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// Scoping boundary for a blob within the local store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlobStoreScope {
    Private,
    Shared,
}

impl std::fmt::Display for BlobStoreScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Private => write!(f, "private"),
            Self::Shared => write!(f, "shared"),
        }
    }
}

/// Policy for how the total storage budget is split between scopes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaPolicy {
    /// Fraction of total bytes allocated to the private scope.
    pub private_fraction: f64,
    /// Fraction of total bytes allocated to the shared scope.
    pub shared_fraction: f64,
    /// Hard cap on private scope in bytes.
    pub private_hard_cap: Option<u64>,
    /// Hard cap on shared scope in bytes.
    pub shared_hard_cap: Option<u64>,
    /// Whether the shared scope is sealed (no further writes from the CLI).
    pub sealed: bool,
    /// Unix timestamp (ms) when the shared scope was sealed.
    pub sealed_at_unix_ms: Option<i64>,
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self::default_split(20u64 * 1024 * 1024 * 1024)
    }
}

impl QuotaPolicy {
    /// Build a `QuotaPolicy` that splits `total_bytes` between scopes
    /// using the standard 50/50 fraction split.
    pub fn default_split(total_bytes: u64) -> Self {
        let private = total_bytes / 2;
        let shared = total_bytes / 2;
        Self {
            private_fraction: 0.5,
            shared_fraction: 0.5,
            private_hard_cap: Some(private),
            shared_hard_cap: Some(shared),
            sealed: false,
            sealed_at_unix_ms: None,
        }
    }
}

/// Per-scope usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopologyUsage {
    pub total_bytes: u64,
    pub private_used: u64,
    pub private_budget: u64,
    pub private_hard_cap: u64,
    pub shared_used: u64,
    pub shared_budget: u64,
    pub shared_hard_cap: u64,
}

/// Errors that can occur during topology operations.
#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("store error: {0}")]
    Store(String),
    #[error("scope error: {0}")]
    Scope(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<String> for TopologyError {
    fn from(s: String) -> Self {
        Self::Scope(s)
    }
}

impl From<&str> for TopologyError {
    fn from(s: &str) -> Self {
        Self::Scope(s.to_string())
    }
}

/// Shared (sealed) store handle exposed by the topology.
///
/// Manages the shared scope with persistent storage.
#[derive(Debug, Clone)]
pub struct SharedStoreHandle {
    store: Option<BlobStore>,
    sealed: bool,
}

impl SharedStoreHandle {
    /// Create a new shared store handle.
    fn new(shared_dir: std::path::PathBuf) -> Self {
        // Try to create the store, but don't fail if shared is sealed
        let store = if shared_dir.join("sealed").exists() {
            None // Sealed, don't allow writes
        } else {
            BlobStore::new(&shared_dir).ok()
        };
        
        Self {
            store,
            sealed: shared_dir.join("sealed").exists(),
        }
    }

    pub fn list_complete(&self) -> Result<Vec<ContentHash>, TopologyError> {
        match &self.store {
            Some(store) => store.list_complete().map_err(|e| TopologyError::Store(e.to_string())),
            None => Ok(Vec::new()),
        }
    }

    pub fn meta(&self, hash: &ContentHash) -> Option<(u64, std::time::SystemTime)> {
        self.store.as_ref().and_then(|s| {
            s.meta(hash).ok().map(|(size, _count)| (size, std::time::SystemTime::now()))
        })
    }

    /// Wipe the sealed shared scope. Returns the number of blobs removed.
    /// Only works if the shared scope is sealed.
    pub fn wipe_admin(&self) -> Result<usize, TopologyError> {
        if !self.sealed {
            return Err(TopologyError::Scope("Shared scope is not sealed".to_string()));
        }
        // List all blobs and remove them
        let blobs = self.list_complete()?;
        let count = blobs.len();
        if let Some(store) = &self.store {
            for blob in blobs {
                let _ = store.remove(&blob);
            }
        }
        Ok(count)
    }

    /// Check if a blob is complete.
    pub fn has_complete(&self, hash: &ContentHash) -> bool {
        self.store.as_ref().map(|s| s.has_complete(hash)).unwrap_or(false)
    }

    /// Read a range of bytes from a blob (verified).
    pub fn read_range_sync_verified(
        &self,
        hash: &ContentHash,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, TopologyError> {
        let range = a3net_types::ByteRange {
            start: offset,
            end: offset.saturating_add(len as u64),
        };
        match &self.store {
            Some(store) => store.read_range_sync(hash, &range)
                .map_err(|e| TopologyError::Store(e.to_string())),
            None => Err(TopologyError::Scope("Shared scope is sealed".to_string())),
        }
    }
    
    /// Get total size of shared storage.
    pub fn usage(&self) -> Result<u64, TopologyError> {
        match &self.store {
            Some(store) => store.total_size().map_err(|e| TopologyError::Store(e.to_string())),
            None => Ok(0),
        }
    }
}

/// Storage topology managing private and shared blob stores.
///
/// This implementation provides:
/// - Persistent quota policy stored in `quota.json`
/// - Real BlobStore instances for private and shared scopes
/// - Usage tracking and reporting
#[derive(Debug, Clone)]
pub struct StorageTopology {
    /// Root data directory.
    pub data_dir: std::path::PathBuf,
    /// Effective quota policy.
    pub quota: QuotaPolicy,
    /// The private-scoped blob store.
    pub private: BlobStore,
    /// Handle to the sealed shared store.
    shared_handle: SharedStoreHandle,
}

/// Metadata for storage topology persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TopologyMetadata {
    quota: QuotaPolicy,
    version: String,
    created_at_unix_ms: i64,
}

impl TopologyMetadata {
    fn new(quota: QuotaPolicy) -> Self {
        Self {
            quota,
            version: "1.0".to_string(),
            created_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        }
    }
}

/// Shared fractions used by `build_dashboard` constants.
pub const DEFAULT_PRIVATE_FRACTION: f64 = 0.5;
pub const DEFAULT_SHARED_FRACTION: f64 = 0.5;

impl StorageTopology {
    /// Open (or create) a storage topology rooted at `data_dir`.
    ///
    /// Creates private and shared subdirectories and persists quota policy.
    pub fn open(data_dir: impl AsRef<Path>, quota: QuotaPolicy) -> Result<Self, TopologyError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        
        // Create subdirectories
        let private_dir = data_dir.join("private");
        let shared_dir = data_dir.join("shared");
        
        std::fs::create_dir_all(&private_dir)?;
        std::fs::create_dir_all(&shared_dir)?;
        
        // Create or load metadata
        let metadata_path = data_dir.join("topology.json");
        let effective_quota = if metadata_path.exists() {
            let data = std::fs::read_to_string(&metadata_path)?;
            let meta: TopologyMetadata = serde_json::from_str(&data)
                .map_err(|e| TopologyError::Store(e.to_string()))?;
            meta.quota
        } else {
            // Save new metadata
            let meta = TopologyMetadata::new(quota.clone());
            let data = serde_json::to_string_pretty(&meta)
                .map_err(|e| TopologyError::Store(e.to_string()))?;
            std::fs::write(&metadata_path, data)?;
            quota
        };
        
        // Create blob stores
        let private = BlobStore::new(&private_dir)
            .map_err(|e| TopologyError::Store(e.to_string()))?;
        
        Ok(Self {
            data_dir,
            quota: effective_quota,
            private,
            shared_handle: SharedStoreHandle::new(shared_dir),
        })
    }

    /// Return per-scope usage statistics.
    pub fn usage(&self) -> Result<TopologyUsage, TopologyError> {
        let private_used = self.private.total_size()
            .map_err(|e| TopologyError::Store(e.to_string()))?;
        
        let shared_used = self.shared_handle.usage()?;
        
        Ok(TopologyUsage {
            total_bytes: self.quota.private_hard_cap.unwrap_or(0)
                + self.quota.shared_hard_cap.unwrap_or(0),
            private_used,
            private_budget: self.quota.private_hard_cap.unwrap_or(0),
            private_hard_cap: self.quota.private_hard_cap.unwrap_or(0),
            shared_used,
            shared_budget: self.quota.shared_hard_cap.unwrap_or(0),
            shared_hard_cap: self.quota.shared_hard_cap.unwrap_or(0),
        })
    }

    /// Return a borrow to the private-scoped blob store.
    pub fn store(&self, _scope: BlobStoreScope) -> &BlobStore {
        &self.private
    }

    /// Return a borrow to the sealed shared store handle.
    pub fn shared_store(&self) -> &SharedStoreHandle {
        &self.shared_handle
    }
    
    /// Check if adding a blob would exceed the private quota.
    pub fn can_store_private(&self, size: u64) -> Result<bool, TopologyError> {
        let usage = self.usage()?;
        Ok(usage.private_used + size <= usage.private_hard_cap)
    }
    
    /// Check if adding a blob would exceed the shared quota.
    pub fn can_store_shared(&self, size: u64) -> Result<bool, TopologyError> {
        let usage = self.usage()?;
        Ok(usage.shared_used + size <= usage.shared_hard_cap)
    }
    
    /// Save quota policy to disk.
    pub fn save_quota(&self) -> Result<(), TopologyError> {
        let metadata_path = self.data_dir.join("topology.json");
        let meta = TopologyMetadata::new(self.quota.clone());
        let data = serde_json::to_string_pretty(&meta)
            .map_err(|e| TopologyError::Store(e.to_string()))?;
        std::fs::write(&metadata_path, data)?;
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────
    //  Garbage collection — drive `BlobStore::gc_*` from a single
    //  PinSet so both scopes are swept in one pass.
    // ─────────────────────────────────────────────────────────────────

    /// Prune every blob (across both scopes) that is **not** in
    /// `pins`. Returns the per-scope deletion lists. Failures
    /// against individual blobs are logged but don't abort the
    /// pass — see [`crate::store::BlobStore::gc_orphans`] for the
    /// per-store contract.
    ///
    /// The shared scope is intentionally **excluded** from the
    /// pass: the audit policy keeps shared blobs locked behind
    /// the explicit replication path, so an orphan-sweep there
    /// would silently delete content the operator believes is
    /// preserved. If you really want to clean shared, run
    /// `gc_all` or wipe it explicitly via `SharedStoreHandle::wipe_admin`.
    pub fn gc_orphans(
        &self,
        pins: &crate::pin_set::PinSet,
    ) -> Result<TopologyGcReport, TopologyError> {
        let private_removed = self
            .private
            .gc_orphans(pins)
            .map_err(|e| TopologyError::Store(e.to_string()))?;
        Ok(TopologyGcReport {
            private_removed,
            shared_removed: Vec::new(),
        })
    }

    /// Like [`gc_orphans`] but the caller passes the pinned set
    /// as a hex-iterable — used by the CLI's
    /// `repo gc --prune-unpinned` path which already has the
    /// pin set in memory.
    pub fn gc_unpinned<I, S>(
        &self,
        pinned: I,
    ) -> Result<TopologyGcReport, TopologyError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let private_removed = self
            .private
            .gc_unpinned(pinned)
            .map_err(|e| TopologyError::Store(e.to_string()))?;
        Ok(TopologyGcReport {
            private_removed,
            shared_removed: Vec::new(),
        })
    }

    /// Operator's "reset" button. Drops every blob from the
    /// private scope. The shared scope is **not** touched —
    /// operators must seal-and-wipe it explicitly. Returns the
    /// number of blobs deleted.
    pub fn gc_all_private(&self) -> Result<Vec<ContentHash>, TopologyError> {
        self.private
            .gc_all()
            .map_err(|e| TopologyError::Store(e.to_string()))
    }
}

/// Per-scope summary of a GC pass. The shared list is always
/// empty in v1 (see [`StorageTopology::gc_orphans`]) but the
/// shape is preserved so a future "shared GC" PR can populate
/// it without changing the call sites.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopologyGcReport {
    pub private_removed: Vec<ContentHash>,
    pub shared_removed: Vec<ContentHash>,
}

impl TopologyGcReport {
    /// Total blobs pruned across both scopes.
    pub fn total(&self) -> usize {
        self.private_removed.len() + self.shared_removed.len()
    }
}
