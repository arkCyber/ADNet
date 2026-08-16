//! Audit logging for agent calls.
//!
//! Provides structured audit records and Prometheus metrics for every
//! AI-agent invocation. Feature-gated so nodes that do not want the
//! overhead pay zero cost.
//!
//! ## Metrics
//!
//! | Name | Type | Labels | Description |
//! |---|---|---|---|
//! | `a3net_agent_calls_total` | Counter | `outcome`, `model` | Total agent invocations |
//! | `a3net_agent_call_duration_seconds` | Histogram | `outcome` | Latency of agent calls |
//! | `a3net_agent_tokens_total` | Counter | `direction` | Tokens processed (input / output) |
//!
//! ## Audit log
//!
//! Each call emits a `tracing::info!` with a JSON payload:
//!
//! ```json
//! { "version": 1, "call_id": "uuid", "peer_node_id": "...",
//!   "model": "hermes-rust", "outcome": "ok",
//!   "started_at": "...", "finished_at": "...",
//!   "latency_ms": 1234,
//!   "usage": { "promptTokens": 42, "completionTokens": 18, "totalTokens": 60 },
//!   "error": null }
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! use a3net_agent::audit::{AuditCtx, Outcome};
//!
//! let ctx = AuditCtx::new("peer_hex".into(), "hermes-rust".into());
//! // ... call model ...
//! ctx.finish(Outcome::Ok);
//! ```

use serde::{Deserialize, Serialize};
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Core types (always compiled)
// ─────────────────────────────────────────────────────────────────────────────

/// Token usage snapshot from a model provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

impl TokenUsage {
    /// Total tokens, preferring the explicit total when present.
    pub fn total_or_estimate(&self) -> u64 {
        self.total_tokens
            .or_else(|| {
                self.prompt_tokens
                    .and_then(|p| self.completion_tokens.map(|c| p.saturating_add(c)))
            })
            .unwrap_or(0)
    }
}

/// Outcome of an agent call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    ModelError,
    AclDenied,
    NoModel,
    PeerNotPermitted,
}

impl Outcome {
    /// Prometheus label value.
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::ModelError => "model_error",
            Outcome::AclDenied => "acl_denied",
            Outcome::NoModel => "no_model",
            Outcome::PeerNotPermitted => "peer_not_permitted",
        }
    }
}

/// Structured audit record for one agent invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRecord {
    pub version: u8,
    pub call_id: String,
    pub peer_node_id: String,
    pub model: String,
    pub outcome: Outcome,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CallRecord {
    /// Emit as a `tracing::info!` event.
    pub fn emit(&self) {
        if !tracing::enabled!(tracing::Level::INFO) {
            return;
        }
        let json = serde_json::to_string(self).unwrap_or_else(|e| {
            format!(r#"{{"call_id":"{}","error":"serde-error:{}"}}"#, self.call_id, e)
        });
        tracing::info!(audit = %json, "agent_call");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Metrics backend
// ─────────────────────────────────────────────────────────────────────────────

/// Global audit metrics. `AGENT_AUDIT.record(...)` is always safe to call.
/// When `audit` is disabled, the call is a no-op.
#[cfg(feature = "audit")]
pub static AGENT_AUDIT: once_cell::sync::Lazy<
    std::sync::Arc<AgentMetrics>,
> = once_cell::sync::Lazy::new(|| std::sync::Arc::new(AgentMetrics::new()));

/// No-op stub when `audit` is disabled.
#[cfg(not(feature = "audit"))]
pub static AGENT_AUDIT: once_cell::sync::Lazy<AgentMetrics> =
    once_cell::sync::Lazy::new(AgentMetrics::new);

/// The actual metrics holder.
#[cfg(feature = "audit")]
pub struct AgentMetrics {
    calls_total: std::sync::Arc<a3net_observability::metrics::Counter>,
    call_duration_seconds: std::sync::Arc<a3net_observability::histogram::Histogram>,
    tokens_total: std::sync::Arc<a3net_observability::metrics::Counter>,
}

/// No-op stub.
#[cfg(not(feature = "audit"))]
pub struct AgentMetrics;

#[cfg(feature = "audit")]
impl AgentMetrics {
    fn new() -> Self {
        use a3net_observability::prelude::*;
        let r = &GLOBAL;
        Self {
            calls_total: r.register_counter("a3net_agent_calls_total", "Total agent invocations."),
            call_duration_seconds: r.register_histogram(
                "a3net_agent_call_duration_seconds",
                "Agent call latency in seconds.",
            ),
            tokens_total: r.register_counter("a3net_agent_tokens_total", "Total tokens processed."),
        }
    }
}

impl AgentMetrics {
    /// Emit metrics for an agent call. Safe to call always.
    pub fn record(
        &self,
        outcome: Outcome,
        model: &str,
        latency: Duration,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    ) {
        #[cfg(feature = "audit")]
        {
            use a3net_observability::prelude::*;

            let labels = LabelSet::new([
                ("outcome".to_string(), outcome.label().to_string()),
                ("model".to_string(), model.to_string()),
            ])
            .expect("static labels are valid");

            self.calls_total.inc_labels(&labels);
            self.call_duration_seconds
                .observe_labels(&labels, latency.as_secs_f64());

            if let Some(pt) = prompt_tokens {
                let input =
                    LabelSet::new([("direction".to_string(), "input".to_string())]).expect("static");
                self.tokens_total.inc_labels_by(&input, pt);
            }
            if let Some(ct) = completion_tokens {
                let output =
                    LabelSet::new([("direction".to_string(), "output".to_string())]).expect("static");
                self.tokens_total.inc_labels_by(&output, ct);
            }
        }
        #[cfg(not(feature = "audit"))]
        {
            let _ = (outcome, model, latency, prompt_tokens, completion_tokens);
        }
    }
}

#[cfg(not(feature = "audit"))]
impl AgentMetrics {
    /// No-op when audit is disabled.
    #[allow(dead_code)]
    pub fn record(
        &self,
        outcome: Outcome,
        model: &str,
        latency: Duration,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    ) {
        let _ = (outcome, model, latency, prompt_tokens, completion_tokens);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Audit context
// ─────────────────────────────────────────────────────────────────────────────

/// RAII guard for measuring call latency and emitting audit records.
///
/// Construct with `AuditCtx::new()`, call `finish()` when the call completes.
/// Use `abort()` for early exit without a successful outcome.
pub struct AuditCtx {
    call_id: String,
    peer_node_id: String,
    model: String,
    started_at: chrono::DateTime<chrono::Utc>,
    started: Instant,
    usage: Option<TokenUsage>,
    error: Option<String>,
}

impl AuditCtx {
    /// Begin a new audit record.
    pub fn new(peer_node_id: String, model: String) -> Self {
        Self {
            call_id: uuid::Uuid::new_v4().to_string(),
            peer_node_id,
            model,
            started_at: chrono::Utc::now(),
            started: Instant::now(),
            usage: None,
            error: None,
        }
    }

    /// Attach token usage to this call.
    #[allow(dead_code)]
    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Record an error message before finishing.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
    }

    /// Finalize the call with the given outcome and emit the audit record.
    ///
    /// Consumes `self` to prevent `Drop` from running.
    pub fn finish(self, outcome: Outcome) {
        // Wrap in ManuallyDrop so Drop does not run after we extract fields.
        let this = ManuallyDrop::new(self);
        let latency = this.started.elapsed();
        let record = CallRecord {
            version: 1,
            call_id: this.call_id.clone(),
            peer_node_id: this.peer_node_id.clone(),
            model: this.model.clone(),
            outcome,
            started_at: this.started_at,
            finished_at: chrono::Utc::now(),
            latency_ms: latency.as_millis() as u64,
            usage: this.usage.clone(),
            error: this.error.clone(),
        };
        record.emit();
        AGENT_AUDIT.record(
            outcome,
            &record.model,
            latency,
            record.usage.as_ref().and_then(|u| u.prompt_tokens),
            record.usage.as_ref().and_then(|u| u.completion_tokens),
        );
    }

    /// Abort the call without emitting a success record.
    /// Emits a `ModelError` record.
    pub fn abort(self) {
        let this = ManuallyDrop::new(self);
        let record = CallRecord {
            version: 1,
            call_id: this.call_id.clone(),
            peer_node_id: this.peer_node_id.clone(),
            model: this.model.clone(),
            outcome: Outcome::ModelError,
            started_at: this.started_at,
            finished_at: chrono::Utc::now(),
            latency_ms: this.started.elapsed().as_millis() as u64,
            usage: this.usage.clone(),
            error: this
                .error
                .clone()
                .or_else(|| Some("call aborted without result".into())),
        };
        record.emit();
        AGENT_AUDIT.record(
            Outcome::ModelError,
            &record.model,
            Duration::from_millis(record.latency_ms),
            record.usage.as_ref().and_then(|u| u.prompt_tokens),
            record.usage.as_ref().and_then(|u| u.completion_tokens),
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_label_is_snake_case() {
        assert_eq!(Outcome::Ok.label(), "ok");
        assert_eq!(Outcome::ModelError.label(), "model_error");
        assert_eq!(Outcome::AclDenied.label(), "acl_denied");
        assert_eq!(Outcome::NoModel.label(), "no_model");
        assert_eq!(Outcome::PeerNotPermitted.label(), "peer_not_permitted");
    }

    #[test]
    fn token_usage_total_or_estimate() {
        let t = TokenUsage {
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: None,
        };
        assert_eq!(t.total_or_estimate(), 15);

        let t = TokenUsage {
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(20),
        };
        assert_eq!(t.total_or_estimate(), 20);
        assert_eq!(TokenUsage::default().total_or_estimate(), 0);
    }

    #[test]
    fn call_record_json_roundtrip() {
        let record = CallRecord {
            version: 1,
            call_id: "test-uuid".into(),
            peer_node_id: "abcd1234".into(),
            model: "hermes-rust".into(),
            outcome: Outcome::Ok,
            started_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            finished_at: chrono::DateTime::from_timestamp(1, 0).unwrap(),
            latency_ms: 1000,
            usage: Some(TokenUsage {
                prompt_tokens: Some(42),
                completion_tokens: Some(18),
                total_tokens: None,
            }),
            error: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(r#""outcome":"ok""#));
        assert!(json.contains(r#""promptTokens":42"#));
        let back: CallRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.call_id, "test-uuid");
        assert_eq!(back.usage.as_ref().unwrap().prompt_tokens, Some(42));
    }

    #[test]
    fn audit_ctx_finish_succeeds() {
        let ctx = AuditCtx::new("peer1".into(), "mock".into());
        ctx.finish(Outcome::Ok);
    }

    #[test]
    fn audit_ctx_abort_succeeds() {
        let ctx = AuditCtx::new("peer1".into(), "mock".into());
        ctx.abort();
    }
}
