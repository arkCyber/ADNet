//! Data model for `a3net-userstore`.

use serde::{Deserialize, Serialize};

/// Max bytes for a hex (BLAKE3) blob hash stored in [`AvatarBlob`].
pub const MAX_AVATAR_BLOB_HASH_LEN: usize = 128;
/// Max bytes for a username string.
pub const MAX_USERNAME_LEN: usize = 64;
/// Max bytes for a display name string.
pub const MAX_DISPLAY_NAME_LEN: usize = 256;

/// Reference to a user avatar. The actual bytes live in
/// `a3net-blobstore`; this record just carries the content hash + MIME.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarBlob {
    /// Hex (lowercase) BLAKE3 hash of the avatar blob.
    pub blob_hash: String,
    /// MIME type, e.g. `image/png` or `image/webp`.
    pub mime_type: String,
    /// Original size in bytes.
    pub size_bytes: u64,
}

impl AvatarBlob {
    pub fn new(blob_hash: impl Into<String>, mime_type: impl Into<String>, size_bytes: u64) -> Self {
        Self {
            blob_hash: blob_hash.into(),
            mime_type: mime_type.into(),
            size_bytes,
        }
    }
}

/// Public-key algorithm tag for [`UserPublicKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicKeyAlgorithm {
    Ed25519,
    X25519,
    Rsa,
    EcdsaP256,
    Other,
}

/// Account kind — distinguishes human operators from
/// machine-controlled identities (agents, services, system
/// publishers). Persisted in the v2 `kind` column on
/// `user_profile` and exposed via
/// [`crate::store::UserStore::get_kind`] / `set_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    #[default]
    Human,
    Agent,
    System,
    Unknown,
}

impl UserKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UserKind::Human => "human",
            UserKind::Agent => "agent",
            UserKind::System => "system",
            UserKind::Unknown => "unknown",
        }
    }

    /// Lenient parser used by [`UserKind`] reads from older rows
    /// that pre-date the column. Unknown strings default to
    /// [`UserKind::Human`] (DO-178C §6.1 — *no panics on missing
    /// data*).
    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "agent" => UserKind::Agent,
            "system" => UserKind::System,
            "unknown" => UserKind::Unknown,
            _ => UserKind::Human,
        }
    }
}

impl PublicKeyAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            PublicKeyAlgorithm::Ed25519 => "ed25519",
            PublicKeyAlgorithm::X25519 => "x25519",
            PublicKeyAlgorithm::Rsa => "rsa",
            PublicKeyAlgorithm::EcdsaP256 => "ecdsa_p256",
            PublicKeyAlgorithm::Other => "other",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "ed25519" => PublicKeyAlgorithm::Ed25519,
            "x25519" => PublicKeyAlgorithm::X25519,
            "rsa" => PublicKeyAlgorithm::Rsa,
            "ecdsa_p256" => PublicKeyAlgorithm::EcdsaP256,
            _ => PublicKeyAlgorithm::Other,
        }
    }
}

/// A public key bound to a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPublicKey {
    pub key_id: String,
    pub user_id: String,
    pub algorithm: String, // see [`PublicKeyAlgorithm::as_str`]
    /// Raw key material (base64-encoded).
    pub key_material: String,
    /// Free-form UI label (e.g. "primary", "rotated 2026-Q3").
    /// Stored in the v2 `label` column on `user_public_keys`.
    #[serde(default)]
    pub label: String,
    /// Unix seconds.
    pub created_at: u64,
    /// Unix seconds; `None` means not revoked.
    pub revoked_at: Option<u64>,
}

impl UserPublicKey {
    pub fn parsed_algorithm(&self) -> PublicKeyAlgorithm {
        PublicKeyAlgorithm::from_str(&self.algorithm)
    }
}

/// Device class for [`UserDevice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    Desktop,
    Mobile,
    Tablet,
    Embedded,
    Headless,
}

impl DeviceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceClass::Desktop => "desktop",
            DeviceClass::Mobile => "mobile",
            DeviceClass::Tablet => "tablet",
            DeviceClass::Embedded => "embedded",
            DeviceClass::Headless => "headless",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "desktop" => DeviceClass::Desktop,
            "mobile" => DeviceClass::Mobile,
            "tablet" => DeviceClass::Tablet,
            "embedded" => DeviceClass::Embedded,
            _ => DeviceClass::Headless,
        }
    }
}

/// A known paired device for a user. `node_id` is the mesh / p2p node id
/// that this device exposes; `pairing_id` references an entry in
/// `a3net-pairing`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDevice {
    pub device_id: String,
    pub user_id: String,
    pub node_id: String,
    pub pairing_id: Option<String>,
    pub device_class: String, // see [`DeviceClass::as_str`]
    /// Free-form label shown in UI ("Arksong's MacBook").
    pub label: String,
    /// Unix seconds.
    pub paired_at: u64,
    /// Unix seconds; `None` means still trusted.
    pub revoked_at: Option<u64>,
}

impl UserDevice {
    pub fn parsed_class(&self) -> DeviceClass {
        DeviceClass::from_str(&self.device_class)
    }
}

/// User-specific preferences. These are intentionally *user-only* —
/// anything that should sync to peers lives in a different crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPreferences {
    /// Theme preference.
    pub theme: String, // "light" | "dark" | "auto"
    /// Locale tag, e.g. `"en-US"` or `"zh-CN"`.
    pub locale: String,
    /// Whether notifications should pop up.
    pub notifications_enabled: bool,
    /// Whether read-receipts are sent to peers.
    pub read_receipts_enabled: bool,
    /// Whether typing indicators are sent to peers.
    pub typing_indicators_enabled: bool,
    /// Free-form JSON for experimental flags. Stored as text.
    pub experimental_json: String,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            theme: "auto".to_string(),
            locale: "en-US".to_string(),
            notifications_enabled: true,
            read_receipts_enabled: true,
            typing_indicators_enabled: true,
            experimental_json: "{}".to_string(),
        }
    }
}

/// User profile record — one per `user_id`. This is the **profile-level**
/// row; the chatstore's `users` table covers identity + last_seen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    /// Optional avatar reference. None means "no avatar".
    pub avatar: Option<AvatarBlob>,
    /// Bio / status line.
    pub bio: String,
    /// Account kind — defaults to [`UserKind::Human`] when read
    /// from older rows that pre-date the column.
    #[serde(default)]
    pub kind: UserKind,
    pub preferences: UserPreferences,
    /// Unix seconds.
    pub created_at: u64,
    /// Unix seconds.
    pub updated_at: u64,
}

impl UserProfile {
    pub fn new(user_id: impl Into<String>, username: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            username: username.into(),
            display_name: String::new(),
            avatar: None,
            bio: String::new(),
            kind: UserKind::default(),
            preferences: UserPreferences::default(),
            created_at: 0,
            updated_at: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prefs_are_sane() {
        let p = UserPreferences::default();
        assert_eq!(p.theme, "auto");
        assert!(p.notifications_enabled);
        assert!(p.read_receipts_enabled);
    }

    #[test]
    fn avatar_blob_size_must_be_u64() {
        let a = AvatarBlob::new("deadbeef", "image/png", 1024);
        assert_eq!(a.size_bytes, 1024);
    }

    #[test]
    fn device_class_round_trip() {
        for c in [
            DeviceClass::Desktop,
            DeviceClass::Mobile,
            DeviceClass::Tablet,
            DeviceClass::Embedded,
            DeviceClass::Headless,
        ] {
            assert_eq!(DeviceClass::from_str(c.as_str()), c);
        }
        assert_eq!(DeviceClass::from_str("nope"), DeviceClass::Headless);
    }
}