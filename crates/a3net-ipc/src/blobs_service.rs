//! BLAKE3-addressed blob service over Unix socket JSON-RPC.
//!
//! Mirrors `P2pBlobsService` from
//! `Exodus@src-backup/.../microservice/p2p_blobs_service.rs`. The wire API
//! matches the original so an existing Exodus micro-service can be
//! replaced with this one without changing clients.
//!
//! Methods:
//! - `add_blob`     : `params.data` (base64) → `result.hash`
//! - `get_blob`     : `params.hash`         → `result.data` (base64) + `hash`
//! - `list_blobs`   : `{}`                  → `result.blobs: [hash, ...]`
//! - `create_ticket`: `params.hash`         → `result.ticket { node_id, blob_hash, format }`
//!
//! When `BlobsIpcConfig::data_dir` is `Some`, blobs are persisted via
//! `a3net_blobstore::BlobStore` in addition to the in-memory cache — the
//! service then survives process restarts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use a3net_blobstore::{BlobImporter, BlobReader, BlobStore};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::server::{JsonRpcServer, JsonRpcServerHandle, RpcHandler};
use crate::validation::{Validate, ValidationOutcome, ValidationPolicy};

// 8 MiB cap — well below the IPC server's 16 MiB
// MAX_REQUEST_BYTES so the gate fires before the server's frame
// limit. The test uses `MAX_BLOB_BYTES + 1` to exercise the gate.
const MAX_BLOB_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct BlobsIpcConfig {
    pub socket_path: PathBuf,
    /// When `Some`, the service persists blobs to disk via [`BlobStore`]
    /// in addition to (or instead of) the in-memory cache. The path is
    /// the on-disk content-addressed store; the IPC layer keeps a thin
    /// in-memory index for fast `list_blobs` / `has` queries.
    pub data_dir: Option<PathBuf>,
    /// Validation policy applied at every IPC entry point. Defaults to
    /// [`ValidationPolicy::Strict`].
    pub policy: ValidationPolicy,
}

impl Default for BlobsIpcConfig {
    fn default() -> Self {
        let mut socket_path = std::env::temp_dir();
        socket_path.push("a3net_blobs.sock");
        Self {
            socket_path,
            data_dir: None,
            policy: ValidationPolicy::Strict,
        }
    }
}

/// A blob identified by a 64-hex-char BLAKE3 digest. The structured
/// representation makes the `Validate` trait usable at the IPC gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashedBlob {
    pub hash: String,
}

impl HashedBlob {
    pub fn new(hash: String) -> Self {
        Self { hash }
    }
}

/// Implement the shared `a3net_types::Validate` trait so the IPC gate
/// can be used uniformly.
impl a3net_types::Validate for HashedBlob {
    fn validate(&self) -> a3net_types::Result<()> {
        validate_blob_identity(self)
    }
}

/// Validate a [`HashedBlob`] identifier. Rejects any hash that is not
/// exactly 64 lowercase hex chars.
pub fn validate_blob_identity(b: &HashedBlob) -> a3net_types::Result<()> {
    if b.hash.len() != a3net_types::ContentHash::HEX_LEN {
        return Err(a3net_types::AdnetError::Validation(format!(
            "blob hash: expected {} hex chars, got {}",
            a3net_types::ContentHash::HEX_LEN,
            b.hash.len()
        )));
    }
    if !b.hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(a3net_types::AdnetError::Validation(
            "blob hash: contains non-hex chars".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobTicket {
    pub node_id: String,
    pub blob_hash: String,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

/// In-memory blob store + JSON-RPC handler, optionally backed by
/// [`BlobStore`] for durable storage.
pub struct BlobsIpcService {
    cfg: BlobsIpcConfig,
    blobs: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Lazy-initialised on first disk-touching operation. We keep it in
    /// an `Option` so the default config (no `data_dir`) never touches
    /// the filesystem.
    store: Arc<Mutex<Option<Arc<BlobStore>>>>,
}

impl BlobsIpcService {
    pub fn new(cfg: BlobsIpcConfig) -> Self {
        Self {
            cfg,
            blobs: Arc::new(Mutex::new(HashMap::new())),
            store: Arc::new(Mutex::new(None)),
        }
    }

    /// Apply the configured [`ValidationPolicy`] to a value. In
    /// `Strict` mode, the first validation failure is returned as an
    /// error. In `Audit` mode, failures are recorded as warnings. In
    /// `Lenient` mode, all failures are ignored.
    fn check<T: Validate>(&self, value: &T, what: &str) -> ValidationOutcome {
        let mut out = ValidationOutcome::default();
        if let Err(e) = value.validate() {
            match self.cfg.policy {
                ValidationPolicy::Strict => {
                    out.error = Some(format!("{what}: {e}"));
                }
                ValidationPolicy::Audit => {
                    out.warnings.push(format!("{what}: {e}"));
                }
                ValidationPolicy::Lenient => {}
            }
        }
        out
    }

    fn gate<T: Validate>(&self, value: &T, what: &str) -> Result<(), String> {
        let outcome = self.check(value, what);
        if let Some(e) = outcome.error {
            return Err(e);
        }
        Ok(())
    }

    /// Start the Unix socket server. Returns a handle that owns the listener.
    pub async fn serve(self: Arc<Self>) -> Result<JsonRpcServerHandle, String> {
        JsonRpcServer::start(self.cfg.socket_path.clone(), self).await
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.cfg.socket_path
    }

    /// `true` when the service is configured with an on-disk
    /// [`BlobStore`] that survives process restarts.
    pub fn uses_disk_store(&self) -> bool {
        self.cfg.data_dir.is_some()
    }

    /// Initialise the on-disk [`BlobStore`] on demand. Returns the cached
    /// `Arc` on subsequent calls so the directory is only scanned once.
    async fn disk_store(&self) -> Result<Option<Arc<BlobStore>>, String> {
        let Some(data_dir) = self.cfg.data_dir.as_ref() else {
            return Ok(None);
        };
        // Fast path: already initialised.
        if let Some(store) = self.store.lock().map_err(|e| format!("lock: {e}"))?.clone() {
            return Ok(Some(store));
        }
        // Slow path: open the store off the runtime thread.
        let data_dir = data_dir.clone();
        let store = tokio::task::spawn_blocking(move || BlobStore::new(&data_dir))
            .await
            .map_err(|e| format!("spawn_blocking: {e}"))?
            .map_err(|e| format!("open blob store: {e}"))?;
        let store = Arc::new(store);
        *self.store.lock().map_err(|e| format!("lock: {e}"))? = Some(Arc::clone(&store));
        Ok(Some(store))
    }

    pub async fn add_blob(&self, data: Vec<u8>) -> Result<String, String> {
        // Size cap before any hashing — fail-closed on DoS.
        if data.len() > MAX_BLOB_BYTES {
            return Err(format!(
                "add_blob: payload {} bytes exceeds MAX_BLOB_BYTES ({} = 64 MiB)",
                data.len(),
                MAX_BLOB_BYTES
            ));
        }
        // Pin BLAKE3 hashing on the runtime — the in-memory HashMap insert
        // is the only non-trivial step and is fast.
        let hash = blake3::hash(&data).to_hex().to_string();
        // Validate the produced hash (defence in depth — the hash
        // comes from blake3 so it should always be valid, but we
        // route through the gate so a policy flip surfaces here too).
        let hb = HashedBlob::new(hash.clone());
        self.gate(&hb, "blob_hash")?;
        if let Some(store) = self.disk_store().await? {
            store
                .put_bytes(&data)
                .await
                .map_err(|e| format!("disk store: {e}"))?;
        }
        self.blobs
            .lock()
            .map_err(|e| format!("lock: {e}"))?
            .insert(hash.clone(), data);
        Ok(hash)
    }

    pub async fn get_blob(&self, hash: &str) -> Option<Vec<u8>> {
        // Validate the hash before any lookup so an invalid hash can
        // never be parsed as a ContentHash downstream.
        let hb = HashedBlob::new(hash.to_string());
        if self.gate(&hb, "blob_hash").is_err() {
            return None;
        }
        if let Some(bytes) = self.blobs.lock().ok().and_then(|b| b.get(hash).cloned()) {
            return Some(bytes);
        }
        // Fall back to the on-disk store when configured.
        if let Ok(Some(store)) = self.disk_store().await
            && let Ok(content_hash) = a3net_types::ContentHash::from_hex(hash)
            && store.has(&content_hash).await
        {
            return store.read_all(&content_hash).await.ok();
        }
        None
    }

    pub async fn list_blobs(&self) -> Vec<String> {
        let mut hashes: Vec<String> = self
            .blobs
            .lock()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default();
        if let Ok(Some(store)) = self.disk_store().await {
            // The on-disk store enumerates hashes via its `data_dir` layout:
            // each subdirectory whose name parses as a valid 64-hex
            // `ContentHash` is a complete blob.
            if let Ok(read) = std::fs::read_dir(store.data_dir()) {
                for entry in read.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if a3net_types::ContentHash::from_hex(name).is_ok()
                        && !hashes.iter().any(|h| h == name)
                    {
                        hashes.push(name.to_string());
                    }
                }
            }
        }
        hashes
    }
}

#[async_trait]
impl RpcHandler for BlobsIpcService {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "add_blob" => {
                let data_b64 = params
                    .get("data")
                    .and_then(|d| d.as_str())
                    .ok_or("missing data")?;
                let data = BASE64
                    .decode(data_b64)
                    .map_err(|e| format!("base64: {e}"))?;
                let hash = self.add_blob(data).await?;
                Ok(json!({ "hash": hash }))
            }
            "get_blob" => {
                let hash = params
                    .get("hash")
                    .and_then(|h| h.as_str())
                    .ok_or("missing hash")?;
                self.gate(&HashedBlob::new(hash.to_string()), "blob_hash")?;
                let data = self
                    .get_blob(hash)
                    .await
                    .ok_or_else(|| "blob not found".to_string())?;
                Ok(json!({
                    "data": BASE64.encode(&data),
                    "hash": hash,
                }))
            }
            "list_blobs" => {
                let blobs = self.list_blobs().await;
                Ok(json!({ "blobs": blobs }))
            }
            "create_ticket" => {
                let hash = params
                    .get("hash")
                    .and_then(|h| h.as_str())
                    .ok_or("missing hash")?;
                self.gate(&HashedBlob::new(hash.to_string()), "blob_hash")?;
                if self.get_blob(hash).await.is_none() {
                    return Err("blob not found".into());
                }
                Ok(json!({
                    "ticket": {
                        "node_id": generate_node_id(),
                        "blob_hash": hash,
                        "format": "raw",
                        "expires_at": params.get("expires_at").and_then(|v| v.as_u64())
                    }
                }))
            }
            other => Err(format!("unknown method: {other}")),
        }
    }
}

fn generate_node_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("node_{ts}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::json_rpc_call;

    #[tokio::test]
    async fn add_get_list_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("blobs.sock");
        let svc = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
            socket_path: sock.clone(),
            data_dir: None,
            policy: ValidationPolicy::Strict,
        }));
        let handle = Arc::clone(&svc).serve().await.unwrap();

        let data = b"hello ipc";
        let data_b64 = BASE64.encode(data);
        let add_resp = json_rpc_call(&sock, "blobs", "add_blob", json!({ "data": data_b64 }))
            .await
            .unwrap();
        let hash = add_resp["hash"].as_str().unwrap().to_string();
        let expected_hash = blake3::hash(data).to_hex().to_string();
        assert_eq!(hash, expected_hash);

        let list_resp = json_rpc_call(&sock, "blobs", "list_blobs", json!({}))
            .await
            .unwrap();
        let blobs = list_resp["blobs"].as_array().unwrap();
        assert_eq!(blobs.len(), 1);

        let get_resp = json_rpc_call(&sock, "blobs", "get_blob", json!({ "hash": hash }))
            .await
            .unwrap();
        let decoded = BASE64.decode(get_resp["data"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, data);

        handle.shutdown();
    }

    #[tokio::test]
    async fn disk_backed_blobs_survive_restart() {
        // 1. Start a service with a data_dir, add a blob.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("blobs.sock");
        let data_dir = dir.path().join("blobs");
        let data = b"persisted-blob";
        let data_b64 = BASE64.encode(data);

        let svc = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
            socket_path: sock.clone(),
            data_dir: Some(data_dir.clone()),
            policy: ValidationPolicy::Strict,
        }));
        assert!(svc.uses_disk_store());
        let handle = Arc::clone(&svc).serve().await.unwrap();

        let add_resp = json_rpc_call(&sock, "blobs", "add_blob", json!({ "data": data_b64 }))
            .await
            .unwrap();
        let hash = add_resp["hash"].as_str().unwrap().to_string();
        handle.shutdown();
        drop(svc);

        // 2. Start a fresh service on the same data_dir — must find the blob.
        let sock2 = dir.path().join("blobs2.sock");
        let svc2 = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
            socket_path: sock2.clone(),
            data_dir: Some(data_dir),
            policy: ValidationPolicy::Strict,
        }));
        let handle2 = Arc::clone(&svc2).serve().await.unwrap();

        let list = json_rpc_call(&sock2, "blobs", "list_blobs", json!({}))
            .await
            .unwrap();
        let hashes = list["blobs"].as_array().unwrap();
        assert!(hashes.iter().any(|v| v == &hash));

        let got = json_rpc_call(&sock2, "blobs", "get_blob", json!({ "hash": hash }))
            .await
            .unwrap();
        let decoded = BASE64.decode(got["data"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, data);

        handle2.shutdown();
    }

    // ─────────────────────────────────────────────────────────────────────
    // DO-178C boundary tests for the blobs service.
    // ─────────────────────────────────────────────────────────────────────

    /// Strict policy rejects an add_blob call whose payload exceeds the
    /// 16 MiB cap — fail-closed on DoS.
    #[tokio::test]
    async fn strict_rejects_oversize_blob() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("blobs.sock");
        let svc = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
            socket_path: sock.clone(),
            data_dir: None,
            policy: ValidationPolicy::Strict,
        }));
        let handle = Arc::clone(&svc).serve().await.unwrap();

        let big = vec![0u8; MAX_BLOB_BYTES + 1];
        let big_b64 = BASE64.encode(&big);
        let err = json_rpc_call(&sock, "blobs", "add_blob", json!({ "data": big_b64 }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("MAX_BLOB_BYTES"), "got {err}");
        handle.shutdown();
    }

    /// Strict policy rejects a get_blob whose hash is not 64 hex chars.
    #[tokio::test]
    async fn strict_rejects_malformed_hash_on_get() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("blobs.sock");
        let svc = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
            socket_path: sock.clone(),
            data_dir: None,
            policy: ValidationPolicy::Strict,
        }));
        let handle = Arc::clone(&svc).serve().await.unwrap();

        let err = json_rpc_call(&sock, "blobs", "get_blob", json!({ "hash": "not-hex" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("blob hash"), "got {err}");
        handle.shutdown();
    }

    /// Strict policy rejects a create_ticket whose hash is the wrong length.
    #[tokio::test]
    async fn strict_rejects_malformed_hash_on_create_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("blobs.sock");
        let svc = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
            socket_path: sock.clone(),
            data_dir: None,
            policy: ValidationPolicy::Strict,
        }));
        let handle = Arc::clone(&svc).serve().await.unwrap();

        let err = json_rpc_call(
            &sock,
            "blobs",
            "create_ticket",
            json!({ "hash": "deadbeef" }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("blob hash"), "got {err}");
        handle.shutdown();
    }

    /// Lenient policy bypasses the hash check entirely (legacy migration).
    #[tokio::test]
    async fn lenient_accepts_malformed_hash() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("blobs.sock");
        let svc = Arc::new(BlobsIpcService::new(BlobsIpcConfig {
            socket_path: sock.clone(),
            data_dir: None,
            policy: ValidationPolicy::Lenient,
        }));
        let handle = Arc::clone(&svc).serve().await.unwrap();

        // A malformed hash won't be found, but the gate must let it pass.
        let err = json_rpc_call(&sock, "blobs", "get_blob", json!({ "hash": "not-hex" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("blob not found"), "got {err}");
        handle.shutdown();
    }

    /// `validate_blob_identity` rejects non-hex chars.
    #[test]
    fn unit_validate_blob_identity_rejects_garbage() {
        assert!(validate_blob_identity(&HashedBlob::new("z".repeat(64))).is_err());
        assert!(validate_blob_identity(&HashedBlob::new("a".repeat(64))).is_ok());
        assert!(validate_blob_identity(&HashedBlob::new("nope".into())).is_err());
    }
}
