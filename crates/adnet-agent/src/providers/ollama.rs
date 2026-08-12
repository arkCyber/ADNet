//! Ollama [`ChatModel`] adapter — `http://localhost:11434/api/chat`.
//!
//! Uses the native Ollama `/api/chat` endpoint for full
//! tool-calling shape.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chat::{ChatMessage, ChatModel, ChatRequest, ChatResponse, FinishReason, ToolCall};
use crate::error::AgentError;

/// Configuration for [`OllamaModel`].
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    /// Ollama server base URL. Defaults to `http://127.0.0.1:11434`.
    pub base_url: String,
    /// Default model name. Defaults to `qwen3`.
    pub model: String,
    /// Per-request timeout in seconds. Defaults to 120 s.
    pub timeout_secs: u64,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434".to_string(),
            model: "qwen3".to_string(),
            timeout_secs: 120,
        }
    }
}

// ---------------------------------------------------------------------------
// Ollama wire types (fully owned)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OllamaTool>>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCallWire>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct OllamaToolCallWire {
    #[serde(rename = "function")]
    function: OllamaFnWire,
}

#[derive(Debug, Serialize)]
struct OllamaFnWire {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    ty: String,
    function: OllamaFnDef,
}

#[derive(Debug, Serialize)]
struct OllamaFnDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    model: String,
    message: OllamaRespMsg,
    done_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaRespMsg {
    role: String,
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCallResp>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCallResp {
    id: String,
    #[serde(rename = "type")]
    ty: String,
    function: OllamaFnResp,
}

#[derive(Debug, Deserialize)]
struct OllamaFnResp {
    name: String,
    arguments: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum OllamaError {
    #[error("reqwest: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("ollama status {status}: {body}")]
    Api { status: u16, body: String },
    #[error("ollama parse: {0}")]
    Parse(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// An Ollama-backed [`ChatModel`]. Implements tool-calling over
/// `http://localhost:11434/api/chat`.
#[derive(Clone)]
pub struct OllamaModel {
    pub(crate) cfg: OllamaConfig,
    http: reqwest::Client,
}

impl std::fmt::Debug for OllamaModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OllamaModel")
            .field("cfg", &self.cfg)
            .finish()
    }
}

impl OllamaModel {
    /// Construct a new Ollama adapter.
    pub fn new(cfg: OllamaConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs))
            .build()
            .expect("OllamaModel reqwest client");
        Self { cfg, http }
    }

    fn resolve_model(&self, req: &ChatRequest) -> String {
        req.params
            .get("model")
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| self.cfg.model.clone())
    }

    fn build_request(&self, req: &ChatRequest) -> Result<OllamaRequest, OllamaError> {
        let model = self.resolve_model(req).to_string();

        let messages: Vec<OllamaMessage> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    crate::chat::Role::System => "system",
                    crate::chat::Role::User => "user",
                    crate::chat::Role::Assistant => "assistant",
                    crate::chat::Role::Tool => "tool",
                }
                .to_string();

                let tool_calls: Option<Vec<OllamaToolCallWire>> = if m.tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        m.tool_calls
                            .iter()
                            .map(|tc| OllamaToolCallWire {
                                function: OllamaFnWire {
                                    name: tc.name.clone(),
                                    arguments: serde_json::to_string(&tc.arguments)
                                        .unwrap_or_else(|_| "{}".into()),
                                },
                            })
                            .collect(),
                    )
                };

                OllamaMessage {
                    role,
                    content: m.content.clone(),
                    tool_calls,
                    tool_call_id: m.tool_call_id.clone(),
                    name: m.name.clone(),
                }
            })
            .collect();

        let tools: Option<Vec<OllamaTool>> = if req.tools.is_empty() {
            None
        } else {
            Some(
                req.tools
                    .iter()
                    .map(|t| OllamaTool {
                        ty: "function".to_string(),
                        function: OllamaFnDef {
                            name: t.name.clone(),
                            description: t.description.clone().unwrap_or_default(),
                            parameters: t.parameters.clone(),
                        },
                    })
                    .collect(),
            )
        };

        Ok(OllamaRequest {
            model,
            messages,
            tools,
            stream: false,
        })
    }

    fn parse_response(&self, raw: OllamaResponse) -> ChatResponse {
        let tool_calls: Vec<ToolCall> = raw
            .message
            .tool_calls
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect();

        let finish = if !tool_calls.is_empty() {
            FinishReason::ToolCalls
        } else {
            match raw.done_reason.as_deref() {
                Some("stop") | None => FinishReason::Stop,
                Some("length") => FinishReason::Length,
                Some("content_filter") => FinishReason::ContentFilter,
                _ => FinishReason::Other,
            }
        };

        let mut metadata = BTreeMap::new();
        metadata.insert("model".to_string(), serde_json::json!(raw.model));

        ChatResponse {
            content: raw.message.content,
            tool_calls,
            finish,
            metadata,
        }
    }
}

#[async_trait]
impl ChatModel for OllamaModel {
    fn model_id(&self) -> &str {
        &self.cfg.model
    }

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, AgentError> {
        let ollama_req = self
            .build_request(&req)
            .map_err(|e| AgentError::ChatModel(e.to_string()))?;

        let url = format!("{}/api/chat", self.cfg.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&ollama_req)
            .send()
            .await
            .map_err(|e| AgentError::ChatModel(format!("ollama network: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AgentError::ChatModel(format!("ollama body read: {e}")))?;

        if !status.is_success() {
            return Err(AgentError::ChatModel(format!(
                "ollama {}: {}",
                status, body
            )));
        }

        let ollama_resp: OllamaResponse = serde_json::from_str(&body)
            .map_err(|e| AgentError::ChatModel(format!("ollama parse: {e}")))?;

        Ok(self.parse_response(ollama_resp))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_req() -> ChatRequest {
        ChatRequest {
            messages: vec![
                crate::chat::ChatMessage::system("You are helpful."),
                crate::chat::ChatMessage::user("Hi"),
            ],
            tools: vec![],
            params: BTreeMap::new(),
        }
    }

    fn make_tool() -> crate::tool::ToolDescriptor {
        crate::tool::ToolDescriptor {
            name: "get_weather".into(),
            description: Some("Get weather".into()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
        }
    }

    #[test]
    fn build_request_plain() {
        let m = OllamaModel::new(OllamaConfig::default());
        let req = make_req();
        let built = m.build_request(&req).unwrap();
        assert_eq!(built.model, "qwen3");
        assert_eq!(built.messages.len(), 2);
        assert!(built.tools.is_none());
    }

    #[test]
    fn build_request_with_tool() {
        let m = OllamaModel::new(OllamaConfig::default());
        let mut req = make_req();
        req.tools.push(make_tool());
        let built = m.build_request(&req).unwrap();
        let tools = built.tools.unwrap();
        assert_eq!(tools[0].function.name, "get_weather");
    }

    #[test]
    fn model_id_reflects_config() {
        let m = OllamaModel::new(OllamaConfig {
            model: "llama3.1".into(),
            ..Default::default()
        });
        assert_eq!(m.model_id(), "llama3.1");
    }

    #[test]
    fn parse_tool_response() {
        let m = OllamaModel::new(OllamaConfig::default());
        let raw = OllamaResponse {
            model: "qwen3".into(),
            message: OllamaRespMsg {
                role: "assistant".into(),
                content: "".into(),
                tool_calls: vec![OllamaToolCallResp {
                    id: "call_1".into(),
                    ty: "function".into(),
                    function: OllamaFnResp {
                        name: "get_weather".into(),
                        arguments: serde_json::json!({"city": "Tokyo"}),
                    },
                }],
            },
            done_reason: Some("stop".into()),
        };
        let resp = m.parse_response(raw);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "get_weather");
        assert_eq!(resp.finish, FinishReason::ToolCalls);
    }

    #[test]
    fn parse_text_response() {
        let m = OllamaModel::new(OllamaConfig::default());
        let raw = OllamaResponse {
            model: "qwen3".into(),
            message: OllamaRespMsg {
                role: "assistant".into(),
                content: "Hello!".into(),
                tool_calls: vec![],
            },
            done_reason: Some("stop".into()),
        };
        let resp = m.parse_response(raw);
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.finish, FinishReason::Stop);
    }
}
