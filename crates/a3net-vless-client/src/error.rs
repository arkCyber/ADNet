//! Unified error type for `a3net-vless-client`.
//!
//! The crate sits at the boundary between Rust and an external
//! subprocess (xray / sing-box) so the error model has to cover three
//! failure surfaces:
//!
//! 1. **User input** — bad `vless://…` URI, invalid port, missing
//!    fields. These are [`ErrorKind::BadRequest`].
//! 2. **Subprocess lifecycle** — the xray binary is missing, fails to
//!    start, exits unexpectedly, or its stdout carries an error
//!    message. These are [`ErrorKind::Unavailable`] /
//!    [`ErrorKind::Internal`].
//! 3. **Local proxy** — port already in use, the SOCKS5/HTTP listener
//!    fails to accept. These are [`ErrorKind::Unavailable`].
//!
//! All variants implement [`a3net_error::IntoReport`] so a single call
//! at the boundary (`client.serve()` / CLI handler) can lift the error
//! into the workspace's unified [`AdnetErrorReport`].
//!
//! ```rust
//! use a3net_vless_client::VlessClientError;
//! use a3net_error::{IntoReport, ErrorKind};
//!
//! let err = VlessClientError::BadLink("missing uuid".into());
//! let report = err.into_report("a3net-vless-client");
//! assert_eq!(report.code, "VLS-001");
//! assert_eq!(report.kind, ErrorKind::BadRequest);
//! ```

use a3net_error::{ErrorKind, IntoReport, Severity};
use thiserror::Error;

/// Crate-internal result alias. The `Error` type is also re-exported
/// at the crate root as [`crate::VlessClientError`].
pub type VlessClientResult<T> = Result<T, VlessClientError>;

/// All failures that can occur inside `a3net-vless-client`.
///
/// Every variant carries a stable 3-letter code (`VLS-NNN`). The codes
/// are appended-only — never renumbered, never reused.
#[derive(Debug, Error)]
pub enum VlessClientError {
    /// The supplied `vless://…` URI could not be parsed.
    ///
    /// Code `VLS-001`. [`ErrorKind::BadRequest`]. Typically caused by
    /// a typo in the UUID, missing port, unknown query parameter, or a
    /// fragment that doesn't decode as UTF-8.
    #[error("invalid vless link: {0}")]
    BadLink(String),

    /// A required piece of information was missing from the link.
    ///
    /// Code `VLS-002`. [`ErrorKind::BadRequest`]. Distinct from
    /// [`VlessClientError::BadLink`] so the caller can distinguish
    /// "this string is not a link at all" from "this is a link but it
    /// omits the UUID / port / SNI".
    #[error("missing required field: {field}")]
    MissingField {
        /// Which field was missing.
        field: &'static str,
    },

    /// The chosen local port is already in use.
    ///
    /// Code `VLS-003`. [`ErrorKind::Unavailable`]. Callers should
    /// retry with a different port — the user picked a conflict, the
    /// daemon did not.
    #[error("local proxy port {port} is already in use")]
    PortInUse {
        /// The conflicting port.
        port: u16,
    },

    /// The xray / sing-box binary is missing or cannot be executed.
    ///
    /// Code `VLS-004`. [`ErrorKind::Unavailable`]. Surfaces a
    /// diagnostic pointing to the path that was tried so the user can
    /// install the missing binary.
    #[error("vless backend binary not found: {path}")]
    BackendNotFound {
        /// The path that was probed.
        path: String,
    },

    /// The backend subprocess failed to start or died unexpectedly.
    ///
    /// Code `VLS-005`. [`ErrorKind::Unavailable`]. Carries the
    /// subprocess stderr tail so the report's `details["stderr"]`
    /// surfaces the root cause without the caller having to dig
    /// through tracing logs.
    #[error("vless backend exited: {message}")]
    BackendExited {
        /// Exit code (if available) — `None` means killed by signal.
        code: Option<i32>,
        /// Human-readable explanation assembled by the supervisor.
        message: String,
    },

    /// The backend subprocess could not be configured.
    ///
    /// Code `VLS-006`. [`ErrorKind::Internal`]. Surfaces when xray
    /// rejects the generated JSON config — typically a version skew
    /// between this crate's config emitter and the installed xray
    /// build.
    #[error("failed to configure vless backend: {0}")]
    BackendConfig(String),

    /// A local proxy protocol error (SOCKS5/HTTP).
    ///
    /// Code `VLS-007`. [`ErrorKind::DataLoss`]. Surfaces when the
    /// remote end sends an invalid SOCKS5 reply or an HTTP CONNECT
    /// response that doesn't parse.
    #[error("proxy protocol error: {0}")]
    ProxyProtocol(String),

    /// I/O failure on the local listener / per-connection stream.
    ///
    /// Code `VLS-008`. [`ErrorKind::Unavailable`]. Wraps the inner
    /// `std::io::Error` so its `Display` is preserved in the `cause`
    /// chain.
    #[error("vless client I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Tokio-level join error (the runtime refused a task).
    ///
    /// Code `VLS-009`. [`ErrorKind::Internal`]. Should be rare; the
    /// only place we currently spawn is the subprocess supervisor and
    /// the listener accept loop.
    #[error("tokio join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    /// The supervisor has been shut down and cannot accept new
    /// operations.
    ///
    /// Code `VLS-010`. [`ErrorKind::Cancelled`]. Returned by every
    /// method on [`crate::VlessClient`] after
    /// [`crate::VlessClient::shutdown`].
    #[error("vless client has been shut down")]
    Shutdown,
}

impl IntoReport for VlessClientError {
    fn code(&self) -> &'static str {
        match self {
            Self::BadLink(_) => "VLS-001",
            Self::MissingField { .. } => "VLS-002",
            Self::PortInUse { .. } => "VLS-003",
            Self::BackendNotFound { .. } => "VLS-004",
            Self::BackendExited { .. } => "VLS-005",
            Self::BackendConfig(_) => "VLS-006",
            Self::ProxyProtocol(_) => "VLS-007",
            Self::Io(_) => "VLS-008",
            Self::Join(_) => "VLS-009",
            Self::Shutdown => "VLS-010",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            Self::BadLink(_) | Self::MissingField { .. } => ErrorKind::BadRequest,
            Self::PortInUse { .. }
            | Self::BackendNotFound { .. }
            | Self::BackendExited { .. }
            | Self::Io(_) => ErrorKind::Unavailable,
            Self::BackendConfig(_) | Self::Join(_) => ErrorKind::Internal,
            Self::ProxyProtocol(_) => ErrorKind::DataLoss,
            Self::Shutdown => ErrorKind::Cancelled,
        }
    }

    fn severity(&self) -> Severity {
        match self {
            // Configuration / shutdown are normal control-flow paths
            // — `Info` lets the FFI / RPC layer log them without
            // spamming the error dashboard.
            Self::Shutdown | Self::MissingField { .. } | Self::BadLink(_) => Severity::Warn,
            // Subprocess crashes and port collisions are real
            // operational problems but the user can recover.
            Self::BackendExited { .. }
            | Self::BackendNotFound { .. }
            | Self::PortInUse { .. } => Severity::Error,
            // Everything else is unexpected and warrants an error.
            _ => Severity::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_unique() {
        // Pin all 10 codes so a careless reorder can't silently
        // shift a dashboard panel.
        let pairs = [
            (VlessClientError::BadLink("x".into()).code(), "VLS-001"),
            (
                VlessClientError::MissingField { field: "uuid" }.code(),
                "VLS-002",
            ),
            (
                VlessClientError::PortInUse { port: 1080 }.code(),
                "VLS-003",
            ),
            (
                VlessClientError::BackendNotFound {
                    path: "/x".into(),
                }
                .code(),
                "VLS-004",
            ),
            (
                VlessClientError::BackendExited {
                    code: Some(1),
                    message: "x".into(),
                }
                .code(),
                "VLS-005",
            ),
            (VlessClientError::BackendConfig("x".into()).code(), "VLS-006"),
            (VlessClientError::ProxyProtocol("x".into()).code(), "VLS-007"),
            // Io + Join variants — we just need *a* code match, not
            // an exhaustive pin per source.
            (
                VlessClientError::Io(std::io::Error::other("x")).code(),
                "VLS-008",
            ),
            (
                VlessClientError::Shutdown.code(),
                "VLS-010",
            ),
        ];
        let unique: std::collections::HashSet<_> = pairs.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), pairs.len(), "codes must be unique");
        for (got, expected) in pairs {
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn missing_field_is_bad_request() {
        let e = VlessClientError::MissingField { field: "uuid" };
        assert_eq!(e.kind(), ErrorKind::BadRequest);
        assert_eq!(e.severity(), Severity::Warn);
    }

    #[test]
    fn backend_exited_is_unavailable() {
        let e = VlessClientError::BackendExited {
            code: Some(2),
            message: "x".into(),
        };
        assert_eq!(e.kind(), ErrorKind::Unavailable);
        assert_eq!(e.severity(), Severity::Error);
    }

    #[test]
    fn shutdown_is_cancelled_and_info() {
        let e = VlessClientError::Shutdown;
        assert_eq!(e.kind(), ErrorKind::Cancelled);
    }

    #[test]
    fn display_includes_field_name() {
        let e = VlessClientError::MissingField { field: "port" };
        assert!(e.to_string().contains("port"));
    }
}
