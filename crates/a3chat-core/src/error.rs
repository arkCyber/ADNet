//! Unified error type for the a3chat stack.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── a3net-error integration ─────────────────────────────────────────────
//
// We opt in to `a3net_error::IntoReport` so every a3chat error can be
// wrapped into a structured `AdnetErrorReport` at any protocol boundary
// (RPC, FFI, CLI, observability). This is the only a3net-* crate
// a3chat-core depends on — it adds zero runtime cost for callers
// that don't use the trait.
use a3net_error::{ErrorKind, IntoReport, Severity};

/// Result alias used across a3chat crates.
pub type A3chatResult<T> = std::result::Result<T, A3chatError>;

/// Every error in the a3chat domain funnels through here so callers can
/// pattern-match on a single enum instead of juggling per-crate error
/// types.
#[derive(Debug, Error)]
pub enum A3chatError {
    /// The requested resource (conversation, contact, message, …) does not
    /// exist on this node.
    #[error("not found: {0}")]
    NotFound(String),

    /// The caller is not allowed to perform the requested operation
    /// (ACL / group membership / blocklist).
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// A field failed validation. The inner string is the field path and
    /// reason (e.g. `"sender_id: empty"`).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Underlying cryptographic operation failed (Noise handshake
    /// aborted, AEAD tag mismatch, …). The string carries the inner
    /// `crypto::CryptoError` Display form.
    #[error("crypto error: {0}")]
    CryptoError(String),

    /// SQLite / persistence layer failure from `a3net_chatstore`.
    #[error("storage error: {0}")]
    StorageError(String),

    /// RpcClient transport / serialization failure.
    #[error("rpc error: {0}")]
    RpcError(String),

    /// P2P delivery failure (peer unreachable, transport timeout).
    #[error("network error: {0}")]
    NetworkError(String),

    /// Catch-all for unexpected internal failures. Should be rare; if
    /// you reach for this in normal logic, add a dedicated variant.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Top-level classification used to decide recovery strategy.
/// DO-178C §6.3 — *fail-safe*: a CLI / IPC client can ask "can I
/// retry this?" / "do I need to abort?" in O(1) by mapping on this
/// enum rather than reading the human message string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Caller error — do not retry, surface to the user. Examples:
    /// `NotFound`, `InvalidInput`, `PermissionDenied`.
    Permanent,
    /// Transient failure — safe to retry with backoff. Examples:
    /// `NetworkError`, `RpcError`.
    Transient,
    /// Internal failure — unexpected, possibly a bug. Page the
    /// operator. `Internal` falls here so automated monitors can
    /// alert on `error_class == Internal`.
    Internal,
    /// Data integrity / cryptographic breach. Stop everything until
    /// the operator acks — `CryptoError` falls here.
    Security,
}

impl A3chatError {
    /// Top-level classification used by clients to decide recovery
    /// strategy without parsing the error message string.
    pub fn error_class(&self) -> ErrorClass {
        match self {
            A3chatError::NotFound(_)
            | A3chatError::InvalidInput(_)
            | A3chatError::PermissionDenied(_) => ErrorClass::Permanent,
            A3chatError::NetworkError(_) | A3chatError::RpcError(_) => ErrorClass::Transient,
            A3chatError::CryptoError(_) => ErrorClass::Security,
            A3chatError::StorageError(_) | A3chatError::Internal(_) => ErrorClass::Internal,
        }
    }

    /// Convenience: `true` if the error is safe to retry blindly
    /// (i.e. the failure is on the transport side, not on the
    /// caller's data). Used by `a3chat-rpc::client` retry loops.
    pub fn is_retryable(&self) -> bool {
        matches!(self.error_class(), ErrorClass::Transient)
    }
}

impl A3chatError {
    /// Stable string code used over the wire (JSON-RPC `error.code`
    /// mapping table). Matches the [`A3chatErrorCode`] enum.
    pub fn code(&self) -> i32 {
        A3chatErrorCode::from(self).code()
    }

    /// Short stable string identifier. Frontends match on this rather
    /// than on the human message.
    pub fn kind(&self) -> &'static str {
        match self {
            A3chatError::NotFound(_) => "not_found",
            A3chatError::PermissionDenied(_) => "permission_denied",
            A3chatError::InvalidInput(_) => "invalid_input",
            A3chatError::CryptoError(_) => "crypto_error",
            A3chatError::StorageError(_) => "storage_error",
            A3chatError::RpcError(_) => "rpc_error",
            A3chatError::NetworkError(_) => "network_error",
            A3chatError::Internal(_) => "internal",
        }
    }
}

/// Stable numeric error codes shared with all a3chat clients. JSON-RPC
/// `error.code` for application errors falls in the `-32099..-32000`
/// range reserved by the spec; we use `-32100..-32107` for a3chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum A3chatErrorCode {
    NotFound = -32100,
    PermissionDenied = -32101,
    InvalidInput = -32102,
    CryptoError = -32103,
    StorageError = -32104,
    RpcError = -32105,
    NetworkError = -32106,
    Internal = -32107,
}

impl A3chatErrorCode {
    pub fn code(self) -> i32 {
        self as i32
    }
}

impl From<&A3chatError> for A3chatErrorCode {
    fn from(e: &A3chatError) -> Self {
        match e {
            A3chatError::NotFound(_) => A3chatErrorCode::NotFound,
            A3chatError::PermissionDenied(_) => A3chatErrorCode::PermissionDenied,
            A3chatError::InvalidInput(_) => A3chatErrorCode::InvalidInput,
            A3chatError::CryptoError(_) => A3chatErrorCode::CryptoError,
            A3chatError::StorageError(_) => A3chatErrorCode::StorageError,
            A3chatError::RpcError(_) => A3chatErrorCode::RpcError,
            A3chatError::NetworkError(_) => A3chatErrorCode::NetworkError,
            A3chatError::Internal(_) => A3chatErrorCode::Internal,
        }
    }
}

impl From<A3chatErrorCode> for i32 {
    fn from(c: A3chatErrorCode) -> Self {
        c as i32
    }
}

// -- Bridge impls so `a3chat-app` and `a3chat-rpc` can `?`-convert
// underlying errors without writing `.map_err(...)` everywhere.
//
// NOTE: We deliberately do **not** `impl From<rusqlite::Error> for
// A3chatError` here — that crate is not a dependency of `a3chat-core`
// and forcing it on every consumer (Tauri frontend, mobile client)
// would bloat the dependency tree. The `a3chat-app` crate owns the
// `rusqlite -> A3chatError` conversion in its `storage` module.

impl From<serde_json::Error> for A3chatError {
    fn from(e: serde_json::Error) -> Self {
        A3chatError::Internal(format!("serde_json: {e}"))
    }
}

impl From<std::io::Error> for A3chatError {
    fn from(e: std::io::Error) -> Self {
        A3chatError::StorageError(format!("io: {e}"))
    }
}

// ── a3net-error bridge ──────────────────────────────────────────────────
//
// Every a3chat error can be reported through `a3net_error::IntoReport`,
// so operators get a uniform shape across all A3Net sub-crates. The
// wire-numeric code (`A3chatErrorCode`) and the string code ("CHA-xxx")
// are kept side-by-side: the wire code is unchanged for backward
// compatibility with existing RPC clients; the string code is what
// observability / dashboards group on.

/// Stable string identifiers for the a3chat error surface.
/// Format: `CHA-<index>`. New entries are append-only.
pub const CHA_NOT_FOUND: &str = "CHA-001";
pub const CHA_PERMISSION_DENIED: &str = "CHA-002";
pub const CHA_INVALID_INPUT: &str = "CHA-003";
pub const CHA_CRYPTO: &str = "CHA-004";
pub const CHA_STORAGE: &str = "CHA-005";
pub const CHA_RPC: &str = "CHA-006";
pub const CHA_NETWORK: &str = "CHA-007";
pub const CHA_INTERNAL: &str = "CHA-008";

impl IntoReport for A3chatError {
    fn code(&self) -> &'static str {
        match self {
            A3chatError::NotFound(_) => CHA_NOT_FOUND,
            A3chatError::PermissionDenied(_) => CHA_PERMISSION_DENIED,
            A3chatError::InvalidInput(_) => CHA_INVALID_INPUT,
            A3chatError::CryptoError(_) => CHA_CRYPTO,
            A3chatError::StorageError(_) => CHA_STORAGE,
            A3chatError::RpcError(_) => CHA_RPC,
            A3chatError::NetworkError(_) => CHA_NETWORK,
            A3chatError::Internal(_) => CHA_INTERNAL,
        }
    }
    fn kind(&self) -> ErrorKind {
        match self {
            A3chatError::NotFound(_) => ErrorKind::NotFound,
            A3chatError::PermissionDenied(_) => ErrorKind::Forbidden,
            A3chatError::InvalidInput(_) => ErrorKind::BadRequest,
            A3chatError::CryptoError(_) => ErrorKind::Forbidden, // Security: treat as Forbidden
            A3chatError::StorageError(_) => ErrorKind::Internal,
            A3chatError::RpcError(_) => ErrorKind::Unavailable,
            A3chatError::NetworkError(_) => ErrorKind::Timeout,
            A3chatError::Internal(_) => ErrorKind::Internal,
        }
    }
    fn severity(&self) -> Severity {
        match self.error_class() {
            ErrorClass::Permanent => Severity::Warn,
            ErrorClass::Transient => Severity::Warn,
            ErrorClass::Internal => Severity::Error,
            ErrorClass::Security => Severity::Fatal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_code_are_stable() {
        let e = A3chatError::NotFound("foo".into());
        assert_eq!(e.kind(), "not_found");
        assert_eq!(e.code(), -32100);
        assert_eq!(A3chatErrorCode::NotFound.code(), -32100);
    }

    #[test]
    fn code_mapping_round_trip() {
        let cases = [
            (A3chatError::NotFound("x".into()), A3chatErrorCode::NotFound),
            (
                A3chatError::PermissionDenied("x".into()),
                A3chatErrorCode::PermissionDenied,
            ),
            (
                A3chatError::InvalidInput("x".into()),
                A3chatErrorCode::InvalidInput,
            ),
            (
                A3chatError::CryptoError("x".into()),
                A3chatErrorCode::CryptoError,
            ),
            (
                A3chatError::StorageError("x".into()),
                A3chatErrorCode::StorageError,
            ),
            (A3chatError::RpcError("x".into()), A3chatErrorCode::RpcError),
            (
                A3chatError::NetworkError("x".into()),
                A3chatErrorCode::NetworkError,
            ),
            (A3chatError::Internal("x".into()), A3chatErrorCode::Internal),
        ];
        for (err, expected) in cases {
            assert_eq!(A3chatErrorCode::from(&err), expected);
        }
    }

    // ── a3net-error bridge tests ──────────────────────────────────────

    #[test]
    fn into_report_codes_are_unique_and_stable() {
        use a3net_error::IntoReport;
        let cases = [
            (A3chatError::NotFound("x".into()), CHA_NOT_FOUND),
            (
                A3chatError::PermissionDenied("x".into()),
                CHA_PERMISSION_DENIED,
            ),
            (A3chatError::InvalidInput("x".into()), CHA_INVALID_INPUT),
            (A3chatError::CryptoError("x".into()), CHA_CRYPTO),
            (A3chatError::StorageError("x".into()), CHA_STORAGE),
            (A3chatError::RpcError("x".into()), CHA_RPC),
            (A3chatError::NetworkError("x".into()), CHA_NETWORK),
            (A3chatError::Internal("x".into()), CHA_INTERNAL),
        ];
        let mut seen = std::collections::HashSet::new();
        for (e, code) in cases {
            assert_eq!(IntoReport::code(&e), code);
            assert!(seen.insert(code), "duplicate code {code}");
        }
    }

    #[test]
    fn into_report_crypto_is_fatal_severity() {
        use a3net_error::{IntoReport, Severity};
        let e = A3chatError::CryptoError("tag mismatch".into());
        assert_eq!(IntoReport::severity(&e), Severity::Fatal);
    }

    #[test]
    fn into_report_permanent_is_warn_severity() {
        use a3net_error::{IntoReport, Severity};
        let e = A3chatError::NotFound("x".into());
        assert_eq!(IntoReport::severity(&e), Severity::Warn);
    }

    #[test]
    fn into_report_transient_is_warn_severity() {
        use a3net_error::{IntoReport, Severity};
        let e = A3chatError::NetworkError("x".into());
        assert_eq!(IntoReport::severity(&e), Severity::Warn);
    }

    #[test]
    fn into_report_internal_is_error_severity() {
        use a3net_error::{IntoReport, Severity};
        let e = A3chatError::Internal("x".into());
        assert_eq!(IntoReport::severity(&e), Severity::Error);
    }

    #[test]
    fn into_report_message_includes_context() {
        use a3net_error::IntoReport;
        let e = A3chatError::NotFound("conversation c-123".into());
        let msg = IntoReport::message(&e);
        assert!(msg.contains("c-123"), "message lost context: {msg}");
    }

    #[test]
    fn into_report_kinds_match_a3net_taxonomy() {
        use a3net_error::{ErrorKind, IntoReport};
        // The mapping is part of the public contract — operators
        // group dashboards by kind.
        let pairs = [
            (A3chatError::NotFound("x".into()), ErrorKind::NotFound),
            (
                A3chatError::PermissionDenied("x".into()),
                ErrorKind::Forbidden,
            ),
            (
                A3chatError::InvalidInput("x".into()),
                ErrorKind::BadRequest,
            ),
            (A3chatError::RpcError("x".into()), ErrorKind::Unavailable),
            (
                A3chatError::NetworkError("x".into()),
                ErrorKind::Timeout,
            ),
        ];
        for (e, expected) in pairs {
            assert_eq!(IntoReport::kind(&e), expected);
        }
    }

    #[test]
    fn into_report_full_report_carries_crate_label() {
        use a3net_error::IntoReport;
        let e = A3chatError::NotFound("foo".into());
        let r = e.into_report("a3chat-core");
        assert_eq!(r.code, CHA_NOT_FOUND);
        // The `crate` detail is set automatically by IntoReport.
        assert!(r.details.contains_key("crate"));
    }

    #[test]
    fn error_display_includes_context() {
        let e = A3chatError::NotFound("conversation c1".into());
        assert!(e.to_string().contains("conversation c1"));
    }

    #[test]
    fn io_and_sqlite_bridge_to_storage_error() {
        let io_err = std::io::Error::other("boom");
        let wrapped: A3chatError = io_err.into();
        assert!(matches!(wrapped, A3chatError::StorageError(_)));
    }

    #[test]
    fn error_class_groups_transient_failures() {
        let e = A3chatError::NetworkError("peer offline".into());
        assert_eq!(e.error_class(), ErrorClass::Transient);
        assert!(e.is_retryable());
    }

    #[test]
    fn error_class_groups_crypto_as_security() {
        let e = A3chatError::CryptoError("AEAD tag mismatch".into());
        assert_eq!(e.error_class(), ErrorClass::Security);
        assert!(!e.is_retryable());
    }

    #[test]
    fn error_class_groups_validation_as_permanent() {
        let cases = [
            A3chatError::NotFound("x".into()),
            A3chatError::InvalidInput("x".into()),
            A3chatError::PermissionDenied("x".into()),
        ];
        for e in cases {
            assert_eq!(e.error_class(), ErrorClass::Permanent);
            assert!(!e.is_retryable());
        }
    }

    #[test]
    fn error_class_groups_internal_and_storage_as_internal() {
        let cases = [
            A3chatError::StorageError("x".into()),
            A3chatError::Internal("x".into()),
        ];
        for e in cases {
            assert_eq!(e.error_class(), ErrorClass::Internal);
            assert!(!e.is_retryable());
        }
    }

    #[test]
    fn error_class_serializes_to_snake_case_strings() {
        // The wire format must be stable across versions so RPC
        // clients and the daemon agree.
        let cases = [
            (ErrorClass::Permanent, r#""permanent""#),
            (ErrorClass::Transient, r#""transient""#),
            (ErrorClass::Internal, r#""internal""#),
            (ErrorClass::Security, r#""security""#),
        ];
        for (cls, expected) in cases {
            let s = serde_json::to_string(&cls).unwrap();
            assert_eq!(s, expected);
        }
    }

    #[test]
    fn is_retryable_is_false_for_permanent_security_internal() {
        assert!(!A3chatError::NotFound("x".into()).is_retryable());
        assert!(!A3chatError::CryptoError("x".into()).is_retryable());
        assert!(!A3chatError::Internal("x".into()).is_retryable());
    }
}
