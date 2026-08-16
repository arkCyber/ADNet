//! HTTP-level integration: bring up the JSON-RPC server on a real
//! (random) loopback port, point a fresh `reqwest::Client` at it,
//! and walk the full send → list → ack → search path.
//!
//! This goes through axum's HTTP layer, JSON serialisation,
//! JSON-RPC envelope validation, the dispatcher, and finally
//! back down to the storage. Anything weaker would let a
//! regression in the HTTP glue (bad status code, wrong header,
//! missing header check) sneak past unit tests.

use a3chat_app::keyring::E2eKeyring;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_rpc::server::{RpcServer, RpcServerConfig};

fn owner() -> UserId {
    UserId::from("alice-node-id")
}

async fn spawn_server() -> (
    tempfile::TempDir,
    std::net::SocketAddr,
    std::sync::Arc<RpcServer>,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let keyring = E2eKeyring::new(owner());
    let storage = ChatStorage::new(
        StorageConfig::new(dir.path().to_path_buf()),
        keyring,
    );
    let bus = NotificationBus::new(64);
    // Build the app with a pre-constructed storage so the RPC layer
    // shares the same database handle as the test's persistence path.
    let app = a3chat_app::A3chatApp::with_storage(storage, bus, owner());

    let cfg = RpcServerConfig::new("127.0.0.1:0".parse().unwrap());
    let server = RpcServer::new(app, cfg);
    let server = std::sync::Arc::new(server);

    // Bind a listener so we can capture the OS-assigned port, then
    // serve via axum in a background task.
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    let listener = TcpListener::bind(server.config.bind_addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    let router = (*server).router();
    let join = tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = rx.await;
        });
        let _ = server.await;
    });

    // Give the listener a tick to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (dir, addr, server, join, tx)
}

async fn rpc_post(
    client: &reqwest::Client,
    base: &str,
    owner: &str,
    method: &'static str,
    params: serde_json::Value,
    id: serde_json::Value,
) -> serde_json::Value {
    client
        .post(format!("{base}/rpc"))
        .header("X-A3Chat-Owner", owner)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_rpc_round_trip_is_successful() {
    let (_dir, addr, _server, _join, shutdown) = spawn_server().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let owner_str = owner().as_str().to_string();

    // 1. Health check
    let h = reqwest::get(format!("{base}/rpc/health"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(h.contains("ok"), "got {h}");

    // 2. conversation.list
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        "a3chat.chat.conversation.list",
        serde_json::json!({}),
        serde_json::json!(1),
    )
    .await;
    assert!(body["result"].is_array());

    // 3. message.send
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        "a3chat.chat.message.send",
        serde_json::json!({
            "conversation_id": "dm:a:b",
            "receiver_id": "bob",
            "message_type": "text",
            "body": { "kind": "plain", "content": "hi" },
            "attachments": [],
            "reply_to": null,
            "sequence": 1,
            "timestamp": 1_700_000_000_i64,
        }),
        serde_json::json!(2),
    )
    .await;
    let message_id = body["result"]["message"]["message_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!message_id.is_empty());

    // 4. message.ack
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        "a3chat.chat.message.ack",
        serde_json::json!({ "message_id": message_id }),
        serde_json::json!(3),
    )
    .await;
    assert_eq!(body["result"]["ok"], true);

    // 5. search (encrypted bodies don't match plaintext, so we
    // assert the call shape rather than the count)
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        "a3chat.chat.search",
        serde_json::json!({ "needle": "hi", "limit": 10 }),
        serde_json::json!(4),
    )
    .await;
    assert!(body["result"].is_array());

    // 6. version
    let v = reqwest::get(format!("{base}/rpc/version"))
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(v.get("service").is_some());

    // 7. unknown method → -32601 (JSON-RPC spec method not found)
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        "a3chat.does.not.exist",
        serde_json::json!({}),
        serde_json::json!(5),
    )
    .await;
    assert_eq!(body["error"]["code"], -32601);

    // 8. conversation.open
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        "a3chat.chat.conversation.open",
        serde_json::json!({ "conversation_id": "dm:a:b" }),
        serde_json::json!(6),
    )
    .await;
    assert!(body["result"].is_object() || body["result"].is_null());

    // 9. message.recall
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        "a3chat.chat.message.recall",
        serde_json::json!({ "message_id": message_id }),
        serde_json::json!(7),
    )
    .await;
    assert!(body["result"]["recalled_at"].is_string());

    // Use the imported identifiers so dead-code lints stay quiet.
    let _ = ConversationId::from("dm:a:b");
    let _ = MessageId::from(message_id);

    // Graceful shutdown — drops the listener task.
    let _ = shutdown.send(());
}
