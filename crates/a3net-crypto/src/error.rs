//! Unified error type for every cryptographic operation in A3Net.
//!
//! Every public API in `a3net-crypto` returns [`CryptoResult<T>`],
//! which is `Result<T, CryptoError>`. The variants intentionally
//! cover the union of failure modes that used to live in
//! `a3net-blobstore::EncryptionError`, `a3net-security::SecurityError`,
//! and `a3net-backup::EncryptionError` — so callers can `?` across
//! crate boundaries without translating.

use std::path::PathBuf;

/// All crypto-layer failures.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// AEAD encryption call failed (very rare — typically OOM).
    #[error("AEAD encryption failed")]
    AeadEncrypt,
    /// AEAD decryption failed: wrong key, corrupted ciphertext, or
    /// truncated input. We do **not** distinguish those cases because
    /// doing so leaks information to an attacker.
    #[error("AEAD decryption failed (wrong key or corrupted ciphertext)")]
    AeadDecrypt,
    /// Ciphertext shorter than the AEAD header (24-byte nonce + 16-byte tag).
    #[error("ciphertext too short: {got} bytes (need at least {need})")]
    CiphertextTooShort { got: usize, need: usize },
    /// Supplied raw bytes were not the expected 32-byte key length.
    #[error("invalid key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    /// Argon2id was given an empty / too-short salt.
    #[error("salt must be at least 8 bytes")]
    InvalidSalt,
    /// Argon2id itself failed (parameters invalid, OOM, etc.).
    #[error("key derivation failed: {0}")]
    Kdf(String),
    /// Malformed JSON / unexpected version on a `KeyFile`.
    #[error("invalid key file metadata: {0}")]
    InvalidKeyFile(String),
    /// Hex decoding of a key file payload failed.
    #[error("invalid hex in key file: {0}")]
    InvalidHex(String),
    /// Passphrase-derived key file was loaded without a passphrase.
    #[error("key file is passphrase-derived; supply the passphrase")]
    PassphraseRequired,
    /// Key file was not present at the expected path.
    #[error("key file not found at {0}")]
    KeyFileMissing(PathBuf),
    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Internal invariant violation — should never happen. Indicates
    /// a bug in the calling code rather than in the crypto primitives.
    #[error("internal error: {0}")]
    Internal(String),
}

pub type CryptoResult<T> = Result<T, CryptoError>;
