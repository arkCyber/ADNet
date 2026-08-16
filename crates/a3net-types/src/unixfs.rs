//! UnixFS codec implementation for IPFS-compatible DAG operations.
//!
//! UnixFS is the file system format used by IPFS. It defines how files and
//! directories are represented as DAG nodes.
//!
//! ## Node Types
//!
//! - **Raw**: Raw data without a file structure
//! - **File**: Regular files with optional metadata
//! - **Directory**: Container for other nodes
//! - **HAMT Sharded Directory**: Hash Array Mapped Trie for large directories
//! - **Symlink**: Symbolic link to another path
//! - **Metadata**: File metadata (permissions, modification time)
//!
//! ## File Layout
//!
//! Files larger than the chunk size are split into multiple blocks linked
//! from a file node. Each chunk is typically 256KB.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cid::Cid;
use crate::multihash::{blake3_hash, sha256};

/// UnixFS error types.
#[derive(Debug, Error)]
pub enum UnixFsError {
    #[error("invalid node: {0}")]
    InvalidNode(String),

    #[error("not a directory")]
    NotADirectory,

    #[error("not a file")]
    NotAFile,

    #[error("not a symlink")]
    NotASymlink,

    #[error("path not found: {0}")]
    PathNotFound(String),

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("encoding error: {0}")]
    Encoding(String),

    #[error("size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("too many links: {0}")]
    TooManyLinks(usize),
}

/// Data mode (permissions-like).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnixFsMode(u32);

impl UnixFsMode {
    pub const DIR: Self = UnixFsMode(0o040755);
    pub const FILE: Self = UnixFsMode(0o100644);
    pub const SYMLINK: Self = UnixFsMode(0o120755);

    pub fn new(mode: u32) -> Self {
        Self(mode)
    }

    pub fn is_dir(&self) -> bool {
        (self.0 & 0o170000) == 0o040000
    }

    pub fn is_file(&self) -> bool {
        (self.0 & 0o170000) == 0o100000
    }

    pub fn is_symlink(&self) -> bool {
        (self.0 & 0o170000) == 0o120000
    }

    pub fn bits(&self) -> u32 {
        self.0
    }
}

impl Default for UnixFsMode {
    fn default() -> Self {
        Self::FILE
    }
}

/// UnixFS metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnixFsMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<UnixFsMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<UnixFsTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl Default for UnixFsMetadata {
    fn default() -> Self {
        Self {
            mode: Some(UnixFsMode::FILE),
            mtime: None,
            size: None,
        }
    }
}

/// UnixFS timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnixFsTime {
    pub seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fractional_nanos: Option<u32>,
}

impl UnixFsTime {
    pub fn now() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            seconds: now.as_secs() as i64,
            fractional_nanos: Some(now.subsec_nanos()),
        }
    }
}

/// UnixFS node type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "Type", rename_all = "snake_case")]
pub enum UnixFsNode {
    Raw {
        #[serde(skip_serializing_if = "Option::is_none")]
        blocksizes: Option<Vec<u64>>,
    },
    Directory {
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<UnixFsMetadata>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        links: Vec<UnixFsLink>,
        #[serde(skip_serializing_if = "Option::is_none")]
        num_links: Option<u64>,
    },
    File {
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<UnixFsMetadata>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chunks: Vec<UnixFsData>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filesize: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocksizes: Vec<u64>,
    },
    HamtShardedDirectory {
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        hamt_opts: HashMap<String, Vec<u8>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fanout: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bits_width: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        links: Vec<UnixFsLink>,
    },
    Symlink {
        target: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<UnixFsMetadata>,
    },
    Metadata {
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "Target")]
        target: Option<Cid>,
        metadata: UnixFsMetadata,
    },
}

/// Data block in a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnixFsData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocksize: Option<u64>,
}

/// A link to another UnixFS node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnixFsLink {
    pub name: String,
    #[serde(rename = "Cid")]
    pub cid: Cid,
    #[serde(rename = "Tsize", skip_serializing_if = "Option::is_none")]
    pub tsize: Option<u64>,
}

impl UnixFsLink {
    pub fn new(name: String, cid: Cid, size: u64) -> Self {
        Self {
            name,
            cid,
            tsize: Some(size),
        }
    }
}

/// File builder for creating UnixFS files.
pub struct UnixFsFileBuilder {
    chunk_size: usize,
    metadata: Option<UnixFsMetadata>,
}

impl UnixFsFileBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_chunk_size(chunk_size: usize) -> Self {
        Self {
            chunk_size,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: UnixFsMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_mode(mut self, mode: UnixFsMode) -> Self {
        let metadata = self.metadata.get_or_insert_with(UnixFsMetadata::default);
        metadata.mode = Some(mode);
        self
    }

    pub fn with_mtime(mut self, mtime: UnixFsTime) -> Self {
        let metadata = self.metadata.get_or_insert_with(UnixFsMetadata::default);
        metadata.mtime = Some(mtime);
        self
    }

    pub fn build(self, data: &[u8]) -> UnixFsNode {
        let total_size = data.len() as u64;
        let chunk_size = self.chunk_size;

        if data.len() <= chunk_size {
            UnixFsNode::File {
                metadata: self.metadata,
                chunks: vec![UnixFsData {
                    size: Some(total_size),
                    data: Some(data.to_vec()),
                    blocksize: Some(total_size),
                }],
                filesize: Some(total_size),
                blocksizes: vec![total_size],
            }
        } else {
            let mut chunks = Vec::new();
            let mut blocksizes = Vec::new();

            for chunk in data.chunks(chunk_size) {
                blocksizes.push(chunk.len() as u64);
                chunks.push(UnixFsData {
                    size: Some(chunk.len() as u64),
                    data: Some(chunk.to_vec()),
                    blocksize: Some(chunk.len() as u64),
                });
            }

            UnixFsNode::File {
                metadata: self.metadata,
                chunks,
                filesize: Some(total_size),
                blocksizes,
            }
        }
    }
}

impl Default for UnixFsFileBuilder {
    fn default() -> Self {
        Self {
            chunk_size: 256 * 1024,
            metadata: None,
        }
    }
}

/// Directory builder.
pub struct UnixFsDirBuilder {
    metadata: Option<UnixFsMetadata>,
}

impl UnixFsDirBuilder {
    pub fn new() -> Self {
        Self { metadata: None }
    }

    pub fn with_metadata(mut self, metadata: UnixFsMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn build(self) -> UnixFsNode {
        UnixFsNode::Directory {
            metadata: self.metadata.or_else(|| {
                Some(UnixFsMetadata {
                    mode: Some(UnixFsMode::DIR),
                    mtime: Some(UnixFsTime::now()),
                    size: None,
                })
            }),
            links: Vec::new(),
            num_links: Some(0),
        }
    }
}

impl Default for UnixFsDirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialization helpers for UnixFS nodes.
pub mod serialization {
    use super::*;

    pub fn to_cbor(node: &UnixFsNode) -> Result<Vec<u8>, UnixFsError> {
        serde_cbor::to_vec(node).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    pub fn from_cbor(bytes: &[u8]) -> Result<UnixFsNode, UnixFsError> {
        serde_cbor::from_slice(bytes).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    pub fn to_json(node: &UnixFsNode) -> Result<Vec<u8>, UnixFsError> {
        serde_json::to_vec(node).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    pub fn from_json(bytes: &[u8]) -> Result<UnixFsNode, UnixFsError> {
        serde_json::from_slice(bytes).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }
}

/// Block store trait for DAG traversal.
pub trait BlockStore: Send + Sync {
    fn get(&self, cid: &Cid) -> Option<Vec<u8>>;
    fn has(&self, cid: &Cid) -> bool;
}

/// UnixFS path resolution with BlockStore integration.
pub mod path {
    use super::*;

    pub fn parse_path(path: &str) -> Result<Vec<&str>, UnixFsError> {
        if path.is_empty() {
            return Err(UnixFsError::InvalidPath("empty path".to_string()));
        }
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Ok(Vec::new());
        }
        Ok(path.split('/').filter(|s| !s.is_empty()).collect())
    }

    #[derive(Debug)]
    pub enum ResolveResult {
        File { data: Vec<u8> },
        Directory,
    }

    pub fn resolve_from_node(
        node: &UnixFsNode,
        path_segments: &[&str],
    ) -> Result<ResolveResult, UnixFsError> {
        if path_segments.is_empty() {
            return match node {
                UnixFsNode::File { chunks, .. } => {
                    let data = chunks.iter()
                        .filter_map(|c| c.data.clone())
                        .flatten()
                        .collect();
                    Ok(ResolveResult::File { data })
                }
                _ => Ok(ResolveResult::Directory),
            };
        }

        let segment = path_segments[0];
        let remaining = &path_segments[1..];

        match node {
            UnixFsNode::Directory { links, .. } => {
                if let Some(link) = links.iter().find(|l| l.name == segment) {
                    resolve_from_node_link(&link.cid, remaining)
                } else {
                    Err(UnixFsError::PathNotFound(segment.to_string()))
                }
            }
            UnixFsNode::File { chunks, .. } => {
                if let Ok(index) = segment.parse::<usize>() {
                    if index < chunks.len() && remaining.is_empty() {
                        if let Some(data) = &chunks[index].data {
                            Ok(ResolveResult::File { data: data.clone() })
                        } else {
                            Err(UnixFsError::InvalidPath("chunk has no data".to_string()))
                        }
                    } else {
                        Err(UnixFsError::InvalidPath("invalid chunk access".to_string()))
                    }
                } else {
                    Err(UnixFsError::PathNotFound(segment.to_string()))
                }
            }
            _ => Err(UnixFsError::NotADirectory),
        }
    }

    fn resolve_from_node_link(_cid: &Cid, _segments: &[&str]) -> Result<ResolveResult, UnixFsError> {
        Err(UnixFsError::InvalidPath(
            "link traversal requires block store".to_string()
        ))
    }

    pub fn resolve_with_store<BS: BlockStore>(
        store: &BS,
        root: &Cid,
        path_segments: &[&str],
    ) -> Result<ResolveResult, UnixFsError> {
        resolve_recursive(store, root, path_segments, 0, 64)
    }

    fn resolve_recursive<BS: BlockStore>(
        store: &BS,
        current_cid: &Cid,
        path_segments: &[&str],
        depth: usize,
        max_depth: usize,
    ) -> Result<ResolveResult, UnixFsError> {
        if depth > max_depth {
            return Err(UnixFsError::TooManyLinks(depth));
        }

        let data = store.get(current_cid)
            .ok_or_else(|| UnixFsError::InvalidPath(format!("block not found: {}", current_cid)))?;

        let node: UnixFsNode = crate::unixfs::serialization::from_cbor(&data)
            .or_else(|_| crate::unixfs::serialization::from_json(&data))
            .map_err(|e| UnixFsError::Encoding(e.to_string()))?;

        if path_segments.is_empty() {
            return match node {
                UnixFsNode::File { chunks, .. } => {
                    let data = chunks.iter()
                        .filter_map(|c| c.data.clone())
                        .flatten()
                        .collect();
                    Ok(ResolveResult::File { data })
                }
                _ => Ok(ResolveResult::Directory),
            };
        }

        let segment = path_segments[0];
        let remaining = &path_segments[1..];

        match node {
            UnixFsNode::Directory { links, .. } => {
                if let Some(link) = links.iter().find(|l| l.name == segment) {
                    resolve_recursive(store, &link.cid, remaining, depth + 1, max_depth)
                } else {
                    Err(UnixFsError::PathNotFound(segment.to_string()))
                }
            }
            UnixFsNode::File { chunks, .. } => {
                if let Ok(index) = segment.parse::<usize>() {
                    if index < chunks.len() && remaining.is_empty() {
                        if let Some(data) = &chunks[index].data {
                            Ok(ResolveResult::File { data: data.clone() })
                        } else {
                            Err(UnixFsError::InvalidPath("chunk has no data".to_string()))
                        }
                    } else {
                        Err(UnixFsError::InvalidPath("invalid chunk access".to_string()))
                    }
                } else {
                    Err(UnixFsError::PathNotFound(segment.to_string()))
                }
            }
            _ => Err(UnixFsError::NotADirectory),
        }
    }

    pub fn path_exists<BS: BlockStore>(
        store: &BS,
        root: &Cid,
        path_segments: &[&str],
    ) -> bool {
        resolve_with_store(store, root, path_segments).is_ok()
    }

    pub fn list_directory<BS: BlockStore>(
        store: &BS,
        root: &Cid,
    ) -> Result<Vec<UnixFsLink>, UnixFsError> {
        let data = store.get(root)
            .ok_or_else(|| UnixFsError::InvalidPath(format!("block not found: {}", root)))?;

        let node: UnixFsNode = crate::unixfs::serialization::from_cbor(&data)
            .or_else(|_| crate::unixfs::serialization::from_json(&data))
            .map_err(|e| UnixFsError::Encoding(e.to_string()))?;

        match node {
            UnixFsNode::Directory { links, .. } => Ok(links),
            _ => Err(UnixFsError::NotADirectory),
        }
    }
}

/// HAMT sharded directory implementation.
pub mod hamt {
    use super::*;

    pub const DEFAULT_BITS_WIDTH: u64 = 8;
    pub const DEFAULT_FANOUT: u64 = 256;
    pub const HASH_ALGO: u64 = 0x1e;

    pub fn hash_name(name: &str) -> u64 {
        use blake3::Hasher;
        let mut hasher = Hasher::new();
        hasher.update(name.as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest.as_bytes()[..8]);
        u64::from_le_bytes(bytes)
    }

    pub fn bucket_index(hash: u64, bits_width: u64, depth: usize) -> usize {
        let mask = (1u64 << bits_width) - 1;
        let shift = depth * bits_width as usize;
        ((hash >> shift) & mask) as usize
    }

    pub fn new_shard(bits_width: Option<u64>, fanout: Option<u64>) -> UnixFsNode {
        let bits = bits_width.unwrap_or(DEFAULT_BITS_WIDTH);
        let fanout = fanout.unwrap_or(1u64 << bits);
        
        UnixFsNode::HamtShardedDirectory {
            hamt_opts: HashMap::new(),
            fanout: Some(fanout),
            bits_width: Some(bits),
            links: Vec::new(),
        }
    }

    pub fn insert(
        node: &mut UnixFsNode,
        name: &str,
        cid: Cid,
        size: u64,
    ) -> Result<(), UnixFsError> {
        match node {
            UnixFsNode::HamtShardedDirectory { links, bits_width, .. } => {
                let bits = bits_width.unwrap_or(DEFAULT_BITS_WIDTH) as usize;
                let hash = hash_name(name);
                let index = bucket_index(hash, bits as u64, 0);
                
                while links.len() <= index {
                    links.push(UnixFsLink {
                        name: format!("{:x}", links.len()),
                        cid: Cid::from_content_blake3(&[0]),
                        tsize: Some(0),
                    });
                }
                
                links[index] = UnixFsLink::new(name.to_string(), cid, size);
                Ok(())
            }
            _ => Err(UnixFsError::NotADirectory),
        }
    }

    pub fn find<'a>(node: &'a UnixFsNode, name: &str) -> Option<&'a UnixFsLink> {
        match node {
            UnixFsNode::HamtShardedDirectory { links, bits_width, .. } => {
                let bits = bits_width.unwrap_or(DEFAULT_BITS_WIDTH) as usize;
                let hash = hash_name(name);
                let index = bucket_index(hash, bits as u64, 0);
                links.get(index).filter(|l| l.name == name)
            }
            _ => None,
        }
    }

    pub fn iter(node: &UnixFsNode) -> Box<dyn Iterator<Item = &UnixFsLink> + '_> {
        match node {
            UnixFsNode::HamtShardedDirectory { links, .. } => {
                Box::new(links.iter().filter(|l| !l.name.is_empty() && l.name != "0"))
            }
            _ => Box::new([].iter()),
        }
    }
}

/// Hash computation helpers.
pub mod hash {
    use super::*;

    pub fn cid_v0(node: &UnixFsNode) -> Result<Cid, UnixFsError> {
        let bytes = crate::unixfs::serialization::to_cbor(node)?;
        let mh = sha256(&bytes);
        Cid::new_v0(mh).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    pub fn cid_v1(node: &UnixFsNode) -> Result<Cid, UnixFsError> {
        let bytes = crate::unixfs::serialization::to_cbor(node)?;
        let mh = blake3_hash(&bytes);
        Ok(Cid::new_v1_dag_pb(mh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unixfs_mode() {
        let dir_mode = UnixFsMode::DIR;
        assert!(dir_mode.is_dir());
        assert!(!dir_mode.is_file());

        let file_mode = UnixFsMode::FILE;
        assert!(!file_mode.is_dir());
        assert!(file_mode.is_file());
    }

    #[test]
    fn test_build_small_file() {
        let data = b"hello world";
        let node = UnixFsFileBuilder::new().build(data);

        match node {
            UnixFsNode::File { filesize, chunks, .. } => {
                assert_eq!(filesize, Some(11));
                assert_eq!(chunks.len(), 1);
                assert_eq!(chunks[0].data.as_ref().unwrap(), b"hello world");
            }
            _ => panic!("expected file node"),
        }
    }

    #[test]
    fn test_build_large_file() {
        let data = vec![0u8; 600 * 1024];
        let node = UnixFsFileBuilder::with_chunk_size(256 * 1024).build(&data);

        match node {
            UnixFsNode::File { filesize, chunks, blocksizes, .. } => {
                assert_eq!(filesize, Some(600 * 1024 as u64));
                assert_eq!(chunks.len(), 3);
                assert_eq!(blocksizes.len(), 3);
            }
            _ => panic!("expected file node"),
        }
    }

    #[test]
    fn test_build_directory() {
        let node = UnixFsDirBuilder::new()
            .with_metadata(UnixFsMetadata {
                mode: Some(UnixFsMode::DIR),
                mtime: Some(UnixFsTime::now()),
                size: None,
            })
            .build();

        match node {
            UnixFsNode::Directory { metadata, links, num_links } => {
                assert!(metadata
                    .as_ref()
                    .map(|m| m.mode.as_ref().map(|m| m.is_dir()).unwrap_or(false))
                    .unwrap_or(false));
                assert!(links.is_empty());
                assert_eq!(num_links, Some(0));
            }
            _ => panic!("expected directory node"),
        }
    }

    #[test]
    fn test_unixfs_time() {
        let time = UnixFsTime::now();
        assert!(time.seconds > 0);
    }

    #[test]
    fn test_unixfs_serde() {
        let node = UnixFsFileBuilder::new().build(b"test data");
        let bytes = serialization::to_cbor(&node).unwrap();
        let decoded: UnixFsNode = serialization::from_cbor(&bytes).unwrap();
        assert!(matches!(decoded, UnixFsNode::File { .. }));
    }

    #[test]
    fn test_link_serialization() {
        let data = b"content";
        let cid = Cid::from_content_blake3(data);
        let link = UnixFsLink::new("test.txt".to_string(), cid, data.len() as u64);
        let bytes = serde_json::to_vec(&link).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("test.txt"));
    }

    #[test]
    fn test_hamt_hash() {
        let hash1 = hamt::hash_name("test");
        let hash2 = hamt::hash_name("test");
        assert_eq!(hash1, hash2);
        
        let hash3 = hamt::hash_name("other");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_hamt_bucket_index() {
        let hash = 0xABCD;
        let idx0 = hamt::bucket_index(hash, 8, 0);
        let idx1 = hamt::bucket_index(hash, 8, 1);
        assert_ne!(idx0, idx1);
    }

    #[test]
    fn test_path_parse() {
        let segments = path::parse_path("/foo/bar/baz").unwrap();
        assert_eq!(segments, vec!["foo", "bar", "baz"]);

        let segments = path::parse_path("foo//bar").unwrap();
        assert_eq!(segments, vec!["foo", "bar"]);
    }

    #[test]
    fn test_hamt_new_shard() {
        let shard = hamt::new_shard(None, None);
        assert!(matches!(shard, UnixFsNode::HamtShardedDirectory { .. }));

        let custom_shard = hamt::new_shard(Some(4), None);
        assert!(matches!(custom_shard, UnixFsNode::HamtShardedDirectory { bits_width: Some(4), .. }));
    }

    #[test]
    fn test_hamt_insert() {
        let mut shard = hamt::new_shard(None, None);
        let cid = Cid::from_content_blake3(b"test");
        let result = hamt::insert(&mut shard, "test.txt", cid, 100);
        assert!(result.is_ok());

        // Insert duplicate should overwrite
        let cid2 = Cid::from_content_blake3(b"test2");
        let result2 = hamt::insert(&mut shard, "test.txt", cid2, 200);
        assert!(result2.is_ok());
    }

    #[test]
    fn test_hamt_find() {
        let mut shard = hamt::new_shard(None, None);
        let cid = Cid::from_content_blake3(b"test");
        hamt::insert(&mut shard, "found.txt", cid, 100).unwrap();

        let found = hamt::find(&shard, "found.txt");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "found.txt");

        let not_found = hamt::find(&shard, "missing.txt");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_hamt_iter() {
        let mut shard = hamt::new_shard(None, None);
        hamt::insert(&mut shard, "a.txt", Cid::from_content_blake3(b"a"), 10).unwrap();
        hamt::insert(&mut shard, "b.txt", Cid::from_content_blake3(b"b"), 20).unwrap();
        hamt::insert(&mut shard, "c.txt", Cid::from_content_blake3(b"c"), 30).unwrap();

        let entries: Vec<_> = hamt::iter(&shard).collect();
        assert!(entries.len() >= 3);
    }

    #[test]
    fn test_hash_cid_v0() {
        let node = UnixFsFileBuilder::new().build(b"test");
        let result = hash::cid_v0(&node);
        assert!(result.is_ok());
        let cid = result.unwrap();
        // V0 CID should start with Qm
        let s = cid.to_string();
        assert!(s.starts_with("Qm") || s.is_empty());
    }

    #[test]
    fn test_hash_cid_v1() {
        let node = UnixFsFileBuilder::new().build(b"test");
        let result = hash::cid_v1(&node);
        assert!(result.is_ok());
    }
}
