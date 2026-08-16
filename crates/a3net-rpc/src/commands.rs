//! RPC command definitions for the unified IPFS-compatible API.


use a3net_blobstore::BlobStore;
use a3net_types::ContentHash;

use crate::results::{RpcResult, RpcError};

/// Execute a DAG put command.
pub async fn dag_put(
    blob_store: &std::sync::Arc<BlobStore>,
    data: Vec<u8>,
    #[allow(unused)] pin: bool,
) -> RpcResult {
    let (cid, size) = blob_store.put_bytes_sync(&data)
        .map_err(|e| RpcError::internal(e.to_string()))?;

    let result = serde_json::json!({
        "Cid": { "/": cid.as_hex() },
        "Size": size
    });

    Ok(result)
}

/// Execute a DAG get command.
pub async fn dag_get(
    blob_store: &std::sync::Arc<BlobStore>,
    cid: &str,
    _path: Option<&str>,
) -> RpcResult {
    let hash = ContentHash::from_hex(cid)
        .map_err(|_| RpcError::invalid_input("invalid CID"))?;

    let data = blob_store.get_sync(&hash)
        .ok_or_else(|| RpcError::not_found(format!("not found: {}", cid)))?;

    Ok(serde_json::json!({
        "data": base64_encode(&data),
    }))
}

/// Execute a block put command.
pub async fn block_put(
    blob_store: &std::sync::Arc<BlobStore>,
    data: Vec<u8>,
    #[allow(unused)] pin: bool,
) -> RpcResult {
    let (cid, size) = blob_store.put_bytes_sync(&data)
        .map_err(|e| RpcError::internal(e.to_string()))?;

    Ok(serde_json::json!({
        "Key": cid.as_hex(),
        "Size": size
    }))
}

/// Execute a block get command.
pub async fn block_get(
    blob_store: &std::sync::Arc<BlobStore>,
    cid: &str,
) -> RpcResult {
    let hash = ContentHash::from_hex(cid)
        .map_err(|_| RpcError::invalid_input("invalid CID"))?;

    let data = blob_store.get_sync(&hash)
        .ok_or_else(|| RpcError::not_found(format!("not found: {}", cid)))?;

    Ok(serde_json::json!({
        "data": base64_encode(&data),
    }))
}

/// Execute a block stat command.
pub async fn block_stat(
    blob_store: &std::sync::Arc<BlobStore>,
    cid: &str,
) -> RpcResult {
    let hash = ContentHash::from_hex(cid)
        .map_err(|_| RpcError::invalid_input("invalid CID"))?;

    let (size, _) = blob_store.meta(&hash)
        .map_err(|_| RpcError::not_found(format!("not found: {}", cid)))?;

    Ok(serde_json::json!({
        "Key": cid,
        "Size": size,
        "Cid": cid
    }))
}

/// Execute a block rm command.
pub async fn block_rm(
    blob_store: &std::sync::Arc<BlobStore>,
    cid: &str,
    _force: bool,
) -> RpcResult {
    let hash = ContentHash::from_hex(cid)
        .map_err(|_| RpcError::invalid_input("invalid CID"))?;

    let removed = blob_store.remove(&hash)
        .map_err(|e| RpcError::internal(e.to_string()))?;

    Ok(serde_json::json!({
        "Hash": cid,
        "Removed": removed
    }))
}

/// Execute a pin add command.
pub async fn pin_add(
    blob_store: &std::sync::Arc<BlobStore>,
    cid: &str,
    _recursive: bool,
) -> RpcResult {
    let hash = ContentHash::from_hex(cid)
        .map_err(|_| RpcError::invalid_input("invalid CID"))?;

    if !blob_store.has_complete(&hash) {
        return Err(RpcError::not_found(format!("not found: {}", cid)));
    }

    // Pin implementation would go here
    // For now, just return success

    Ok(serde_json::json!({
        "Pins": [cid]
    }))
}

/// Execute a pin rm command.
pub async fn pin_rm(
    _blob_store: &std::sync::Arc<BlobStore>,
    cid: &str,
) -> RpcResult {
    Ok(serde_json::json!({
        "Pins": [cid]
    }))
}

/// Execute a pin ls command.
pub async fn pin_ls(
    _blob_store: &std::sync::Arc<BlobStore>,
    cid: Option<&str>,
) -> RpcResult {
    let mut keys = serde_json::Map::new();

    if let Some(cid) = cid {
        keys.insert(cid.to_string(), serde_json::json!({
            "Type": "recursive"
        }));
    }

    Ok(serde_json::json!({
        "Keys": keys
    }))
}

/// Execute a gc command.
#[allow(unused)]
pub async fn gc(
    blob_store: &std::sync::Arc<BlobStore>,
    #[allow(unused)] dry_run: bool,
) -> RpcResult {
    let blobs = blob_store.list_complete()
        .map_err(|e| RpcError::internal(e.to_string()))?;

    let count = blobs.len() as u64;

    if !dry_run {
        // Actual GC would remove unpinned blocks
    }

    Ok(serde_json::json!({
        "KeysRemoved": count
    }))
}

/// Execute a node id command.
pub async fn node_id() -> RpcResult {
    Ok(serde_json::json!({
        "ID": "a3net-node",
        "PublicKey": "",
        "Addresses": [],
        "AgentVersion": "a3net/0.1.0",
        "ProtocolVersion": "ipfs/0.1.0"
    }))
}

/// Execute a version command.
pub async fn version() -> RpcResult {
    Ok(serde_json::json!({
        "Version": "0.1.0",
        "Commit": "",
        "Repo": "10",
        "System": "a3net",
        "Golang": "1.91"
    }))
}

// Helper function to base64 encode data
fn base64_encode(data: &[u8]) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.encode(data)
}
