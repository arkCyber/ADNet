//! LLM-provider-agnostic chat types.
//!
//! The shapes are deliberately minimal: a `ChatModel` is anything
//! that takes a [`ChatRequest`] and returns a [`ChatResponse`].
//! Tool-calling is modelled as an open conversation: the model
//! may emit [`ToolCall`]s, the host executes them via
//! [`crate::tool::Tool`], and feeds the [`ToolCallResult`]s back
//! in the next request.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Role of a message in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single chat message. The `payload` is intentionally opaque
/// to allow providers that mix text / images / audio inside one
/// message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    /// Free-form content; usually UTF-8 text but providers may
    /// accept richer payloads (e.g. multipart).
    #[serde(default)]
    pub content: String,
    /// Tool calls the model wants executed before the next
    /// message in the conversation continues.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set when `role == Tool`; identifies which call this
    /// message fulfils.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional author tag (e.g. `"node-abc"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            name: None,
        }
    }
}

/// A model-emitted request to invoke a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Opaque, provider-assigned id. The agent uses it to route
    /// the [`ToolCallResult`] back to the correct call.
    pub id: String,
    /// Tool name as registered in [`crate::ToolRegistry`].
    pub name: String,
    /// JSON arguments. Parsing is the tool's responsibility.
    pub arguments: serde_json::Value,
}

/// Tool-execution result. Sent back to the model in the next
/// [`ChatRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub call_id: String,
    /// Either a JSON value (when the tool was successful) or a
    /// human-readable error string. Empty `value` + `Some(error)`
    /// marks a failure path; the [`Role::Tool`] message will
    /// surface that error to the model.
    #[serde(default)]
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolCallResult {
    pub fn ok(call_id: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            call_id: call_id.into(),
            value,
            error: None,
        }
    }
    pub fn err(call_id: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            value: serde_json::Value::Null,
            error: Some(msg.into()),
        }
    }
}

/// Single chat-completion request to a [`ChatModel`].
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// Conversation so far. The host is expected to manage the
    /// growing history; the model only sees what's in this slice.
    pub messages: Vec<ChatMessage>,
    /// Tool schemas the model may choose to call. Built from
    /// [`crate::ToolRegistry`].
    pub tools: Vec<crate::tool::ToolDescriptor>,
    /// Optional model-side knobs (temperature, max_tokens, …).
    /// Carried opaquely so the provider can interpret them.
    pub params: BTreeMap<String, serde_json::Value>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            params: BTreeMap::new(),
        }
    }
}

/// Reason the model returned without further tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Model emitted a final textual answer.
    Stop,
    /// Model emitted one or more tool calls.
    ToolCalls,
    /// Caller-side cap reached (length, time, etc).
    Length,
    /// Provider-specific content filter / safety.
    ContentFilter,
    /// Other provider-specific reason.
    Other,
}

/// Single chat-completion response.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The model's textual answer (may be empty if `tool_calls`
    /// is non-empty).
    pub content: String,
    /// Tool calls the model wants executed before the next step.
    pub tool_calls: Vec<ToolCall>,
    pub finish: FinishReason,
    /// Provider-emitted opaque metadata. Stored verbatim so the
    /// caller can correlate logs, costs, or trace ids.
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ChatResponse {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
            finish: FinishReason::Stop,
            metadata: BTreeMap::new(),
        }
    }
}

/// Provider-agnostic chat-completion model. Implemented by the
/// host to plug any LLM (OpenAI / Anthropic / local / …) into
/// the ADNet agent layer.
#[async_trait::async_trait]
pub trait ChatModel: Send + Sync {
    /// Stable identifier for diagnostics / cost accounting.
    fn model_id(&self) -> &str;

    /// Run a single chat-completion. Returns the model's
    /// response and may emit `tool_calls` which the agent
    /// layer will execute before issuing the next request.
    async fn complete(
        &self,
        request: ChatRequest,
    ) -> Result<ChatResponse, crate::AgentError>;
}
