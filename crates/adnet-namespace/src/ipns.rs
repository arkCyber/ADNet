//! IPNS (InterPlanetary Naming System) implementation for ADNet.
//!
//! IPNS provides mutable naming for immutable content.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// IPNS record - the core data structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnRecord {
    /// Name (hash of public key).
    pub name: String,
    /// The value this name points to.
    pub value: String,
    /// Sequence number for ordering updates.
    pub sequence: u64,
    /// Time-to-live for caching.
    #[serde(default = "default_ttl_secs")]
    pub ttl_secs: u64,
    /// Timestamp when the record was created.
    pub created: u64,
    /// Timestamp when the record expires.
    pub expires: u64,
    /// Signature over the record data.
    pub signature: Vec<u8>,
    /// Signature type (0 = RSA, 1 = Ed25519, 2 = Secp256k1).
    #[serde(default)]
    pub sig_type: u8,
}

fn default_ttl_secs() -> u64 {
    3600 // 1 hour default
}

impl IpnRecord {
    /// Create a new IPNS record.
    pub fn new(name: String, value: String, ttl: Duration) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            name,
            value,
            sequence: 1,
            ttl_secs: ttl.as_secs(),
            created: now,
            expires: now + ttl.as_secs(),
            signature: Vec::new(),
            sig_type: 1, // Ed25519
        }
    }

    /// Create a record from a name and initial value.
    pub fn with_name_value(name: String, value: String) -> Self {
        Self::new(name, value, Duration::from_secs(default_ttl_secs()))
    }

    /// Update the record with a new value.
    pub fn update(&mut self, new_value: String) {
        self.value = new_value;
        self.sequence += 1;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.created = now;
        self.expires = now + self.ttl_secs;
        self.signature.clear();
    }

    /// Set TTL for the record.
    pub fn set_ttl(&mut self, ttl: Duration) {
        self.ttl_secs = ttl.as_secs();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.expires = now + self.ttl_secs;
    }

    /// Check if the record has expired.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.expires < now
    }

    /// Check if this record is newer than another.
    pub fn is_newer_than(&self, other: &IpnRecord) -> bool {
        self.sequence > other.sequence || (self.sequence == other.sequence && self.created > other.created)
    }

    /// Sign this record with a secret key.
    pub fn sign(&mut self, secret_key: &dyn SecretKey) -> Result<(), IpnsError> {
        let data = self.signing_data();
        self.signature = secret_key.sign(&data);
        Ok(())
    }

    /// Verify the record's signature using a verifier.
    pub fn verify_signature(&self, verifier: &dyn Verifier) -> bool {
        if self.signature.is_empty() {
            return false;
        }
        let data = self.signing_data();
        verifier.verify(&data, &self.signature)
    }

    /// Data that should be signed.
    fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(0); // version 0
        data.push(self.sig_type);
        data.extend_from_slice(&self.sequence.to_be_bytes());
        data.extend_from_slice(&self.ttl_secs.to_be_bytes());
        data.extend_from_slice(&self.created.to_be_bytes());
        data.extend_from_slice(&self.expires.to_be_bytes());
        let value_bytes = self.value.as_bytes();
        data.extend_from_slice(&(value_bytes.len() as u32).to_be_bytes());
        data.extend_from_slice(value_bytes);
        data
    }

    /// Encode as bytes for transmission.
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }

    /// Decode from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IpnsError> {
        bincode::deserialize(bytes).map_err(|e| IpnsError::Deserialize(e.to_string()))
    }
}

/// Secret key trait for signing IPNS records.
pub trait SecretKey: Send + Sync {
    fn sign(&self, data: &[u8]) -> Vec<u8>;
    fn public_key_bytes(&self) -> Vec<u8>;
}

/// Verifier trait for signature verification.
pub trait Verifier: Send + Sync {
    fn verify(&self, data: &[u8], signature: &[u8]) -> bool;
}

/// Ed25519-based secret key implementation.
pub struct Ed25519SecretKey {
    key: ed25519_dalek::SigningKey,
    verifying_key_bytes: Vec<u8>,
}

impl Ed25519SecretKey {
    /// Generate a new key pair.
    pub fn generate() -> Self {
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let verifying_key = key.verifying_key();
        let verifying_key_bytes = verifying_key.as_bytes().to_vec();
        Self { key, verifying_key_bytes }
    }

    /// Create from raw bytes.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, IpnsError> {
        let key = ed25519_dalek::SigningKey::from_bytes(bytes);
        let verifying_key = key.verifying_key();
        let verifying_key_bytes = verifying_key.as_bytes().to_vec();
        Ok(Self { key, verifying_key_bytes })
    }

    /// Get the IPNS name (hash of public key).
    pub fn ipns_name(&self) -> String {
        let hash = blake3::hash(&self.verifying_key_bytes);
        hash.to_hex().to_string()
    }
}

impl SecretKey for Ed25519SecretKey {
    fn sign(&self, data: &[u8]) -> Vec<u8> {
        use ed25519_dalek::Signer;
        let signature = self.key.sign(data);
        signature.to_bytes().to_vec()
    }

    fn public_key_bytes(&self) -> Vec<u8> {
        self.verifying_key_bytes.clone()
    }
}

/// Ed25519 verifier.
pub struct Ed25519Verifier {
    verifying_key: ed25519_dalek::VerifyingKey,
}

impl Ed25519Verifier {
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, IpnsError> {
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(bytes)
            .map_err(|_| IpnsError::InvalidKey("Invalid Ed25519 public key".into()))?;
        Ok(Self { verifying_key })
    }
}

impl Verifier for Ed25519Verifier {
    fn verify(&self, data: &[u8], signature: &[u8]) -> bool {
        use ed25519_dalek::Verifier;
        if signature.len() != 64 {
            return false;
        }
        let sig_array: [u8; 64] = match signature.try_into() {
            Ok(a) => a,
            Err(_) => return false,
        };
        let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
        self.verifying_key.verify(data, &signature).is_ok()
    }
}

/// IPNS error types.
#[derive(Debug, thiserror::Error)]
pub enum IpnsError {
    #[error("Signature verification failed")]
    InvalidSignature,
    #[error("Record expired")]
    Expired,
    #[error("Invalid key: {0}")]
    InvalidKey(String),
    #[error("Serialization error: {0}")]
    Deserialize(String),
    #[error("Not authorized")]
    NotAuthorized,
    #[error("Record not found")]
    NotFound,
    #[error("transport: {0}")]
    Transport(String),
    #[error("transport unavailable")]
    Unavailable,
}

/// IPNS name resolver.
pub struct IpnResolver {
    cache: RwLock<HashMap<String, IpnRecord>>,
    cache_ttl: Duration,
}

impl std::fmt::Debug for IpnResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpnResolver").finish()
    }
}

impl IpnResolver {
    pub fn new(cache_ttl: Duration) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            cache_ttl,
        }
    }

    pub async fn resolve(&self, name: &str) -> Result<String, IpnsError> {
        {
            let cache = self.cache.read().unwrap();
            if let Some(record) = cache.get(name) {
                if !record.is_expired() {
                    return Ok(record.value.clone());
                }
            }
        }
        Err(IpnsError::NotFound)
    }

    /// Insert a record into the local cache.
    ///
    /// Sequence-monotonicity is enforced: an incoming record is
    /// accepted only if its `sequence` is strictly greater than the
    /// cached record. Equal sequences are rejected (deterministic
    /// ordering). The IPFS spec requires this — older records must
    /// never replace newer ones.
    pub fn cache_record(&self, record: IpnRecord) {
        let mut cache = self.cache.write().unwrap();
        match cache.get(&record.name) {
            Some(existing) if record.sequence <= existing.sequence => {
                // Existing record is at least as fresh. Drop the
                // incoming record silently — this is the same
                // compromise IPFS makes when gossip reorders messages.
                return;
            }
            _ => {}
        }
        cache.insert(record.name.clone(), record);
    }

    pub fn clear_expired(&self) {
        let mut cache = self.cache.write().unwrap();
        cache.retain(|_, r| !r.is_expired());
    }

    pub fn get_cached(&self, name: &str) -> Option<IpnRecord> {
        let cache = self.cache.read().unwrap();
        cache.get(name).cloned().filter(|r| !r.is_expired())
    }
}

/// IPNS publisher for announcing records.
pub struct IpnPublisher {
    local_records: RwLock<HashMap<String, IpnRecord>>,
    secret_key: Arc<dyn SecretKey>,
}

impl std::fmt::Debug for IpnPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpnPublisher").finish()
    }
}

impl IpnPublisher {
    pub fn new(secret_key: Arc<dyn SecretKey>) -> Self {
        Self {
            local_records: RwLock::new(HashMap::new()),
            secret_key,
        }
    }

    pub fn publish(&self, name: &str, value: String, ttl: Duration) -> Result<IpnRecord, IpnsError> {
        let mut record = {
            let records = self.local_records.read().unwrap();
            records.get(name).cloned().unwrap_or_else(|| {
                IpnRecord::new(name.to_string(), value.clone(), ttl)
            })
        };
        record.update(value);
        record.set_ttl(ttl);
        record.sign(&*self.secret_key)?;
        {
            let mut records = self.local_records.write().unwrap();
            records.insert(name.to_string(), record.clone());
        }
        Ok(record)
    }

    pub fn get_local(&self, name: &str) -> Option<IpnRecord> {
        let records = self.local_records.read().unwrap();
        records.get(name).cloned()
    }

    pub fn list_local(&self) -> Vec<(String, IpnRecord)> {
        let records = self.local_records.read().unwrap();
        records
            .iter()
            .filter(|(_, r)| !r.is_expired())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn secret_key(&self) -> Arc<dyn SecretKey> {
        self.secret_key.clone()
    }
}

/// Helper to get IPNS name from public key bytes.
pub fn public_key_to_ipns_name(pubkey_bytes: &[u8]) -> String {
    let hash = blake3::hash(pubkey_bytes);
    hash.to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipns_record_creation() {
        let record = IpnRecord::new(
            "k51qzi5uqu5dkkciu33khkzbcmxtyhn2...".to_string(),
            "/ipfs/Qm...".to_string(),
            Duration::from_secs(3600),
        );
        assert_eq!(record.sequence, 1);
        assert!(!record.is_expired());
    }

    #[test]
    fn test_ipns_record_update() {
        let mut record = IpnRecord::new(
            "test_name".to_string(),
            "/ipfs/QmOld".to_string(),
            Duration::from_secs(3600),
        );
        assert_eq!(record.sequence, 1);
        record.update("/ipfs/QmNew".to_string());
        assert_eq!(record.sequence, 2);
        assert_eq!(record.value, "/ipfs/QmNew");
    }

    #[test]
    fn test_record_signing() {
        let secret_key = Ed25519SecretKey::generate();
        let pubkey_bytes: [u8; 32] = secret_key.public_key_bytes().as_slice().try_into().unwrap();
        let verifier = Ed25519Verifier::from_bytes(&pubkey_bytes).unwrap();

        let mut record = IpnRecord::with_name_value(
            secret_key.ipns_name(),
            "/ipfs/QmTest".to_string(),
        );
        record.sign(&secret_key).unwrap();
        assert!(record.verify_signature(&verifier));
    }

    #[test]
    fn test_ipns_name_derivation() {
        let secret_key = Ed25519SecretKey::generate();
        let name = secret_key.ipns_name();
        assert_eq!(name.len(), 64);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn test_resolver_cache() {
        let resolver = IpnResolver::new(Duration::from_secs(3600));
        let record = IpnRecord::with_name_value(
            "test_name".to_string(),
            "/ipfs/QmCached".to_string(),
        );
        resolver.cache_record(record);
        let result = resolver.resolve("test_name").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/ipfs/QmCached");
    }

    /// Sequence-monotonicity: a record with `sequence <= existing.sequence`
    /// must be dropped. This is the IPFS rule that prevents out-of-order
    /// gossip from rolling back a name.
    #[tokio::test]
    async fn test_resolver_enforces_sequence_monotonicity() {
        let resolver = IpnResolver::new(Duration::from_secs(3600));

        let mut newer = IpnRecord::with_name_value("k".into(), "/ipfs/QmNew".into());
        newer.sequence = 5;
        resolver.cache_record(newer);

        // Older record (sequence 4) must be ignored.
        let mut older = IpnRecord::with_name_value("k".into(), "/ipfs/QmOld".into());
        older.sequence = 4;
        resolver.cache_record(older);
        let cached = resolver.get_cached("k").expect("newer record persists");
        assert_eq!(cached.value, "/ipfs/QmNew", "older record must not roll back");
        assert_eq!(cached.sequence, 5);

        // Equal sequence is also rejected (no in-place rewrite).
        let mut equal = IpnRecord::with_name_value("k".into(), "/ipfs/QmEqual".into());
        equal.sequence = 5;
        resolver.cache_record(equal);
        let cached = resolver.get_cached("k").unwrap();
        assert_eq!(cached.value, "/ipfs/QmNew", "equal sequence must not rewrite");

        // Higher sequence is accepted.
        let mut newest = IpnRecord::with_name_value("k".into(), "/ipfs/QmLatest".into());
        newest.sequence = 6;
        resolver.cache_record(newest);
        let cached = resolver.get_cached("k").unwrap();
        assert_eq!(cached.value, "/ipfs/QmLatest");
    }

    /// Signature tampering: a record with a flipped bit must fail
    /// `verify_signature`. This is the regression test for the
    /// "accepts any non-empty payload" hole the audit flagged.
    #[test]
    fn test_signature_tamper_is_rejected() {
        let secret_key = Ed25519SecretKey::generate();
        let pubkey_bytes: [u8; 32] = secret_key.public_key_bytes().as_slice().try_into().unwrap();
        let verifier = Ed25519Verifier::from_bytes(&pubkey_bytes).unwrap();

        let mut record = IpnRecord::with_name_value(
            secret_key.ipns_name(),
            "/ipfs/QmTamper".into(),
        );
        record.sign(&secret_key).unwrap();
        assert!(record.verify_signature(&verifier));

        // Tamper with the value.
        record.value = "/ipfs/QmReplaced".into();
        assert!(!record.verify_signature(&verifier));

        // Restore the value but flip a bit in the signature.
        let mut record = IpnRecord::with_name_value(
            secret_key.ipns_name(),
            "/ipfs/QmTamper".into(),
        );
        record.sign(&secret_key).unwrap();
        let original = record.signature.clone();
        let mut tampered = original.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        record.signature = tampered;
        assert!(!record.verify_signature(&verifier));

        // A different key must not validate the record.
        let other = Ed25519SecretKey::generate();
        let other_pub: [u8; 32] = other.public_key_bytes().as_slice().try_into().unwrap();
        let other_verifier = Ed25519Verifier::from_bytes(&other_pub).unwrap();
        let mut record = IpnRecord::with_name_value(
            secret_key.ipns_name(),
            "/ipfs/QmTamper".into(),
        );
        record.sign(&secret_key).unwrap();
        assert!(!record.verify_signature(&other_verifier));
    }

    /// `is_newer_than` semantics: equal sequence number + newer
    /// timestamp still counts as newer (sub-second tie-break).
    #[test]
    fn test_is_newer_than_uses_sequence_then_timestamp() {
        let mut a = IpnRecord::with_name_value("k".into(), "v1".into());
        a.sequence = 10;
        a.created = 100;
        let mut b = IpnRecord::with_name_value("k".into(), "v2".into());
        b.sequence = 10;
        b.created = 200;
        assert!(b.is_newer_than(&a));
        assert!(!a.is_newer_than(&b));
        let mut c = IpnRecord::with_name_value("k".into(), "v3".into());
        c.sequence = 11;
        c.created = 50;
        assert!(c.is_newer_than(&b));
    }
}
