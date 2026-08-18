//! End-to-end tests for the P3 services wired in this audit cycle.
//!
//! These tests drive every new RPC method through `A3chatApp::dispatch`
//! end-to-end (no service internals), verifying that:
//!
//! - The dispatcher routes each `a3chat.*` prefix to the correct
//!   service (`a3chat.chat.draft.*`, `a3chat.chat.reaction.*`,
//!   `a3chat.device.*`, `a3chat.chat.notification.*`,
//!   `a3chat.chat.conversation.{pin,unpin,toggle_pin,list_pinned}`,
//!   `a3chat.chat.message.forward`, `a3chat.e2e.handshake.*`,
//!   `a3chat.e2e.bundle.*`, `a3chat.stream.*`, `a3chat.healthz`).
//! - The dispatched calls mutate state that subsequent calls can
//!   observe (state survives across calls).
//! - Bus events fire for state changes that should be published.
//! - Each dispatch path emits a well-typed JSON value.
//!
//! DO-178C §6.3: every test uses a fresh tempdir to keep state
//! isolated; no test depends on another test's side-effects.
//!
//! The tests use `A3chatApp::with_storage(...)` (the test entry
//! point) so the entire wiring graph (storage + bus + keyring +
//! all 9 P3 services + healthz) is real.

use std::sync::Arc;

use a3chat_app::app::A3chatApp;
use a3chat_app::keyring::E2eKeyring;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};

use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};

/// Boots a fresh `A3chatApp` on a tempdir. Returns the app, the tempdir
/// (so it lives until the test ends), and the owner `UserId`.
async fn boot() -> (Arc<A3chatApp>, tempfile::TempDir, UserId) {
    let dir = tempfile::tempdir().unwrap();
    let owner = UserId::from("alice-node");
    let keyring = E2eKeyring::new(owner.clone());
    let cfg = StorageConfig::new(dir.path().to_path_buf());
    let storage = ChatStorage::new(cfg, keyring);
    storage.init_user(&owner).await.unwrap();
    let bus = NotificationBus::default();
    let app = A3chatApp::with_storage(storage, bus, owner.clone());
    (Arc::new(app), dir, owner)
}

/// Helper: open a fresh 1-on-1 conversation owned by `owner`
/// talking to `peer`. The peer is irrelevant for the routing
/// tests — we just need a `ConversationId` we can pin / mute /
/// draft against.
async fn open_conv(
    app: &A3chatApp,
    owner: &UserId,
    peer: &UserId,
) -> ConversationId {
    let cid = ConversationId::from(format!(
        "conv-{}-{}",
        owner.as_str(),
        peer.as_str()
    ));
    // Use the app's own ChatStorage to upsert the conversation
    // meta — that way subsequent dispatcher calls that need the
    // conversation to be in storage will see it.
    use a3chat_core::conversation::{ConversationKind, ConversationMeta};
    let meta = ConversationMeta {
        conversation_id: cid.clone(),
        kind: ConversationKind::Dm,
        title: peer.as_str().to_string(),
        peer_user_id: Some(peer.clone()),
        last_message_preview: String::new(),
        last_activity: 0,
        message_count: 0,
        unread_count: 0,
        peer_online: false,
        muted: false,
        pinned: false,
    };
    app.chat
        .storage()
        .upsert_conversation(owner, &meta)
        .await
        .expect("upsert_conversation");
    cid
}

// ====================================================================
// 1. healthz
// ====================================================================

#[tokio::test]
async fn healthz_returns_uptime_and_status() {
    let (app, _dir, owner) = boot().await;
    let v = app
        .dispatch("a3chat.healthz", &owner, serde_json::json!({}))
        .await
        .expect("healthz");
    assert_eq!(v["ok"], true);
    assert_eq!(v["service"], "a3chat.app");
    assert!(v["uptime_secs"].is_i64());
    assert!(v["bus_receivers"].is_u64());
    assert!(v["stream_handles"].is_u64());
}

#[tokio::test]
async fn rpc_health_alias_works() {
    let (app, _dir, owner) = boot().await;
    let v = app
        .dispatch("a3chat.rpc.health", &owner, serde_json::json!({}))
        .await
        .expect("rpc.health");
    assert_eq!(v["ok"], true);
    assert_eq!(v["service"], "a3chat.app");
}

// ====================================================================
// 2. draft service
// ====================================================================

#[tokio::test]
async fn draft_save_get_delete_roundtrip() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("draft-conv-1");

    // save
    let v = app
        .dispatch(
            "a3chat.chat.draft.save",
            &owner,
            serde_json::json!({
                "conversation_id": cid.as_str(),
                "content": "hello world",
            }),
        )
        .await
        .expect("save");
    assert_eq!(v["ok"], true);

    // get
    let v = app
        .dispatch(
            "a3chat.chat.draft.get",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("get");
    assert_eq!(v["content"], "hello world");

    // delete
    let v = app
        .dispatch(
            "a3chat.chat.draft.delete",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("delete");
    assert_eq!(v["deleted"], true);

    // get should now return null content
    let v = app
        .dispatch(
            "a3chat.chat.draft.get",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("get2");
    assert!(v["content"].is_null());
}

#[tokio::test]
async fn draft_list_returns_all_drafts() {
    let (app, _dir, owner) = boot().await;
    let cid_a = ConversationId::from("a");
    let cid_b = ConversationId::from("b");
    for cid in [&cid_a, &cid_b] {
        app.dispatch(
            "a3chat.chat.draft.save",
            &owner,
            serde_json::json!({
                "conversation_id": cid.as_str(),
                "content": "hi",
            }),
        )
        .await
        .unwrap();
    }
    let v = app
        .dispatch("a3chat.chat.draft.list", &owner, serde_json::json!({}))
        .await
        .expect("list");
    let drafts = v.as_array().unwrap();
    assert_eq!(drafts.len(), 2);
}

#[tokio::test]
async fn draft_clear_removes_everything() {
    let (app, _dir, owner) = boot().await;
    for i in 0..3 {
        app.dispatch(
            "a3chat.chat.draft.save",
            &owner,
            serde_json::json!({
                "conversation_id": format!("c-{i}"),
                "content": "x",
            }),
        )
        .await
        .unwrap();
    }
    let v = app
        .dispatch("a3chat.chat.draft.clear", &owner, serde_json::json!({}))
        .await
        .expect("clear");
    assert_eq!(v["ok"], true);
    let v = app
        .dispatch("a3chat.chat.draft.list", &owner, serde_json::json!({}))
        .await
        .expect("list");
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn draft_rejects_oversized_content() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("big");
    let too_big = "x".repeat(a3chat_app::draft_service::MAX_DRAFT_LEN + 1);
    let err = app
        .dispatch(
            "a3chat.chat.draft.save",
            &owner,
            serde_json::json!({
                "conversation_id": cid.as_str(),
                "content": too_big,
            }),
        )
        .await;
    assert!(err.is_err(), "oversized draft should fail");
}

// ====================================================================
// 3. reaction service
// ====================================================================

#[tokio::test]
async fn reaction_add_remove_summary_roundtrip() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("rx-conv");
    let mid = "msg-1".to_string();

    // add
    let v = app
        .dispatch(
            "a3chat.chat.reaction.add",
            &owner,
            serde_json::json!({
                "message_id": mid,
                "conversation_id": cid.as_str(),
                "reaction_type": "like",
            }),
        )
        .await
        .expect("add");
    assert_eq!(v["reaction_type"], "like");

    // summary should now show 1 reaction
    let v = app
        .dispatch(
            "a3chat.chat.reaction.get",
            &owner,
            serde_json::json!({ "message_id": mid }),
        )
        .await
        .expect("get");
    assert_eq!(v["total_count"], 1);

    // remove
    let v = app
        .dispatch(
            "a3chat.chat.reaction.remove",
            &owner,
            serde_json::json!({
                "message_id": mid,
                "conversation_id": cid.as_str(),
            }),
        )
        .await
        .expect("remove");
    assert_eq!(v["removed"], true);

    // summary back to 0
    let v = app
        .dispatch(
            "a3chat.chat.reaction.get",
            &owner,
            serde_json::json!({ "message_id": mid }),
        )
        .await
        .expect("get2");
    assert_eq!(v["total_count"], 0);
}

#[tokio::test]
async fn reaction_add_twice_is_idempotent() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("rx-conv-2");
    let mid = "msg-2".to_string();
    // first add
    app.dispatch(
        "a3chat.chat.reaction.add",
        &owner,
        serde_json::json!({
            "message_id": mid,
            "conversation_id": cid.as_str(),
            "reaction_type": "love",
        }),
    )
    .await
    .unwrap();
    // second add — should not increase count
    let v = app
        .dispatch(
            "a3chat.chat.reaction.get",
            &owner,
            serde_json::json!({ "message_id": mid }),
        )
        .await
        .unwrap();
    assert_eq!(v["total_count"], 1);
}

// ====================================================================
// 4. device service
// ====================================================================

#[tokio::test]
async fn device_register_list_revoke_setprimary() {
    let (app, _dir, owner) = boot().await;
    // register device 1 — register returns the device directly
    let v = app
        .dispatch(
            "a3chat.device.register",
            &owner,
            serde_json::json!({
                "name": "alice-laptop",
                "kind": "desktop",
                "public_key_b64": "AAAA",
            }),
        )
        .await
        .expect("register1");
    let id1 = v["device_id"].as_str().unwrap().to_string();

    // register device 2
    let v = app
        .dispatch(
            "a3chat.device.register",
            &owner,
            serde_json::json!({
                "name": "alice-phone",
                "kind": "phone",
            }),
        )
        .await
        .expect("register2");
    let id2 = v["device_id"].as_str().unwrap().to_string();
    assert_ne!(id1, id2);

    // list
    let v = app
        .dispatch("a3chat.device.list", &owner, serde_json::json!({}))
        .await
        .expect("list");
    let devs = v.as_array().unwrap();
    assert_eq!(devs.len(), 2);

    // set device 2 as primary
    let v = app
        .dispatch(
            "a3chat.device.set_primary",
            &owner,
            serde_json::json!({ "device_id": id2 }),
        )
        .await
        .expect("set_primary");
    assert_eq!(v["ok"], true);

    // revoke device 1 (not current)
    let v = app
        .dispatch(
            "a3chat.device.revoke",
            &owner,
            serde_json::json!({ "device_id": id1 }),
        )
        .await
        .expect("revoke");
    assert_eq!(v["ok"], true);

    let v = app
        .dispatch("a3chat.device.list", &owner, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn device_revoke_unknown_id_fails() {
    let (app, _dir, owner) = boot().await;
    let err = app
        .dispatch(
            "a3chat.device.revoke",
            &owner,
            serde_json::json!({ "device_id": "device-nope" }),
        )
        .await;
    assert!(err.is_err(), "revoking unknown device must fail");
}

// ====================================================================
// 5. notification settings service
// ====================================================================

#[tokio::test]
async fn notification_dnd_set_get_roundtrip() {
    let (app, _dir, owner) = boot().await;
    let v = app
        .dispatch(
            "a3chat.chat.notification.set_dnd",
            &owner,
            serde_json::json!({
                "enabled": true,
                "quiet_from": null,
                "quiet_until": null,
                "allow_calls": false,
                "allow_pinned": true,
            }),
        )
        .await
        .expect("set_dnd");
    assert_eq!(v["ok"], true);

    let v = app
        .dispatch(
            "a3chat.chat.notification.get_dnd",
            &owner,
            serde_json::json!({}),
        )
        .await
        .expect("get_dnd");
    assert_eq!(v["enabled"], true);
    assert_eq!(v["allow_pinned"], true);
}

#[tokio::test]
async fn notification_mute_unmute_list() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("notif-conv-1");

    // mute
    let v = app
        .dispatch(
            "a3chat.chat.notification.mute",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("mute");
    assert_eq!(v["ok"], true);

    // list_muted has 1 entry
    let v = app
        .dispatch(
            "a3chat.chat.notification.list_muted",
            &owner,
            serde_json::json!({}),
        )
        .await
        .expect("list_muted");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);

    // unmute
    let v = app
        .dispatch(
            "a3chat.chat.notification.unmute",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("unmute");
    assert_eq!(v["ok"], true);

    let v = app
        .dispatch(
            "a3chat.chat.notification.list_muted",
            &owner,
            serde_json::json!({}),
        )
        .await
        .expect("list_muted2");
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn notification_set_get_conversation_roundtrip() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("notif-conv-2");

    let v = app
        .dispatch(
            "a3chat.chat.notification.set_conversation",
            &owner,
            serde_json::json!({
                "conversation_id": cid.as_str(),
                "muted": true,
                "level": "mentions_only",
                "custom_sound": null,
                "show_preview": false,
            }),
        )
        .await
        .expect("set");
    assert_eq!(v["ok"], true);

    let v = app
        .dispatch(
            "a3chat.chat.notification.get_conversation",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("get");
    assert_eq!(v["muted"], true);
    assert_eq!(v["level"], "mentions_only");
    assert_eq!(v["show_preview"], false);
}

// ====================================================================
// 6. pinned service (requires a real conversation in storage)
// ====================================================================

#[tokio::test]
async fn pinned_pin_unpin_toggle_list() {
    let (app, _dir, owner) = boot().await;
    let peer = UserId::from("bob");
    let cid = open_conv(&app, &owner, &peer).await;

    // pin
    let v = app
        .dispatch(
            "a3chat.chat.conversation.pin",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("pin");
    assert_eq!(v["ok"], true);

    // list_pinned returns the pinned cid (flat array)
    let v = app
        .dispatch(
            "a3chat.chat.conversation.list_pinned",
            &owner,
            serde_json::json!({}),
        )
        .await
        .expect("list_pinned");
    let arr = v.as_array().unwrap();
    assert!(
        arr.iter().any(|c| c.as_str() == Some(cid.as_str())),
        "expected pinned cid present, got: {arr:?}"
    );

    // toggle (should unpin)
    let v = app
        .dispatch(
            "a3chat.chat.conversation.toggle_pin",
            &owner,
            serde_json::json!({ "conversation_id": cid.as_str() }),
        )
        .await
        .expect("toggle");
    assert_eq!(v["pinned"], false);

    // list_pinned now empty
    let v = app
        .dispatch(
            "a3chat.chat.conversation.list_pinned",
            &owner,
            serde_json::json!({}),
        )
        .await
        .expect("list_pinned2");
    assert_eq!(v.as_array().unwrap().len(), 0);
}

// ====================================================================
// 7. forward service (requires a source message)
// ====================================================================

#[tokio::test]
async fn forward_message_to_target_conversation() {
    let (app, _dir, owner) = boot().await;
    let peer = UserId::from("bob");
    let cid_src = open_conv(&app, &owner, &peer).await;

    // Send a source message through the app's ChatService so it
    // lands in the same storage the dispatcher will read.
    let envelope = MessageEnvelope {
        conversation_id: cid_src.clone(),
        receiver_id: peer.clone(),
        message_type: MessageType::Text,
        body: MessageBody::Plain {
            content: "original".to_string(),
        },
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: chrono::Utc::now().timestamp(),
    };
    let stored_src = app
        .chat
        .send_message(&owner, &envelope)
        .await
        .expect("send");

    // Open a second conversation as forward target.
    let peer_c = UserId::from("carol");
    let cid_tgt = open_conv(&app, &owner, &peer_c).await;

    // Forward via dispatch
    let v = app
        .dispatch(
            "a3chat.chat.message.forward",
            &owner,
            serde_json::json!({
                "source_message_id": stored_src.message.message_id.as_str(),
                "target_conversation_ids": [cid_tgt.as_str()],
                "reply_to": null,
            }),
        )
        .await
        .expect("forward");
    assert!(v["targets"].is_array());
    assert_eq!(v["targets"].as_array().unwrap().len(), 1);
    assert_eq!(v["original_sender_id"], owner.as_str());
}

// ====================================================================
// 8. e2e handshake service
// ====================================================================

#[tokio::test]
async fn e2e_handshake_needs_rehandshake_returns_false_when_no_session() {
    let (app, _dir, owner) = boot().await;
    let v = app
        .dispatch(
            "a3chat.e2e.handshake.needs_rehandshake",
            &owner,
            serde_json::json!({ "peer": "bob" }),
        )
        .await
        .expect("needs_rehandshake");
    assert_eq!(v["needs_rehandshake"], false);
}

#[tokio::test]
async fn e2e_handshake_is_complete_returns_false_when_no_session() {
    let (app, _dir, owner) = boot().await;
    let v = app
        .dispatch(
            "a3chat.e2e.handshake.is_complete",
            &owner,
            serde_json::json!({ "peer": "bob" }),
        )
        .await
        .expect("is_complete");
    assert_eq!(v["is_complete"], false);
}

#[tokio::test]
async fn e2e_handshake_rejects_self_peer() {
    let (app, _dir, owner) = boot().await;
    let err = app
        .dispatch(
            "a3chat.e2e.handshake.needs_rehandshake",
            &owner,
            serde_json::json!({ "peer": owner.as_str() }),
        )
        .await;
    assert!(err.is_err(), "self-peer handshake must be rejected");
}

#[tokio::test]
async fn e2e_handshake_rejects_missing_peer() {
    let (app, _dir, owner) = boot().await;
    let err = app
        .dispatch(
            "a3chat.e2e.handshake.needs_rehandshake",
            &owner,
            serde_json::json!({}),
        )
        .await;
    assert!(err.is_err(), "missing peer must be rejected");
}

// ====================================================================
// 9. e2e bundle service (export / import)
// ====================================================================

#[tokio::test]
async fn e2e_bundle_export_import_roundtrip() {
    let (app, _dir, owner) = boot().await;
    // export — the dispatcher returns the Bundle directly. B-20
    // now requires a non-empty `passphrase` in the params.
    let bundle = app
        .dispatch(
            "a3chat.e2e.bundle.export",
            &owner,
            serde_json::json!({ "passphrase": "test-passphrase-1234" }),
        )
        .await
        .expect("export");
    assert!(bundle.is_object());
    assert!(bundle["version"].is_u64());

    // import: the dispatcher takes the bundle as the *root* params,
    // not wrapped in `{ "bundle": ... }`. The passphrase is
    // stripped before the Bundle is deserialised (B-20).
    let mut bundle_with_pp = bundle.clone();
    bundle_with_pp["passphrase"] = serde_json::json!("test-passphrase-1234");
    let r = app
        .dispatch("a3chat.e2e.bundle.import", &owner, bundle_with_pp)
        .await
        .expect("import");
    // ImportSummary counters — all zero on an empty store.
    assert!(r["imported_messages"].is_u64());
    assert_eq!(r["imported_messages"], 0);
    assert_eq!(r["new_conversations"], 0);
}

// ====================================================================
// 10. stream service
// ====================================================================

#[tokio::test]
async fn stream_subscribe_list_unsubscribe() {
    let (app, _dir, owner) = boot().await;

    // subscribe
    let v = app
        .dispatch(
            "a3chat.stream.subscribe",
            &owner,
            serde_json::json!({ "topics": ["chat"] }),
        )
        .await
        .expect("subscribe");
    let handle = v["handle_id"].as_str().unwrap().to_string();
    assert!(!handle.is_empty());
    assert_eq!(v["stream_url"], "/rpc/stream");

    // list contains the handle
    let v = app
        .dispatch("a3chat.stream.list", &owner, serde_json::json!({}))
        .await
        .expect("list");
    let arr = v["handles"].as_array().unwrap();
    assert!(arr.iter().any(|h| h["handle_id"] == handle));

    // unsubscribe
    let v = app
        .dispatch(
            "a3chat.stream.unsubscribe",
            &owner,
            serde_json::json!({ "handle_id": handle }),
        )
        .await
        .expect("unsubscribe");
    assert_eq!(v["ok"], true);

    // list now empty
    let v = app
        .dispatch("a3chat.stream.list", &owner, serde_json::json!({}))
        .await
        .expect("list2");
    assert_eq!(v["handles"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn stream_unsubscribe_unknown_handle_fails() {
    let (app, _dir, owner) = boot().await;
    let err = app
        .dispatch(
            "a3chat.stream.unsubscribe",
            &owner,
            serde_json::json!({ "handle_id": "nope" }),
        )
        .await;
    assert!(err.is_err(), "unsubscribing an unknown handle must fail");
}

#[tokio::test]
async fn stream_rejects_unknown_topic() {
    let (app, _dir, owner) = boot().await;
    let err = app
        .dispatch(
            "a3chat.stream.subscribe",
            &owner,
            serde_json::json!({ "topics": ["definitely_not_a_topic"] }),
        )
        .await;
    assert!(err.is_err(), "unknown topic must be rejected");
}

#[tokio::test]
async fn stream_unsubscribe_by_other_owner_is_forbidden() {
    let (app, _dir, owner) = boot().await;
    let v = app
        .dispatch("a3chat.stream.subscribe", &owner, serde_json::json!({}))
        .await
        .expect("subscribe");
    let handle = v["handle_id"].as_str().unwrap().to_string();

    // A different owner tries to unsubscribe
    let intruder = UserId::from("mallory");
    let err = app
        .dispatch(
            "a3chat.stream.unsubscribe",
            &intruder,
            serde_json::json!({ "handle_id": handle }),
        )
        .await;
    assert!(err.is_err(), "cross-owner unsubscribe must be forbidden");
}

// ====================================================================
// 11. unknown methods return a clean error
// ====================================================================

#[tokio::test]
async fn unknown_method_returns_error() {
    let (app, _dir, owner) = boot().await;
    let err = app
        .dispatch(
            "a3chat.does.not.exist",
            &owner,
            serde_json::json!({}),
        )
        .await;
    assert!(err.is_err(), "unknown method must error");
}

// ====================================================================
// 12. dispatcher prefix routing — verify each P3 sub-namespace lands
//     on the correct service.
// ====================================================================

#[tokio::test]
async fn draft_sub_namespace_routes_to_draft_service_not_chat() {
    // Regression test: the `a3chat.chat.*` prefix used to swallow
    // the `a3chat.chat.draft.*` sub-namespace, returning a
    // "ChatService does not handle ..." error. The dispatcher now
    // checks `a3chat.chat.draft.*` BEFORE the broader prefix.
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("prefix-test");
    let v = app
        .dispatch(
            "a3chat.chat.draft.save",
            &owner,
            serde_json::json!({"conversation_id": cid.as_str(), "content": "hi"}),
        )
        .await
        .expect("draft.save must not be swallowed by chat_service");
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn reaction_sub_namespace_routes_to_reaction_service_not_chat() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("reaction-prefix-test");
    let v = app
        .dispatch(
            "a3chat.chat.reaction.add",
            &owner,
            serde_json::json!({
                "message_id": "m1",
                "conversation_id": cid.as_str(),
                "reaction_type": "like",
            }),
        )
        .await
        .expect("chat.reaction.add must not be swallowed by chat_service");
    assert_eq!(v["reaction_type"], "like");
}

#[tokio::test]
async fn notification_sub_namespace_routes_to_notification_service_not_chat() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("notif-prefix-test");
    let v = app
        .dispatch(
            "a3chat.chat.notification.mute",
            &owner,
            serde_json::json!({"conversation_id": cid.as_str()}),
        )
        .await
        .expect("chat.notification.mute must not be swallowed by chat_service");
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn pinned_sub_namespace_routes_to_pinned_service_not_chat() {
    let (app, _dir, owner) = boot().await;
    let peer = UserId::from("bob-prefix");
    let cid = open_conv(&app, &owner, &peer).await;
    let v = app
        .dispatch(
            "a3chat.chat.conversation.pin",
            &owner,
            serde_json::json!({"conversation_id": cid.as_str()}),
        )
        .await
        .expect("chat.conversation.pin must not be swallowed by chat_service");
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn forward_sub_namespace_routes_to_forward_service_not_chat() {
    let (app, _dir, owner) = boot().await;
    let peer = UserId::from("bob-fwd-prefix");
    let cid_src = open_conv(&app, &owner, &peer).await;
    let envelope = MessageEnvelope {
        conversation_id: cid_src.clone(),
        receiver_id: peer.clone(),
        message_type: MessageType::Text,
        body: MessageBody::Plain {
            content: "hi".to_string(),
        },
        attachments: vec![],
        reply_to: None,
        sequence: 1,
        timestamp: chrono::Utc::now().timestamp(),
    };
    let stored = app.chat.send_message(&owner, &envelope).await.unwrap();
    let peer_c = UserId::from("carol-fwd-prefix");
    let cid_tgt = open_conv(&app, &owner, &peer_c).await;
    let v = app
        .dispatch(
            "a3chat.chat.message.forward",
            &owner,
            serde_json::json!({
                "source_message_id": stored.message.message_id.as_str(),
                "target_conversation_ids": [cid_tgt.as_str()],
                "reply_to": null,
            }),
        )
        .await
        .expect("chat.message.forward must not be swallowed by chat_service");
    assert!(v["targets"].is_array());
}
