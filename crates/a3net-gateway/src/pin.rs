//! Pin service for persistent storage management.
//!
//! This module provides the pin service that handles:
//! - Pinning content to prevent garbage collection
//! - Recursive pinning of entire DAGs
//! - Pin metadata and status tracking
//! - Garbage collection coordination
//!
//! ## Pin Types
//!
//! | Type | Description |
//! |------|-------------|
//! | Direct | Pin a single block |
//! | Recursive | Pin a CID and all its descendants |
//! | Indirect | Pin that was added via a parent recursive pin |
//! | All | Legacy alias for recursive |

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_types::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

/// Pin status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinStatus {
    /// Pin is queued.
    Queued,
    /// Pin is in progress.
    Pinned,
    /// Pin failed.
    Failed,
    /// Pin is being unpinned.
    Unpinned,
}

impl std::fmt::Display for PinStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinStatus::Queued => write!(f, "queued"),
            PinStatus::Pinned => write!(f, "pinned"),
            PinStatus::Failed => write!(f, "failed"),
            PinStatus::Unpinned => write!(f, "unpinned"),
        }
    }
}

/// Pin type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinType {
    /// Direct pin (single block).
    Direct,
    /// Recursive pin (entire DAG).
    Recursive,
    /// Indirect pin (descendant of recursive pin).
    Indirect,
    /// All (legacy, equivalent to recursive).
    All,
}

impl std::fmt::Display for PinType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinType::Direct => write!(f, "direct"),
            PinType::Recursive => write!(f, "recursive"),
            PinType::Indirect => write!(f, "indirect"),
            PinType::All => write!(f, "all"),
        }
    }
}

/// Pin information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinInfo {
    /// The pinned CID.
    pub cid: ContentHash,
    /// Type of pin.
    pub pin_type: PinType,
    /// Pin status.
    pub status: PinStatus,
    /// When the pin was created.
    pub created: std::time::SystemTime,
    /// When the pin was last updated.
    pub updated: std::time::SystemTime,
    /// Reference count (number of pin sources).
    pub refs: u32,
    /// Error message if status is Failed.
    pub error: Option<String>,
}

impl PinInfo {
    /// Check if the pin is active (pinned or queued).
    pub fn is_active(&self) -> bool {
        matches!(self.status, PinStatus::Pinned | PinStatus::Queued)
    }
}

/// Pin service errors.
#[derive(Debug, Error)]
pub enum PinError {
    #[error("pin not found: {0}")]
    NotFound(String),

    #[error("already pinned: {0}")]
    AlreadyPinned(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("pin operation failed: {0}")]
    PinFailed(String),
}

/// Pin service for managing pins.
#[derive(Clone)]
pub struct PinService {
    blob_store: Arc<BlobStore>,
    /// Pin database: CID -> PinInfo
    pins: Arc<RwLock<HashMap<ContentHash, PinInfo>>>,
    /// Reverse index: which pins reference a given CID
    pin_refs: Arc<RwLock<HashMap<ContentHash, Vec<ContentHash>>>>,
    /// Path to the pin database file.
    db_path: PathBuf,
}

impl PinService {
    /// Create a new pin service.
    pub fn new(blob_store: Arc<BlobStore>, data_dir: PathBuf) -> Self {
        let db_path = data_dir.join("pins.json");
        Self {
            blob_store,
            pins: Arc::new(RwLock::new(HashMap::new())),
            pin_refs: Arc::new(RwLock::new(HashMap::new())),
            db_path,
        }
    }

    /// Load pin database from disk.
    pub async fn load(&self) -> Result<(), PinError> {
        if !self.db_path.exists() {
            return Ok(());
        }

        let data = tokio::fs::read(&self.db_path)
            .await
            .map_err(|e| PinError::Internal(e.to_string()))?;

        let loaded: HashMap<String, PinInfo> = serde_json::from_slice(&data)
            .map_err(|e| PinError::Internal(e.to_string()))?;

        let mut pins = self.pins.write().await;
        let mut pin_refs = self.pin_refs.write().await;

        for (cid_str, info) in loaded {
            if let Ok(cid) = ContentHash::from_hex(&cid_str) {
                pins.insert(cid.clone(), info.clone());
                // Rebuild reverse index
                if info.pin_type == PinType::Recursive {
                    pin_refs.insert(cid, Vec::new());
                }
            }
        }

        Ok(())
    }

    /// Save pin database to disk.
    pub async fn save(&self) -> Result<(), PinError> {
        let pins = self.pins.read().await;
        let mut serializable: HashMap<String, PinInfo> = HashMap::new();

        for (cid, info) in pins.iter() {
            serializable.insert(cid.as_hex().to_string(), info.clone());
        }

        let data = serde_json::to_vec_pretty(&serializable)
            .map_err(|e| PinError::Internal(e.to_string()))?;

        tokio::fs::write(&self.db_path, data)
            .await
            .map_err(|e| PinError::Internal(e.to_string()))?;

        Ok(())
    }

    /// Add a pin for a CID.
    pub async fn add_pin(&self, cid: &ContentHash, recursive: bool) -> Result<(), PinError> {
        // Check if content exists
        if !self.blob_store.has_complete(cid) {
            return Err(PinError::PinFailed(format!(
                "content not found: {}",
                cid.as_hex()
            )));
        }

        let now = std::time::SystemTime::now();
        let pin_type = if recursive { PinType::Recursive } else { PinType::Direct };

        let mut pins = self.pins.write().await;

        if let Some(existing) = pins.get_mut(cid) {
            // Update existing pin
            if existing.pin_type == PinType::Recursive && recursive {
                return Err(PinError::AlreadyPinned(cid.as_hex().to_string()));
            }
            existing.pin_type = pin_type;
            existing.status = PinStatus::Pinned;
            existing.updated = now;
            existing.refs += 1;
        } else {
            // Create new pin
            pins.insert(cid.clone(), PinInfo {
                cid: cid.clone(),
                pin_type,
                status: PinStatus::Pinned,
                created: now,
                updated: now,
                refs: 1,
                error: None,
            });
        }

        if recursive {
            // For recursive pins, we need to traverse the DAG
            // and mark all descendants as indirect pins
            drop(pins);
            self.mark_descendants(cid).await?;
        } else {
            // Update reverse index for direct pin
            let mut pin_refs = self.pin_refs.write().await;
            pin_refs.entry(cid.clone()).or_insert_with(Vec::new);
        }

        Ok(())
    }

    /// Mark all descendants of a CID as indirectly pinned.
    async fn mark_descendants(&self, cid: &ContentHash) -> Result<(), PinError> {
        let mut to_visit = vec![cid.clone()];
        let mut visited = std::collections::HashSet::new();
        let now = std::time::SystemTime::now();

        while let Some(current) = to_visit.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            // Add to reverse index
            {
                let mut pin_refs = self.pin_refs.write().await;
                let refs = pin_refs.entry(current.clone()).or_insert_with(Vec::new);
                if !refs.contains(cid) {
                    refs.push(cid.clone());
                }
            }

            // Get links from DAG
            if let Ok(Some(dag_links)) = self.get_dag_links(&current).await {
                for link in dag_links {
                    if let Ok(link_cid) = ContentHash::from_hex(&link.hash) {
                        to_visit.push(link_cid);
                    }
                }
            }

            // Add indirect pin
            let mut pins = self.pins.write().await;
            if !pins.contains_key(&current) {
                pins.insert(current.clone(), PinInfo {
                    cid: current.clone(),
                    pin_type: PinType::Indirect,
                    status: PinStatus::Pinned,
                    created: now,
                    updated: now,
                    refs: 0,
                    error: None,
                });
            }
        }

        Ok(())
    }

    /// Get links from a DAG node.
    async fn get_dag_links(&self, cid: &ContentHash) -> Result<Option<Vec<super::dag::DagLink>>, PinError> {
        let data = self.blob_store.get_sync(cid);
        if data.is_none() {
            return Ok(None);
        }

        let data = data.unwrap();

        // Try to parse as CBOR DAG node
        if let Ok(node) = serde_cbor::from_slice::<super::dag::DagNode>(&data) {
            return Ok(Some(node.links));
        }

        Ok(None)
    }

    /// Remove a pin for a CID.
    pub async fn remove_pin(&self, cid: &ContentHash) -> Result<(), PinError> {
        let mut pins = self.pins.write().await;

        if let Some(info) = pins.get_mut(cid) {
            if info.refs > 1 {
                info.refs -= 1;
                return Ok(());
            }

            if info.pin_type == PinType::Recursive {
                // For recursive pins, remove indirect pins too
                let to_remove = self.find_indirect_pins_locked(cid, &pins).await;
                for remove_cid in to_remove {
                    pins.remove(&remove_cid);
                }
            }

            pins.remove(cid);
            Ok(())
        } else {
            Err(PinError::NotFound(cid.as_hex().to_string()))
        }
    }

    /// Find all indirect pins that are descendants of a recursive pin.
    async fn find_indirect_pins_locked(
        &self,
        root: &ContentHash,
        pins: &HashMap<ContentHash, PinInfo>,
    ) -> Vec<ContentHash> {
        let mut to_visit = vec![root.clone()];
        let mut visited = std::collections::HashSet::new();
        let mut indirect_pins = Vec::new();

        while let Some(current) = to_visit.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if current != *root
                && pins.contains_key(&current) {
                    indirect_pins.push(current.clone());
                }

            if let Ok(Some(dag_links)) = self.get_dag_links(&current).await {
                for link in dag_links {
                    if let Ok(link_cid) = ContentHash::from_hex(&link.hash) {
                        to_visit.push(link_cid);
                    }
                }
            }
        }

        indirect_pins
    }

    /// List all pins, optionally filtered by CID.
    pub async fn list_pins(&self, filter: Option<&ContentHash>) -> Vec<PinInfo> {
        let pins = self.pins.read().await;

        match filter {
            Some(cid) => {
                pins.get(cid)
                    .map(|info| vec![info.clone()])
                    .unwrap_or_default()
            }
            None => {
                pins.values()
                    .filter(|info| info.pin_type != PinType::Indirect)
                    .cloned()
                    .collect()
            }
        }
    }

    /// Check if a CID is pinned.
    pub async fn is_pinned(&self, cid: &ContentHash) -> bool {
        let pins = self.pins.read().await;
        pins.contains_key(cid)
    }

    /// Check if a CID can be garbage collected.
    pub async fn can_gc(&self, cid: &ContentHash) -> bool {
        let pins = self.pins.read().await;
        !pins.contains_key(cid)
    }

    /// Get all CIDs eligible for garbage collection.
    pub async fn get_gc_candidates(&self) -> Vec<ContentHash> {
        let pins = self.pins.read().await;
        let all_blobs = self.blob_store.list_complete().unwrap_or_default();

        all_blobs
            .into_iter()
            .filter(|cid| !pins.contains_key(cid))
            .collect()
    }

    /// Get pin statistics.
    pub async fn stats(&self) -> PinStats {
        let pins = self.pins.read().await;

        let mut direct = 0u32;
        let mut recursive = 0u32;
        let mut indirect = 0u32;

        for info in pins.values() {
            match info.pin_type {
                PinType::Direct => direct += 1,
                PinType::Recursive => recursive += 1,
                PinType::Indirect => indirect += 1,
                PinType::All => recursive += 1,
            }
        }

        PinStats {
            direct,
            recursive,
            indirect,
            total: pins.len() as u32,
        }
    }
}

/// Pin statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinStats {
    /// Number of direct pins.
    pub direct: u32,
    /// Number of recursive pins.
    pub recursive: u32,
    /// Number of indirect pins.
    pub indirect: u32,
    /// Total number of pins.
    pub total: u32,
}

/// Garbage collection service.
pub struct GcService {
    blob_store: Arc<BlobStore>,
    pin_service: Arc<PinService>,
}

impl GcService {
    /// Create a new GC service.
    pub fn new(blob_store: Arc<BlobStore>, pin_service: Arc<PinService>) -> Self {
        Self { blob_store, pin_service }
    }

    /// Run garbage collection.
    /// Returns the number of blocks removed.
    pub async fn run(&self) -> Result<GcResult, PinError> {
        let candidates = self.pin_service.get_gc_candidates().await;
        let mut removed = 0u64;
        let mut failed = 0u64;

        for cid in candidates {
            match self.blob_store.remove(&cid) {
                Ok(true) => removed += 1,
                Ok(false) => {}
                Err(_) => failed += 1,
            }
        }

        Ok(GcResult { removed, failed })
    }

    /// Get the number of blocks that would be removed.
    pub async fn dry_run(&self) -> Result<u64, PinError> {
        let candidates = self.pin_service.get_gc_candidates().await;
        Ok(candidates.len() as u64)
    }
}

/// Result of a GC run.
#[derive(Debug, Clone)]
pub struct GcResult {
    /// Number of blocks removed.
    pub removed: u64,
    /// Number of blocks that failed to remove.
    pub failed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pin_service() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            a3net_blobstore::BlobStore::new(dir.path()).unwrap()
        );

        let pin_service = PinService::new(
            blob_store.clone(),
            dir.path().to_path_buf()
        );

        // Create a test blob
        let (cid, _) = blob_store.put_bytes_sync(b"test data").unwrap();

        // Add a pin
        pin_service.add_pin(&cid, true).await.unwrap();

        // Check it's pinned
        assert!(pin_service.is_pinned(&cid).await);

        // Check stats
        let stats = pin_service.stats().await;
        assert_eq!(stats.direct, 0);
        assert_eq!(stats.recursive, 1);

        // Remove the pin
        pin_service.remove_pin(&cid).await.unwrap();

        // Check it's no longer pinned
        assert!(!pin_service.is_pinned(&cid).await);
    }

    #[tokio::test]
    async fn test_pin_refcount() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            a3net_blobstore::BlobStore::new(dir.path()).unwrap()
        );

        let pin_service = PinService::new(
            blob_store.clone(),
            dir.path().to_path_buf()
        );

        // Create a test blob
        let (cid, _) = blob_store.put_bytes_sync(b"test data").unwrap();

        // Add multiple pins
        pin_service.add_pin(&cid, false).await.unwrap();
        pin_service.add_pin(&cid, false).await.unwrap();

        // Check it's pinned and ref count is 2
        let pins = pin_service.list_pins(Some(&cid)).await;
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].refs, 2);

        // Remove once
        pin_service.remove_pin(&cid).await.unwrap();
        let pins = pin_service.list_pins(Some(&cid)).await;
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].refs, 1);

        // Remove again
        pin_service.remove_pin(&cid).await.unwrap();
        assert!(!pin_service.is_pinned(&cid).await);
    }
}
