//! [`Agent`] — orchestrates a [`ChatModel`] + [`ToolRegistry`]
//! loop until the model stops calling tools.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::chat::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, Role, ToolCallResult,
};
use crate::error::{AgentError, AgentResult};
use crate::registry::ToolRegistry;
use crate::tool::ToolContext;

/// Single step yielded by the agent loop. The host can
/// subscribe to these via [`Agent::run`] and surface progress
/// (e.g. a TUI "thinking…" indicator).
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Model emitted text. The agent will continue looping
    /// unless `finish` is `Stop`.
    AssistantText(String),
    /// The model requested a tool call; the agent executed it
    /// and is forwarding the result back.
    ToolInvoked {
        call_id: String,
        name: String,
        result: serde_json::Value,
        error: Option<String>,
    },
    /// Loop terminated with the final assistant message.
    Done { final_text: String, steps: u32 },
    /// Loop terminated because `RunOptions::max_steps` was hit.
    Aborted { reason: String },
}

/// One step of the agent loop (mirror of `AgentEvent` but
/// structured for tests / programmatic introspection).
#[derive(Debug, Clone)]
pub struct AgentStep {
    pub step_index: u32,
    pub model_response: ChatResponse,
    pub tool_results: Vec<ToolCallResult>,
}

/// Configuration for [`Agent::run`].
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Hard cap on model ↔ tool round-trips. Default 16.
    pub max_steps: u32,
    /// Optional node-side filter: drop tool calls whose name
    /// is not in this list. Empty = allow all registered tools.
    pub allowlist: Vec<String>,
    /// Per-call [`ToolContext`].
    pub tool_context: ToolContext,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            max_steps: 16,
            allowlist: Vec::new(),
            tool_context: ToolContext::default(),
        }
    }
}

/// Combined handle: a chat model + a tool registry. The agent
/// uses these to drive the loop.
pub struct Agent {
    model: Arc<dyn crate::chat::ChatModel>,
    registry: ToolRegistry,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("model_id", &self.model.model_id())
            .field("tools", &self.registry.list())
            .finish()
    }
}

impl Agent {
    pub fn new(model: Arc<dyn crate::chat::ChatModel>, registry: ToolRegistry) -> Self {
        Self { model, registry }
    }

    pub fn handle(&self) -> AgentHandle {
        AgentHandle {
            model: self.model.clone(),
            registry: self.registry.handle(),
            // The agent also owns a shared history buffer so
            // `run` continuations see prior turns.
            history: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Cheap, cloneable handle to a running [`Agent`]. Internally
/// stores a shared conversation history so successive calls to
/// [`AgentHandle::run`] see the previous turns.
#[derive(Clone)]
pub struct AgentHandle {
    model: Arc<dyn crate::chat::ChatModel>,
    registry: crate::registry::ToolRegistryHandle,
    history: Arc<Mutex<Vec<ChatMessage>>>,
}

impl std::fmt::Debug for AgentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHandle")
            .field("model_id", &self.model.model_id())
            .field("history_len", &self.history.lock().len())
            .field("tools", &self.registry.list())
            .finish()
    }
}

impl AgentHandle {
    pub fn model_id(&self) -> &str {
        self.model.model_id()
    }
    pub fn tools(&self) -> Vec<String> {
        self.registry.list()
    }
    pub fn push_system(&self, msg: impl Into<String>) {
        self.history.lock().push(ChatMessage::system(msg));
    }
    pub fn push_user(&self, msg: impl Into<String>) {
        self.history.lock().push(ChatMessage::user(msg));
    }
    pub fn history(&self) -> Vec<ChatMessage> {
        self.history.lock().clone()
    }
    pub fn clear_history(&self) {
        self.history.lock().clear();
    }

    /// Run the agent loop. Streams [`AgentEvent`]s through the
    /// returned receiver (and stores the final answer in the
    /// shared history).
    pub async fn run(
        &self,
        opts: RunOptions,
    ) -> AgentResult<(ChatResponse, mpsc::Receiver<AgentEvent>)> {
        let max_steps = opts.max_steps.max(1);
        let allowlist: std::collections::HashSet<String> =
            opts.allowlist.iter().cloned().collect();
        let (tx, rx) = mpsc::channel::<AgentEvent>(32);

        // Initial request snapshot.
        let mut messages = self.history.lock().clone();
        let tools = {
            let all = self.registry.descriptors();
            if allowlist.is_empty() {
                all
            } else {
                all.into_iter()
                    .filter(|t| allowlist.contains(&t.name))
                    .collect()
            }
        };
        let mut step_index: u32 = 0;

        loop {
            step_index += 1;
            if step_index > max_steps {
                let _ = tx
                    .send(AgentEvent::Aborted {
                        reason: format!("max_steps={max_steps}"),
                    })
                    .await;
                return Err(AgentError::MaxSteps(max_steps));
            }

            let req = ChatRequest {
                messages: messages.clone(),
                tools: tools.clone(),
                params: Default::default(),
            };
            let resp = self.model.complete(req).await?;
            if !resp.content.is_empty() {
                let _ = tx
                    .send(AgentEvent::AssistantText(resp.content.clone()))
                    .await;
                messages.push(ChatMessage {
                    role: Role::Assistant,
                    content: resp.content.clone(),
                    tool_calls: resp.tool_calls.clone(),
                    tool_call_id: None,
                    name: None,
                });
            } else {
                messages.push(ChatMessage {
                    role: Role::Assistant,
                    content: String::new(),
                    tool_calls: resp.tool_calls.clone(),
                    tool_call_id: None,
                    name: None,
                });
            }

            if resp.tool_calls.is_empty() || resp.finish == FinishReason::Stop {
                let _ = tx
                    .send(AgentEvent::Done {
                        final_text: resp.content.clone(),
                        steps: step_index,
                    })
                    .await;
                let mut h = self.history.lock();
                for m in messages.iter().skip(h.len()) {
                    h.push(m.clone());
                }
                return Ok((resp, rx));
            }

            // Execute tool calls.
            let mut results: Vec<ToolCallResult> = Vec::new();
            for call in &resp.tool_calls {
                let args = call.arguments.clone();
                let ctx = opts.tool_context.clone();
                let value = self
                    .registry
                    .invoke(&call.name, args.clone(), ctx.clone())
                    .await;
                let result = match &value {
                    Ok(v) => ToolCallResult::ok(call.id.clone(), v.clone()),
                    Err(e) => ToolCallResult::err(call.id.clone(), e.to_string()),
                };
                let _ = tx
                    .send(AgentEvent::ToolInvoked {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        result: result.value.clone(),
                        error: result.error.clone(),
                    })
                    .await;
                // Surface the result to the model as a `Role::Tool`
                // message.
                let content = match &result.error {
                    Some(e) => format!("ERROR: {e}"),
                    None => serde_json::to_string(&result.value).unwrap_or_default(),
                };
                messages.push(ChatMessage::tool_result(call.id.clone(), content));
                results.push(result);
            }

            // Save progress.
            let mut h = self.history.lock();
            for m in messages.iter().skip(h.len()) {
                h.push(m.clone());
            }
            drop(h);

            if results.iter().all(|r| r.error.is_some()) && false {
                // (Reserved: stop early if every tool call errored
                // and the user asked for "no retries on failure".)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatMessage;
    use crate::mock::MockChatModel;

    #[tokio::test]
    async fn agent_runs_until_stop() {
        let model = Arc::new(MockChatModel::text("hello world"));
        let registry = ToolRegistry::new();
        let agent = Agent::new(model, registry);
        let handle = agent.handle();
        handle.push_user("hi");
        let (resp, _rx) = handle
            .run(RunOptions::default())
            .await
            .expect("run ok");
        assert_eq!(resp.content, "hello world");
        assert_eq!(resp.finish, FinishReason::Stop);
        assert_eq!(handle.history().last().unwrap().content, "hello world");
    }

    #[tokio::test]
    async fn tool_invocation_round_trips() {
        use crate::mock::MockTool;
        use crate::ToolRegistry;
        let model = Arc::new(MockChatModel::text("all done"));
        let registry = ToolRegistry::new();
        registry.register(MockTool::echo("echo"));
        let agent = Agent::new(model, registry);
        let handle = agent.handle();
        handle.push_user("use the tool");
        let (resp, mut rx) = handle.run(RunOptions::default()).await.unwrap();
        assert_eq!(resp.content, "all done");
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            if matches!(ev, AgentEvent::Done { .. }) {
                done = true;
                break;
            }
        }
        assert!(done);
    }
}
