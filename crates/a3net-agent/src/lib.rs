//! `a3net-agent` — AI-agent integration seams for A3Net.
//!
//! A3Net does **not** ship an LLM client. The crate defines a
//! compact set of provider-agnostic traits so the host
//! application (which has its own AI agent product, per the
//! project notes) can plug any LLM / tool stack into a running
//! node without changing the workspace source.
//!
//! # Layering
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │ host LLM SDK (OpenAI / Anthropic / …)   │  ← your code
//! └─────────────────┬───────────────────────┘
//!                   │ implements
//! ┌─────────────────▼───────────────────────┐
//! │ a3net_agent::ChatModel                  │  ← this crate
//! │ a3net_agent::Tool                       │
//! │ a3net_agent::ToolRegistry               │
//! │ a3net_agent::Agent                      │
//! └─────────────────┬───────────────────────┘
//!                   │ registered with
//! ┌─────────────────▼───────────────────────┐
//! │ a3net_node::NodeAgentBridge             │  ← node interface
//! └─────────────────────────────────────────┘
//! ```
//!
//! # What this crate is NOT
//!
//! - It does **not** pull in `reqwest`, `openai`, `anthropic`,
//!   `rig`, `genai`, or any provider SDK. Adding one would
//!   double-compile time and version-lock users.
//! - It does **not** persist conversation state. Persistence
//!   is delegated to the host.
//! - It does **not** run any LLM. `Agent::run` is a thin
//!   orchestrator that delegates each step to a `ChatModel`
//!   + `ToolRegistry` pair.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod chat;
pub mod error;
pub mod tool;
pub mod registry;
pub mod agent;
pub mod mock;
pub mod providers;
#[cfg(feature = "audit")]
pub mod audit;
#[cfg(feature = "mail")]
pub mod mail_tool;

pub use chat::{
    ChatMessage, ChatModel, ChatRequest, ChatResponse, FinishReason, Role, ToolCall,
    ToolCallResult,
};
pub use error::{AgentError, AgentResult};
pub use tool::{Tool, ToolContext, ToolDescriptor, ToolError, ToolResult};
pub use registry::{ToolRegistry, ToolRegistryHandle};
pub use agent::{Agent, AgentEvent, AgentHandle, AgentStep, RunOptions};

pub use mock::{MockChatModel, MockTool};
pub use providers::{AllamaConfig, AllamaModel, OllamaModel};
#[cfg(feature = "audit")]
pub use audit::{AuditCtx, CallRecord, Outcome, TokenUsage, AGENT_AUDIT};
#[cfg(feature = "mail")]
pub use mail_tool::{
    MailAccountConfig, MailTool, TOOL_READ_INBOX, TOOL_SEND_EMAIL, TOOL_REPLY_EMAIL,
    TOOL_RESOLVE_PEER,
};
