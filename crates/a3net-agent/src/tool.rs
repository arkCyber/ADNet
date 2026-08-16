//! Tool abstraction — every function the agent may call.
//!
//! A `Tool` is anything that can be invoked with a JSON-args
//! blob and return a JSON value (or an error). The registry
//! turns these into the JSON-Schema descriptors the LLM sees
//! during a chat-completion.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::AgentResult;

/// Per-invocation context passed into [`Tool::invoke`].
/// Carries anything the tool might want beyond raw arguments:
/// the calling node id, capability hints, request id, etc.
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    /// Identifier of the A3Net node the call originated from.
    pub node_id: Option<String>,
    /// Opaque call id (request-tracing correlator).
    pub request_id: Option<String>,
    /// Optional user-supplied tag (e.g. peer `NodeId`).
    pub caller: Option<String>,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool invocation failed: {0}")]
    Failed(String),
    #[error("invalid arguments: {0}")]
    BadArgs(String),
    #[error("tool denied access: {0}")]
    Denied(String),
}

pub type ToolResult = Result<serde_json::Value, ToolError>;

/// JSON-Schema-style descriptor for a single tool. Built by the
/// registry and shipped verbatim to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON-Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// The trait every A3Net-callable tool implements.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Stable name used by the LLM and the registry.
    fn name(&self) -> &str;

    /// Build the JSON-Schema descriptor; called once when the
    /// tool is registered. Keep it cheap; the result is cached.
    fn descriptor(&self) -> ToolDescriptor;

    /// Invoke the tool. Implementations MUST be cancellable
    /// through standard tokio mechanisms (cancellation tokens
    /// or `select!` over `ctx.cancellation`).
    async fn invoke(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> ToolResult;
}

/// Convenience: boxed [`Tool`].
pub type BoxedTool = Arc<dyn Tool>;
