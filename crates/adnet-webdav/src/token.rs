//! CapabilityToken — wire format for `Authorization: Capability <b64>`.
//!
//! DAL-A SR-14 (replay), SR-18 (privilege), SR-12 (auth) all flow
//! through here. The on-wire shape is a flat base64url(JSON) blob:
//!
//! ```json
//! {
//!   "capability_id": "cred-1",
//!   "nonce": "0123456789abcdef...",   // 32 bytes hex
//!   "expires_unix_ms": 1730000000000,
//!   "signature": "..."                 // HMAC-SHA256 over canonical
//! }
//! ```
//!
//! The signature is over the canonical form
//! `capability_id|nonce|expires_unix_ms` keyed by the issuer's
//! `webdav_secret` (loaded once from disk, then zeroised in
//! memory after the build). This crate owns the verifier; the
//! issuer-side lives in `adnet-pairing` (a future PR).

use base64::Engine;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct CapabilityToken {
    pub capability_id: String,
    pub nonce: [u8; 32],
    pub expires_unix_ms: i64,
    pub signature: [u8; 32],
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenWire {
    capability_id: String,
    nonce_hex: String,
    expires_unix_ms: i64,
    signature_hex: String,
}

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("malformed token header")]
    MalformedHeader,
    #[error("missing bearer prefix")]
    MissingBearer,
    #[error("base64 decode failed: {0}")]
    Base64(String),
    #[error("json decode failed: {0}")]
    Json(String),
    #[error("invalid hex: {0}")]
    Hex(String),
    #[error("invalid nonce length: got {0}")]
    NonceLength(usize),
    #[error("invalid signature length: got {0}")]
    SignatureLength(usize),
    #[error("bad signature")]
    BadSignature,
}

impl CapabilityToken {
    /// `Authorization: Capability <b64url>` → parsed token.
    pub fn from_header(header: &str) -> Result<Self, TokenError> {
        let raw = header
            .strip_prefix("Capability ")
            .or_else(|| header.strip_prefix("capability "))
            .or_else(|| header.strip_prefix("Bearer "))
            .ok_or(TokenError::MissingBearer)?
            .trim();
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|e| TokenError::Base64(e.to_string()))?;
        let wire: TokenWire = serde_json::from_slice(&bytes)
            .map_err(|e| TokenError::Json(e.to_string()))?;
        let nonce = decode_hex32(&wire.nonce_hex).map_err(TokenError::Hex)?;
        let signature = decode_hex32(&wire.signature_hex).map_err(TokenError::Hex)?;
        if wire.nonce_hex.len() != 64 {
            return Err(TokenError::NonceLength(wire.nonce_hex.len()));
        }
        if wire.signature_hex.len() != 64 {
            return Err(TokenError::SignatureLength(wire.signature_hex.len()));
        }
        Ok(Self {
            capability_id: wire.capability_id,
            nonce,
            expires_unix_ms: wire.expires_unix_ms,
            signature,
        })
    }

    /// Encode for round-trip tests. Production tokens are issued
    /// by `adnet-pairing`; this method is for the test suite.
    pub fn to_header(&self) -> String {
        let wire = TokenWire {
            capability_id: self.capability_id.clone(),
            nonce_hex: hex::encode(self.nonce),
            expires_unix_ms: self.expires_unix_ms,
            signature_hex: hex::encode(self.signature),
        };
        let json = serde_json::to_vec(&wire).unwrap_or_default();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
        format!("Capability {b64}")
    }
}

fn decode_hex32(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Verifier side. We take a `&[u8; 32]` HMAC key in tests; in
/// production the key is loaded from disk at boot.
pub struct TokenVerifier {
    key: [u8; 32],
}

impl Clone for TokenVerifier {
    fn clone(&self) -> Self {
        Self { key: self.key }
    }
}

impl TokenVerifier {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn verify(&self, token: &CapabilityToken) -> Result<(), TokenError> {
        let canonical = format!(
            "{}|{}|{}",
            token.capability_id,
            hex::encode(token.nonce),
            token.expires_unix_ms,
        );
        let expected = hmac_sha256(&self.key, canonical.as_bytes());
        // Constant-time compare.
        let mut diff = 0u8;
        for (a, b) in expected.iter().zip(token.signature.iter()) {
            diff |= a ^ b;
        }
        if diff == 0 {
            Ok(())
        } else {
            Err(TokenError::BadSignature)
        }
    }

    pub fn sign(&self, capability_id: &str, nonce: [u8; 32], expires_unix_ms: i64) -> CapabilityToken {
        let canonical = format!("{capability_id}|{}|{expires_unix_ms}", hex::encode(nonce));
        let signature = hmac_sha256(&self.key, canonical.as_bytes());
        CapabilityToken {
            capability_id: capability_id.to_string(),
            nonce,
            expires_unix_ms,
            signature,
        }
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    // Tiny HMAC implementation; we don't pull in `hmac` to keep
    // the dependency surface tight.
    let block_size = 64;
    let mut k = [0u8; 64];
    if key.len() > block_size {
        let mut h = Sha256::new();
        h.update(key);
        let digest = h.finalize();
        k[..digest.len()].copy_from_slice(&digest);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0x36u8; 64];
    let mut outer = [0x5cu8; 64];
    for i in 0..64 {
        inner[i] ^= k[i];
        outer[i] ^= k[i];
    }
    let mut h = Sha256::new();
    h.update(inner);
    h.update(msg);
    let inner_digest = h.finalize();
    let mut h2 = Sha256::new();
    h2.update(outer);
    h2.update(inner_digest);
    let out = h2.finalize();
    let mut out_arr = [0u8; 32];
    out_arr.copy_from_slice(&out);
    out_arr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let v = TokenVerifier::new([7u8; 32]);
        let t = v.sign("cred-1", [0xAAu8; 32], 9_999_999_999_999);
        let h = t.to_header();
        let t2 = CapabilityToken::from_header(&h).unwrap();
        assert_eq!(t.capability_id, t2.capability_id);
        assert_eq!(t.expires_unix_ms, t2.expires_unix_ms);
        v.verify(&t).unwrap();
        v.verify(&t2).unwrap();
    }

    #[test]
    fn bad_signature_rejected() {
        let v = TokenVerifier::new([7u8; 32]);
        let mut t = v.sign("cred-1", [0xAAu8; 32], 9_999_999_999_999);
        t.signature[0] ^= 0xFF;
        assert!(v.verify(&t).is_err());
    }

    #[test]
    fn header_without_prefix_rejected() {
        let v = TokenVerifier::new([7u8; 32]);
        let t = v.sign("cred-1", [0u8; 32], 0);
        let h = t.to_header();
        let bad = h.replacen("Capability", "Token", 1);
        assert!(CapabilityToken::from_header(&bad).is_err());
    }
}
