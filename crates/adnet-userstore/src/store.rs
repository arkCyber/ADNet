//! Store trait + info record for the user-profile layer.

use async_trait::async_trait;

use crate::error::UserStoreResult;
use crate::model::{UserDevice, UserPreferences, UserProfile, UserPublicKey};

/// Storage capabilities required by every user-profile backend.
#[async_trait]
pub trait UserStore: Send + Sync {
    // ------- Profile -------

    /// Insert or fully-replace the profile keyed by `user_id`.
    async fn put_profile(&self, profile: UserProfile) -> UserStoreResult<()>;

    /// Fetch a profile by id.
    async fn get_profile(&self, user_id: &str) -> UserStoreResult<Option<UserProfile>>;

    /// Patch only the preferences row.
    async fn put_preferences(
        &self,
        user_id: &str,
        prefs: UserPreferences,
    ) -> UserStoreResult<()>;

    /// List every profile (no pagination).
    async fn list_profiles(&self) -> UserStoreResult<Vec<UserProfile>>;

    /// Remove a profile and all child rows (keys, devices). Returns the
    /// number of profiles removed (0 or 1).
    async fn delete_profile(&self, user_id: &str) -> UserStoreResult<usize>;

    // ------- Public keys -------

    async fn put_public_key(&self, key: UserPublicKey) -> UserStoreResult<()>;
    async fn revoke_public_key(&self, key_id: &str) -> UserStoreResult<()>;
    async fn list_public_keys(&self, user_id: &str) -> UserStoreResult<Vec<UserPublicKey>>;

    // ------- Devices -------

    async fn put_device(&self, device: UserDevice) -> UserStoreResult<()>;
    async fn revoke_device(&self, device_id: &str) -> UserStoreResult<()>;
    async fn list_devices(&self, user_id: &str) -> UserStoreResult<Vec<UserDevice>>;

    // ------- 12-digit ID -------

    /// Compute and persist the canonical `user_id -> 12_digit_id` mapping
    /// using [`adnet_roster::stable_digit_from_node`]. Returns the digit.
    async fn ensure_user_digit(&self, user_id: &str) -> UserStoreResult<String>;

    /// Look up the canonical digit for a user (if any).
    async fn resolve_user_digit(&self, user_id: &str) -> UserStoreResult<Option<String>>;
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