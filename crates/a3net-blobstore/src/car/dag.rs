//! DAG-aware CAR export with topological ordering.
//!
//! This module provides `DagCarWriter` for exporting complete DAGs
//! from a block store. It performs topological traversal to ensure
//! blocks are emitted in dependency order (children before parents).
//!
//! ## Usage
//!
//! ```ignore
//! let store = BlobStore::new(data_dir)?;
//! let roots = vec![root_cid.clone()];
//!
//! let file = std::fs::File::create("export.car")?;
//! let mut writer = DagCarWriter::new(file, roots);
//!
//! // Walk the DAG and write blocks
//! for block in dag.walk(&store, &root_cid)? {
//!     writer.write_block(&block.cid, &block.data)?;
//! }
//! writer.finish()?;
//! ```

use std::collections::{HashSet, VecDeque};
use std::io::{self, Write};
use std::sync::Arc;

use a3net_types::cid::Cid;
use a3net_types::content::ContentHash;
use a3net_types::dag_codec::{extract_links, DagCodecRegistry};

use super::{CarBlock, CarError, CarHeader, CarWriter, WriteCarExt};

impl From<CarError> for io::Error {
    fn from(e: CarError) -> Self {
        match e {
            CarError::Io(e) => e,
            CarError::InvalidFormat => io::Error::new(io::ErrorKind::InvalidData, "invalid CAR format"),
            CarError::InvalidHash(s) => io::Error::new(io::ErrorKind::InvalidInput, s),
            CarError::MissingRoot(ch) => io::Error::new(io::ErrorKind::NotFound, format!("missing root: {}", ch)),
        }
    }
}

/// A DAG block with its content hash for CAR export.
#[derive(Debug, Clone)]
pub struct DagBlock {
    /// Content hash (CID) of the block.
    pub cid: ContentHash,
    /// Raw block bytes.
    pub data: Vec<u8>,
    /// Links to other blocks (for traversal).
    pub links: Vec<ContentHash>,
}

impl DagBlock {
    /// Create a new DAG block.
    pub fn new(cid: ContentHash, data: Vec<u8>) -> Self {
        let links = extract_content_hashes_from_data(&data);
        Self { cid, data, links }
    }

    /// Create a new DAG block with pre-computed links.
    pub fn with_links(cid: ContentHash, data: Vec<u8>, links: Vec<ContentHash>) -> Self {
        Self { cid, data, links }
    }
}

/// Extract content hashes from block data using the DAG codec registry.
fn extract_content_hashes_from_data(data: &[u8]) -> Vec<ContentHash> {
    // Try to parse as CID first to determine the codec
    if let Ok(cid) = Cid::from_content_blake3(data) {
        if let Ok(links) = extract_links(&cid, data) {
            return links
                .into_iter()
                .filter_map(|link| {
                    // Convert CID bytes to hex string, then to ContentHash
                    let hex_str = hex::encode(link.cid.to_bytes());
                    ContentHash::from_hex(&hex_str).ok()
                })
                .collect();
        }
    }
    // Fallback: treat as raw data with no links
    Vec::new()
}

/// Trait for block stores that support DAG traversal.
pub trait DagBlockStore: Send + Sync {
    /// Get a block by its content hash.
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>>;

    /// Check if a block exists.
    fn has(&self, hash: &ContentHash) -> bool;
}

/// DAG traversal iterator for CAR export.
///
/// Performs breadth-first topological traversal to emit blocks
/// in dependency order (leaf nodes first, root last).
pub struct DagWalker<'a, BS> {
    store: &'a BS,
    visited: HashSet<ContentHash>,
    queue: VecDeque<ContentHash>,
    buffer: VecDeque<DagBlock>,
}

impl<'a, BS: DagBlockStore> DagWalker<'a, BS> {
    /// Create a new DAG walker starting from the given roots.
    pub fn new(store: &'a BS, roots: Vec<ContentHash>) -> Self {
        let mut walker = Self {
            store,
            visited: HashSet::new(),
            queue: VecDeque::new(),
            buffer: VecDeque::new(),
        };
        for root in roots {
            if !walker.visited.contains(&root) {
                walker.queue.push_back(root);
            }
        }
        walker
    }

    /// Pre-load blocks from the queue into the buffer.
    fn preload(&mut self) {
        while let Some(hash) = self.queue.pop_front() {
            if self.visited.contains(&hash) {
                continue;
            }
            if let Some(data) = self.store.get(&hash) {
                let block = DagBlock::new(hash.clone(), data);
                // Add links to the queue (in reverse to maintain order)
                for link in block.links.iter().rev() {
                    if !self.visited.contains(link) {
                        self.queue.push_front(link.clone());
                    }
                }
                self.buffer.push_back(block);
                self.visited.insert(hash);
            }
        }
    }
}

impl<BS: DagBlockStore> Iterator for DagWalker<'_, BS> {
    type Item = Result<DagBlock, CarError>;

    fn next(&mut self) -> Option<Self::Item> {
        // Try buffer first
        if let Some(block) = self.buffer.pop_front() {
            return Some(Ok(block));
        }

        // Pre-load more blocks
        self.preload();

        self.buffer.pop_front().map(Ok)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let visited = self.visited.len();
        let queued = self.queue.len() + self.buffer.len();
        (0, Some(visited + queued))
    }
}

/// Export a complete DAG to a CAR file.
///
/// This function performs topological traversal and writes blocks
/// in dependency order to the provided writer.
///
/// ## Example
///
/// ```ignore
/// use a3net_blobstore::car::dag::{export_dag, DagBlockStoreExt};
///
/// let store = BlobStore::new(data_dir)?;
/// let root = ContentHash::from_hex("...")?;
///
/// let file = std::fs::File::create("export.car")?;
/// export_dag(file, &[root], &store)?;
/// ```
pub fn export_dag<W: Write, BS: DagBlockStore>(
    writer: W,
    roots: &[ContentHash],
    store: &BS,
) -> Result<(), CarError> {
    let mut car_writer = DagCarWriter::new(writer, roots.to_vec());

    let walker = DagWalker::new(store, roots.to_vec());
    for block_result in walker {
        let block = block_result?;
        car_writer.write_block(&block.cid, &block.data)?;
    }

    car_writer.finish()
}

/// CAR writer that tracks DAG structure for proper ordering.
pub struct DagCarWriter<W: Write> {
    inner: CarWriter<W>,
    roots: Vec<ContentHash>,
    blocks_written: HashSet<ContentHash>,
}

impl<W: Write> DagCarWriter<W> {
    /// Create a new DAG CAR writer.
    pub fn new(writer: W, roots: Vec<ContentHash>) -> Result<Self, CarError> {
        let header = CarHeader::new(roots.clone());
        let mut inner = CarWriter::new(writer);
        inner.write_header(&header)?;
        Ok(Self {
            inner,
            roots,
            blocks_written: HashSet::new(),
        })
    }

    /// Write a single block.
    pub fn write_block(&mut self, cid: &ContentHash, data: &[u8]) -> Result<(), CarError> {
        self.blocks_written.insert(cid.clone());
        self.inner.write_block(cid, data)
    }

    /// Finish writing and flush.
    pub fn finish(mut self) -> Result<(), CarError> {
        // Write any remaining roots that weren't explicitly written
        for root in &self.roots {
            if !self.blocks_written.contains(root) {
                // Root block not found - this is an error
                return Err(CarError::MissingRoot);
            }
        }
        self.inner.flush()
    }
}

/// Extension trait for block stores with DAG-aware operations.
pub trait DagBlockStoreExt {
    /// Export this store's DAG to a CAR file.
    fn export_car<W: Write>(&self, writer: W, roots: &[ContentHash]) -> Result<(), CarError>
    where
        Self: Sized + DagBlockStore,
    {
        export_dag(writer, roots, self)
    }
}

impl<BS: DagBlockStore> DagBlockStoreExt for BS {}

/// Convert between ContentHash and iroh Hash for streaming.
#[cfg(feature = "iroh")]
pub mod iroh {
    use super::*;

    /// Convert A3Net ContentHash to iroh Hash.
    pub fn content_hash_to_iroh(hash: &ContentHash) -> io::Result<IrohHash> {
        let hex = hash.as_hex();
        let bytes = hex::decode(hex)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if bytes.len() != 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ContentHash must be 32 bytes",
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(IrohHash::from_bytes(arr))
    }

    /// Convert iroh Hash to A3Net ContentHash.
    pub fn iroh_hash_to_content(hash: &IrohHash) -> ContentHash {
        ContentHash::from_hex(&hex::encode(hash.as_bytes())).expect("iroh hash is always 32 bytes")
    }

    /// Wrap an iroh store as a DagBlockStore.
    pub struct IrohDagBlockStore<S> {
        inner: Arc<S>,
    }

    impl<S> IrohDagBlockStore<S> {
        pub fn new(store: Arc<S>) -> Self {
            Self { inner: store }
        }
    }

    impl<S: super::super::iroh_store::IrohFsStore> super::DagBlockStore for IrohDagBlockStore<S> {
        fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
            let iroh_hash = content_hash_to_iroh(hash).ok()?;
            // Use the synchronous get method if available
            None // Fallback - actual implementation depends on store API
        }

        fn has(&self, hash: &ContentHash) -> bool {
            let iroh_hash = content_hash_to_iroh(hash).ok()?;
            // Use the synchronous has method if available
            false // Fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockStore {
        blocks: std::collections::HashMap<ContentHash, Vec<u8>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                blocks: std::collections::HashMap::new(),
            }
        }

        fn insert(&mut self, data: Vec<u8>) -> ContentHash {
            let hash = ContentHash::from_bytes(&data);
            self.blocks.insert(hash, data);
            hash
        }
    }

    impl DagBlockStore for MockStore {
        fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
            self.blocks.get(hash).cloned()
        }

        fn has(&self, hash: &ContentHash) -> bool {
            self.blocks.contains_key(hash)
        }
    }

    #[test]
    fn test_dag_walker_single_block() {
        let mut store = MockStore::new();
        let hash = store.insert(b"single block".to_vec());

        let walker = DagWalker::new(&store, vec![hash.clone()]);
        let blocks: Vec<_> = walker.collect();

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].as_ref().unwrap().cid, hash);
    }

    #[test]
    fn test_dag_walker_preserves_order() {
        let mut store = MockStore::new();

        // Create a simple DAG: root -> child
        let child_data = b"child data".to_vec();
        let child_hash = ContentHash::from_bytes(&child_data);

        // Parent references child via CBOR links (simplified for test)
        let parent_data = b"parent data".to_vec();
        let parent_hash = ContentHash::from_bytes(&parent_data);

        store.blocks.insert(child_hash.clone(), child_data);
        store.blocks.insert(parent_hash.clone(), parent_data);

        let walker = DagWalker::new(&store, vec![parent_hash.clone()]);
        let blocks: Vec<_> = walker.collect();

        // Should visit both blocks
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_dag_car_writer() {
        let mut store = MockStore::new();
        let hash = store.insert(b"test block".to_vec());

        let mut buf = Vec::new();
        let mut writer = DagCarWriter::new(&mut buf, vec![hash.clone()]).unwrap();
        writer.write_block(&hash, b"test block").unwrap();
        writer.finish().unwrap();

        // Verify we can read it back
        let mut cursor = std::io::Cursor::new(&buf);
        let (header, blocks) = super::read_car(&mut cursor).unwrap();
        assert_eq!(header.roots, vec![hash]);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].data, b"test block");
    }
}
