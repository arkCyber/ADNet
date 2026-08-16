//! Key management system for A3Net.
//!
//! Provides secure key storage, rotation, and revocation with support for:
//! - Automatic key rotation
//! - Key versioning
//! - Key revocation and invalidation
//! - Hardware Security Module (HSM) integration (future)
//! - Shamir's Secret Sharing (future)

use blake3::Hasher;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::error::{SecurityError, SecurityResult};

/// Key version information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyVersion {
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub rotated_at: Option<DateTime<Utc>>,
    pub key_id: String,
    pub is_active: bool,
    pub is_revoked: bool,
}

/// Metadata associated with a key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyMetadata {
    pub key_id: String,
    pub name: String,
    pub description: Option<String>,
    pub key_type: KeyType,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub tags: Vec<String>,
    pub access_count: u64,
    pub last_accessed: Option<DateTime<Utc>>,
    pub rotation_policy: Option<String>,
}

impl KeyMetadata {
    /// Create new key metadata.
    pub fn new(key_id: String, name: String, key_type: KeyType) -> Self {
        Self {
            key_id,
            name,
            description: None,
            key_type,
            created_at: Utc::now(),
            created_by: None,
            tags: Vec::new(),
            access_count: 0,
            last_accessed: None,
            rotation_policy: None,
        }
    }
}

/// Type of cryptographic key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyType {
    /// Symmetric key (AES-256-GCM)
    Symmetric,
    /// Asymmetric private key
    PrivateKey,
    /// Asymmetric public key
    PublicKey,
    /// Signing key
    Signing,
    /// Encryption key
    Encryption,
    /// Master key
    Master,
    /// Session key
    Session,
    /// HMAC key
    Hmac,
}

/// A rotating key with version management.
#[derive(Debug, Clone)]
pub struct RotatingKey {
    pub metadata: KeyMetadata,
    pub current_version: u32,
    pub versions: HashMap<u32, KeyVersion>,
    pub key_data: HashMap<u32, Vec<u8>>,
}

impl RotatingKey {
    /// Create a new rotating key.
    pub fn new(name: String, key_type: KeyType) -> Self {
        let key_id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();

        let version = KeyVersion {
            version: 1,
            created_at: now,
            expires_at: None,
            rotated_at: None,
            key_id: key_id.clone(),
            is_active: true,
            is_revoked: false,
        };

        Self {
            metadata: KeyMetadata::new(key_id.clone(), name, key_type),
            current_version: 1,
            versions: [(1, version)].into(),
            key_data: HashMap::new(),
        }
    }

    /// Generate a new key version.
    pub fn rotate(&mut self, rotation_id: Option<&str>) -> SecurityResult<KeyVersion> {
        let now = Utc::now();

        // Revoke old version
        if let Some(old_version) = self.versions.get_mut(&self.current_version) {
            old_version.is_active = false;
        }

        // Create new version
        let new_version = self.current_version + 1;
        let version = KeyVersion {
            version: new_version,
            created_at: now,
            expires_at: None,
            rotated_at: Some(now),
            key_id: self.metadata.key_id.clone(),
            is_active: true,
            is_revoked: false,
        };

        self.versions.insert(new_version, version.clone());
        self.current_version = new_version;

        Ok(version)
    }

    /// Revoke all versions of this key.
    pub fn revoke(&mut self) {
        for version in self.versions.values_mut() {
            version.is_revoked = true;
            version.is_active = false;
        }
    }

    /// Check if the key has a valid active version.
    pub fn is_valid(&self) -> bool {
        self.versions
            .values()
            .any(|v| v.is_active && !v.is_revoked)
    }

    /// Get the active version.
    pub fn active_version(&self) -> Option<&KeyVersion> {
        self.versions.get(&self.current_version)
    }
}

/// Key rotation policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRotationPolicy {
    pub id: String,
    pub name: String,
    pub key_type: KeyType,
    /// Rotation interval
    pub rotation_interval: Duration,
    /// Maximum key age before mandatory rotation
    pub max_age: Duration,
    /// Minimum number of active versions to keep
    pub min_versions: usize,
    /// Whether rotation is automatic
    pub automatic: bool,
    /// Created at
    pub created_at: DateTime<Utc>,
    /// Last rotation time
    pub last_rotation: Option<DateTime<Utc>>,
    /// Next scheduled rotation
    pub next_rotation: Option<DateTime<Utc>>,
    /// Whether this policy is enabled
    pub enabled: bool,
}

impl KeyRotationPolicy {
    /// Create a new rotation policy.
    pub fn new(name: String, key_type: KeyType, rotation_interval: Duration) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            key_type,
            rotation_interval,
            max_age: rotation_interval * 3,
            min_versions: 2,
            automatic: false,
            created_at: now,
            last_rotation: None,
            next_rotation: Some(now + rotation_interval),
            enabled: true,
        }
    }

    /// Check if rotation is due.
    pub fn is_rotation_due(&self) -> bool {
        if !self.enabled {
            return false;
        }
        if let Some(next) = self.next_rotation {
            return Utc::now() >= next;
        }
        true
    }

    /// Record a rotation.
    pub fn record_rotation(&mut self) {
        let now = Utc::now();
        self.last_rotation = Some(now);
        self.next_rotation = Some(now + self.rotation_interval);
    }
}

/// Key storage backend.
#[derive(Debug, Clone)]
pub enum KeyStoreBackend {
    /// In-memory storage (for testing)
    Memory,
    /// Encrypted file storage
    File(String),
    /// SQLite storage
    Sqlite(String),
}

/// Secure key storage.
#[derive(Debug)]
pub struct KeyStore {
    keys: Arc<RwLock<HashMap<String, RotatingKey>>>,
    policies: Arc<RwLock<HashMap<String, KeyRotationPolicy>>>,
    backend: KeyStoreBackend,
}

impl KeyStore {
    /// Create a new key store.
    pub fn new(backend: KeyStoreBackend) -> Self {
        Self {
            keys: Arc::new(RwLock::new(HashMap::new())),
            policies: Arc::new(RwLock::new(HashMap::new())),
            backend,
        }
    }

    /// Create an in-memory key store (for testing).
    pub fn memory() -> Self {
        Self::new(KeyStoreBackend::Memory)
    }

    /// Create a new rotating key.
    pub async fn create_key(
        &self,
        name: String,
        key_type: KeyType,
        key_data: Vec<u8>,
        rotation_policy: Option<KeyRotationPolicy>,
    ) -> SecurityResult<String> {
        let mut key = RotatingKey::new(name, key_type);
        key.key_data.insert(1, key_data);

        if let Some(policy) = rotation_policy {
            key.metadata.rotation_policy = Some(policy.id.clone());
            let mut policies = self.policies.write().await;
            policies.insert(policy.id.clone(), policy);
        }

        let key_id = key.metadata.key_id.clone();
        let mut keys = self.keys.write().await;
        keys.insert(key_id.clone(), key);

        Ok(key_id)
    }

    /// Get a key by ID.
    pub async fn get_key(&self, key_id: &str) -> SecurityResult<RotatingKey> {
        let keys = self.keys.read().await;
        keys
            .get(key_id)
            .cloned()
            .ok_or_else(|| SecurityError::KeyNotFound {
                id: key_id.to_string(),
            })
    }

    /// Get key data for the active version.
    pub async fn get_active_key_data(&self, key_id: &str) -> SecurityResult<Vec<u8>> {
        let mut keys = self.keys.write().await;
        let key = keys
            .get_mut(key_id)
            .ok_or_else(|| SecurityError::KeyNotFound {
                id: key_id.to_string(),
            })?;

        // Update access metadata
        key.metadata.access_count += 1;
        key.metadata.last_accessed = Some(Utc::now());

        key.key_data
            .get(&key.current_version)
            .cloned()
            .ok_or_else(|| SecurityError::KeyError {
                reason: "No key data for active version".to_string(),
            })
    }

    /// Rotate a key to a new version.
    pub async fn rotate_key(
        &self,
        key_id: &str,
        new_key_data: Vec<u8>,
    ) -> SecurityResult<KeyVersion> {
        let mut keys = self.keys.write().await;
        let key = keys
            .get_mut(key_id)
            .ok_or_else(|| SecurityError::KeyNotFound {
                id: key_id.to_string(),
            })?;

        let version = key.rotate(None)?;

        // Store new key data
        key.key_data.insert(version.version, new_key_data);

        // Update policy if exists
        if let Some(ref policy_id) = key.metadata.rotation_policy {
            let mut policies = self.policies.write().await;
            if let Some(policy) = policies.get_mut(policy_id) {
                policy.record_rotation();
            }
        }

        Ok(version)
    }

    /// Revoke a key.
    pub async fn revoke_key(&self, key_id: &str) -> SecurityResult<()> {
        let mut keys = self.keys.write().await;
        let key = keys
            .get_mut(key_id)
            .ok_or_else(|| SecurityError::KeyNotFound {
                id: key_id.to_string(),
            })?;

        key.revoke();
        Ok(())
    }

    /// Add a rotation policy.
    pub async fn add_policy(&self, policy: KeyRotationPolicy) -> SecurityResult<()> {
        let mut policies = self.policies.write().await;
        policies.insert(policy.id.clone(), policy);
        Ok(())
    }

    /// Get all policies.
    pub async fn list_policies(&self) -> Vec<KeyRotationPolicy> {
        let policies = self.policies.read().await;
        policies.values().cloned().collect()
    }

    /// Process due rotations.
    pub async fn process_due_rotations(&self) -> SecurityResult<Vec<String>> {
        let policies = self.policies.read().await;
        let mut due = Vec::new();

        for policy in policies.values() {
            if policy.is_rotation_due() {
                due.push(policy.id.clone());
            }
        }

        drop(policies);

        let mut rotated = Vec::new();
        for policy_id in due {
            // Find keys with this policy and rotate them
            let mut keys = self.keys.write().await;
            for (key_id, key) in keys.iter_mut() {
                if key.metadata.rotation_policy.as_ref() == Some(&policy_id) {
                    // Generate new key data (simplified - would use proper CSPRNG)
                    let mut new_key_data = vec![0u8; 32];
                    getrandom::getrandom(&mut new_key_data).map_err(|_| SecurityError::KeyError {
                        reason: "Failed to generate random key data".to_string(),
                    })?;

                    key.rotate(None)?;
                    key.key_data.insert(key.current_version, new_key_data);
                    rotated.push(key_id.clone());
                }
            }

            // Update policy
            let mut policies = self.policies.write().await;
            if let Some(policy) = policies.get_mut(&policy_id) {
                policy.record_rotation();
            }
        }

        Ok(rotated)
    }

    /// List all keys.
    pub async fn list_keys(&self) -> Vec<KeyMetadata> {
        let keys = self.keys.read().await;
        keys.values().map(|k| k.metadata.clone()).collect()
    }

    /// Delete a key.
    pub async fn delete_key(&self, key_id: &str) -> SecurityResult<()> {
        let mut keys = self.keys.write().await;
        keys.remove(key_id)
            .ok_or_else(|| SecurityError::KeyNotFound {
                id: key_id.to_string(),
            })?;
        Ok(())
    }

    /// Get key statistics.
    pub async fn get_stats(&self) -> KeyStoreStats {
        let keys = self.keys.read().await;
        let policies = self.policies.read().await;

        KeyStoreStats {
            total_keys: keys.len(),
            active_keys: keys.values().filter(|k| k.is_valid()).count(),
            revoked_keys: keys.values().filter(|k| !k.is_valid()).count(),
            total_versions: keys.values().map(|k| k.versions.len()).sum(),
            total_policies: policies.len(),
            enabled_policies: policies.values().filter(|p| p.enabled).count(),
        }
    }
}

/// Key store statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyStoreStats {
    pub total_keys: usize,
    pub active_keys: usize,
    pub revoked_keys: usize,
    pub total_versions: usize,
    pub total_policies: usize,
    pub enabled_policies: usize,
}

/// Key rotation error types.
#[derive(Error, Debug)]
pub enum KeyRotationError {
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Key expired")]
    KeyExpired,

    #[error("Key revoked")]
    KeyRevoked,

    #[error("Rotation failed: {0}")]
    RotationFailed(String),

    #[error("Storage error: {0}")]
    StorageError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_key_creation() {
        let store = KeyStore::memory();

        let key_id = store
            .create_key(
                "test-key".to_string(),
                KeyType::Symmetric,
                vec![0u8; 32],
                None,
            )
            .await
            .unwrap();

        let key = store.get_key(&key_id).await.unwrap();
        assert_eq!(key.metadata.name, "test-key");
    }

    #[tokio::test]
    async fn test_key_rotation() {
        let store = KeyStore::memory();

        let key_id = store
            .create_key(
                "rotating-key".to_string(),
                KeyType::Symmetric,
                vec![0u8; 32],
                None,
            )
            .await
            .unwrap();

        // Rotate the key
        let new_version = store
            .rotate_key(&key_id, vec![1u8; 32])
            .await
            .unwrap();

        assert_eq!(new_version.version, 2);

        let key = store.get_key(&key_id).await.unwrap();
        assert_eq!(key.current_version, 2);
    }

    #[tokio::test]
    async fn test_key_revocation() {
        let store = KeyStore::memory();

        let key_id = store
            .create_key(
                "revocable-key".to_string(),
                KeyType::Signing,
                vec![0u8; 32],
                None,
            )
            .await
            .unwrap();

        store.revoke_key(&key_id).await.unwrap();

        let key = store.get_key(&key_id).await.unwrap();
        assert!(!key.is_valid());
    }

    #[tokio::test]
    async fn test_rotation_policy() {
        let store = KeyStore::memory();

        let policy = KeyRotationPolicy::new(
            "auto-rotate".to_string(),
            KeyType::Symmetric,
            Duration::days(30),
        );

        store.add_policy(policy.clone()).await.unwrap();

        let policies = store.list_policies().await;
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].name, "auto-rotate");
    }

    #[tokio::test]
    async fn test_policy_is_due() {
        let policy = KeyRotationPolicy::new(
            "test-policy".to_string(),
            KeyType::Symmetric,
            Duration::seconds(1),
        );

        // Policy is enabled by default, so should be due if next_rotation is in the past
        // or exactly now. Let's verify it's initially enabled
        assert!(policy.enabled);

        // Create a new policy and check it works
        let mut policy2 = KeyRotationPolicy::new(
            "test-policy2".to_string(),
            KeyType::Symmetric,
            Duration::hours(1),
        );

        // Should not be due immediately (next rotation is in 1 hour)
        assert!(!policy2.is_rotation_due());

        // After recording rotation, should still not be due
        policy2.record_rotation();
        assert!(!policy2.is_rotation_due());

        // Disabled policy should not be due
        policy2.enabled = false;
        assert!(!policy2.is_rotation_due());
    }
}
