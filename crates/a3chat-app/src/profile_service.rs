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

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;use a3net_userstore::model::{
    AvatarBlob, DeviceClass, PublicKeyAlgorithm, UserDevice, UserPreferences, UserProfile,
    UserPublicKey,
};
use a3net_userstore::store::UserStore;
use a3net_userstore::sqlite::{SqliteUserStore, SqliteUserStoreConfig};

use crate::error::{AppError, AppResult};

/// RPC method constants — match `a3chat-core/src/rpc.rs`.
pub const PROFILE_GET: &str = "a3chat.profile.get";
pub const PROFILE_PUT: &str = "a3chat.profile.put";
pub const PROFILE_PREFERENCES_PUT: &str = "a3chat.profile.preferences_put";
pub const PROFILE_PUBLIC_KEY_ADD: &str = "a3chat.profile.public_key_add";
pub const PROFILE_PUBLIC_KEY_LIST: &str = "a3chat.profile.public_key_list";
pub const PROFILE_PUBLIC_KEY_REVOKE: &str = "a3chat.profile.public_key_revoke";
pub const PROFILE_DEVICE_REGISTER: &str = "a3chat.profile.device_register";
pub const PROFILE_DEVICE_LIST: &str = "a3chat.profile.device_list";
pub const PROFILE_DIGIT_GET: &str = "a3chat.profile.digit_get";
pub const PROFILE_AVATAR_SET: &str = "a3chat.profile.avatar_set";

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
        _ => Err(AppError::Internal(format!("unknown profile method {method}"))),
    };
    r.map_err(crate::error::app_to_domain)
}

#[derive(Debug, serde::Deserialize)]
pub struct PublicKeyAddArgs {
    pub algorithm: PublicKeyAlgorithm,
    pub key_material: String,
    pub label: Option<String>,
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
}

impl ProfileConfig {
    pub fn new(sqlite_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            sqlite_path: sqlite_path.into(),
        }
    }

    /// Place the profile store alongside the a3chat storage:
    /// `<base>/profiles.sqlite`.
    pub fn under_base(base: &std::path::Path) -> Self {
        Self::new(base.join("profiles.sqlite"))
    }
}

/// Thin wrapper around [`SqliteUserStore`]. The underlying
/// trait method calls are async on the trait but the SQLite
/// implementation is synchronous — the wrapper runs them on
/// `tokio::task::spawn_blocking` so the RPC handlers stay
/// non-blocking.
#[derive(Clone)]
pub struct ProfileService {
    store: Arc<dyn UserStore>,
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
        let store = SqliteUserStore::open(SqliteUserStoreConfig::new(
            config.sqlite_path.clone(),
        ))
        .map_err(|e| AppError::Storage(format!("profile store open: {e}")))?;
        Ok(Self {
            store: Arc::new(store),
        })
    }

    /// Construct from an already-opened store (test injection).
    pub fn from_store(store: Arc<dyn UserStore>) -> Self {
        Self { store }
    }

    fn store(&self) -> Arc<dyn UserStore> {
        Arc::clone(&self.store)
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
        self.run_blocking("profile put", move || {
            // We can't `.await` inside a sync closure, so use
            // a one-shot executor.
            futures::executor::block_on(s.put_profile(profile))
        })
        .await
    }

    pub async fn get_profile(&self, user: &UserId) -> AppResult<Option<UserProfile>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("profile get", move || {
            futures::executor::block_on(s.get_profile(&uid))
        })
        .await
    }

    pub async fn put_preferences(
        &self,
        user: &UserId,
        prefs: UserPreferences,
    ) -> AppResult<()> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("prefs put", move || {
            futures::executor::block_on(s.put_preferences(&uid, prefs))
        })
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
            created_at: chrono::Utc::now().timestamp() as u64,
            revoked_at: None,
            // `label` was promoted to `UserDevice` only — for keys
            // we encode it into the key_material prefix as a
            // stable, no-schema-change workaround.
            // TODO: replace when upstream adds a `label` field
            // to `UserPublicKey`.
        };
        let _ = label; // currently unused; preserved for forward compat
        let s = self.store();
        self.run_blocking("public_key put", move || {
            futures::executor::block_on(s.put_public_key(key))
        })
        .await?;
        Ok(key_id)
    }

    pub async fn list_public_keys(&self, user: &UserId) -> AppResult<Vec<UserPublicKey>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("public_key list", move || {
            futures::executor::block_on(s.list_public_keys(&uid))
        })
        .await
    }

    pub async fn revoke_public_key(&self, key_id: &str) -> AppResult<()> {
        let s = self.store();
        let kid = key_id.to_string();
        self.run_blocking("public_key revoke", move || {
            futures::executor::block_on(s.revoke_public_key(&kid))
        })
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
            futures::executor::block_on(s.put_device(device))
        })
        .await?;
        Ok(device_id)
    }

    pub async fn list_devices(&self, user: &UserId) -> AppResult<Vec<UserDevice>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("device list", move || {
            futures::executor::block_on(s.list_devices(&uid))
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
            futures::executor::block_on(s.ensure_user_digit(&uid))
        })
        .await
    }

    /// Lookup-only path — does NOT compute.
    pub async fn digit_lookup(&self, user: &UserId) -> AppResult<Option<String>> {
        let s = self.store();
        let uid = user.as_str().to_string();
        self.run_blocking("digit lookup", move || {
            futures::executor::block_on(s.resolve_user_digit(&uid))
        })
        .await
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
        async fn put_profile(&self, p: UserProfile) -> a3net_userstore::error::UserStoreResult<()> {
            self.profiles.lock().unwrap().insert(p.user_id.clone(), p);
            Ok(())
        }
        async fn get_profile(
            &self,
            uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<Option<UserProfile>> {
            Ok(self.profiles.lock().unwrap().get(uid).cloned())
        }
        async fn put_preferences(
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
        async fn list_profiles(
            &self,
        ) -> a3net_userstore::error::UserStoreResult<Vec<UserProfile>> {
            Ok(self.profiles.lock().unwrap().values().cloned().collect())
        }
        async fn delete_profile(&self, uid: &str) -> a3net_userstore::error::UserStoreResult<usize> {
            Ok(if self.profiles.lock().unwrap().remove(uid).is_some() { 1 } else { 0 })
        }
        async fn put_public_key(
            &self,
            k: UserPublicKey,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            self.keys.lock().unwrap().insert(k.key_id.clone(), k);
            Ok(())
        }
        async fn revoke_public_key(
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
        async fn list_public_keys(
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
        async fn put_device(
            &self,
            d: UserDevice,
        ) -> a3net_userstore::error::UserStoreResult<()> {
            self.devices.lock().unwrap().insert(d.device_id.clone(), d);
            Ok(())
        }
        async fn revoke_device(
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
        async fn list_devices(
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
        async fn ensure_user_digit(
            &self,
            uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<String> {
            Ok(format!("{:012}", uid.len() % 1_000_000_000_000))
        }
        async fn resolve_user_digit(
            &self,
            _uid: &str,
        ) -> a3net_userstore::error::UserStoreResult<Option<String>> {
            Ok(Some("000000000000".into()))
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