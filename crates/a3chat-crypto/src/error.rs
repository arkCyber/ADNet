//! Crypto-specific error type. Bridges into `a3chat_core::A3chatError`
//! via `From` so `?`-conversion is ergonomic.

use thiserror::Error;

use crate::sender_keys::SenderKeyId;

/// Result alias used across `a3chat-crypto`.
pub type CryptoResult<T> = std::result::Result<T, CryptoError>;

/// Every crypto failure funnels through this enum.
#[derive(Debug, Error)]
pub enum CryptoError {
    /// AEAD tag mismatch — ciphertext was tampered with or the wrong
    /// key was used.
    #[error("AEAD tag mismatch (tampering or wrong key)")]
    AeadTagMismatch,

    /// Noise_XX handshake aborted or produced an invalid state.
    #[error("noise handshake error: {0}")]
    NoiseHandshake(String),

    /// HKDF / KDF output too short (impossible with the constants we
    /// use, but caught as a defensive guard).
    #[error("KDF output too short: requested {requested} bytes, got {actual}")]
    KdfOutputTooShort { requested: usize, actual: usize },

    /// Argon2id KDF failure (e.g. invalid parameters).
    #[error("Argon2id failure: {0}")]
    Argon2(String),

    /// Ciphertext / nonce / tag length mismatch.
    #[error("invalid length: {field} expected {expected} bytes, got {actual}")]
    InvalidLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },

    /// Sender Key referenced a `SenderKeyId` we have no record of.
    #[error("unknown sender key id: {0:?}")]
    UnknownSenderKey(SenderKeyId),

    /// Sender Key chain advanced past its iteration ceiling
    /// (should be impossible; indicates a bug or replay).
    #[error("sender key chain exhausted (id={id:?}, iteration={iteration})")]
    SenderKeyExhausted { id: SenderKeyId, iteration: u32 },

    /// Base64 decoding of an AEAD field failed.
    #[error("base64 decode failed: {0}")]
    Base64Decode(String),

    /// Hex decoding of a nonce / tag failed.
    #[error("hex decode failed: {0}")]
    HexDecode(String),

    /// Generic catch-all (rare — prefer a dedicated variant).
    #[error("crypto internal error: {0}")]
    Internal(String),
}

impl From<CryptoError> for a3chat_core::A3chatError {
    fn from(e: CryptoError) -> Self {
        a3chat_core::A3chatError::CryptoError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridges_into_a3chat_error() {
        let ce = CryptoError::AeadTagMismatch;
        let ae: a3chat_core::A3chatError = ce.into();
        match ae {
            a3chat_core::A3chatError::CryptoError(msg) => {
                assert!(msg.contains("tag mismatch"));
            }
            other => panic!("expected CryptoError, got {other:?}"),
        }
    }

    #[test]
    fn invalid_length_error_displays_context() {
        let e = CryptoError::InvalidLength {
            field: "nonce",
            expected: 12,
            actual: 8,
        };
        let s = e.to_string();
        assert!(s.contains("nonce"));
        assert!(s.contains("12"));
        assert!(s.contains("8"));
    }

    #[test]
    fn unknown_sender_key_preserves_id() {
        let id = SenderKeyId([1u8; 16]);
        let e = CryptoError::UnknownSenderKey(id);
        assert!(e.to_string().contains("unknown sender key"));
    }

    #[test]
    fn sender_key_exhausted_displays_iteration() {
        let e = CryptoError::SenderKeyExhausted {
            id: SenderKeyId([0u8; 16]),
            iteration: 4_294_967_295,
        };
        let s = e.to_string();
        assert!(s.contains("4294967295"));
    }
}
