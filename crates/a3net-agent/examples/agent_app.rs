//! App-level a3net-agent example: scripted multi-turn agent.
//!
//! Demonstrates how a host can drive a complete "agent loop" with
//! pre-recorded (`MockChatModel`) responses:
//!
//!  1. The model emits a tool call asking for the current time;
//!  2. The agent runs the `TimeTool` and feeds the result back;
//!  3. The model emits a final text answer and stops.
//!
//! No network is touched. This is the canonical "glue layer" used
//! to test agent integrations in CI.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-agent --example agent_app
//! ```

use a3net_agent::{
    Agent, AgentEvent, ChatModel, ChatResponse, FinishReason, RunOptions, Tool, ToolContext,
    ToolDescriptor, ToolRegistry, ToolResult, mock::MockChatModel,
};
use async_trait::async_trait;
use std::sync::Arc;

struct TimeTool;

#[async_trait]
impl Tool for TimeTool {
    fn name(&self) -> &str {
        "now"
    }
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "now".into(),
            description: Some("Return current unix timestamp in seconds.".into()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
            }),
        }
    }
    async fn invoke(&self, _args: serde_json::Value, _ctx: ToolContext) -> ToolResult {
        Ok(serde_json::json!(chrono::Utc::now().timestamp()))
    }
}

fn scripted_chat_model() -> impl ChatModel {
    MockChatModel::scripted(
        "mock-scripted/1",
        vec![
            // Step 1: model asks for the time.
            ChatResponse {
                content: String::new(),
                tool_calls: vec![a3net_agent::ToolCall {
                    id: "call_time_1".into(),
                    name: "now".into(),
                    arguments: serde_json::json!({}),
                }],
                finish: FinishReason::ToolCalls,
                metadata: Default::default(),
            },
            // Step 2: model uses the timestamp to give a final answer.
            ChatResponse::text("It is now defined in nanoseconds by the unix clock. 💡"),
        ],
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model: Arc<dyn ChatModel> = Arc::new(scripted_chat_model());
    let registry = ToolRegistry::new();
    registry.register(TimeTool);

    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_system("You are a polite assistant. Use the `now` tool when asked.");
    handle.push_user("What time is it?");

    let (resp, mut rx) = handle.run(RunOptions::default()).await?;
    println!("== Final response ==");
    println!("content: {}", resp.content);
    println!("finish : {:?}", resp.finish);

    let mut tool_invocations = 0;
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::AssistantText(_) => {}
            AgentEvent::ToolInvoked {
                call_id,
                name,
                result,
                error,
            } => {
                tool_invocations += 1;
                println!("[tool] {call_id} {name} -> {result:?} (error: {error:?})");
            }
            AgentEvent::Done { steps, .. } => println!("[done] steps={steps}"),
            AgentEvent::Aborted { reason } => println!("[aborted] {reason}"),
        }
    }
    assert_eq!(tool_invocations, 1, "expected exactly one tool call");

    println!("\n== Recorded history ==");
    for m in handle.history() {
        let role = format!("{:?}", m.role);
        let extra = if m.tool_calls.is_empty() {
            String::new()
        } else {
            format!(" [tool_calls={}]", m.tool_calls.len())
        };
        let id = m
            .tool_call_id
            .as_ref()
            .map(|id| format!(" (id={id})"))
            .unwrap_or_default();
        println!("{role:>9}: {}{extra}{id}", m.content);
    }

    Ok(())
}
