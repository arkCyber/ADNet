//! `ProfileService` — bridges `a3net-userstore` into the a3chat
//! runtime so every consumer (RPC, CLI, Tauri) reads/writes the
//! **same** profile data as every other A3Net sub-crate.
//!
//! ## Why this service exists (N-06 in
//! `AUDIT_A3CHAT_NET_INTEGRATION.md`)
//!
//! Before this module, a3chat had no notion of:
//!
//! * a stable 12-digit Exodus ID (`a3net_roster::stable_digit_from_node`);
//! * an avatar blob reference (the actual blob lives in
//!   `a3net-blobstore`);
//! * a per-user public-key directory for trust bindings;
//! * a per-user device list for cross-device sync;
//! * user preferences (notification, language, theme, …).
//!
//! All of these are now exposed via RPC through the
//! `a3chat.profile.*` namespace.
//!
//! DO-178C §6.3 — *fail-safe*: every method returns
//! `AppResult<T>` so callers never see raw SQLite errors; the
//! `Display` form is stable for the JSON-RPC error envelope.

use std::sync::Arc;

use base64::Engine as _;

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;
use a3net_blobstore::BlobStore;
use a3net_userstore::model::{
    AvatarBlob, DeviceClass, PublicKeyAlgorithm, UserDevice, UserKind, UserPreferences, UserProfile,
    UserPublicKey,
};
use a3net_userstore::store::UserStore;
use a3net_userstore::sqlite::{SqliteUserStore, SqliteUserStoreConfig};

use crate::error::{AppError, AppResult};

/// Maximum avatar payload size (4 MiB). Mirrors the
/// `media_service::MAX_ATTACHMENT_BYTES` envelope — keeping the two
/// limits aligned avoids confusion at the application layer.
pub const MAX_AVATAR_BYTES: usize = 4 * 1024 * 1024;

/// MIME types accepted by [`ProfileService::upload_avatar`]. The
/// list is intentionally small — DO-178C §6.3 *fail-safe defaults*:
/// anything outside this set is rejected at the boundary.
pub const ALLOWED_AVATAR_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
];

/// RPC method constants — match `a3chat-core/src/rpc.rs`. We
/// re-export the canonical strings here so callers (CLI, Tauri,
/// tests) only need one source of truth.
pub const PROFILE_GET: &str = A3chatRpcMethod::PROFILE_GET;
pub const PROFILE_PUT: &str = A3chatRpcMethod::PROFILE_PUT;
pub const PROFILE_PREFERENCES_PUT: &str = A3chatRpcMethod::PROFILE_PREFERENCES_PUT;
pub const PROFILE_PUBLIC_KEY_ADD: &str = A3chatRpcMethod::PROFILE_PUBLIC_KEY_ADD;
pub const PROFILE_PUBLIC_KEY_LIST: &str = A3chatRpcMethod::PROFILE_PUBLIC_KEY_LIST;
pub const PROFILE_PUBLIC_KEY_REVOKE: &str = A3chatRpcMethod::PROFILE_PUBLIC_KEY_REVOKE;
pub const PROFILE_PUBLIC_KEY_LABEL: &str = "a3chat.profile.public_key.label";
pub const PROFILE_DEVICE_REGISTER: &str = A3chatRpcMethod::PROFILE_DEVICE_REGISTER;
pub const PROFILE_DEVICE_LIST: &str = A3chatRpcMethod::PROFILE_DEVICE_LIST;
pub const PROFILE_DIGIT_GET: &str = A3chatRpcMethod::PROFILE_DIGIT_GET;
pub const PROFILE_AVATAR_SET: &str = A3chatRpcMethod::PROFILE_AVATAR_SET;
pub const PROFILE_AVATAR_UPLOAD: &str = "a3chat.profile.avatar.upload";
pub const PROFILE_AVATAR_GET: &str = "a3chat.profile.avatar.get";
pub const PROFILE_AVATAR_REMOVE: &str = "a3chat.profile.avatar.remove";
pub const PROFILE_KIND_GET: &str = "a3chat.profile.kind.get";
pub const PROFILE_KIND_SET: &str = "a3chat.profile.kind.set";

/// JSON-RPC dispatcher — routes `a3chat.profile.*` methods to the
/// matching service method.
pub async fn dispatch(
    svc: Arc<ProfileService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    let r: AppResult<serde_json::Value> = match method {
        PROFILE_GET => svc.get_profile(owner).await.map(|opt| {
            opt.map(|p| serde_json::to_value(&p).unwrap_or(serde_json::Value::Null))
                .unwrap_or(serde_json::Value::Null)
        }),
        PROFILE_PUT => {
            let profile: UserProfile = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("profile parse: {e}")))?;
            svc.upsert_profile(profile).await?;
            Ok(serde_json::json!({"ok": true}))
        }
        PROFILE_PREFERENCES_PUT => {
            let prefs: UserPreferences = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("prefs parse: {e}")))?;
            svc.put_preferences(owner, prefs).await?;
            Ok(serde_json::json!({"ok": true}))
        }
        PROFILE_PUBLIC_KEY_ADD => {
            let args: PublicKeyAddArgs = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("public_key_add parse: {e}")))?;
            let key_id = svc.add_public_key(
                owner,
                args.algorithm,
                args.key_material,
                args.label,
            ).await?;
            Ok(serde_json::to_value(key_id).unwrap())
        }
        PROFILE_PUBLIC_KEY_LIST => {
            let keys = svc.list_public_keys(owner).await?;
            Ok(serde_json::to_value(keys).unwrap())
        }
        PROFILE_PUBLIC_KEY_REVOKE => {
            let key_id: String = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("public_key_revoke parse: {e}")))?;
            svc.revoke_public_key(&key_id).await?;
            Ok(serde_json::json!({"ok": true}))
        }
        PROFILE_DEVICE_REGISTER => {
            let args: DeviceRegisterArgs = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("device_register parse: {e}")))?;
            let device_id = svc.register_device(
                owner,
                args.device_class,
                args.label,
                args.node_id,
                args.pairing_id,
            ).await?;
            Ok(serde_json::to_value(device_id).unwrap())
        }
        PROFILE_DEVICE_LIST => {
            let devices = svc.list_devices(owner).await?;
            Ok(serde_json::to_value(devices).unwrap())
        }
        PROFILE_DIGIT_GET => {
            let digit = svc.digit_for(owner).await?;
            Ok(serde_json::to_value(digit).unwrap())
        }
        PROFILE_AVATAR_SET => {
            let blob: AvatarBlob = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("avatar_set parse: {e}")))?;
            let mut profile = svc.get_profile(owner).await?
                .ok_or_else(|| AppError::Domain("profile not found".into()))?;
            profile.avatar = Some(blob);
            profile.updated_at = chrono::Utc::now().timestamp() as u64;
            svc.upsert_profile(profile).await?;
            Ok(serde_json::json!({"ok": true}))
        }
        PROFILE_AVATAR_UPLOAD => {
            let args: AvatarUploadArgs = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("avatar_upload parse: {e}")))?;
            let blob = svc
                .upload_avatar(owner, args.mime_type, args.bytes_b64)
                .await?;
            Ok(serde_json::to_value(blob).unwrap())
        }
        PROFILE_AVATAR_GET => {
            let got = svc.get_avatar(owner).await?;
            Ok(got
                .map(|b| serde_json::to_value(b).unwrap_or(serde_json::Value::Null))
                .unwrap_or(serde_json::Value::Null))
        }
        PROFILE_AVATAR_REMOVE => {
            svc.remove_avatar(owner).await?;
            Ok(serde_json::json!({"ok": true}))
        }
        PROFILE_KIND_GET => {
            let k = svc.get_kind(owner).await?;
            Ok(serde_json::to_value(k).unwrap())
        }
        PROFILE_KIND_SET => {
            let args: KindSetArgs = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("kind_set parse: {e}")))?;
            svc.set_kind(owner, args.kind).await?;
            Ok(serde_json::json!({"ok": true}))
        }
        PROFILE_PUBLIC_KEY_LABEL => {
            let args: PublicKeyLabelArgs = serde_json::from_value(params)
                .map_err(|e| AppError::Internal(format!("public_key_label parse: {e}")))?;
            svc.label_public_key(&args.key_id, &args.label).await?;
            Ok(serde_json::json!({"ok": true}))
        }
        _ => Err(AppError::Internal(format!("unknown profile method {method}"))),
    };
    r.map_err(crate::error::app_to_domain)
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct PublicKeyAddArgs {
    pub algorithm: PublicKeyAlgorithm,
    pub key_material: String,
    pub label: Option<String>,
}

/// Wire-shape for `a3chat.profile.public_key.label`.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct PublicKeyLabelArgs {
    pub key_id: String,
    pub label: String,
}

/// Wire-shape for `a3chat.profile.kind.set`.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct KindSetArgs {
    pub kind: UserKind,
}

/// Wire-shape for `a3chat.profile.avatar.upload`. The actual
/// bytes are base64-encoded over the wire — DO-178C §6.3 *no
/// raw byte streams over JSON-RPC*.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct AvatarUploadArgs {
    pub mime_type: String,
    pub bytes_b64: String,
}

/// Result of `a3chat.profile.avatar.get`. The bytes are returned
/// as base64 (same convention as the upload side) plus the
/// content-address reference that was stored on the profile row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AvatarBytes {
    pub blob: AvatarBlob,
    pub bytes_b64: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct DeviceRegisterArgs {
    pub device_class: DeviceClass,
    pub label: String,
    pub node_id: String,
    pub pairing_id: Option<String>,
}
/// Stable storage config — pinned paths inside the a3chat
/// data directory so a3chat-app and `a3net-userstore` can
/// share a single SQLite file (or live side-by-side, depending
/// on operator preference).
#[derive(Debug, Clone)]
pub struct ProfileConfig {
    /// Path to the userstore SQLite file.
    pub sqlite_path: std::path::PathBuf,
    /// Path to the avatar blobstore directory.
    pub blobstore_path: std::path::PathBuf,
}

impl ProfileConfig {
    pub fn new(
        sqlite_path: impl Into<std::path::PathBuf>,
        blobstore_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            sqlite_path: sqlite_path.into(),
            blobstore_path: blobstore_path.into(),
        }
    }

    /// Place the profile store alongside the a3chat storage:
    /// `<base>/profiles.sqlite` + `<base>/avatar_blobs`.
    pub fn under_base(base: &std::path::Path) -> Self {
        Self::new(
            base.join("profiles.sqlite"),
            base.join("avatar_blobs"),
        )
    }
}

/// Thin wrapper around [`SqliteUserStore`] + an avatar
/// [`BlobStore`]. The underlying trait method calls are async on
/// the trait but the SQLite / blob-store implementations are
/// synchronous — the wrapper runs them on
/// `tokio::task::spawn_blocking` so the RPC handlers stay
/// non-blocking.
#[derive(Clone)]
pub struct ProfileService {
    store: Arc<dyn UserStore>,
    blobs: Arc<BlobStore>,
}

impl std::fmt::Debug for ProfileService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileService").finish()
    }
}

impl ProfileService {
    pub fn open(config: &ProfileConfig) -> AppResult<Self> {
        if let Some(parent) = config.sqlite_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Storage(format!("profile dir create: {e}")))?;
        }
        std::fs::create_dir_all(&config.blobstore_path)
            .map_err(|e| AppError::Storage(format!("blobstore dir create: {e}")))?;
        let store = SqliteUserStore::open(SqliteUserStoreConfig::new(
            config.sqlite_path.clone(),
        ))
        .map_err(|e| AppError::Storage(format!("profile store open: {e}")))?;
        let blobs = BlobStore::new(&config.blobstore_path)
            .map_err(|e| AppError::Storage(format!("avatar blobstore open: {e}")))?;
        Ok(Self {
            store: Arc::new(store),
            blobs: Arc::new(blobs),
        })
    }

    /// Construct from an already-opened store (test injection).
    pub fn from_store(store: Arc<dyn UserStore>) -> Self {
        // Tests that don't care about avatars get a dummy in-memory
        // blobstore. The e2e tests use [`ProfileService::open`]
        // directly.
        let tmp = tempfile::TempDir::new().expect("tempdir for test blobstore");
        let blobs = BlobStore::new(tmp.path()).expect("test blobstore opens");
        // Intentionally leak the tempdir — the blobs are inside it,
        // and we don't want the directory to be removed before the
        // store is dropped. This is fine for short-lived tests.
        std::mem::forget(tmp);
        Self {
            store,
            blobs: Arc::new(blobs),
        }
    }

    fn store(&self) -> Arc<dyn UserStore> {
        Arc::clone(&self.store)
    }

    fn blobs(&self) -> Arc<BlobStore> {
        Arc::clone(&self.blobs)
    }

    /// Run a sync closure that returns `UserStoreResult<T>` on the
    /// blocking pool, then map its error to `AppError`.
    async fn run_blocking<F, T>(&self, label: &'static str, f: F) -> AppResult<T>
    where
        F: FnOnce() -> a3net_userstore::error::UserStoreResult<T> + Send + 'static,
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| AppError::Internal(format!("{label} blocking join: {e}")))?
            .map_err(|e| AppError::Storage(format!("{label}: {e}")))
    }

    // ── Profile CRUD ──────────────────────────────────────────────────

    pub async fn upsert_profile(&self, profile: UserProfile) -> AppResult<()> {
        let s = self.store();
        self.run_blocking("profile put", move || s.put_profile(profile))
            .await
    }

    pub async fn get_profile(&self, user: &UserId) -> AppResult<Option<UserProfile>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("profile get", move || s.get_profile(&uid))
            .await
    }

    pub async fn put_preferences(
        &self,
        user: &UserId,
        prefs: UserPreferences,
    ) -> AppResult<()> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("prefs put", move || s.put_preferences(&uid, prefs))
            .await
    }

    // ── Public keys ───────────────────────────────────────────────────

    pub async fn add_public_key(
        &self,
        user: &UserId,
        algorithm: PublicKeyAlgorithm,
        key_material: String,
        label: Option<String>,
    ) -> AppResult<String> {
        // Generate the key id deterministically: blake3 hash of
        // (user_id, algorithm, key_material).
        let mut hasher = blake3::Hasher::new();
        hasher.update(user.as_str().as_bytes());
        hasher.update(algorithm.as_str().as_bytes());
        hasher.update(key_material.as_bytes());
        let key_id = hasher.finalize().to_hex().to_string();
        let key = UserPublicKey {
            key_id: key_id.clone(),
            user_id: user.as_str().to_string(),
            algorithm: algorithm.as_str().to_string(),
            key_material,
            label: label.unwrap_or_default(),
            created_at: chrono::Utc::now().timestamp() as u64,
            revoked_at: None,
        };
        let s = self.store();
        self.run_blocking("public_key put", move || {
            s.put_public_key(key)
        })
        .await?;
        Ok(key_id)
    }

    pub async fn list_public_keys(&self, user: &UserId) -> AppResult<Vec<UserPublicKey>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("public_key list", move || {
            s.list_public_keys(&uid)
        })
        .await
    }

    pub async fn revoke_public_key(&self, key_id: &str) -> AppResult<()> {
        let s = self.store();
        let kid = key_id.to_string();
        self.run_blocking("public_key revoke", move || {
            s.revoke_public_key(&kid)
        })
        .await
    }

    /// Patch the human-readable `label` on an existing public key.
    /// Best-effort: a non-existent `key_id` is a no-op (the FK is
    /// enforced upstream when the key is *created*).
    pub async fn label_public_key(&self, key_id: &str, label: &str) -> AppResult<()> {
        let s = self.store();
        let kid = key_id.to_string();
        let l = label.to_string();
        self.run_blocking("public_key label", move || {
            s.set_public_key_label(&kid, &l)
        })
        .await
    }

    // ── Account kind (v2) ────────────────────────────────────────────

    /// Persist the account kind for `user`. Auto-creates a minimal
    /// profile row if none exists yet so subsequent calls (e.g.
    /// `add_public_key`) can satisfy their FK constraints without
    /// the caller having to seed the profile first.
    pub async fn set_kind(&self, user: &UserId, kind: UserKind) -> AppResult<()> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("kind set", move || s.set_kind(&uid, kind))
            .await
    }

    /// Read the account kind. Unknown users default to
    /// [`UserKind::Human`] — DO-178C §6.1.
    pub async fn get_kind(&self, user: &UserId) -> AppResult<UserKind> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("kind get", move || s.get_kind(&uid))
            .await
    }

    // ── Devices ───────────────────────────────────────────────────────

    pub async fn register_device(
        &self,
        user: &UserId,
        device_class: DeviceClass,
        label: String,
        node_id: String,
        pairing_id: Option<String>,
    ) -> AppResult<String> {
        let device_id = uuid::Uuid::new_v4().to_string();
        let device = UserDevice {
            device_id: device_id.clone(),
            user_id: user.as_str().to_string(),
            node_id,
            pairing_id,
            device_class: device_class.as_str().to_string(),
            label,
            paired_at: chrono::Utc::now().timestamp() as u64,
            revoked_at: None,
        };
        let s = self.store();
        self.run_blocking("device put", move || {
            s.put_device(device)
        })
        .await?;
        Ok(device_id)
    }

    pub async fn list_devices(&self, user: &UserId) -> AppResult<Vec<UserDevice>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("device list", move || {
            s.list_devices(&uid)
        })
        .await
    }

    // ── 12-digit ID ───────────────────────────────────────────────────

    /// Returns the canonical 12-digit ID for a user. If the
    /// store doesn't have one yet, it's computed and persisted.
    pub async fn digit_for(&self, user: &UserId) -> AppResult<String> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("digit compute", move || {
            s.ensure_user_digit(&uid)
        })
        .await
    }

    /// Lookup-only path — does NOT compute.
    pub async fn digit_lookup(&self, user: &UserId) -> AppResult<Option<String>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("digit lookup", move || {
            s.resolve_user_digit(&uid)
        })
        .await
    }

    // ── Avatar (BLAKE3-content-addressed blobstore) ──────────────────

    /// Decode a base64 payload, write it to the avatar blobstore,
    /// and patch the `user_profile.avatar` reference. Returns the
    /// [`AvatarBlob`] descriptor so callers can verify the hash.
    pub async fn upload_avatar(
        &self,
        user: &UserId,
        mime_type: String,
        bytes_b64: String,
    ) -> AppResult<AvatarBlob> {
        // 1. Validate MIME allow-list (boundary check — DO-178C §6.3).
        if !ALLOWED_AVATAR_MIME_TYPES.contains(&mime_type.as_str()) {
            return Err(AppError::Domain(format!(
                "avatar mime_type {mime_type} not in allow-list"
            )));
        }
        // 2. Decode base64 → raw bytes.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(bytes_b64.as_bytes())
            .map_err(|e| AppError::Domain(format!("avatar base64 decode: {e}")))?;
        // 3. Enforce size cap at the boundary.
        if bytes.len() > MAX_AVATAR_BYTES {
            return Err(AppError::Domain(format!(
                "avatar size {} exceeds cap {}",
                bytes.len(),
                MAX_AVATAR_BYTES
            )));
        }
        // 4. Compute the BLAKE3 hash that becomes the content address.
        let hash_hex = blake3::hash(&bytes).to_hex().to_string();
        // 5. Write chunks to the blobstore (sync, off the runtime).
        let blobs = self.blobs();
        let bytes_for_blocking = bytes.clone();
        let (hash_check, size) = tokio::task::spawn_blocking(move || {
            blobs.put_bytes_sync(&bytes_for_blocking)
        })
        .await
        .map_err(|e| AppError::Internal(format!("avatar blobstore join: {e}")))?
        .map_err(|e| AppError::Storage(format!("avatar blobstore put: {e}")))?;
        // 6. Sanity: the hash the blobstore derived must equal ours.
        let stored_hex = hash_check.as_hex().to_string();
        if stored_hex != hash_hex {
            return Err(AppError::Internal(format!(
                "blobstore hash mismatch: expected {hash_hex}, got {stored_hex}"
            )));
        }
        let blob = AvatarBlob::new(hash_hex.clone(), mime_type.clone(), size);
        // 7. Patch the profile row, auto-creating it if necessary.
        let s = self.store();
        let uid = user.as_str().to_string();
        let blob_for_db = blob.clone();
        self.run_blocking("avatar upsert", move || {
            let mut profile = s.get_profile(&uid)?
                .unwrap_or_else(|| {
                    // Re-use the user-id as a placeholder username
                    // — DO-178C §6.1 *fail-safe*: callers can fill
                    // in a real username later via `put_profile`.
                    UserProfile::new(uid.clone(), uid.clone())
                });
            profile.avatar = Some(blob_for_db);
            profile.updated_at = chrono::Utc::now().timestamp() as u64;
            s.put_profile(profile)
        })
        .await?;
        Ok(blob)
    }

    /// Read an avatar back as [`AvatarBytes`]. Returns `None` if no
    /// avatar is currently bound (either the row is missing or the
    /// blob is gone).
    pub async fn get_avatar(&self, user: &UserId) -> AppResult<Option<AvatarBytes>> {
        // 1. Pull the profile row to learn the avatar hash + MIME.
        let s = self.store();
        let uid = user.as_str().to_string();
        let profile = self
            .run_blocking("avatar get profile", move || s.get_profile(&uid))
            .await?;
        let Some(profile) = profile else {
            return Ok(None);
        };
        let Some(blob) = profile.avatar else {
            return Ok(None);
        };
        // 2. Reconstruct the ContentHash from the hex string and
        //    fetch the bytes from the disk blobstore.
        let blobs = self.blobs();
        let hex = blob.blob_hash.clone();
        let bytes = tokio::task::spawn_blocking(move || {
            use a3net_types::ContentHash;
            let h = ContentHash::from_hex(&hex)
                .map_err(|e| format!("avatar hash hex: {e}"))?;
            blobs.get_sync(&h).ok_or_else(|| "blob missing".to_string())
        })
        .await
        .map_err(|e| AppError::Internal(format!("avatar blobstore join: {e}")))?
        .map_err(|e| AppError::Domain(format!("avatar blob: {e}")))?;
        // 3. Re-encode as base64 for the wire shape.
        let bytes_b64 =
            base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(Some(AvatarBytes { blob, bytes_b64 }))
    }

    /// Drop the avatar blob and clear the profile reference. The
    /// blob is best-effort removed (already-missing is fine); the
    /// row patch is the authoritative part.
    pub async fn remove_avatar(&self, user: &UserId) -> AppResult<()> {
        // 1. Pull the profile so we know which blob to delete.
        let s = self.store();
        let uid = user.as_str().to_string();
        let profile = self
            .run_blocking("avatar get for remove", move || s.get_profile(&uid))
            .await?;
        let Some(profile) = profile else {
            return Ok(());
        };
        if let Some(blob) = profile.avatar.as_ref() {
            let blobs = self.blobs();
            let hex = blob.blob_hash.clone();
            let _ = tokio::task::spawn_blocking(move || {
                use a3net_types::ContentHash;
                if let Ok(h) = ContentHash::from_hex(&hex) {
                    let _ = blobs.remove(&h);
                }
            })
            .await
            .map_err(|e| AppError::Internal(format!("avatar blobstore join: {e}")))?;
        }
        // 2. Clear the row reference (auto-create if needed).
        let mut next = profile;
        next.avatar = None;
        next.updated_at = chrono::Utc::now().timestamp() as u64;
        let s2 = self.store();
        self.run_blocking("avatar remove patch", move || s2.put_profile(next))
        .await?;
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────
//
// These tests use an in-memory mock `UserStore` so they don't
// depend on SQLite being initialised. The full SQLite round-trip
// is exercised in `tests/profile_service_e2e.rs` (see below).

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_userstore::model::{AvatarBlob, UserPreferences, UserProfile};
    use a3net_userstore::store::UserStoreInfo;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory mock for the `UserStore` trait. Sufficient for
    /// exercising the service's wrapper logic (spawn_blocking,
    /// error mapping) without touching SQLite.
    #[derive(Default)]
    struct MockUserStore {
        profiles: Mutex<HashMap<String, UserProfile>>,
        keys: Mutex<HashMap<String, UserPublicKey>>,
        devices: Mutex<HashMap<String, UserDevice>>,
    }

    impl MockUserStore {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
    }

    #[async_trait::async_trait]
    impl UserStore for MockUserStore {
        fn put_profile(&self, p: UserProfile) -> a3net_userstore::error::UserStoreResult<()> {
            self.profiles.lock().unwrap().insert(p.user_id.clone(), p);
            Ok(())
        }
        fn get_profile(
            &self,
            uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<Option<UserProfile>> {
            Ok(self.profiles.lock().unwrap().get(uid).cloned())
        }
        fn put_preferences(
            &self,
            uid: &str,
            prefs: UserPreferences,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            let mut g = self.profiles.lock().unwrap();
            if let Some(p) = g.get_mut(uid) {
                p.preferences = prefs;
                Ok(())
            } else {
                Err(a3net_userstore::error::UserStoreError::NotFound {
                    kind: "profile",
                    id: uid.into(),
                })
            }
        }
        fn list_profiles(
            &self,
        ) -> a3net_userstore::error::UserStoreResult<Vec<UserProfile>> {
            Ok(self.profiles.lock().unwrap().values().cloned().collect())
        }
        fn delete_profile(&self, uid: &str) -> a3net_userstore::error::UserStoreResult<usize> {
            Ok(if self.profiles.lock().unwrap().remove(uid).is_some() { 1 } else { 0 })
        }
        fn put_public_key(
            &self,
            k: UserPublicKey,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            self.keys.lock().unwrap().insert(k.key_id.clone(), k);
            Ok(())
        }
        fn revoke_public_key(
            &self,
            kid: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            let mut g = self.keys.lock().unwrap();
            if let Some(k) = g.get_mut(kid) {
                k.revoked_at = Some(1);
                Ok(())
            } else {
                Err(a3net_userstore::error::UserStoreError::NotFound {
                    kind: "public_key",
                    id: kid.into(),
                })
            }
        }
        fn list_public_keys(
            &self,
            uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<Vec<UserPublicKey>> {
            Ok(self
                .keys
                .lock()
                .unwrap()
                .values()
                .filter(|k| k.user_id == uid)
                .cloned()
                .collect())
        }
        fn put_device(
            &self,
            d: UserDevice,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            self.devices.lock().unwrap().insert(d.device_id.clone(), d);
            Ok(())
        }
        fn revoke_device(
            &self,
            did: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            let mut g = self.devices.lock().unwrap();
            if let Some(d) = g.get_mut(did) {
                d.revoked_at = Some(1);
                Ok(())
            } else {
                Err(a3net_userstore::error::UserStoreError::NotFound {
                    kind: "device",
                    id: did.into(),
                })
            }
        }
        fn list_devices(
            &self,
            uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<Vec<UserDevice>> {
            Ok(self
                .devices
                .lock()
                .unwrap()
                .values()
                .filter(|d| d.user_id == uid)
                .cloned()
                .collect())
        }
        fn ensure_user_digit(
            &self,
            uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<String> {
            Ok(format!("{:012}", uid.len() % 1_000_000_000_000))
        }
        fn resolve_user_digit(
            &self,
            _uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<Option<String>> {
            Ok(Some("000000000000".into()))
        }
        fn set_public_key_label(
            &self,
            key_id: &str,
            label: &str,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            let mut g = self.keys.lock().unwrap();
            if let Some(k) = g.get_mut(key_id) {
                k.label = label.to_string();
            }
            Ok(())
        }
        fn set_kind(
            &self,
            uid: &str,
            kind: UserKind,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            let mut g = self.profiles.lock().unwrap();
            g.entry(uid.to_string())
                .or_insert_with(|| UserProfile::new(uid, uid))
                .kind = kind;
            Ok(())
        }
        fn get_kind(
            &self,
            uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<UserKind> {
            Ok(self
                .profiles
                .lock()
                .unwrap()
                .get(uid)
                .map(|p| p.kind)
                .unwrap_or_default())
        }
    }

    fn alice() -> UserId {
        UserId::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    }

    #[tokio::test]
    async fn upsert_then_get_roundtrips_profile() {
        let svc = ProfileService::from_store(MockUserStore::new() as Arc<dyn UserStore>);
        let profile = UserProfile::new(alice().as_str(), "alice");
        svc.upsert_profile(profile).await.unwrap();
        let got = svc.get_profile(&alice()).await.unwrap();
        assert!(got.is_some(), "profile should round-trip");
        assert_eq!(got.unwrap().username, "alice");
    }

    #[tokio::test]
    async fn add_public_key_returns_deterministic_id() {
        let svc = ProfileService::from_store(MockUserStore::new() as Arc<dyn UserStore>);
        let id1 = svc
            .add_public_key(
                &alice(),
                PublicKeyAlgorithm::Ed25519,
                "deadbeef".into(),
                None,
            )
            .await
            .unwrap();
        let id2 = svc
            .add_public_key(
                &alice(),
                PublicKeyAlgorithm::Ed25519,
                "deadbeef".into(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(id1, id2, "key id must be deterministic for same input");
    }

    #[tokio::test]
    async fn register_device_returns_uuid() {
        let svc = ProfileService::from_store(MockUserStore::new() as Arc<dyn UserStore>);
        let id = svc
            .register_device(
                &alice(),
                DeviceClass::Mobile,
                "alice-iphone".into(),
                "node-id-1".into(),
                None,
            )
            .await
            .unwrap();
        // UUID v4 format: 36 chars, 4 dashes.
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
    }

    #[tokio::test]
    async fn digit_for_returns_twelve_chars() {
        let svc = ProfileService::from_store(MockUserStore::new() as Arc<dyn UserStore>);
        let d = svc.digit_for(&alice()).await.unwrap();
        assert_eq!(d.len(), 12, "12-digit id: {d}");
    }

    #[tokio::test]
    async fn profile_config_under_base_uses_profiles_sqlite() {
        let cfg = ProfileConfig::under_base(std::path::Path::new("/var/data/a3chat"));
        assert_eq!(
            cfg.sqlite_path,
            std::path::PathBuf::from("/var/data/a3chat/profiles.sqlite")
        );
    }

    #[tokio::test]
    async fn avatar_blob_serialization_roundtrips() {
        // The `AvatarBlob` shape is part of the public contract —
        // a3chat CLI consumers must be able to read it back.
        let blob = AvatarBlob::new(
            "abcdef1234567890",
            "image/png",
            4096,
        );
        let s = serde_json::to_string(&blob).unwrap();
        let back: AvatarBlob = serde_json::from_str(&s).unwrap();
        assert_eq!(back.blob_hash, blob.blob_hash);
    }

    #[test]
    fn user_store_info_carries_backend_label() {
        let mut info = UserStoreInfo::new("sqlite-mock");
        info.profile_count = 3;
        assert_eq!(info.backend, "sqlite-mock");
        assert_eq!(info.profile_count, 3);
    }
}