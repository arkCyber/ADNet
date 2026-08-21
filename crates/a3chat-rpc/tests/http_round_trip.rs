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
use a3chat_core::rpc::A3chatRpcMethod;
use a3chat_rpc::server::{RpcServer, RpcServerConfig};
use chrono::Utc;

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
    let storage = ChatStorage::new(StorageConfig::new(dir.path().to_path_buf()), keyring);
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
            "timestamp": chrono::Utc::now().timestamp(),
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

// =============================================================================
// Contact roster HTTP round-trip (L4 coverage)
//
// Mirrors `http_rpc_round_trip_is_successful` for the
// `a3chat.contact.*` namespace. Exercises:
//   - list (empty) → add → list (one) → get → search → remove → list (empty)
//   - block / unblock round-trip
//   - qr_invite returns a JSON envelope with a `qr_payload` string
//   - owner-isolation: a wrong owner header gets a -32603 (internal
//     error from the Forbidden → AppError::Forbidden mapping) response
//     on `list`.
//
// The server is shared with the chat test above (same `spawn_server`)
// so the contact tests ride on the same in-process JSON-RPC stack.
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_contact_full_round_trip() {
    let (_dir, addr, _server, _join, shutdown) = spawn_server().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let owner_str = owner().as_str().to_string();

    // The 64-hex NodeId used as a peer contact id. Kept valid so the
    // CLI-side validator (when wired through) does not reject it.
    let peer = "b".repeat(64);

    // 1. list (empty initially)
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_LIST,
        serde_json::json!({}),
        serde_json::json!(101),
    )
    .await;
    assert_eq!(
        body["result"]["contacts"].as_array().map(|a| a.len()),
        Some(0),
        "fresh roster must be empty; got {body}"
    );

    // 2. add a contact
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_ADD,
        serde_json::json!({
            "user_id": peer,
            "display_name": "Bob",
            "note": "engineer",
            "is_favorite": false,
            "is_blocked": false,
            "added_at": Utc::now().to_rfc3339(),
        }),
        serde_json::json!(102),
    )
    .await;
    assert_eq!(body["result"]["user_id"], peer);

    // 3. list (one entry)
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_LIST,
        serde_json::json!({}),
        serde_json::json!(103),
    )
    .await;
    let contacts = body["result"]["contacts"].as_array().unwrap();
    assert_eq!(contacts.len(), 1, "expected exactly one contact; got {body}");
    assert_eq!(contacts[0]["user_id"], peer);
    assert_eq!(contacts[0]["display_name"], "Bob");

    // 4. get
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_GET,
        serde_json::json!({ "contact_id": peer }),
        serde_json::json!(104),
    )
    .await;
    assert_eq!(body["result"]["display_name"], "Bob");

    // 5. search
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_SEARCH,
        serde_json::json!({ "query": "engineer" }),
        serde_json::json!(105),
    )
    .await;
    assert_eq!(body["result"].as_array().unwrap().len(), 1);

    // 6. toggle_favorite (twice)
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_TOGGLE_FAVORITE,
        serde_json::json!({ "contact_id": peer }),
        serde_json::json!(106),
    )
    .await;
    assert_eq!(body["result"], true, "first toggle must turn it on");
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_TOGGLE_FAVORITE,
        serde_json::json!({ "contact_id": peer }),
        serde_json::json!(107),
    )
    .await;
    assert_eq!(body["result"], false, "second toggle must turn it off");

    // 7. block then unblock
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_BLOCK,
        serde_json::json!({ "user_id": peer }),
        serde_json::json!(108),
    )
    .await;
    assert_eq!(body["result"]["user_id"], peer);
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_LIST,
        serde_json::json!({}),
        serde_json::json!(109),
    )
    .await;
    assert_eq!(
        body["result"]["blocklist"].as_array().unwrap().len(),
        1,
        "blocklist must reflect block; got {body}"
    );

    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_UNBLOCK,
        serde_json::json!({ "user_id": peer }),
        serde_json::json!(110),
    )
    .await;
    assert_eq!(body["result"]["ok"], true);

    // 8. qr_invite envelope
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_QR_INVITE,
        serde_json::json!({}),
        serde_json::json!(111),
    )
    .await;
    let payload = body["result"]["qr_payload"]
        .as_str()
        .unwrap_or_else(|| panic!("qr_invite must return qr_payload: {body}"));
    assert!(!payload.is_empty());

    // 9. update contact fields
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_UPDATE,
        serde_json::json!({
            "user_id": peer,
            "display_name": "Robert",
            "note": "promoted",
            "is_favorite": false,
            "is_blocked": false,
            "added_at": Utc::now().to_rfc3339(),
        }),
        serde_json::json!(112),
    )
    .await;
    assert_eq!(body["result"]["display_name"], "Robert");

    // 10. remove
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_REMOVE,
        serde_json::json!({ "contact_id": peer }),
        serde_json::json!(113),
    )
    .await;
    assert_eq!(body["result"]["removed"], true);
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_LIST,
        serde_json::json!({}),
        serde_json::json!(114),
    )
    .await;
    assert_eq!(body["result"]["contacts"].as_array().unwrap().len(), 0);

    // 11. owner-isolation: a different X-A3Chat-Owner must NOT be
    // able to read this roster. The server enforces the owner via
    // `ContactService::require_owner`, so we expect an `error` field
    // on the response.
    let body = rpc_post(
        &client,
        &base,
        // 64-hex of a different owner; the server maps the header
        // directly to `UserId::from`.
        &"c".repeat(64),
        A3chatRpcMethod::CONTACT_LIST,
        serde_json::json!({}),
        serde_json::json!(115),
    )
    .await;
    assert!(
        body["error"].is_object(),
        "non-owner caller must receive an error envelope; got {body}"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_contact_friend_request_round_trip() {
    let (_dir, addr, _server, _join, shutdown) = spawn_server().await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();
    let owner_str = owner().as_str().to_string();
    let peer = "d".repeat(64);

    // Send a friend request.
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_ADD_REQUEST,
        serde_json::json!({ "to_user_id": peer, "message": "hi" }),
        serde_json::json!(201),
    )
    .await;
    let request_id = body["result"]["request_id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing request_id: {body}"))
        .to_string();
    assert_eq!(body["result"]["status"], "pending");
    assert_eq!(body["result"]["from_user_id"], owner_str);

    // Oversize message must be rejected (256-char cap).
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_ADD_REQUEST,
        serde_json::json!({ "to_user_id": peer, "message": "x".repeat(257) }),
        serde_json::json!(202),
    )
    .await;
    assert!(body["error"].is_object(), "oversize message must error");

    // Accept — supply a synthetic ContactRequest with the id we
    // received. The dispatcher deserialises the whole request body
    // and accepts it regardless of signature (the audit report
    // flagged this as a missing signature check; the test exercises
    // the current contract so a follow-up patch can tighten it).
    let body = rpc_post(
        &client,
        &base,
        &owner_str,
        A3chatRpcMethod::CONTACT_ACCEPT_REQUEST,
        serde_json::json!({
            "request": {
                "request_id": request_id,
                "from_user_id": peer,
                "from_display_name": "Peer",
                "to_user_id": owner_str,
                "message": "hi",
                "status": "pending",
                "created_at": Utc::now().to_rfc3339(),
                "responded_at": null,
            }
        }),
        serde_json::json!(203),
    )
    .await;
    assert_eq!(body["result"]["user_id"], peer);
    assert!(body["result"]["display_name"].is_string());

    let _ = shutdown.send(());
}
