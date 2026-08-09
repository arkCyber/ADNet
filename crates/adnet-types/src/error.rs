//! Cross-crate error type. Keep this small — most IO / network errors should
//! use [`anyhow::Error`] at the boundary and `AdnetError` for protocol-level
//! invariants (malformed ticket, unknown kind, ...).

use thiserror::Error;

pub type Result<T, E = AdnetError> = std::result::Result<T, E>;

/// ADNet protocol-level error.
#[derive(Debug, Error)]
pub enum AdnetError {
    #[error("invalid ticket: {0}")]
    InvalidTicket(String),

    #[error("invalid content hash: {0}")]
    InvalidContentHash(String),

    #[error("invalid node id: {0}")]
    InvalidNodeId(String),

    #[error("unknown content kind: {0}")]
    UnknownContentKind(String),

    #[error("validation: {0}")]
    Validation(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
