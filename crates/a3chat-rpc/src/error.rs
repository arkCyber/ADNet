//! JSON-RPC 2.0 error type. The numeric codes for the standard
//! cases match the IETF spec; the `a3chat.*` extras are documented
//! in `A3CHAT_DESIGN.md` §6.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use a3chat_core::error::A3chatError;
use a3net_error::IntoReport;

/// Standard JSON-RPC 2.0 error codes.
pub const ERR_PARSE: i32 = -32700;
pub const ERR_INVALID_REQUEST: i32 = -32600;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERR_INVALID_PARAMS: i32 = -32602;
pub const ERR_INTERNAL: i32 = -32603;

/// `a3chat.*` server-defined error codes. Negative numbers so they
/// don't collide with the standard set.
pub const ERR_A3CHAT_NOT_FOUND: i32 = -32001;
pub const ERR_A3CHAT_PERMISSION_DENIED: i32 = -32002;
pub const ERR_A3CHAT_INVALID_INPUT: i32 = -32003;
pub const ERR_A3CHAT_CRYPTO: i32 = -32004;
pub const ERR_A3CHAT_STORAGE: i32 = -32005;
pub const ERR_A3CHAT_NETWORK: i32 = -32006;
pub const ERR_A3CHAT_NOT_AUTHENTICATED: i32 = -32007;

/// Wire-format RPC error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    /// Optional structured payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Stable string code (`CHA-xxx`) — set when the error was
    /// produced by `a3chat-app`. Useful for observability
    /// dashboards that group on string codes rather than
    /// numeric ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_code: Option<String>,
    /// Coarse error kind from `a3net_error::ErrorKind`. Set
    /// alongside `string_code` for the same reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
            string_code: None,
            kind: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Annotate the error with the a3chat-specific string code and
    /// `a3net_error` kind, and emit a structured tracing event
    /// through `IntoReport::emit()` for observability dashboards.
    pub fn with_report_metadata(mut self, a3chat: &A3chatError, request_id: Option<&str>) -> Self {
        self.string_code = Some(<A3chatError as IntoReport>::code(a3chat).to_string());
        self.kind = Some(<A3chatError as IntoReport>::kind(a3chat).as_str().to_string());
        // Side-effect: emit structured tracing event.
        let mut report = a3chat.into_report("a3chat-rpc");
        if let Some(rid) = request_id {
            report = report.with_correlation(rid);
        }
        report.emit();
        self
    }

    pub fn parse_error() -> Self {
        Self::new(ERR_PARSE, "Parse error")
    }
    pub fn invalid_request() -> Self {
        Self::new(ERR_INVALID_REQUEST, "Invalid Request")
    }
    pub fn method_not_found(method: &str) -> Self {
        Self::new(ERR_METHOD_NOT_FOUND, format!("method not found: {method}"))
    }
    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(ERR_INVALID_PARAMS, detail)
    }
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(ERR_INTERNAL, detail)
    }

    /// True for codes that the client should retry. Used by the
    /// metrics layer to bucket errors as `RpcOutcome::Transient`
    /// vs `RpcOutcome::Error`.
    pub fn is_transient(&self) -> bool {
        matches!(
            self.code,
            ERR_A3CHAT_NETWORK | ERR_A3CHAT_STORAGE | ERR_INTERNAL
        )
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

impl From<A3chatError> for RpcError {
    fn from(e: A3chatError) -> Self {
        let code = match e {
            A3chatError::NotFound(_) => ERR_A3CHAT_NOT_FOUND,
            A3chatError::PermissionDenied(_) => ERR_A3CHAT_PERMISSION_DENIED,
            A3chatError::InvalidInput(_) => ERR_A3CHAT_INVALID_INPUT,
            A3chatError::CryptoError(_) => ERR_A3CHAT_CRYPTO,
            A3chatError::StorageError(_) => ERR_A3CHAT_STORAGE,
            A3chatError::NetworkError(_) => ERR_A3CHAT_NETWORK,
            A3chatError::RpcError(_) => ERR_INTERNAL,
            A3chatError::Internal(_) => ERR_INTERNAL,
        };
        let mut r = Self::new(code, e.to_string());
        r.string_code = Some(<A3chatError as IntoReport>::code(&e).to_string());
        r.kind = Some(<A3chatError as IntoReport>::kind(&e).as_str().to_string());
        r
    }
}

impl From<serde_json::Error> for RpcError {
    fn from(e: serde_json::Error) -> Self {
        Self::new(ERR_PARSE, format!("json: {e}"))
    }
}

/// Axum's `Json` extractor rejects with a 422 on parse failures
/// and a 413 on oversize bodies; map *both* into a JSON-RPC
/// `parse_error` envelope so the dispatcher surface is uniform.
impl From<axum::extract::rejection::JsonRejection> for RpcError {
    fn from(e: axum::extract::rejection::JsonRejection) -> Self {
        use axum::extract::rejection::JsonRejection::*;
        let (code, message) = match &e {
            JsonDataError(e) => (ERR_PARSE, format!("json: {e}")),
            JsonSyntaxError(e) => (ERR_PARSE, format!("json: {e}")),
            MissingJsonContentType(_) => (
                ERR_INVALID_PARAMS,
                "missing content-type: application/json".to_string(),
            ),
            BytesRejection(_) => (ERR_INVALID_PARAMS, "could not read body".to_string()),
            _ => (ERR_PARSE, format!("json rejection: {e}")),
        };
        Self::new(code, message)
    }
}

impl axum::response::IntoResponse for RpcError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "data": self.data,
            }
        });
        (axum::http::StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_has_standard_code() {
        let e = RpcError::parse_error();
        assert_eq!(e.code, ERR_PARSE);
    }

    #[test]
    fn method_not_found_includes_name() {
        let e = RpcError::method_not_found("a3chat.bogus");
        assert_eq!(e.code, ERR_METHOD_NOT_FOUND);
        assert!(e.message.contains("a3chat.bogus"));
    }

    #[test]
    fn display_includes_code_and_message() {
        let e = RpcError::invalid_params("missing x");
        let s = e.to_string();
        assert!(s.contains(&ERR_INVALID_PARAMS.to_string()));
        assert!(s.contains("missing x"));
    }

    #[test]
    fn with_data_attaches_payload() {
        let e = RpcError::internal("oops").with_data(serde_json::json!({"k": 1}));
        assert_eq!(e.data, Some(serde_json::json!({"k": 1})));
    }

    #[test]
    fn from_a3chat_error_maps_codes() {
        assert_eq!(
            RpcError::from(A3chatError::NotFound("x".into())).code,
            ERR_A3CHAT_NOT_FOUND
        );
        assert_eq!(
            RpcError::from(A3chatError::PermissionDenied("x".into())).code,
            ERR_A3CHAT_PERMISSION_DENIED
        );
        assert_eq!(
            RpcError::from(A3chatError::InvalidInput("x".into())).code,
            ERR_A3CHAT_INVALID_INPUT
        );
        assert_eq!(
            RpcError::from(A3chatError::CryptoError("x".into())).code,
            ERR_A3CHAT_CRYPTO
        );
        assert_eq!(
            RpcError::from(A3chatError::StorageError("x".into())).code,
            ERR_A3CHAT_STORAGE
        );
        assert_eq!(
            RpcError::from(A3chatError::NetworkError("x".into())).code,
            ERR_A3CHAT_NETWORK
        );
        assert_eq!(
            RpcError::from(A3chatError::Internal("x".into())).code,
            ERR_INTERNAL
        );
    }

    #[test]
    fn serde_round_trip() {
        let e = RpcError::new(ERR_INTERNAL, "x").with_data(serde_json::json!(1));
        let s = serde_json::to_string(&e).unwrap();
        let back: RpcError = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }

    // ── a3net-error integration tests ─────────────────────────────────

    #[test]
    fn from_a3chat_error_annotates_string_code_and_kind() {
        let a3chat = A3chatError::NotFound("conversation c-1".into());
        let rpc: RpcError = a3chat.into();
        assert_eq!(rpc.code, ERR_A3CHAT_NOT_FOUND);
        assert_eq!(rpc.string_code.as_deref(), Some("CHA-001"));
        assert_eq!(rpc.kind.as_deref(), Some("not_found"));
    }

    #[test]
    fn from_a3chat_error_annotates_crypto_as_forbidden() {
        let a3chat = A3chatError::CryptoError("tag mismatch".into());
        let rpc: RpcError = a3chat.into();
        assert_eq!(rpc.code, ERR_A3CHAT_CRYPTO);
        assert_eq!(rpc.string_code.as_deref(), Some("CHA-004"));
        assert_eq!(rpc.kind.as_deref(), Some("forbidden"));
    }

    #[test]
    fn from_a3chat_error_annotates_network_as_timeout() {
        let a3chat = A3chatError::NetworkError("peer unreachable".into());
        let rpc: RpcError = a3chat.into();
        assert_eq!(rpc.code, ERR_A3CHAT_NETWORK);
        assert_eq!(rpc.string_code.as_deref(), Some("CHA-007"));
        assert_eq!(rpc.kind.as_deref(), Some("timeout"));
    }

    #[test]
    fn from_a3chat_error_annotates_internal_as_internal() {
        let a3chat = A3chatError::Internal("bug".into());
        let rpc: RpcError = a3chat.into();
        assert_eq!(rpc.code, ERR_INTERNAL);
        assert_eq!(rpc.string_code.as_deref(), Some("CHA-008"));
        assert_eq!(rpc.kind.as_deref(), Some("internal"));
    }

    #[test]
    fn with_report_metadata_emits_without_panicking() {
        let a3chat = A3chatError::PermissionDenied("not in group".into());
        let rpc = RpcError::from(a3chat).with_report_metadata(
            &A3chatError::PermissionDenied("not in group".into()),
            Some("req-123"),
        );
        // `string_code` and `kind` set by `with_report_metadata`
        // would be a no-op (already set by `From`), but the side
        // effect — a tracing event with structured fields —
        // must not panic.
        assert_eq!(rpc.string_code.as_deref(), Some("CHA-002"));
        assert_eq!(rpc.kind.as_deref(), Some("forbidden"));
    }

    #[test]
    fn serde_round_trip_preserves_string_code_and_kind() {
        let a3chat = A3chatError::InvalidInput("empty sender".into());
        let rpc: RpcError = a3chat.into();
        let s = serde_json::to_string(&rpc).unwrap();
        // Wire format includes the string code.
        assert!(s.contains("CHA-003"), "missing string_code: {s}");
        assert!(s.contains("bad_request"), "missing kind: {s}");
        let back: RpcError = serde_json::from_str(&s).unwrap();
        assert_eq!(back.string_code, rpc.string_code);
        assert_eq!(back.kind, rpc.kind);
    }
}
