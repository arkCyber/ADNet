//! HTTP JSON-RPC client used by Tauri commands (and any frontend
//! that wants to wrap the daemon). This is the loopback counterpart
//! of `a3chat-rpc`'s server.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;
#[allow(unused_imports)]
use a3chat_core::rpc::{A3chatRpcMethod, RpcClient};

use crate as _; // satisfy `pub use` re-export below

/// Configuration for [`A3chatClient`].
#[derive(Debug, Clone)]
pub struct A3chatClientConfig {
    /// Base URL of the `a3chat-rpc` daemon — e.g.
    /// `http://127.0.0.1:53421`.
    pub base_url: String,
    /// Owner identity sent on every call via `X-A3Chat-Owner`.
    pub owner: UserId,
    /// Request timeout.
    pub timeout: Duration,
}

impl A3chatClientConfig {
    pub fn new(base_url: impl Into<String>, owner: UserId) -> Self {
        Self {
            base_url: base_url.into(),
            owner,
            timeout: Duration::from_secs(30),
        }
    }
}

/// JSON-RPC request payload (mirrors `a3chat-rpc::RpcRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RawRequest {
    jsonrpc: String,
    method: String,
    params: serde_json::Value,
    id: serde_json::Value,
}

/// JSON-RPC response payload (mirrors `a3chat-rpc::RpcResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RawResponse {
    jsonrpc: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<RawError>,
    id: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RawError {
    code: i32,
    message: String,
}

/// Thin HTTP client. Cheap to clone — the underlying `reqwest::Client`
/// is internally Arc'd.
#[derive(Clone)]
pub struct A3chatClient {
    cfg: A3chatClientConfig,
    http: reqwest::Client,
}

impl A3chatClient {
    pub fn new(cfg: A3chatClientConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .expect("reqwest client build");
        Self { cfg, http }
    }

    pub fn config(&self) -> &A3chatClientConfig {
        &self.cfg
    }

    /// Send a JSON-RPC call and return the parsed `result` on success.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, A3chatError> {
        let req = RawRequest {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id: serde_json::json!(uuid::Uuid::new_v4().to_string()),
        };
        let url = format!("{}/rpc", self.cfg.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .header("X-A3Chat-Owner", self.cfg.owner.as_str())
            .json(&req)
            .send()
            .await
            .map_err(|e| A3chatError::NetworkError(format!("http send: {e}")))?;
        let status = resp.status();
        let raw: RawResponse = resp
            .json()
            .await
            .map_err(|e| A3chatError::RpcError(format!("http read: {e}")))?;
        if let Some(err) = raw.error {
            return Err(A3chatError::RpcError(format!(
                "[{}] {}",
                err.code, err.message
            )));
        }
        if !status.is_success() {
            return Err(A3chatError::RpcError(format!(
                "http {} from server",
                status.as_u16()
            )));
        }
        raw.result
            .ok_or_else(|| A3chatError::RpcError("empty response body".into()))
    }
}

#[async_trait]
impl RpcClient for A3chatClient {
    async fn call_json(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, A3chatError> {
        self.call(method, params).await
    }
}

/// Quick-start helper for tests + scripts.
pub async fn ping(cfg: &A3chatClientConfig) -> Result<serde_json::Value, A3chatError> {
    let client = A3chatClient::new(cfg.clone());
    let url = format!("{}/rpc/health", cfg.base_url.trim_end_matches('/'));
    let resp = client
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| A3chatError::NetworkError(format!("ping: {e}")))?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| A3chatError::RpcError(format!("ping read: {e}")))?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    #[test]
    fn config_constructor_sets_defaults() {
        let cfg = A3chatClientConfig::new("http://127.0.0.1:8080", owner());
        assert_eq!(cfg.base_url, "http://127.0.0.1:8080");
        assert_eq!(cfg.owner, owner());
        assert_eq!(cfg.timeout, Duration::from_secs(30));
    }

    #[test]
    fn client_is_clone() {
        let cfg = A3chatClientConfig::new("http://127.0.0.1:0", owner());
        let c1 = A3chatClient::new(cfg);
        let _c2 = c1.clone();
    }

    #[test]
    fn raw_request_serializes() {
        let r = RawRequest {
            jsonrpc: "2.0".into(),
            method: "a3chat.foo".into(),
            params: serde_json::json!({}),
            id: serde_json::json!(1),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
    }

    #[test]
    fn raw_response_parses_success() {
        let s = r#"{"jsonrpc":"2.0","result":{"ok":true},"id":1}"#;
        let r: RawResponse = serde_json::from_str(s).unwrap();
        assert!(r.error.is_none());
        assert!(r.result.is_some());
    }

    #[test]
    fn raw_response_parses_error() {
        let s = r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"not found"},"id":1}"#;
        let r: RawResponse = serde_json::from_str(s).unwrap();
        let err = r.error.unwrap();
        assert_eq!(err.code, -32601);
    }

    #[test]
    fn uses_all_known_methods() {
        // Smoke test: ensure the A3chatRpcMethod constants are
        // reachable from this crate (no shadowing).
        let _ = A3chatRpcMethod::CHAT_MESSAGE_SEND;
        let _ = A3chatRpcMethod::CONTACT_LIST;
        let _ = A3chatRpcMethod::GROUP_CREATE;
    }

    #[tokio::test]
    async fn ping_fails_fast_against_unreachable_url() {
        // Unroutable address → reqwest should error quickly.
        let cfg = A3chatClientConfig::new("http://127.0.0.1:1", owner());
        let r = tokio::time::timeout(Duration::from_secs(5), ping(&cfg)).await;
        match r {
            Ok(Ok(_)) => panic!("expected error against 127.0.0.1:1"),
            Ok(Err(_)) => {}
            Err(_) => {} // timeout acceptable
        }
    }

    #[tokio::test]
    async fn call_fails_against_unreachable_url() {
        let cfg = A3chatClientConfig::new("http://127.0.0.1:1", owner());
        let client = A3chatClient::new(cfg);
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            client.call("a3chat.foo", serde_json::json!({})),
        )
        .await;
        match r {
            Ok(Ok(_)) => panic!("expected error"),
            Ok(Err(_)) => {}
            Err(_) => {}
        }
    }

    // End-to-end: spin up a real `a3chat-rpc` HTTP server on a
    // loopback port, point the client at it, and exercise the four
    // core chat.* methods (list, send, open, ack).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn e2e_chat_send_list_open_ack_via_http() {
        use a3chat_app::A3chatApp;
        use a3chat_app::storage::StorageConfig;
        use a3chat_core::id::ConversationId;
        use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};
        use a3chat_core::rpc::A3chatRpcMethod;
        use a3chat_rpc::{RpcServer, RpcServerConfig};

        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        app.init_user(&owner()).await.unwrap();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let client = A3chatClient::new(A3chatClientConfig::new(&base, owner()));

        // 1) chat.conversation.list — should start empty.
        let list = client
            .call(
                A3chatRpcMethod::CHAT_CONVERSATION_LIST,
                serde_json::json!({}),
            )
            .await
            .expect("list");
        assert!(list.is_array(), "list must be a JSON array");
        assert_eq!(list.as_array().unwrap().len(), 0);

        // 2) chat.message.send — send a DM to Bob.
        let env = MessageEnvelope {
            conversation_id: ConversationId::from("dm:alice-node-id:bob-node-id"),
            receiver_id: a3chat_core::id::UserId::from("bob-node-id"),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hello".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        let sent = client
            .call(
                A3chatRpcMethod::CHAT_MESSAGE_SEND,
                serde_json::to_value(&env).unwrap(),
            )
            .await
            .expect("send");
        let msg_id = sent
            .get("message")
            .and_then(|m| m.get("message_id"))
            .and_then(|s| s.as_str())
            .expect("sent.message.message_id")
            .to_string();

        // 3) chat.conversation.list — should now have 1 entry.
        let list = client
            .call(
                A3chatRpcMethod::CHAT_CONVERSATION_LIST,
                serde_json::json!({}),
            )
            .await
            .expect("list after send");
        assert_eq!(list.as_array().unwrap().len(), 1);

        // 4) chat.conversation.open — open the DM.
        let opened = client
            .call(
                A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
                serde_json::json!({
                    "conversation_id": env.conversation_id.as_str(),
                }),
            )
            .await
            .expect("open");
        assert!(opened.get("meta").is_some());

        // 5) chat.message.ack — mark the message read.
        let ack = client
            .call(
                A3chatRpcMethod::CHAT_MESSAGE_ACK,
                serde_json::json!({ "message_id": msg_id }),
            )
            .await
            .expect("ack");
        assert_eq!(ack.get("ok").and_then(|v| v.as_bool()), Some(true));

        // 6) /rpc/health round-trip.
        let h = ping(client.config()).await.expect("ping");
        assert_eq!(h.get("status").and_then(|v| v.as_str()), Some("ok"));

        handle.stop().await;
    }
}
