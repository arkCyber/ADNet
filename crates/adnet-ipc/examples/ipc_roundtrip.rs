//! Demo: spin up both an in-process gossip service + a JSON-RPC client that
//! talks to it through the Unix socket.

use std::sync::Arc;

use adnet_ipc::{json_rpc_call, GossipIpcConfig, GossipIpcService};
use serde_json::json;

#[tokio::main]
async fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("demo.sock");
    let svc = Arc::new(GossipIpcService::new(GossipIpcConfig {
        socket_path: sock.clone(),
    }));
    let handle = Arc::clone(&svc).serve().await.expect("serve");
    println!("gossip service listening at {}", sock.display());

    json_rpc_call(
        &sock,
        "demo",
        "subscribe",
        json!({ "topic": "adnet-room-demo", "subscriber_id": "client-1" }),
    )
    .await
    .expect("subscribe");

    let resp = json_rpc_call(
        &sock,
        "demo",
        "publish",
        json!({
            "topic": "adnet-room-demo",
            "payload": { "hello": "adnet" }
        }),
    )
    .await
    .expect("publish");
    println!("publish ok: {resp}");

    let msgs = json_rpc_call(
        &sock,
        "demo",
        "get_messages",
        json!({ "topic": "adnet-room-demo", "limit": 5 }),
    )
    .await
    .expect("get_messages");
    println!("messages: {msgs}");

    handle.shutdown();
}
