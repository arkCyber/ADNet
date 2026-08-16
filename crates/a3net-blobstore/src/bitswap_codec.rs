//! Bitswap Protocol - Message Encoding
//!
//! This module provides message encoding/decoding for the Bitswap protocol.
//! It supports both JSON (for debugging) and a binary format compatible with
//! the IPFS Bitswap protocol.
//!
//! ## DO-178C Traceability
//!
//! - BITSWAP-5: Binary encoding ensures wire-format compatibility

use thiserror::Error;

use crate::bitswap::BitswapMessage;

// ─────────────────────────────────────────────────────────────────
// Error Types
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("encode error: {0}")]
    Encode(String),

    #[error("decode error: {0}")]
    Decode(String),

    #[error("invalid message type: {0}")]
    InvalidMessageType(String),

    #[error("buffer too small: {0} bytes available")]
    BufferTooSmall(usize),

    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),
}

/// Result type for codec operations.
pub type Result<T> = std::result::Result<T, CodecError>;

// ─────────────────────────────────────────────────────────────────
// Codec Trait
// ─────────────────────────────────────────────────────────────────

/// Trait for Bitswap message encoding/decoding.
pub trait BitswapCodec: Send + Sync {
    /// Encode a Bitswap message to bytes.
    fn encode(&self, message: &BitswapMessage) -> Result<Vec<u8>>;

    /// Decode a Bitswap message from bytes.
    fn decode(&self, bytes: &[u8]) -> Result<BitswapMessage>;

    /// Get the codec name (for debugging/logging).
    fn name(&self) -> &'static str;
}

// ─────────────────────────────────────────────────────────────────
// JSON Codec
// ─────────────────────────────────────────────────────────────────

/// JSON codec for Bitswap messages.
///
/// This codec is useful for debugging and testing.
#[derive(Debug, Clone, Default)]
pub struct JsonCodec {
    _private: (),
}

impl JsonCodec {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl BitswapCodec for JsonCodec {
    fn encode(&self, message: &BitswapMessage) -> Result<Vec<u8>> {
        serde_json::to_vec(message)
            .map_err(|e| CodecError::Encode(format!("JSON serialization failed: {}", e)))
    }

    fn decode(&self, bytes: &[u8]) -> Result<BitswapMessage> {
        serde_json::from_slice(bytes)
            .map_err(|e| CodecError::Decode(format!("JSON deserialization failed: {}", e)))
    }

    fn name(&self) -> &'static str {
        "json"
    }
}

// ─────────────────────────────────────────────────────────────────
// Binary Codec (Simple Length-Prefixed Format)
// ─────────────────────────────────────────────────────────────────

/// Binary codec for Bitswap messages.
///
/// Uses a simple length-prefixed format:
/// - Message type: 1 byte
/// - Payload length: 4 bytes (big-endian)
/// - Payload: variable length
#[derive(Debug, Clone, Default)]
pub struct BinaryCodec {
    _private: (),
}

impl BinaryCodec {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl BitswapCodec for BinaryCodec {
    fn encode(&self, message: &BitswapMessage) -> Result<Vec<u8>> {
        let json = serde_json::to_vec(message)
            .map_err(|e| CodecError::Encode(format!("JSON serialization failed: {}", e)))?;

        let msg_type = message_type_code(message);
        let mut result = Vec::with_capacity(5 + json.len());
        result.push(msg_type);

        // Length prefix (big-endian)
        let len = json.len() as u32;
        result.extend_from_slice(&len.to_be_bytes());

        result.extend_from_slice(&json);
        Ok(result)
    }

    fn decode(&self, bytes: &[u8]) -> Result<BitswapMessage> {
        if bytes.len() < 5 {
            return Err(CodecError::BufferTooSmall(bytes.len()));
        }

        let msg_type = bytes[0];
        let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;

        if bytes.len() < 5 + len {
            return Err(CodecError::BufferTooSmall(bytes.len()));
        }

        let json = &bytes[5..5 + len];
        let message = serde_json::from_slice(json)
            .map_err(|e| CodecError::Decode(format!("JSON deserialization failed: {}", e)))?;

        // Verify message type matches
        let expected_type = message_type_code(&message);
        if msg_type != expected_type {
            return Err(CodecError::InvalidMessageType(format!(
                "expected type {}, got {}",
                expected_type, msg_type
            )));
        }

        Ok(message)
    }

    fn name(&self) -> &'static str {
        "binary"
    }
}

/// Get the message type code.
fn message_type_code(msg: &BitswapMessage) -> u8 {
    match msg {
        BitswapMessage::WantHave { .. } => 0x01,
        BitswapMessage::WantBlock { .. } => 0x02,
        BitswapMessage::Have { .. } => 0x03,
        BitswapMessage::DontHave { .. } => 0x04,
        BitswapMessage::Block { .. } => 0x05,
        BitswapMessage::Cancel { .. } => 0x06,
        BitswapMessage::BatchWant { .. } => 0x07,
        BitswapMessage::BatchResponse { .. } => 0x08,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::ContentHash;

    #[test]
    fn test_json_codec_roundtrip_want_block() {
        let codec = JsonCodec::new();
        let expected_block = ContentHash::from_bytes(b"test");
        let msg = BitswapMessage::WantBlock {
            block: expected_block.clone(),
            priority: 10,
        };

        let encoded = codec.encode(&msg).expect("encode failed");
        let decoded = codec.decode(&encoded).expect("decode failed");

        match decoded {
            BitswapMessage::WantBlock { block, priority } => {
                assert_eq!(block, expected_block);
                assert_eq!(priority, 10);
            }
            _ => panic!("expected WantBlock"),
        }
    }

    #[test]
    fn test_json_codec_roundtrip_dont_have() {
        let codec = JsonCodec::new();
        let expected_block = ContentHash::from_bytes(b"missing");
        let msg = BitswapMessage::DontHave {
            block: expected_block.clone(),
        };

        let encoded = codec.encode(&msg).expect("encode failed");
        let decoded = codec.decode(&encoded).expect("decode failed");

        match decoded {
            BitswapMessage::DontHave { block } => {
                assert_eq!(block, expected_block);
            }
            _ => panic!("expected DontHave"),
        }
    }

    #[test]
    fn test_binary_codec_roundtrip() {
        let codec = BinaryCodec::new();
        let expected_block = ContentHash::from_bytes(b"binary-test");
        let msg = BitswapMessage::WantHave {
            block: expected_block.clone(),
            priority: 5,
            send_dont_have: true,
        };

        let encoded = codec.encode(&msg).expect("encode failed");
        assert!(encoded.len() > 5);

        let decoded = codec.decode(&encoded).expect("decode failed");

        match decoded {
            BitswapMessage::WantHave { block, priority, send_dont_have } => {
                assert_eq!(block, expected_block);
                assert_eq!(priority, 5);
                assert!(send_dont_have);
            }
            _ => panic!("expected WantHave"),
        }
    }

    #[test]
    fn test_binary_codec_block_roundtrip() {
        let codec = BinaryCodec::new();
        let data = b"hello bitswap".to_vec();
        let block_hash = ContentHash::from_bytes(&data);
        let expected_data = data.clone();
        let msg = BitswapMessage::Block {
            block: block_hash.clone(),
            data: expected_data.clone(),
        };

        let encoded = codec.encode(&msg).expect("encode failed");
        let decoded = codec.decode(&encoded).expect("decode failed");

        match decoded {
            BitswapMessage::Block { block, data } => {
                assert_eq!(block, block_hash);
                assert_eq!(data, expected_data);
            }
            _ => panic!("expected Block"),
        }
    }

    #[test]
    fn test_codec_name() {
        let json = JsonCodec::new();
        let binary = BinaryCodec::new();

        assert_eq!(json.name(), "json");
        assert_eq!(binary.name(), "binary");
    }

    #[test]
    fn test_binary_codec_insufficient_data() {
        let codec = BinaryCodec::new();
        let result = codec.decode(&[0x01]);
        assert!(result.is_err());
    }
}
