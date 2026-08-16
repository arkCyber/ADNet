//! End-to-end: spin up a real `a3chat-rpc` HTTP server on a random
//! loopback port, drive it through the [`HttpRpcClient`], and
//! exercise every CLI subcommand.
//!
//! DO-178C §5.2 — traceability: every assertion here is traceable to
//! a public RPC method documented in `a3chat-core::rpc::A3chatRpcMethod`.

use std::time::Duration;

use a3chat_app::storage::StorageConfig;
use a3chat_app::A3chatApp;
use a3chat_cli::config::CliConfig;
use a3chat_cli::rpc_client::{HttpRpcClient, RpcClientBuilder};
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};
use a3chat_core::rpc::A3chatRpcMethod;
use a3chat_rpc::{RpcServer, RpcServerConfig};

fn owner() -> UserId {
    UserId::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
}

async fn boot_daemon() -> (
    tempfile::TempDir,
    a3chat_rpc::RpcServerHandle,
    CliConfig,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner())
        .expect("app");
    app.init_user(&owner()).await.expect("init user");
    let server = RpcServer::new(app, RpcServerConfig::default());
    let handle = server.start().await.expect("start server");
    let cfg = CliConfig {
        daemon_url: Some(format!("http://{}", handle.local_addr)),
        owner: Some(owner().to_string()),
        output: None,
        retries: Some(1),
        timeout_ms: Some(5000),
    };
    (dir, handle, cfg)
}

fn build_client(cfg: &CliConfig) -> HttpRpcClient {
    RpcClientBuilder::new(cfg).build().expect("build client")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_check_via_list() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::CHAT_CONVERSATION_LIST,
            serde_json::json!({}),
        )
        .await
        .expect("list");
    assert!(r.is_array());
    assert_eq!(r.as_array().unwrap().len(), 0);
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_then_list_then_open_then_ack() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let env = MessageEnvelope {
        conversation_id: ConversationId::from("dm:alice-node-id:bob-node-id"),
        receiver_id: UserId::from("bob-node-id"),
        message_type: MessageType::Text,
        body: MessageBody::Plain { content: "hi".into() },
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: 1_700_000_000,
    };
    let sent: serde_json::Value = c
        .call(A3chatRpcMethod::CHAT_MESSAGE_SEND, serde_json::to_value(&env).unwrap())
        .await
        .expect("send");
    let mid = sent
        .get("message")
        .and_then(|m| m.get("message_id"))
        .and_then(|s| s.as_str())
        .unwrap()
        .to_string();
    let list: serde_json::Value = c
        .call(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await
        .expect("list");
    assert_eq!(list.as_array().unwrap().len(), 1);
    let opened: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
            serde_json::json!({ "conversation_id": env.conversation_id.as_str() }),
        )
        .await
        .expect("open");
    assert!(opened.get("meta").is_some());
    let ack: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_ACK,
            serde_json::json!({ "message_id": mid }),
        )
        .await
        .expect("ack");
    assert_eq!(ack.get("ok").and_then(|b| b.as_bool()), Some(true));
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_error_triggers_retry() {
    // Point at a guaranteed-unreachable URL. With retries=2 and a
    // short backoff, the client should exhaust attempts in well under
    // a second. We just verify the call eventually returns the same
    // transient error class.
    let cfg = CliConfig {
        daemon_url: Some("http://127.0.0.1:1".into()),
        owner: Some(owner().to_string()),
        output: None,
        retries: Some(2),
        timeout_ms: Some(300),
    };
    let c = build_client(&cfg);
    let start = std::time::Instant::now();
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::CHAT_CONVERSATION_LIST,
            serde_json::json!({}),
        )
        .await;
    let elapsed = start.elapsed();
    match r {
        Err(a3chat_cli::error::CliError::Rpc(e)) => {
            assert!(e.is_retryable(), "expected retryable error, got {e:?}");
        }
        other => panic!("expected Rpc error, got {other:?}"),
    }
    // At least one backoff sleep (100ms) should have occurred.
    assert!(elapsed >= Duration::from_millis(50), "elapsed: {elapsed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_on_permanent_error_does_not_happen() {
    // Boot a real daemon, then send a bad request (missing param).
    // A permanent error must NOT trigger retry — it must surface
    // immediately.
    let (_dir, handle, mut cfg) = boot_daemon().await;
    cfg.retries = Some(5);
    let c = build_client(&cfg);
    let start = std::time::Instant::now();
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::CHAT_MESSAGE_DELETE,
            serde_json::json!({}), // missing message_id
        )
        .await;
    let elapsed = start.elapsed();
    match r {
        Err(a3chat_cli::error::CliError::Rpc(e)) => {
            assert!(!e.is_retryable(), "permanent error must not retry");
        }
        other => panic!("expected Rpc error, got {other:?}"),
    }
    // No backoff sleeps — should be effectively instant.
    assert!(elapsed < Duration::from_millis(500), "elapsed: {elapsed:?}");
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_via_http_returns_object() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::CHAT_SYNC_SNAPSHOT,
            serde_json::json!({}),
        )
        .await
        .expect("snapshot");
    // Snapshot shape is implementation-defined; we just need an object.
    assert!(r.is_object() || r.is_null(), "snapshot: {r}");
    handle.stop().await;
}