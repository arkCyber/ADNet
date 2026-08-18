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

#[tokio::test]
async fn rpc_temp_admin_grant_via_dispatch() {
    use a3chat_app::group_service as gs;
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

    // Alice grants temp admin to Bob via RPC
    let resp = gs::dispatch(
        svc.clone(),
        a3chat_core::rpc::A3chatRpcMethod::GROUP_TEMP_ADMIN_GRANT,
        &ids.alice(),
        serde_json::json!({
            "conversation_id": cid,
            "user_id": ids.bob(),
            "duration_secs": 3600
        }),
    )
    .await
    .unwrap();
    assert!(resp.get("ok").and_then(|v| v.as_bool()) == Some(true));
}

#[tokio::test]
async fn rpc_temp_admin_revoke_via_dispatch() {
    use a3chat_app::group_service as gs;
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

    // Grant then revoke
    svc.grant_temp_admin(&ids.alice(), &cid, &ids.bob(), 3600)
        .await
        .unwrap();

    let resp = gs::dispatch(
        svc,
        a3chat_core::rpc::A3chatRpcMethod::GROUP_TEMP_ADMIN_REVOKE,
        &ids.alice(),
        serde_json::json!({
            "conversation_id": cid,
            "user_id": ids.bob()
        }),
    )
    .await
    .unwrap();
    assert!(resp.get("ok").and_then(|v| v.as_bool()) == Some(true));
}

#[tokio::test]
async fn rpc_temp_admin_rejects_invalid_duration() {
    use a3chat_app::group_service as gs;
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

    // Try with invalid duration (0)
    let err = gs::dispatch(
        svc,
        a3chat_core::rpc::A3chatRpcMethod::GROUP_TEMP_ADMIN_GRANT,
        &ids.alice(),
        serde_json::json!({
            "conversation_id": cid,
            "user_id": ids.bob(),
            "duration_secs": 0
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        a3chat_core::error::A3chatError::InvalidInput(_)
    ));
}

// ── Offline / Presence Tracking Tests ────────────────────────────────────────

#[tokio::test]
async fn touch_member_updates_last_seen_and_is_online() {
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

    // Initially offline
    let bob_before = svc.get_member(&cid, &ids.bob()).await.unwrap();
    assert!(!bob_before.is_online);
    assert!(bob_before.last_seen.is_none());

    // Touch member presence
    svc.touch_member(&cid, &ids.bob(), true).await.unwrap();

    // Now online with last_seen updated
    let bob_after = svc.get_member(&cid, &ids.bob()).await.unwrap();
    assert!(bob_after.is_online);
    assert!(bob_after.last_seen.is_some());
}

#[tokio::test]
async fn touch_member_emits_presence_changed_event() {
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

    let mut rx = svc.bus().subscribe();
    svc.touch_member(&cid, &ids.bob(), true).await.unwrap();

    let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
        .await
        .expect("event should arrive")
        .expect("event should be present");
    assert!(matches!(
        evt,
        a3chat_core::event::A3chatEvent::GroupMemberPresenceChanged { .. }
    ));
}

#[tokio::test]
async fn list_members_returns_presence_info() {
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

    // Touch bob's presence
    svc.touch_member(&cid, &ids.bob(), true).await.unwrap();

    let members = svc.list_members(&cid).await.unwrap();
    let bob = members.iter().find(|m| m.user_id == ids.bob()).unwrap();
    assert!(bob.is_online);
    assert!(bob.last_seen.is_some());
}

// ── Temporary Admin Tests ────────────────────────────────────────────────────

#[tokio::test]
async fn grant_temp_admin_allows_member_to_perform_admin_actions() {
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

    // Subscribe BEFORE granting temp admin so we can receive the event
    let mut rx = svc.bus().subscribe();

    // Initially bob can't set announcement (not admin)
    let err = svc
        .set_announcement(&ids.bob(), &cid, "Hello".into())
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));

    // Alice grants temporary admin to bob for 1 hour
    svc.grant_temp_admin(&ids.alice(), &cid, &ids.bob(), 3600)
        .await
        .unwrap();

    // Verify temp admin event was emitted
    let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
        .await
        .expect("event should arrive")
        .expect("event should be present");
    assert!(matches!(
        evt,
        a3chat_core::event::A3chatEvent::GroupTempAdminGranted { .. }
    ));

    // Now bob CAN perform admin actions (set announcement)
    svc.set_announcement(&ids.bob(), &cid, "Hello from temp admin Bob".into())
        .await
        .unwrap();
}

#[tokio::test]
async fn revoke_temp_admin_removes_temporary_privileges() {
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

    // Grant temp admin
    svc.grant_temp_admin(&ids.alice(), &cid, &ids.bob(), 3600)
        .await
        .unwrap();

    // Bob CAN set announcement while temp admin
    svc.set_announcement(&ids.bob(), &cid, "Hi from temp admin".into())
        .await
        .unwrap();

    // Revoke temp admin
    svc.revoke_temp_admin(&ids.alice(), &cid, &ids.bob())
        .await
        .unwrap();

    // Bob can no longer perform admin action
    let err = svc
        .set_announcement(&ids.bob(), &cid, "Try again".into())
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));
}

#[tokio::test]
async fn non_admin_cannot_grant_temp_admin() {
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
    svc.add_member(&ids.alice(), &cid, &ids.carol()).await.unwrap();

    // Bob tries to grant temp admin to Carol (not admin, should fail)
    let err = svc
        .grant_temp_admin(&ids.bob(), &cid, &ids.carol(), 3600)
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));
}

#[tokio::test]
async fn admin_can_grant_temp_admin() {
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
    svc.add_member(&ids.alice(), &cid, &ids.carol()).await.unwrap();

    // Alice makes Bob an admin
    svc.set_role(&ids.alice(), &cid, &ids.bob(), MemberRole::Admin)
        .await
        .unwrap();

    // Bob can grant temp admin (he's an admin)
    svc.grant_temp_admin(&ids.bob(), &cid, &ids.carol(), 3600)
        .await
        .unwrap();
}

// ── Owner Offline Scenarios ─────────────────────────────────────────────────

#[tokio::test]
async fn owner_cannot_leave_without_transfer() {
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

    // Alice (owner) tries to leave without transferring ownership
    let err = svc.leave(&ids.alice(), &cid).await.unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Forbidden(_)));
    assert!(err
        .to_string()
        .contains("owner must transfer ownership or dissolve"));
}

#[tokio::test]
async fn owner_can_transfer_and_then_leave() {
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

    // Alice transfers ownership to Bob
    svc.transfer_ownership(&ids.alice(), &cid, &ids.bob())
        .await
        .unwrap();

    // Now Alice can leave (she's no longer owner)
    svc.leave(&ids.alice(), &cid).await.unwrap();

    // Verify Alice is no longer a member
    let members = svc.list_members(&cid).await.unwrap();
    assert!(!members.iter().any(|m| m.user_id == ids.alice()));
}

#[tokio::test]
async fn admin_can_still_manage_when_owner_offline() {
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
    svc.set_role(&ids.alice(), &cid, &ids.bob(), MemberRole::Admin)
        .await
        .unwrap();

    // Bob (admin) can add a new member even if Alice (owner) is "offline"
    // We simulate this by just having Alice not being present - Bob can still act
    svc.add_member(&ids.bob(), &cid, &ids.carol()).await.unwrap();

    // Bob can also set announcement
    svc.set_announcement(&ids.bob(), &cid, "Admin announcement".into())
        .await
        .unwrap();

    let members = svc.list_members(&cid).await.unwrap();
    assert_eq!(members.len(), 3); // Alice, Bob, and Carol
}

#[tokio::test]
async fn member_cannot_perform_admin_actions() {
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
    svc.add_member(&ids.alice(), &cid, &ids.carol()).await.unwrap();

    // Bob (member) cannot:
    // - Add member
    let err = svc
        .add_member(&ids.bob(), &cid, &UserId::from("dave"))
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));

    // - Set role
    let err = svc
        .set_role(&ids.bob(), &cid, &ids.carol(), MemberRole::Admin)
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));

    // - Set announcement
    let err = svc
        .set_announcement(&ids.bob(), &cid, "Hello".into())
        .await
        .unwrap_err();
    assert!(matches!(err, a3chat_app::error::AppError::Domain(_)));
}
