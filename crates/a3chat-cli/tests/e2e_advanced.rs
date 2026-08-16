//! End-to-end tests for the new `rpc`, `trace`, and live `audit`
//! subcommands against a real `a3chat-rpc` daemon.

use std::time::Duration;

use a3chat_app::storage::StorageConfig;
use a3chat_app::A3chatApp;
use a3chat_cli::config::CliConfig;
use a3chat_cli::rpc_client::{HttpRpcClient, RpcClientBuilder};
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;
use a3chat_rpc::{RpcServer, RpcServerConfig};

fn owner() -> UserId {
    UserId::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
}

async fn boot_daemon_with_bus() -> (
    tempfile::TempDir,
    a3chat_rpc::RpcServerHandle,
    CliConfig,
    a3chat_app::notification_bus::NotificationBus,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner())
        .expect("app");
    app.init_user(&owner()).await.expect("init user");
    let bus = app.bus.clone();
    let server = RpcServer::new(app, RpcServerConfig::default());
    let handle = server.start().await.expect("start server");
    let cfg = CliConfig {
        daemon_url: Some(format!("http://{}", handle.local_addr)),
        owner: Some(owner().to_string()),
        output: None,
        retries: Some(1),
        timeout_ms: Some(5000),
    };
    (dir, handle, cfg, bus)
}

async fn boot_daemon() -> (
    tempfile::TempDir,
    a3chat_rpc::RpcServerHandle,
    CliConfig,
) {
    let (dir, h, cfg, _) = boot_daemon_with_bus().await;
    (dir, h, cfg)
}

fn build_client(cfg: &CliConfig) -> HttpRpcClient {
    RpcClientBuilder::new(cfg).build().expect("build client")
}

/// `call_raw_with_meta` must return the same value as a raw POST,
/// plus non-empty `request_id` and accurate `attempts`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_raw_with_meta_reports_request_id_and_attempts() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let r = c
        .call_raw_with_meta(
            A3chatRpcMethod::CHAT_CONVERSATION_LIST,
            serde_json::json!({}),
            1,
        )
        .await
        .expect("call");
    assert!(!r.request_id.is_empty(), "request_id should be a UUID");
    assert_eq!(r.attempts, 1, "no retries → attempts == 1");
    assert!(r.value.is_array());
    handle.stop().await;
}

/// Unknown methods must come back as `RpcError` — the CLI is
/// supposed to short-circuit unknown names BEFORE sending, but the
/// client itself should still surface the server's error
/// faithfully.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_method_returns_typed_error() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let r = c
        .call_raw("a3chat.no.such.method", serde_json::json!({}))
        .await;
    // We don't strictly enforce "method_not_found" wording — just
    // that it's an `Rpc` variant.
    assert!(matches!(
        r,
        Err(a3chat_cli::error::CliError::Rpc(
            a3chat_core::error::A3chatError::RpcError(_)
        ))
    ));
    handle.stop().await;
}

/// SSE: open the stream, publish an event, assert we receive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_stream_delivers_published_events() {
    use eventsource_stream::Eventsource;
    use futures::StreamExt;

    let (_dir, handle, cfg, bus) = boot_daemon_with_bus().await;
    let c = build_client(&cfg);

    let request_id = uuid::Uuid::new_v4().to_string();
    let resp = c.connect_sse(&request_id).await.expect("sse connect");
    assert!(resp.status().is_success());
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.starts_with("text/event-stream"));

    let mut stream = resp.bytes_stream().eventsource();

    // Give the handler a tick to register the subscription.
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Publish through the bus we cloned before the server started.
    bus.publish(a3chat_core::event::A3chatEvent::ChatTyping {
        user_id: owner(),
        conversation_id: a3chat_core::id::ConversationId::from("dm:test"),
        expires_at: 0,
    });

    // Read the first SSE message.
    let msg = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("sse timed out")
        .expect("sse stream ended")
        .expect("sse parse");
    assert!(
        msg.event
            .contains(A3chatRpcMethod::NOTIFICATION_CHAT_TYPING),
        "expected typing event, got {}",
        msg.event
    );

    drop(stream);
    drop(handle);
}

/// Live audit: probe the daemon for `chat.conversation.list`. It
/// must come back as `implemented`, not `method_not_found`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_audit_chat_conversation_list_is_implemented() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let v = c
        .call_raw(
            A3chatRpcMethod::CHAT_CONVERSATION_LIST,
            serde_json::json!({}),
        )
        .await
        .expect("list");
    assert!(v.is_array());
    handle.stop().await;
}

/// Live audit: probe a known stub method
/// (`a3chat.media.upload_init`). The app dispatcher has no handler
/// for it, so the daemon responds with an `Internal` error
/// ("A3chatApp does not handle method …"). Operators should treat
/// this as a stub marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_audit_stub_method_is_unhandled() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let r = c
        .call_raw(
            A3chatRpcMethod::MEDIA_UPLOAD_INIT,
            serde_json::json!({}),
        )
        .await;
    match r {
        Err(a3chat_cli::error::CliError::Rpc(e)) => {
            // The dispatcher rejects unknown prefixes with an
            // `Internal` error; we accept any Rpc error here.
            assert!(!e.is_retryable(), "stub error must be non-retryable");
        }
        other => panic!("expected Rpc error for stub, got {other:?}"),
    }
    handle.stop().await;
}

// ── Profile (a3net-userstore bridge) tests ───────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_get_returns_null_for_new_user() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let r = c
        .call_raw(A3chatRpcMethod::PROFILE_GET, serde_json::json!({}))
        .await
        .expect("PROFILE_GET must succeed");
    assert!(r.is_null(), "new user must have null profile, got {r:?}");
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_digit_get_returns_twelve_digits() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    let r = c
        .call_raw(A3chatRpcMethod::PROFILE_DIGIT_GET, serde_json::json!({}))
        .await
        .expect("PROFILE_DIGIT_GET must succeed");
    let s = r.as_str().expect("digit must be a string");
    assert_eq!(s.len(), 12, "digit must be 12 chars: {s:?}");
    assert!(s.chars().all(|c| c.is_ascii_digit()), "all digits: {s}");
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_public_key_add_then_list() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    // a3net-userstore enforces a FK — must create profile first.
    c.call_raw(
        A3chatRpcMethod::PROFILE_PUT,
        serde_json::json!({
            "userId": owner().as_str(),
            "username": "alice",
            "displayName": "Alice",
            "avatar": null,
            "bio": "",
            "preferences": {
                "theme": "auto",
                "locale": "en-US",
                "notificationsEnabled": true,
                "readReceiptsEnabled": true,
                "typingIndicatorsEnabled": true,
                "experimentalJson": "{}",
            },
            "createdAt": 0,
            "updatedAt": 0,
        }),
    )
    .await
    .expect("PROFILE_PUT must succeed");
    let key_id = c
        .call_raw(
            A3chatRpcMethod::PROFILE_PUBLIC_KEY_ADD,
            serde_json::json!({
                "algorithm": "ed25519",
                "key_material": "deadbeefcafe",
                "label": null,
            }),
        )
        .await
        .expect("PROFILE_PUBLIC_KEY_ADD must succeed");
    let kid = key_id.as_str().expect("key_id is string");
    assert!(!kid.is_empty(), "key_id must not be empty");
    let list = c
        .call_raw(
            A3chatRpcMethod::PROFILE_PUBLIC_KEY_LIST,
            serde_json::json!({}),
        )
        .await
        .expect("PROFILE_PUBLIC_KEY_LIST must succeed");
    let arr = list.as_array().expect("list must be array");
    assert_eq!(arr.len(), 1, "must have 1 key after add");
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_device_register_then_list() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    // FK pre-check requires profile.
    c.call_raw(
        A3chatRpcMethod::PROFILE_PUT,
        serde_json::json!({
            "userId": owner().as_str(),
            "username": "alice",
            "displayName": "Alice",
            "avatar": null,
            "bio": "",
            "preferences": {
                "theme": "auto",
                "locale": "en-US",
                "notificationsEnabled": true,
                "readReceiptsEnabled": true,
                "typingIndicatorsEnabled": true,
                "experimentalJson": "{}",
            },
            "createdAt": 0,
            "updatedAt": 0,
        }),
    )
    .await
    .expect("PROFILE_PUT must succeed");
    let id = c
        .call_raw(
            A3chatRpcMethod::PROFILE_DEVICE_REGISTER,
            serde_json::json!({
                "device_class": "mobile",
                "label": "test-iphone",
                "node_id": "node-1",
                "pairing_id": null,
            }),
        )
        .await
        .expect("PROFILE_DEVICE_REGISTER must succeed");
    let id_s = id.as_str().expect("device_id is string");
    assert_eq!(id_s.len(), 36, "device_id must be UUID: {id_s}");
    let list = c
        .call_raw(
            A3chatRpcMethod::PROFILE_DEVICE_LIST,
            serde_json::json!({}),
        )
        .await
        .expect("PROFILE_DEVICE_LIST must succeed");
    let arr = list.as_array().expect("list is array");
    assert_eq!(arr.len(), 1);
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_avatar_set_round_trip() {
    let (_dir, handle, cfg) = boot_daemon().await;
    let c = build_client(&cfg);
    // 1. Create a profile.
    c.call_raw(
        A3chatRpcMethod::PROFILE_PUT,
        serde_json::json!({
            "userId": owner().as_str(),
            "username": "alice",
            "displayName": "Alice",
            "avatar": null,
            "bio": "",
            "preferences": {
                "theme": "auto",
                "locale": "en-US",
                "notificationsEnabled": true,
                "readReceiptsEnabled": true,
                "typingIndicatorsEnabled": true,
                "experimentalJson": "{}",
            },
            "createdAt": 0,
            "updatedAt": 0,
        }),
    )
    .await
    .expect("PROFILE_PUT must succeed");
    // 2. Set avatar.
    c.call_raw(
        A3chatRpcMethod::PROFILE_AVATAR_SET,
        serde_json::json!({
            "blobHash": "abcdef0123456789",
            "mimeType": "image/png",
            "sizeBytes": 4096,
        }),
    )
    .await
    .expect("PROFILE_AVATAR_SET must succeed");
    // 3. Verify avatar is set.
    let r = c
        .call_raw(A3chatRpcMethod::PROFILE_GET, serde_json::json!({}))
        .await
        .expect("PROFILE_GET must succeed");
    let avatar = r.get("avatar").expect("profile has avatar");
    assert_eq!(avatar["blobHash"], "abcdef0123456789");
    assert_eq!(avatar["mimeType"], "image/png");
    handle.stop().await;
}