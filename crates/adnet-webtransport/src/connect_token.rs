//! HMAC-signed connect-tokens used to gate WebTransport sessions.
//!
//! When a browser wants to open a WebTransport connection to an ADNet
//! node, the node first issues a one-shot token. The browser includes the
//! token as an HTTP header (`Authorization: Bearer <token>`) on the
//! initial WebTransport handshake. The server verifies the HMAC and the
//! timestamp, then accepts or rejects the connection.
//!
//! ## Token format
//!
//! ```text
//!   base64url(payload_json || 0x00 || base64url(hmac_sha256(secret, payload_json)))
//! ```
//!
//! `payload_json` is a small JSON document:
//! ```json
//! { "node_id": "<hex>", "issued_at": <unix_seconds>, "ttl_seconds": 60 }
//! ```
//!
//! This format is human-inspectable (you can decode the base64 in a
//! browser console to see what permissions are being granted) and stays
//! under 256 bytes including the HMAC.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

use adnet_types::NodeId;

type HmacSha256 = Hmac<Sha256>;

const TOKEN_PREFIX: &str = "adnet-wt-v1:";

/// Token-format error.
#[derive(Debug, Error)]
pub enum ConnectTokenError {
    #[error("malformed token: {0}")]
    Malformed(String),
    #[error("signature mismatch")]
    BadSignature,
    #[error("token expired")]
    Expired,
    #[error("hmac: {0}")]
    Hmac(String),
    #[error("base64: {0}")]
    Base64(String),
}

/// A connect-token claim, before signing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenClaim {
    pub node_id: NodeId,
    pub issued_at: u64,
    pub ttl_seconds: u64,
}

impl TokenClaim {
    pub fn new(node_id: NodeId, ttl_seconds: u64) -> Self {
        let issued_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            node_id,
            issued_at,
            ttl_seconds,
        }
    }

    pub fn is_expired(&self, now: u64) -> bool {
        now.saturating_sub(self.issued_at) > self.ttl_seconds
    }
}

/// Connect-token: the claim + HMAC tag, base64-encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectToken(String);

impl ConnectToken {
    /// Construct a token from a wire-format string (e.g. received from an
    /// HTTP `Authorization: Bearer` header).
    pub fn from_string(s: impl Into<String>) -> Result<Self, ConnectTokenError> {
        Ok(Self(s.into()))
    }

    /// Sign a claim with the given HMAC secret and return the wire-format
    /// token (prefix + base64(payload) + 0x00 + base64(hmac)).
    pub fn sign(claim: &TokenClaim, secret: &[u8]) -> crate::WebTransportResult<Self> {
        let payload = serde_json::to_vec(claim)
            .map_err(|e| crate::WebTransportError::Token(ConnectTokenError::Malformed(format!("encode claim: {e}"))))?;
        let mut mac = <HmacSha256 as Mac>::new_from_slice(secret)
            .map_err(|e| ConnectTokenError::Hmac(e.to_string()))?;
        mac.update(&payload);
        let tag = mac.finalize().into_bytes();
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload);
        let tag_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag);
        Ok(Self(format!("{TOKEN_PREFIX}{payload_b64}\x00{tag_b64}")))
    }

    /// Verify a token against the secret. Returns the claim on success.
    pub fn verify(&self, secret: &[u8], now: u64) -> Result<TokenClaim, ConnectTokenError> {
        let body = self
            .0
            .strip_prefix(TOKEN_PREFIX)
            .ok_or_else(|| ConnectTokenError::Malformed("missing prefix".into()))?;
        let mut parts = body.split('\x00');
        let payload_b64 = parts
            .next()
            .ok_or_else(|| ConnectTokenError::Malformed("missing payload".into()))?;
        let tag_b64 = parts
            .next()
            .ok_or_else(|| ConnectTokenError::Malformed("missing tag".into()))?;
        if parts.next().is_some() {
            return Err(ConnectTokenError::Malformed("extra bytes".into()));
        }

        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|e| ConnectTokenError::Base64(e.to_string()))?;
        let tag = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(tag_b64)
            .map_err(|e| ConnectTokenError::Base64(e.to_string()))?;

        let mut mac = <HmacSha256 as Mac>::new_from_slice(secret)
            .map_err(|e| ConnectTokenError::Hmac(e.to_string()))?;
        mac.update(&payload);
        mac.verify_slice(&tag)
            .map_err(|_| ConnectTokenError::BadSignature)?;

        let claim: TokenClaim = serde_json::from_slice(&payload)
            .map_err(|e| ConnectTokenError::Malformed(format!("decode claim: {e}")))?;
        if claim.is_expired(now) {
            return Err(ConnectTokenError::Expired);
        }
        Ok(claim)
    }

    /// Inspect the wire-format string (for logging only — does not verify).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_node_id() -> NodeId {
        NodeId::random()
    }

    #[test]
    fn roundtrip() {
        let secret = b"super-secret";
        let claim = TokenClaim::new(fresh_node_id(), 60);
        let token = ConnectToken::sign(&claim, secret).unwrap();
        let recovered = token.verify(secret, claim.issued_at + 1).unwrap();
        assert_eq!(recovered, claim);
    }

    #[test]
    fn wrong_secret_rejected() {
        let claim = TokenClaim::new(fresh_node_id(), 60);
        let token = ConnectToken::sign(&claim, b"a").unwrap();
        assert!(matches!(
            token.verify(b"b", claim.issued_at + 1),
            Err(ConnectTokenError::BadSignature)
        ));
    }

    #[test]
    fn expired_rejected() {
        let secret = b"x";
        let claim = TokenClaim::new(fresh_node_id(), 5);
        let token = ConnectToken::sign(&claim, secret).unwrap();
        let now = claim.issued_at + 10;
        assert!(matches!(
            token.verify(secret, now),
            Err(ConnectTokenError::Expired)
        ));
    }

    #[test]
    fn tampered_payload_rejected() {
        let secret = b"x";
        let claim = TokenClaim::new(fresh_node_id(), 60);
        let token = ConnectToken::sign(&claim, secret).unwrap();
        // Flip a byte in the payload.
        let mut bytes = token.0.into_bytes();
        bytes[20] ^= 0x01;
        let tampered = ConnectToken(String::from_utf8(bytes).unwrap());
        assert!(tampered.verify(secret, claim.issued_at + 1).is_err());
    }

    #[test]
    fn missing_prefix_rejected() {
        let claim = TokenClaim::new(fresh_node_id(), 60);
        let token = ConnectToken::sign(&claim, b"x").unwrap();
        let no_prefix = ConnectToken(token.0.replacen(TOKEN_PREFIX, "", 1));
        assert!(matches!(
            no_prefix.verify(b"x", claim.issued_at + 1),
            Err(ConnectTokenError::Malformed(_))
        ));
    }
}
