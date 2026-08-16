//! Full-stack integration tests for `a3chat-rpc`.
//!
//! These spin up a real `RpcServer` bound to a loopback port and
//! drive it over HTTP with `reqwest`. The point is to catch
//! regressions in the *wiring* between axum, the dispatcher, and
//! `A3chatApp` — exactly the kind of thing unit tests on individual
//! handlers miss.

use std::time::Duration;

use a3chat_app::storage::StorageConfig;
use a3chat_app::A3chatApp;
use a3chat_core::id::UserId;
use a3chat_rpc::{RpcServer, RpcServerConfig};
use serde_json::{Value, json};
use tempfile::TempDir;

const HEADER_OWNER: &str = "x-a3chat-owner";

async fn boot_server() -> (TempDir, std::net::SocketAddr) {
    let dir = tempfile::tempdir().expect("tempdir");
    let owner = UserId::from("alice");
    let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner)
        .expect("A3chatApp::new");
    let server = RpcServer::new(app, RpcServerConfig::default());
    let handle = server.start().await.expect("start server");
    // We deliberately leak the handle (drop the shutdown sender
    // but keep the JoinHandle alive until the test exits). axum's
    // graceful shutdown would otherwise wait forever for SSE
    // connections, which we don't open in this file.
    let addr = handle.local_addr;
    std::mem::forget(handle);
    (dir, addr)
}

async fn rpc_call(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    owner: &str,
    method: &str,
    params: Value,
    id: Value,
) -> reqwest::Response {
    let body = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": id,
    });
    client
        .post(format!("http://{addr}/rpc"))
        .header("content-type", "application/json")
        .header(HEADER_OWNER, owner)
        .json(&body)
        .send()
        .await
        .expect("rpc call")
}

fn fresh_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client")
}

// -- /rpc/health and /rpc/version over real HTTP ---------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_endpoint_responds_ok() {
    let (_dir, addr) = boot_server().await;
    let resp = fresh_client()
        .get(format!("http://{addr}/rpc/health"))
        .send()
        .await
        .expect("get health");
    assert!(resp.status().is_success(), "status={}", resp.status());
    let body: Value = resp.json().await.expect("decode health body");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "a3chat-rpc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_endpoint_returns_version_field() {
    let (_dir, addr) = boot_server().await;
    let resp = fresh_client()
        .get(format!("http://{addr}/rpc/version"))
        .send()
        .await
        .expect("get version");
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.expect("decode body");
    assert!(body.get("version").is_some(), "no version field");
    assert_eq!(body["service"], "a3chat");
}

// -- Header enforcement -----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_call_without_owner_header_returns_400() {
    let (_dir, addr) = boot_server().await;
    let client = fresh_client();
    // Missing `x-a3chat-owner` header — must be rejected.
    let body = json!({
        "jsonrpc": "2.0",
        "method": "a3chat.contact.list",
        "params": {},
        "id": 1,
    });
    let resp = client
        .post(format!("http://{addr}/rpc"))
        .json(&body)
        .send()
        .await
        .expect("post");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "missing owner must yield 400"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_call_with_invalid_json_returns_400() {
    let (_dir, addr) = boot_server().await;
    let client = fresh_client();
    let resp = client
        .post(format!("http://{addr}/rpc"))
        .header("content-type", "application/json")
        .header(HEADER_OWNER, "alice")
        .body("not json at all".as_bytes().to_vec())
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

// -- End-to-end contact flow ------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contact_add_request_then_list_round_trip() {
    // Full-stack wiring test. In P0 the storage layer is a stub
    // (`ContactsSnapshot::default()`), so we can only assert that
    // *both* the add path and the list path complete without
    // error, returning a valid JSON-RPC envelope. The stronger
    // assertion — that adding makes the request visible in the
    // subsequent list — lands with P1's persistent roster.
    let (_dir, addr) = boot_server().await;
    let client = fresh_client();
    let owner = "alice";
    let bob = "bob-node-id";

    // 1. Initial list: empty snapshot, valid envelope.
    let list_resp = rpc_call(
        &client,
        addr,
        owner,
        "a3chat.contact.list",
        json!({}),
        json!(1),
    )
    .await;
    assert!(list_resp.status().is_success(), "list status");
    let list_body: Value = list_resp.json().await.expect("decode list");
    assert_eq!(
        list_body["error"], Value::Null,
        "list must not error initially: {list_body}"
    );
    assert_eq!(
        list_body["result"]["contacts"].as_array().map(|a| a.len()),
        Some(0),
        "P0 returns no contacts"
    );
    assert_eq!(
        list_body["result"]["blocklist"].as_array().map(|a| a.len()),
        Some(0),
        "P0 returns no blocked users"
    );

    // 2. Add a contact request — must succeed and return the new
    //    request_id and status.
    let add_resp = rpc_call(
        &client,
        addr,
        owner,
        "a3chat.contact.add_request",
        json!({ "to_user_id": bob, "message": "let's chat" }),
        json!(2),
    )
    .await;
    assert!(add_resp.status().is_success(), "add status");
    let add_body: Value = add_resp.json().await.expect("decode add");
    assert_eq!(
        add_body["error"], Value::Null,
        "add_request must not error: {add_body}"
    );
    let request_id = add_body["result"]["request_id"]
        .as_str()
        .expect("request_id present")
        .to_string();
    assert!(
        !request_id.is_empty(),
        "request_id must be a non-empty string"
    );
    assert_eq!(
        add_body["result"]["status"], "pending",
        "fresh request must be pending"
    );
    assert_eq!(add_body["result"]["to_user_id"], bob);

    // 3. Subsequent list still returns a well-formed envelope
    //    (P0 stub doesn't persist — P1 will assert visibility).
    let list_after = rpc_call(
        &client,
        addr,
        owner,
        "a3chat.contact.list",
        json!({}),
        json!(3),
    )
    .await;
    assert!(list_after.status().is_success());
    let list_after_body: Value = list_after.json().await.expect("decode list after");
    assert_eq!(list_after_body["error"], Value::Null);
    assert!(list_after_body["result"]["contacts"].is_array());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contact_add_request_rejects_oversize_message() {
    // The 256-char limit on the friend-request `message` field is
    // a hard contract — verify the RPC layer returns an error
    // envelope rather than crashing or truncating silently.
    let (_dir, addr) = boot_server().await;
    let client = fresh_client();
    let huge = "x".repeat(300);
    let resp = rpc_call(
        &client,
        addr,
        "alice",
        "a3chat.contact.add_request",
        json!({ "to_user_id": "bob", "message": huge }),
        json!(1),
    )
    .await;
    // Oversize is a JSON-RPC domain error → server returns 400 +
    // an error envelope with the standard `{ code, message }`.
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("decode");
    assert_eq!(body["result"], Value::Null);
    assert!(
        body["error"]["message"]
            .as_str()
            .map(|s| s.contains("256") || s.contains("exceeds"))
            .unwrap_or(false),
        "error message should mention 256/exceeds, got: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contact_block_and_unblock_complete() {
    // P0 doesn't persist the blocklist to the storage layer yet,
    // so we can only verify the methods don't 5xx and return a
    // well-formed envelope. The actual persistence path lands with
    // P1's roster integration.
    let (_dir, addr) = boot_server().await;
    let client = fresh_client();
    let owner = "alice";
    let bob = "bob";

    let block_resp = rpc_call(
        &client,
        addr,
        owner,
        "a3chat.contact.block",
        json!({ "user_id": bob }),
        json!(1),
    )
    .await;
    assert!(block_resp.status().is_success());
    let block_body: Value = block_resp.json().await.expect("decode block");
    assert_eq!(block_body["error"], Value::Null);
    assert_eq!(
        block_body["result"]["user_id"], bob,
        "block response must echo user_id"
    );

    let unblock_resp = rpc_call(
        &client,
        addr,
        owner,
        "a3chat.contact.unblock",
        json!({ "user_id": bob }),
        json!(2),
    )
    .await;
    assert!(unblock_resp.status().is_success());
    let unblock_body: Value = unblock_resp.json().await.expect("decode unblock");
    assert_eq!(unblock_body["error"], Value::Null);
    assert_eq!(unblock_body["result"]["ok"], true);
}

// -- Error path: unknown method over real HTTP -----------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_method_returns_rpc_error_envelope() {
    let (_dir, addr) = boot_server().await;
    let client = fresh_client();
    let resp = rpc_call(
        &client,
        addr,
        "alice",
        "a3chat.bogus.does.not.exist",
        json!({}),
        json!(99),
    )
    .await;
    // Server treats unknown method as a JSON-RPC error — HTTP is
    // 400 (per the server's policy in server.rs) and the body
    // contains a `result: null, error: {...}` envelope.
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("decode");
    assert_eq!(body["result"], Value::Null);
    assert_eq!(
        body["error"]["code"], -32601,
        "method-not-found code (-32601)"
    );
    assert_eq!(body["id"], 99, "response id must echo the request id");
}

// -- Concurrency: many parallel calls serialize cleanly --------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_parallel_list_calls_all_succeed() {
    // Catches deadlocks, missing-state-sharing, or any other
    // issue that would only surface under concurrent use.
    let (_dir, addr) = boot_server().await;
    let client = fresh_client();
    let mut handles = Vec::new();
    for i in 0..20 {
        let c = client.clone();
        let h = tokio::spawn(async move {
            let resp = rpc_call(
                &c,
                addr,
                "alice",
                "a3chat.contact.list",
                json!({}),
                json!(i),
            )
            .await;
            assert!(resp.status().is_success(), "iter {i}");
            let v: Value = resp.json().await.expect("decode");
            assert_eq!(v["error"], Value::Null, "iter {i}");
            assert_eq!(v["id"], json!(i), "iter {i} id preserved");
        });
        handles.push(h);
    }
    for h in handles {
        h.await.expect("join");
    }
}