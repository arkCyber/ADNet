//! `a3chat` media (attachment) service.
//!
//! Bridges the chat-attachment lifecycle onto the disk-backed,
//! BLAKE3 content-addressed [`a3net_blobstore::BlobStore`].
//!
//! # Why `a3net-blobstore`
//!
//! Reusing `a3net-blobstore::BlobStore` gives `a3chat`:
//!
//! * Immutable, content-addressed blobs (`ContentHash` = BLAKE3
//!   digest), which map naturally to chat attachments that should
//!   be deduplicated across conversations.
//! * Provenance: every attachment is identified by the hash of its
//!   bytes, so the network can fetch it from any peer holding a
//!   replica.
//! * Reproducibility (DO-178C §5.3): the same bytes always produce
//!   the same `ContentHash` — no hidden state.
//! * Disk hygiene via the existing [`PinSet`](a3net_blobstore::PinSet)
//!   and garbage collector.
//!
//! # Surface
//!
//! Four RPC methods (see [`METHODS`]) back the lifecycle:
//!
//! | RPC                          | Purpose                                         |
//! |------------------------------|-------------------------------------------------|
//! | `a3chat.media.upload_init`   | Begin an upload, return a temporary upload token |
//! | `a3chat.media.upload_chunk`  | Append data to the in-flight upload             |
//! | `a3chat.media.upload_finalize` | Seal the upload; promote to a pinned attachment |
//! | `a3chat.media.download_get`  | Fetch a finalized attachment by `ContentHash`   |

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use a3net_blobstore::pin_set::{now_unix, PinSet};
use a3net_blobstore::store::BlobStore;
use a3net_types::content::ContentHash;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::task;
use std::collections::BTreeSet;
use uuid::Uuid;

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;

use crate::error::{app_to_domain, AppError, AppResult};

/// Maximum size of a single attachment after finalisation (32 MiB).
pub const MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum bytes accepted in a single `upload_chunk` call (1 MiB).
pub const MAX_CHUNK_BYTES: usize = 1024 * 1024;

/// RPC method-name constants owned by this module.
pub const METHODS: &[&str] = &[
    "a3chat.media.upload_init",
    "a3chat.media.upload_chunk",
    "a3chat.media.upload_finalize",
    "a3chat.media.download_get",
];

/// Configuration for [`MediaService`].
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// Directory where attachments are stored on disk.
    pub data_dir: PathBuf,
    /// Maximum bytes per finalised attachment (defence against DoS).
    pub max_attachment_bytes: usize,
    /// Maximum bytes per chunk.
    pub max_chunk_bytes: usize,
}

impl MediaConfig {
    /// Build a config under `<base>/media`.
    pub fn under_base(base: &Path) -> Self {
        Self {
            data_dir: base.join("media"),
            max_attachment_bytes: MAX_ATTACHMENT_BYTES,
            max_chunk_bytes: MAX_CHUNK_BYTES,
        }
    }
}

/// In-flight upload state.
#[derive(Debug)]
struct InFlightUpload {
    /// Bytes accumulated so far (kept only until `finalize`).
    buf: Vec<u8>,
    /// Owner of the upload (security: cross-check on every chunk).
    owner: UserId,
    /// Configured maximum (snapshotted to make limits audit-friendly).
    max_bytes: usize,
}

/// Media service. Thread-safe and cheaply cloneable.
#[derive(Clone)]
pub struct MediaService {
    inner: Arc<MediaInner>,
}

struct MediaInner {
    store: BlobStore,
    pins: Mutex<PinSet>,
    cfg: MediaConfig,
    /// In-flight uploads keyed by an opaque upload token.
    uploads: Mutex<HashMap<String, InFlightUpload>>,
}

/// Service-level error variants.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("upload session not found: {0}")]
    UnknownSession(String),
    #[error("upload session owner mismatch")]
    OwnerMismatch,
    #[error("upload chunk exceeds max size")]
    ChunkTooLarge,
    #[error("upload size exceeds max attachment size")]
    AttachmentTooLarge,
    #[error("blob store error: {0}")]
    BlobStore(String),
    #[error("attachment not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl From<MediaError> for AppError {
    fn from(e: MediaError) -> Self {
        match e {
            MediaError::UnknownSession(_) | MediaError::NotFound(_) => {
                AppError::Domain(e.to_string())
            }
            MediaError::OwnerMismatch => AppError::Forbidden(e.to_string()),
            MediaError::ChunkTooLarge
            | MediaError::AttachmentTooLarge
            | MediaError::InvalidInput(_) => AppError::Domain(e.to_string()),
            MediaError::BlobStore(_) => AppError::Storage(e.to_string()),
        }
    }
}

impl From<MediaError> for A3chatError {
    fn from(e: MediaError) -> Self {
        app_to_domain(AppError::from(e))
    }
}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        MediaError::BlobStore(e.to_string())
    }
}

impl MediaService {
    /// Open the service on disk.
    pub fn open(cfg: &MediaConfig) -> AppResult<Self> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let store = BlobStore::new(&cfg.data_dir)
            .map_err(|e| AppError::Internal(format!("BlobStore::new: {e}")))?;
        Ok(Self {
            inner: Arc::new(MediaInner {
                store,
                pins: Mutex::new(PinSet::default()),
                cfg: cfg.clone(),
                uploads: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Open the service in a temporary directory (test helper).
    pub fn open_in(dir: &Path) -> AppResult<Self> {
        let cfg = MediaConfig {
            data_dir: dir.to_path_buf(),
            max_attachment_bytes: MAX_ATTACHMENT_BYTES,
            max_chunk_bytes: MAX_CHUNK_BYTES,
        };
        Self::open(&cfg)
    }

    /// Health snapshot — exposed for the RPC `a3chat.media.health` probe.
    pub fn health(&self) -> MediaHealth {
        MediaHealth {
            store_healthy: self.inner.store.is_healthy(),
            data_dir: self.inner.cfg.data_dir.display().to_string(),
            max_attachment_bytes: self.inner.cfg.max_attachment_bytes,
            max_chunk_bytes: self.inner.cfg.max_chunk_bytes,
        }
    }

    /// Returns the canonical data dir (for diagnostics).
    pub fn data_dir(&self) -> &Path {
        &self.inner.cfg.data_dir
    }

    /// Begin an upload session.
    pub async fn upload_init(
        &self,
        owner: UserId,
        _mime_type: Option<String>,
    ) -> AppResult<String> {
        let token = Uuid::new_v4().to_string();
        let upload = InFlightUpload {
            buf: Vec::new(),
            owner,
            max_bytes: self.inner.cfg.max_attachment_bytes,
        };
        self.inner.uploads.lock().insert(token.clone(), upload);
        Ok(token)
    }

    /// Append a chunk to an in-flight upload.
    pub async fn upload_chunk(
        &self,
        owner: UserId,
        token: &str,
        chunk: Vec<u8>,
    ) -> AppResult<UploadChunkResult> {
        if chunk.len() > self.inner.cfg.max_chunk_bytes {
            return Err(MediaError::ChunkTooLarge.into());
        }

        let new_len = {
            let mut guard = self.inner.uploads.lock();
            let upload = guard
                .get_mut(token)
                .ok_or_else(|| MediaError::UnknownSession(token.to_string()))?;
            if upload.owner != owner {
                return Err(MediaError::OwnerMismatch.into());
            }
            let next = upload.buf.len().saturating_add(chunk.len());
            if next > upload.max_bytes {
                return Err(MediaError::AttachmentTooLarge.into());
            }
            upload.buf.extend_from_slice(&chunk);
            upload.buf.len()
        };

        Ok(UploadChunkResult {
            bytes_received: new_len,
            max_bytes: self.inner.cfg.max_attachment_bytes,
        })
    }

    /// Seal the upload and persist the resulting blob.
    pub async fn upload_finalize(
        &self,
        owner: UserId,
        token: &str,
        filename: Option<String>,
    ) -> AppResult<UploadFinalizeResult> {
        // 1. Extract bytes under lock.
        let bytes = {
            let mut guard = self.inner.uploads.lock();
            let upload = guard
                .remove(token)
                .ok_or_else(|| MediaError::UnknownSession(token.to_string()))?;
            if upload.owner != owner {
                return Err(MediaError::OwnerMismatch.into());
            }
            upload.buf
        };

        if bytes.is_empty() {
            return Err(MediaError::InvalidInput(
                "empty attachment cannot be finalised".into(),
            )
            .into());
        }

        // 2. Offload the disk I/O to a blocking thread.
        let store = self.inner.store.clone();
        let bytes = Arc::new(bytes);
        let result: Result<(ContentHash, u64), AppError> =
            task::spawn_blocking(move || -> Result<(ContentHash, u64), AppError> {
                store
                    .put_bytes_sync(&bytes)
                    .map_err(|e| AppError::Internal(format!("put_bytes_sync: {e}")))
            })
            .await
            .map_err(|e| AppError::Internal(format!("join: {e}")))?;

        let (hash, size) = result?;

        // 3. Pin it.
        {
            let mut pins = self.inner.pins.lock();
            pins.add(&hash, true, BTreeSet::new(), now_unix());
        }

        Ok(UploadFinalizeResult {
            hash: hex::encode(hash.as_bytes_array()),
            size,
            filename,
        })
    }

    /// Fetch a finalised attachment by content hash (hex BLAKE3).
    pub async fn download_get(
        &self,
        _owner: UserId,
        hash_hex: &str,
    ) -> AppResult<DownloadResult> {
        let hash = ContentHash::from_hex(hash_hex)
            .map_err(|e| MediaError::InvalidInput(format!("bad content hash: {e}")))?;

        let store = self.inner.store.clone();
        let hash_clone = hash.clone();
        let bytes: Option<Vec<u8>> = task::spawn_blocking(move || store.get_sync(&hash_clone))
            .await
            .map_err(|e| AppError::Internal(format!("join: {e}")))?;

        match bytes {
            Some(data) => Ok(DownloadResult {
                hash: hash_hex.to_string(),
                size: data.len() as u64,
                data_hex: hex::encode(&data),
            }),
            None => Err(MediaError::NotFound(hash_hex.to_string()).into()),
        }
    }
}

// ---------- DTOs ------------------------------------------------------

/// Result of `upload_chunk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadChunkResult {
    pub bytes_received: usize,
    pub max_bytes: usize,
}

/// Result of `upload_finalize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadFinalizeResult {
    /// BLAKE3 content hash (hex).
    pub hash: String,
    pub size: u64,
    pub filename: Option<String>,
}

/// Result of `download_get`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub hash: String,
    pub size: u64,
    pub data_hex: String,
}

/// Health snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaHealth {
    pub store_healthy: bool,
    pub data_dir: String,
    pub max_attachment_bytes: usize,
    pub max_chunk_bytes: usize,
}

// ---------- RPC dispatch ---------------------------------------------

/// Top-level dispatcher for any RPC method starting with `a3chat.media.`.
pub async fn dispatch(
    svc: Arc<MediaService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        "a3chat.media.upload_init" => {
            let mime_type = params
                .get("mimeType")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let token = svc.upload_init(owner.clone(), mime_type).await?;
            Ok(serde_json::json!({ "token": token }))
        }
        "a3chat.media.upload_chunk" => {
            let token = params
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'token'".into()))?;
            let data_hex = params
                .get("dataHex")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'dataHex'".into()))?;
            let chunk = decode_hex(data_hex).map_err(A3chatError::InvalidInput)?;
            let r = svc.upload_chunk(owner.clone(), token, chunk).await?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        "a3chat.media.upload_finalize" => {
            let token = params
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'token'".into()))?;
            let filename = params
                .get("filename")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let r = svc.upload_finalize(owner.clone(), token, filename).await?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        "a3chat.media.download_get" => {
            let hash = params
                .get("hash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("missing 'hash'".into()))?;
            let r = svc.download_get(owner.clone(), hash).await?;
            serde_json::to_value(r).map_err(A3chatError::from)
        }
        "a3chat.media.health" => {
            let h = svc.health();
            serde_json::to_value(h).map_err(A3chatError::from)
        }
        m => Err(A3chatError::InvalidInput(format!(
            "unknown media method: {m}"
        ))),
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd-length hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

// ---------- Tests -----------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir();
        let unique = format!("a3chat-media-test-{}", Uuid::new_v4());
        let p = base.join(unique);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn tiny_cfg(dir: &Path, max_chunk: usize, max_at: usize) -> MediaConfig {
        MediaConfig {
            data_dir: dir.to_path_buf(),
            max_chunk_bytes: max_chunk,
            max_attachment_bytes: max_at,
        }
    }

    #[tokio::test]
    async fn upload_init_chunk_finalize_round_trip() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("test-user");

        let token = svc
            .upload_init(owner.clone(), Some("text/plain".into()))
            .await
            .unwrap();

        let payload = b"hello a3chat-media round trip".to_vec();
        let r = svc
            .upload_chunk(owner.clone(), &token, payload.clone())
            .await
            .unwrap();
        assert_eq!(r.bytes_received, payload.len());

        let fin = svc
            .upload_finalize(owner.clone(), &token, Some("hello.txt".into()))
            .await
            .unwrap();
        assert_eq!(fin.size as usize, payload.len());
        assert_eq!(fin.hash.len(), 64); // 32 bytes hex

        let downloaded = svc
            .download_get(owner.clone(), &fin.hash)
            .await
            .unwrap();
        assert_eq!(downloaded.size as usize, payload.len());
        assert_eq!(downloaded.data_hex, hex::encode(&payload));
    }

    #[tokio::test]
    async fn upload_finalize_rejects_empty() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let token = svc.upload_init(owner.clone(), None).await.unwrap();
        let err = svc.upload_finalize(owner, &token, None).await.unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn upload_chunk_rejects_oversized_chunk() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, 16, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let token = svc.upload_init(owner.clone(), None).await.unwrap();
        let err = svc
            .upload_chunk(owner, &token, vec![0u8; 32])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn upload_chunk_rejects_unknown_token() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let err = svc
            .upload_chunk(owner, "no-such-token", vec![1, 2, 3])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn upload_chunk_rejects_wrong_owner() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner_a = UserId::new("alice");
        let owner_b = UserId::new("bob");
        let token = svc.upload_init(owner_a.clone(), None).await.unwrap();
        let err = svc
            .upload_chunk(owner_b, &token, vec![1, 2, 3])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn download_get_returns_not_found_for_unknown_hash() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let owner = UserId::new("u");
        let fake_hash = hex::encode([0u8; 32]);
        let err = svc.download_get(owner, &fake_hash).await.unwrap_err();
        assert!(matches!(err, AppError::Domain(_)));
    }

    #[tokio::test]
    async fn health_reports_store() {
        let dir = tmpdir();
        let svc = MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap();
        let h = svc.health();
        assert_eq!(h.max_attachment_bytes, MAX_ATTACHMENT_BYTES);
        assert!(h.data_dir.ends_with("media") || h.data_dir.contains(&dir.file_name().unwrap().to_string_lossy().to_string()));
    }

    #[tokio::test]
    async fn dispatch_round_trip() {
        let dir = tmpdir();
        let svc = Arc::new(
            MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap(),
        );
        let owner = UserId::new("alice");

        // init
        let v = dispatch(
            svc.clone(),
            "a3chat.media.upload_init",
            &owner,
            serde_json::json!({"mimeType":"text/plain"}),
        )
        .await
        .unwrap();
        let token = v["token"].as_str().unwrap().to_string();

        // chunk
        let payload = b"dispatch payload".to_vec();
        let _ = dispatch(
            svc.clone(),
            "a3chat.media.upload_chunk",
            &owner,
            serde_json::json!({"token": token, "dataHex": hex::encode(&payload)}),
        )
        .await
        .unwrap();

        // finalize
        let fin = dispatch(
            svc.clone(),
            "a3chat.media.upload_finalize",
            &owner,
            serde_json::json!({"token": token, "filename": "f.txt"}),
        )
        .await
        .unwrap();
        let h = fin["hash"].as_str().unwrap().to_string();

        // download
        let d = dispatch(
            svc.clone(),
            "a3chat.media.download_get",
            &owner,
            serde_json::json!({"hash": h}),
        )
        .await
        .unwrap();
        assert_eq!(d["data_hex"].as_str().unwrap(), hex::encode(&payload));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let dir = tmpdir();
        let svc = Arc::new(
            MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap(),
        );
        let owner = UserId::new("alice");
        let err = dispatch(
            svc.clone(),
            "a3chat.media.no_such_method",
            &owner,
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_missing_field_errors() {
        let dir = tmpdir();
        let svc = Arc::new(
            MediaService::open(&tiny_cfg(&dir, MAX_CHUNK_BYTES, MAX_ATTACHMENT_BYTES)).unwrap(),
        );
        let owner = UserId::new("alice");
        let err = dispatch(
            svc.clone(),
            "a3chat.media.upload_chunk",
            &owner,
            serde_json::json!({"token": "x"}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_method_count_matches_methods_const() {
        assert_eq!(METHODS.len(), 4);
        assert!(METHODS.contains(&"a3chat.media.upload_init"));
        assert!(METHODS.contains(&"a3chat.media.download_get"));
    }
}
