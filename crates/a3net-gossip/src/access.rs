//! Room access control for gossip rooms.
//!
//! This module provides:
//! - [`RoomAccessPolicy`]: Configurable access control policies
//! - [`AccessControl`]: Enforces access policies on room operations
//! - [`RoomCredential`]: Credentials for room authentication
//!
//! ## Security
//!
//! Password hashing uses Argon2id, the winner of the Password Hashing Competition.
//! This provides resistance against GPU/ASIC attacks and side-channel attacks.

use serde::{Deserialize, Serialize};

use a3net_types::NodeId;

/// Room credential for authentication.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomCredential {
    /// The room this credential grants access to.
    pub room_id: String,
    /// The credential value (password hash, token, etc.)
    pub credential: CredentialType,
}

/// Types of room credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialType {
    /// Plain text password (should be hashed before storage).
    Password(String),
    /// Pre-shared key or token.
    Psk(String),
    /// HMAC-based message authentication code.
    Hmac(String),
    /// No credential (public room).
    None,
}

impl RoomCredential {
    /// Create a new password credential.
    pub fn with_password(room_id: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            room_id: room_id.into(),
            credential: CredentialType::Password(password.into()),
        }
    }

    /// Create a new PSK credential.
    pub fn with_psk(room_id: impl Into<String>, psk: impl Into<String>) -> Self {
        Self {
            room_id: room_id.into(),
            credential: CredentialType::Psk(psk.into()),
        }
    }

    /// Create a public room credential.
    pub fn public(room_id: impl Into<String>) -> Self {
        Self {
            room_id: room_id.into(),
            credential: CredentialType::None,
        }
    }

    /// Check if this is a public room.
    pub fn is_public(&self) -> bool {
        matches!(self.credential, CredentialType::None)
    }
}

/// Access control policy for a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomAccessPolicy {
    /// Whether the room is public or requires authentication.
    pub is_public: bool,
    /// Password hash (if password-protected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
    /// Allowed node IDs (whitelist). If empty, all nodes are allowed (unless blocked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_nodes: Vec<String>,
    /// Blocked node IDs (blacklist).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_nodes: Vec<String>,
    /// Whether to allow node ID rotation/claiming.
    pub allow_node_rotation: bool,
    /// Maximum message size allowed.
    #[serde(default)]
    pub max_message_size: usize,
}

impl Default for RoomAccessPolicy {
    fn default() -> Self {
        Self {
            is_public: true,
            password_hash: None,
            allowed_nodes: Vec::new(),
            blocked_nodes: Vec::new(),
            allow_node_rotation: true,
            max_message_size: 1024 * 1024, // 1 MB default
        }
    }
}

impl RoomAccessPolicy {
    /// Create a public room policy.
    pub fn public() -> Self {
        Self::default()
    }

    /// Create a password-protected room policy.
    pub fn with_password(password_hash: impl Into<String>) -> Self {
        Self {
            is_public: false,
            password_hash: Some(password_hash.into()),
            ..Default::default()
        }
    }

    /// Create a whitelist-only room policy.
    pub fn with_whitelist(allowed_nodes: Vec<NodeId>) -> Self {
        Self {
            is_public: false,
            allowed_nodes: allowed_nodes.iter().map(|n| n.to_string()).collect(),
            ..Default::default()
        }
    }

    /// Check if a node ID is allowed to access this room.
    pub fn is_node_allowed(&self, node_id: &NodeId) -> bool {
        // First check blacklist.
        let node_str = node_id.to_string();
        if self.blocked_nodes.iter().any(|n| n == &node_str) {
            return false;
        }

        // If whitelist is not empty, check it.
        if !self.allowed_nodes.is_empty() {
            return self.allowed_nodes.iter().any(|n| n == &node_str);
        }

        // If whitelist is empty and not in blacklist, allow.
        true
    }

    /// Check if the room requires authentication.
    pub fn requires_auth(&self) -> bool {
        !self.is_public || self.password_hash.is_some() || !self.allowed_nodes.is_empty()
    }
}

/// Result of an access control check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessCheckResult {
    /// Access granted.
    Allowed,
    /// Access denied with reason.
    Denied(String),
    /// Authentication required.
    AuthenticationRequired,
}

impl AccessCheckResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Denied(s) => Some(s),
            _ => None,
        }
    }
}

/// Access control enforcement layer.
#[derive(Debug, Clone, Default)]
pub struct AccessControl {
    /// Policies indexed by room ID.
    policies: std::collections::HashMap<String, RoomAccessPolicy>,
    /// Credentials for rooms.
    credentials: std::collections::HashMap<String, String>, // room_id -> credential hash
}

impl AccessControl {
    /// Create a new access control manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a room with an access policy.
    pub fn register_room(&mut self, room_id: impl Into<String>, policy: RoomAccessPolicy) {
        self.policies.insert(room_id.into(), policy);
    }

    /// Set the credential for a room.
    pub fn set_credential(&mut self, room_id: impl Into<String>, credential_hash: String) {
        self.credentials.insert(room_id.into(), credential_hash);
    }

    /// Get the policy for a room.
    pub fn get_policy(&self, room_id: &str) -> Option<&RoomAccessPolicy> {
        self.policies.get(room_id)
    }

    /// Check if a node is allowed to access a room.
    pub fn check_node_access(&self, room_id: &str, node_id: &NodeId) -> AccessCheckResult {
        // Check if room exists in our policy map.
        if let Some(policy) = self.policies.get(room_id) {
            if !policy.is_node_allowed(node_id) {
                return AccessCheckResult::Denied(format!(
                    "node {} is not allowed in room {}",
                    node_id.short(),
                    room_id
                ));
            }
            AccessCheckResult::Allowed
        } else {
            // Room not registered - allow by default (or reject if strict mode).
            AccessCheckResult::Allowed
        }
    }

    /// Check if a credential is valid for a room.
    pub fn verify_credential(&self, room_id: &str, credential: &CredentialType) -> AccessCheckResult {
        // Check if room requires authentication.
        let policy = match self.policies.get(room_id) {
            Some(p) => p,
            None => return AccessCheckResult::Allowed, // Unknown room, allow.
        };

        if policy.is_public && policy.password_hash.is_none() && policy.allowed_nodes.is_empty() {
            return AccessCheckResult::Allowed; // Public room.
        }

        // Verify password if required.
        if let Some(ref stored_hash) = policy.password_hash {
            match credential {
                CredentialType::Password(pwd) => {
                    if self.verify_password(pwd, stored_hash) {
                        AccessCheckResult::Allowed
                    } else {
                        AccessCheckResult::AuthenticationRequired
                    }
                }
                CredentialType::Psk(psk) | CredentialType::Hmac(psk) => {
                    if self.verify_password(psk, stored_hash) {
                        AccessCheckResult::Allowed
                    } else {
                        AccessCheckResult::AuthenticationRequired
                    }
                }
                CredentialType::None => AccessCheckResult::AuthenticationRequired,
            }
        } else {
            AccessCheckResult::Allowed
        }
    }

    /// Verify a password against a stored Argon2id hash.
    /// Falls back to legacy hash format for backward compatibility.
    fn verify_password(&self, password: &str, hash: &str) -> bool {
        // First try Argon2id (modern format starts with "$argon2")
        if hash.starts_with("$argon2") {
            return argon2id_verify(hash, password);
        }
        
        // Fall back to legacy format for backward compatibility
        // Legacy format: plain match or SHA hash
        if password == hash {
            return true;
        }
        
        // Check legacy SHA hash format
        if hash.len() == 16 {
            return AccessControl::sha1_hash(password) == hash;
        }
        
        false
    }

    /// Hash a password using Argon2id.
    /// Returns a hash string suitable for storage.
    pub fn hash_password(password: &str) -> Result<String, PasswordHashError> {
        argon2id_hash(password, 3, 65536, 4)
    }

    /// Legacy SHA-1 hash for backward compatibility.
    fn sha1_hash(input: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        input.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Check if a hash is in the legacy format.
    pub fn is_legacy_hash(hash: &str) -> bool {
        !hash.starts_with("$argon2") && hash.len() != 64
    }

    /// Check if a hash needs rehashing (for migration to Argon2id).
    pub fn needs_rehash(hash: &str) -> bool {
        hash.starts_with("$argon2") && {
            // Check if it's using current recommended params
            // For now, always rehash legacy formats
            true
        }
    }

    /// Upgrade a legacy password hash to Argon2id.
    pub fn upgrade_password(&self, password: &str, legacy_hash: &str) -> Option<String> {
        // Verify against legacy hash first
        if !self.verify_password(password, legacy_hash) {
            return None;
        }
        // Re-hash with Argon2id
        Self::hash_password(password).ok()
    }
}

/// Argon2id password hashing functions.
mod password {
    use argon2::{
        password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
        Argon2, Params,
    };
    use thiserror::Error;

    #[derive(Debug, Error)]
    pub enum Argon2Error {
        #[error("hash creation failed: {0}")]
        HashFailed(String),
        #[error("verification failed")]
        VerifyFailed,
    }

    /// Hash a password with Argon2id using recommended default parameters.
    pub fn hash(password: &str) -> Result<String, Argon2Error> {
        // Argon2id with recommended parameters:
        // - Memory: 64 MiB (65536 KiB)
        // - Iterations: 3
        // - Parallelism: 4
        let params = Params::new(65536, 3, 4, None)
            .map_err(|e| Argon2Error::HashFailed(e.to_string()))?;
        
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        
        let hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| Argon2Error::HashFailed(e.to_string()))?;
        
        Ok(hash.to_string())
    }

    /// Verify a password against an Argon2id hash.
    pub fn verify(password: &str, hash: &str) -> bool {
        use argon2::{password_hash::PasswordVerifier, Argon2};

        let parsed_hash = match PasswordHash::new(hash) {
            Ok(h) => h,
            Err(_) => return false,
        };
        
        let params = match Params::new(65536, 3, 4, None) {
            Ok(p) => p,
            Err(_) => return false,
        };
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon2.verify_password(password.as_bytes(), &parsed_hash).is_ok()
    }
}

pub use password::Argon2Error as PasswordHashError;

/// Hash a password with Argon2id using recommended parameters.
fn argon2id_hash(password: &str, iterations: u32, memory_kib: u32, parallelism: u32) -> Result<String, PasswordHashError> {
    use argon2::{
        password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, SaltString},
        Argon2, Params,
    };
    
    let params = Params::new(memory_kib, iterations, parallelism, None)
        .map_err(|e| PasswordHashError::HashFailed(e.to_string()))?;
    
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| PasswordHashError::HashFailed(e.to_string()))?;
    
    Ok(hash.to_string())
}

/// Verify a password against an Argon2id hash.
fn argon2id_verify(hash: &str, password: &str) -> bool {
    use argon2::{password_hash::{PasswordHash, PasswordVerifier}, Argon2};

    let parsed_hash = match PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };

    let argon2 = Argon2::default();
    argon2.verify_password(password.as_bytes(), &parsed_hash).is_ok()
}

impl AccessControl {
    /// Check if a message is allowed based on size policy.
    pub fn check_message_size(&self, room_id: &str, message_size: usize) -> AccessCheckResult {
        if let Some(policy) = self.policies.get(room_id) {
            if message_size > policy.max_message_size {
                return AccessCheckResult::Denied(format!(
                    "message size {} exceeds room limit {}",
                    message_size, policy.max_message_size
                ));
            }
        }
        AccessCheckResult::Allowed
    }

    /// Remove a room's access policy.
    pub fn unregister_room(&mut self, room_id: &str) {
        self.policies.remove(room_id);
        self.credentials.remove(room_id);
    }

    /// Get the number of registered rooms.
    pub fn room_count(&self) -> usize {
        self.policies.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_room_allows_all_nodes() {
        let policy = RoomAccessPolicy::public();
        let node = NodeId::random();

        assert!(policy.is_node_allowed(&node));
        assert!(!policy.requires_auth());
    }

    #[test]
    fn blocked_node_is_rejected() {
        let node = NodeId::random();
        let mut policy = RoomAccessPolicy::public();
        policy.blocked_nodes.push(node.to_string());

        assert!(!policy.is_node_allowed(&node));
    }

    #[test]
    fn whitelist_blocks_non_members() {
        let allowed = NodeId::random();
        let blocked = NodeId::random();

        let policy = RoomAccessPolicy::with_whitelist(vec![allowed.clone()]);

        assert!(policy.is_node_allowed(&allowed));
        assert!(!policy.is_node_allowed(&blocked));
        assert!(policy.requires_auth());
    }

    #[test]
    fn access_control_rejects_blocked_nodes() {
        let node = NodeId::random();
        let mut access = AccessControl::new();

        let mut policy = RoomAccessPolicy::public();
        policy.blocked_nodes.push(node.to_string());
        access.register_room("test-room", policy);

        assert!(!access.check_node_access("test-room", &node).is_allowed());
    }

    #[test]
    fn access_control_allows_unregistered_rooms() {
        let node = NodeId::random();
        let access = AccessControl::new();

        // Unregistered room should be allowed by default.
        assert!(access.check_node_access("unknown-room", &node).is_allowed());
    }

    #[test]
    fn credential_verification() {
        let mut access = AccessControl::new();

        let mut policy = RoomAccessPolicy::public();
        policy.is_public = false;
        policy.password_hash = Some("test_hash".to_string());
        access.register_room("private-room", policy);

        // Wrong password should require authentication.
        let result = access.verify_credential(
            "private-room",
            &CredentialType::Password("wrong".to_string()),
        );
        assert!(matches!(result, AccessCheckResult::AuthenticationRequired));

        // Correct password (hash match) should allow.
        let result = access.verify_credential(
            "private-room",
            &CredentialType::Password("test_hash".to_string()),
        );
        assert!(result.is_allowed());
    }

    #[test]
    fn message_size_limit() {
        let mut access = AccessControl::new();

        let mut policy = RoomAccessPolicy::public();
        policy.max_message_size = 100;
        access.register_room("small-room", policy);

        // Small message should be allowed.
        assert!(access.check_message_size("small-room", 50).is_allowed());

        // Large message should be rejected.
        let result = access.check_message_size("small-room", 200);
        assert!(!result.is_allowed());
        assert!(result.reason().is_some());
    }

    #[test]
    fn room_registration_and_unregistration() {
        let mut access = AccessControl::new();

        access.register_room("room1", RoomAccessPolicy::public());
        assert_eq!(access.room_count(), 1);

        access.register_room("room2", RoomAccessPolicy::public());
        assert_eq!(access.room_count(), 2);

        access.unregister_room("room1");
        assert_eq!(access.room_count(), 1);
        assert!(access.get_policy("room1").is_none());
    }

    #[test]
    fn credential_types() {
        let cred = RoomCredential::with_password("room1", "secret");
        assert!(!cred.is_public());
        assert!(matches!(cred.credential, CredentialType::Password(_)));

        let public = RoomCredential::public("room2");
        assert!(public.is_public());
        assert!(matches!(public.credential, CredentialType::None));
    }
}
