//! In-memory mocks useful for unit tests. These never touch the
//! network — the host plugs them in to drive deterministic
//! tests of the agent loop.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::chat::{ChatMessage, ChatModel, ChatRequest, ChatResponse, FinishReason, Role, ToolCall};
use crate::error::AgentError;
use crate::tool::{Tool, ToolContext, ToolDescriptor, ToolError, ToolResult};

/// Deterministic chat model that returns the same canned
/// response (with optional pre-scripted tool-call turns).
pub struct MockChatModel {
    id: String,
    script: Mutex<Vec<ChatResponse>>,
}

impl MockChatModel {
    /// Single-turn text answer.
    pub fn text(answer: impl Into<String>) -> Self {
        Self {
            id: "mock-text/1".to_string(),
            script: Mutex::new(vec![ChatResponse::text(answer)]),
        }
    }

    /// Multi-turn scripted responses. The first call returns
    /// the first response, and so on.
    pub fn scripted(id: impl Into<String>, responses: Vec<ChatResponse>) -> Self {
        Self {
            id: id.into(),
            script: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl ChatModel for MockChatModel {
    fn model_id(&self) -> &str {
        &self.id
    }
    async fn complete(&self, _req: ChatRequest) -> Result<ChatResponse, AgentError> {
        let mut g = self.script.lock().unwrap();
        if g.is_empty() {
            return Ok(ChatResponse::text(""));
        }
        Ok(g.remove(0))
    }
}

/// Test tool that echoes its arguments back as a JSON value.
pub struct MockTool {
    name: String,
    desc: String,
}

impl MockTool {
    pub fn echo(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            desc: "Echoes the arguments back as JSON.".to_string(),
        }
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name.clone(),
            description: Some(self.desc.clone()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            }),
        }
    }
    async fn invoke(&self, args: serde_json::Value, _ctx: ToolContext) -> ToolResult {
        Ok(args)
    }
}

// Suppress unused warnings for items that are only used in
// tests / doctests.
#[allow(dead_code)]
fn _unused(_: &ChatMessage, _: Role, _: &ToolCall) {}
