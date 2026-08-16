//! Refs API for listing references to objects.
//!
//! This module provides IPFS refs operations including:
//! - Listing references to an object
//! - Refs with expiration

use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_types::ContentHash;
use serde::{Deserialize, Serialize};

use crate::dag::DagNode;

/// Refs API errors.
#[derive(Debug, thiserror::Error)]
pub enum RefsError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid object: {0}")]
    InvalidObject(String),

    #[error("internal error: {0}")]
    Internal(String),
}

/// A reference from one object to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ref {
    /// The source object.
    #[serde(rename = "Ref")]
    pub ref_str: String,
    /// The referenced object.
    #[serde(rename = "Err")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

/// Refs service.
#[derive(Clone)]
pub struct RefsService {
    blob_store: Arc<BlobStore>,
}

impl RefsService {
    /// Create a new refs service.
    pub fn new(blob_store: Arc<BlobStore>) -> Self {
        Self { blob_store }
    }

    /// List all references to a given object (non-recursive, single level).
    pub async fn list(&self, hash: &ContentHash) -> Result<Vec<Ref>, RefsError> {
        let data = self.blob_store.get_sync(hash)
            .ok_or_else(|| RefsError::NotFound(hash.as_hex().to_string()))?;

        let node: DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| RefsError::InvalidObject(e.to_string()))?;

        let refs: Vec<Ref> = node.links
            .iter()
            .map(|link| Ref {
                ref_str: format!("{} {}", hash.as_hex(), link.hash),
                err: None,
            })
            .collect();

        Ok(refs)
    }

    /// Get all direct child CIDs for a given object.
    pub fn get_direct_links(&self, hash: &ContentHash) -> Result<Vec<ContentHash>, RefsError> {
        let data = self.blob_store.get_sync(hash)
            .ok_or_else(|| RefsError::NotFound(hash.as_hex().to_string()))?;

        let node: DagNode = serde_cbor::from_slice(&data)
            .map_err(|e| RefsError::InvalidObject(e.to_string()))?;

        let links: Vec<ContentHash> = node.links
            .iter()
            .filter_map(|link| ContentHash::from_hex(&link.hash).ok())
            .collect();

        Ok(links)
    }
}
