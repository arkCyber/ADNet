//! Error type for [`crate`].
//!
//! Every fallible call on [`crate::EvmChainClient`] returns
//! [`WalletResult<T>`] = `Result<T, WalletError>`. We deliberately expose
//! a flat enum (no `Box<dyn Error>`) so callers can match on the variant
//! and CLI/RPC layers can render a stable human-readable message.
//!
//! ## Alloy error model
//!
//! In alloy 2.1.x the unified transport error is
//! `alloy_transport::TransportError`, which is a **type alias** for
//! `alloy_json_rpc::RpcError<TransportErrorKind, Box<RawValue>>`. Its
//! variants are:
//!
//! - `ErrorResp(ErrorPayload)` — server returned a JSON-RPC error
//!   response (e.g. `"method not found"`).
//! - `NullResp` — server returned `null` for a non-nullable result.
//! - `SerError` / `DeserError` — request/response (de)serialization.
//! - `Transport(TransportErrorKind)` — HTTP / connection / custom
//!   failures (DNS, TCP, TLS, 5xx, etc.).
//! - `UnsupportedFeature(&'static str)` / `LocalUsageError(...)` —
//!   caller-side problems that never hit the network.
//!
//! We collapse this into our four-bucket enum so callers only have one
//! error type to match on, regardless of which alloy variant produced
//! it.

use thiserror::Error;

use alloy_json_rpc::{ErrorPayload, RpcError};
use alloy_transport::{TransportError, TransportErrorKind};

/// Result alias used throughout the crate.
pub type WalletResult<T> = std::result::Result<T, WalletError>;

/// User-facing error type for EVM wallet operations.
///
/// Buckets, in order of "what the operator did wrong / what the network
/// did wrong":
///
/// - [`Transport`](Self::Transport) — could not reach the RPC endpoint
///   (DNS, TCP, TLS, HTTP 5xx, backend gone). Worth retrying.
/// - [`Rpc`](Self::Rpc) — endpoint was reached but rejected the call
///   with a JSON-RPC error response (method not found, internal error,
///   rate-limited). Not worth an immediate retry without changing the
///   request.
/// - [`Decode`](Self::Decode) — endpoint returned something we could not
///   parse into the expected typed response. Almost always a bug in our
///   type mapping or a non-standard RPC server.
/// - [`Invalid`](Self::Invalid) — caller-supplied input was malformed
///   (bad hex, unsupported chain id). Operator's fault, do not retry.
#[derive(Debug, Error)]
pub enum WalletError {
    /// RPC endpoint unreachable (DNS / TCP / TLS / HTTP 5xx / backend gone).
    ///
    /// Surfaced from `TransportErrorKind` variants. Treat as transient
    /// unless it persists.
    #[error("evm transport error: {0}")]
    Transport(String),

    /// RPC endpoint rejected the call with a JSON-RPC error response.
    ///
    /// Surfaced from `RpcError::ErrorResp(ErrorPayload)`. The wrapped
    /// string carries the server's `message` (and `code` when present).
    #[error("evm rpc error: {0}")]
    Rpc(String),

    /// RPC returned a payload that did not match the expected shape
    /// (null response, bad JSON, missing field, length mismatch, etc.).
    ///
    /// Almost always indicates either (a) a mismatch between our request
    /// and the server's response format, or (b) the server is non-standard.
    #[error("evm decode error: {0}")]
    Decode(String),

    /// Caller-side validation failure (bad address hex, unknown chain id,
    /// length mismatch, unsupported feature, local usage error).
    /// Returned synchronously before any network IO.
    #[error("evm invalid argument: {0}")]
    Invalid(String),

    /// Transaction-signing failure (EIP-712 build, signer key
    /// unavailable, etc.). Maps directly to the underlying
    /// `a3net_identity::IdentityError` so the caller can use the same
    /// display strings they already see from the identity crate.
    #[error("evm signing error: {0}")]
    Signing(String),

    /// Signer / wallet does not match the `from` address implied by
    /// the transaction. We refuse to silently re-sign to a different
    /// address because that is always a programming bug.
    #[error("signer mismatch: wallet {wallet} but tx from {tx_from}")]
    SignerMismatch { wallet: String, tx_from: String },

    /// Transaction timed out waiting for confirmation
    /// (`wait_for_receipt` polled past `timeout` without seeing the
    /// transaction in a block).
    #[error("transaction not confirmed within {timeout_secs}s: hash {tx_hash}")]
    ReceiptTimeout { tx_hash: String, timeout_secs: u64 },
}

impl WalletError {
    /// `true` when retrying without changing the request is unlikely to
    /// help (i.e. `Invalid` or `Decode`). Used by callers that wrap us in
    /// a retry-with-backoff loop.
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Invalid(_) | Self::Decode(_))
    }

    /// `true` when the error is on the network path (`Transport` or `Rpc`).
    /// Callers can use this to decide whether to surface a "network
    /// problem" message to the user.
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Rpc(_))
    }
}

// -- alloy error plumbing --------------------------------------------------
//
// `alloy_transport::TransportError` is a type alias for
// `RpcError<TransportErrorKind, Box<RawValue>>`. We match exhaustively
// on it once and map each variant into one of our four buckets. New
// alloy variants will fail to compile here, which is exactly what we
// want — it forces a deliberate review when the upstream enum grows.

impl From<TransportError> for WalletError {
    fn from(e: TransportError) -> Self {
        match e {
            // -- JSON-RPC level errors ----------------------------------
            RpcError::ErrorResp(payload) => Self::Rpc(format_error_payload(&payload)),
            RpcError::NullResp => Self::Decode(
                "server returned a null response for a non-nullable call".into(),
            ),
            RpcError::UnsupportedFeature(s) => Self::Invalid(format!("unsupported feature: {s}")),
            RpcError::LocalUsageError(e) => Self::Invalid(format!("local usage error: {e}")),

            // -- (de)serialization --------------------------------------
            RpcError::SerError(e) => Self::Decode(format!("serialize request: {e}")),
            RpcError::DeserError { err, .. } => Self::Decode(format!("deserialize response: {err}")),

            // -- Transport layer ----------------------------------------
            RpcError::Transport(kind) => Self::Transport(format_transport_kind(&kind)),
        }
    }
}

/// Render a JSON-RPC `ErrorPayload` into a single line for the
/// [`WalletError::Rpc`] variant. The wrapped message preserves the
/// server's `message` (which is what humans want to read) plus the
/// numeric `code` (which is what machines want to grep).
fn format_error_payload(payload: &ErrorPayload) -> String {
    match &payload.data {
        Some(data) => format!(
            "code {}: {} (data: {})",
            payload.code,
            payload.message,
            data.get(),
        ),
        None => format!("code {}: {}", payload.code, payload.message),
    }
}

/// Render a `TransportErrorKind` into a single line for the
/// [`WalletError::Transport`] variant. We deliberately use
/// `Display`-format for `HttpError` (which already includes status +
/// body) and `to_string()` for the boxed custom errors.
fn format_transport_kind(kind: &TransportErrorKind) -> String {
    match kind {
        // `HttpError`'s Display impl includes status + body.
        TransportErrorKind::HttpError(http) => http.to_string(),
        // The remaining variants' Display impls are all reasonable as-is.
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_json_rpc::ErrorPayload;

    #[test]
    fn classification_helpers() {
        let e = WalletError::Transport("timeout".into());
        assert!(e.is_network());
        assert!(!e.is_permanent());

        let e = WalletError::Rpc("-32601 method not found".into());
        assert!(e.is_network());
        assert!(!e.is_permanent());

        let e = WalletError::Decode("expected u64, got string".into());
        assert!(!e.is_network());
        assert!(e.is_permanent());

        let e = WalletError::Invalid("bad address hex".into());
        assert!(!e.is_network());
        assert!(e.is_permanent());
    }

    #[test]
    fn display_includes_context() {
        let e = WalletError::Transport("connection refused".into());
        assert!(e.to_string().contains("connection refused"));

        let e = WalletError::Invalid("foo".into());
        assert!(e.to_string().contains("foo"));
    }

    #[test]
    fn error_resp_maps_to_rpc_bucket() {
        let json = r#"{"code":-32601,"message":"method not found"}"#;
        let payload: ErrorPayload = serde_json::from_str(json).unwrap();
        let e: WalletError = TransportError::ErrorResp(payload).into();
        assert!(matches!(e, WalletError::Rpc(_)));
        let s = e.to_string();
        assert!(s.contains("method not found"), "{s}");
        assert!(s.contains("-32601"), "{s}");
    }

    #[test]
    fn error_resp_with_data_includes_data() {
        let json = r#"{"code":3,"message":"execution reverted","data":"0xdeadbeef"}"#;
        let payload: ErrorPayload = serde_json::from_str(json).unwrap();
        let e: WalletError = TransportError::ErrorResp(payload).into();
        let s = e.to_string();
        assert!(s.contains("execution reverted"));
        assert!(s.contains("0xdeadbeef"));
    }

    #[test]
    fn null_resp_maps_to_decode() {
        let e: WalletError = TransportError::NullResp.into();
        assert!(matches!(e, WalletError::Decode(_)));
    }

    #[test]
    fn unsupported_feature_maps_to_invalid() {
        let e: WalletError = TransportError::UnsupportedFeature("eth_subscribe").into();
        assert!(matches!(e, WalletError::Invalid(_)));
        assert!(e.to_string().contains("eth_subscribe"));
    }

    #[test]
    fn http_error_maps_to_transport() {
        let kind = TransportErrorKind::HttpError(alloy_transport::HttpError {
            status: 502,
            body: "bad gateway".into(),
        });
        let e: WalletError = TransportError::Transport(kind).into();
        assert!(matches!(e, WalletError::Transport(_)));
        let s = e.to_string();
        assert!(s.contains("502"), "{s}");
        assert!(s.contains("bad gateway"), "{s}");
    }

    #[test]
    fn custom_transport_error_maps_to_transport() {
        let kind = TransportErrorKind::Custom("connection reset by peer".into());
        let e: WalletError = TransportError::Transport(kind).into();
        assert!(matches!(e, WalletError::Transport(_)));
        assert!(e.to_string().contains("connection reset by peer"));
    }
}