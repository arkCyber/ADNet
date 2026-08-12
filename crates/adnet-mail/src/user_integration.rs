//! User system integration for `adnet-mail`.
//!
//! This module bridges the email layer (`adnet-mail`) with the ADNet identity
//! and user systems (`adnet-identity`, `adnet-userstore`).
//!
//! ## Integration Points
//!
//! - **Identity binding** — map email addresses to ADNet `Address` (secp256k1)
//! - **Key lookup** — find user's encryption keys for E2EE mail
//! - **Profile enrichment** — attach display name, avatar to outgoing mail
//! - **Authentication** — use ADNet credentials for SMTP/IMAP auth

use adnet_identity::{Address, X25519PublicKey};
use adnet_userstore::{UserProfile, UserStore};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{MailError, Result};
use crate::mime::{Address as EmailAddress, Mail};

/// A user's email address bound to their ADNet identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailIdentity {
    /// The user's ADNet wallet address.
    pub adnet_address: Address,
    /// Primary email address for this user.
    pub email: String,
    /// Display name (from user profile).
    pub display_name: Option<String>,
    /// Avatar blob hash (from user profile).
    pub avatar_hash: Option<String>,
    /// User's encryption key for E2EE mail.
    pub encryption_key: Option<X25519PublicKey>,
    /// Unix seconds.
    pub bound_at: u64,
}

impl EmailIdentity {
    pub fn to_email_address(&self) -> EmailAddress {
        let addr = EmailAddress::new(&self.email);
        match &self.display_name {
            Some(name) if !name.is_empty() => addr.with_name(name),
            _ => addr,
        }
    }
}

/// Resolver that maps email addresses to ADNet identities.
///
/// This is the primary integration point between the email system
/// and the ADNet identity system.
pub struct IdentityResolver {
    user_store: Arc<dyn UserStore>,
    /// Cache of email → identity mappings.
    cache: parking_lot::RwLock<HashMap<String, EmailIdentity>>,
}

impl IdentityResolver {
    pub fn new(user_store: Arc<dyn UserStore>) -> Self {
        Self {
            user_store,
            cache: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    /// Look up a user's email identity by their email address.
    pub async fn resolve_by_email(&self, email: &str) -> Result<Option<EmailIdentity>> {
        // Check cache first.
        {
            let cache = self.cache.read();
            if let Some(identity) = cache.get(email) {
                return Ok(Some(identity.clone()));
            }
        }

        // Query user store for the user with this email.
        // Note: UserStore doesn't directly support email lookup, so we
        // scan profiles. In production, you'd add an email index.
        let profiles = self.user_store.list_profiles().await
            .map_err(|e| MailError::Internal(format!("user store error: {e}")))?;

        for profile in profiles {
            // Check if this profile's username matches the email local part
            // or if they have a bound email identity.
            // For now, we create a synthetic identity.
            let identity = self.build_identity(&profile, email).await?;
            if identity.email.eq_ignore_ascii_case(email) {
                let identity = Some(identity);
                // Cache it.
                {
                    let mut cache = self.cache.write();
                    cache.insert(email.to_lowercase(), identity.as_ref().unwrap().clone());
                }
                return Ok(identity);
            }
        }

        Ok(None)
    }

    /// Look up a user's email identity by their ADNet address.
    pub async fn resolve_by_address(&self, addr: &Address) -> Result<Option<EmailIdentity>> {
        let addr_hex = addr.to_hex();

        // Check cache.
        {
            let cache = self.cache.read();
            for identity in cache.values() {
                if identity.adnet_address.to_hex() == addr_hex {
                    return Ok(Some(identity.clone()));
                }
            }
        }

        // Query by address.
        if let Some(profile) = self.user_store.get_profile(&addr_hex).await
            .map_err(|e| MailError::Internal(format!("user store error: {e}")))?
        {
            let identity = self.build_identity(&profile, "").await?;
            return Ok(Some(identity));
        }

        Ok(None)
    }

    /// Bind an email address to a user's ADNet identity.
    pub async fn bind_email(
        &self,
        user_id: &str,
        email: &str,
    ) -> Result<EmailIdentity> {
        let profile = self.user_store.get_profile(user_id).await
            .map_err(|e| MailError::Internal(format!("user store error: {e}")))?
            .ok_or_else(|| MailError::Config("user not found".into()))?;

        let identity = self.build_identity(&profile, email).await?;

        // Cache it.
        {
            let mut cache = self.cache.write();
            cache.insert(email.to_lowercase(), identity.clone());
        }

        Ok(identity)
    }

    async fn build_identity(&self, profile: &UserProfile, email: &str) -> Result<EmailIdentity> {
        let adnet_address = Address::from_hex(&profile.user_id)
            .map_err(|_| MailError::Config("invalid user_id in profile".into()))?;

        // Look up encryption key.
        let encryption_key = self.get_encryption_key(&profile.user_id).await.ok();

        Ok(EmailIdentity {
            adnet_address,
            email: if email.is_empty() {
                format!("{}@adnet.local", profile.username)
            } else {
                email.to_string()
            },
            display_name: if profile.display_name.is_empty() {
                None
            } else {
                Some(profile.display_name.clone())
            },
            avatar_hash: profile.avatar.as_ref().map(|a| a.blob_hash.clone()),
            encryption_key,
            bound_at: profile.created_at,
        })
    }

    async fn get_encryption_key(&self, user_id: &str) -> Result<X25519PublicKey> {
        use base64::Engine as _;

        let keys = self.user_store.list_public_keys(user_id).await
            .map_err(|e| MailError::Internal(format!("user store error: {e}")))?;

        for key in keys {
            if key.algorithm == "x25519" && key.revoked_at.is_none() {
                // Key material is base64-encoded in UserPublicKey.
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&key.key_material)
                    .map_err(|e| MailError::Config(format!("invalid base64 key: {e}")))?;
                return X25519PublicKey::from_bytes(&bytes)
                    .map_err(|e| MailError::Config(format!("invalid x25519 key: {e}")));
            }
        }

        Err(MailError::Config("no x25519 key found for user".into()))
    }

    /// Clear the identity cache.
    pub fn clear_cache(&self) {
        let mut cache = self.cache.write();
        cache.clear();
    }

    /// Get cache size.
    pub fn cache_size(&self) -> usize {
        self.cache.read().len()
    }
}

/// Enrich an outgoing mail with user profile data.
pub fn enrich_mail_with_profile(mail: &mut Mail, identity: &EmailIdentity) {
    // Use the user's display name for the From address.
    mail.from = identity.to_email_address();
}

/// Helper to extract the ADNet address from an email identity.
pub fn extract_adnet_address(identity: &EmailIdentity) -> &Address {
    &identity.adnet_address
}

/// Check if an email address belongs to the ADNet domain.
pub fn is_adnet_domain(email: &str) -> bool {
    email.ends_with("@adnet.local") || email.ends_with("@adnet.mesh")
}

// ─── Mock implementation for testing ──────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use adnet_identity::Address;
    use adnet_userstore::{UserDevice, UserPreferences, UserProfile, UserPublicKey};

    /// A mock UserStore for testing.
    struct MockUserStore {
        profiles: HashMap<String, UserProfile>,
        keys: HashMap<String, Vec<UserPublicKey>>,
    }

    impl MockUserStore {
        fn new() -> Self {
            Self {
                profiles: HashMap::new(),
                keys: HashMap::new(),
            }
        }

        fn add_profile(&mut self, profile: UserProfile) {
            self.profiles.insert(profile.user_id.clone(), profile);
        }

        fn add_key(&mut self, key: UserPublicKey) {
            self.keys
                .entry(key.user_id.clone())
                .or_default()
                .push(key);
        }
    }

    #[async_trait]
    impl UserStore for MockUserStore {
        async fn put_profile(&self, _profile: UserProfile) -> adnet_userstore::UserStoreResult<()> {
            Ok(())
        }

        async fn get_profile(&self, user_id: &str) -> adnet_userstore::UserStoreResult<Option<UserProfile>> {
            Ok(self.profiles.get(user_id).cloned())
        }

        async fn put_preferences(
            &self,
            _user_id: &str,
            _prefs: UserPreferences,
        ) -> adnet_userstore::UserStoreResult<()> {
            Ok(())
        }

        async fn list_profiles(&self) -> adnet_userstore::UserStoreResult<Vec<UserProfile>> {
            Ok(self.profiles.values().cloned().collect())
        }

        async fn delete_profile(&self, _user_id: &str) -> adnet_userstore::UserStoreResult<usize> {
            Ok(0)
        }

        async fn put_public_key(&self, _key: UserPublicKey) -> adnet_userstore::UserStoreResult<()> {
            Ok(())
        }

        async fn revoke_public_key(&self, _key_id: &str) -> adnet_userstore::UserStoreResult<()> {
            Ok(())
        }

        async fn list_public_keys(&self, user_id: &str) -> adnet_userstore::UserStoreResult<Vec<UserPublicKey>> {
            Ok(self.keys.get(user_id).cloned().unwrap_or_default())
        }

        async fn put_device(&self, _device: UserDevice) -> adnet_userstore::UserStoreResult<()> {
            Ok(())
        }

        async fn revoke_device(&self, _device_id: &str) -> adnet_userstore::UserStoreResult<()> {
            Ok(())
        }

        async fn list_devices(&self, _user_id: &str) -> adnet_userstore::UserStoreResult<Vec<UserDevice>> {
            Ok(Vec::new())
        }

        async fn ensure_user_digit(&self, _user_id: &str) -> adnet_userstore::UserStoreResult<String> {
            Ok("000000000000".to_string())
        }

        async fn resolve_user_digit(&self, _user_id: &str) -> adnet_userstore::UserStoreResult<Option<String>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn identity_resolver_creates_synthetic_identity() {
        let addr_bytes = [0x42u8; 20];
        let addr = Address::from_bytes(addr_bytes);

        let mut store = MockUserStore::new();
        store.add_profile(UserProfile {
            user_id: addr.to_hex(),
            username: "alice".into(),
            display_name: "Alice Example".into(),
            avatar: None,
            bio: "".into(),
            preferences: UserPreferences::default(),
            created_at: 1000,
            updated_at: 1000,
        });

        let resolver = IdentityResolver::new(Arc::new(store));
        let identity = resolver.resolve_by_address(&addr).await.unwrap().unwrap();

        assert_eq!(identity.email, "alice@adnet.local");
        assert_eq!(identity.display_name, Some("Alice Example".into()));
        assert_eq!(identity.adnet_address.to_hex(), addr.to_hex());
    }

    #[tokio::test]
    async fn is_adnet_domain_works() {
        assert!(is_adnet_domain("alice@adnet.local"));
        assert!(is_adnet_domain("bob@adnet.mesh"));
        assert!(!is_adnet_domain("alice@gmail.com"));
        assert!(!is_adnet_domain("bob@example.org"));
    }

    #[test]
    fn email_identity_display_name_round_trip() {
        let identity = EmailIdentity {
            adnet_address: Address::from_bytes([0x42u8; 20]),
            email: "alice@adnet.local".into(),
            display_name: Some("Alice".into()),
            avatar_hash: None,
            encryption_key: None,
            bound_at: 1000,
        };

        let addr = identity.to_email_address();
        assert_eq!(addr.address, "alice@adnet.local");
        assert_eq!(addr.name, Some("Alice".into()));
    }
}
