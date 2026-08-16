//! Cross-cutting error type for `a3net-identity`.

use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, IdentityError>;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("invalid secp256k1 secret key: {0}")]
    InvalidSecretKey(String),

    #[error("invalid secp256k1 public key: {0}")]
    InvalidPublicKey(String),

    #[error("invalid signature: {0}")]
    InvalidSignature(String),

    #[error("invalid x25519 key: {0}")]
    InvalidX25519Key(String),

    #[error("EVM address must be 20 bytes, got {0}")]
    InvalidAddressLength(usize),

    #[error("hex decode error: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("envelope too short: need at least {need} bytes, got {got}")]
    EnvelopeTooShort { need: usize, got: usize },

    #[error("envelope version mismatch: expected {expected}, got {got}")]
    EnvelopeVersionMismatch { expected: u8, got: u8 },

    #[error("envelope magic mismatch: expected {expected:?}, got {got:?}")]
    EnvelopeMagicMismatch { expected: [u8; 4], got: [u8; 4] },

    #[error("AEAD decryption failed: {0}")]
    Aead(String),

    #[error("HKDF expansion failed: {0}")]
    Hkdf(String),

    #[error("ECDHE failed: {0}")]
    Ecdhe(String),

    #[error("mnemonic error: {0}")]
    Mnemonic(String),

    #[error("HD derivation error: {0}")]
    HdDerivation(String),

    #[error("keystore error: {0}")]
    Keystore(String),

    #[error("invalid kdf parameters: {0}")]
    InvalidKdf(String),

    #[error("wrong keystore passphrase")]
    WrongPassphrase,

    #[error("unsupported keystore version: {0}")]
    UnsupportedKeystoreVersion(u8),

    #[error("EIP-712 error: {0}")]
    Eip712(String),
}
