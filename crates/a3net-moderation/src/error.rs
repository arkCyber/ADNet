//! Errors raised by the moderation subsystems.

use a3net_error::{ErrorKind, IntoReport, Severity};
use thiserror::Error;

/// Result alias for the moderation crate.
pub type ModerationResult<T> = Result<T, ModerationError>;

/// Errors raised by the moderation subsystems.
#[derive(Debug, Error)]
pub enum ModerationError {
    /// Storage I/O failure (persistence, atomic rename, …).
    #[error("moderation I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization / deserialization failure.
    #[error("moderation serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// The supplied hash could not be parsed as a content hash.
    #[error("invalid content hash: {0}")]
    InvalidHash(String),

    /// The supplied blocklist file is malformed.
    #[error("invalid blocklist format: {0}")]
    InvalidBlocklist(String),

    /// A takedown operation was attempted on a hash that is not
    /// currently pinned (or even present) on the local node.
    #[error("content not pinned locally: {0}")]
    NotPinned(String),

    /// The takedown was rejected by a precondition (e.g. operator
    /// role lacking, evidence missing).
    #[error("takedown precondition failed: {0}")]
    Precondition(String),

    /// Reputation subsystem rejected the bridge call.
    #[error("reputation bridge error: {0}")]
    Reputation(#[from] a3net_reputation::ReputationError),
}

// P0-5: unified error reporting.  Codes `MOD-NNN` for the
// moderation crate.
impl IntoReport for ModerationError {
    fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "MOD-001",
            Self::Serde(_) => "MOD-002",
            Self::InvalidHash(_) => "MOD-003",
            Self::InvalidBlocklist(_) => "MOD-004",
            Self::NotPinned(_) => "MOD-005",
            Self::Precondition(_) => "MOD-006",
            Self::Reputation(_) => "MOD-007",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            // Caller supplied bad input.
            Self::InvalidHash(_) | Self::InvalidBlocklist(_) => ErrorKind::BadRequest,
            // Takedown precondition — caller didn't meet it.
            Self::Precondition(_) => ErrorKind::BadRequest,
            // Resource state issue.
            Self::NotPinned(_) => ErrorKind::NotFound,
            // Storage fault.
            Self::Io(_) => ErrorKind::Unavailable,
            // Bytes-on-disk fault.
            Self::Serde(_) => ErrorKind::DataLoss,
            // Wrapped reputation error — surface the inner kind.
            Self::Reputation(c) => c.kind(),
        }
    }

    fn severity(&self) -> Severity {
        match self {
            // Caller mistakes — warn.
            Self::InvalidHash(_)
            | Self::InvalidBlocklist(_)
            | Self::Precondition(_)
            | Self::NotPinned(_) => Severity::Warn,
            // Storage / corruption — page.
            Self::Io(_) | Self::Serde(_) => Severity::Error,
            // Reputation bridge — pass through.
            Self::Reputation(c) => c.severity(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moderation_error_codes_are_stable() {
        let pairs: Vec<(ModerationError, &str, ErrorKind, Severity)> = vec![
            (
                ModerationError::InvalidHash("x".into()),
                "MOD-003",
                ErrorKind::BadRequest,
                Severity::Warn,
            ),
            (
                ModerationError::InvalidBlocklist("x".into()),
                "MOD-004",
                ErrorKind::BadRequest,
                Severity::Warn,
            ),
            (
                ModerationError::NotPinned("x".into()),
                "MOD-005",
                ErrorKind::NotFound,
                Severity::Warn,
            ),
            (
                ModerationError::Precondition("x".into()),
                "MOD-006",
                ErrorKind::BadRequest,
                Severity::Warn,
            ),
        ];
        for (err, code, kind, sev) in pairs {
            assert_eq!(err.code(), code, "code for {err:?}");
            assert_eq!(err.kind(), kind, "kind for {err:?}");
            assert_eq!(err.severity(), sev, "severity for {err:?}");
        }
    }

    #[test]
    fn moderation_error_into_report_carries_cause() {
        let e = ModerationError::NotPinned("hash-abc".into());
        let report = e.into_report("a3net-moderation");
        assert_eq!(report.code, "MOD-005");
        assert!(report.cause.as_deref().unwrap_or("").contains("hash-abc"));
    }
}
