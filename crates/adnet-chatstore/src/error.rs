//! Crate-wide error type.
//!
//! Kept intentionally small and concrete: every variant tells the
//! caller exactly which subsystem failed (SQLite, JSON, lock, ID gen,
//! schema migration, …). Callers that need a single boxed error can
//! map to `anyhow::Error` at the IPC boundary.
//!
//! # Recoverability classification (DO-178C)
//!
//! Every public API in this crate can fail with one of these
//! [`ChatStoreError`] variants. To make the "should I retry?" question
//! answerable without string-matching, [`ChatStoreError::recoverability`]
//! returns one of [`ErrorClass`]:
//!
//! | Class        | Variants                                                                                                  | Caller behaviour                              |
//! |--------------|-----------------------------------------------------------------------------------------------------------|-----------------------------------------------|
//! | `UserError`  | `Validation`, `Invalid`, `Constraint`, `ForeignKey`                                                       | Surface to the user; do not retry.            |
//! | `Recoverable`| `NotFound`                                                                                                | Retry once the missing resource may exist.   |
//! | `Fatal`      | `Sqlite`, `Lock`, `Json`, `Bincode`, `Zstd`, `Io`, `IdGen`, `SchemaVersion`, `DatabaseCorrupt`             | Refuse to continue; require operator action. |
//!
//! Note that [`crate::docs_bridge::DocsBridgeError`] carries its own
//! `recoverability` helper for the iroh-docs side of the crate.

use thiserror::Error;

pub type Result<T, E = ChatStoreError> = std::result::Result<T, E>;

/// Coarse classification of an error variant for upstream handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Transient or contention-induced failure; safe to retry.
    Recoverable,
    /// Caller passed invalid input; do not retry.
    UserError,
    /// Internal invariant broken or unrecoverable backend failure;
    /// do not retry.
    Fatal,
}

/// Errors produced by the chatstore crate.
#[derive(Debug, Error)]
pub enum ChatStoreError {
    /// Underlying SQLite error.
    ///
    /// **Recoverability:** `Fatal` — the underlying error is opaque
    /// to the caller and may indicate disk failure or a corrupted
    /// page; retrying will not help. The original `rusqlite::Error`
    /// is preserved for diagnostics.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// `std::sync::Mutex` poisoned — usually means a previous holder
    /// panicked mid-write. We surface this rather than silently
    /// ignoring it.
    ///
    /// **Recoverability:** `Fatal` — by definition a thread
    /// panicked while holding the storage lock. Caller must
    /// re-open the database (after triage of the panic) to
    /// guarantee the on-disk state is consistent.
    #[error("mutex poisoned")]
    Lock,

    /// JSON (de)serialisation error.
    ///
    /// **Recoverability:** `Fatal` — either the input is
    /// structurally invalid or the schema drifted. Retry will not
    /// help.
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// Bincode (de)serialisation error (sync compression path).
    ///
    /// **Recoverability:** `Fatal`.
    #[error("bincode error: {0}")]
    Bincode(#[from] bincode::Error),

    /// zstd compression / decompression error.
    ///
    /// **Recoverability:** `Fatal`.
    #[error("zstd error: {0}")]
    Zstd(String),

    /// I/O error during filesystem operations (create dir, open file).
    ///
    /// **Recoverability:** `Fatal` for `storage::open` (the disk is
    /// the problem); *may* be `Recoverable` for transient writes
    /// inside an already-open store, but we conservatively classify
    /// all `Io` as `Fatal` because the storage layer retries via
    /// the caller's higher-level retry policy.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A unique-id generator could not produce an id (should be
    /// vanishingly rare — only if `rand` is exhausted).
    ///
    /// **Recoverability:** `Fatal` — `rand::thread_rng()` is not
    /// expected to fail under any reasonable load; if it does,
    /// something is wrong with the process environment.
    #[error("id generation failed: {0}")]
    IdGen(String),

    /// A chat / group invariant was violated (cyclic sequence wrap,
    /// out-of-range sequence, …).
    ///
    /// **Recoverability:** `UserError` — the caller asked for
    /// something the data model does not permit.
    #[error("invalid chat invariant: {0}")]
    Invalid(String),

    /// A validated record was rejected at the boundary.
    ///
    /// **Recoverability:** `UserError` — the caller passed
    /// malformed input.
    #[error("validation: {0}")]
    Validation(String),

    /// The caller asked for a record that does not exist.
    ///
    /// **Recoverability:** `Recoverable` — the missing record may
    /// appear after a sync round. Callers that surface
    /// `NotFound` as a hard error to the user are still correct;
    /// classification as `Recoverable` only means the *caller may
    /// try again*.
    #[error("not found: {0}")]
    NotFound(String),

    /// A unique-constraint violation (duplicate username, duplicate
    /// `(user_id, friend_id)`, …). Distinct from [`Self::Invalid`]
    /// because callers can choose to ignore / upsert on conflict.
    ///
    /// **Recoverability:** `UserError`.
    #[error("constraint violation: {0}")]
    Constraint(String),

    /// A foreign-key constraint failed (e.g. receipt for a message
    /// id that doesn't exist).
    ///
    /// **Recoverability:** `UserError`.
    #[error("foreign key violation: {0}")]
    ForeignKey(String),

    /// The stored schema version is older than what this build of
    /// `adnet-chatstore` understands and the caller has not opted
    /// into auto-migration.
    ///
    /// **Recoverability:** `Fatal` — the operator must either
    /// upgrade `adnet-chatstore` or run the explicit migration
    /// ladder. Retrying with the same binary will fail identically.
    #[error("schema version mismatch: stored={stored}, supported={supported}")]
    SchemaVersion { stored: u32, supported: u32 },

    /// SQLite `PRAGMA integrity_check` returned non-`ok`. The
    /// database may be corrupt — caller should refuse to operate
    /// on it.
    ///
    /// **Recoverability:** `Fatal` — caller should refuse to
    /// operate on the file until it has been restored from backup.
    #[error("database integrity check failed: {0}")]
    DatabaseCorrupt(String),
}

impl ChatStoreError {
    /// Classify the error for upstream handling. The mapping is
    /// deliberately explicit — DO-178C requires that error
    /// classification never rely on string matching.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use adnet_chatstore::error::{ChatStoreError, ErrorClass};
    ///
    /// match storage.save_direct_message(user, msg) {
    ///     Ok(()) => {},
    ///     Err(e) if e.recoverability() == ErrorClass::UserError => {
    ///         ui.show_error(e);
    ///     }
    ///     Err(e) => log::error!("storage failure: {e:?}; refusing to continue"),
    /// }
    /// ```
    pub fn recoverability(&self) -> ErrorClass {
        match self {
            Self::Validation(_)
            | Self::Invalid(_)
            | Self::Constraint(_)
            | Self::ForeignKey(_) => ErrorClass::UserError,
            Self::NotFound(_) => ErrorClass::Recoverable,
            Self::Sqlite(_)
            | Self::Lock
            | Self::Json(_)
            | Self::Bincode(_)
            | Self::Zstd(_)
            | Self::Io(_)
            | Self::IdGen(_)
            | Self::SchemaVersion { .. }
            | Self::DatabaseCorrupt(_) => ErrorClass::Fatal,
        }
    }
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
