//! CID-aware DAG block store adapter for GraphSync.
//!
//! This module bridges the gap between:
//! - `BlobStore` (stores blobs by [`ContentHash`], uses BLAKE3)
//! - `graphsync::BlockStore` (stores DAG blocks by [`Cid`], extracts links)
//!
//! The adapter layer:
//! 1. Maintains a secondary index: `Cid -> ContentHash` for blocks stored via GraphSync
//! 2. Implements `links()` by decoding DAG-PB / DAG-CBOR to extract child CIDs
//! 3. Handles CID encoding variants (raw, dag-pb, dag-cbor) for proper link extraction

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use adnet_types::cid::{Cid, Codec};
use adnet_types::content::ContentHash;
use adnet_types::dag_codec::extract_links;
use adnet_types::graphsync::BlockStore;
use parking_lot::RwLock;

/// Errors that can occur in the DAG block store.
#[derive(Debug, thiserror::Error)]
pub enum DagBlockStoreError {
    #[error("CID encoding not supported: {0}")]
    UnsupportedCodec(String),

    #[error("content hash not found for CID: {0}")]
    ContentHashNotFound(Cid),

    #[error("blob not found: {0}")]
    BlobNotFound(ContentHash),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("link extraction failed: {0}")]
    LinkExtraction(String),
}

/// Index entry mapping a CID to its stored blob hash.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CidIndex {
    /// Content hash used to store the block data in BlobStore.
    pub content_hash: ContentHash,
    /// Codec used to encode the CID.
    pub codec: Codec,
    /// Size of the stored block in bytes.
    pub block_size: u64,
}

impl CidIndex {
    pub fn new(content_hash: ContentHash, codec: Codec, block_size: u64) -> Self {
        Self {
            content_hash,
            codec,
            block_size,
        }
    }
}

/// DAG-aware block store that wraps a `BlobStore` and provides GraphSync
/// `BlockStore` semantics.
///
/// This adapter:
///
/// - Stores incoming blocks in the underlying `BlobStore` by computing their
///   BLAKE3 content hash.
/// - Maintains a CID index mapping each CID to its content hash.
/// - Decodes blocks on `links()` calls to extract child CIDs based on
///   the block's codec (DAG-PB, DAG-CBOR, DAG-JSON, Raw).
pub struct DagBlockStore {
    /// Underlying blob storage.
    blob_store: Arc<dyn BlobStoreAdapter>,
    /// CID -> ContentHash index.
    cid_index: Arc<RwLock<HashMap<Cid, CidIndex>>>,
    /// Reverse index: ContentHash -> Cids (for efficient listing).
    reverse_index: Arc<RwLock<HashMap<ContentHash, Vec<Cid>>>>,
    /// Base directory for storing the index.
    index_dir: PathBuf,
}

impl std::fmt::Debug for DagBlockStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DagBlockStore")
            .field("index_dir", &self.index_dir)
            .finish()
    }
}

/// Trait for blob storage backends that can be used with `DagBlockStore`.
pub trait BlobStoreAdapter: Send + Sync + 'static {
    /// Check if a blob exists.
    fn has(&self, hash: &ContentHash) -> bool;

    /// Get the full blob data.
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>>;

    /// Store blob data, returning the content hash.
    fn put(&self, data: &[u8]) -> std::io::Result<ContentHash>;

    /// List all known content hashes.
    fn list(&self) -> std::io::Result<Vec<ContentHash>>;

    /// Remove a blob.
    fn remove(&self, hash: &ContentHash) -> std::io::Result<bool>;
}

/// Implementation of `BlobStoreAdapter` for the standard `BlobStore`.
impl BlobStoreAdapter for crate::store::BlobStore {
    fn has(&self, hash: &ContentHash) -> bool {
        self.has_complete(hash)
    }

    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        self.get_sync(hash)
    }

    fn put(&self, data: &[u8]) -> std::io::Result<ContentHash> {
        let (hash, _) = self.put_bytes_sync(data)?;
        Ok(hash)
    }

    fn list(&self) -> std::io::Result<Vec<ContentHash>> {
        self.list_complete()
    }

    fn remove(&self, hash: &ContentHash) -> std::io::Result<bool> {
        self.remove(hash)
    }
}

impl DagBlockStore {
    /// Create a new `DagBlockStore` wrapping the given blob store.
    pub fn new(blob_store: Arc<dyn BlobStoreAdapter>, index_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&index_dir).ok();
        Self {
            blob_store,
            cid_index: Arc::new(RwLock::new(HashMap::new())),
            reverse_index: Arc::new(RwLock::new(HashMap::new())),
            index_dir,
        }
    }

    /// Create from a standard `BlobStore`, deriving the index directory
    /// as `<blob_store.data_dir>/dag-index`.
    pub fn from_blob_store(blob_store: Arc<crate::store::BlobStore>) -> Self {
        let index_dir = blob_store.data_dir().join("dag-index");
        Self::new(blob_store, index_dir)
    }

    /// Load the CID index from disk.
    pub fn load_index(&self) -> std::io::Result<()> {
        let index_path = self.index_dir.join("cid-index.json");
        if !index_path.exists() {
            return Ok(());
        }

        let data = std::fs::read(&index_path)?;
        let index: Vec<(String, CidIndex)> = serde_json::from_slice(&data)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let mut cid_index = self.cid_index.write();
        let mut reverse_index = self.reverse_index.write();

        for (cid_str, idx) in index {
            if let Ok(cid) = Cid::parse(&cid_str) {
                let cid_clone = cid.clone();
                cid_index.insert(cid_clone, idx.clone());
                reverse_index
                    .entry(idx.content_hash.clone())
                    .or_default()
                    .push(cid);
            }
        }
        Ok(())
    }

    /// Persist the CID index to disk.
    pub fn save_index(&self) -> std::io::Result<()> {
        let index_path = self.index_dir.join("cid-index.json");
        let index: Vec<(String, CidIndex)> = self
            .cid_index
            .read()
            .iter()
            .map(|(cid, idx)| (cid.to_string(), idx.clone()))
            .collect();

        let data = serde_json::to_vec_pretty(&index)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&index_path, data)?;
        Ok(())
    }

    /// Get the codec for a CID.
    pub fn get_codec(&self, cid: &Cid) -> Option<Codec> {
        cid.codec()
    }

    /// Get all CIDs indexed in this store.
    pub fn indexed_cids(&self) -> Vec<Cid> {
        self.cid_index.read().keys().cloned().collect()
    }

    /// Get the content hash for a CID, if indexed.
    pub fn content_hash_for_cid(&self, cid: &Cid) -> Option<ContentHash> {
        self.cid_index.read().get(cid).map(|idx| idx.content_hash.clone())
    }

    /// Get the CID for a content hash, if indexed.
    pub fn cid_for_content_hash(&self, hash: &ContentHash) -> Option<Vec<Cid>> {
        self.reverse_index.read().get(hash).cloned()
    }

    /// Check if a CID is indexed.
    pub fn has_cid(&self, cid: &Cid) -> bool {
        self.cid_index.read().contains_key(cid)
    }

    /// Get statistics about the store.
    pub fn stats(&self) -> DagBlockStoreStats {
        let index = self.cid_index.read();
        let cid_count = index.len();
        let total_size: u64 = index.values().map(|idx| idx.block_size).sum();
        let blobs = self.blob_store.list().unwrap_or_default();
        let blob_count = blobs.len();

        DagBlockStoreStats {
            cid_count,
            blob_count,
            total_block_size: total_size,
            index_dir: self.index_dir.clone(),
        }
    }
}

/// Statistics about a `DagBlockStore`.
#[derive(Debug, Clone)]
pub struct DagBlockStoreStats {
    pub cid_count: usize,
    pub blob_count: usize,
    pub total_block_size: u64,
    pub index_dir: PathBuf,
}

impl BlockStore for DagBlockStore {
    fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
        let idx = self.cid_index.read().get(cid)?.clone();
        self.blob_store.get(&idx.content_hash)
    }

    fn put(&self, cid: &Cid, block: &[u8]) {
        // Compute content hash from the block bytes.
        let content_hash = match self.blob_store.put(block) {
            Ok(hash) => hash,
            Err(e) => {
                tracing::warn!("failed to store block {:?}: {}", cid, e);
                return;
            }
        };

        let codec = cid.codec().unwrap_or(Codec::Raw);
        let block_size = block.len() as u64;

        let idx = CidIndex::new(content_hash.clone(), codec, block_size);

        // Update CID index.
        let mut cid_index = self.cid_index.write();
        cid_index.insert(cid.clone(), idx);

        // Update reverse index.
        let mut reverse_index = self.reverse_index.write();
        reverse_index
            .entry(content_hash)
            .or_default()
            .push(cid.clone());

        tracing::debug!(%cid, codec = ?codec, size = block_size, "stored DAG block");
    }

    fn has(&self, cid: &Cid) -> bool {
        let guard = self.cid_index.read();
        let idx = match guard.get(cid) {
            Some(idx) => idx.clone(),
            None => return false,
        };
        drop(guard);
        self.blob_store.has(&idx.content_hash)
    }

    fn links(&self, cid: &Cid) -> Vec<Cid> {
        let block = match self.get(cid) {
            Some(b) => b,
            None => return Vec::new(),
        };

        match extract_links(cid, &block) {
            Ok(refs) => refs.into_iter().map(|r| r.cid).collect(),
            Err(e) => {
                tracing::debug!(%cid, "failed to extract links: {}", e);
                Vec::new()
            }
        }
    }

    fn links_named(&self, cid: &Cid) -> Vec<(Option<String>, Cid)> {
        let block = match self.get(cid) {
            Some(b) => b,
            None => return Vec::new(),
        };

        match extract_links(cid, &block) {
            Ok(refs) => refs
                .into_iter()
                .map(|r| (r.name, r.cid))
                .collect(),
            Err(e) => {
                tracing::debug!(%cid, "failed to extract named links: {}", e);
                Vec::new()
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// In-memory implementation for testing
// ─────────────────────────────────────────────────────────────────

/// In-memory blob store adapter for testing.
#[derive(Debug, Default, Clone)]
pub struct MemBlobStoreAdapter {
    blobs: Arc<std::sync::Mutex<HashMap<ContentHash, Vec<u8>>>>,
}

impl MemBlobStoreAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlobStoreAdapter for MemBlobStoreAdapter {
    fn has(&self, hash: &ContentHash) -> bool {
        self.blobs.lock().unwrap().contains_key(hash)
    }

    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        self.blobs.lock().unwrap().get(hash).cloned()
    }

    fn put(&self, data: &[u8]) -> std::io::Result<ContentHash> {
        let hash = ContentHash::from_bytes(data);
        self.blobs.lock().unwrap().insert(hash.clone(), data.to_vec());
        Ok(hash)
    }

    fn list(&self) -> std::io::Result<Vec<ContentHash>> {
        Ok(self.blobs.lock().unwrap().keys().cloned().collect())
    }

    fn remove(&self, hash: &ContentHash) -> std::io::Result<bool> {
        Ok(self.blobs.lock().unwrap().remove(hash).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_cid(data: &[u8], codec: Codec) -> Cid {
        Cid::from_content_blake3_with_codec(data, codec)
    }

    #[test]
    fn test_mem_blob_store_adapter() {
        let store = MemBlobStoreAdapter::new();
        let data = b"hello world";
        let hash = store.put(data).unwrap();
        assert!(store.has(&hash));
        assert_eq!(store.get(&hash), Some(data.to_vec()));
        assert_eq!(store.list().unwrap(), vec![hash.clone()]);
        assert!(store.remove(&hash).unwrap());
        assert!(!store.has(&hash));
    }

    #[test]
    fn test_dag_block_store_put_get() {
        let blob_store = Arc::new(MemBlobStoreAdapter::new());
        let dag_store = DagBlockStore::new(blob_store.clone(), PathBuf::from("/tmp/test-dag"));

        let data = b"test block data";
        let cid = dummy_cid(data, Codec::Raw);

        dag_store.put(&cid, data);
        assert!(dag_store.has(&cid));
        assert_eq!(dag_store.get(&cid), Some(data.to_vec()));
    }

    #[test]
    fn test_dag_block_store_links() {
        let blob_store = Arc::new(MemBlobStoreAdapter::new());
        let dag_store = DagBlockStore::new(blob_store.clone(), PathBuf::from("/tmp/test-dag-links"));

        // Create a mock DAG-PB directory structure
        // In real usage, this would be proper protobuf-encoded data
        // For now, test that Raw blocks return empty links
        let data = b"raw data";
        let cid = dummy_cid(data, Codec::Raw);

        dag_store.put(&cid, data);
        assert!(dag_store.has(&cid));
        // Raw blocks have no links
        assert!(dag_store.links(&cid).is_empty());
    }

    #[test]
    fn test_content_hash_lookup() {
        let blob_store = Arc::new(MemBlobStoreAdapter::new());
        let dag_store = DagBlockStore::new(blob_store.clone(), PathBuf::from("/tmp/test-hash-lookup"));

        let data = b"lookup test";
        let cid = dummy_cid(data, Codec::Raw);

        dag_store.put(&cid, data);

        // CID should be indexed
        assert!(dag_store.has_cid(&cid));

        // Get content hash for CID
        let content_hash = dag_store.content_hash_for_cid(&cid);
        assert!(content_hash.is_some());

        // Get CID for content hash
        let cids = dag_store.cid_for_content_hash(&content_hash.unwrap());
        assert!(cids.is_some());
        assert!(cids.unwrap().contains(&cid));
    }

    #[test]
    fn test_stats() {
        let blob_store = Arc::new(MemBlobStoreAdapter::new());
        let dag_store = DagBlockStore::new(blob_store.clone(), PathBuf::from("/tmp/test-stats"));

        let data = b"stats test";
        let cid = dummy_cid(data, Codec::Raw);
        dag_store.put(&cid, data);

        let stats = dag_store.stats();
        assert_eq!(stats.cid_count, 1);
        assert!(stats.total_block_size > 0);
    }
}
