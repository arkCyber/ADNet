//! End-to-end bus-event tests for the P3 services.
//!
//! Pattern: subscribe to the in-process bus BEFORE performing the
//! state change, then assert the expected `A3chatEvent` variant
//! arrives on the receiver.
//!
//! These tests do NOT use the SSE bridge — they observe the bus
//! directly. This keeps the tests fast and deterministic while
//! still proving that:
//!
//! 1. The service emits the correct event variant.
//! 2. The event payload carries the right identifiers
//!    (conversation_id, device_id, message_id, etc.).
//! 3. Multiple events can be received in the right order.
//!
//! Together with `crates/a3chat-rpc/src/sse.rs::tests`, which
//! verifies each event variant serializes correctly to SSE, these
//! tests cover the full `service → bus → SSE` chain.

use std::sync::Arc;
use std::time::Duration;

use a3chat_app::app::A3chatApp;
use a3chat_app::keyring::E2eKeyring;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};

use a3chat_core::event::A3chatEvent;
use a3chat_core::id::{ConversationId, MessageId, UserId};

async fn boot() -> (Arc<A3chatApp>, tempfile::TempDir, UserId) {
    let dir = tempfile::tempdir().unwrap();
    let owner = UserId::from("alice-bus-test");
    let keyring = E2eKeyring::new(owner.clone());
    let cfg = StorageConfig::new(dir.path().to_path_buf());
    let storage = ChatStorage::new(cfg, keyring);
    storage.init_user(&owner).await.unwrap();
    let bus = NotificationBus::default();
    let app = A3chatApp::with_storage(storage, bus, owner.clone());
    (Arc::new(app), dir, owner)
}

async fn next_event(
    rx: &mut a3chat_app::notification_bus::NotificationReceiver,
) -> A3chatEvent {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("bus event timed out")
        .expect("bus closed")
}

// ─── Pinned ─────────────────────────────────────────────────────────

#[tokio::test]
async fn pin_change_publishes_event() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("dm:bus-pin-1");
    // Pre-seed the conversation meta so the storage layer accepts
    // the pin.
    use a3chat_core::conversation::{ConversationKind, ConversationMeta};
    let meta = ConversationMeta {
        conversation_id: cid.clone(),
        kind: ConversationKind::Dm,
        title: "t".into(),
        peer_user_id: None,
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
        .upsert_conversation(&owner, &meta)
        .await
        .unwrap();

    let mut rx = app.bus.subscribe_for(owner.clone());
    app.dispatch(
        "a3chat.chat.conversation.pin",
        &owner,
        serde_json::json!({ "conversation_id": cid.as_str() }),
    )
    .await
    .expect("pin");

    let evt = next_event(&mut rx).await;
    assert!(
        matches!(
            &evt,
            A3chatEvent::ConversationPinChanged {
                conversation_id,
                pinned: true,
                ..
            } if conversation_id.as_str() == cid.as_str()
        ),
        "expected ConversationPinChanged with pinned=true, got {evt:?}"
    );
}

// ─── Notification settings ──────────────────────────────────────────

#[tokio::test]
async fn notification_settings_change_publishes_event() {
    let (app, _dir, owner) = boot().await;
    let cid = ConversationId::from("dm:bus-notif-1");
    // The conversation_id is optional — leave it None to exercise
    // the global-DND path.
    let mut rx = app.bus.subscribe_for(owner.clone());
    app.dispatch(
        "a3chat.chat.notification.set_dnd",
        &owner,
        serde_json::json!({
            "enabled": true,
            "quiet_from": null,
            "quiet_until": null,
            "allow_calls": false,
            "allow_pinned": false,
        }),
    )
    .await
    .expect("set_dnd");

    let evt = next_event(&mut rx).await;
    // The service must publish a NotificationSettingsChanged with
    // global_dnd populated for the global-DND path.
    assert!(
        matches!(
            &evt,
            A3chatEvent::NotificationSettingsChanged { global_dnd: Some(_), .. }
        ),
        "expected NotificationSettingsChanged with Some(global_dnd), got {evt:?}"
    );
    // Per-conv event is a separate emission triggered by
    // set_conversation.
    app.dispatch(
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
    .expect("set_conversation");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(
            &evt,
            A3chatEvent::NotificationSettingsChanged {
                conversation_id: Some(c),
                ..
            } if c.as_str() == cid.as_str()
        ),
        "expected per-conversation NotificationSettingsChanged, got {evt:?}"
    );
}

// ─── Devices ────────────────────────────────────────────────────────

#[tokio::test]
async fn device_register_publishes_event() {
    let (app, _dir, owner) = boot().await;
    let mut rx = app.bus.subscribe_for(owner.clone());
    let v = app
        .dispatch(
            "a3chat.device.register",
            &owner,
            serde_json::json!({
                "name": "ci-laptop",
                "public_key_b64": "AAAA",
                "kind": "desktop",
            }),
        )
        .await
        .expect("register");
    let device_id = v["device_id"].as_str().unwrap().to_string();

    let evt = next_event(&mut rx).await;
    assert!(
        matches!(
            &evt,
            A3chatEvent::DeviceRegistered { device_id: d, .. } if d == &device_id
        ),
        "expected DeviceRegistered with id {device_id}, got {evt:?}"
    );
}

// ─── Reactions ──────────────────────────────────────────────────────

#[tokio::test]
async fn reaction_add_publishes_event() {
    let (app, _dir, owner) = boot().await;
    let mut rx = app.bus.subscribe_for(owner.clone());
    let cid = ConversationId::from("dm:bus-react-1");
    let mid = MessageId::from("m-bus-react-1");

    app.dispatch(
        "a3chat.chat.reaction.add",
        &owner,
        serde_json::json!({
            "message_id": mid.as_str(),
            "conversation_id": cid.as_str(),
            "reaction_type": "like",
        }),
    )
    .await
    .expect("add");

    let evt = next_event(&mut rx).await;
    assert!(
        matches!(
            &evt,
            A3chatEvent::ChatMessageReactionToggled {
                reaction_type,
                is_added: true,
                ..
            } if reaction_type == "like"
        ),
        "expected ChatMessageReactionToggled with is_added=true, got {evt:?}"
    );
}

// ─── Stream ─────────────────────────────────────────────────────────

#[tokio::test]
async fn stream_unsubscribe_does_not_publish_event() {
    // Sanity-check: subscribe + unsubscribe must not produce a bus
    // event of its own — events are owned by the services, not by
    // the stream subscribe book-keeping. We assert this by
    // subscribing to the bus and confirming nothing arrives within
    // a 200 ms quiet window after subscribe + unsubscribe.
    let (app, _dir, owner) = boot().await;
    let mut rx = app.bus.subscribe_for(owner.clone());

    // Subscribe
    app.dispatch(
        "a3chat.stream.subscribe",
        &owner,
        serde_json::json!({ "topics": ["chat"] }),
    )
    .await
    .expect("subscribe");
    // Unsubscribe — fetch the handle first.
    let list_v = app
        .dispatch("a3chat.stream.list", &owner, serde_json::json!({}))
        .await
        .expect("list");
    let handle_id = list_v["handles"][0]["handle_id"]
        .as_str()
        .expect("handle_id")
        .to_string();
    app.dispatch(
        "a3chat.stream.unsubscribe",
        &owner,
        serde_json::json!({ "handle_id": handle_id }),
    )
    .await
    .expect("unsubscribe");

    // Quiet window — drain anything pending immediately, then
    // assert the second poll is also empty.
    let pending_1 =
        tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        pending_1.is_err(),
        "no event should be emitted by stream subscribe/unsubscribe"
    );
    let pending_2 =
        tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(pending_2.is_err());
}

// ─── Contacts ──────────────────────────────────────────────────────

#[tokio::test]
async fn contact_full_lifecycle_publishes_events() {
    let (app, _dir, owner) = boot().await;
    let mut rx = app.bus.subscribe_for(owner.clone());
    let peer = UserId::from("a".repeat(64));

    // 1. add
    app.dispatch(
        "a3chat.contact.add",
        &owner,
        serde_json::json!({
            "user_id": peer.as_str(),
            "display_name": "Alice",
            "note": "n",
            "is_favorite": false,
            "is_blocked": false,
            "added_at": "2026-01-01T00:00:00Z",
        }),
    )
    .await
    .expect("add");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(&evt, A3chatEvent::ContactAdded { contact_id } if contact_id == peer.as_str()),
        "expected ContactAdded for {peer}, got {evt:?}"
    );

    // 2. toggle favorite (true)
    app.dispatch(
        "a3chat.contact.toggle_favorite",
        &owner,
        serde_json::json!({ "contact_id": peer.as_str() }),
    )
    .await
    .expect("fav");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(&evt, A3chatEvent::ContactFavoriteToggled { contact_id, is_favorite: true } if contact_id == peer.as_str()),
        "expected ContactFavoriteToggled is_favorite=true, got {evt:?}"
    );

    // 3. update
    app.dispatch(
        "a3chat.contact.update",
        &owner,
        serde_json::json!({
            "user_id": peer.as_str(),
            "display_name": "Alice2",
            "note": "n2",
            "is_favorite": true,
            "is_blocked": false,
            "added_at": "2026-01-01T00:00:00Z",
        }),
    )
    .await
    .expect("update");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(&evt, A3chatEvent::ContactUpdated { contact_id } if contact_id == peer.as_str()),
        "expected ContactUpdated, got {evt:?}"
    );

    // 4. block
    app.dispatch(
        "a3chat.contact.block",
        &owner,
        serde_json::json!({ "user_id": peer.as_str() }),
    )
    .await
    .expect("block");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(&evt, A3chatEvent::ContactBlocked { user_id } if user_id == &peer),
        "expected ContactBlocked, got {evt:?}"
    );

    // 5. unblock
    app.dispatch(
        "a3chat.contact.unblock",
        &owner,
        serde_json::json!({ "user_id": peer.as_str() }),
    )
    .await
    .expect("unblock");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(&evt, A3chatEvent::ContactUnblocked { user_id } if user_id == &peer),
        "expected ContactUnblocked, got {evt:?}"
    );

    // 6. remove
    app.dispatch(
        "a3chat.contact.remove",
        &owner,
        serde_json::json!({ "contact_id": peer.as_str() }),
    )
    .await
    .expect("remove");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(&evt, A3chatEvent::ContactRemoved { contact_id } if contact_id == peer.as_str()),
        "expected ContactRemoved, got {evt:?}"
    );
}

#[tokio::test]
async fn contact_friend_request_lifecycle_publishes_events() {
    let (app, _dir, owner) = boot().await;
    let mut rx = app.bus.subscribe_for(owner.clone());
    let peer = UserId::from("b".repeat(64));

    // add_request → ContactRequestReceived
    app.dispatch(
        "a3chat.contact.add_request",
        &owner,
        serde_json::json!({ "to_user_id": peer.as_str(), "message": "hi" }),
    )
    .await
    .expect("add_request");
    let evt = next_event(&mut rx).await;
    let request_id = match &evt {
        A3chatEvent::ContactRequestReceived { request_id } => request_id.clone(),
        other => panic!("expected ContactRequestReceived, got {other:?}"),
    };

    // accept_request → ContactRequestAccepted
    app.dispatch(
        "a3chat.contact.accept_request",
        &owner,
        serde_json::json!({
            "request": {
                "request_id": request_id,
                "from_user_id": peer.as_str(),
                "from_display_name": "Peer",
                "to_user_id": owner.as_str(),
                "message": "hi",
                "status": "pending",
                "created_at": "2026-01-01T00:00:00Z",
                "responded_at": null,
            }
        }),
    )
    .await
    .expect("accept");
    let evt = next_event(&mut rx).await;
    assert!(
        matches!(
            &evt,
            A3chatEvent::ContactRequestAccepted {
                request_id: rid,
                contact_id: cid,
            } if rid == &request_id && cid == peer.as_str()
        ),
        "expected ContactRequestAccepted with matching ids, got {evt:?}"
    );
}
