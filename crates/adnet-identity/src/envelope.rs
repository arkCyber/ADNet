//! Application-layer sealed envelope — a structured framing on top of
//! [`EciesCiphertext`] that makes ADNet's encrypted messages
//! self-describing and versioned.
//!
//! Layout (all big-endian, all length-prefixed where needed):
//!
//! ```text
//! ┌──────────┬──────────┬────────────┬─────────────┬──────────────┬─────────────┐
//! | magic[4] | ver: u8  | recip: 32  | ephemeral:32| nonce: 12    | ct+tag: len |
//! └──────────┴──────────┴────────────┴─────────────┴──────────────┴─────────────┘
//! ```
//!
//! - `magic` = `b"ADEN"` — lets log scanners and protocol police spot ADNet
//!   envelopes even when they look like random bytes in a larger stream.
//! - `ver` = 1 today. Bumping it is a breaking change to the on-wire format.
//! - `recip` is the recipient's *static* X25519 public key. We include it so
//!   a relay can route the envelope to the right worker without starting
//!   decryption itself.
//! - `ephemeral`, `nonce`, `ct` are the standard ECIES fields.
//!
//! The envelope is *not* authenticated as a whole. The AES-GCM tag inside
//! `ct` is the only authentication. Recipients must verify the tag before
//! trusting the plaintext.

use serde::{Deserialize, Serialize};

use crate::ecies::{self, EciesCiphertext, EciesPublicKey, EciesSecretKey};
use crate::error::{IdentityError, Result};
use crate::x25519::X25519PublicKey;

/// 4-byte magic at the head of every envelope.
pub const ADR_ENVELOPE_MAGIC: [u8; 4] = *b"ADEN";

/// Current envelope wire version. Bump on breaking layout changes.
pub const ADR_ENVELOPE_VERSION: u8 = 1;

/// Plain payload, opaque to ADNet. ADNet does not inspect this; it can be
/// any JSON, postcard, or raw bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload(pub Vec<u8>);

impl EncryptedPayload {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl From<Vec<u8>> for EncryptedPayload {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl From<&[u8]> for EncryptedPayload {
    fn from(v: &[u8]) -> Self {
        Self(v.to_vec())
    }
}

impl AsRef<[u8]> for EncryptedPayload {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Self-describing ECIES envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedEnvelope {
    pub version: u8,
    /// Recipient's static X25519 public key — used for routing before
    /// decryption.
    pub recipient: X25519PublicKey,
    #[serde(flatten)]
    pub inner: EciesCiphertext,
}

impl SealedEnvelope {
    /// Encrypt `payload` for `recipient`. The returned envelope is ready to
    /// ship over gossip / QUIC / etc.
    pub fn seal(recipient: &EciesPublicKey, payload: EncryptedPayload) -> Result<Self> {
        let inner = ecies::encrypt(recipient, payload.as_bytes())?;
        Ok(Self {
            version: ADR_ENVELOPE_VERSION,
            recipient: *recipient,
            inner,
        })
    }

    /// Inverse of [`Self::seal`]. Returns the original plaintext.
    pub fn open(self, recipient: &EciesSecretKey) -> Result<EncryptedPayload> {
        if self.version != ADR_ENVELOPE_VERSION {
            return Err(IdentityError::EnvelopeVersionMismatch {
                expected: ADR_ENVELOPE_VERSION,
                got: self.version,
            });
        }
        let plaintext = ecies::decrypt(recipient, &self.inner)?;
        Ok(EncryptedPayload(plaintext))
    }

    /// Serialize to the canonical wire format (magic + version + fields).
    ///
    /// We do *not* use `serde_json` here because envelopes may be large
    /// (think: a whole `adnet-blob://` ticket) and the binary form is
    /// 4-5× smaller. The format is versioned via the leading byte.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 1 + 32 + 32 + 12 + self.inner.ciphertext.len());
        out.extend_from_slice(&ADR_ENVELOPE_MAGIC);
        out.push(self.version);
        out.extend_from_slice(&self.recipient.to_bytes());
        out.extend_from_slice(&self.inner.ephemeral_pub);
        out.extend_from_slice(&self.inner.nonce);
        out.extend_from_slice(&self.inner.ciphertext);
        out
    }

    /// Parse a wire-format envelope.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        const HEADER: usize = 4 + 1 + 32 + 32 + 12;
        if bytes.len() < HEADER {
            return Err(IdentityError::EnvelopeTooShort {
                need: HEADER,
                got: bytes.len(),
            });
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[..4]);
        if magic != ADR_ENVELOPE_MAGIC {
            return Err(IdentityError::EnvelopeMagicMismatch {
                expected: ADR_ENVELOPE_MAGIC,
                got: magic,
            });
        }
        let version = bytes[4];
        let recipient = X25519PublicKey::from_bytes(&bytes[5..5 + 32])?;
        let ephemeral_pub: [u8; 32] = bytes[5 + 32..5 + 64].try_into().unwrap();
        let nonce: [u8; 12] = bytes[5 + 64..5 + 76].try_into().unwrap();
        let ciphertext = bytes[5 + 76..].to_vec();
        Ok(Self {
            version,
            recipient,
            inner: EciesCiphertext {
                ephemeral_pub,
                nonce,
                ciphertext,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let recipient = EciesSecretKey::generate();
        let payload = EncryptedPayload::from(b"hello sealed world".to_vec());
        let env = SealedEnvelope::seal(&recipient.public_key(), payload.clone()).unwrap();
        let decoded = env.open(&recipient).unwrap();
        assert_eq!(decoded.0, payload.0);
    }

    #[test]
    fn wire_round_trip() {
        let recipient = EciesSecretKey::generate();
        let payload = EncryptedPayload::from(b"binary \x00\xff ok".to_vec());
        let env = SealedEnvelope::seal(&recipient.public_key(), payload.clone()).unwrap();
        let wire = env.encode();
        let back = SealedEnvelope::decode(&wire).unwrap();
        let out = back.open(&recipient).unwrap();
        assert_eq!(out.0, payload.0);
    }

    #[test]
    fn encodes_magic() {
        let recipient = EciesSecretKey::generate();
        let env =
            SealedEnvelope::seal(&recipient.public_key(), EncryptedPayload(vec![1, 2, 3])).unwrap();
        let wire = env.encode();
        assert_eq!(&wire[..4], &ADR_ENVELOPE_MAGIC);
        assert_eq!(wire[4], ADR_ENVELOPE_VERSION);
    }

    #[test]
    fn rejects_truncated() {
        let err = SealedEnvelope::decode(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, IdentityError::EnvelopeTooShort { .. }));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = vec![0u8; 100];
        bytes[..4].copy_from_slice(b"ZZZZ");
        let err = SealedEnvelope::decode(&bytes).unwrap_err();
        assert!(matches!(err, IdentityError::EnvelopeMagicMismatch { .. }));
    }
}
