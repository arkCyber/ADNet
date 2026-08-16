//! IPFS-compatible Multihash implementation.
//!
//! Multihash is a format for describing hashes used in IPFS. It allows the same
//! hash to be described with different hash functions while maintaining forward
//! compatibility.
//!
//! ## Format
//!
//! ```text
//! <hash-func-code><digest-length><digest>
//! ```
//!
//! - `hash-func-code`: 1-2 bytes indicating the hash function
//! - `digest-length`: 1 byte indicating the length of the digest
//! - `digest`: the raw hash bytes

use serde::{Deserialize, Serialize};
use std::fmt;

/// Multihash error types.
#[derive(Debug, thiserror::Error)]
pub enum MultihashError {
    #[error("digest length does not match hash function: expected {expected}, got {actual}")]
    DigestTooShort { expected: usize, actual: usize },

    #[error("invalid multihash encoding")]
    InvalidEncoding,

    #[error("invalid hash code: {0}")]
    InvalidCode(u64),

    #[error("unsupported hash function")]
    UnsupportedHash,

    #[error("invalid digest")]
    InvalidDigest,
}

/// Hash function codes used in multihash.
///
/// See: https://github.com/multiformats/multihash
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u64)]
pub enum HashCode {
    /// Identity hash (32 bytes)
    Identity = 0x00,
    /// SHA-256 (32 bytes)
    Sha256 = 0x12,
    /// SHA-512 (64 bytes)
    Sha512 = 0x13,
    /// BLAKE3-256 (32 bytes), registered multicodec code.
    Blake3 = 0x1e,
    /// SHA-1 (20 bytes) - deprecated but still used
    Sha1 = 0x11,
    /// MD5 (16 bytes) - deprecated but still used
    Md5 = 0xd5,
}

impl HashCode {
    /// Get the default digest length for this hash function.
    pub fn digest_len(&self) -> usize {
        match self {
            HashCode::Identity => 32,
            HashCode::Sha256 => 32,
            HashCode::Sha512 => 64,
            HashCode::Blake3 => 32,
            HashCode::Sha1 => 20,
            HashCode::Md5 => 16,
        }
    }

    /// Try to create from a raw code value.
    pub fn from_u64(code: u64) -> Option<Self> {
        match code {
            0x00 => Some(HashCode::Identity),
            0x11 => Some(HashCode::Sha1),
            0x12 => Some(HashCode::Sha256),
            0x13 => Some(HashCode::Sha512),
            0xd5 => Some(HashCode::Md5),
            0x1e => Some(HashCode::Blake3),
            _ => None,
        }
    }

    /// Convert to the raw code value.
    pub fn to_u64(&self) -> u64 {
        *self as u64
    }

    /// Get the name for this hash function.
    pub fn name(&self) -> &'static str {
        match self {
            HashCode::Identity => "identity",
            HashCode::Sha256 => "sha2-256",
            HashCode::Sha512 => "sha2-512",
            HashCode::Blake3 => "blake3",
            HashCode::Sha1 => "sha1",
            HashCode::Md5 => "md5",
        }
    }

    /// Try to get HashCode from name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "identity" => Some(HashCode::Identity),
            "sha2-256" | "sha256" => Some(HashCode::Sha256),
            "sha2-512" | "sha512" => Some(HashCode::Sha512),
            "blake3" => Some(HashCode::Blake3),
            "sha1" => Some(HashCode::Sha1),
            "md5" => Some(HashCode::Md5),
            _ => None,
        }
    }
}

impl fmt::Display for HashCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A multihash value: a self-describing hash.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Multihash {
    code: u64,
    digest: Vec<u8>,
}

impl Multihash {
    /// Create a new multihash with the given hash code and digest.
    pub fn new(code: HashCode, digest: Vec<u8>) -> Result<Self, MultihashError> {
        let expected_len = code.digest_len();
        if digest.len() != expected_len {
            return Err(MultihashError::DigestTooShort {
                expected: expected_len,
                actual: digest.len(),
            });
        }
        Ok(Self {
            code: code.to_u64(),
            digest,
        })
    }

    /// Create a multihash from raw bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MultihashError> {
        if bytes.len() < 2 {
            return Err(MultihashError::InvalidEncoding);
        }

        // Decode varint for code
        let (code, code_len) = decode_varint(bytes);
        if code_len == 0 || code_len >= bytes.len() {
            return Err(MultihashError::InvalidEncoding);
        }

        // Decode varint for length
        let (len, len_len) = decode_varint(&bytes[code_len..]);
        if len_len == 0 || code_len + len_len + len as usize > bytes.len() {
            return Err(MultihashError::InvalidEncoding);
        }

        let digest_start = code_len + len_len;
        let digest = bytes[digest_start..digest_start + len as usize].to_vec();

        Ok(Self { code, digest })
    }

    /// Convert to raw bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(2 + self.digest.len() + 10);
        encode_varint(self.code, &mut result);
        encode_varint(self.digest.len() as u64, &mut result);
        result.extend_from_slice(&self.digest);
        result
    }

    /// Get the hash code.
    pub fn code(&self) -> u64 {
        self.code
    }

    /// Get the hash code as a typed enum.
    pub fn code_typed(&self) -> Option<HashCode> {
        HashCode::from_u64(self.code)
    }

    /// Get the digest bytes.
    pub fn digest(&self) -> &[u8] {
        &self.digest
    }

    /// Get the digest as a hex string.
    pub fn hex_digest(&self) -> String {
        hex::encode(&self.digest)
    }

    /// Get the name of the hash function.
    pub fn name(&self) -> Option<&'static str> {
        self.code_typed().map(|c| c.name())
    }

    /// Convert the multihash to hex string (for compatibility).
    pub fn to_hex(&self) -> String {
        hex::encode(self.to_bytes())
    }

    /// Get the total encoded size.
    pub fn encoded_len(&self) -> usize {
        let code_len = varint_len(self.code);
        let len_len = varint_len(self.digest.len() as u64);
        code_len + len_len + self.digest.len()
    }

    /// Create from raw BLAKE3 hash bytes (A3Net native format).
    pub fn from_blake3(digest: &[u8]) -> Result<Self, MultihashError> {
        Self::new(HashCode::Blake3, digest.to_vec())
    }

    /// Create from SHA-256 hash bytes.
    pub fn from_sha256(digest: &[u8]) -> Result<Self, MultihashError> {
        Self::new(HashCode::Sha256, digest.to_vec())
    }
}

impl fmt::Debug for Multihash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code_name = self.name().unwrap_or("unknown");
        write!(
            f,
            "Multihash({}:{})",
            code_name,
            &hex::encode(&self.digest)[..16]
        )
    }
}

impl fmt::Display for Multihash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}({})",
            self.name().unwrap_or("unknown"),
            self.hex_digest()
        )
    }
}

/// Calculate the length of a varint encoding.
fn varint_len(value: u64) -> usize {
    if value < 0x80 {
        1
    } else if value < 0x4000 {
        2
    } else if value < 0x200000 {
        3
    } else if value < 0x10000000 {
        4
    } else {
        5
    }
}

/// Encode a value as unsigned LEB128 (little-endian base-128).
fn encode_varint(value: u64, output: &mut Vec<u8>) {
    let mut v = value;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            output.push(byte);
            break;
        }
        output.push(byte | 0x80);
    }
}

/// Decode a varint from the beginning of a byte slice.
fn decode_varint(bytes: &[u8]) -> (u64, usize) {
    let mut result = 0u64;
    let mut shift = 0;
    let mut len = 0;

    for &byte in bytes.iter().take(9) {
        len += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return (result, len);
        }
        shift += 7;
    }

    (0, 0)
}

/// Hash a byte slice using SHA-256 and return as multihash.
pub fn sha256(bytes: &[u8]) -> Multihash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    Multihash::new(HashCode::Sha256, result.to_vec()).unwrap()
}

/// Hash a byte slice using BLAKE3 and return as multihash.
pub fn blake3_hash(bytes: &[u8]) -> Multihash {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    Multihash::new(HashCode::Blake3, result.as_bytes().to_vec()).unwrap()
}

/// Compute BLAKE3 hash and return as multihash.
impl From<blake3::Hash> for Multihash {
    fn from(h: blake3::Hash) -> Self {
        Self::new(HashCode::Blake3, h.as_bytes().to_vec()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multihash_roundtrip() {
        let original = blake3_hash(b"hello world");
        let bytes = original.to_bytes();
        let decoded = Multihash::from_bytes(&bytes).unwrap();
        assert_eq!(original.code(), decoded.code());
        assert_eq!(original.digest(), decoded.digest());
    }

    #[test]
    fn test_sha256_multihash() {
        let mh = sha256(b"test data");
        assert_eq!(mh.code(), 0x12); // SHA-256 code
        assert_eq!(mh.digest().len(), 32);
    }

    #[test]
    fn test_blake3_multihash() {
        let mh = blake3_hash(b"test data");
        assert_eq!(mh.code(), 0x1e); // BLAKE3-256 multihash code
        assert_eq!(mh.digest().len(), 32);
    }

    #[test]
    fn test_hash_code_names() {
        assert_eq!(HashCode::Sha256.name(), "sha2-256");
        assert_eq!(HashCode::Blake3.name(), "blake3");
        assert_eq!(HashCode::from_name("sha2-256"), Some(HashCode::Sha256));
        assert_eq!(HashCode::from_name("blake3"), Some(HashCode::Blake3));
        assert_eq!(HashCode::from_name("unknown"), None);
    }

    #[test]
    fn test_varint_encoding() {
        let mut buf = Vec::new();
        encode_varint(300, &mut buf);
        assert_eq!(buf.len(), 2);
        let (decoded, len) = decode_varint(&buf);
        assert_eq!(decoded, 300);
        assert_eq!(len, 2);
    }

    #[test]
    fn test_multihash_debug() {
        let mh = blake3_hash(b"hello");
        let debug = format!("{:?}", mh);
        assert!(debug.starts_with("Multihash(blake3:"));
    }
}
