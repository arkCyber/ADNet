//! Hermes-Rust [`ChatModel`] adapter — OpenAI-compatible API at `localhost:11438`.
//!
//! Hermes-Rust is a local AI agent that exposes an OpenAI-compatible
//! HTTP API at `http://127.0.0.1:<port>/v1/chat/completions` (alongside
//! `/v1/chat`, `/v1/models`, `/health`, `/v1/curator/*`, …). This
//! adapter speaks the OpenAI-shaped subset so A3Net's `Agent` loop
//! can drive Hermes-Rust from any node without Hermes-Rust needing
//! to know about A3Net.
//!
//! # Defaults
//!
//! - `base_url`: `http://127.0.0.1:11438` (the Hermes-Rust-AI-agent
//!   port reserved for A3Net's local-host discovery range)
//! - `model`:   `hermes-rust` (the value Hermes-Rust reports on
//!   `GET /v1/models`)
//! - `path`:    `/v1/chat/completions` (OpenAI-compatible shape)
//!
//! Hermes-Rust's native (`/v1/chat`) endpoint takes a single `message`
//! string; the OpenAI-compatible endpoint takes a `messages` array.
//! A3Net always carries a `messages` array, so we use the OpenAI
//! shape by default — the operator can flip to `/v1/chat` by
//! overriding [`HermesConfig::path`] when hermes-rust's chat surface
//! is the preferred one.
//!
//! # Authentication
//!
//! When Hermes-Rust is launched with `api_token = "..."` in
//! `hermes.toml`, every request must carry either
//! `Authorization: Bearer <token>` or `X-API-Key: <token>`. The
//! adapter honours this via [`HermesConfig::api_token`].
//!
//! # Failure model
//!
//! Hermes-Rust speaks OpenAI's response shape verbatim, so the
//! adapter piggybacks on the Ollama wire types' response parser by
//! sharing the same `ChatResponse` shape (`content` + `tool_calls` +
//! `finish_reason`). Hermes-Rust does not (yet) echo `tool_calls` in
//! its response — its built-in tools run server-side. Calls fall
//! back to `FinishReason::Stop` unless the server explicitly returns
//! a `finish_reason` other than `"stop"`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chat::{
    ChatMessage, ChatModel, ChatRequest, ChatResponse, FinishReason, Role, ToolCall,
};
use crate::error::AgentError;

/// Configuration for [`HermesModel`].
#[derive(Debug, Clone)]
pub struct HermesConfig {
    /// Full base URL of the Hermes-Rust server, e.g. `http://127.0.0.1:11438`.
    /// Defaults to `http://127.0.0.1:11438`.
    pub base_url: String,
    /// Default model name sent in every request. Defaults to `hermes-rust`
    /// (the value Hermes-Rust reports on `/v1/models`).
    pub model: String,
    /// HTTP path under `base_url`. Defaults to `/v1/chat/completions`
    /// (OpenAI-compatible). Set to `/v1/chat` to use the native
    /// single-message shape (`{"message": "..."}`).
    pub path: String,
    /// Request timeout in seconds. Defaults to 300 s (Hermes-Rust runs
    /// locally and may take a while when invoking shell/file tools).
    pub timeout_secs: u64,
    /// Optional bearer / X-API-Key token. Mirrors Hermes-Rust's
    /// `api_token` config field; when `None`, the adapter skips the
    /// auth headers.
    pub api_token: Option<String>,
    /// Optional `X-Session-Id` value for conversation continuity.
    /// Hermes-Rust's `/v1/chat` returns a `session_id` on the first
    /// turn; the operator can stash it and re-supply it here so the
    /// next call resumes the same Hermes-Rust session.
    pub session_id: Option<String>,
}

impl Default for HermesConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11438".to_string(),
            model: "hermes-rust".to_string(),
            path: "/v1/chat/completions".to_string(),
            timeout_secs: 300,
            api_token: None,
            session_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types (OpenAI-compatible)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OpenAIRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAIRequestMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// We always run a single-shot — keep `stream` off so the
    /// response is fully buffered. Hermes-Rust's SSE flow would
    /// require `AgentEvent::AssistantText` plumbing; that land in
    /// a follow-up PR so the first cut stays simple.
    stream: bool,
}

#[derive(Debug, Serialize)]
struct OpenAIRequestMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<OpenAIChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

/// Token usage returned by the model provider.
///
/// The OpenAI API returns this inside the top-level response.
/// We deserialize it here and forward it into `ChatResponse.metadata`
/// under the `"usage"` key so callers (including the audit logger)
/// can extract it without needing to know about the wire format.
#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    index: usize,
    message: OpenAIResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponseMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIErrorBody {
    #[serde(default)]
    error: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OpenAIModelsResponse {
    #[serde(default)]
    data: Vec<OpenAIModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModelEntry {
    id: String,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum HermesError {
    #[error("reqwest: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("hermes-rust status {status}: {body}")]
    Api { status: u16, body: String },
    #[error("hermes-rust parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("hermes-rust returned empty choices")]
    EmptyChoices,
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// A Hermes-Rust-backed [`ChatModel`]. Speaks Hermes-Rust's
/// OpenAI-compatible `/v1/chat/completions` endpoint.
#[derive(Clone)]
pub struct HermesModel {
    cfg: HermesConfig,
    http: reqwest::Client,
}

impl std::fmt::Debug for HermesModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HermesModel")
            .field("base_url", &self.cfg.base_url)
            .field("model", &self.cfg.model)
            .field("path", &self.cfg.path)
            .finish()
    }
}

impl HermesModel {
    /// Construct a new Hermes-Rust adapter with the given config.
    pub fn new(cfg: HermesConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs))
            .build()
            .expect("HermesModel reqwest client");
        Self { cfg, http }
    }

    /// Convenience: connect to the default Hermes-Rust endpoint at
    /// `http://127.0.0.1:11438` with no auth.
    pub fn default_local() -> Self {
        Self::new(HermesConfig::default())
    }

    /// Direct access to the config — for tests / inspection.
    pub fn config(&self) -> &HermesConfig {
        &self.cfg
    }

    /// Mutable access to the API token. Operators can rotate the
    /// token at runtime without rebuilding the `reqwest::Client`.
    pub fn set_api_token(&mut self, token: Option<String>) {
        self.cfg.api_token = token;
    }

    /// Mutable access to the session id. Hermes-Rust returns a
    /// `session_id` on each `/v1/chat` turn; passing it back on the
    /// next call keeps the conversation history on the Hermes-Rust
    /// side.
    pub fn set_session_id(&mut self, id: Option<String>) {
        self.cfg.session_id = id;
    }

    /// Pull the session id from the latest response (only populated
    /// when using the `/v1/chat` endpoint, which is not the default).
    pub fn session_id(&self) -> Option<&str> {
        self.cfg.session_id.as_deref()
    }

    /// Build the OpenAI-shaped request body from a [`ChatRequest`].
    ///
    /// Public for the unit tests in this crate — the production
    /// path uses [`Self::complete`].
    pub fn build_request<'a>(&'a self, req: &'a ChatRequest) -> OpenAIRequest<'a> {
        let model = req
            .params
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.cfg.model);

        let temperature = req
            .params
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);

        let max_tokens = req
            .params
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let messages = req
            .messages
            .iter()
            .map(|m: &ChatMessage| OpenAIRequestMessage {
                role: role_to_str(m.role).to_string(),
                content: m.content.clone(),
                name: m.name.clone(),
            })
            .collect();

        OpenAIRequest {
            model,
            messages,
            temperature,
            max_tokens,
            stream: false,
        }
    }

    /// Parse the OpenAI-shaped response into A3Net's [`ChatResponse`].
    pub fn parse_response(&self, raw: OpenAIResponse) -> ChatResponse {
        let choice = raw.choices.into_iter().next();
        let (content, finish_reason) = match choice {
            Some(c) => (c.message.content, c.finish_reason),
            None => (String::new(), Some("stop".to_string())),
        };

        let finish = match finish_reason.as_deref() {
            Some("tool_calls") | Some("function_call") => FinishReason::ToolCalls,
            Some("length") => FinishReason::Length,
            Some("content_filter") => FinishReason::ContentFilter,
            Some("stop") | None => FinishReason::Stop,
            Some(_) => FinishReason::Other,
        };

        let mut metadata = BTreeMap::new();
        metadata.insert(
            "provider".to_string(),
            serde_json::json!("hermes-rust"),
        );
        if !raw.model.is_empty() {
            metadata.insert("model".to_string(), serde_json::json!(raw.model));
        }
        if !raw.id.is_empty() {
            metadata.insert("response_id".to_string(), serde_json::json!(raw.id));
        }
        // Forward token usage into metadata so audit loggers and callers
        // can extract it without needing to know the wire format.
        if let Some(usage) = raw.usage {
            let usage_map = serde_json::json!({
                "promptTokens": usage.prompt_tokens,
                "completionTokens": usage.completion_tokens,
                "totalTokens": usage.total_tokens,
            });
            metadata.insert("usage".to_string(), usage_map);
        }

        ChatResponse {
            content,
            // Hermes-Rust executes tools server-side; the response
            // never carries `tool_calls` today. We surface the empty
            // slice so the Agent loop simply terminates.
            tool_calls: Vec::<ToolCall>::new(),
            finish,
            metadata,
        }
    }

    /// Quick readiness probe — `GET /v1/models`. Returns the list of
    /// model ids the server advertises (Hermes-Rust typically returns
    /// just `["hermes-rust"]`).
    pub async fn list_models(&self) -> Result<Vec<String>, HermesError> {
        let url = format!("{}/v1/models", self.cfg.base_url);
        let req = self.http.get(&url);
        let req = self.apply_auth(req);
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(HermesError::Api { status: status.as_u16(), body });
        }
        let parsed: OpenAIModelsResponse = serde_json::from_str(&body)?;
        Ok(parsed.data.into_iter().map(|m| m.id).collect())
    }

    /// `GET /health` — returns the raw body so the caller can decide
    /// what version / status codes count as "ready".
    pub async fn health(&self) -> Result<String, HermesError> {
        let url = format!("{}/health", self.cfg.base_url);
        let req = self.http.get(&url);
        let req = self.apply_auth(req);
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(HermesError::Api { status: status.as_u16(), body });
        }
        Ok(body)
    }

    fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        let Some(token) = self.cfg.api_token.as_deref() else {
            return builder;
        };
        let mut b = builder;
        b = b.header("Authorization", format!("Bearer {token}"));
        b = b.header("X-API-Key", token);
        b
    }
}

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

#[async_trait]
impl ChatModel for HermesModel {
    fn model_id(&self) -> &str {
        &self.cfg.model
    }

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, AgentError> {
        let url = format!("{}{}", self.cfg.base_url, self.cfg.path);
        let body = self.build_request(&req);

        let http_req = self.http.post(&url).json(&body);
        let http_req = self.apply_auth(http_req);
        let http_req = if let Some(ref sid) = self.cfg.session_id {
            http_req.header("X-Session-Id", sid)
        } else {
            http_req
        };

        let resp = http_req
            .send()
            .await
            .map_err(|e| AgentError::ChatModel(format!("hermes-rust network: {e}")))?;

        let status = resp.status();
        let raw = resp
            .text()
            .await
            .map_err(|e| AgentError::ChatModel(format!("hermes-rust body read: {e}")))?;

        if !status.is_success() {
            // Try to surface a structured error body if the server
            // gave us one, otherwise fall back to the raw text.
            let msg = match serde_json::from_str::<OpenAIErrorBody>(&raw) {
                Ok(parsed) => parsed.error.to_string(),
                Err(_) => raw.clone(),
            };
            return Err(AgentError::ChatModel(format!(
                "hermes-rust {status}: {msg}"
            )));
        }

        let parsed: OpenAIResponse = serde_json::from_str(&raw)
            .map_err(|e| AgentError::ChatModel(format!("hermes-rust parse: {e}")))?;

        if parsed.choices.is_empty() {
            return Err(AgentError::ChatModel(
                "hermes-rust: empty choices".into(),
            ));
        }

        Ok(self.parse_response(parsed))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatMessage;

    fn make_req() -> ChatRequest {
        ChatRequest {
            messages: vec![
                ChatMessage::system("You are helpful."),
                ChatMessage::user("Hi"),
            ],
            tools: vec![],
            params: BTreeMap::new(),
        }
    }

    #[test]
    fn default_base_url_is_11438() {
        let cfg = HermesConfig::default();
        assert_eq!(cfg.base_url, "http://127.0.0.1:11438");
        assert_eq!(cfg.model, "hermes-rust");
        assert_eq!(cfg.path, "/v1/chat/completions");
        assert!(cfg.api_token.is_none());
        assert!(cfg.session_id.is_none());
    }

    #[test]
    fn model_id_reflects_config() {
        let m = HermesModel::new(HermesConfig {
            model: "hermes-rust-custom".into(),
            ..Default::default()
        });
        assert_eq!(m.model_id(), "hermes-rust-custom");
    }

    #[test]
    fn build_request_uses_default_model() {
        let m = HermesModel::new(HermesConfig::default());
        let req = make_req();
        let built = m.build_request(&req);
        assert_eq!(built.model, "hermes-rust");
        assert_eq!(built.messages.len(), 2);
        assert_eq!(built.messages[0].role, "system");
        assert_eq!(built.messages[1].role, "user");
        assert!(built.stream == false);
    }

    #[test]
    fn build_request_params_override_default_model() {
        let mut req = make_req();
        req.params.insert("model".to_string(), serde_json::json!("custom-llm"));
        req.params
            .insert("temperature".to_string(), serde_json::json!(0.42));
        req.params
            .insert("max_tokens".to_string(), serde_json::json!(256));
        let m = HermesModel::new(HermesConfig::default());
        let built = m.build_request(&req);
        assert_eq!(built.model, "custom-llm");
        assert_eq!(built.temperature, Some(0.42));
        assert_eq!(built.max_tokens, Some(256));
    }

    #[test]
    fn parse_response_text() {
        let m = HermesModel::new(HermesConfig::default());
        let raw = OpenAIResponse {
            id: "chatcmpl-1".into(),
            model: "hermes-rust".into(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: OpenAIResponseMessage {
                    role: "assistant".into(),
                    content: "Hello!".into(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: None,
        };
        let resp = m.parse_response(raw);
        assert_eq!(resp.content, "Hello!");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.finish, FinishReason::Stop);
        assert_eq!(
            resp.metadata.get("provider").map(|v| v.as_str()),
            Some(Some("hermes-rust"))
        );
    }

    #[test]
    fn parse_response_includes_usage_metadata() {
        let m = HermesModel::new(HermesConfig::default());
        let raw = OpenAIResponse {
            id: "chatcmpl-2".into(),
            model: "hermes-rust".into(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: OpenAIResponseMessage {
                    role: "assistant".into(),
                    content: "Hi there!".into(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(OpenAIUsage {
                prompt_tokens: Some(42),
                completion_tokens: Some(18),
                total_tokens: Some(60),
            }),
        };
        let resp = m.parse_response(raw);
        let usage = resp.metadata.get("usage").expect("usage key must be present");
        assert_eq!(usage.get("promptTokens").and_then(|v| v.as_u64()), Some(42));
        assert_eq!(usage.get("completionTokens").and_then(|v| v.as_u64()), Some(18));
        assert_eq!(usage.get("totalTokens").and_then(|v| v.as_u64()), Some(60));
    }

    #[test]
    fn parse_response_length_finish() {
        let m = HermesModel::new(HermesConfig::default());
        let raw = OpenAIResponse {
            id: "".into(),
            model: "hermes-rust".into(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: OpenAIResponseMessage {
                    role: "assistant".into(),
                    content: "…".into(),
                },
                finish_reason: Some("length".into()),
            }],
        };
        let resp = m.parse_response(raw);
        assert_eq!(resp.finish, FinishReason::Length);
    }

    #[test]
    fn parse_response_empty_choices_does_not_panic() {
        let m = HermesModel::new(HermesConfig::default());
        let raw = OpenAIResponse {
            id: "".into(),
            model: "hermes-rust".into(),
            choices: vec![],
        };
        // parse_response picks a default; the empty choices guard
        // lives in `complete` so we don't error here.
        let resp = m.parse_response(raw);
        assert_eq!(resp.content, "");
        assert_eq!(resp.finish, FinishReason::Stop);
    }

    #[test]
    fn clone_preserves_config() {
        let m = HermesModel::new(HermesConfig {
            base_url: "http://localhost:11438".into(),
            api_token: Some("secret".into()),
            session_id: Some("s-1".into()),
            ..Default::default()
        });
        let m2 = m.clone();
        assert_eq!(m.model_id(), m2.model_id());
        assert_eq!(m.config().session_id, m2.config().session_id);
        assert_eq!(m.config().api_token, m2.config().api_token);
    }

    #[test]
    fn set_auth_and_session_mutators() {
        let mut m = HermesModel::new(HermesConfig::default());
        assert!(m.config().api_token.is_none());
        m.set_api_token(Some("abc".into()));
        assert_eq!(m.config().api_token.as_deref(), Some("abc"));
        m.set_api_token(None);
        assert!(m.config().api_token.is_none());

        m.set_session_id(Some("s-9".into()));
        assert_eq!(m.session_id(), Some("s-9"));
        m.set_session_id(None);
        assert!(m.session_id().is_none());
    }
}
