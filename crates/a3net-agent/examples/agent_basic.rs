//! Minimal a3net-agent example.
//!
//! Builds a `MockChatModel` that returns a single canned text
//! answer, registers no tools, drives the agent loop, and prints
//! the final assistant message + the recorded conversation history.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p a3net-agent --example agent_basic
//! ```

use a3net_agent::{
    Agent, AgentEvent, ChatModel, RunOptions, ToolRegistry,
    mock::MockChatModel,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model: Arc<dyn ChatModel> = Arc::new(MockChatModel::text(
        "Hello, human! I am an A3Net agent running on a mock model.",
    ));
    let registry = ToolRegistry::new();

    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_system("You are a concise A3Net assistant.");
    handle.push_user("Greet me and tell me your model id.");

    let (resp, mut rx) = handle.run(RunOptions::default()).await?;
    println!("== Final response ==");
    println!("content: {}", resp.content);
    println!("finish : {:?}", resp.finish);

    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::AssistantText(text) => println!("[assistant] {text}"),
            AgentEvent::ToolInvoked { name, .. } => println!("[tool] {name}"),
            AgentEvent::Done { final_text, steps } => {
                println!("[done] steps={steps} text={final_text}");
            }
            AgentEvent::Aborted { reason } => println!("[aborted] {reason}"),
        }
    }

    println!("\n== History ==");
    for m in handle.history() {
        let role = format!("{:?}", m.role);
        println!("{role:>9} : {}", m.content);
    }

    Ok(())
}
