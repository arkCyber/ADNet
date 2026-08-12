//! Crate-wide error type.

use thiserror::Error;

use crate::store::BulletinStoreError;

/// Result alias for the `adnet-news` crate.
pub type NewsResult<T> = std::result::Result<T, NewsError>;

/// Errors produced by the news service. Mirrors the
/// `recoverability`-friendly pattern used by
/// [`crate::store::BulletinStoreError`] so callers can decide whether
/// to retry, surface to the user, or refuse to continue.
#[derive(Debug, Error)]
pub enum NewsError {
    /// Persistence layer error (SQLite, IO, schema mismatch). Always
    /// `Fatal` — retry will not help.
    #[error("news store error: {0}")]
    Store(#[from] BulletinStoreError),

    /// Validation failure on a publish / ingest path.
    #[error("news validation: {0}")]
    Validation(String),

    /// Network / gossip failure.
    #[error("gossip error: {0}")]
    Gossip(String),

    /// Serialisation failure (JSON / envelope codec).
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Caller asked for something the service has not been
    /// configured for (e.g. an empty room id).
    #[error("news config: {0}")]
    Config(String),

    /// The bulletin already exists or its sequence regressed.
    #[error("ordering violation: {0}")]
    Ordering(String),

    /// Catch-all for unexpected internal failures.
    #[error("news internal: {0}")]
    Internal(String),
}

impl NewsError {
    /// Coarse recoverability classification. Mirrors
    /// `ChatStoreError::recoverability` for ergonomic upstream
    /// handling.
    pub fn recoverability(&self) -> NewsRecoverability {
        match self {
            Self::Store(_) | Self::Internal(_) | Self::Serde(_) => NewsRecoverability::Fatal,
            Self::Gossip(_) | Self::Ordering(_) => NewsRecoverability::Recoverable,
            Self::Validation(_) | Self::Config(_) => NewsRecoverability::UserError,
        }
    }
}

/// Coarse classification used by callers that need a quick retry /
/// surface / abort decision without string-matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsRecoverability {
    /// Transient or contention-induced failure; safe to retry.
    Recoverable,
    /// Caller passed invalid input; do not retry.
    UserError,
    /// Internal invariant broken or unrecoverable backend failure;
    /// do not retry.
    Fatal,
}

impl From<anyhow::Error> for NewsError {
    fn from(e: anyhow::Error) -> Self {
        Self::Gossip(e.to_string())
    }
}