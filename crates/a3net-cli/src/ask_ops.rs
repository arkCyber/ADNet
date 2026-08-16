//! `a3net ask` — ask a remote peer node's AI agent a question over P2P.
//!
//! This is the **client side** of the `agent.v1` protocol. The node dials
//! a known peer and sends a JSON frame carrying a `ChatRequest`. The peer's
//! `NodeAgentBridge::handle_v1_chat` replies with a `AgentV1ChatResponse`
//! that contains the peer's `ChatResponse`.
//!
//! The CLI is intentionally thin: it talks to the **local daemon's IPC socket**
//! via JSON-RPC (the same mechanism used by all other `a3net` subcommands),
//! passing the peer's NodeId and the question. The daemon handles the P2P
//! transport, ACL check, and model invocation.
//!
//! # Wire protocol (transport layer)
//!
//! ```text
//! CLI → daemon (IPC / JSON-RPC)
//!   { "method": "agent.ask", "params": { "peer": "<hex>", "question": "…" } }
//!
//! daemon → peer (P2P / QUIC)
//!   { "cmd": "AgentV1Chat", "requestId": "…", "from": "<local_hex>", "body": { … } }
//!
//! peer → daemon (P2P / QUIC)
//!   { "cmd": "AgentV1ChatResponse", "requestId": "…", "from": "<peer_hex>",
//!     "body": { "content": "…", "finish": "stop", … }, "error": null }
//! daemon → CLI (IPC / JSON-RPC)
//!   { "result": { "content": "…", "model": "hermes-rust", "from": "…", "finish": "stop" } }
//! ```

use serde::{Deserialize, Serialize};

/// JSON-RPC request envelope sent from CLI → daemon.
/// The daemon dials `peer_node_id` and forwards the question as an
/// `agent.v1` P2P frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskRequest {
    /// 64-hex NodeId of the target peer.
    pub peer: String,
    /// Human-readable question to send to the peer's registered agent.
    pub question: String,
}

/// JSON-RPC response envelope returned from daemon → CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskResponse {
    /// The peer's agent's textual reply.
    pub content: String,
    /// Model label reported by the peer's agent.
    pub model: Option<String>,
    /// NodeId of the responding peer.
    pub from: String,
    /// Finish reason (`stop` / `tool_calls` / …).
    #[serde(default)]
    pub finish: Option<String>,
    /// Tool calls, if the model emitted any (uncommon in single-turn mode).
    #[serde(default)]
    pub tool_calls: Vec<ToolCallWire>,
    /// Error message if the P2P call failed.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallWire {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Run the `a3net ask` subcommand.
///
/// Delegates to the local daemon via JSON-RPC over the IPC socket.
///
/// # Arguments
///
/// * `data_dir` — path to the local A3Net data directory (contains `ipc.sock`)
/// * `peer` — 64-hex NodeId of the target peer
/// * `question` — the question to ask
/// * `json` — whether to emit the raw JSON envelope
pub async fn run_ask(
    data_dir: &std::path::Path,
    peer: String,
    question: String,
    json: bool,
) -> anyhow::Result<()> {
    let rpc = crate::ipc_client::IpcClient::connect(data_dir);
    let result: AskResponse = rpc
        .call("agent.ask", AskRequest { peer, question })
        .await?;

    if let Some(err) = &result.error {
        anyhow::bail!("agent.ask failed: {err}");
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("[{}] {}", result.from, result.content);
    }
    Ok(())
}

/// Shared request builder for use in IPC/RPC integration tests.
pub fn build_request(peer: &str, question: &str) -> AskRequest {
    AskRequest {
        peer: peer.to_string(),
        question: question.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_request_roundtrip() {
        let req = build_request("abcd1234abcd1234", "hello remote agent");
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("abcd1234"));
        let back: AskRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peer, "abcd1234abcd1234");
        assert_eq!(back.question, "hello remote agent");
    }

    #[test]
    fn ask_response_success_roundtrip() {
        let resp = AskResponse {
            content: "hi from peer".to_string(),
            model: Some("hermes-rust".to_string()),
            from: "peer1234".to_string(),
            finish: Some("stop".to_string()),
            tool_calls: Vec::new(),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AskResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content, "hi from peer");
        assert_eq!(back.model.as_deref(), Some("hermes-rust"));
        assert!(back.error.is_none());
    }

    #[test]
    fn ask_response_error_roundtrip() {
        let resp = AskResponse {
            content: String::new(),
            model: None,
            from: "peer5678".to_string(),
            finish: None,
            tool_calls: Vec::new(),
            error: Some("peer not permitted".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AskResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.error.as_deref(), Some("peer not permitted"));
        assert!(back.content.is_empty());
    }

    #[test]
    fn ask_response_with_tool_calls() {
        let resp = AskResponse {
            content: "I need to run a tool".to_string(),
            model: Some("hermes-rust".to_string()),
            from: "peer9abc".to_string(),
            finish: Some("tool_calls".to_string()),
            tool_calls: vec![ToolCallWire {
                id: "call_1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            }],
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AskResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_calls.len(), 1);
        assert_eq!(back.tool_calls[0].name, "echo");
    }
}

