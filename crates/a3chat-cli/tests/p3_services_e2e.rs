//! CLI golden tests for the P3 RPC commands.
//!
//! Each test boots an in-process daemon, then exercises the wire
//! surface of one of the newly wired sub-namespaces:
//!
//! - `a3chat.chat.draft.*`
//! - `a3chat.chat.reaction.*`
//! - `a3chat.chat.conversation.pin / unpin / toggle_pin / list_pinned`
//! - `a3chat.chat.message.forward`
//! - `a3chat.chat.notification.{set_dnd, get_dnd, mute, unmute, list_muted}`
//! - `a3chat.device.{register, list, get, revoke, set_primary, get_current, touch}`
//! - `a3chat.stream.{subscribe, list, unsubscribe}`
//! - `a3chat.e2e.bundle.{export, import}`
//! - `a3chat.e2e.handshake.{needs_rehandshake, is_complete}`
//!
//! The "golden" assertion model is intentionally small: the tests
//! only assert on the *shape* of the JSON response (top-level
//! keys, array/object types) rather than replaying full snapshots.
//! That keeps them cheap to maintain while still catching
//! regressions where a CLI command silently loses or renames a
//! field.

use a3chat_app::storage::StorageConfig;
use a3chat_app::A3chatApp;
use a3chat_cli::config::CliConfig;
use a3chat_cli::rpc_client::{HttpRpcClient, RpcClientBuilder};
use a3chat_core::id::{ConversationId, MessageId, UserId};
use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};
use a3chat_core::rpc::A3chatRpcMethod;
use a3chat_rpc::{RpcServer, RpcServerConfig};

fn owner() -> UserId {
    UserId::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
}

fn peer() -> UserId {
    UserId::from(
        "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
    )
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
    let handle = server.start().await.expect("start");
    let cfg = CliConfig {
        daemon_url: Some(format!("http://{}", handle.local_addr)),
        owner: Some(owner().to_string()),
        output: None,
        retries: Some(1),
        timeout_ms: Some(5000),
    };
    (dir, handle, cfg)
}

fn client(cfg: &CliConfig) -> HttpRpcClient {
    RpcClientBuilder::new(cfg).build().expect("client")
}

async fn shutdown(handle: a3chat_rpc::RpcServerHandle) {
    let _ = handle.stop().await;
}

// ─── Drafts ─────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_save_then_get_then_delete() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let cid = ConversationId::from("dm:draft-conv-1");

    // Save
    let saved: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_DRAFT_SAVE,
            serde_json::json!({
                "conversation_id": cid.as_str(),
                "content": "draft text",
            }),
        )
        .await
        .expect("save");
    assert_eq!(saved.get("ok").and_then(|v| v.as_bool()), Some(true));

    // Get returns the Draft object directly (content lives on the
    // serialized struct).
    let got: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_DRAFT_GET,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("get");
    assert_eq!(got["content"], "draft text");
    assert_eq!(got["conversation_id"], cid.as_str());

    // List returns an array (possibly empty for unrelated).
    let _list: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_DRAFT_LIST,
            serde_json::json!({}),
        )
        .await
        .expect("list");

    // Delete returns `{ deleted: bool }`.
    let deleted: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_DRAFT_DELETE,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("delete");
    assert!(deleted.get("deleted").is_some());
    shutdown(h).await;
}

// ─── Reactions ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reaction_add_summary_remove_round_trip() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let mid = MessageId::from("r-msg-1");
    let cid = ConversationId::from("dm:reactions:cli");

    // add requires `conversation_id` + `message_id` + `reaction_type`
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_REACTION_ADD,
            serde_json::json!({
                "message_id": mid.as_str(),
                "conversation_id": cid.as_str(),
                "reaction_type": "like",
            }),
        )
        .await
        .expect("add");
    // The payload is the serialized ReactionRecord — must at least
    // carry the message_id we passed.
    assert_eq!(v["message_id"], mid.as_str());

    let summary: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_REACTION_GET,
            serde_json::json!({ "message_id": mid.as_str() }),
        )
        .await
        .expect("summary");
    assert!(summary.get("message_id").is_some());

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_REACTION_REMOVE,
            serde_json::json!({
                "message_id": mid.as_str(),
                "conversation_id": cid.as_str(),
            }),
        )
        .await
        .expect("remove");
    assert!(v.get("removed").is_some());
    shutdown(h).await;
}

// ─── Pinned conversations ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinned_pin_unpin_toggle_list_roundtrip() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let cid = ConversationId::from("dm:pin-conv-1");

    // Materialize the conversation first by sending a message; the
    // storage layer refuses to pin a conversation that does not
    // exist (avoids dangling pinned-state).
    let env = MessageEnvelope {
        conversation_id: cid.clone(),
        receiver_id: peer(),
        message_type: MessageType::Text,
        body: MessageBody::Plain {
            content: "ping".into(),
        },
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: 1_700_000_000,
    };
    let _sent: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_SEND,
            serde_json::to_value(&env).expect("encode envelope"),
        )
        .await
        .expect("send");

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_PIN,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("pin");
    assert_eq!(v.get("pinned").and_then(|x| x.as_bool()), Some(true));

    let list: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_LIST_PINNED,
            serde_json::json!({}),
        )
        .await
        .expect("list");
    assert!(list.is_array());

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_TOGGLE_PIN,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("toggle");
    assert_eq!(v.get("pinned").and_then(|x| x.as_bool()), Some(false));

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_UNPIN,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("unpin");
    assert_eq!(v.get("pinned").and_then(|x| x.as_bool()), Some(false));
    shutdown(h).await;
}

// ─── Notification settings ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_dnd_get_set_roundtrip() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    // DndSettings uses `chrono::DateTime<Utc>` — wire format must
    // be RFC-3339 strings (or absent).
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_NOTIFICATION_SET_DND,
            serde_json::json!({
                "enabled": true,
                "quiet_from": "2024-01-01T22:00:00Z",
                "quiet_until": "2024-01-02T07:00:00Z",
                "allow_calls": false,
                "allow_pinned": true,
            }),
        )
        .await
        .expect("set_dnd");
    assert_eq!(v.get("ok").and_then(|v| v.as_bool()), Some(true));
    let got: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_NOTIFICATION_GET_DND,
            serde_json::json!({}),
        )
        .await
        .expect("get_dnd");
    assert_eq!(got.get("enabled").and_then(|v| v.as_bool()), Some(true));
    shutdown(h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_mute_unmute_list() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let cid = ConversationId::from("dm:notif-mute-cli");

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_NOTIFICATION_MUTE,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("mute");
    assert_eq!(v.get("ok").and_then(|v| v.as_bool()), Some(true));

    let list: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_NOTIFICATION_LIST_MUTED,
            serde_json::json!({}),
        )
        .await
        .expect("list_muted");
    assert!(list.is_array());

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CHAT_NOTIFICATION_UNMUTE,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("unmute");
    assert_eq!(v.get("ok").and_then(|v| v.as_bool()), Some(true));
    shutdown(h).await;
}

// ─── Devices ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_register_list_get_set_primary_touch_current() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::DEVICE_REGISTER,
            serde_json::json!({
                "name": "cli-test-laptop",
                "public_key_b64": "AAAA",
                "kind": "desktop",
            }),
        )
        .await
        .expect("register");
    let device_id = v["device_id"]
        .as_str()
        .expect("device_id is string")
        .to_string();

    let list: serde_json::Value = c
        .call(A3chatRpcMethod::DEVICE_LIST, serde_json::json!({}))
        .await
        .expect("list");
    assert!(list.is_array());

    let got: serde_json::Value = c
        .call(
            A3chatRpcMethod::DEVICE_GET,
            serde_json::json!({ "device_id": device_id }),
        )
        .await
        .expect("get");
    assert_eq!(got["device_id"], device_id);

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::DEVICE_SET_PRIMARY,
            serde_json::json!({ "device_id": device_id }),
        )
        .await
        .expect("set_primary");
    assert_eq!(v.get("ok").and_then(|v| v.as_bool()), Some(true));

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::DEVICE_TOUCH,
            serde_json::json!({ "device_id": device_id }),
        )
        .await
        .expect("touch");
    assert_eq!(v.get("ok").and_then(|v| v.as_bool()), Some(true));

    let v: serde_json::Value = c
        .call(A3chatRpcMethod::DEVICE_GET_CURRENT, serde_json::json!({}))
        .await
        .expect("current");
    // The dispatcher returns the bare DeviceId string (not an
    // object envelope) for backward compatibility with existing
    // Tauri/CLI consumers.
    assert_eq!(
        v.as_str().unwrap_or_default(),
        device_id,
        "get_current must echo the most-recently-registered device id"
    );
    shutdown(h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_register_rejects_oversized_name() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::DEVICE_REGISTER,
            serde_json::json!({
                "name": "x".repeat(200),
                "public_key_b64": "AAAA",
                "kind": "desktop",
            }),
        )
        .await;
    assert!(r.is_err(), "oversized device name must be rejected");
    shutdown(h).await;
}

// ─── Stream ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_subscribe_list_unsubscribe() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::STREAM_SUBSCRIBE,
            serde_json::json!({ "topics": ["chat"] }),
        )
        .await
        .expect("subscribe");
    let handle_id = v["handle_id"]
        .as_str()
        .expect("handle_id is string")
        .to_string();

    let list: serde_json::Value = c
        .call("a3chat.stream.list", serde_json::json!({}))
        .await
        .expect("list");
    assert!(list.get("handles").is_some());

    let v: serde_json::Value = c
        .call(
            "a3chat.stream.unsubscribe",
            serde_json::json!({ "handle_id": handle_id }),
        )
        .await
        .expect("unsub");
    assert_eq!(v.get("ok").and_then(|v| v.as_bool()), Some(true));
    shutdown(h).await;
}

// ─── E2E bundle ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_bundle_export_returns_envelope() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let v: serde_json::Value = c
        .call(A3chatRpcMethod::E2E_BUNDLE_EXPORT, serde_json::json!({}))
        .await
        .expect("export");
    // Bundle envelope: { version, owner, exported_at_unix, kdf_params,
    // salt_b64, nonce_b64, payload_b64 }.
    assert!(v.get("payload_b64").is_some());
    assert!(v.get("nonce_b64").is_some());
    assert!(v.get("salt_b64").is_some());
    assert!(v.get("version").is_some());
    shutdown(h).await;
}

// ─── E2E handshake introspection ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_handshake_introspection_reports_no_session() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::E2E_NEEDS_REHANDSHAKE,
            serde_json::json!({ "peer": peer().as_str() }),
        )
        .await
        .expect("needs");
    assert!(v.get("state").is_some());
    assert_eq!(v["state"], "no_session");
    shutdown(h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_handshake_introspection_rejects_self_peer() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::E2E_NEEDS_REHANDSHAKE,
            serde_json::json!({ "peer": owner().as_str() }),
        )
        .await;
    assert!(r.is_err(), "self-as-peer must be rejected");
    shutdown(h).await;
}

// ─── Forward ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forward_rejects_duplicate_targets() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::CHAT_MESSAGE_FORWARD,
            serde_json::json!({
                "source_message_id": "src-msg",
                "target_conversation_ids": ["dm:x:y", "dm:x:y"],
            }),
        )
        .await;
    assert!(r.is_err(), "duplicate targets must be rejected");
    shutdown(h).await;
}

// ─── Contacts (L5 — CLI integration against the daemon) ────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contact_full_roundtrip_via_client() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let peer_hex = "f".repeat(64);

    // 1. empty list
    let list: serde_json::Value = c
        .call(A3chatRpcMethod::CONTACT_LIST, serde_json::json!({}))
        .await
        .expect("list");
    assert_eq!(list["contacts"].as_array().map(|a| a.len()), Some(0));

    // 2. add
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_ADD,
            serde_json::json!({
                "user_id": peer_hex,
                "display_name": "Carol",
                "note": "vip",
                "is_favorite": false,
                "is_blocked": false,
                "added_at": "2026-01-01T00:00:00Z",
            }),
        )
        .await
        .expect("add");
    assert_eq!(v["user_id"], peer_hex);

    // 3. list (one)
    let list: serde_json::Value = c
        .call(A3chatRpcMethod::CONTACT_LIST, serde_json::json!({}))
        .await
        .expect("list");
    assert_eq!(list["contacts"].as_array().unwrap().len(), 1);

    // 4. get
    let got: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_GET,
            serde_json::json!({ "contact_id": peer_hex }),
        )
        .await
        .expect("get");
    assert_eq!(got["display_name"], "Carol");

    // 5. search
    let s: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_SEARCH,
            serde_json::json!({ "query": "carol" }),
        )
        .await
        .expect("search");
    assert_eq!(s.as_array().unwrap().len(), 1);

    // 6. toggle favorite twice
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_TOGGLE_FAVORITE,
            serde_json::json!({ "contact_id": peer_hex }),
        )
        .await
        .expect("toggle1");
    assert_eq!(v, serde_json::json!(true));
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_TOGGLE_FAVORITE,
            serde_json::json!({ "contact_id": peer_hex }),
        )
        .await
        .expect("toggle2");
    assert_eq!(v, serde_json::json!(false));

    // 7. block / unblock
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_BLOCK,
            serde_json::json!({ "user_id": peer_hex }),
        )
        .await
        .expect("block");
    assert_eq!(v["user_id"], peer_hex);
    let list: serde_json::Value = c
        .call(A3chatRpcMethod::CONTACT_LIST, serde_json::json!({}))
        .await
        .expect("list");
    assert_eq!(list["blocklist"].as_array().unwrap().len(), 1);

    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_UNBLOCK,
            serde_json::json!({ "user_id": peer_hex }),
        )
        .await
        .expect("unblock");
    assert_eq!(v["ok"], true);

    // 8. update
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_UPDATE,
            serde_json::json!({
                "user_id": peer_hex,
                "display_name": "Caroline",
                "note": "promoted",
                "is_favorite": false,
                "is_blocked": false,
                "added_at": "2026-01-01T00:00:00Z",
            }),
        )
        .await
        .expect("update");
    assert_eq!(v["display_name"], "Caroline");

    // 9. qr_invite
    let v: serde_json::Value = c
        .call(A3chatRpcMethod::CONTACT_QR_INVITE, serde_json::json!({}))
        .await
        .expect("qr");
    assert!(
        v["qr_payload"].as_str().is_some(),
        "qr_invite must return qr_payload envelope; got {v}"
    );

    // 10. remove
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_REMOVE,
            serde_json::json!({ "contact_id": peer_hex }),
        )
        .await
        .expect("remove");
    assert_eq!(v["removed"], true);
    let list: serde_json::Value = c
        .call(A3chatRpcMethod::CONTACT_LIST, serde_json::json!({}))
        .await
        .expect("list");
    assert_eq!(list["contacts"].as_array().unwrap().len(), 0);

    shutdown(h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contact_friend_request_roundtrip_via_client() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    let peer_hex = "e".repeat(64);

    // send a friend request
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_ADD_REQUEST,
            serde_json::json!({ "to_user_id": peer_hex, "message": "hi" }),
        )
        .await
        .expect("add_request");
    let request_id = v["request_id"]
        .as_str()
        .expect("request_id present")
        .to_string();
    assert_eq!(v["status"], "pending");
    assert_eq!(v["from_user_id"], owner().as_str());

    // oversize message → error
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::CONTACT_ADD_REQUEST,
            serde_json::json!({ "to_user_id": peer_hex, "message": "x".repeat(300) }),
        )
        .await;
    assert!(r.is_err(), "oversize message must be rejected");

    // accept (signature check intentionally not yet wired)
    let v: serde_json::Value = c
        .call(
            A3chatRpcMethod::CONTACT_ACCEPT_REQUEST,
            serde_json::json!({
                "request": {
                    "request_id": request_id,
                    "from_user_id": peer_hex,
                    "from_display_name": "Peer",
                    "to_user_id": owner().as_str(),
                    "message": "hi",
                    "status": "pending",
                    "created_at": "2026-01-01T00:00:00Z",
                    "responded_at": null,
                }
            }),
        )
        .await
        .expect("accept");
    assert_eq!(v["user_id"], peer_hex);

    shutdown(h).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contact_dispatch_rejects_missing_required_field() {
    let (_dir, h, cfg) = boot_daemon().await;
    let c = client(&cfg);
    // Contact envelope with a missing required field (`display_name`)
    // must surface as a validation error.
    let r = c
        .call::<serde_json::Value, serde_json::Value>(
            A3chatRpcMethod::CONTACT_ADD,
            serde_json::json!({
                "user_id": "a".repeat(64),
                "note": "",
                "is_favorite": false,
                "is_blocked": false,
                "added_at": "2026-01-01T00:00:00Z",
            }),
        )
        .await;
    assert!(
        r.is_err(),
        "missing `display_name` must be rejected by the dispatcher"
    );
    shutdown(h).await;
}
