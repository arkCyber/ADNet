//! DHT records — provider announcements and IPNS-like mutable records.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

use adnet_types::NodeId;

/// Key type for DHT operations.
/// In Kademlia, keys are arbitrary bytes; we use content hashes as keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DhtKey(#[serde(with = "hex_bytes")] Vec<u8>);

impl DhtKey {
    /// Create from a content hash hex string.
    pub fn from_content_hash_hex(hex: &str) -> Self {
        Self(hex::decode(hex).unwrap_or_default())
    }

    /// Create from raw bytes.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// XOR distance to another key (Kademlia metric).
    pub fn xor_distance(&self, other: &DhtKey) -> Vec<u8> {
        self.0
            .iter()
            .zip(other.0.iter())
            .map(|(a, b)| a ^ b)
            .collect()
    }

    /// Log distance (Kademlia bucket index).
    pub fn log_distance(&self, other: &DhtKey) -> Option<u8> {
        let xor = self.xor_distance(other);
        for (i, &byte) in xor.iter().enumerate().rev() {
            if byte != 0 {
                for bit in (0..8).rev() {
                    if (byte >> bit) & 1 == 1 {
                        return Some((i * 8 + (7 - bit)) as u8);
                    }
                }
            }
        }
        Some(0)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn as_hex(&self) -> String {
        hex::encode(&self.0)
    }
}

impl From<&adnet_types::content::ContentHash> for DhtKey {
    fn from(hash: &adnet_types::content::ContentHash) -> Self {
        Self::from_content_hash_hex(hash.as_hex())
    }
}

/// Provider record: announces that a node can serve content for a given key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRecord {
    /// The key this provider claims to serve.
    pub key: DhtKey,
    /// The provider's node ID.
    pub provider_id: NodeId,
    /// Provider's multiaddr (can be encoded as string).
    pub provider_addr: String,
    /// Time-to-live in seconds (default 24 hours).
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
    /// When this record was created.
    #[serde(default = "now_secs")]
    pub created_at: u64,
    /// Signature over the record data (provider must sign to prove ownership).
    #[serde(skip)]
    pub signature: Option<Vec<u8>>,
}

fn default_ttl() -> u64 {
    86400 // 24 hours
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl ProviderRecord {
    /// Create a new provider record.
    pub fn new(key: DhtKey, provider_id: NodeId, provider_addr: String) -> Self {
        Self {
            key,
            provider_id,
            provider_addr,
            ttl_secs: default_ttl(),
            created_at: now_secs(),
            signature: None,
        }
    }

    /// Check if the record has expired.
    pub fn is_expired(&self) -> bool {
        let now = now_secs();
        self.created_at + self.ttl_secs < now
    }

    /// Remaining TTL in seconds.
    pub fn remaining_ttl(&self) -> Duration {
        let now = now_secs();
        let expires_at = self.created_at + self.ttl_secs;
        if expires_at <= now {
            Duration::ZERO
        } else {
            Duration::from_secs(expires_at - now)
        }
    }

    /// Sign this record using the provider's secret key.
    pub fn sign(&mut self, secret_key: &impl Signer) {
        let data = self.signing_data();
        self.signature = Some(secret_key.sign(&data));
    }

    /// Verify the record's signature.
    pub fn verify_signature(&self, public_key: &impl Verifier) -> bool {
        match &self.signature {
            Some(sig) => public_key.verify(&self.signing_data(), sig),
            None => false,
        }
    }

    /// Data to be signed (deterministic serialization without signature field).
    fn signing_data(&self) -> Vec<u8> {
        let data = format!(
            "{}:{}:{}:{}",
            self.key.as_hex(),
            self.provider_id,
            self.provider_addr,
            self.ttl_secs
        );
        data.into_bytes()
    }
}

/// Trait for signing operations.
pub trait Signer {
    fn sign(&self, data: &[u8]) -> Vec<u8>;
}

/// Trait for signature verification.
pub trait Verifier {
    fn verify(&self, data: &[u8], signature: &[u8]) -> bool;
}

/// IPNS-like mutable record for pointing to content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnRecord {
    /// The name (typically the public key hash, like IPNS).
    pub name: DhtKey,
    /// The current value (typically a content hash or path).
    pub value: String,
    /// Sequence number for versioning (incremented on each update).
    pub sequence: u64,
    /// Time-to-live for the record.
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
    /// Timestamp when the record was created.
    #[serde(default = "now_secs")]
    pub created: u64,
    /// Timestamp when the record expires.
    pub expires: u64,
    /// Signature over the record.
    pub signature: Vec<u8>,
}

impl IpnRecord {
    /// Create a new IPNS record.
    pub fn new(name: DhtKey, value: String) -> Self {
        let now = now_secs();
        Self {
            name,
            value,
            sequence: 1,
            ttl_secs: default_ttl(),
            created: now,
            expires: now + default_ttl(),
            signature: Vec::new(),
        }
    }

    /// Update the record with a new value (increments sequence).
    pub fn update(&mut self, new_value: String) {
        self.value = new_value;
        self.sequence += 1;
        let now = now_secs();
        self.created = now;
        self.expires = now + self.ttl_secs;
    }

    /// Check if the record has expired.
    pub fn is_expired(&self) -> bool {
        let now = now_secs();
        self.expires < now
    }

    /// Verify the record's signature using the public key derived from name.
    pub fn verify(&self, public_key: &impl Verifier) -> bool {
        public_key.verify(&self.signing_data(), &self.signature)
    }

    /// Data that should be signed.
    fn signing_data(&self) -> Vec<u8> {
        format!(
            "{}:{}:{}:{}:{}",
            self.name.as_hex(),
            self.value,
            self.sequence,
            self.created,
            self.expires
        )
        .into_bytes()
    }
}

/// DHT message types (similar to libp2p Kademlia).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DhtMessage {
    /// Request: find nodes closest to a key.
    FindNode {
        key: DhtKey,
        request_id: String,
    },
    /// Response: return closest nodes.
    Nodes {
        request_id: String,
        nodes: Vec<NodeInfo>,
    },
    /// Request: get providers for a key.
    GetProviders {
        key: DhtKey,
        request_id: String,
    },
    /// Response: return provider records.
    Providers {
        request_id: String,
        providers: Vec<ProviderRecord>,
    },
    /// Request: add a provider for a key.
    AddProvider {
        key: DhtKey,
        provider: ProviderRecord,
        request_id: String,
    },
    /// Request: get a value.
    GetValue {
        key: DhtKey,
        request_id: String,
    },
    /// Response: return a value.
    Value {
        request_id: String,
        value: Option<DhtValue>,
    },
    /// Request: put a value.
    PutValue {
        key: DhtKey,
        value: DhtValue,
        request_id: String,
    },
    /// Response: acknowledgment of put.
    PutAck {
        request_id: String,
        success: bool,
    },
}

/// Information about a node in the DHT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub addrs: Vec<String>,
}

/// A value stored in the DHT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtValue {
    pub data: Vec<u8>,
    pub timestamp: u64,
    pub ttl_secs: u64,
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        hex::encode(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let hex: String = Deserialize::deserialize(d)?;
        hex::decode(&hex).map_err(serde::de::Error::custom)
    }
}
