//! DAG service for IPFS-compatible DAG operations.
//!
//! This module provides the DAG (Directed Acyclic Graph) service that handles:
//! - Storing and retrieving DAG nodes
//! - Resolving paths through DAG structures
//! - Supporting UnixFS-like DAG traversal
//!
//! ## DAG Format
//!
//! ADNet DAGs are stored as CBOR-encoded DAG nodes with the following structure:
//!
//! ```text
//! {
//!     "data": [...],          // UnixFS-style data node
//!     "links": [              // Links to child nodes
//!         {
//!             "Name": "...",   // Optional name
//!             "Hash": "...",   // CID of child node
//!             "Size": 123       // Size of child
//!         }
//!     ]
//! }
//! ```

use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_types::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur in DAG operations.
#[derive(Debug, Error)]
pub enum DagError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid node: {0}")]
    InvalidNode(String),

    #[error("invalid link: {0}")]
    InvalidLink(String),

    #[error("decode error: {0}")]
    DecodeError(String),

    #[error("encode error: {0}")]
    EncodeError(String),

    #[error("path error: {0}")]
    PathError(String),

    #[error("internal error: {0}")]
    Internal(String),
}

/// A link to another DAG node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagLink {
    /// Name of the link (optional).
    #[serde(rename = "Name")]
    pub name: Option<String>,

    /// CID/Hash of the linked node.
    #[serde(rename = "Hash")]
    pub hash: String,

    /// Size of the linked node.
    #[serde(rename = "Size")]
    pub size: u64,
}

/// A DAG node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    /// UnixFS-style data (for file/directory markers).
    #[serde(rename = "data", skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,

    /// Links to child nodes.
    #[serde(rename = "links", default)]
    pub links: Vec<DagLink>,

    /// Optional UnixFS version.
    #[serde(rename = "UnixFS", skip_serializing_if = "Option::is_none")]
    pub unixfs: Option<UnixFsNode>,
}

/// UnixFS node metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UnixFsNode {
    #[serde(rename = "file")]
    File {
        #[serde(rename = "size")]
        size: Option<u64>,
        #[serde(rename = "mtime")]
        mtime: Option<UnixFsTime>,
    },
    #[serde(rename = "directory")]
    Directory {
        #[serde(rename = "mtime")]
        mtime: Option<UnixFsTime>,
    },
    #[serde(rename = "raw")]
    Raw {
        #[serde(rename = "contentType")]
        content_type: Option<String>,
    },
    #[serde(rename = "hamt-sharded-directory")]
    HamtDirectory {
        #[serde(rename = "mtime")]
        mtime: Option<UnixFsTime>,
    },
}

/// UnixFS timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnixFsTime {
    #[serde(rename = "Seconds")]
    pub seconds: i64,
    #[serde(rename = "FractionalNanoseconds", skip_serializing_if = "Option::is_none")]
    pub fractional_ns: Option<u32>,
}

/// Result of a DAG put operation.
#[derive(Debug, Clone)]
pub struct DagPutResult {
    /// The CID of the stored DAG node.
    pub cid: String,
    /// Size of the stored data.
    pub size: u64,
}

/// Result of a DAG get operation.
#[derive(Debug, Clone)]
pub struct DagGetResult {
    /// The raw data of the resolved path.
    pub data: Vec<u8>,
    /// The remaining path if not fully resolved.
    pub remaining_path: Option<String>,
    /// Whether the result is a DAG node or raw data.
    pub is_dag_node: bool,
}

/// Result of a DAG resolve operation.
#[derive(Debug, Clone)]
pub struct DagResolveResult {
    /// The CID of the resolved content.
    pub cid: String,
    /// Remaining path segment.
    pub path: Option<String>,
}

/// The DAG service.
#[derive(Clone)]
pub struct DagService {
    blob_store: Arc<BlobStore>,
}

impl DagService {
    /// Create a new DAG service.
    pub fn new(blob_store: Arc<BlobStore>) -> Self {
        Self { blob_store }
    }

    /// Store a new DAG node.
    pub async fn put(&self, data: &[u8]) -> Result<DagPutResult, DagError> {
        // Try to decode as a DAG node first
        if let Ok(node) = serde_cbor::from_slice::<DagNode>(data) {
            // Validate the node
            self.validate_node(&node)?;

            // Encode and store
            let encoded = serde_cbor::to_vec(&node)
                .map_err(|e| DagError::EncodeError(e.to_string()))?;

            let (hash, size) = self.blob_store.put_bytes_sync(&encoded)
                .map_err(|e| DagError::Internal(e.to_string()))?;

            return Ok(DagPutResult {
                cid: hash.as_hex().to_string(),
                size,
            });
        }

        // If not a valid CBOR DAG node, treat as raw data
        // In IPFS, raw data gets wrapped in a DAG-PB node
        let wrapped = self.wrap_raw_data(data)?;
        let encoded = serde_cbor::to_vec(&wrapped)
            .map_err(|e| DagError::EncodeError(e.to_string()))?;

        let (hash, size) = self.blob_store.put_bytes_sync(&encoded)
            .map_err(|e| DagError::Internal(e.to_string()))?;

        Ok(DagPutResult {
            cid: hash.as_hex().to_string(),
            size,
        })
    }

    /// Get a DAG node by CID.
    pub async fn get(&self, hash: &ContentHash, path: &[String]) -> Result<DagGetResult, DagError> {
        // Get the raw data
        let data = self.blob_store.get_sync(hash)
            .ok_or_else(|| DagError::NotFound(hash.as_hex().to_string()))?;

        // Try to decode as DAG node
        if let Ok(node) = serde_cbor::from_slice::<DagNode>(&data) {
            if path.is_empty() {
                // Return the whole node
                return Ok(DagGetResult {
                    data,
                    remaining_path: None,
                    is_dag_node: true,
                });
            }

            // Resolve path through links
            return self.resolve_path_internal(&node, path);
        }

        // Not a valid DAG node, return as raw data
        if path.is_empty() {
            return Ok(DagGetResult {
                data,
                remaining_path: None,
                is_dag_node: false,
            });
        }

        Err(DagError::PathError(format!(
            "cannot traverse path {:?} in raw data",
            path
        )))
    }

    /// Resolve an IPFS-style path (e.g., /ipfs/cid/some/path).
    pub async fn resolve(&self, path: &str) -> Result<DagResolveResult, DagError> {
        let path = path.trim_start_matches("/ipfs/").trim_start_matches('/');

        // Split into CID and remainder
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        let cid = parts[0];
        let remainder = parts.get(1).map(|s| s.to_string());

        // Verify the CID exists
        let hash = ContentHash::from_hex(cid)
            .map_err(|_| DagError::InvalidNode(format!("invalid CID: {}", cid)))?;

        if !self.blob_store.has_complete(&hash) {
            return Err(DagError::NotFound(cid.to_string()));
        }

        Ok(DagResolveResult {
            cid: cid.to_string(),
            path: remainder,
        })
    }

    /// Resolve a path within a DAG, starting from a hash.
    pub async fn resolve_path(&self, hash: &ContentHash, segments: &[String]) -> Result<Vec<u8>, DagError> {
        let result = self.get(hash, segments).await?;

        if result.is_dag_node && !result.remaining_path.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
            return Err(DagError::PathError(format!(
                "unresolved path: {}",
                result.remaining_path.unwrap_or_default()
            )));
        }

        Ok(result.data)
    }

    /// Internal path resolution through DAG links.
    fn resolve_path_internal(&self, node: &DagNode, path: &[String]) -> Result<DagGetResult, DagError> {
        if path.is_empty() {
            return Ok(DagGetResult {
                data: serde_cbor::to_vec(node).map_err(|e| DagError::EncodeError(e.to_string()))?,
                remaining_path: None,
                is_dag_node: true,
            });
        }

        let segment = &path[0];
        let remaining = &path[1..];

        // Check UnixFS directory
        if let Some(UnixFsNode::Directory { .. }) = &node.unixfs {
            // Find link by name
            let link = node.links.iter()
                .find(|l| l.name.as_deref() == Some(segment.as_str()));

            if let Some(link) = link {
                let hash = ContentHash::from_hex(&link.hash)
                    .map_err(|_| DagError::InvalidLink(format!("invalid hash: {}", link.hash)))?;

                let data = self.blob_store.get_sync(&hash)
                    .ok_or_else(|| DagError::NotFound(link.hash.clone()))?;

                if let Ok(child) = serde_cbor::from_slice::<DagNode>(&data) {
                    return self.resolve_path_internal(&child, remaining);
                }

                // Raw node
                if remaining.is_empty() {
                    return Ok(DagGetResult {
                        data,
                        remaining_path: None,
                        is_dag_node: false,
                    });
                }

                return Err(DagError::PathError(format!(
                    "cannot traverse path {:?} in raw node",
                    remaining
                )));
            }
        }

        // Try numeric index for file chunks
        if let Ok(index) = segment.parse::<usize>()
            && index < node.links.len() {
                let link = &node.links[index];
                let hash = ContentHash::from_hex(&link.hash)
                    .map_err(|_| DagError::InvalidLink(format!("invalid hash: {}", link.hash)))?;

                let data = self.blob_store.get_sync(&hash)
                    .ok_or_else(|| DagError::NotFound(link.hash.clone()))?;

                if remaining.is_empty() {
                    return Ok(DagGetResult {
                        data,
                        remaining_path: None,
                        is_dag_node: false,
                    });
                }

                if let Ok(child) = serde_cbor::from_slice::<DagNode>(&data) {
                    return self.resolve_path_internal(&child, remaining);
                }

                return Err(DagError::PathError(format!(
                    "cannot traverse remaining path {:?}",
                    remaining
                )));
            }

        Err(DagError::PathError(format!(
            "link not found: {} in {:?}",
            segment, node.links
        )))
    }

    /// Validate a DAG node.
    fn validate_node(&self, node: &DagNode) -> Result<(), DagError> {
        // Check that all link hashes are valid
        for link in &node.links {
            if ContentHash::from_hex(&link.hash).is_err() {
                return Err(DagError::InvalidLink(format!(
                    "invalid hash format: {}",
                    link.hash
                )));
            }
        }

        // If it's a directory, it should have links
        if let Some(UnixFsNode::Directory { .. }) = &node.unixfs
            && node.links.is_empty() {
                return Err(DagError::InvalidNode(
                    "directory node must have links".to_string()
                ));
            }

        Ok(())
    }

    /// Wrap raw data in a DAG-PB node.
    fn wrap_raw_data(&self, data: &[u8]) -> Result<DagNode, DagError> {
        Ok(DagNode {
            data: Some(data.to_vec()),
            links: Vec::new(),
            unixfs: Some(UnixFsNode::Raw {
                content_type: Some("application/octet-stream".to_string()),
            }),
        })
    }

    /// List all links in a DAG node.
    pub async fn list_links(&self, hash: &ContentHash) -> Result<Vec<DagLink>, DagError> {
        let data = self.blob_store.get_sync(hash)
            .ok_or_else(|| DagError::NotFound(hash.as_hex().to_string()))?;

        let node: DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| DagError::DecodeError(e.to_string()))?;

        Ok(node.links)
    }

    /// Check if a CID represents a directory.
    pub async fn is_directory(&self, hash: &ContentHash) -> Result<bool, DagError> {
        let data = self.blob_store.get_sync(hash)
            .ok_or_else(|| DagError::NotFound(hash.as_hex().to_string()))?;

        let node: DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| DagError::DecodeError(e.to_string()))?;

        Ok(matches!(
            node.unixfs,
            Some(UnixFsNode::Directory { .. }) | Some(UnixFsNode::HamtDirectory { .. })
        ))
    }

    /// Get the size of a DAG node.
    pub async fn get_size(&self, hash: &ContentHash) -> Result<u64, DagError> {
        let (size, _) = self.blob_store.meta(hash)
            .map_err(|_| DagError::NotFound(hash.as_hex().to_string()))?;
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dag_node_serialization() {
        let node = DagNode {
            data: Some(vec![1, 2, 3]),
            links: vec![
                DagLink {
                    name: Some("chunk-0".to_string()),
                    hash: "abc123".to_string(),
                    size: 1024,
                },
            ],
            unixfs: Some(UnixFsNode::File { size: Some(3), mtime: None }),
        };

        let encoded = serde_cbor::to_vec(&node).unwrap();
        let decoded: DagNode = serde_cbor::from_slice(&encoded).unwrap();

        assert_eq!(decoded.data, node.data);
        assert_eq!(decoded.links.len(), node.links.len());
    }

    #[test]
    fn test_dag_link_serialization() {
        let link = DagLink {
            name: Some("test.txt".to_string()),
            hash: "QmTestHash123456".to_string(),
            size: 12345,
        };

        let json = serde_json::to_string(&link).unwrap();
        assert!(json.contains("test.txt"));
        assert!(json.contains("QmTestHash123456"));
    }
}
