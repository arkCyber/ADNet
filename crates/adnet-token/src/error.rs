//! Cross-cutting error type for `adnet-token`.

use thiserror::Error;

pub use adnet_identity::IdentityError;

pub type Result<T> = std::result::Result<T, TokenError>;

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("identity error: {0}")]
    Identity(#[from] IdentityError),

    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("invalid amount: {0}")]
    InvalidAmount(String),

    #[error("invalid expiry: {0}")]
    InvalidExpiry(String),

    #[error("invalid nonce: {0}")]
    InvalidNonce(String),

    #[error("invalid EVM address: {0}")]
    InvalidAddress(String),

    #[error("chain_id mismatch: expected {expected}, got {got}")]
    ChainIdMismatch { expected: u64, got: u64 },

    #[error("expiry in the past: now={now}, pledge={pledge}")]
    Expired { now: i64, pledge: i64 },

    #[error("signature does not recover to the pledgor address")]
    RecoveredWrongSigner,

    #[error("hex decode error: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}
