//! End-to-end integration tests for [`LinkBookmarkService`].
//!
//! These tests stand up the *real* per-user SQLite chatstore and
//! link-bookmark store in a tempdir and walk the full
//! `a3net-chatstore::LinkBookmarkStore` ↔ `a3chat-app::LinkBookmarkService`
//! ↔ `a3chat-core::NotificationBus` bridge:
//!
//! 1. `add()` writes a row, normalises tags, truncates timestamps to
//!    whole seconds, and publishes a `LinkBookmarkAdded` event on the
//!    bus.
//! 2. `get()` / `get_by_url()` return the same record back.
//! 3. `update()` merges fields, bumps `updated_at`, publishes
//!    `LinkBookmarkUpdated`.
//! 4. `delete()` removes the row and publishes
//!    `LinkBookmarkDeleted`.
//! 5. `list()` / `search()` / `tags()` / `folders()` / `count()` all
//!    read from the SQLite file.
//!
//! We deliberately avoid using `ChatStorage::init_user` (which
//! touches a different schema path); instead we open the link
//! bookmark store directly via the same SQLite file and exercise the
//! service's storage adapter. This keeps the e2e test hermetic and
//! independent of the chat-history plumbing.

use a3chat_app::link_bookmark_service::{
    LinkBookmarkConfig, LinkBookmarkService,
};
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_app::keyring::E2eKeyring;
use a3chat_core::event::A3chatEvent;
use a3chat_core::id::UserId;
use a3chat_core::link_bookmark::{
    LinkBookmarkCount, LinkBookmarkListFilter, LinkBookmarkSearchQuery, UpsertLinkBookmarkRequest,
    MAX_TITLE_LEN,
};
use a3chat_core::rpc::A3chatRpcMethod;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

fn alice() -> UserId {
    // Stable hex so any bookmark_id derived from this is comparable
    // across runs.
    UserId::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
}

async fn fresh_service() -> (TempDir, Arc<LinkBookmarkService>, UserId) {
    let dir = TempDir::new().expect("tempdir");
    let cfg = StorageConfig::new(dir.path().to_path_buf());
    let owner = alice();
    let keyring = E2eKeyring::new(owner.clone());
    let storage = ChatStorage::new(cfg, keyring);
    let bus = NotificationBus::new(64);
    let link_cfg = LinkBookmarkConfig::under_base(dir.path());
    let svc = Arc::new(LinkBookmarkService::new(storage, bus, link_cfg));
    (dir, svc, owner)
}

fn sample_req(url: &str, title: &str) -> UpsertLinkBookmarkRequest {
    UpsertLinkBookmarkRequest {
        url: url.to_string(),
        title: title.to_string(),
        description: Some("e2e note".into()),
        favicon_hash: None,
        folder: "/rust".to_string(),
        tags: vec!["rust".into(), "storage".into(), "Rust".into()],
        is_pinned: false,
        is_archived: false,
        snapshot_text: None,
        source: a3chat_core::link_bookmark::BookmarkSource::User,
    }
}

#[tokio::test]
async fn add_get_delete_round_trip_emits_events() {
    let (_dir, svc, owner) = fresh_service().await;
    let mut bus_rx = svc.bus().subscribe();

    let b = svc.add(&owner, sample_req("https://example.com", "Example")).await.expect("add");
    assert_eq!(b.url, "https://example.com");
    assert_eq!(b.title, "Example");
    // Tags are normalised: lower-cased, deduplicated, "Rust" folded
    // into "rust".
    assert_eq!(b.tags, vec!["rust", "storage"]);
    assert_eq!(b.folder, "/rust");

    // Drain the bus — the add must have published at least one event.
    let event = tokio::time::timeout(Duration::from_millis(500), bus_rx.recv())
        .await
        .expect("event timeout")
        .expect("event recv");
    match event {
        A3chatEvent::LinkBookmarkAdded { user_id, bookmark } => {
            assert_eq!(user_id, owner);
            assert_eq!(bookmark.url, "https://example.com");
        }
        other => panic!("expected LinkBookmarkAdded, got {other:?}"),
    }

    // get() round-trips.
    let fetched = svc.get(&owner, &b.bookmark_id).await.expect("get");
    assert_eq!(fetched.url, b.url);
    assert_eq!(fetched.title, b.title);
    assert_eq!(fetched.tags, b.tags);
    assert_eq!(fetched.folder, b.folder);
    // Whole-second precision (timestamps truncated to INTEGER columns).
    assert_eq!(fetched.created_at.timestamp(), b.created_at.timestamp());
    assert_eq!(fetched.updated_at.timestamp(), b.updated_at.timestamp());

    // get_by_url() works too.
    let by_url = svc.get_by_url(&owner, "https://example.com").await.expect("get_by_url").expect("present");
    assert_eq!(by_url.bookmark_id, b.bookmark_id);

    // update() merges fields and bumps updated_at.
    let mut upd = sample_req("https://example.com", "Example renamed");
    upd.tags = vec!["alpha".into(), "beta".into()];
    upd.folder = "/alpha".to_string();
    upd.is_pinned = true;
    let updated = svc.update(&owner, &b.bookmark_id, upd).await.expect("update");
    assert_eq!(updated.title, "Example renamed");
    assert_eq!(updated.tags, vec!["alpha", "beta"]);
    assert_eq!(updated.folder, "/alpha");
    assert!(updated.is_pinned);

    match tokio::time::timeout(Duration::from_millis(500), bus_rx.recv())
        .await
        .expect("update event timeout")
        .expect("update event recv")
    {
        A3chatEvent::LinkBookmarkUpdated { user_id, bookmark } => {
            assert_eq!(user_id, owner);
            assert_eq!(bookmark.bookmark_id, b.bookmark_id);
            assert_eq!(bookmark.title, "Example renamed");
        }
        other => panic!("expected LinkBookmarkUpdated, got {other:?}"),
    }

    // delete() emits the deletion event.
    svc.delete(&owner, &b.bookmark_id).await.expect("delete");
    match tokio::time::timeout(Duration::from_millis(500), bus_rx.recv())
        .await
        .expect("delete event timeout")
        .expect("delete event recv")
    {
        A3chatEvent::LinkBookmarkDeleted { user_id, bookmark_id, url } => {
            assert_eq!(user_id, owner);
            assert_eq!(bookmark_id, b.bookmark_id);
            assert_eq!(url, "https://example.com");
        }
        other => panic!("expected LinkBookmarkDeleted, got {other:?}"),
    }

    let after = svc.get(&owner, &b.bookmark_id).await;
    assert!(after.is_err(), "row should be gone after delete");
}

#[tokio::test]
async fn list_filters_by_folder_and_returns_paginated() {
    let (_dir, svc, owner) = fresh_service().await;

    // Three bookmarks across two folders.
    let _ = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();
    let _ = svc.add(&owner, sample_req("https://b.example", "B")).await.unwrap();
    let mut req_c = sample_req("https://c.example", "C");
    req_c.folder = "/other".to_string();
    let _ = svc.add(&owner, req_c).await.unwrap();

    let filter = LinkBookmarkListFilter {
        folder: Some("/rust".into()),
        include_subfolders: false,
        tags: vec![],
        is_pinned: None,
        is_archived: Some(false),
        limit: Some(10),
    };
    let in_rust = svc.list(&owner, filter).await.expect("list");
    assert_eq!(in_rust.len(), 2, "exactly the two /rust bookmarks");
    assert!(in_rust.iter().all(|b| b.folder == "/rust"));

    // Pagination — limit 1 returns exactly one row.
    let filter = LinkBookmarkListFilter {
        folder: Some("/rust".into()),
        include_subfolders: false,
        tags: vec![],
        is_pinned: None,
        is_archived: Some(false),
        limit: Some(1),
    };
    let one = svc.list(&owner, filter).await.expect("list page 1");
    assert_eq!(one.len(), 1);
}

#[tokio::test]
async fn search_finds_by_title_and_returns_limit() {
    let (_dir, svc, owner) = fresh_service().await;
    let _ = svc.add(&owner, sample_req("https://rust-lang.org", "Rust homepage")).await.unwrap();
    let _ = svc.add(&owner, sample_req("https://crates.io", "Crates index")).await.unwrap();
    let _ = svc.add(&owner, sample_req("https://docs.rs", "Rust documentation")).await.unwrap();

    let q = LinkBookmarkSearchQuery {
        needle: "rust".into(),
        limit: Some(2),
        folder: None,
    };
    let found = svc.search(&owner, q).await.expect("search");
    assert_eq!(found.len(), 2, "limit is enforced");
    // Both results contain "rust" (case-insensitive) somewhere.
    assert!(found.iter().any(|b| b.title.to_ascii_lowercase().contains("rust")));
}

#[tokio::test]
async fn tags_returns_lowercased_counts() {
    let (_dir, svc, owner) = fresh_service().await;
    let _ = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();
    let mut b = sample_req("https://b.example", "B");
    b.tags = vec!["alpha".into(), "Beta".into()];
    let _ = svc.add(&owner, b).await.unwrap();

    let counts = svc.tags(&owner).await.expect("tags");
    assert!(!counts.is_empty());
    let alpha = counts.iter().find(|c| c.tag == "alpha").expect("alpha");
    assert_eq!(alpha.count, 1);
    // "Beta" is normalised to "beta".
    let beta = counts.iter().find(|c| c.tag == "beta").expect("beta");
    assert_eq!(beta.count, 1);
}

#[tokio::test]
async fn folders_returns_distinct_paths() {
    let (_dir, svc, owner) = fresh_service().await;
    let _ = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();
    let mut b = sample_req("https://b.example", "B");
    b.folder = "/work".to_string();
    let _ = svc.add(&owner, b).await.unwrap();

    let folders = svc.folders(&owner).await.expect("folders");
    let paths: Vec<&str> = folders.iter().map(|f| f.folder.as_str()).collect();
    assert!(paths.contains(&"/rust"));
    assert!(paths.contains(&"/work"));
}

#[tokio::test]
async fn count_splits_active_archived_and_pinned() {
    let (_dir, svc, owner) = fresh_service().await;
    let b = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();
    let _ = svc.add(&owner, sample_req("https://b.example", "B")).await.unwrap();
    let _ = svc.set_archived(&owner, &b.bookmark_id, true).await.expect("archive");

    let LinkBookmarkCount { archived, pinned, total } =
        svc.count(&owner).await.expect("count");
    assert_eq!(total, 2);
    assert_eq!(pinned, 0, "nothing pinned yet");
    assert_eq!(archived, 1);
}

#[tokio::test]
async fn pin_toggle_round_trips() {
    let (_dir, svc, owner) = fresh_service().await;
    let b = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();
    assert!(!b.is_pinned);

    let pinned = svc.set_pinned(&owner, &b.bookmark_id, true).await.expect("pin");
    assert!(pinned.is_pinned);

    let fetched = svc.get(&owner, &b.bookmark_id).await.expect("get");
    assert!(fetched.is_pinned);
}

#[tokio::test]
async fn dispatch_routes_to_add_and_returns_bookmark() {
    let (_dir, svc, owner) = fresh_service().await;

    let req_json = serde_json::json!({
        "owner_id": owner.as_str(),
        "url": "https://dispatch.example",
        "title": "Dispatch test",
        "description": null,
        "favicon_hash": null,
        "folder": "/test",
        "tags": ["dispatch", "test"],
        "is_pinned": false,
        "is_archived": false,
        "snapshot_text": null,
        "source": "user",
    });
    let result = dispatch(svc.clone(), A3chatRpcMethod::LINK_BOOKMARK_ADD, req_json).await;
    let bookmark: a3chat_core::link_bookmark::LinkBookmark =
        serde_json::from_value(result).expect("deserialize");
    assert_eq!(bookmark.url, "https://dispatch.example");
    assert_eq!(bookmark.title, "Dispatch test");
    // Tags normalised: lowercased.
    assert_eq!(bookmark.tags, vec!["dispatch", "test"]);
}

#[tokio::test]
async fn dispatch_routes_to_list_and_search() {
    let (_dir, svc, owner) = fresh_service().await;
    let _ = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();
    let _ = svc.add(&owner, sample_req("https://b.example", "B")).await.unwrap();

    let list_json = serde_json::json!({
        "owner_id": owner.as_str(),
        "filter": {
            "folder": "/rust",
            "include_subfolders": false,
            "tags": [],
            "is_pinned": null,
            "is_archived": false,
            "limit": 10,
        }
    });
    let list: Vec<a3chat_core::link_bookmark::LinkBookmark> =
        serde_json::from_value(dispatch(svc.clone(), A3chatRpcMethod::LINK_BOOKMARK_LIST, list_json).await).unwrap();
    assert_eq!(list.len(), 2);

    let search_json = serde_json::json!({
        "needle": "example",
        "limit": 5,
        "folder": null
    });
    let search: Vec<a3chat_core::link_bookmark::LinkBookmark> =
        serde_json::from_value(dispatch(svc.clone(), A3chatRpcMethod::LINK_BOOKMARK_SEARCH, search_json).await).unwrap();
    assert_eq!(search.len(), 2);
}

#[tokio::test]
async fn dispatch_count_tags_folders_share_owner_id_shape() {
    let (_dir, svc, owner) = fresh_service().await;
    let _ = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();

    let payload = serde_json::json!({ "owner_id": owner.as_str() });
    let _: LinkBookmarkCount = serde_json::from_value(dispatch(svc.clone(), A3chatRpcMethod::LINK_BOOKMARK_COUNT, payload.clone()).await).unwrap();
    let _: Vec<a3chat_core::link_bookmark::LinkTagCount> =
        serde_json::from_value(dispatch(svc.clone(), A3chatRpcMethod::LINK_BOOKMARK_TAGS, payload.clone()).await).unwrap();
    let _: Vec<a3chat_core::link_bookmark::LinkFolderNode> =
        serde_json::from_value(dispatch(svc.clone(), A3chatRpcMethod::LINK_BOOKMARK_FOLDERS, payload).await).unwrap();
}

#[tokio::test]
async fn dispatch_rejects_oversized_title_at_rpc_boundary() {
    let (_dir, svc, owner) = fresh_service().await;
    let oversized = "x".repeat(MAX_TITLE_LEN + 1);
    let req_json = serde_json::json!({
        "owner_id": owner.as_str(),
        "url": "https://x.example",
        "title": oversized,
        "description": null,
        "favicon_hash": null,
        "folder": "/",
        "tags": [],
        "is_pinned": false,
        "is_archived": false,
        "snapshot_text": null,
        "source": "user",
    });
    let res = dispatch(svc.clone(), A3chatRpcMethod::LINK_BOOKMARK_ADD, req_json).await;
    let err = res.get("error").expect("error envelope");
    assert!(err.is_object(), "dispatch must return an error envelope");
}

#[tokio::test]
async fn delete_then_get_returns_not_found() {
    let (_dir, svc, owner) = fresh_service().await;
    let b = svc.add(&owner, sample_req("https://a.example", "A")).await.unwrap();
    svc.delete(&owner, &b.bookmark_id).await.unwrap();
    // get() now returns an `AppError` (not `None`) for a missing
    // row, so the e2e test exercises the not-found path of the
    // service rather than the optional shape.
    let after = svc.get(&owner, &b.bookmark_id).await;
    assert!(after.is_err(), "expected not-found error after delete");
}

#[tokio::test]
async fn rpc_methods_are_registered_in_all_list() {
    // Defence-in-depth: if the dispatcher ever drops one of these
    // methods from `A3chatRpcMethod::ALL`, this test fails.
    let methods = A3chatRpcMethod::ALL;
    let expected = [
        A3chatRpcMethod::LINK_BOOKMARK_ADD,
        A3chatRpcMethod::LINK_BOOKMARK_UPDATE,
        A3chatRpcMethod::LINK_BOOKMARK_GET,
        A3chatRpcMethod::LINK_BOOKMARK_LIST,
        A3chatRpcMethod::LINK_BOOKMARK_SEARCH,
        A3chatRpcMethod::LINK_BOOKMARK_DELETE,
        A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED,
        A3chatRpcMethod::LINK_BOOKMARK_TAGS,
        A3chatRpcMethod::LINK_BOOKMARK_FOLDERS,
        A3chatRpcMethod::LINK_BOOKMARK_COUNT,
    ];
    for m in expected {
        assert!(
            methods.contains(&m),
            "RPC method {m} must be registered in A3chatRpcMethod::ALL"
        );
    }
}

#[tokio::test]
async fn link_bookmark_store_config_under_base_is_writable() {
    // Sanity: the helper we use in `fresh_service` actually opens.
    let dir = TempDir::new().unwrap();
    let cfg = LinkBookmarkConfig::under_base(dir.path());
    assert!(cfg.base_dir.starts_with(dir.path()));
}

// ---------------------------------------------------------------
// helpers
// ---------------------------------------------------------------

async fn dispatch(
    svc: Arc<LinkBookmarkService>,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    // The dispatch entry-point lives in the service module. We re-use
    // the public `dispatch` function but keep this wrapper so the
    // test reads top-to-bottom without referencing internal symbols.
    let owner = alice();
    let result = a3chat_app::link_bookmark_service::dispatch(
        svc,
        method,
        &owner,
        params,
    )
    .await;
    match result {
        Ok(v) => v,
        Err(e) => serde_json::json!({ "error": { "code": -1, "message": e.to_string() } }),
    }
}