//! Store trait + info record for the user-profile layer.

use crate::error::UserStoreResult;
use crate::model::{UserDevice, UserKind, UserPreferences, UserProfile, UserPublicKey};

/// Storage capabilities required by every user-profile backend.
/// All methods are synchronous — callers wrap the call in
/// `tokio::task::spawn_blocking` when they need async bridging.
pub trait UserStore: Send + Sync {
    // ------- Profile -------

    /// Insert or fully-replace the profile keyed by `user_id`.
    fn put_profile(&self, profile: UserProfile) -> UserStoreResult<()>;

    /// Fetch a profile by id.
    fn get_profile(&self, user_id: &str) -> UserStoreResult<Option<UserProfile>>;

    /// Patch only the preferences row.
    fn put_preferences(
        &self,
        user_id: &str,
        prefs: UserPreferences,
    ) -> UserStoreResult<()>;

    /// List every profile (no pagination).
    fn list_profiles(&self) -> UserStoreResult<Vec<UserProfile>>;

    /// Remove a profile and all child rows (keys, devices). Returns the
    /// number of profiles removed (0 or 1).
    fn delete_profile(&self, user_id: &str) -> UserStoreResult<usize>;

    // ------- Public keys -------

    fn put_public_key(&self, key: UserPublicKey) -> UserStoreResult<()>;
    fn revoke_public_key(&self, key_id: &str) -> UserStoreResult<()>;
    fn list_public_keys(&self, user_id: &str) -> UserStoreResult<Vec<UserPublicKey>>;

    /// Patch only the human-readable label on a public key.
    /// Backends that don't support labelling may no-op the call
    /// but must still return `Ok(())`.
    fn set_public_key_label(&self, key_id: &str, label: &str) -> UserStoreResult<()>;

    // ------- Account kind (v2) -------

    /// Persist the account [`UserKind`] for `user_id`. If the user
    /// does not exist yet, a minimal profile row is auto-created so
    /// downstream FK constraints are always satisfiable.
    fn set_kind(&self, user_id: &str, kind: UserKind) -> UserStoreResult<()>;

    /// Read the account kind. Defaults to [`UserKind::Human`] for
    /// unknown users (DO-178C §6.1 — *no panics on missing data*).
    fn get_kind(&self, user_id: &str) -> UserStoreResult<UserKind>;

    // ------- Devices -------

    fn put_device(&self, device: UserDevice) -> UserStoreResult<()>;
    fn revoke_device(&self, device_id: &str) -> UserStoreResult<()>;
    fn list_devices(&self, user_id: &str) -> UserStoreResult<Vec<UserDevice>>;

    // ------- 12-digit ID -------

    /// Compute and persist the canonical `user_id -> 12_digit_id` mapping
    /// using [`a3net_roster::stable_digit_from_node`]. Returns the digit.
    fn ensure_user_digit(&self, user_id: &str) -> UserStoreResult<String>;

    /// Look up the canonical digit for a user (if any).
    fn resolve_user_digit(&self, user_id: &str) -> UserStoreResult<Option<String>>;
}

/// Diagnostic info for a backend.
#[derive(Debug, Clone)]
pub struct UserStoreInfo {
    pub backend: &'static str,
    pub location: Option<String>,
    pub profile_count: usize,
    pub public_key_count: usize,
    pub device_count: usize,
}

impl UserStoreInfo {
    pub fn new(backend: &'static str) -> Self {
        Self {
            backend,
            location: None,
            profile_count: 0,
            public_key_count: 0,
            device_count: 0,
        }
    }
}