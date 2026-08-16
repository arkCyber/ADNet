//! Allama inference server adapter — Ollama-compatible API at `localhost:11435`.
//!
//! Allama is a local inference server that speaks the same
//! wire protocol as Ollama (`/api/chat` with tool-calling).
//! This adapter is a thin wrapper around [`super::ollama::OllamaModel`]
//! with the default base URL pointing to `http://127.0.0.1:11435`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use super::ollama::{OllamaError, OllamaModel};
use crate::chat::{ChatModel, ChatRequest, ChatResponse};
use crate::error::AgentError;

/// Configuration for [`AllamaModel`].
#[derive(Debug, Clone)]
pub struct AllamaConfig {
    /// Full base URL of the Allama server.
    /// Defaults to `http://127.0.0.1:11435`.
    pub base_url: String,
    /// Default model name. Defaults to `qwen3`.
    pub model: String,
    /// Request timeout in seconds. Defaults to 120 s.
    pub timeout_secs: u64,
}

impl Default for AllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11435".to_string(),
            model: "qwen3".to_string(),
            timeout_secs: 120,
        }
    }
}

/// An Allama-backed [`ChatModel`]. Structurally identical to
/// [`OllamaModel`] but defaults to port 11435.
///
/// # Example
///
/// ```ignore
/// use a3net_agent::providers::allama::AllamaModel;
///
/// let model = AllamaModel::new(AllamaConfig {
///     model: "qwen3".into(),
///     timeout_secs: 180,
///     ..Default::default()
/// });
/// node.register_agent(std::sync::Arc::new(model));
/// ```
#[derive(Clone)]
pub struct AllamaModel {
    inner: OllamaModel,
}

impl std::fmt::Debug for AllamaModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllamaModel")
            .field("model", &self.inner.model_id())
            .finish()
    }
}

impl AllamaModel {
    /// Construct a new Allama adapter.
    pub fn new(cfg: AllamaConfig) -> Self {
        let inner = OllamaModel::new(super::ollama::OllamaConfig {
            base_url: cfg.base_url,
            model: cfg.model,
            timeout_secs: cfg.timeout_secs,
        });
        Self { inner }
    }

    /// Base URL the model is connecting to.
    pub fn base_url(&self) -> &str {
        &self.inner.cfg.base_url
    }
}

#[async_trait]
impl ChatModel for AllamaModel {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, AgentError> {
        self.inner.complete(req).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_base_url_is_11435() {
        let cfg = AllamaConfig::default();
        assert_eq!(cfg.base_url, "http://127.0.0.1:11435");
        assert_eq!(cfg.model, "qwen3");
    }

    #[test]
    fn model_id_reflects_config() {
        let m = AllamaModel::new(AllamaConfig {
            model: "mistral".into(),
            ..Default::default()
        });
        assert_eq!(m.model_id(), "mistral");
    }

    #[test]
    fn clone_preserves_inner() {
        let m = AllamaModel::new(AllamaConfig::default());
        let m2 = m.clone();
        assert_eq!(m.model_id(), m2.model_id());
    }
}
