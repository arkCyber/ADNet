//! Error types for the agent layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("chat model error: {0}")]
    ChatModel(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("tool not registered: {0}")]
    UnknownTool(String),

    #[error("tool call parse error: {0}")]
    ToolParse(String),

    #[error("max steps reached ({0}); the agent did not converge")]
    MaxSteps(u32),

    #[error("agent cancelled")]
    Cancelled,

    #[error("agent internal: {0}")]
    Internal(String),
}

pub type AgentResult<T> = std::result::Result<T, AgentError>;
