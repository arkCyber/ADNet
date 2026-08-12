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
    /// File mode (permissions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<UnixFsMode>,

    /// Modification time (seconds since epoch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<UnixFsTime>,

    /// File size (only for non-DAG nodes).
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
    /// Seconds since epoch.
    pub seconds: i64,

    /// Fractional nanoseconds.
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
    /// Raw data block.
    Raw {
        #[serde(skip_serializing_if = "Option::is_none")]
        blocksizes: Option<Vec<u64>>,
    },

    /// Directory node.
    Directory {
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<UnixFsMetadata>,

        /// Named links to child nodes.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        links: Vec<UnixFsLink>,

        /// Number of links in this directory.
        #[serde(skip_serializing_if = "Option::is_none")]
        num_links: Option<u64>,
    },

    /// File node (PB node).
    File {
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<UnixFsMetadata>,

        /// Chunks of file data (only for file nodes).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chunks: Vec<UnixFsData>,

        /// Total file size.
        #[serde(skip_serializing_if = "Option::is_none")]
        filesize: Option<u64>,

        /// Block sizes for each chunk.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocksizes: Vec<u64>,
    },

    /// HAMT sharded directory.
    HamtShardedDirectory {
        /// HAMT-specific fields.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        hamt_opts: HashMap<String, Vec<u8>>,

        /// Number of links.
        #[serde(skip_serializing_if = "Option::is_none")]
        fanout: Option<u64>,

        /// Bits per segment.
        #[serde(skip_serializing_if = "Option::is_none")]
        bits_width: Option<u64>,
    },

    /// Symlink node.
    Symlink {
        /// Target path.
        target: String,

        /// Metadata.
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<UnixFsMetadata>,
    },

    /// Metadata node.
    Metadata {
        /// Metadata node points to another node.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "Target")]
        target: Option<Cid>,

        /// Metadata.
        metadata: UnixFsMetadata,
    },
}

/// Data block in a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnixFsData {
    /// Size of this data block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,

    /// Raw data bytes (for small files).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,

    /// Block size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocksize: Option<u64>,
}

/// A link to another UnixFS node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnixFsLink {
    /// Name of the link (file name or directory entry).
    pub name: String,

    /// CID of the linked node.
    #[serde(rename = "Cid")]
    pub cid: Cid,

    /// Cumulative size of the linked subtree.
    #[serde(rename = "Tsize", skip_serializing_if = "Option::is_none")]
    pub tsize: Option<u64>,
}

impl UnixFsLink {
    /// Create a new link.
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
    /// Create a new builder with default chunk size (256KB).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with custom chunk size.
    pub fn with_chunk_size(chunk_size: usize) -> Self {
        Self {
            chunk_size,
            metadata: None,
        }
    }

    /// Set metadata.
    pub fn with_metadata(mut self, metadata: UnixFsMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Add file metadata.
    pub fn with_mode(mut self, mode: UnixFsMode) -> Self {
        let metadata = self.metadata.get_or_insert_with(UnixFsMetadata::default);
        metadata.mode = Some(mode);
        self
    }

    /// Add modification time.
    pub fn with_mtime(mut self, mtime: UnixFsTime) -> Self {
        let metadata = self.metadata.get_or_insert_with(UnixFsMetadata::default);
        metadata.mtime = Some(mtime);
        self
    }

    /// Build a UnixFS file node from data.
    pub fn build(self, data: &[u8]) -> UnixFsNode {
        let total_size = data.len() as u64;
        let chunk_size = self.chunk_size;

        if data.len() <= chunk_size {
            // Small file: single node with inline data
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
            // Large file: node with links to chunks
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
            chunk_size: 256 * 1024, // 256KB default
            metadata: None,
        }
    }
}

/// Directory builder.
pub struct UnixFsDirBuilder {
    metadata: Option<UnixFsMetadata>,
}

impl UnixFsDirBuilder {
    /// Create a new directory builder.
    pub fn new() -> Self {
        Self { metadata: None }
    }

    /// Set metadata.
    pub fn with_metadata(mut self, metadata: UnixFsMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Build an empty directory.
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

    /// Serialize UnixFS node to CBOR bytes.
    pub fn to_cbor(node: &UnixFsNode) -> Result<Vec<u8>, UnixFsError> {
        serde_cbor::to_vec(node).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    /// Deserialize UnixFS node from CBOR bytes.
    pub fn from_cbor(bytes: &[u8]) -> Result<UnixFsNode, UnixFsError> {
        serde_cbor::from_slice(bytes).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    /// Serialize UnixFS node to JSON bytes.
    pub fn to_json(node: &UnixFsNode) -> Result<Vec<u8>, UnixFsError> {
        serde_json::to_vec(node).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    /// Deserialize UnixFS node from JSON bytes.
    pub fn from_json(bytes: &[u8]) -> Result<UnixFsNode, UnixFsError> {
        serde_json::from_slice(bytes).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }
}

/// UnixFS path resolution.
pub mod path {
    use super::*;

    /// Parse a UnixFS path into segments.
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

    /// Resolve a path through a directory.
    ///
    /// Note: This is a stub implementation. Full path resolution requires
    /// access to a block store to fetch child nodes.
    #[allow(dead_code)]
    pub fn resolve<'a>(
        root: &'a UnixFsNode,
        path_segments: &[&str],
    ) -> Result<&'a UnixFsNode, UnixFsError> {
        let current = root;

        for segment in path_segments {
            match current {
                UnixFsNode::Directory { links, .. } => {
                    let _link = links
                        .iter()
                        .find(|l| l.name == *segment)
                        .ok_or_else(|| UnixFsError::PathNotFound(segment.to_string()))?;
                    return Err(UnixFsError::InvalidPath(
                        "path traversal requires block store access".to_string(),
                    ));
                }
                _ => return Err(UnixFsError::NotADirectory),
            }
        }

        Ok(current)
    }
}

/// Hash computation helpers.
pub mod hash {
    use super::*;

    /// Compute the CID for a UnixFS node using SHA-256.
    pub fn cid_v0(node: &UnixFsNode) -> Result<Cid, UnixFsError> {
        let bytes = crate::unixfs::serialization::to_cbor(node)?;
        let mh = sha256(&bytes);
        Cid::new_v0(mh).map_err(|e| UnixFsError::Encoding(e.to_string()))
    }

    /// Compute the CID for a UnixFS node using BLAKE3 (CIDv1).
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
            UnixFsNode::File {
                filesize, chunks, ..
            } => {
                assert_eq!(filesize, Some(11));
                assert_eq!(chunks.len(), 1);
                assert_eq!(chunks[0].data.as_ref().unwrap(), b"hello world");
            }
            _ => panic!("expected file node"),
        }
    }

    #[test]
    fn test_build_large_file() {
        let data = vec![0u8; 600 * 1024]; // 600KB, more than 256KB chunk
        let node = UnixFsFileBuilder::with_chunk_size(256 * 1024).build(&data);

        match node {
            UnixFsNode::File {
                filesize,
                chunks,
                blocksizes,
                ..
            } => {
                assert_eq!(filesize, Some(600 * 1024 as u64));
                assert_eq!(chunks.len(), 3); // Should be split into 3 chunks
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
            UnixFsNode::Directory {
                metadata,
                links,
                num_links,
            } => {
                assert!(
                    metadata
                        .as_ref()
                        .map(|m| m.mode.as_ref().map(|m| m.is_dir()).unwrap_or(false))
                        .unwrap_or(false)
                );
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
        use crate::unixfs::serialization;
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
}
