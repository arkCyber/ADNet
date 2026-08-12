//! IPFS-compatible CID (Content Identifier) implementation.
//!
//! A CID is a self-describing content-addressed identifier used in IPFS.
//! CIDs are based on multicodecs and multihashes.
//!
//! ## CIDv1 Format
//!
//! ```text
//! <version><codec><multihash>
//! ```
//!
//! - `version`: 1 byte (always 1 for CIDv1)
//! - `codec`: multiformat codec code
//! - `multihash`: the multihash of the content
//!
//! ## CIDv0 Format
//!
//! CIDv0 is a base58btc-encoded multihash (for backwards compatibility).
//! Format: `Qm...` where the multihash is SHA-256 based.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::multihash::{HashCode, Multihash, MultihashError};

/// CID error types.
#[derive(Debug, thiserror::Error)]
pub enum CidError {
    #[error("invalid CID format")]
    InvalidFormat,

    #[error("unsupported CID version: {0}")]
    UnsupportedVersion(u8),

    #[error("invalid codec: {0}")]
    InvalidCodec(u64),

    #[error("multihash error: {0}")]
    Multihash(#[from] MultihashError),

    #[error("invalid base58 encoding")]
    InvalidBase58,

    #[error("CIDv0 must use SHA-256")]
    Cidv0NotSha256,

    #[error("CID does not contain a BLAKE3-256 multihash")]
    NotBlake3,

    #[error("CIDv1 codec is not raw: {0}")]
    UnsupportedContentCodec(u64),
}

/// Codec codes for DAG formats.
///
/// See: https://github.com/multiformats/multicodec
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u64)]
pub enum Codec {
    /// DAG-PB (Protocol Buffers) - default for files
    DagPb = 0x70,
    /// DAG-CBOR
    DagCbor = 0x71,
    /// Raw data
    Raw = 0x55,
    /// DAG-JSON
    DagJson = 0x85,
}

impl Codec {
    /// Get the codec code as u64.
    pub fn code(&self) -> u64 {
        *self as u64
    }

    /// Try to create from a code value.
    pub fn from_code(code: u64) -> Option<Self> {
        match code {
            0x55 => Some(Codec::Raw),
            0x70 => Some(Codec::DagPb),
            0x71 => Some(Codec::DagCbor),
            0x85 => Some(Codec::DagJson),
            _ => None,
        }
    }

    /// Get the name for this codec.
    pub fn name(&self) -> &'static str {
        match self {
            Codec::DagPb => "dag-pb",
            Codec::DagCbor => "dag-cbor",
            Codec::Raw => "raw",
            Codec::DagJson => "dag-json",
        }
    }

    /// Try to get Codec from name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dag-pb" | "protobuf" => Some(Codec::DagPb),
            "dag-cbor" | "cbor" => Some(Codec::DagCbor),
            "raw" => Some(Codec::Raw),
            "dag-json" | "json" => Some(Codec::DagJson),
            _ => None,
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// CID (Content Identifier) version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
    /// CIDv0: base58btc-encoded SHA-256 multihash
    V0,
    /// CIDv1: binary format with version byte
    V1,
}

impl Version {
    /// Wire byte for this version (`0` or `1`).
    pub fn code(&self) -> u8 {
        match self {
            Version::V0 => 0,
            Version::V1 => 1,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Version::V0 => write!(f, "0"),
            Version::V1 => write!(f, "1"),
        }
    }
}

/// A CID (Content Identifier) for IPFS-compatible content addressing.
///
/// CIDs are self-describing content-addressed identifiers that combine
/// a version byte, a codec identifier, and a cryptographic hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cid {
    /// CID version
    version: u8,
    /// Codec identifier (multicodec)
    codec: u64,
    /// The content hash
    multihash: Multihash,
}

impl Cid {
    /// Create a new CIDv0 (for SHA-256 multihashes only).
    pub fn new_v0(multihash: Multihash) -> Result<Self, CidError> {
        if multihash.code() != HashCode::Sha256 as u64 {
            return Err(CidError::Cidv0NotSha256);
        }
        Ok(Self {
            version: 0,
            codec: 0,
            multihash,
        })
    }

    /// Create a new CIDv1 with the given codec and multihash.
    pub fn new_v1(codec: Codec, multihash: Multihash) -> Self {
        Self {
            version: 1,
            codec: codec as u64,
            multihash,
        }
    }

    /// Create a CIDv1 with dag-pb codec.
    pub fn new_v1_dag_pb(multihash: Multihash) -> Self {
        Self::new_v1(Codec::DagPb, multihash)
    }

    /// Create a CIDv1 with dag-cbor codec.
    pub fn new_v1_dag_cbor(multihash: Multihash) -> Self {
        Self::new_v1(Codec::DagCbor, multihash)
    }

    /// Create a CIDv1 with raw codec.
    pub fn new_v1_raw(multihash: Multihash) -> Self {
        Self::new_v1(Codec::Raw, multihash)
    }

    /// Create a CID from raw bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CidError> {
        if bytes.is_empty() {
            return Err(CidError::InvalidFormat);
        }
        if bytes[0] == 0 {
            // CIDv0: raw multihash bytes
            let multihash = Multihash::from_bytes(bytes)?;
            return Self::new_v0(multihash);
        }
        // CIDv1
        if bytes[0] != 1 {
            return Err(CidError::UnsupportedVersion(bytes[0]));
        }
        let (codec, codec_len) = decode_varint(&bytes[1..]);
        if codec_len == 0 {
            return Err(CidError::InvalidFormat);
        }
        let multihash = Multihash::from_bytes(&bytes[1 + codec_len..])?;
        Ok(Self {
            version: 1,
            codec,
            multihash,
        })
    }

    /// Parse a CID from a string.
    pub fn parse(s: &str) -> Result<Self, CidError> {
        if s.is_empty() {
            return Err(CidError::InvalidFormat);
        }
        if s.starts_with("ipfs://") {
            return Self::parse(&s[7..]);
        }
        if s.starts_with('Q') || (s.len() == 46 && !s.starts_with('b')) {
            // Likely CIDv0
            return Self::from_v0_str(s);
        }
        // CIDv1
        Self::from_v1_str(s)
    }

    /// Convert a BLAKE3 content hash to a CIDv1 using the raw codec.
    pub fn from_content_hash(hash: &crate::content::ContentHash) -> Self {
        Self::new_v1_raw(
            crate::multihash::Multihash::from_blake3(&hash.as_bytes())
                .expect("ContentHash is always 32 bytes"),
        )
    }

    /// Return the BLAKE3 content hash carried by this CID.
    ///
    /// This is intentionally fallible: SHA-256 CIDv0 and SHA-256 CIDv1 values
    /// cannot be reinterpreted as BLAKE3 hashes.
    pub fn to_content_hash(&self) -> Result<crate::content::ContentHash, CidError> {
        if self.multihash.code() != HashCode::Blake3 as u64 {
            return Err(CidError::NotBlake3);
        }
        crate::content::ContentHash::from_hex(&self.multihash.hex_digest())
            .map_err(|_| CidError::InvalidFormat)
    }

    /// Create a CIDv1 with the raw codec from a BLAKE3 content hash.
    pub fn from_content_blake3(data: &[u8]) -> Self {
        Self::from_content_hash(&crate::content::ContentHash::from_bytes(data))
    }

    /// Create a CID directly from content using SHA-256 hash.
    pub fn from_content_sha256(data: &[u8]) -> Result<Self, CidError> {
        let hash = crate::multihash::sha256(data);
        match Self::new_v0(hash.clone()) {
            Ok(cid) => Ok(cid),
            Err(_) => Ok(Self::new_v1(Codec::DagPb, hash)),
        }
    }

    /// Parse CIDv0 from a base58btc string.
    fn from_v0_str(s: &str) -> Result<Self, CidError> {
        if s.len() != 46 {
            return Err(CidError::InvalidFormat);
        }

        // Decode base58btc
        let bytes = decode_base58(s)?;

        // Parse as multihash
        let multihash = Multihash::from_bytes(&bytes)?;

        // Verify it's SHA-256
        if multihash.code() != HashCode::Sha256 as u64 {
            return Err(CidError::Cidv0NotSha256);
        }

        Ok(Self {
            version: 0,
            codec: 0,
            multihash,
        })
    }

    /// Parse CIDv1 from a string (base32 or raw bytes).
    fn from_v1_str(s: &str) -> Result<Self, CidError> {
        // CIDv1 strings start with "bafy", "bagy", etc.
        // Strip the prefix if present
        let s = if s.starts_with("bafy") {
            &s[4..]
        } else if s.starts_with("bagy") {
            &s[4..]
        } else if s.starts_with("baer") {
            &s[4..]
        } else {
            s
        };

        // Decode base32
        let bytes = base32_decode(s)?;

        // Parse the CID binary format
        if bytes.is_empty() {
            return Err(CidError::InvalidFormat);
        }

        if bytes[0] != 1 {
            return Err(CidError::UnsupportedVersion(bytes[0]));
        }

        if bytes.len() < 3 {
            return Err(CidError::InvalidFormat);
        }

        // Decode varint for codec
        let (codec, codec_len) = decode_varint(&bytes[1..]);
        if codec_len == 0 {
            return Err(CidError::InvalidFormat);
        }

        // Rest is the multihash
        let multihash = Multihash::from_bytes(&bytes[1 + codec_len..])?;

        Ok(Self {
            version: 1,
            codec,
            multihash,
        })
    }

    /// Convert to the binary representation.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.version == 0 {
            // CIDv0 is just the multihash bytes
            self.multihash.to_bytes()
        } else {
            // CIDv1: version + codec + multihash
            let mut result = vec![1]; // version
            encode_varint(self.codec, &mut result);
            result.extend_from_slice(&self.multihash.to_bytes());
            result
        }
    }

    /// Convert to CIDv0 string if possible (only for SHA-256).
    pub fn to_v0_string(&self) -> Result<String, CidError> {
        if self.version == 0 {
            return Ok(self.to_string());
        }

        if self.multihash.code() != HashCode::Sha256 as u64 {
            return Err(CidError::Cidv0NotSha256);
        }

        // Encode multihash as base58btc
        let bytes = self.multihash.to_bytes();
        Ok(encode_base58(&bytes))
    }

    /// Convert to CIDv1 string representation.
    pub fn to_v1_string(&self) -> String {
        let bytes = self.to_bytes();
        // CIDv1 uses base32 with "bafy" prefix (multicodec for dag-pb)
        let encoded = base32_encode(&bytes);
        // Add the multihash prefix for CIDv1 dag-pb
        format!("bafy{}", encoded)
    }

    /// Get the CID version.
    pub fn version(&self) -> Version {
        match self.version {
            0 => Version::V0,
            _ => Version::V1,
        }
    }

    /// Get the codec.
    pub fn codec(&self) -> Option<Codec> {
        Codec::from_code(self.codec)
    }

    /// Get the multihash.
    pub fn hash(&self) -> &Multihash {
        &self.multihash
    }

    /// Get the multihash as bytes.
    pub fn hash_bytes(&self) -> Vec<u8> {
        self.multihash.to_bytes()
    }

    /// Hex-encoded multihash digest (lowercase, no prefix).
    pub fn hash_hex(&self) -> String {
        self.multihash.hex_digest()
    }

    /// True if this is a CIDv0 (legacy base58btc sha-256).
    pub fn is_v0(&self) -> bool {
        self.version == 0
    }

    /// True if this is a CIDv1.
    pub fn is_v1(&self) -> bool {
        self.version == 1
    }

    /// Verify that `bytes` matches this CID's content hash.
    ///
    /// Re-computes the digest using the multihash algorithm recorded
    /// in this CID and compares it byte-for-byte. Returns `true` when
    /// the digest matches, `false` when it differs or when the hash
    /// algorithm is not implemented in this crate.
    ///
    /// This is the trust boundary that catches corrupted or malicious
    /// block payloads — [`crate::graphsync::GraphSyncEngine::handle_block`]
    /// calls it before counting a block toward request stats.
    pub fn verify_bytes(&self, bytes: &[u8]) -> bool {
        use crate::multihash::{HashCode, blake3_hash, sha256};
        let expected = self.multihash.digest();
        match self.multihash.code_typed() {
            Some(HashCode::Sha256) => sha256(bytes).digest() == expected,
            Some(HashCode::Blake3) => blake3_hash(bytes).digest() == expected,
            // Algorithms not (yet) implemented in this crate — be
            // conservative and report "could not verify". Returning
            // `false` here would silently drop legitimate blocks;
            // callers can override `verify_bytes` on their own CID
            // wrapper if they need to support more algorithms.
            Some(HashCode::Sha1 | HashCode::Sha512 | HashCode::Md5 | HashCode::Identity) | None => {
                false
            }
        }
    }
}

impl fmt::Display for Cid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.version == 0 {
            // CIDv0: base58btc
            let bytes = self.multihash.to_bytes();
            write!(f, "{}", encode_base58(&bytes))
        } else {
            // CIDv1: base32
            write!(f, "{}", self.to_v1_string())
        }
    }
}

// Helper functions for varint encoding/decoding

/// Encode a value as unsigned LEB128 (little-endian base-128).
fn encode_varint(value: u64, output: &mut Vec<u8>) {
    let mut v = value;
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if v == 0 {
            break;
        }
    }
}

/// Decode an unsigned LEB128 value.
fn decode_varint(data: &[u8]) -> (u64, usize) {
    let mut result = 0u64;
    let mut shift = 0;
    let mut len = 0;

    for &byte in data.iter() {
        if shift >= 64 {
            return (result, 0); // Overflow
        }
        result |= ((byte & 0x7f) as u64) << shift;
        len += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }

    (result, len)
}

// Base58 encoding/decoding

const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Encode bytes to base58 string.
fn encode_base58(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }

    // Count leading zeros
    let mut leading_zeros = 0;
    for &byte in data.iter() {
        if byte == 0 {
            leading_zeros += 1;
        } else {
            break;
        }
    }

    // Convert bytes to base58
    let mut result = Vec::new();
    let mut temp = data.to_vec();

    while !temp.is_empty() {
        let mut carry = 0u16;
        let mut new_temp = Vec::new();

        for &byte in temp.iter() {
            carry = carry * 256 + byte as u16;
            if carry >= 58 || !new_temp.is_empty() {
                new_temp.push((carry / 58) as u8);
            }
            carry = carry % 58;
        }

        result.push(BASE58_ALPHABET[carry as usize] as char);

        // Remove leading zeros from temp
        while !new_temp.is_empty() && new_temp[0] == 0 {
            new_temp.remove(0);
        }
        temp = new_temp;
    }

    // Add leading '1's for leading zeros
    for _ in 0..leading_zeros {
        result.push('1');
    }

    // Reverse and return
    result.reverse();
    result.into_iter().collect()
}

/// Decode base58 string to bytes.
fn decode_base58(s: &str) -> Result<Vec<u8>, CidError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }

    // Count leading '1's
    let mut leading_zeros = 0;
    for c in s.chars() {
        if c == '1' {
            leading_zeros += 1;
        } else {
            break;
        }
    }

    // Convert base58 to bytes
    let mut result: Vec<u8> = Vec::new();

    for c in s.chars() {
        let digit = match BASE58_ALPHABET.iter().position(|&x| x as char == c) {
            Some(v) => v as u16,
            None => return Err(CidError::InvalidBase58),
        };

        let mut carry = digit;
        for byte in result.iter_mut().rev() {
            carry += (*byte as u16) * 58;
            *byte = (carry % 256) as u8;
            carry /= 256;
        }

        while carry > 0 {
            result.insert(0, (carry % 256) as u8);
            carry /= 256;
        }
    }

    // Add leading zeros
    for _ in 0..leading_zeros {
        result.insert(0, 0);
    }

    Ok(result)
}

// Base32 encoding/decoding (for CIDv1)

const BASE32_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Encode bytes to base32 string (standard RFC 4648).
fn base32_encode(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    let mut buffer = 0u64;
    let mut bits_in_buffer = 0;

    for &byte in data.iter() {
        buffer = (buffer << 8) | byte as u64;
        bits_in_buffer += 8;

        while bits_in_buffer >= 5 {
            bits_in_buffer -= 5;
            let index = ((buffer >> bits_in_buffer) & 0x1F) as usize;
            result.push(BASE32_ALPHABET[index] as char);
        }
    }

    // Handle remaining bits
    if bits_in_buffer > 0 {
        let index = ((buffer << (5 - bits_in_buffer)) & 0x1F) as usize;
        result.push(BASE32_ALPHABET[index] as char);
    }

    result
}

/// Decode base32 string to bytes (standard RFC 4648).
fn base32_decode(s: &str) -> Result<Vec<u8>, CidError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }

    let s = s.to_ascii_uppercase();
    let mut result = Vec::new();
    let mut buffer = 0u64;
    let mut bits_in_buffer = 0;

    for c in s.chars() {
        let value = match BASE32_ALPHABET.iter().position(|&x| x as char == c) {
            Some(v) => v as u64,
            None if c == '=' => continue, // padding
            None => return Err(CidError::InvalidFormat),
        };

        buffer = (buffer << 5) | value;
        bits_in_buffer += 5;

        if bits_in_buffer >= 8 {
            bits_in_buffer -= 8;
            let byte = (buffer >> bits_in_buffer) as u8;
            result.push(byte);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cid_v0_roundtrip() {
        let data = b"hello world";
        let hash = crate::multihash::sha256(data);
        let cid = Cid::new_v0(hash).unwrap();
        let s = cid.to_string();
        assert_eq!(s.len(), 46);
        assert!(s.starts_with('Q'));

        // Parse back
        let parsed = Cid::parse(&s).unwrap();
        assert_eq!(parsed, cid);
    }

    #[test]
    fn test_cid_v1_blake3() {
        let data = b"hello world";
        let cid = Cid::from_content_blake3(data);
        assert_eq!(cid.version(), Version::V1);
        let s = cid.to_string();
        assert!(
            s.starts_with("bafy"),
            "CIDv1 should start with bafy, got: {}",
            s
        );
    }

    #[test]
    fn test_cid_display() {
        let data = b"test";
        let cid = Cid::from_content_blake3(data);
        let s = cid.to_string();
        assert!(!s.is_empty());
    }

    #[test]
    fn test_base58_roundtrip() {
        let original = b"hello world";
        let encoded = encode_base58(original);
        let decoded = decode_base58(&encoded).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_base58_leading_zeros() {
        let data = &[0, 0, 1, 2, 3];
        let encoded = encode_base58(data);
        let decoded = decode_base58(&encoded).unwrap();
        assert_eq!(data.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_base32_roundtrip() {
        let original = b"hello world";
        let encoded = base32_encode(original);
        let decoded = base32_decode(&encoded).unwrap();
        assert_eq!(original.as_slice(), decoded.as_slice());
    }
}
