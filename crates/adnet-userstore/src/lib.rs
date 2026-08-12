//! `adnet-userstore` — per-user profile persistence.
//!
//! ## Why this crate exists
//!
//! `adnet-chatstore` already owns the `users` row that came out of
//! `Exodus@src-backup/exodus-hub-server/src/manager.rs`
//! (`id / username / display_name / created_at / last_seen`). This crate
//! is **complementary**, not overlapping: it stores the user *profile*
//! bits that aren't conversation content — preferences, avatar blob hash,
//! public-key bindings for trust, device list, and the canonical
//! 12-digit Exodus ID for this user.
//!
//! ## Tables (schema v1)
//!
//! - `user_profile`        — one row per `user_id`, holds display prefs.
//! - `user_public_keys`    — many public-key bindings per `user_id`.
//! - `user_devices`        — known paired devices per `user_id`.
//! - `user_id_digit`       — canonical `user_id -> 12_digit_id` mapping.
//!
//! The 12-digit derivation is reused from
//! [`adnet_roster::stable_digit_from_node`] so the same algorithm produces
//! the same number on every device.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod error;
pub mod model;
pub mod sqlite;
pub mod store;

pub use error::{UserStoreError, UserStoreResult};
pub use model::{
    AvatarBlob, DeviceClass, PublicKeyAlgorithm, UserDevice, UserPreferences, UserProfile,
    UserPublicKey, MAX_AVATAR_BLOB_HASH_LEN, MAX_DISPLAY_NAME_LEN, MAX_USERNAME_LEN,
};
pub use sqlite::{SqliteUserStore, SqliteUserStoreConfig, USER_SCHEMA_VERSION};
pub use store::{UserStore, UserStoreInfo};