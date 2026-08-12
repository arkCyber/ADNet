//! Object API for IPFS-compatible object operations.
//!
//! This module provides IPFS object operations including:
//! - Creating DAG-PB objects
//! - Getting object information
//! - Linking objects together
//! - Resolving object paths

use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_types::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dag::{DagLink, DagNode, UnixFsNode};

/// Object API errors.
#[derive(Debug, Error)]
pub enum ObjectError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid object: {0}")]
    InvalidObject(String),

    #[error("internal error: {0}")]
    Internal(String),
}

/// Object statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectStats {
    #[serde(rename = "Hash")]
    pub hash: String,
    #[serde(rename = "NumLinks")]
    pub num_links: u32,
    #[serde(rename = "BlockSize")]
    pub block_size: u64,
    #[serde(rename = "LinksSize")]
    pub links_size: u64,
    #[serde(rename = "DataSize")]
    pub data_size: u64,
    #[serde(rename = "CumulativeSize")]
    pub cumulative_size: u64,
}

/// Object data (CBOR-encoded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectData {
    #[serde(rename = "Data")]
    pub data: Vec<u8>,
    #[serde(rename = "Links")]
    pub links: Vec<DagLink>,
}

/// Object API service.
#[derive(Clone)]
pub struct ObjectService {
    blob_store: Arc<BlobStore>,
}

impl ObjectService {
    /// Create a new object service.
    pub fn new(blob_store: Arc<BlobStore>) -> Self {
        Self { blob_store }
    }

    /// Get object statistics for a CID.
    pub async fn stat(&self, hash: &ContentHash) -> Result<ObjectStats, ObjectError> {
        let data = self.blob_store.get_sync(hash)
            .ok_or_else(|| ObjectError::NotFound(hash.as_hex().to_string()))?;

        let node: DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| ObjectError::InvalidObject(e.to_string()))?;

        let links_size: u64 = node.links.iter().map(|l| l.size).sum();
        let data_size = node.data.as_ref().map(|d| d.len() as u64).unwrap_or(0);
        let block_size = data.len() as u64;

        // Calculate cumulative size (this would need recursive traversal for full accuracy)
        let cumulative_size = block_size + links_size;

        Ok(ObjectStats {
            hash: hash.as_hex().to_string(),
            num_links: node.links.len() as u32,
            block_size,
            links_size,
            data_size,
            cumulative_size,
        })
    }

    /// Get object data.
    pub async fn get(&self, hash: &ContentHash) -> Result<ObjectData, ObjectError> {
        let data = self.blob_store.get_sync(hash)
            .ok_or_else(|| ObjectError::NotFound(hash.as_hex().to_string()))?;

        let node: DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| ObjectError::InvalidObject(e.to_string()))?;

        Ok(ObjectData {
            data: node.data.unwrap_or_default(),
            links: node.links,
        })
    }

    /// Create a new empty DAG-PB node.
    pub async fn new_object(&self) -> Result<ContentHash, ObjectError> {
        let node = DagNode {
            data: Some(Vec::new()),
            links: Vec::new(),
            unixfs: Some(UnixFsNode::Directory { mtime: None }),
        };

        let encoded = serde_cbor::to_vec(&node)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        let (hash, _) = self.blob_store.put_bytes_sync(&encoded)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        Ok(hash)
    }

    /// Set object data.
    pub async fn set_data(&self, hash: &ContentHash, data: Vec<u8>) -> Result<ContentHash, ObjectError> {
        let old_data = self.blob_store.get_sync(hash)
            .ok_or_else(|| ObjectError::NotFound(hash.as_hex().to_string()))?;

        let old_node: DagNode = serde_cbor::from_slice(&old_data)
            .map_err(|e| ObjectError::InvalidObject(e.to_string()))?;

        let new_node = DagNode {
            data: Some(data),
            links: old_node.links,
            unixfs: old_node.unixfs,
        };

        let encoded = serde_cbor::to_vec(&new_node)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        let (new_hash, _) = self.blob_store.put_bytes_sync(&encoded)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        Ok(new_hash)
    }

    /// Link an object to another.
    pub async fn link(&self, parent: &ContentHash, child: &ContentHash, name: &str) -> Result<ContentHash, ObjectError> {
        let parent_data = self.blob_store.get_sync(parent)
            .ok_or_else(|| ObjectError::NotFound(parent.as_hex().to_string()))?;

        // Verify child exists (we just check meta, not read data)
        let _child_data = self.blob_store.get_sync(child)
            .ok_or_else(|| ObjectError::NotFound(child.as_hex().to_string()))?;

        let parent_node: DagNode = serde_cbor::from_slice(&parent_data)
            .map_err(|e| ObjectError::InvalidObject(e.to_string()))?;

        let (child_size, _) = self.blob_store.meta(child)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        let mut links = parent_node.links;
        links.push(DagLink {
            name: Some(name.to_string()),
            hash: child.as_hex().to_string(),
            size: child_size,
        });

        let new_node = DagNode {
            data: parent_node.data,
            links,
            unixfs: parent_node.unixfs,
        };

        let encoded = serde_cbor::to_vec(&new_node)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        let (new_hash, _) = self.blob_store.put_bytes_sync(&encoded)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        Ok(new_hash)
    }

    /// Unlink an object.
    pub async fn unlink(&self, parent: &ContentHash, name: &str) -> Result<ContentHash, ObjectError> {
        let parent_data = self.blob_store.get_sync(parent)
            .ok_or_else(|| ObjectError::NotFound(parent.as_hex().to_string()))?;

        let parent_node: DagNode = serde_cbor::from_slice(&parent_data)
            .map_err(|e| ObjectError::InvalidObject(e.to_string()))?;

        let links: Vec<DagLink> = parent_node.links
            .into_iter()
            .filter(|l| l.name.as_ref() != Some(&name.to_string()))
            .collect();

        let new_node = DagNode {
            data: parent_node.data,
            links,
            unixfs: parent_node.unixfs,
        };

        let encoded = serde_cbor::to_vec(&new_node)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        let (new_hash, _) = self.blob_store.put_bytes_sync(&encoded)
            .map_err(|e| ObjectError::Internal(e.to_string()))?;

        Ok(new_hash)
    }
}
