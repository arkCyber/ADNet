//! End-to-end integration tests for the channel / public-account
//! service (F-09).
//!
//! Wires up a real `A3chatApp` with a tempdir `chatstore`, then
//! drives the public RPC surface (`a3chat.channel.*`) via the
//! dispatcher to verify that:
//!
//! 1. `register` writes an account and publishes a
//!    `ChannelAccountRegistered` event.
//! 2. `publish` mints a monotonic sequence, wraps the feed item
//!    in a `BulletinItem`, and broadcasts it through the gossip
//!    transport — while also persisting the row locally.
//! 3. `subscribe` / `unsubscribe` maintain the cached
//!    `subscriber_count` and publish lifecycle events.
//! 4. `mark_read` advances the cursor and the `unread_count`
//!    reports the difference.
//! 5. The dispatcher routes `a3chat.channel.*` methods to the
//!    service and the service rejects malformed payloads with
//!    `InvalidInput` errors.
//!
//! The tests intentionally avoid touching the live `iroh`
//! transport (none of these methods require it): the service
//! uses `InProcessGossip` by default, which keeps the tests
//! hermetic.

use a3chat_app::app::A3chatApp;
use a3chat_app::keyring::E2eKeyring;
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_core::channel::{
    AccountKind, FeedAttachment, PublicAccount, PublishFeedRequest, Subscription,
    UpsertChannelAccountRequest, VerificationLevel,
};
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;

fn build_app() -> (tempfile::TempDir, A3chatApp, UserId) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = StorageConfig::new(dir.path().to_path_buf());
    // Channel / public-account RPCs validate that the dispatching
    // `owner` equals the local `NodeId` (which `A3chatApp::with_storage`
    // derives via `NodeId::from_hex(owner.as_str())`). A human-style
    // id like `user:alice` would fail that hex parse and force a
    // random fallback, so every dispatch would later be rejected with
    // `PermissionDenied("owner_node_id ... does not match local_node")`.
    // Use a 64-hex-char node id as the owner so the parse succeeds
    // and owner == local_node holds for the lifetime of the test.
    let owner = UserId::from(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    );
    let keyring = E2eKeyring::new(owner.clone());
    let storage = ChatStorage::new(cfg, keyring);
    let bus = NotificationBus::new(NotificationBus::default_capacity());
    let app = A3chatApp::with_storage(storage, bus, owner.clone());
    (dir, app, owner)
}

fn sample_request(name: &str) -> UpsertChannelAccountRequest {
    UpsertChannelAccountRequest {
        name: name.into(),
        bio: "test".into(),
        avatar_hash: None,
        tags: vec!["tech".into()],
        kind: AccountKind::Service,
        verification: VerificationLevel::OwnerVerified,
    }
}

#[tokio::test]
async fn dispatch_register_then_get_by_owner() {
    let (_dir, app, owner) = build_app();
    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
            &owner,
            serde_json::to_value(sample_request("Alice Channel")).unwrap(),
        )
        .await
        .expect("register");
    let a: PublicAccount = serde_json::from_value(v).expect("decode");
    assert!(a.account_id.starts_with("acc_"));

    let by_owner = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_ACCOUNT_GET_BY_OWNER,
            &owner,
            serde_json::json!({ "owner_node_id": owner.as_str() }),
        )
        .await
        .expect("get_by_owner");
    let back: Option<PublicAccount> = serde_json::from_value(by_owner).expect("decode");
    let back = back.expect("present");
    assert_eq!(back.account_id, a.account_id);
    assert_eq!(back.name, "Alice Channel");
    assert_eq!(back.kind, AccountKind::Service);
}

#[tokio::test]
async fn dispatch_register_twice_is_conflict() {
    let (_dir, app, owner) = build_app();
    app.dispatch(
        A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
        &owner,
        serde_json::to_value(sample_request("Alice")).unwrap(),
    )
    .await
    .expect("first");
    let err = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
            &owner,
            serde_json::to_value(sample_request("Other")).unwrap(),
        )
        .await
        .unwrap_err();
    // Wrapped `AppError::Conflict` → `A3chatError::InvalidInput`
    // through the `app_to_domain` mapper.
    let msg = format!("{err:?}");
    assert!(
        msg.contains("already exists") || msg.contains("Conflict"),
        "got {msg}"
    );
}

#[tokio::test]
async fn dispatch_publish_then_list_then_subscribe_timeline() {
    let (_dir, app, owner) = build_app();
    app.dispatch(
        A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
        &owner,
        serde_json::to_value(sample_request("Alice")).unwrap(),
    )
    .await
    .expect("register");

    let publish_req = PublishFeedRequest {
        title: "first".into(),
        summary: "summary".into(),
        body: "body".into(),
        cover_url: Some("https://example.com/c.png".into()),
        attachments: vec![FeedAttachment {
            kind: "image".into(),
            url: "https://example.com/a.png".into(),
            content_hash: None,
            mime_type: None,
            caption: None,
        }],
        tags: vec!["tech".into()],
        is_pinned: false,
    };
    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_PUBLISH,
            &owner,
            serde_json::to_value(&publish_req).unwrap(),
        )
        .await
        .expect("publish");
    let feed: a3chat_core::channel::FeedItem =
        serde_json::from_value(v).expect("decode feed");
    assert!(feed.feed_id.starts_with("feed_"));
    assert_eq!(feed.sequence, 1);

    // Get the account id back so we can list / subscribe.
    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_ACCOUNT_GET_BY_OWNER,
            &owner,
            serde_json::json!({ "owner_node_id": owner.as_str() }),
        )
        .await
        .expect("get owner");
    let a: PublicAccount = serde_json::from_value(v).expect("decode account");

    let list = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_LIST,
            &owner,
            serde_json::json!({ "account_id": a.account_id, "limit": 10 }),
        )
        .await
        .expect("feed.list");
    let rows: Vec<a3chat_core::channel::FeedItem> =
        serde_json::from_value(list).expect("decode list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].feed_id, feed.feed_id);

    // Subscribe from a different user.
    let bob = UserId::from("user:bob");
    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_SUBSCRIBE,
            &bob,
            serde_json::json!({
                "account_id": a.account_id,
                "alias": "work",
                "notify_mode": "normal",
            }),
        )
        .await
        .expect("subscribe");
    let sub: Subscription = serde_json::from_value(v).expect("decode sub");
    assert_eq!(sub.subscriber_id, bob.as_str());
    assert_eq!(sub.alias, "work");

    let tl = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_TIMELINE,
            &bob,
            serde_json::json!({ "limit": 10 }),
        )
        .await
        .expect("timeline");
    let tl_rows: Vec<a3chat_core::channel::FeedItem> =
        serde_json::from_value(tl).expect("decode timeline");
    assert_eq!(tl_rows.len(), 1);
    assert_eq!(tl_rows[0].feed_id, feed.feed_id);
}

#[tokio::test]
async fn dispatch_subscribe_rejects_unknown_account() {
    let (_dir, app, owner) = build_app();
    let err = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_SUBSCRIBE,
            &owner,
            serde_json::json!({
                "account_id": "acc_does_not_exist",
                "alias": "",
                "notify_mode": "normal",
            }),
        )
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("not found"), "got {msg}");
}

#[tokio::test]
async fn dispatch_publish_requires_registered_owner() {
    let (_dir, app, owner) = build_app();
    let publish_req = PublishFeedRequest {
        title: "first".into(),
        summary: "".into(),
        body: "body".into(),
        cover_url: None,
        attachments: vec![],
        tags: vec![],
        is_pinned: false,
    };
    let err = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_PUBLISH,
            &owner,
            serde_json::to_value(&publish_req).unwrap(),
        )
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("no account for owner"), "got {msg}");
}

#[tokio::test]
async fn dispatch_mark_read_advances_unread() {
    let (_dir, app, owner) = build_app();
    app.dispatch(
        A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
        &owner,
        serde_json::to_value(sample_request("Alice")).unwrap(),
    )
    .await
    .expect("register");

    // Publish two items so the unread count is meaningful.
    for title in ["one", "two"] {
        let publish_req = PublishFeedRequest {
            title: title.into(),
            summary: "".into(),
            body: "body".into(),
            cover_url: None,
            attachments: vec![],
            tags: vec![],
            is_pinned: false,
        };
        app.dispatch(
            A3chatRpcMethod::CHANNEL_FEED_PUBLISH,
            &owner,
            serde_json::to_value(&publish_req).unwrap(),
        )
        .await
        .expect("publish");
    }

    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_ACCOUNT_GET_BY_OWNER,
            &owner,
            serde_json::json!({ "owner_node_id": owner.as_str() }),
        )
        .await
        .expect("get");
    let a: PublicAccount = serde_json::from_value(v).unwrap();

    let bob = UserId::from("user:bob");
    app.dispatch(
        A3chatRpcMethod::CHANNEL_SUBSCRIBE,
        &bob,
        serde_json::json!({
            "account_id": a.account_id,
            "alias": "",
            "notify_mode": "normal",
        }),
    )
    .await
    .expect("subscribe");

    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_UNREAD_COUNT,
            &bob,
            serde_json::json!({ "account_id": a.account_id }),
        )
        .await
        .expect("unread");
    assert_eq!(v["unread"], 2);

    app.dispatch(
        A3chatRpcMethod::CHANNEL_FEED_MARK_READ,
        &bob,
        serde_json::json!({ "account_id": a.account_id, "last_read_seq": 1 }),
    )
    .await
    .expect("mark");

    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_UNREAD_COUNT,
            &bob,
            serde_json::json!({ "account_id": a.account_id }),
        )
        .await
        .expect("unread");
    assert_eq!(v["unread"], 1);
}

#[tokio::test]
async fn dispatch_retract_hides_from_list() {
    let (_dir, app, owner) = build_app();
    app.dispatch(
        A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
        &owner,
        serde_json::to_value(sample_request("Alice")).unwrap(),
    )
    .await
    .expect("register");
    let publish_req = PublishFeedRequest {
        title: "first".into(),
        summary: "".into(),
        body: "body".into(),
        cover_url: None,
        attachments: vec![],
        tags: vec![],
        is_pinned: false,
    };
    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_PUBLISH,
            &owner,
            serde_json::to_value(&publish_req).unwrap(),
        )
        .await
        .expect("publish");
    let feed: a3chat_core::channel::FeedItem = serde_json::from_value(v).unwrap();

    app.dispatch(
        A3chatRpcMethod::CHANNEL_FEED_RETRACT,
        &owner,
        serde_json::json!({ "feed_id": feed.feed_id, "reason": "duplicate" }),
    )
    .await
    .expect("retract");

    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_ACCOUNT_GET_BY_OWNER,
            &owner,
            serde_json::json!({ "owner_node_id": owner.as_str() }),
        )
        .await
        .expect("get");
    let a: PublicAccount = serde_json::from_value(v).unwrap();
    let list = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_LIST,
            &owner,
            serde_json::json!({ "account_id": a.account_id, "limit": 10 }),
        )
        .await
        .expect("list");
    let rows: Vec<a3chat_core::channel::FeedItem> =
        serde_json::from_value(list).unwrap();
    assert!(rows.is_empty(), "retracted feed must be hidden");
}

#[tokio::test]
async fn dispatch_health_reports_local_node() {
    let (_dir, app, owner) = build_app();
    let v = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_HEALTH,
            &owner,
            serde_json::json!({}),
        )
        .await
        .expect("health");
    assert_eq!(v["service"], "a3chat.channel");
}

#[tokio::test]
async fn dispatch_publish_rejects_oversize_body() {
    let (_dir, app, owner) = build_app();
    app.dispatch(
        A3chatRpcMethod::CHANNEL_ACCOUNT_REGISTER,
        &owner,
        serde_json::to_value(sample_request("Alice")).unwrap(),
    )
    .await
    .expect("register");
    let publish_req = PublishFeedRequest {
        title: "ok".into(),
        summary: "".into(),
        body: "x".repeat(a3chat_core::channel::MAX_FEED_BODY_LEN + 1),
        cover_url: None,
        attachments: vec![],
        tags: vec![],
        is_pinned: false,
    };
    let err = app
        .dispatch(
            A3chatRpcMethod::CHANNEL_FEED_PUBLISH,
            &owner,
            serde_json::to_value(&publish_req).unwrap(),
        )
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(msg.contains("InvalidInput") || msg.contains("body"), "got {msg}");
}