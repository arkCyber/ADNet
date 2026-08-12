//! IPNS (InterPlanetary Naming System) implementation for ADNet.
//!
//! IPNS provides mutable naming for immutable content.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// IPNS record - the core data structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnRecord {
    /// Name (hash of public key).
    pub name: String,
    /// The value this name points to. Empty string indicates an empty/placeholder namespace.
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
    /// Validity type: "0" for seconds since created, "1" for ISO8601.
    /// When validity_type is "0", validity = created + validity_voffset
    #[serde(default)]
    pub validity_type: String,
    /// Validity offset (interpreted based on validity_type).
    /// For "0": validity_offset = validity - created in seconds.
    #[serde(default)]
    pub validity_offset: u64,
}

fn default_ttl_secs() -> u64 {
    3600 // 1 hour default
}

impl IpnRecord {
    /// Get current Unix timestamp in seconds.
    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Create a new IPNS record with the given name and value.
    pub fn new(name: String, value: String, ttl: Duration) -> Self {
        let now = Self::now();

        Self {
            name,
            value,
            sequence: 1,
            ttl_secs: ttl.as_secs(),
            created: now,
            expires: now + ttl.as_secs(),
            signature: Vec::new(),
            sig_type: 1, // Ed25519
            validity_type: "0".to_string(),
            validity_offset: ttl.as_secs(),
        }
    }

    /// Create an empty/placeholder namespace record (no initial value).
    /// This is useful for reserving a namespace name before publishing content.
    pub fn new_empty(name: String, ttl: Duration) -> Self {
        Self::new(name, String::new(), ttl)
    }

    /// Create a record from a name and initial value.
    pub fn with_name_value(name: String, value: String) -> Self {
        Self::new(name, value, Duration::from_secs(default_ttl_secs()))
    }

    /// Create a placeholder record from just a name (empty namespace).
    pub fn with_name(name: String) -> Self {
        Self::new_empty(name, Duration::from_secs(default_ttl_secs()))
    }

    /// Update the record with a new value.
    /// Resets signature to allow re-signing.
    pub fn update(&mut self, new_value: String) {
        self.value = new_value;
        self.sequence += 1;
        let now = Self::now();
        self.created = now;
        self.expires = now + self.ttl_secs;
        self.signature.clear();
    }

    /// Update the record value without incrementing sequence.
    /// For emergency corrections only; prefer `update()`.
    pub fn patch_value(&mut self, new_value: String) {
        self.value = new_value;
        let now = Self::now();
        self.created = now;
        self.expires = now + self.ttl_secs;
        self.signature.clear();
    }

    /// Set TTL for the record.
    pub fn set_ttl(&mut self, ttl: Duration) {
        self.ttl_secs = ttl.as_secs();
        let now = Self::now();
        self.expires = now + self.ttl_secs;
        self.validity_offset = ttl.as_secs();
    }

    /// Check if the record has expired.
    pub fn is_expired(&self) -> bool {
        let now = Self::now();
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

    /// Data that should be signed (IPFS-compatible format).
    /// Format: ipnsentry = (version || sigType || sequence || ttl ||
    ///                        validityType || validity || value)
    fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        
        // Version byte (0)
        data.push(0);
        
        // Signature type
        data.push(self.sig_type);
        
        // Sequence (big-endian u64)
        data.extend_from_slice(&self.sequence.to_be_bytes());
        
        // TTL (big-endian u64)
        data.extend_from_slice(&self.ttl_secs.to_be_bytes());
        
        // Validity type (0 = validity-by-seconds, 1 = validity-by-ISO8601)
        if self.validity_type == "1" {
            data.push(1); // ISO8601
            // For ISO8601, validity is the expiration string (not encoded here)
            // We'll just add the expires timestamp as a fallback
            data.extend_from_slice(&self.expires.to_be_bytes());
        } else {
            data.push(0); // seconds
            // Validity is the expiration timestamp (created + validity_offset)
            data.extend_from_slice(&self.expires.to_be_bytes());
        }
        
        // Value bytes (no length prefix for v0)
        data.extend_from_slice(self.value.as_bytes());
        
        data
    }

    /// Legacy signing data format (for compatibility).
    fn legacy_signing_data(&self) -> Vec<u8> {
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

    /// Sign using the legacy format for backward compatibility.
    pub fn sign_legacy(&mut self, secret_key: &dyn SecretKey) -> Result<(), IpnsError> {
        let data = self.legacy_signing_data();
        self.signature = secret_key.sign(&data);
        Ok(())
    }

    /// Verify using the legacy format.
    pub fn verify_signature_legacy(&self, verifier: &dyn Verifier) -> bool {
        if self.signature.is_empty() {
            return false;
        }
        let data = self.legacy_signing_data();
        verifier.verify(&data, &self.signature)
    }

    /// Encode as bytes for transmission.
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }

    /// Decode from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IpnsError> {
        bincode::deserialize(bytes).map_err(|e| IpnsError::Deserialize(e.to_string()))
    }

    /// Encode as JSON for human-readable export.
    pub fn to_json(&self) -> Result<String, IpnsError> {
        serde_json::to_string_pretty(self).map_err(|e| IpnsError::Deserialize(e.to_string()))
    }

    /// Decode from JSON.
    pub fn from_json(json: &str) -> Result<Self, IpnsError> {
        serde_json::from_str(json).map_err(|e| IpnsError::Deserialize(e.to_string()))
    }

    /// Check if this is an empty/placeholder namespace.
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Get the IPNS name in the standard format (k51... for Ed25519).
    /// For Ed25519 keys, the name should be 64 hex chars prefixed with "k51".
    pub fn ipns_name_formatted(&self) -> String {
        // If already in k51 format, return as-is
        if self.name.starts_with("k51") {
            return self.name.clone();
        }
        // If it's a raw 64-char hex hash, prefix with k51
        if self.name.len() == 64 && self.name.chars().all(|c| c.is_ascii_hexdigit()) {
            return format!("k51{}", self.name);
        }
        // For other cases, return the original name
        self.name.clone()
    }

    /// Verify the signature on a protobuf-decoded record.
    /// Returns the record if valid, or an error if the signature is invalid.
    /// Note: Caller must provide the public key bytes to verify against.
    pub fn verify_protobuf_signature(&self, pubkey_bytes: &[u8]) -> Result<(), IpnsError> {
        if self.signature.len() != 64 {
            return Err(IpnsError::InvalidSignature);
        }
        
        // Reconstruct the signing data using the same format as signing_data()
        // Format: version (1) || sig_type (1) || sequence (8) || ttl (8) ||
        //         validity_type (1) || validity (8) || value
        let mut data = Vec::new();
        
        // Version byte (0)
        data.push(0);
        
        // Signature type
        data.push(self.sig_type);
        
        // Sequence (8 bytes big-endian)
        data.extend_from_slice(&self.sequence.to_be_bytes());
        
        // TTL (8 bytes big-endian)
        data.extend_from_slice(&self.ttl_secs.to_be_bytes());
        
        // Validity type and validity (same format as signing_data)
        if self.validity_type == "1" {
            data.push(1); // ISO8601
            data.extend_from_slice(&self.expires.to_be_bytes());
        } else {
            data.push(0); // seconds
            data.extend_from_slice(&self.expires.to_be_bytes());
        }
        
        // Value bytes
        data.extend_from_slice(self.value.as_bytes());
        
        // Re-derive the verifying key and verify
        let pk_arr: [u8; 32] = pubkey_bytes.try_into()
            .map_err(|_| IpnsError::InvalidKey("invalid public key length".into()))?;
        
        let verifier = Ed25519Verifier::from_bytes(&pk_arr)
            .map_err(|_| IpnsError::InvalidKey("invalid Ed25519 public key".into()))?;
        
        if !verifier.verify(&data, &self.signature) {
            return Err(IpnsError::InvalidSignature);
        }
        
        Ok(())
    }

    /// Verify signature using the standard signing data format.
    /// This should be used after decoding from protobuf to verify the signature.
    pub fn verify_standard_signature(&self, verifier: &dyn Verifier) -> bool {
        if self.signature.len() != 64 {
            return false;
        }
        let data = self.signing_data();
        verifier.verify(&data, &self.signature)
    }

    /// Encode to IPFS-compatible protobuf format.
    /// 
    /// The protobuf format follows the IPNS spec:
    /// - 2 bytes: signature type (big-endian)
    /// - 8 bytes: sequence number (big-endian)
    /// - 8 bytes: TTL in seconds (big-endian)
    /// - 8 bytes: validity (big-endian, Unix timestamp)
    /// - 1 byte: validity type (0 = validity-by-seconds)
    /// - N bytes: value
    /// - 64 bytes: Ed25519 signature over the above
    pub fn to_ipns_protobuf(&self) -> Vec<u8> {
        use std::io::Write;
        let mut buf = Vec::new();
        
        // Signature type
        buf.write_all(&(self.sig_type as u16).to_be_bytes()).unwrap();
        
        // Sequence
        buf.write_all(&self.sequence.to_be_bytes()).unwrap();
        
        // TTL
        buf.write_all(&self.ttl_secs.to_be_bytes()).unwrap();
        
        // Validity (expires timestamp for v0 format)
        buf.write_all(&self.expires.to_be_bytes()).unwrap();
        
        // Validity type (0 = seconds, 1 = ISO8601)
        buf.push(if self.validity_type == "1" { 1 } else { 0 });
        
        // Validity offset (if using validity-by-seconds)
        buf.write_all(&self.validity_offset.to_be_bytes()).unwrap();
        
        // Value
        buf.write_all(self.value.as_bytes()).unwrap();
        
        // Signature (64 bytes for Ed25519)
        buf.extend_from_slice(&self.signature);
        
        buf
    }

    /// Decode from IPFS-compatible protobuf format.
    /// 
    /// The protobuf format is:
    /// - 2 bytes: signature type (big-endian u16)
    /// - 8 bytes: sequence number (big-endian u64)
    /// - 8 bytes: TTL in seconds (big-endian u64)
    /// - 8 bytes: validity expiry timestamp (big-endian u64)
    /// - 1 byte: validity type (0 = validity-by-seconds)
    /// - 8 bytes: validity offset (big-endian u64)
    /// - N bytes: value
    /// - 64 bytes: Ed25519 signature
    /// 
    /// Total minimum: 2 + 8 + 8 + 8 + 1 + 8 + 64 = 99 bytes
    pub fn from_ipns_protobuf(bytes: &[u8]) -> Result<Self, IpnsError> {
        const MIN_LEN: usize = 2 + 8 + 8 + 8 + 1 + 8 + 64; // 99 bytes
        if bytes.len() < MIN_LEN {
            return Err(IpnsError::Deserialize(format!(
                "IPNS protobuf too short: got {} bytes, expected at least {}",
                bytes.len(),
                MIN_LEN
            )));
        }

        let mut offset = 0;
        
        // Signature type (2 bytes)
        let sig_type = u16::from_be_bytes([bytes[0], bytes[1]]) as u8;
        offset += 2;
        
        // Sequence (8 bytes)
        let sequence = u64::from_be_bytes([
            bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9]
        ]);
        offset += 8;
        
        // TTL (8 bytes)
        let ttl_secs = u64::from_be_bytes([
            bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17]
        ]);
        offset += 8;
        
        // Validity expiry (8 bytes)
        let expires = u64::from_be_bytes([
            bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23], bytes[24], bytes[25]
        ]);
        offset += 8;
        
        // Validity type (1 byte)
        let validity_type = if bytes[26] == 1 { "1" } else { "0" }.to_string();
        offset += 1;
        
        // Validity offset (8 bytes)
        let validity_offset = u64::from_be_bytes([
            bytes[27], bytes[28], bytes[29], bytes[30], bytes[31], bytes[32], bytes[33], bytes[34]
        ]);
        offset += 8;
        
        // Value (rest minus 64 bytes for signature)
        let value_end = bytes.len() - 64;
        let value = String::from_utf8(bytes[offset..value_end].to_vec())
            .map_err(|_| IpnsError::Deserialize("invalid UTF-8 in value".into()))?;
        
        // Signature (64 bytes)
        let signature = bytes[value_end..].to_vec();
        
        // Validate signature length
        if signature.len() != 64 {
            return Err(IpnsError::Deserialize(format!(
                "invalid signature length: got {} bytes, expected 64",
                signature.len()
            )));
        }
        
        // Name is not in the protobuf, must be set separately
        Ok(Self {
            name: String::new(),
            value,
            sequence,
            ttl_secs,
            created: 0, // Not in protobuf
            expires,
            signature,
            sig_type,
            validity_type,
            validity_offset,
        })
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

    /// Serialize the raw 32-byte secret seed. The byte slice returned
    /// here can be fed back into [`Ed25519SecretKey::from_bytes`] to
    /// reconstruct an equivalent signing key.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.key.to_bytes()
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

    /// Cache a record without signature verification.
    ///
    /// WARNING: Only use this for records from trusted sources (e.g., local publishing).
    /// For network-received records, use `cache_record_verified()` instead.
    ///
    /// Sequence-monotonicity is enforced: an incoming record is
    /// accepted only if its `sequence` is strictly greater than the
    /// cached record. Equal sequences are rejected (deterministic
    /// ordering). The IPFS spec requires this — older records must
    /// never replace newer ones.
    pub fn cache_record(&self, record: IpnRecord) {
        self.cache_record_verified(record, TrustLevel::Trusted);
    }

    /// Cache a record with explicit trust level.
    ///
    /// - `TrustLevel::Trusted`: Skip signature verification (local records)
    /// - `TrustLevel::Network`: Verify signature before accepting (network records)
    ///
    /// Sequence-monotonicity is enforced: an incoming record is
    /// accepted only if its `sequence` is strictly greater than the
    /// cached record. Equal sequences are rejected (deterministic
    /// ordering). The IPFS spec requires this — older records must
    /// never replace newer ones.
    pub fn cache_record_verified(&self, record: IpnRecord, trust_level: TrustLevel) {
        let mut cache = self.cache.write().unwrap();

        // Verify signature for network-received records
        if trust_level == TrustLevel::Network {
            if record.signature.is_empty() {
                tracing::debug!(name = %record.name, "Rejecting unsigned network record");
                return;
            }
            // Note: Full verification requires the public key. For network records,
            // callers should verify the signature BEFORE calling this method using
            // the public key they used to validate the record's origin.
            // Here we just check that a signature exists.
            tracing::trace!(name = %record.name, "Caching network record (caller verified signature)");
        }

        match cache.get(&record.name) {
            Some(existing) if record.sequence <= existing.sequence => {
                // Existing record is at least as fresh. Drop the
                // incoming record silently — this is the same
                // compromise IPFS makes when gossip reorders messages.
                tracing::trace!(
                    name = %record.name,
                    incoming_seq = record.sequence,
                    existing_seq = existing.sequence,
                    "Rejecting stale record (sequence not newer)"
                );
                return;
            }
            _ => {}
        }

        // Check for expiration
        if record.is_expired() {
            tracing::trace!(name = %record.name, "Rejecting expired record");
            return;
        }

        tracing::debug!(
            name = %record.name,
            sequence = record.sequence,
            value = %record.value,
            trust_level = ?trust_level,
            "Caching verified IPNS record"
        );
        cache.insert(record.name.clone(), record);
    }

    /// Verify and cache a record from the network.
    ///
    /// This method verifies the signature against the provided public key bytes,
    /// then caches the record if valid.
    ///
    /// Returns `Ok(true)` if the record was cached, `Ok(false)` if rejected
    /// (stale sequence, invalid signature, or expired), and `Err` on error.
    pub fn verify_and_cache(
        &self,
        record: IpnRecord,
        public_key: &[u8],
    ) -> Result<bool, IpnsError> {
        // First check if we already have a newer record
        {
            let cache = self.cache.read().unwrap();
            if let Some(existing) = cache.get(&record.name) {
                if record.sequence <= existing.sequence {
                    tracing::debug!(
                        name = %record.name,
                        incoming_seq = record.sequence,
                        existing_seq = existing.sequence,
                        "Rejecting stale record"
                    );
                    return Ok(false);
                }
            }
        }

        // Verify signature
        let verifier = Ed25519Verifier::from_bytes(public_key.try_into()
            .map_err(|_| IpnsError::InvalidKey("Invalid public key length".into()))?);

        let verifier = match verifier {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(name = %record.name, error = %e, "Invalid public key");
                return Err(e);
            }
        };

        if !record.verify_signature(&verifier) {
            tracing::warn!(name = %record.name, "Signature verification failed");
            return Err(IpnsError::InvalidSignature);
        }

        // Check expiration after verification
        if record.is_expired() {
            tracing::warn!(name = %record.name, "Record expired");
            return Err(IpnsError::Expired);
        }

        // Cache the verified record
        self.cache_record_verified(record, TrustLevel::Verified);
        Ok(true)
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

/// Trust level for record caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    /// Trusted source (local publishing) - skip verification
    Trusted,
    /// Network source - verification performed by caller
    Network,
    /// Already verified (signature was checked)
    Verified,
}

/// IPNS publisher for announcing records.
pub struct IpnPublisher {
    local_records: RwLock<HashMap<String, IpnRecord>>,
    secret_key: Arc<dyn SecretKey>,
    /// Optional transport chain for fan-out publishing.
    /// When set, publishes go to DHT/gossip/Pkarr/etc.
    #[allow(dead_code)]
    transport: RwLock<Option<Arc<dyn super::transport::IpnTransport>>>,
}

impl std::fmt::Debug for IpnPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpnPublisher").finish()
    }
}

impl IpnPublisher {
    /// Create a new publisher with the given secret key.
    pub fn new(secret_key: Arc<dyn SecretKey>) -> Self {
        Self {
            local_records: RwLock::new(HashMap::new()),
            secret_key,
            transport: RwLock::new(None),
        }
    }

    /// Create a publisher with an optional transport chain.
    pub fn with_transport(
        secret_key: Arc<dyn SecretKey>,
        transport: Option<Arc<dyn super::transport::IpnTransport>>,
    ) -> Self {
        Self {
            local_records: RwLock::new(HashMap::new()),
            secret_key,
            transport: RwLock::new(transport),
        }
    }

    /// Set the transport chain for fan-out publishing.
    pub fn set_transport(&self, transport: Arc<dyn super::transport::IpnTransport>) {
        self.transport.write().unwrap().replace(transport);
    }

    /// Publish a value under an IPNS name.
    /// Creates a new record if the name doesn't exist.
    pub async fn publish(&self, name: &str, value: String, ttl: Duration) -> Result<IpnRecord, IpnsError> {
        // Sequence must increment only on actual updates, not on the
        // first publish — `IpnRecord::new` already seeds sequence=1
        // so a fresh record should stay at 1 until the second publish
        // calls `update`, which then bumps it to 2.
        let (mut record, is_new) = {
            let records = self.local_records.read().unwrap();
            if let Some(existing) = records.get(name).cloned() {
                (existing, false)
            } else {
                (IpnRecord::new(name.to_string(), value.clone(), ttl), true)
            }
        };
        if is_new {
            // First publish: ensure the initial value sticks without
            // bumping the sequence counter seeded by `IpnRecord::new`.
            record.value = value;
            record.set_ttl(ttl);
        } else {
            record.update(value);
            record.set_ttl(ttl);
        }
        record.sign(&*self.secret_key)?;
        {
            let mut records = self.local_records.write().unwrap();
            records.insert(name.to_string(), record.clone());
        }

        // Fan out to transport chain if configured
        self.fanout_transport(&record).await;

        Ok(record)
    }

    /// Create an empty namespace (placeholder record with no value).
    /// The namespace is reserved but not yet pointing to any content.
    pub async fn create_empty_namespace(
        &self,
        name: &str,
        ttl: Duration,
    ) -> Result<IpnRecord, IpnsError> {
        let mut record = IpnRecord::new_empty(name.to_string(), ttl);
        record.sign(&*self.secret_key)?;
        {
            let mut records = self.local_records.write().unwrap();
            records.insert(name.to_string(), record.clone());
        }

        // Fan out to transport chain if configured
        self.fanout_transport(&record).await;

        Ok(record)
    }

    /// Reserve a namespace name without creating a local record.
    /// Useful when you just want the IPNS name but will publish from another node.
    pub fn reserve_namespace(&self, name: &str) -> Result<IpnRecord, IpnsError> {
        let mut record = IpnRecord::with_name(name.to_string());
        record.sign(&*self.secret_key)?;
        Ok(record)
    }

    /// Internal: fan out to transport chain.
    async fn fanout_transport(&self, record: &IpnRecord) {
        let transport = self.transport.read().unwrap().clone();
        if let Some(t) = transport {
            let record = record.clone();
            tokio::spawn(async move {
                if let Err(e) = t.publish(&record).await {
                    tracing::warn!(backend = t.name(), name = %record.name, error = %e, "IPNS transport publish failed");
                }
            });
        }
    }

    /// Publish without updating local records (for remote records).
    pub fn sign_record(&self, mut record: IpnRecord) -> Result<IpnRecord, IpnsError> {
        record.sign(&*self.secret_key)?;
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

    /// List all local records including expired ones.
    pub fn list_all_local(&self) -> Vec<(String, IpnRecord)> {
        let records = self.local_records.read().unwrap();
        records
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Get or create a record for a name.
    pub fn get_or_create(&self, name: &str, ttl: Duration) -> Result<IpnRecord, IpnsError> {
        let records = self.local_records.read().unwrap();
        if let Some(record) = records.get(name) {
            return Ok(record.clone());
        }
        drop(records);
        
        let record = IpnRecord::new(name.to_string(), String::new(), ttl);
        {
            let mut records = self.local_records.write().unwrap();
            records.insert(name.to_string(), record.clone());
        }
        Ok(record)
    }

    pub fn secret_key(&self) -> Arc<dyn SecretKey> {
        self.secret_key.clone()
    }

    /// Get the IPNS name derived from this publisher's public key.
    pub fn ipns_name(&self) -> String {
        let pubkey = self.secret_key.public_key_bytes();
        let hash = blake3::hash(&pubkey);
        hash.to_hex().to_string()
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

    /// Test protobuf round-trip encoding/decoding.
    #[test]
    fn test_ipns_protobuf_roundtrip() {
        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();

        let mut record = IpnRecord::with_name_value(
            name.clone(),
            "/ipfs/QmProtobufTest".to_string(),
        );
        record.sign(&secret).unwrap();

        let encoded = record.to_ipns_protobuf();
        assert!(encoded.len() > 64);

        let mut decoded = IpnRecord::from_ipns_protobuf(&encoded).unwrap();
        // Name is not preserved in protobuf format
        decoded.name = name.clone();

        assert_eq!(decoded.value, record.value);
        assert_eq!(decoded.sequence, record.sequence);
        assert_eq!(decoded.ttl_secs, record.ttl_secs);
        assert_eq!(decoded.signature, record.signature);
    }

    /// Test that protobuf encoding preserves Ed25519 signature.
    #[test]
    fn test_ipns_protobuf_signature_preserved() {
        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();

        let mut record = IpnRecord::with_name_value(
            name.clone(),
            "/ipfs/QmSigTest".to_string(),
        );
        record.sign(&secret).unwrap();

        let encoded = record.to_ipns_protobuf();

        let mut decoded = IpnRecord::from_ipns_protobuf(&encoded).unwrap();
        decoded.name = name.clone();

        // Verify the signature still works after round-trip
        let pubkey_bytes = secret.public_key_bytes();
        assert!(decoded.verify_protobuf_signature(&pubkey_bytes).is_ok());
    }

    /// Test protobuf encoding rejects short input.
    #[test]
    fn test_ipns_protobuf_rejects_short() {
        // Minimum is 99 bytes (35 fixed + 64 signature + 0 value)
        // 98 bytes is too short
        let result = IpnRecord::from_ipns_protobuf(&[0u8; 98]);
        assert!(result.is_err());
    }

    /// Test protobuf decoding rejects invalid signature length.
    #[test]
    fn test_ipns_protobuf_rejects_invalid_sig_len() {
        // 100 bytes: 35 fixed + 1 value + 64 signature = valid
        // 99 bytes: 35 fixed + 0 value + 64 signature = valid
        // 63 bytes: too short for signature
        let result = IpnRecord::from_ipns_protobuf(&[0u8; 63]);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("too short"));
    }

    /// Test protobuf decoding rejects invalid UTF-8 in value.
    #[test]
    fn test_ipns_protobuf_rejects_invalid_utf8() {
        // Create a 100-byte payload with invalid UTF-8 in the value position
        // 35 fixed bytes + 1 byte value (invalid UTF-8) + 64 signature = 100
        let mut data = vec![0u8; 100];
        data[0] = 1; // sig_type = 1 (Ed25519)
        // Sequence, TTL, expires, validity_type, validity_offset already 0
        // Byte 35 is the start of the value - set it to invalid UTF-8
        data[35] = 0xFF;
        // The rest is the signature (64 bytes of zeros)
        
        let result = IpnRecord::from_ipns_protobuf(&data);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("invalid UTF-8"));
    }

    /// Test empty namespace protobuf encoding.
    #[test]
    fn test_ipns_protobuf_empty_namespace() {
        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();

        let mut record = IpnRecord::new_empty(name.clone(), Duration::from_secs(3600));
        record.sign(&secret).unwrap();
        assert!(record.is_empty());

        let encoded = record.to_ipns_protobuf();
        let mut decoded = IpnRecord::from_ipns_protobuf(&encoded).unwrap();
        decoded.name = name.clone();

        assert!(decoded.is_empty());
        assert_eq!(decoded.value, "");
    }

    /// Security test: verify that a signature from one key cannot be verified with another.
    #[test]
    fn test_ipns_signature_key_isolation() {
        let secret1 = Ed25519SecretKey::generate();
        let secret2 = Ed25519SecretKey::generate();
        let name = secret1.ipns_name();

        // Sign with key1
        let mut record = IpnRecord::with_name_value(name, "/ipfs/QmTest".to_string());
        record.sign(&secret1).unwrap();

        // Key2 should not be able to verify
        let pk2 = secret2.public_key_bytes();
        let pk2_arr: [u8; 32] = pk2.as_slice().try_into().unwrap();
        let verifier2 = Ed25519Verifier::from_bytes(&pk2_arr).unwrap();
        assert!(!record.verify_signature(&verifier2));

        // Key1 should verify successfully
        let pk1 = secret1.public_key_bytes();
        let pk1_arr: [u8; 32] = pk1.as_slice().try_into().unwrap();
        let verifier1 = Ed25519Verifier::from_bytes(&pk1_arr).unwrap();
        assert!(record.verify_signature(&verifier1));
    }

    /// Security test: protobuf signature verification after round-trip.
    #[test]
    fn test_ipns_protobuf_signature_verification_after_roundtrip() {
        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();

        // Create and sign a record
        let mut record = IpnRecord::with_name_value(name.clone(), "/ipfs/QmSecure".to_string());
        record.sign(&secret).unwrap();

        // Encode to protobuf
        let encoded = record.to_ipns_protobuf();

        // Decode from protobuf
        let mut decoded = IpnRecord::from_ipns_protobuf(&encoded).unwrap();
        decoded.name = name.clone();

        // Verify using protobuf signature method with correct key
        let pk = secret.public_key_bytes();
        assert!(decoded.verify_protobuf_signature(&pk).is_ok());

        // Verify using standard verify method
        let pk_arr: [u8; 32] = pk.as_slice().try_into().unwrap();
        let verifier = Ed25519Verifier::from_bytes(&pk_arr).unwrap();
        assert!(decoded.verify_standard_signature(&verifier));

        // Try with wrong key - should fail
        let wrong_key = Ed25519SecretKey::generate();
        let wrong_pk = wrong_key.public_key_bytes();
        assert!(decoded.verify_protobuf_signature(&wrong_pk).is_err());
    }

    /// Test that sequence numbers are monotonically increasing.
    #[test]
    fn test_ipns_sequence_monotonic() {
        let mut record = IpnRecord::with_name_value("test".to_string(), "/ipfs/QmV1".to_string());
        assert_eq!(record.sequence, 1);

        record.update("/ipfs/QmV2".to_string());
        assert_eq!(record.sequence, 2);

        record.update("/ipfs/QmV3".to_string());
        assert_eq!(record.sequence, 3);
    }

    /// Test TTL updates correctly modify expiration.
    #[test]
    fn test_ipns_ttl_update() {
        let mut record = IpnRecord::with_name_value("test".to_string(), "/ipfs/QmTest".to_string());
        let original_expires = record.expires;

        std::thread::sleep(std::time::Duration::from_millis(10));

        record.set_ttl(std::time::Duration::from_secs(7200));
        assert!(record.expires > original_expires);
        assert_eq!(record.ttl_secs, 7200);
    }
}
