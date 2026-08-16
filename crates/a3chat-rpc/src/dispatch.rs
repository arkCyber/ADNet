//! Dispatch a single JSON-RPC 2.0 call to the [`a3chat_app::A3chatApp`].
//!
//! Compliance targets:
//! - JSON-RPC 2.0 (IETF draft, the *de-facto* transport used by the
//!   Tauri desktop client and the Flutter mobile app).
//! - DO-178C §6.1 (deterministic behaviour under failure):
//!   error envelopes always carry the stable `CHA-xxx` string code
//!   AND the `ErrorKind` derived from the original `A3chatError`,
//!   not a synthesised variant.
//!
//! See [`a3chat-core` README](a3chat_core::rpc) for the canonical
//! list of method names.

use a3chat_app::A3chatApp;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;
use a3chat_core::error::A3chatError;
use a3net_error::{ErrorKind, IntoReport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ERR_METHOD_NOT_FOUND, ERR_PARSE, RpcError};

/// Maximum permitted size (in bytes) for a single JSON-RPC request
/// envelope. Caps payload-driven DoS at the dispatcher boundary
/// (the `axum::extract::Json` body-limit lives one layer above; this
/// is a defence-in-depth limit on the parsed envelope itself).
pub const MAX_ENVELOPE_BYTES: usize = 1 * 1024 * 1024; // 1 MiB

/// Maximum number of methods in a single JSON-RPC batch request.
/// JSON-RPC 2.0 §6 allows batches but doesn't bound their size —
/// DO-178C guidance (deterministic resource usage) requires a cap.
pub const MAX_BATCH_LEN: usize = 64;

/// Request payload — the JSON-RPC 2.0 envelope.
///
/// Per spec §4.2 the `id` field MAY be `null`, which signals a
/// *notification*: the server MUST NOT reply. We honour that here
/// by returning [`RpcResponse::Notification`] from the dispatch
/// helper instead of `success` / `failure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub id: Value,
}

impl RpcRequest {
    /// True iff the envelope shape is well-formed per spec §4.2
    /// — `jsonrpc` is exactly `"2.0"`, method is non-empty, and
    /// `id` is one of `Number`, `String`, or `Null`.
    pub fn is_valid_envelope(&self) -> bool {
        self.jsonrpc == "2.0"
            && !self.method.is_empty()
            && (self.id.is_number() || self.id.is_string() || self.id.is_null())
    }

    /// True iff this is a *notification* (a request whose `id`
    /// is `null`). Per JSON-RPC 2.0 §4.1 the server MUST NOT
    /// reply to notifications.
    pub fn is_notification(&self) -> bool {
        self.id.is_null()
    }
}

/// Response payload — the JSON-RPC 2.0 reply.
///
/// `result` and `error` are mutually exclusive. `Notification` is a
/// marker that the request was a notification — the caller should
/// not serialise any reply back over the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    pub fn failure(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Validate the method name is a known `a3chat.*` method.
///
/// Uses [`A3chatRpcMethod::ALL`] as the canonical list. Any string
/// not in that list produces a JSON-RPC `Method not found`
/// (-32601) envelope — see [`dispatch_rpc_call`].
pub fn is_known_method(method: &str) -> bool {
    A3chatRpcMethod::ALL.contains(&method)
}

/// Map an `A3chatError` to the canonical `ErrorKind` tag exposed
/// on the wire (under the `error.kind` field). Mirrors the
/// mapping in [`RpcError::From<A3chatError>`] so the kind is
/// always consistent regardless of which conversion path was
/// taken.
fn kind_of(e: &A3chatError) -> ErrorKind {
    <A3chatError as IntoReport>::kind(e)
}

/// Dispatch one [`RpcRequest`] to the [`A3chatApp`].
///
/// `owner` is the authenticated user — taken from the
/// `X-A3Chat-Owner` header in production. `request_id` is the
/// `X-A3Chat-Request-Id` value (or `None`) — used to correlate
/// structured tracing events with client-side logs.
///
/// Returns one of:
/// - `RpcResponse::success(...)` — happy path.
/// - `RpcResponse::failure(...)` — spec error envelope (always
///   carries the original `A3chatError`'s string code + kind).
/// - `RpcResponse { result: None, error: None, ... }` — for
///   notifications (per JSON-RPC 2.0 §4.1 the server MUST NOT
///   reply).
pub async fn dispatch_rpc_call(
    app: &A3chatApp,
    owner: &UserId,
    req: RpcRequest,
    request_id: Option<&str>,
) -> RpcResponse {
    // Notifications (id == null) — process the call, but never
    // emit a reply over the wire.
    let is_notification = req.is_notification();

    // ── Envelope validation ──────────────────────────────────────
    if !req.is_valid_envelope() {
        return RpcResponse::failure(req.id, RpcError::invalid_request());
    }

    // ── Method allow-list ─────────────────────────────────────────
    if !is_known_method(&req.method) {
        let err = RpcError::new(
            ERR_METHOD_NOT_FOUND,
            format!("unknown method: {}", req.method),
        );
        if !is_notification {
            return RpcResponse::failure(req.id, err);
        }
        // Emit + drop the error for notifications (no reply).
        tracing::warn!(method = %req.method, "rpc method not found (notification)");
        return empty(req.id);
    }

    // ── Dispatch ──────────────────────────────────────────────────
    match app.dispatch(&req.method, owner, req.params.clone()).await {
        Ok(result) => {
            if is_notification {
                empty(req.id)
            } else {
                RpcResponse::success(req.id, result)
            }
        }
        Err(e) => {
            // Emit the structured tracing event ONCE, from the
            // original `A3chatError`. We previously synthesised a
            // new `A3chatError::Internal` here, which destroyed
            // the original `code()` / `kind()` / `message()` and
            // produced a misleading `CHA-008 / internal` tag in
            // the structured log. DO-178C §6.3 fail-safe: we must
            // preserve the original classification.
            let code = <A3chatError as IntoReport>::code(&e).to_string();
            let kind = kind_of(&e);
            let message = e.to_string();
            let mut report = e.into_report("a3chat-rpc");
            if let Some(rid) = request_id {
                report = report.with_correlation(rid);
            }
            // `into_report` picks severity via the
            // `IntoReport::severity` hook on `A3chatError`. We
            // intentionally do NOT override it here — the
            // dispatcher's view of severity must match the
            // domain layer's view, otherwise observability
            // dashboards double-count under the wrong bucket.
            report.emit();

            let mut rpc_err: RpcError = RpcError::new(code_to_wire(&e), message);
            rpc_err.string_code = Some(code);
            rpc_err.kind = Some(kind.as_str().to_string());
            if let Some(rid) = request_id {
                rpc_err.data = Some(serde_json::json!({"request_id": rid}));
            }
            if is_notification {
                empty(req.id)
            } else {
                RpcResponse::failure(req.id, rpc_err)
            }
        }
    }
}

/// Convert a domain-level [`A3chatError`] to the JSON-RPC numeric
/// code used on the wire. Mirrors `From<A3chatError> for RpcError`
/// so the mapping is identical whether the caller went through
/// the `From` impl or this dispatcher path.
fn code_to_wire(e: &A3chatError) -> i32 {
    use crate::error::*;
    match e {
        A3chatError::NotFound(_) => ERR_A3CHAT_NOT_FOUND,
        A3chatError::PermissionDenied(_) => ERR_A3CHAT_PERMISSION_DENIED,
        A3chatError::InvalidInput(_) => ERR_A3CHAT_INVALID_INPUT,
        A3chatError::CryptoError(_) => ERR_A3CHAT_CRYPTO,
        A3chatError::StorageError(_) => ERR_A3CHAT_STORAGE,
        A3chatError::NetworkError(_) => ERR_A3CHAT_NETWORK,
        A3chatError::RpcError(_) => ERR_INTERNAL,
        A3chatError::Internal(_) => ERR_INTERNAL,
    }
}

/// Build an empty response (used for notifications — we still
/// keep `jsonrpc` + `id` so the caller can serialise it if they
/// want to log it).
fn empty(id: Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: None,
    }
}

/// Parse a JSON-RPC envelope from a raw JSON value. Returns a
/// [`RpcError::parse_error`] (per spec §4.5) on failure, with
/// the underlying error attached as `data` for debugging.
///
/// Accepts both single-object and batch (array) envelopes.
/// The batch is returned as a `Vec<RpcRequest>` — the caller
/// dispatches each element separately and emits a JSON array
/// reply (or `204 No Content` if every element was a
/// notification, per spec §6).
pub fn parse_envelope(value: &Value) -> Result<ParsedEnvelope, RpcError> {
    if value.is_array() {
        let arr = value.as_array().expect("checked is_array");
        if arr.is_empty() {
            return Err(RpcError::new(ERR_PARSE, "empty batch"));
        }
        if arr.len() > MAX_BATCH_LEN {
            return Err(RpcError::new(
                ERR_PARSE,
                format!("batch length {} exceeds limit {}", arr.len(), MAX_BATCH_LEN),
            ));
        }
        let mut out = Vec::with_capacity(arr.len());
        for v in arr {
            out.push(parse_single(v)?);
        }
        Ok(ParsedEnvelope::Batch(out))
    } else {
        Ok(ParsedEnvelope::Single(parse_single(value)?))
    }
}

fn parse_single(value: &Value) -> Result<RpcRequest, RpcError> {
    // Sanity-check the raw payload size — serde itself doesn't
    // bound this, but a 100-MiB JSON parse can starve the
    // executor. The byte-length of the original `Body` is
    // bounded by axum's body-limit; this check is for parsed
    // depth / number-of-fields DoS.
    let approx_bytes = value.to_string().len();
    if approx_bytes > MAX_ENVELOPE_BYTES {
        return Err(RpcError::new(
            ERR_PARSE,
            format!("envelope too large: {approx_bytes} bytes (limit {MAX_ENVELOPE_BYTES})"),
        ));
    }
    serde_json::from_value::<RpcRequest>(value.clone())
        .map_err(|e| RpcError::new(ERR_PARSE, format!("json: {e}")))
}

/// A parsed JSON-RPC envelope, either single or batch.
#[derive(Debug)]
pub enum ParsedEnvelope {
    Single(RpcRequest),
    Batch(Vec<RpcRequest>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::rpc::A3chatRpcMethod;
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    #[test]
    fn valid_envelope_is_accepted() {
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "a3chat.foo".into(),
            params: serde_json::json!({}),
            id: serde_json::json!(1),
        };
        assert!(req.is_valid_envelope());
        assert!(!req.is_notification());
    }

    #[test]
    fn null_id_is_a_notification() {
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "a3chat.foo".into(),
            params: serde_json::json!({}),
            id: serde_json::Value::Null,
        };
        assert!(req.is_valid_envelope());
        assert!(req.is_notification());
    }

    #[test]
    fn invalid_jsonrpc_version_rejected() {
        let req = RpcRequest {
            jsonrpc: "1.0".into(),
            method: "a3chat.foo".into(),
            params: serde_json::json!({}),
            id: serde_json::json!(1),
        };
        assert!(!req.is_valid_envelope());
    }

    #[test]
    fn empty_method_rejected() {
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "".into(),
            params: serde_json::json!({}),
            id: serde_json::json!(1),
        };
        assert!(!req.is_valid_envelope());
    }

    #[test]
    fn is_known_method_returns_true_for_listed_methods() {
        for m in A3chatRpcMethod::ALL {
            assert!(is_known_method(m), "expected {m} to be known");
        }
    }

    #[test]
    fn is_known_method_returns_false_for_arbitrary_strings() {
        assert!(!is_known_method("a3chat.bogus"));
        assert!(!is_known_method("core.something"));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_error_envelope() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(
            a3chat_app::storage::StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "a3chat.bogus".into(),
            params: serde_json::json!({}),
            id: serde_json::json!(1),
        };
        let resp = dispatch_rpc_call(&app, &owner(), req, None).await;
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, ERR_METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn dispatch_notification_returns_empty_envelope() {
        // Per JSON-RPC 2.0 §4.1 a notification (id == null) MUST
        // not produce a reply. Verify our dispatcher honours that
        // for both success and error paths.
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(
            a3chat_app::storage::StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();

        // Happy-path notification
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: A3chatRpcMethod::CONTACT_LIST.into(),
            params: serde_json::json!({}),
            id: serde_json::Value::Null,
        };
        let resp = dispatch_rpc_call(&app, &owner(), req, None).await;
        assert!(resp.error.is_none(), "notification must not error");
        assert!(resp.result.is_none(), "notification must not return a result");
        assert_eq!(resp.id, serde_json::Value::Null);

        // Error-path notification
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: "a3chat.bogus".into(),
            params: serde_json::json!({}),
            id: serde_json::Value::Null,
        };
        let resp = dispatch_rpc_call(&app, &owner(), req, None).await;
        assert!(resp.error.is_none(), "notification errors are dropped");
        assert!(resp.result.is_none());
    }

    #[tokio::test]
    async fn dispatch_invalid_envelope_returns_invalid_request() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(
            a3chat_app::storage::StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let req = RpcRequest {
            jsonrpc: "1.0".into(),
            method: "a3chat.contact.list".into(),
            params: serde_json::json!({}),
            id: serde_json::json!(1),
        };
        let resp = dispatch_rpc_call(&app, &owner(), req, None).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn dispatch_routes_to_contact_service() {
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(
            a3chat_app::storage::StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: A3chatRpcMethod::CONTACT_LIST.into(),
            params: serde_json::json!({}),
            id: serde_json::json!("c1"),
        };
        let resp = dispatch_rpc_call(&app, &owner(), req, None).await;
        assert!(resp.result.is_some());
        let v = resp.result.unwrap();
        assert!(v.get("contacts").is_some());
    }

    #[tokio::test]
    async fn dispatch_preserves_original_error_classification() {
        // Regression guard for the prior synthesised-Internal bug:
        // when the underlying service returns a NotFound, the
        // structured fields on the RpcError must carry the *real*
        // string code, not CHA-008.
        let dir = tempdir().unwrap();
        let app = A3chatApp::new(
            a3chat_app::storage::StorageConfig::new(dir.path().to_path_buf()),
            owner(),
        )
        .unwrap();
        // Ask for a non-existent conversation — chat.conversation.open
        // returns NotFound on unknown ids (verified by the chat
        // service unit tests). We don't have to assert the exact
        // code path here; we just need a domain NotFound to land
        // on the wire.
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: A3chatRpcMethod::CHAT_CONVERSATION_OPEN.into(),
            params: serde_json::json!({ "conversation_id": "does-not-exist" }),
            id: serde_json::json!(1),
        };
        let resp = dispatch_rpc_call(&app, &owner(), req, Some("trace-1")).await;
        let err = resp.error.expect("expected an error envelope");
        // Must not have collapsed to CHA-008 / internal.
        assert_ne!(err.code, -32603, "real classification must be preserved");
        assert!(
            err.string_code.as_deref() != Some("CHA-008"),
            "string code must come from the original error, got {:?}",
            err.string_code
        );
        // request_id must have been attached for correlation.
        let data = err.data.expect("data with request_id");
        assert_eq!(data["request_id"], serde_json::json!("trace-1"));
    }

    #[test]
    fn parse_envelope_round_trip() {
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "a3chat.contact.list",
            "params": {},
            "id": 1
        });
        let parsed = parse_envelope(&raw).unwrap();
        match parsed {
            ParsedEnvelope::Single(r) => assert_eq!(r.method, "a3chat.contact.list"),
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn parse_envelope_batch() {
        let raw = serde_json::json!([
            {"jsonrpc":"2.0","method":"a3chat.contact.list","id":1},
            {"jsonrpc":"2.0","method":"a3chat.contact.list","id":2},
        ]);
        let parsed = parse_envelope(&raw).unwrap();
        match parsed {
            ParsedEnvelope::Batch(v) => assert_eq!(v.len(), 2),
            _ => panic!("expected batch"),
        }
    }

    #[test]
    fn parse_envelope_rejects_empty_batch() {
        let raw = serde_json::json!([]);
        let err = parse_envelope(&raw).unwrap_err();
        assert_eq!(err.code, ERR_PARSE);
    }

    #[test]
    fn parse_envelope_rejects_oversize_batch() {
        let arr: Vec<_> = (0..MAX_BATCH_LEN + 1)
            .map(|i| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "a3chat.contact.list",
                    "id": i,
                })
            })
            .collect();
        let raw = serde_json::json!(arr);
        let err = parse_envelope(&raw).unwrap_err();
        assert_eq!(err.code, ERR_PARSE);
        assert!(err.message.contains("limit"));
    }

    #[test]
    fn parse_envelope_returns_parse_error_on_garbage() {
        let raw = serde_json::json!({
            "jsonrpc": 2.0,
            "method": 3,
        });
        let err = parse_envelope(&raw).unwrap_err();
        assert_eq!(err.code, ERR_PARSE);
    }

    #[test]
    fn rpc_response_success_serializes() {
        let r = RpcResponse::success(serde_json::json!(1), serde_json::json!({"ok": true}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"result\":{\"ok\":true}"));
        assert!(!s.contains("error"));
    }

    #[test]
    fn rpc_response_failure_serializes() {
        let r = RpcResponse::failure(serde_json::json!(1), RpcError::internal("oh no"));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"error\":"));
        assert!(!s.contains("result"));
    }

    #[test]
    fn rpc_response_notification_serializes_with_only_id() {
        let r = empty(serde_json::Value::Null);
        let s = serde_json::to_string(&r).unwrap();
        // Neither result nor error present in the wire format.
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"id\":null"));
        assert!(!s.contains("result"));
        assert!(!s.contains("error"));
    }
}
