//! Crate-wide error type.
//!
//! Kept intentionally small and concrete: every variant tells the
//! caller exactly which subsystem failed (SQLite, JSON, lock, ID gen,
//! schema migration, …). Callers that need a single boxed error can
//! map to `anyhow::Error` at the IPC boundary.

use thiserror::Error;

pub type Result<T, E = ChatStoreError> = std::result::Result<T, E>;

/// Errors produced by the chatstore crate.
#[derive(Debug, Error)]
pub enum ChatStoreError {
    /// Underlying SQLite error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// `std::sync::Mutex` poisoned — usually means a previous holder
    /// panicked mid-write. We surface this rather than silently
    /// ignoring it.
    #[error("mutex poisoned")]
    Lock,

    /// JSON (de)serialisation error.
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// Bincode (de)serialisation error (sync compression path).
    #[error("bincode error: {0}")]
    Bincode(#[from] bincode::Error),

    /// zstd compression / decompression error.
    #[error("zstd error: {0}")]
    Zstd(String),

    /// I/O error during filesystem operations (create dir, open file).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A unique-id generator could not produce an id (should be
    /// vanishingly rare — only if `rand` is exhausted).
    #[error("id generation failed: {0}")]
    IdGen(String),

    /// A chat / group invariant was violated (cyclic sequence wrap,
    /// out-of-range sequence, …).
    #[error("invalid chat invariant: {0}")]
    Invalid(String),

    /// A validated record was rejected at the boundary.
    #[error("validation: {0}")]
    Validation(String),

    /// The caller asked for a record that does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A unique-constraint violation (duplicate username, duplicate
    /// `(user_id, friend_id)`, …). Distinct from [`Self::Invalid`]
    /// because callers can choose to ignore / upsert on conflict.
    #[error("constraint violation: {0}")]
    Constraint(String),

    /// A foreign-key constraint failed (e.g. receipt for a message
    /// id that doesn't exist).
    #[error("foreign key violation: {0}")]
    ForeignKey(String),

    /// The stored schema version is older than what this build of
    /// `adnet-chatstore` understands and the caller has not opted
    /// into auto-migration.
    #[error("schema version mismatch: stored={stored}, supported={supported}")]
    SchemaVersion { stored: u32, supported: u32 },

    /// SQLite `PRAGMA integrity_check` returned non-`ok`. The
    /// database may be corrupt — caller should refuse to operate
    /// on it.
    #[error("database integrity check failed: {0}")]
    DatabaseCorrupt(String),
}

impl<T> From<std::sync::PoisonError<T>> for ChatStoreError {
    fn from(_: std::sync::PoisonError<T>) -> Self {
        ChatStoreError::Lock
    }
}

impl<T> From<std::sync::TryLockError<T>> for ChatStoreError {
    fn from(_: std::sync::TryLockError<T>) -> Self {
        ChatStoreError::Lock
    }
}

impl From<tokio::sync::TryLockError> for ChatStoreError {
    fn from(_: tokio::sync::TryLockError) -> Self {
        ChatStoreError::Lock
    }
}

impl From<tokio::task::JoinError> for ChatStoreError {
    fn from(_: tokio::task::JoinError) -> Self {
        ChatStoreError::Lock
    }
}

impl From<std::num::ParseIntError> for ChatStoreError {
    fn from(e: std::num::ParseIntError) -> Self {
        ChatStoreError::Invalid(format!("parse int: {e}"))
    }
}

impl From<chrono::ParseError> for ChatStoreError {
    fn from(e: chrono::ParseError) -> Self {
        ChatStoreError::Invalid(format!("parse chrono: {e}"))
    }
}

/// `adnet_types` uses its own `AdnetError` enum (validation, ticket,
/// ...). Allow `?` to bridge into [`ChatStoreError`] when a typed
/// record fails validation at the storage boundary.
impl From<adnet_types::error::AdnetError> for ChatStoreError {
    fn from(e: adnet_types::error::AdnetError) -> Self {
        ChatStoreError::Validation(e.to_string())
    }
}
