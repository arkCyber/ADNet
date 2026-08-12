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
    /// Multihash containing hash function and digest
    multihash: Multihash,
}

impl Cid {
    /// Create a CIDv0 from a multihash (must be SHA-256).
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

    /// Create a CIDv1 from a codec and multihash.
    pub fn new_v1(codec: Codec, multihash: Multihash) -> Self {
        Self {
            version: 1,
            codec: codec as u64,
            multihash,
        }
    }

    /// Create a CIDv1 with DAG-PB codec.
    pub fn new_v1_dag_pb(multihash: Multihash) -> Self {
        Self::new_v1(Codec::DagPb, multihash)
    }

    /// Create a CIDv1 with DAG-CBOR codec.
    pub fn new_v1_dag_cbor(multihash: Multihash) -> Self {
        Self::new_v1(Codec::DagCbor, multihash)
    }

    /// Create a CIDv1 with raw codec.
    pub fn new_v1_raw(multihash: Multihash) -> Self {
        Self::new_v1(Codec::Raw, multihash)
    }

    /// Parse CID from bytes (CIDv1 binary format).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CidError> {
        if bytes.is_empty() {
            return Err(CidError::InvalidFormat);
        }

        if bytes[0] == 0 {
            // CIDv0
            let multihash = Multihash::from_bytes(&bytes[1..])?;
            return Self::new_v0(multihash);
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

    /// Create a CIDv1 from a BLAKE3 content hash.
    pub fn from_content_hash(hash: &crate::content::ContentHash) -> Self {
        // ContentHash is stored as hex string, convert to bytes
        let hash_bytes = hex::decode(hash.as_hex()).expect("valid hex");
        // Create a proper multihash with BLAKE3 code (0x1e)
        let multihash = Multihash::from_blake3(&hash_bytes)
            .expect("BLAKE3 hash is valid multihash");
        Self {
            version: 1,
            codec: Codec::Raw as u64,
            multihash,
        }
    }

    /// Create a CIDv1 with the raw codec from a BLAKE3 content hash.
    pub fn from_content_blake3(data: &[u8]) -> Self {
        Self::from_content_hash(&crate::content::ContentHash::from_bytes(data))
    }

    /// Create a CIDv1 with a specific codec from BLAKE3 content hash.
    pub fn from_content_blake3_with_codec(data: &[u8], codec: Codec) -> Self {
        use crate::multihash::Multihash;
        let hash_bytes = blake3::hash(data);
        let mh = Multihash::from_blake3(hash_bytes.as_bytes())
            .expect("BLAKE3 hash is valid multihash");
        Self::new_v1(codec, mh)
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
        // CIDv1 strings are typically:
        // - "bafy..." (IPFS standard base32 with prefix)
        // - Raw base32 encoded bytes (without prefix)
        //
        // The standard IPFS CIDv1 format uses base32 encoding of the binary CID.
        // The binary format is: version(1) + codec(varint) + multihash(varint + digest)
        
        let bytes = if s.starts_with("bafy") || s.starts_with("bagy") || s.starts_with("baer") || s.starts_with("baga") {
            // Standard IPFS CIDv1 format - strip prefix and decode base32
            base32_decode(&s[4..])?
        } else if s.starts_with('b') && s.len() > 4 {
            // Other base32 CID format (e.g., "b" + base32)
            base32_decode(&s[1..])?
        } else {
            // Assume it's raw base32 encoded bytes
            base32_decode(s)?
        };

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
    pub fn to_v0_string(&self) -> Option<String> {
        if self.version == 0 {
            return Some(encode_base58(&self.multihash.to_bytes()));
        }

        if self.multihash.code() == HashCode::Sha256 as u64 {
            Some(encode_base58(&self.multihash.to_bytes()))
        } else {
            None
        }
    }

    /// Parse a CID from a string or bytes.
    pub fn parse(s: &str) -> Result<Self, CidError> {
        // Try CIDv0 first
        if s.starts_with("Qm") && s.len() == 46 {
            return Self::from_v0_str(s);
        }

        // Try CIDv1 string
        if s.starts_with("bafy") || s.starts_with("bagy") || s.starts_with("baer") {
            return Self::from_v1_str(s);
        }

        // Try as hex bytes
        if let Ok(bytes) = hex::decode(s) {
            if let Ok(cid) = Self::from_bytes(&bytes) {
                return Ok(cid);
            }
        }

        // Try CIDv1 string without prefix
        Self::from_v1_str(s)
    }

    /// Get the CID version.
    pub fn version(&self) -> Version {
        if self.version == 0 {
            Version::V0
        } else {
            Version::V1
        }
    }

    /// Get the codec if it's a valid DAG codec.
    pub fn codec(&self) -> Option<Codec> {
        Codec::from_code(self.codec)
    }

    /// Get the multihash.
    pub fn hash(&self) -> &Multihash {
        &self.multihash
    }

    /// Get the hash function code.
    pub fn hash_code(&self) -> u64 {
        self.multihash.code()
    }

    /// Get the hash digest as bytes.
    pub fn hash_digest(&self) -> &[u8] {
        self.multihash.digest()
    }

    /// Verify this CID matches the given content.
    pub fn verify_content(&self, content: &[u8]) -> bool {
        // Compute BLAKE3 hash of content
        let hash = blake3::hash(content);
        // Compare with stored multihash
        // Note: The stored multihash may have a different digest length
        // We need to compare just the digest bytes
        let stored_digest = self.multihash.digest();
        let computed_digest = hash.as_bytes();
        
        // Compare the shorter of the two digests
        let compare_len = stored_digest.len().min(computed_digest.len());
        stored_digest[..compare_len] == computed_digest[..compare_len]
    }

    /// Check if this is a CIDv0.
    pub fn is_v0(&self) -> bool {
        self.version == 0
    }

    /// Check if this is a CIDv1.
    pub fn is_v1(&self) -> bool {
        self.version == 1
    }

    /// Get a hex string representation of the multihash digest.
    pub fn hash_hex(&self) -> String {
        self.multihash.to_hex()
    }

    /// Try to create a CID from an IPLD link.
    ///
    /// This is used by the DAG codec to extract CIDs from IPLD structures.
    /// Handles the conversion from ipld-core's CID representation.
    pub fn from_ipld(link: &ipld_core::ipld::Ipld) -> Result<Self, CidError> {
        match link {
            ipld_core::ipld::Ipld::Link(cid) => {
                // ipld-core uses the cid crate's Cid type
                // We need to convert it to our format by extracting the bytes
                Self::from_ipld_cid(cid)
            }
            ipld_core::ipld::Ipld::Bytes(bytes) => {
                // Try to parse as CID binary format
                Self::from_bytes(bytes)
            }
            _ => Err(CidError::InvalidFormat),
        }
    }

    /// Create our CID from an ipld-core CID.
    ///
    /// ipld-core uses the `cid` crate's CID, which has different
    /// internal representation than our custom CID.
    fn from_ipld_cid(cid: &cid::Cid) -> Result<Self, CidError> {
        // Get the bytes representation from the ipld-core CID
        let bytes = cid.to_bytes();
        Self::from_bytes(&bytes)
    }
}

impl TryFrom<&ipld_core::cid::Cid> for Cid {
    type Error = CidError;

    fn try_from(cid: &ipld_core::cid::Cid) -> Result<Self, Self::Error> {
        // Extract multihash bytes from the ipld-core CID
        let mh_bytes = cid.hash().to_bytes();
        let multihash = Multihash::from_bytes(&mh_bytes)
            .map_err(|e| CidError::Multihash(e))?;
        
        // Get version and codec
        let version = match cid.version() {
            ipld_core::cid::Version::V0 => 0,
            ipld_core::cid::Version::V1 => 1,
        };
        let codec = cid.codec();
        
        Ok(Self {
            version,
            codec,
            multihash,
        })
    }
}

impl fmt::Display for Cid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(v0) = self.to_v0_string() {
            write!(f, "{}", v0)
        } else {
            // CIDv1: use standard IPFS "bafy" prefix + base32 encoding
            // This ensures parse() can correctly round-trip the CID
            let bytes = self.to_bytes();
            let encoded = encode_base32(&bytes);
            write!(f, "bafy{}", encoded)
        }
    }
}

/// Base58 alphabet (Bitcoin style)
const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Decode base58 string to bytes.
fn decode_base58(s: &str) -> Result<Vec<u8>, CidError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    
    // Build lookup table: char -> value (255 = invalid)
    fn char_to_val(c: u8) -> i8 {
        match c {
            b'1' => 0,
            b'2' => 1,
            b'3' => 2,
            b'4' => 3,
            b'5' => 4,
            b'6' => 5,
            b'7' => 6,
            b'8' => 7,
            b'9' => 8,
            b'A' => 9,
            b'B' => 10,
            b'C' => 11,
            b'D' => 12,
            b'E' => 13,
            b'F' => 14,
            b'G' => 15,
            b'H' => 16,
            // I (17) is skipped
            b'J' => 17,
            b'K' => 18,
            b'L' => 19,
            b'M' => 20,
            b'N' => 21,
            // O (22) is skipped
            b'P' => 22,
            b'Q' => 23,
            b'R' => 24,
            b'S' => 25,
            b'T' => 26,
            b'U' => 27,
            b'V' => 28,
            b'W' => 29,
            b'X' => 30,
            b'Y' => 31,
            b'Z' => 32,
            b'a' => 33,
            b'b' => 34,
            b'c' => 35,
            b'd' => 36,
            b'e' => 37,
            b'f' => 38,
            b'g' => 39,
            b'h' => 40,
            b'i' => 41,
            b'j' => 42,
            b'k' => 43,
            // l (44) is skipped
            b'm' => 44,
            b'n' => 45,
            b'o' => 46,
            b'p' => 47,
            b'q' => 48,
            b'r' => 49,
            b's' => 50,
            b't' => 51,
            b'u' => 52,
            b'v' => 53,
            b'w' => 54,
            b'x' => 55,
            b'y' => 56,
            b'z' => 57,
            _ => -1,
        }
    }
    
    // Count leading '1' characters (representing zero bytes)
    let leading_ones = s.bytes().take_while(|&c| c == b'1').count();
    let s_without_ones = &s[leading_ones..];
    
    if s_without_ones.is_empty() {
        return Ok(vec![0u8; leading_ones]);
    }
    
    // Decode base58 to big integer
    let mut result: Vec<u8> = vec![0];
    
    for c in s_without_ones.bytes() {
        let val = char_to_val(c);
        if val < 0 {
            return Err(CidError::InvalidBase58);
        }
        let val = val as usize;
        
        // Multiply result by 58 and add val
        let mut carry = val;
        for i in (0..result.len()).rev() {
            carry += (result[i] as usize) * 58;
            result[i] = (carry % 256) as u8;
            carry /= 256;
        }
        
        while carry > 0 {
            result.insert(0, (carry % 256) as u8);
            carry /= 256;
        }
    }
    
    // Add back the leading zero bytes
    let mut output = vec![0u8; leading_ones];
    output.extend_from_slice(&result);
    
    Ok(output)
}

/// Encode bytes to base58 string.
fn encode_base58(data: &[u8]) -> String {
    const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    
    if data.is_empty() {
        return String::new();
    }
    
    let mut result = Vec::new();
    let mut num = data.to_vec();
    
    // Handle leading zeros
    while !num.is_empty() && num[0] == 0 {
        result.push(b'1');
        num.remove(0);
    }
    
    // Convert to base58 using long division
    while !num.is_empty() {
        let mut carry = 0u64;
        let mut quotient = Vec::new();
        
        for &b in &num {
            carry = carry * 256 + b as u64;
            quotient.push((carry / 58) as u8);
            carry %= 58;
        }
        
        // Get the remainder as a character
        result.push(BASE58_ALPHABET[carry as usize]);
        
        // Remove leading zeros from quotient
        while !quotient.is_empty() && quotient[0] == 0 {
            quotient.remove(0);
        }
        
        num = quotient;
    }
    
    // Reverse and return
    result.reverse();
    String::from_utf8(result).expect("valid base58")
}

/// Encode bytes to base32 string (lowercase).
fn encode_base32(data: &[u8]) -> String {
    const BASE32_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

    let mut result = String::new();
    let mut bits = 0u32;
    let mut bit_count = 0;

    for &b in data {
        bits = (bits << 8) | (b as u32);
        bit_count += 8;

        while bit_count >= 5 {
            bit_count -= 5;
            let index = ((bits >> bit_count) & 0x1F) as usize;
            result.push(BASE32_ALPHABET[index] as char);
        }
    }

    if bit_count > 0 {
        let index = ((bits << (5 - bit_count)) & 0x1F) as usize;
        result.push(BASE32_ALPHABET[index] as char);
    }

    result
}

/// Decode base32 string to bytes (lowercase).
fn base32_decode(s: &str) -> Result<Vec<u8>, CidError> {
    // Use the base32 crate for decoding (base32 with lowercase rfc4648)
    // CID uses lowercase base32 encoding, no padding
    base32::decode(base32::Alphabet::RFC4648 { padding: false }, s)
        .ok_or(CidError::InvalidBase58)
}

/// Decode a varint from bytes, returning (value, bytes_consumed).
fn decode_varint(data: &[u8]) -> (u64, usize) {
    let mut result = 0u64;
    let mut consumed = 0;

    for (i, &b) in data.iter().enumerate() {
        consumed = i + 1;
        result |= ((b & 0x7F) as u64) << (i * 7);
        if b & 0x80 == 0 {
            break;
        }
    }

    (result, consumed)
}

/// Encode a varint to the given vector.
fn encode_varint(mut value: u64, output: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            output.push(byte);
            break;
        }
        output.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cid_creation() {
        let data = b"hello world";
        let cid = Cid::from_content_blake3(data);
        assert!(cid.is_v1());
        assert_eq!(cid.codec(), Some(Codec::Raw));
    }

    #[test]
    fn test_cid_creation_with_codec() {
        let data = b"hello world";
        let cid = Cid::from_content_blake3_with_codec(data, Codec::DagPb);
        assert!(cid.is_v1());
        assert_eq!(cid.codec(), Some(Codec::DagPb));
    }

    #[test]
    fn test_cid_v0() {
        let data = b"hello world";
        let cid = Cid::from_content_sha256(data).unwrap();
        assert!(cid.is_v0());
        let v0_str = cid.to_v0_string();
        assert!(v0_str.is_some());
    }

    #[test]
    fn test_cid_display() {
        let data = b"test";
        let cid = Cid::from_content_blake3(data);
        let display = format!("{}", cid);
        assert!(!display.is_empty());
    }

    #[test]
    fn test_cid_parse() {
        // Test CIDv0
        let cid0 = Cid::parse("QmT5NvUtoM5nWFfrQdVrFtvGfKFmG7AHE8P34isapyhCxX")
            .expect("failed to parse CIDv0");
        assert!(cid0.is_v0());

        // Test invalid CID (contains a space, making it invalid base58)
        let result = Cid::parse("bafyreidf Carol");
        assert!(result.is_err(), "should fail to parse invalid CID with spaces");
    }

    #[test]
    fn test_cid_verify() {
        let data = b"hello world";
        let cid = Cid::from_content_blake3(data);
        assert!(cid.verify_content(data));
        assert!(!cid.verify_content(b"different"));
    }

    #[test]
    fn test_base58_roundtrip() {
        let original = b"hello world";
        let encoded = encode_base58(original);
        let decoded = decode_base58(&encoded).unwrap();
        assert_eq!(original.to_vec(), decoded);
    }

    #[test]
    fn test_base32_roundtrip() {
        let original = b"hello world";
        let encoded = encode_base32(original);
        let decoded = base32_decode(&encoded).unwrap();
        assert_eq!(original.to_vec(), decoded);
    }

    #[test]
    fn test_varint() {
        let mut buf = Vec::new();
        encode_varint(300, &mut buf);
        let (decoded, consumed) = decode_varint(&buf);
        assert_eq!(decoded, 300);
        assert_eq!(consumed, buf.len());
    }
}
