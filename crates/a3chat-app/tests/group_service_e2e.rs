//! End-to-end tests for `GroupService`.
//!
//! These tests stand up a fresh `ChatStorage` *and* a fresh
//! `ImManager` for every test so they don't share on-disk state
//! (DO-178C §6.3 — test isolation).

use std::sync::Arc;

use a3chat_app::group_service::{
    CreateGroupRequest, GroupService,
};
use a3chat_app::notification_bus::NotificationBus;

use a3chat_core::group::MemberRole;
use a3chat_core::id::UserId;
use a3net_chatstore::ImManager;


async fn boot() -> (Arc<GroupService>, tempfile::TempDir, UserStore) {
    let dir = tempfile::tempdir().unwrap();
    let hub_path = dir.path().join("hub.sqlite");
    let hub = ImManager::new(hub_path).unwrap();

    // Create hub users first to get their canonical IDs.
    let first_user = hub.create_user("alice", "Alice").await.unwrap();
    let alice_uid = UserId::from(first_user.id.clone());
    let bob_user = hub.create_user("bob", "Bob").await.unwrap();
    let bob_uid = UserId::from(bob_user.id.clone());
    let carol_user = hub.create_user("carol", "Carol").await.unwrap();
    let carol_uid = UserId::from(carol_user.id.clone());

    let mut store = UserStore::default();
    store.ids.insert("alice".to_string(), alice_uid.clone());
    store.ids.insert("bob".to_string(), bob_uid.clone());
    store.ids.insert("carol".to_string(), carol_uid.clone());

    // GroupService only needs hub for these tests; storage is optional.
    let svc = Arc::new(GroupService::new(NotificationBus::default()))
        .with_hub(Arc::new(hub));
    (svc, dir, store)
}

#[derive(Default)]
struct UserStore {
    ids: std::collections::HashMap<String, a3chat_core::id::UserId>,
}

impl UserStore {
    fn user(&self, key: &str) -> a3chat_core::id::UserId {
        self.ids.get(key).cloned().unwrap_or_else(|| a3chat_core::id::UserId::from(key))
    }
    fn alice(&self) -> a3chat_core::id::UserId {
        self.user("alice")
    }
    fn bob(&self) -> a3chat_core::id::UserId {
        self.user("bob")
    }
    fn carol(&self) -> a3chat_core::id::UserId {
        self.user("carol")
    }
}

#[tokio::test]
async fn create_persists_to_hub_and_mirror() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "engineering".into(),
                description: "Core team".into(),
                avatar_url: None,
                is_private: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(op.group.owner_id, ids.alice());
    assert_eq!(op.group.member_count, 1);
    assert!(op.group.is_private);
    let list = svc.list(&ids.alice()).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].conversation_id, op.group.conversation_id);
}

#[tokio::test]
async fn add_member_emits_event_and_persists_in_hub() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    let mut rx = svc.bus().subscribe();
    let m = svc.add_member(&ids.alice(), &cid, &ids.bob()).await.unwrap();
    assert_eq!(m.role, MemberRole::Member);
    let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
        .await
        .expect("event")
        .expect("event some");
    assert!(matches!(
        evt,
        a3chat_core::event::A3chatEvent::GroupMemberJoined { .. }
    ));
    let roster = svc.list_members(&cid).await.unwrap();
    assert_eq!(roster.len(), 2);
}

#[tokio::test]
async fn set_role_then_get_returns_admin() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    svc.add_member(&ids.alice(), &cid, &ids.bob()).await.unwrap();
    let updated = svc.set_role(&ids.alice(), &cid, &ids.bob(), MemberRole::Admin).await.unwrap();
    assert_eq!(updated.role, MemberRole::Admin);
    let got = svc.get_member(&cid, &ids.bob()).await.unwrap();
    assert_eq!(got.role, MemberRole::Admin);
}

#[tokio::test]
async fn set_role_rejects_non_admin_actor() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    svc.add_member(&ids.alice(), &cid, &ids.bob()).await.unwrap();
    let err = svc
        .set_role(&ids.bob(), &cid, &ids.alice(), MemberRole::Admin)
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));
}

#[tokio::test]
async fn remove_member_rejects_when_target_is_owner() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    let err = svc
        .remove_member(&ids.bob(), &cid, &ids.alice())
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));
}

#[tokio::test]
async fn transfer_ownership_demotes_old_owner_to_admin() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    svc.add_member(&ids.alice(), &cid, &ids.bob()).await.unwrap();
    svc.transfer_ownership(&ids.alice(), &cid, &ids.bob())
        .await
        .unwrap();
    let alice_row = svc.get_member(&cid, &ids.alice()).await.unwrap();
    let bob_row = svc.get_member(&cid, &ids.bob()).await.unwrap();
    assert_eq!(alice_row.role, MemberRole::Admin);
    assert_eq!(bob_row.role, MemberRole::Owner);
}

#[tokio::test]
async fn set_announcement_rejects_non_admin() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    svc.add_member(&ids.alice(), &cid, &ids.bob()).await.unwrap();
    let err = svc
        .set_announcement(&ids.bob(), &cid, "Hi".into())
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));
}

#[tokio::test]
async fn get_member_returns_specific_user() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    svc.add_member(&ids.alice(), &cid, &ids.bob()).await.unwrap();
    let m = svc.get_member(&cid, &ids.bob()).await.unwrap();
    assert_eq!(m.user_id, ids.bob());
}

#[tokio::test]
async fn get_member_rejects_non_member() {
    let (svc, _dir, ids) = boot().await;
    let op = svc
        .create(
            &ids.alice(),
            CreateGroupRequest {
                name: "g".into(),
                description: "".into(),
                avatar_url: None,
                is_private: false,
            },
        )
        .await
        .unwrap();
    let cid = op.group.conversation_id;
    let err = svc.get_member(&cid, &ids.carol()).await.unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));
}

#[tokio::test]
async fn rpc_group_create_returns_operation() {
    use a3chat_app::group_service as gs;
    let (svc, _dir, ids) = boot().await;
    let r = gs::dispatch(
        svc,
        a3chat_core::rpc::A3chatRpcMethod::GROUP_CREATE,
        &ids.alice(),
        serde_json::json!({
            "name": "team",
            "description": "",
            "avatarUrl": null,
            "isPrivate": false
        }),
    )
    .await
    .unwrap();
    let resp: a3chat_app::group_service::CreateGroupResponse = serde_json::from_value(r).unwrap();
    assert_eq!(resp.group.name, "team");
}

#[tokio::test]
async fn rpc_unknown_method_returns_internal_error() {
    use a3chat_app::group_service as gs;
    let (svc, _dir, ids) = boot().await;
    let err = gs::dispatch(
        svc,
        "a3chat.bogus",
        &ids.alice(),
        serde_json::json!({}),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        a3chat_core::error::A3chatError::Internal(_)
    ));
}
