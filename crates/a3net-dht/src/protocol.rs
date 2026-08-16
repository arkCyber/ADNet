//! DHT protocol codec - serialization for DHT messages.
//!
//! This module handles encoding/decoding of DHT protocol messages
//! for wire transmission over the network.

use rand::Rng;
use serde::{Deserialize, Serialize};

use a3net_types::NodeId;

/// ALPN identifier for DHT protocol.
/// Matches the pattern used by a3net-transport.
pub const DHT_ALPN: &[u8] = b"a3net/dht/1";

/// Wire format version.
pub const DHT_VERSION: u8 = 1;

/// Maximum peers to return in FIND_NODE response.
pub const MAX_NODES: usize = 20;

/// Maximum providers to return in GET_PROVIDERS response.
pub const MAX_PROVIDERS: usize = 20;

/// Maximum value size for PUT/GET operations.
pub const MAX_VALUE_SIZE: usize = 1024 * 1024; // 1 MiB

/// DHT message types (wire protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DhtWireMessage {
    /// Find nodes closest to a key.
    FindNode(FindNodePayload),
    /// Response with closest nodes.
    Nodes(NodesPayload),
    /// Get providers for a key.
    GetProviders(GetProvidersPayload),
    /// Response with provider records.
    Providers(ProvidersPayload),
    /// Announce that we provide content.
    AddProvider(AddProviderPayload),
    /// Acknowledgment of add provider.
    AddProviderAck(AddProviderAckPayload),
    /// Get a value.
    GetValue(GetValuePayload),
    /// Response with a value.
    Value(ValuePayload),
    /// Put a value.
    PutValue(PutValuePayload),
    /// Acknowledgment of put.
    PutAck(PutAckPayload),
    /// Ping a peer (liveness check).
    Ping(PingPayload),
    /// Pong response.
    Pong(PongPayload),
}

impl DhtWireMessage {
    /// Get the message type as a string for serialization.
    #[allow(dead_code)]
    pub fn message_type(&self) -> &'static str {
        match self {
            DhtWireMessage::FindNode(_) => "find_node",
            DhtWireMessage::Nodes(_) => "nodes",
            DhtWireMessage::GetProviders(_) => "get_providers",
            DhtWireMessage::Providers(_) => "providers",
            DhtWireMessage::AddProvider(_) => "add_provider",
            DhtWireMessage::AddProviderAck(_) => "add_provider_ack",
            DhtWireMessage::GetValue(_) => "get_value",
            DhtWireMessage::Value(_) => "value",
            DhtWireMessage::PutValue(_) => "put_value",
            DhtWireMessage::PutAck(_) => "put_ack",
            DhtWireMessage::Ping(_) => "ping",
            DhtWireMessage::Pong(_) => "pong",
        }
    }
}

/// FindNode request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindNodePayload {
    /// The key to find nodes closest to.
    pub key: Vec<u8>,
    /// Unique request identifier.
    pub request_id: RequestId,
    /// Sender's node ID (for routing back).
    pub sender_id: NodeId,
}

/// Nodes response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodesPayload {
    /// Matching request identifier.
    pub request_id: RequestId,
    /// Closest nodes found.
    pub nodes: Vec<NodeContact>,
}

/// Contact information for a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeContact {
    /// Node ID.
    pub id: NodeId,
    /// Known addresses (multiaddr strings).
    pub addrs: Vec<String>,
}

/// GetProviders request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetProvidersPayload {
    /// The key to find providers for.
    pub key: Vec<u8>,
    /// Unique request identifier.
    pub request_id: RequestId,
    /// Sender's node ID.
    pub sender_id: NodeId,
}

/// Providers response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersPayload {
    /// Matching request identifier.
    pub request_id: RequestId,
    /// Provider records.
    pub providers: Vec<ProviderRecordWire>,
}

/// Provider record for wire transmission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRecordWire {
    /// Provider's node ID.
    pub provider_id: NodeId,
    /// Provider's addresses.
    pub addrs: Vec<String>,
    /// TTL in seconds.
    pub ttl_secs: u64,
    /// Signature (optional for verification).
    pub signature: Option<Vec<u8>>,
}

/// AddProvider announcement payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddProviderPayload {
    /// The key being provided.
    pub key: Vec<u8>,
    /// Provider record.
    pub provider: ProviderRecordWire,
    /// Unique request identifier.
    pub request_id: RequestId,
    /// Sender's node ID.
    pub sender_id: NodeId,
}

/// AddProvider acknowledgment payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddProviderAckPayload {
    /// Matching request identifier.
    pub request_id: RequestId,
    /// Whether the provider was accepted.
    pub accepted: bool,
    /// Optional error message.
    pub error: Option<String>,
}

/// GetValue request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetValuePayload {
    /// The key to get.
    pub key: Vec<u8>,
    /// Unique request identifier.
    pub request_id: RequestId,
    /// Sender's node ID.
    pub sender_id: NodeId,
}

/// Value response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValuePayload {
    /// Matching request identifier.
    pub request_id: RequestId,
    /// The value if found.
    pub value: Option<ValueData>,
}

/// Value data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueData {
    /// Raw value bytes.
    pub data: Vec<u8>,
    /// Timestamp when value was stored.
    pub timestamp: u64,
    /// TTL in seconds.
    pub ttl_secs: u64,
}

/// PutValue request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutValuePayload {
    /// The key to store.
    pub key: Vec<u8>,
    /// The value to store.
    pub value: ValueData,
    /// Unique request identifier.
    pub request_id: RequestId,
    /// Sender's node ID.
    pub sender_id: NodeId,
}

/// PutValue acknowledgment payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutAckPayload {
    /// Matching request identifier.
    pub request_id: RequestId,
    /// Whether the put was successful.
    pub success: bool,
    /// Optional error message.
    pub error: Option<String>,
}

/// Ping request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingPayload {
    /// Unique request identifier.
    pub request_id: RequestId,
    /// Sender's node ID.
    pub sender_id: NodeId,
}

/// Pong response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PongPayload {
    /// Matching request identifier.
    pub request_id: RequestId,
    /// Sender's node ID (for verification).
    pub sender_id: NodeId,
}

/// Unique request identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

impl RequestId {
    /// Generate a new random request ID.
    pub fn new() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let random: u64 = rand::thread_rng().gen();
        Self(format!("{:x}-{:x}", now, random))
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

/// DHT protocol codec for encoding/decoding messages.
pub struct DhtCodec;

impl DhtCodec {
    /// Encode a message for wire transmission using JSON.
    pub fn encode(msg: &DhtWireMessage) -> Result<Vec<u8>, CodecError> {
        serde_json::to_vec(msg)
            .map_err(|e| CodecError::Encode(e.to_string()))
    }

    /// Decode a message from wire bytes using JSON.
    pub fn decode(bytes: &[u8]) -> Result<DhtWireMessage, CodecError> {
        serde_json::from_slice(bytes)
            .map_err(|e| CodecError::Decode(e.to_string()))
    }

    /// Encode a message as bytes.
    pub fn encode_bytes(msg: &DhtWireMessage) -> Result<Vec<u8>, CodecError> {
        Self::encode(msg)
    }

    /// Decode bytes into a message.
    pub fn decode_bytes(bytes: &[u8]) -> Result<DhtWireMessage, CodecError> {
        Self::decode(bytes)
    }

    /// Create a FindNode request.
    pub fn find_node(key: Vec<u8>, sender_id: NodeId) -> DhtWireMessage {
        DhtWireMessage::FindNode(FindNodePayload {
            key,
            request_id: RequestId::new(),
            sender_id,
        })
    }

    /// Create a GetProviders request.
    pub fn get_providers(key: Vec<u8>, sender_id: NodeId) -> DhtWireMessage {
        DhtWireMessage::GetProviders(GetProvidersPayload {
            key,
            request_id: RequestId::new(),
            sender_id,
        })
    }

    /// Create a Ping request.
    pub fn ping(sender_id: NodeId) -> DhtWireMessage {
        DhtWireMessage::Ping(PingPayload {
            request_id: RequestId::new(),
            sender_id,
        })
    }
}

/// Codec error types.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("Encode error: {0}")]
    Encode(String),

    #[error("Decode error: {0}")]
    Decode(String),
}

/// Builder for creating DHT protocol messages.
pub struct DhtMessageBuilder {
    sender_id: NodeId,
}

impl DhtMessageBuilder {
    /// Create a new builder with the sender's node ID.
    pub fn new(sender_id: NodeId) -> Self {
        Self { sender_id }
    }

    /// Create a FindNode request.
    pub fn find_node(&self, key: Vec<u8>) -> DhtWireMessage {
        DhtCodec::find_node(key, self.sender_id.clone())
    }

    /// Create a GetProviders request.
    pub fn get_providers(&self, key: Vec<u8>) -> DhtWireMessage {
        DhtCodec::get_providers(key, self.sender_id.clone())
    }

    /// Create an AddProvider announcement.
    pub fn add_provider(
        &self,
        key: Vec<u8>,
        provider_id: NodeId,
        addrs: Vec<String>,
        ttl_secs: u64,
    ) -> DhtWireMessage {
        DhtWireMessage::AddProvider(AddProviderPayload {
            key,
            provider: ProviderRecordWire {
                provider_id,
                addrs,
                ttl_secs,
                signature: None,
            },
            request_id: RequestId::new(),
            sender_id: self.sender_id.clone(),
        })
    }

    /// Create a Ping request.
    pub fn ping(&self) -> DhtWireMessage {
        DhtCodec::ping(self.sender_id.clone())
    }
}

// Note: Frame re-exported from a3net_transport in integration
// For standalone use, we'll use Vec<u8> directly

/// Alias for wire bytes.
pub type WireBytes = Vec<u8>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_uniqueness() {
        let id1 = RequestId::new();
        let id2 = RequestId::new();
        assert_ne!(id1.0, id2.0);
    }

    #[test]
    fn test_encode_decode_find_node() {
        let sender = NodeId::random();
        let msg = DhtCodec::find_node(vec![0u8; 32], sender.clone());

        let encoded = DhtCodec::encode(&msg).unwrap();
        let decoded = DhtCodec::decode(&encoded).unwrap();

        match decoded {
            DhtWireMessage::FindNode(payload) => {
                assert_eq!(payload.key.len(), 32);
                assert_eq!(payload.sender_id, sender);
            }
            _ => panic!("Expected FindNode"),
        }
    }

    #[test]
    fn test_encode_decode_nodes() {
        let request_id = RequestId::new();
        let nodes = vec![
            NodeContact {
                id: NodeId::random(),
                addrs: vec!["127.0.0.1:8080".to_string()],
            },
        ];
        let msg = DhtWireMessage::Nodes(NodesPayload {
            request_id,
            nodes,
        });

        let encoded = DhtCodec::encode(&msg).unwrap();
        let decoded = DhtCodec::decode(&encoded).unwrap();

        match decoded {
            DhtWireMessage::Nodes(payload) => {
                assert_eq!(payload.nodes.len(), 1);
            }
            _ => panic!("Expected Nodes"),
        }
    }

    #[test]
    fn test_encode_decode_providers() {
        let request_id = RequestId::new();
        let providers = vec![
            ProviderRecordWire {
                provider_id: NodeId::random(),
                addrs: vec!["192.168.1.1:8080".to_string()],
                ttl_secs: 86400,
                signature: None,
            },
        ];
        let msg = DhtWireMessage::Providers(ProvidersPayload {
            request_id,
            providers,
        });

        let encoded = DhtCodec::encode(&msg).unwrap();
        let decoded = DhtCodec::decode(&encoded).unwrap();

        match decoded {
            DhtWireMessage::Providers(payload) => {
                assert_eq!(payload.providers.len(), 1);
                assert_eq!(payload.providers[0].ttl_secs, 86400);
            }
            _ => panic!("Expected Providers"),
        }
    }

    #[test]
    fn test_message_builder() {
        let node_id = NodeId::random();
        let builder = DhtMessageBuilder::new(node_id.clone());

        let find_node = builder.find_node(vec![1, 2, 3]);
        match find_node {
            DhtWireMessage::FindNode(payload) => {
                assert_eq!(payload.key, vec![1, 2, 3]);
                assert_eq!(payload.sender_id, node_id);
            }
            _ => panic!("Expected FindNode"),
        }
    }

    #[test]
    fn test_ping_pong_roundtrip() {
        let sender = NodeId::random();
        let ping = DhtCodec::ping(sender.clone());

        let encoded = DhtCodec::encode(&ping).unwrap();
        let decoded = DhtCodec::decode(&encoded).unwrap();

        match decoded {
            DhtWireMessage::Ping(payload) => {
                assert_eq!(payload.sender_id, sender);
            }
            _ => panic!("Expected Ping"),
        }
    }
}
