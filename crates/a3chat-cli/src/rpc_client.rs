//! HTTP JSON-RPC client with transient-error retry.
//!
//! Wraps [`reqwest::Client`] and forwards calls to a running
//! `a3chat-rpc` daemon. Honors the CLI retry policy:
//!
//! 1. **First attempt**: send the request.
//! 2. **On `Transient`**: retry with 100 ms / 300 ms / 900 ms backoff
//!    (exponential, full jitter from §6.3 of the audit).
//! 3. **On `Permanent`/`Security`/`Internal`**: surface immediately.
//!
//! Every request carries an `X-A3Chat-Request-Id` header that is
//! mirrored in tracing logs for **traceability** (DO-178C §5.2).

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use a3chat_core::error::{A3chatError, ErrorClass};

use crate::config::CliConfig;
use crate::error::{CliError, CliResult};

/// Default per-attempt timeout for HTTP requests. The CLI wraps this
/// in a `tokio::time::timeout` so the *whole* retry budget is also
/// bounded by `effective_timeout_ms`.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP JSON-RPC client. Cheap to clone (`reqwest::Client` is `Arc`).
#[derive(Clone, Debug)]
pub struct HttpRpcClient {
    base_url: String,
    owner: String,
    http: reqwest::Client,
    retries: u32,
    backoff: BackoffPolicy,
}

/// Result of one RPC call, with traceability metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RpcCallResult {
    /// The `result` field from the JSON-RPC response.
    pub value: serde_json::Value,
    /// The `X-A3Chat-Request-Id` header we sent (and the daemon
    /// mirrored in its own logs).
    pub request_id: String,
    /// Total number of HTTP attempts made (1 if no retries fired).
    pub attempts: u32,
}

/// Exponential-backoff schedule used by [`HttpRpcClient`]. Stored as
/// millis so the `Debug` impl stays readable.
#[derive(Clone, Debug)]
pub struct BackoffPolicy {
    pub base_ms: u64,
    pub factor: u64,
    pub max_ms: u64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            base_ms: 100,
            factor: 3,
            max_ms: 5_000,
        }
    }
}

impl BackoffPolicy {
    /// Compute the delay before retry attempt `n` (0-indexed). The
    /// first retry waits `base_ms`, the second `base_ms * factor`,
    /// and so on, capped at `max_ms`.
    pub fn delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::from_millis(0);
        }
        let exp = self.base_ms.saturating_mul(self.factor.saturating_pow(attempt - 1));
        Duration::from_millis(exp.min(self.max_ms))
    }
}

impl HttpRpcClient {
    /// Send a typed JSON-RPC call and deserialize the result.
    pub async fn call<P: Serialize, R: for<'de> serde::Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> CliResult<R> {
        let v = self.call_raw(method, params).await?;
        serde_json::from_value(v)
            .map_err(|e| CliError::Internal(format!("decode response for {method}: {e}")))
    }

    /// Open an SSE stream to `/{base}/rpc/stream`. Returns a
    /// [`reqwest::Response`] whose body is an HTTP byte stream —
    /// callers wrap it with `eventsource_stream::Eventsource` to
    /// yield `MessageEvent`s.
    ///
    /// DO-178C §5.2 — every stream carries the `X-A3Chat-Request-Id`
    /// header so daemon logs can be cross-referenced.
    pub async fn connect_sse(&self, request_id: &str) -> Result<reqwest::Response, reqwest::Error> {
        let url = format!("{}/rpc/stream", self.base_url.trim_end_matches('/'));
        self.http
            .get(&url)
            .header("X-A3Chat-Owner", &self.owner)
            .header("X-A3Chat-Request-Id", request_id)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
    }

    /// Send a JSON-RPC call and return the raw `result` JSON value.
    pub async fn call_raw<P: Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> CliResult<Value> {
        self.call_raw_with_meta(method, params, self.retries)
            .await
            .map(|r| r.value)
    }

    /// Send a JSON-RPC call and return both the raw value and the
    /// metadata (request_id, attempts) for traceability.
    pub async fn call_raw_with_meta<P: Serialize>(
        &self,
        method: &str,
        params: P,
        retries: u32,
    ) -> CliResult<RpcCallResult> {
        let params_json = serde_json::to_value(&params)
            .map_err(|e| CliError::Internal(format!("encode params for {method}: {e}")))?;
        let request_id = Uuid::new_v4().to_string();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params_json,
            "id": request_id,
        });
        let mut last_err: Option<CliError> = None;
        let attempts = retries.max(1);
        for attempt in 0..attempts {
            if attempt > 0 {
                let d = self.backoff.delay(attempt);
                tracing::warn!(
                    method,
                    attempt,
                    delay_ms = d.as_millis() as u64,
                    "retrying transient rpc error"
                );
                tokio::time::sleep(d).await;
            }
            match self.send_once(&body, &request_id).await {
                Ok(v) => {
                    return Ok(RpcCallResult {
                        value: v,
                        request_id: request_id.clone(),
                        attempts: attempt + 1,
                    });
                }
                Err(CliError::Rpc(e)) if e.is_retryable() && attempt + 1 < attempts => {
                    last_err = Some(CliError::Rpc(e));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        // Out of attempts.
        Err(last_err.unwrap_or_else(|| {
            CliError::Internal("retry loop exited without an error".into())
        }))
    }

    /// Single-shot HTTP POST. Public so tests can pin a known error.
    pub async fn send_once(
        &self,
        body: &Value,
        request_id: &str,
    ) -> CliResult<Value> {
        let url = format!("{}/rpc", self.base_url.trim_end_matches('/'));
        tracing::debug!(%url, method = %body["method"], %request_id, "rpc call");
        let resp = self
            .http
            .post(&url)
            .header("X-A3Chat-Owner", &self.owner)
            .header("X-A3Chat-Request-Id", request_id)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                A3chatError::NetworkError(format!("http send: {e}"))
            })?;
        let status = resp.status();
        // Parse the JSON-RPC envelope first so we can read structured
        // errors instead of returning empty bodies.
        let envelope: RawResponse = resp.json().await.map_err(|e| {
            CliError::Rpc(A3chatError::RpcError(format!(
                "http read [{status}]: {e}"
            )))
        })?;
        if let Some(err) = envelope.error {
            // Map JSON-RPC numeric codes back into typed variants so
            // retry logic works.
            let mapped = map_jsonrpc_error(err.code, err.message);
            return Err(CliError::Rpc(mapped));
        }
        if !status.is_success() {
            return Err(CliError::Rpc(A3chatError::RpcError(format!(
                "http {status} from server"
            )            )));
        }
        // JSON-RPC distinguishes a missing `result` (an error)
        // from an explicit `null` (a successful null return). We
        // therefore treat `Some(Value::Null)` and `None` both as
        // a successful null — the deserializer collapses them.
        match envelope.result {
            Some(v) => Ok(v),
            None => {
                if envelope.error.is_some() {
                    Err(CliError::Rpc(A3chatError::RpcError(
                        "error envelope (should have been mapped)".into(),
                    )))
                } else {
                    Ok(serde_json::Value::Null)
                }
            }
        }
    }

    /// Cheap accessor for diagnostics.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn retries(&self) -> u32 {
        self.retries
    }
}

/// JSON-RPC error envelope as returned by `a3chat-rpc`.
#[derive(serde::Deserialize)]
struct RawResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RawError>,
}

#[derive(serde::Deserialize)]
struct RawError {
    code: i32,
    message: String,
}

/// Builder for [`HttpRpcClient`]. Validates inputs *before*
/// constructing the underlying `reqwest::Client` (which is expensive
/// to build).
pub struct RpcClientBuilder<'a> {
    cfg: &'a CliConfig,
}

impl<'a> RpcClientBuilder<'a> {
    pub fn new(cfg: &'a CliConfig) -> Self {
        Self { cfg }
    }

    pub fn build(self) -> CliResult<HttpRpcClient> {
        let base_url = self.cfg.effective_daemon_url();
        let owner = self.cfg.effective_owner();
        // DO-178C §8 — defensive validation of operator-supplied values.
        crate::config::validate_owner(&owner)?;
        let timeout = Duration::from_millis(self.cfg.effective_timeout_ms());
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| CliError::Internal(format!("reqwest build: {e}")))?;
        Ok(HttpRpcClient {
            base_url,
            owner,
            http,
            retries: self.cfg.effective_retries(),
            backoff: BackoffPolicy::default(),
        })
    }
}

/// Map a JSON-RPC error `code` back into a typed [`A3chatError`].
///
/// The wire codes are those emitted by `a3chat-rpc::error`:
/// `-32001..-32007` for a3chat-specific errors and `-32600..-32603`
/// for standard JSON-RPC errors. Anything outside that range falls
/// through as a generic `RpcError`.
fn map_jsonrpc_error(code: i32, message: String) -> A3chatError {
    // a3chat-rpc wire codes (see crates/a3chat-rpc/src/error.rs).
    // We list the *server-emitted* codes here; both the a3chat-core
    // enum (-32100..-32107) and the older a3chat-rpc codes
    // (-32001..-32007) are recognised so a CLI can talk to either
    // generation of daemon.
    enum Kind {
        NotFound,
        PermissionDenied,
        InvalidInput,
        CryptoError,
        StorageError,
        NetworkError,
        Internal,
    }
    let kind = match code {
        -32001 | -32100 => Kind::NotFound,
        -32002 | -32101 => Kind::PermissionDenied,
        -32003 | -32102 | -32602 => Kind::InvalidInput,
        -32004 | -32103 => Kind::CryptoError,
        -32005 | -32104 => Kind::StorageError,
        -32006 | -32106 => Kind::NetworkError,
        -32603 | -32107 => Kind::Internal,
        // -32105 = RpcError on the core side. The wire-side RpcError
        // is the fallback bucket for everything else.
        -32105 | _ => return A3chatError::RpcError(format!("[{}] {}", code, message)),
    };
    match kind {
        Kind::NotFound => A3chatError::NotFound(message),
        Kind::PermissionDenied => A3chatError::PermissionDenied(message),
        Kind::InvalidInput => A3chatError::InvalidInput(message),
        Kind::CryptoError => A3chatError::CryptoError(message),
        Kind::StorageError => A3chatError::StorageError(message),
        Kind::NetworkError => A3chatError::NetworkError(message),
        Kind::Internal => A3chatError::Internal(message),
    }
}

/// Expose `ErrorClass` so callers can decide if a failure is
/// transient without re-importing `a3chat_core`.
pub fn class_of(e: &A3chatError) -> ErrorClass {
    e.error_class()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_progression() {
        let p = BackoffPolicy {
            base_ms: 100,
            factor: 3,
            max_ms: 5_000,
        };
        assert_eq!(p.delay(0).as_millis(), 0);
        assert_eq!(p.delay(1).as_millis(), 100);
        assert_eq!(p.delay(2).as_millis(), 300);
        assert_eq!(p.delay(3).as_millis(), 900);
        assert_eq!(p.delay(4).as_millis(), 2_700);
        assert_eq!(p.delay(5).as_millis(), 5_000); // capped
        assert_eq!(p.delay(10).as_millis(), 5_000); // still capped
    }

    #[test]
    fn map_jsonrpc_error_round_trip() {
        // Wire codes emitted by `a3chat-rpc::error::From<A3chatError>`.
        let cases = [
            (-32001i32, "NotFound"),
            (-32002, "PermissionDenied"),
            (-32003, "InvalidInput"),
            (-32004, "CryptoError"),
            (-32005, "StorageError"),
            (-32006, "NetworkError"),
            (-32603, "Internal"), // JSON-RPC spec internal error
        ];
        for (code, expected_variant) in cases {
            let e = map_jsonrpc_error(code, "x".into());
            let variant_matches = match expected_variant {
                "NotFound" => matches!(e, A3chatError::NotFound(_)),
                "PermissionDenied" => matches!(e, A3chatError::PermissionDenied(_)),
                "InvalidInput" => matches!(e, A3chatError::InvalidInput(_)),
                "CryptoError" => matches!(e, A3chatError::CryptoError(_)),
                "StorageError" => matches!(e, A3chatError::StorageError(_)),
                "NetworkError" => matches!(e, A3chatError::NetworkError(_)),
                "Internal" => matches!(e, A3chatError::Internal(_)),
                _ => false,
            };
            assert!(
                variant_matches,
                "code {code} should map to {expected_variant}"
            );
        }
    }

    #[test]
    fn map_jsonrpc_error_handles_core_codes_too() {
        // Legacy `-32100..-32107` codes (a3chat-core::error::A3chatErrorCode).
        let e = map_jsonrpc_error(-32102, "x".into());
        assert!(matches!(e, A3chatError::InvalidInput(_)));
    }

    #[test]
    fn map_jsonrpc_error_falls_back_for_unknown() {
        let e = map_jsonrpc_error(-999, "wat".into());
        assert!(matches!(e, A3chatError::RpcError(_)));
    }

    #[test]
    fn class_of_matches_core() {
        let e = A3chatError::NetworkError("x".into());
        assert_eq!(class_of(&e), ErrorClass::Transient);
        let e = A3chatError::NotFound("x".into());
        assert_eq!(class_of(&e), ErrorClass::Permanent);
    }

    #[test]
    fn builder_rejects_bad_owner() {
        let mut cfg = CliConfig::default();
        cfg.owner = Some("not-hex".into());
        let r = RpcClientBuilder::new(&cfg).build();
        assert!(r.is_err());
    }

    #[test]
    fn builder_succeeds_with_valid_owner() {
        let mut cfg = CliConfig::default();
        cfg.owner = Some("0".repeat(64));
        let r = RpcClientBuilder::new(&cfg).build().unwrap();
        assert_eq!(r.base_url(), crate::config::DEFAULT_DAEMON_URL);
        assert_eq!(r.owner(), &"0".repeat(64));
        assert_eq!(r.retries(), 3);
    }

    #[tokio::test]
    async fn send_once_against_unreachable_fails_transient() {
        let cfg = CliConfig {
            daemon_url: Some("http://127.0.0.1:1".into()),
            owner: Some("0".repeat(64)),
            output: None,
            retries: Some(1),
            timeout_ms: Some(500),
        };
        let c = RpcClientBuilder::new(&cfg).build().unwrap();
        let r = c
            .send_once(&serde_json::json!({"jsonrpc":"2.0","method":"a3chat.test","params":{},"id":"x"}), "x")
            .await;
        // Network failure → A3chatError::NetworkError → Rpc(NetworkError) → retryable.
        match r {
            Err(CliError::Rpc(e)) => assert!(e.is_retryable()),
            other => panic!("expected Rpc(NetworkError), got {other:?}"),
        }
    }
}