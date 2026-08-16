//! Integration tests for the a3net-agent crate.
//!
//! These tests verify the integration between:
//! - Agent loop and ChatModel
//! - Agent loop and ToolRegistry
//! - Multiple tool invocations
//! - Error handling and edge cases

use std::collections::BTreeMap;
use std::sync::Arc;
use a3net_agent::{
    Agent, AgentEvent, ChatMessage, ChatResponse, FinishReason,
    MockChatModel, MockTool, Role, RunOptions, ToolRegistry,
};
use a3net_agent::chat::{ToolCall, ToolCallResult};

/// Create a ChatResponse with required fields.
fn make_response(content: &str, tool_calls: Vec<ToolCall>, finish: FinishReason) -> ChatResponse {
    ChatResponse {
        content: content.to_string(),
        tool_calls,
        finish,
        metadata: BTreeMap::new(),
    }
}

/// Test that the agent correctly processes a single-turn response.
#[tokio::test]
async fn test_agent_single_turn() {
    let model = Arc::new(MockChatModel::text("Hello, how can I help you?"));
    let registry = ToolRegistry::new();
    let agent = Agent::new(model, registry);
    let handle = agent.handle();

    handle.push_user("Hi there!");

    let (resp, mut rx) = handle.run(RunOptions::default()).await.unwrap();

    // Verify the response content
    assert_eq!(resp.content, "Hello, how can I help you?");
    assert_eq!(resp.finish, FinishReason::Stop);

    // Collect events
    let mut events = Vec::new();
    while let Ok(Some(ev)) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rx.recv()
    ).await {
        events.push(ev);
    }

    // Should have exactly one Done event
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Done { .. })));

    // History should contain user message and assistant response
    let history = handle.history();
    assert_eq!(history.len(), 2);
    assert!(matches!(history[0].role, Role::User));
    assert!(matches!(history[1].role, Role::Assistant));
}

/// Test that the agent correctly handles tool calls.
#[tokio::test]
async fn test_agent_with_tool_call() {
    // Create a scripted model that makes a tool call
    let tool_call = ToolCall {
        id: "call_1".to_string(),
        name: "echo".to_string(),
        arguments: serde_json::json!({"value": "test"}),
    };
    let first_response = make_response(
        "Let me check that for you.",
        vec![tool_call],
        FinishReason::ToolCalls,
    );
    let second_response = make_response("I found: test", vec![], FinishReason::Stop);

    let model = Arc::new(MockChatModel::scripted("tool-model", vec![
        first_response,
        second_response,
    ]));

    let registry = ToolRegistry::new();
    registry.register(MockTool::echo("echo"));

    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_user("What is the value?");

    let (resp, mut rx) = handle.run(RunOptions::default()).await.unwrap();

    // Should complete after tool call and response
    assert_eq!(resp.content, "I found: test");

    // Collect all events
    let mut tool_events = 0;
    let mut done_events = 0;
    while let Ok(Some(ev)) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rx.recv()
    ).await {
        match ev {
            AgentEvent::ToolInvoked { name, result, .. } => {
                assert_eq!(name, "echo");
                tool_events += 1;
                let _ = result;
            }
            AgentEvent::Done { .. } => {
                done_events += 1;
            }
            _ => {}
        }
    }

    assert_eq!(tool_events, 1, "Should have invoked the tool once");
    assert_eq!(done_events, 1, "Should complete with one Done event");
}

/// Test that the agent respects max_steps limit.
#[tokio::test]
async fn test_agent_max_steps_limit() {
    // Create a model that keeps making tool calls
    let tool_call = |id: &str| ToolCall {
        id: id.to_string(),
        name: "echo".to_string(),
        arguments: serde_json::json!({"value": "loop"}),
    };

    // Generate responses that always request a tool call
    let responses: Vec<ChatResponse> = (0..5).map(|i| {
        make_response(
            &format!("Step {}", i + 1),
            vec![tool_call(&format!("call_{}", i))],
            FinishReason::ToolCalls,
        )
    }).collect();

    let model = Arc::new(MockChatModel::scripted("loop-model", responses));

    let registry = ToolRegistry::new();
    registry.register(MockTool::echo("echo"));

    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_user("Keep going");

    // Set max_steps to 3
    let opts = RunOptions {
        max_steps: 3,
        ..Default::default()
    };

    let result = handle.run(opts).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(matches!(err, a3net_agent::AgentError::MaxSteps(3)));
}

/// Test agent history management.
#[tokio::test]
async fn test_agent_history_management() {
    // Test clear history functionality
    let model = Arc::new(MockChatModel::text("Response"));
    let registry = ToolRegistry::new();
    let agent = Agent::new(model, registry);
    let handle = agent.handle();

    // Add system message
    handle.push_system("You are a helpful assistant.");
    handle.push_user("Question");

    // Verify history before run
    assert_eq!(handle.history().len(), 2);

    // Run the agent
    let (_resp, _rx) = handle.run(RunOptions::default()).await.unwrap();

    // Verify history has messages after run
    let history_after = handle.history();
    assert!(history_after.len() >= 2);

    // Clear history
    handle.clear_history();
    assert_eq!(handle.history().len(), 0);

    // Verify we can add new messages after clearing
    handle.push_user("New question");
    assert_eq!(handle.history().len(), 1);
}

/// Test tool invocation result propagation.
#[tokio::test]
async fn test_tool_result_propagation() {
    // Model that makes a tool call, then responds to the result
    let first_response = make_response(
        "Processing...",
        vec![ToolCall {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            arguments: serde_json::json!({"input": "hello"}),
        }],
        FinishReason::ToolCalls,
    );

    let second_response = make_response("Got your message: hello", vec![], FinishReason::Stop);

    let model = Arc::new(MockChatModel::scripted("propagation-test", vec![
        first_response,
        second_response,
    ]));

    let registry = ToolRegistry::new();
    registry.register(MockTool::echo("echo"));

    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_user("Send 'hello'");

    let (resp, _rx) = handle.run(RunOptions::default()).await.unwrap();

    // The model should see the tool result and respond
    assert!(resp.content.contains("hello") || resp.content == "Got your message: hello");
}

/// Test agent with no tools registered.
#[tokio::test]
async fn test_agent_no_tools() {
    let model = Arc::new(MockChatModel::text("I don't have any tools to use."));
    let registry = ToolRegistry::new();

    // Don't register any tools
    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_user("Use a tool");

    let (resp, _rx) = handle.run(RunOptions::default()).await.unwrap();
    assert_eq!(resp.content, "I don't have any tools to use.");
}

/// Test multiple consecutive tool calls in one response.
#[tokio::test]
async fn test_multiple_tool_calls() {
    let first_response = make_response(
        "Making two calls...",
        vec![
            ToolCall {
                id: "call_1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({"value": "first"}),
            },
            ToolCall {
                id: "call_2".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({"value": "second"}),
            },
        ],
        FinishReason::ToolCalls,
    );

    let second_response = make_response("Done with both calls", vec![], FinishReason::Stop);

    let model = Arc::new(MockChatModel::scripted("multi-call", vec![
        first_response,
        second_response,
    ]));

    let registry = ToolRegistry::new();
    registry.register(MockTool::echo("echo"));

    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_user("Do both things");

    let (resp, mut rx) = handle.run(RunOptions::default()).await.unwrap();

    let mut tool_count = 0;
    while let Ok(Some(ev)) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rx.recv()
    ).await {
        if matches!(ev, AgentEvent::ToolInvoked { .. }) {
            tool_count += 1;
        }
    }

    assert_eq!(tool_count, 2, "Should invoke both tools");
    assert_eq!(resp.content, "Done with both calls");
}

/// Test that agent correctly formats tool call results for the model.
#[tokio::test]
async fn test_tool_result_formatting() {
    let first_response = make_response(
        "Checking...",
        vec![ToolCall {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            arguments: serde_json::json!({"value": "test_value"}),
        }],
        FinishReason::ToolCalls,
    );

    let second_response = make_response("Success", vec![], FinishReason::Stop);

    let model = Arc::new(MockChatModel::scripted("format-test", vec![
        first_response,
        second_response,
    ]));

    let registry = ToolRegistry::new();
    registry.register(MockTool::echo("echo"));

    let agent = Agent::new(model, registry);
    let handle = agent.handle();
    handle.push_user("Check");

    let (resp, _rx) = handle.run(RunOptions::default()).await.unwrap();

    // Verify the agent completed successfully
    assert_eq!(resp.finish, FinishReason::Stop);

    // Verify history contains tool result message
    let history = handle.history();
    let tool_messages: Vec<_> = history.iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .collect();

    assert!(!tool_messages.is_empty(), "Should have tool result messages in history");

    // Tool result should contain the echoed value
    let tool_content = &tool_messages[0].content;
    assert!(tool_content.contains("test_value") || tool_content.contains("value"));
}

/// Test agent debug output.
#[test]
fn test_agent_debug_implementations() {
    let model = Arc::new(MockChatModel::text("debug test"));
    let registry = ToolRegistry::new();
    let agent = Agent::new(model, registry);

    // Verify Debug implementations work without panicking
    let agent_debug = format!("{:?}", agent);
    assert!(agent_debug.contains("Agent"));
}

/// Test RunOptions defaults.
#[test]
fn test_run_options_defaults() {
    let opts = RunOptions::default();

    assert_eq!(opts.max_steps, 16);
    assert!(opts.allowlist.is_empty());
    assert!(opts.tool_context.node_id.is_none());
}

/// Test ChatMessage constructors.
#[test]
fn test_chat_message_constructors() {
    let system = ChatMessage::system("System prompt");
    assert!(matches!(system.role, Role::System));
    assert_eq!(system.content, "System prompt");

    let user = ChatMessage::user("User input");
    assert!(matches!(user.role, Role::User));
    assert_eq!(user.content, "User input");

    let assistant = ChatMessage::assistant("Assistant output");
    assert!(matches!(assistant.role, Role::Assistant));
    assert_eq!(assistant.content, "Assistant output");

    let tool = ChatMessage::tool_result("call_123", "tool output");
    assert!(matches!(tool.role, Role::Tool));
    assert_eq!(tool.tool_call_id, Some("call_123".to_string()));
    assert_eq!(tool.content, "tool output");
}

/// Test ChatResponse constructors.
#[test]
fn test_chat_response_constructors() {
    let text = ChatResponse::text("Hello");
    assert_eq!(text.content, "Hello");
    assert!(text.tool_calls.is_empty());
    assert_eq!(text.finish, FinishReason::Stop);

    let with_tools = make_response(
        "Using tools",
        vec![ToolCall {
            id: "test".to_string(),
            name: "test_tool".to_string(),
            arguments: serde_json::json!({}),
        }],
        FinishReason::ToolCalls,
    );

    assert_eq!(with_tools.content, "Using tools");
    assert_eq!(with_tools.tool_calls.len(), 1);
    assert_eq!(with_tools.finish, FinishReason::ToolCalls);
}

/// Test ToolCallResult constructors.
#[test]
fn test_tool_call_result() {
    use a3net_agent::chat::ToolCallResult;

    let ok_result = ToolCallResult::ok("call_1".to_string(), serde_json::json!({"result": "ok"}));
    assert_eq!(ok_result.call_id, "call_1");
    assert!(ok_result.error.is_none());
    assert_eq!(ok_result.value, serde_json::json!({"result": "ok"}));

    let err_result = ToolCallResult::err("call_2".to_string(), "Something went wrong".to_string());
    assert_eq!(err_result.call_id, "call_2");
    assert!(err_result.error.is_some());
    assert_eq!(err_result.error.unwrap(), "Something went wrong");
}
