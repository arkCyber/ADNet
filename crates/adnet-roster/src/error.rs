//! Error types for `adnet-roster`.
//!
//! The error codes are aligned with the original
//! `Exodus@src-backup/src-tauri/src/microservice/contact_directory_service.rs`
//! `AerospaceError` enum (CD-001..CD-010) so that any audit log shipped from
//! the legacy codebase can still be parsed.

use thiserror::Error;

/// Roster-layer error.
#[derive(Debug, Error)]
pub enum RosterError {
    /// CD-001 — invalid parameter (e.g. malformed 12-digit id).
    #[error("invalid parameter `{parameter}`: {reason}")]
    InvalidParameter { parameter: String, reason: String },

    /// CD-002 — input failed structured validation (length, charset, …).
    #[error("validation failed on `{field}`: {reason}")]
    Validation { field: String, reason: String },

    /// CD-003 — failed to acquire a `Mutex` lock.
    #[error("lock error: {reason}")]
    Lock { reason: String },

    /// CD-004 — I/O failure (sqlite, fs, socket, …).
    #[error("io error during `{operation}`: {reason}")]
    Io { operation: String, reason: String },

    /// CD-005 — serde_json / bincode failure.
    #[error("serialization error during `{operation}`: {reason}")]
    Serialization { operation: String, reason: String },

    /// CD-006 — would exceed configured resource cap.
    #[error("resource limit exceeded on `{resource}` (limit = {limit})")]
    ResourceLimit { resource: String, limit: usize },

    /// CD-007 — referenced operation / endpoint does not exist.
    #[error("operation not found: {operation}")]
    OperationNotFound { operation: String },

    /// CD-008 — internal state became inconsistent.
    #[error("state inconsistency: {description}")]
    StateInconsistency { description: String },

    /// CD-009 — caller is not allowed to perform the operation.
    #[error("permission denied: {operation}")]
    PermissionDenied { operation: String },

    /// CD-010 — rate-limit tripped for the named operation.
    #[error("rate limit exceeded on `{operation}` (limit = {limit})")]
    RateLimit { operation: String, limit: u32 },

    /// Contact / group / mapping does not exist.
    #[error("not found: {kind} `{id}`")]
    NotFound { kind: &'static str, id: String },

    /// Contact / group / mapping already exists (unique-key collision).
    #[error("already exists: {kind} `{id}`")]
    AlreadyExists { kind: &'static str, id: String },
}

/// Convenience alias.
pub type RosterResult<T> = Result<T, RosterError>;

impl RosterError {
    /// Stable error code mirroring the legacy CD-001..CD-010 taxonomy.
    pub fn code(&self) -> &'static str {
        match self {
            RosterError::InvalidParameter { .. } => "CD-001",
            RosterError::Validation { .. } => "CD-002",
            RosterError::Lock { .. } => "CD-003",
            RosterError::Io { .. } => "CD-004",
            RosterError::Serialization { .. } => "CD-005",
            RosterError::ResourceLimit { .. } => "CD-006",
            RosterError::OperationNotFound { .. } => "CD-007",
            RosterError::StateInconsistency { .. } => "CD-008",
            RosterError::PermissionDenied { .. } => "CD-009",
            RosterError::RateLimit { .. } => "CD-010",
            RosterError::NotFound { .. } => "CD-NF",
            RosterError::AlreadyExists { .. } => "CD-AE",
        }
    }

    /// Recoverable errors (transient lock/IO/rate-limit).
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            RosterError::Lock { .. } | RosterError::Io { .. } | RosterError::RateLimit { .. }
        )
    }
}

impl From<rusqlite::Error> for RosterError {
    fn from(err: rusqlite::Error) -> Self {
        RosterError::Io {
            operation: "sqlite".to_string(),
            reason: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for RosterError {
    fn from(err: serde_json::Error) -> Self {
        RosterError::Serialization {
            operation: "json".to_string(),
            reason: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_legacy_taxonomy() {
        assert_eq!(
            RosterError::InvalidParameter {
                parameter: "x".into(),
                reason: "y".into()
            }
            .code(),
            "CD-001"
        );
        assert_eq!(
            RosterError::ResourceLimit {
                resource: "contacts".into(),
                limit: 100_000
            }
            .code(),
            "CD-006"
        );
        assert_eq!(
            RosterError::NotFound {
                kind: "contact",
                id: "abc".into()
            }
            .code(),
            "CD-NF"
        );
    }
}