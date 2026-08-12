//! GraphSync extensions and helpers.
//!
//! This module provides additional utilities for GraphSync integration,
//! including re-exports from the graphsync module and extension traits.

use adnet_types::cid::Cid;
use adnet_types::dag_codec;
use adnet_types::graphsync::BlockStore;

/// Extension trait for convenient DAG operations on CIDs.
pub trait DagExt {
    /// Check if this CID represents a directory-like node.
    fn is_directory_block(&self, data: &[u8]) -> bool;

    /// Get the total size of a DAG node.
    fn dag_node_size(&self, data: &[u8]) -> u64;
}

impl DagExt for Cid {
    fn is_directory_block(&self, data: &[u8]) -> bool {
        dag_codec::is_directory(self, data)
    }

    fn dag_node_size(&self, data: &[u8]) -> u64 {
        dag_codec::dag_size(self, data).unwrap_or(data.len() as u64)
    }
}

/// Helper to calculate the total size of a DAG subtree.
pub fn dag_subtree_size(store: &dyn BlockStore, root: &Cid) -> u64 {
    let mut total = 0u64;
    let mut to_visit = vec![root.clone()];
    let mut visited = std::collections::HashSet::new();

    while let Some(cid) = to_visit.pop() {
        if visited.contains(&cid) {
            continue;
        }
        visited.insert(cid.clone());

        if let Some(data) = store.get(&cid) {
            total += data.len() as u64;
            for child in store.links(&cid) {
                if !visited.contains(&child) {
                    to_visit.push(child);
                }
            }
        }
    }

    total
}
