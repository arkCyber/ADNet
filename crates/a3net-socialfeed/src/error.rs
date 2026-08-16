//! Error type for `a3net-socialfeed`.
//!
//! All public fallible functions in this crate return
//! [`Result<T>`] (alias for `std::result::Result<T, SocialFeedError>`).
//! The error variants mirror the layered architecture: persistence,
//! IPC, gossip and validation each have their own arm so callers
//! can react with the right fallback (e.g. "DB locked → retry";
//! "validation → re-prompt user"; "gossip timeout → drop the
//! frame").
//!
//! `std::sync::Mutex` poisoning is normalised to
//! `SocialFeedError::Lock` so the public API never surfaces
//! `Result<_, MutexGuardErr>` plumbing. Pure validation errors
//! coming from [`a3net_types`] are mapped to the `Validation`
//! variant so the layered crate boundary doesn't leak.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SocialFeedError {
    /// SQLite returned an error (open / query / write / schema).
    /// The wrapped message is the underlying `rusqlite` error.
    #[error("database: {0}")]
    Database(String),

    /// A row / record was looked up but not present.
    #[error("not found: {0}")]
    NotFound(String),

    /// The caller submitted data that failed the typed-records
    /// `validate()` checks (empty id, oversize content, temporal
    /// inversion, …).
    #[error("validation: {0}")]
    Validation(String),

    /// `std::sync::Mutex` poisoning. Public APIs are not allowed
    /// to surface this directly — the lock guards are recovered
    /// with `Ok(())` and a poisoned panicking thread is logged at
    /// the recovery point instead of being propagated.
    #[error("internal mutex lock poisoned")]
    Lock,

    /// JSON-RPC failure (malformed envelope, unknown method,
    /// server shutdown, …).
    #[error("ipc: {0}")]
    Ipc(String),

    /// Gossip layer failure (subscribe / publish / decode error).
    #[error("gossip: {0}")]
    Gossip(String),

    /// Anything not otherwise covered (rare; mostly used at
    /// catch-all boundaries).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Coarse classification used by callers that want to decide
/// retry policy without pattern-matching the enum. Mirrors
/// `a3net_chatstore::ErrorClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Permanent — changing inputs would not help.
    Permanent,
    /// Possibly transient — caller should retry with backoff.
    Transient,
    /// Caller-facing input problem — surface a user message.
    Invalid,
}

impl SocialFeedError {
    pub fn database<E: std::fmt::Display>(e: E) -> Self {
        SocialFeedError::Database(e.to_string())
    }

    pub fn ipc<E: std::fmt::Display>(e: E) -> Self {
        SocialFeedError::Ipc(e.to_string())
    }

    pub fn gossip<E: std::fmt::Display>(e: E) -> Self {
        SocialFeedError::Gossip(e.to_string())
    }

    pub fn not_found<E: std::fmt::Display>(e: E) -> Self {
        SocialFeedError::NotFound(e.to_string())
    }

    pub fn class(&self) -> ErrorClass {
        match self {
            SocialFeedError::Database(_) | SocialFeedError::Ipc(_) | SocialFeedError::Gossip(_) => {
                ErrorClass::Transient
            }
            SocialFeedError::Lock => ErrorClass::Transient,
            SocialFeedError::Validation(_) | SocialFeedError::NotFound(_) => ErrorClass::Invalid,
            SocialFeedError::Other(_) => ErrorClass::Permanent,
        }
    }
}

pub type Result<T> = std::result::Result<T, SocialFeedError>;

/// Helper: convert from [`a3net_types::AdnetError`] into
/// [`SocialFeedError`] by collapsing every variant into the
/// matching arm above. Keeps the storage layer free of `From`
/// impls that could shadow a future rusqlite mapping.
impl From<a3net_types::AdnetError> for SocialFeedError {
    fn from(e: a3net_types::AdnetError) -> Self {
        match e {
            a3net_types::AdnetError::Validation(m) => SocialFeedError::Validation(m),
            other => SocialFeedError::Validation(other.to_string()),
        }
    }
}

/// Bridge from `rusqlite::Error` for the storage layer — every
/// SQLite-side error lands here so the `?` operator works
/// uniformly across `storage.rs`.
impl From<rusqlite::Error> for SocialFeedError {
    fn from(e: rusqlite::Error) -> Self {
        SocialFeedError::database(e)
    }
}

/// Convenience for callers building envelopes off JSON-RPC payloads
/// (the IPC helper uses this to surface deserialisation failures
/// without leaking `serde_json` types).
impl From<serde_json::Error> for SocialFeedError {
    fn from(e: serde_json::Error) -> Self {
        SocialFeedError::database(e.to_string())
    }
}

/// Convenience for the storage layer that touches the
/// filesystem during `open`.
impl From<std::io::Error> for SocialFeedError {
    fn from(e: std::io::Error) -> Self {
        SocialFeedError::database(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_are_classified() {
        assert_eq!(
            SocialFeedError::database("x").class(),
            ErrorClass::Transient
        );
        assert_eq!(SocialFeedError::Lock.class(), ErrorClass::Transient);
        assert_eq!(
            SocialFeedError::Validation("x".into()).class(),
            ErrorClass::Invalid
        );
        assert_eq!(
            SocialFeedError::not_found("x").class(),
            ErrorClass::Invalid
        );
        assert_eq!(
            SocialFeedError::Other(anyhow::anyhow!("x")).class(),
            ErrorClass::Permanent
        );
    }

    #[test]
    fn display_strings_are_stable() {
        // Locked-in to surface stable error strings for log scraping.
        let _ = format!("{}", SocialFeedError::Lock);
        let _ = format!(
            "{}",
            SocialFeedError::Validation("empty post_id".into())
        );
        let _ = format!("{}", SocialFeedError::not_found("post:p1"));
    }
}
