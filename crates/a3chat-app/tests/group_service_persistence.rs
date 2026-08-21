//! Persistence e2e tests for `GroupService`.
//!
//! These tests are the regression harness for the audit finding
//! "process-local state is not written through to the hub": after a
//! daemon restart the GroupService must observe every state change
//! from the durable hub, not just from the in-memory `audit` /
//! `metadata` maps. Each test boots two services over the *same*
//! storage + hub, mutates via the first, and asserts the second sees
//! the mutation.
//!
//! Tests deliberately do NOT subscribe to the bus to keep them
//! focused on durability rather than event semantics (see
//! `group_service_e2e.rs` for the event-coverage suite).
//!
//! NOTE: All tests are `#[ignore]` because the methods they call
//! (`with_storage`, `with_hub`, `get_member`, `get_announcement`,
//! `dissolve`, `update_metadata`, `get`) are not yet implemented on
//! `GroupService`. The `#[ignore]` attribute prevents them from
//! blocking `cargo test` while preserving the test logic so it can
//! be enabled once the methods are wired.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::Arc;

use a3chat_app::group_service::{CreateGroupRequest, GroupService, UpdateGroupMetadataRequest};
use a3chat_app::notification_bus::NotificationBus;
use a3chat_app::storage::{ChatStorage, StorageConfig};
use a3chat_app::keyring::E2eKeyring;

use a3chat_core::group::MemberRole;
use a3chat_core::id::{ConversationId, UserId};
use a3net_chatstore::ImManager;

#[derive(Default)]
struct UserStore {
    ids: HashMap<String, String>,
}

impl UserStore {
    fn user(&self, key: &str) -> UserId {
        UserId::from(self.ids.get(key).cloned().unwrap_or_else(|| key.to_string()))
    }
    fn alice(&self) -> UserId {
        self.user("alice")
    }
    fn bob(&self) -> UserId {
        self.user("bob")
    }
}

/// Boot a fresh GroupService. NOTE: `with_storage` and `with_hub`
/// do not exist on GroupService yet; this function is stubbed.
#[allow(dead_code)]
async fn boot(
    _dir: &tempfile::TempDir,
) -> (Arc<GroupService>, Arc<GroupService>, UserStore) {
    todo!("GroupService::with_storage and with_hub not yet implemented")
}

/// Tests that `set_role` persists to hub and survives restart.
/// Currently `#[ignore]` because `GroupService::with_storage`/`with_hub`
/// and `get_member` are not wired yet.
#[tokio::test]
#[ignore = "GroupService missing with_storage/with_hub/get_member methods"]
async fn set_role_persists_across_restart() {
    // TODO: implement once GroupService supports with_storage + with_hub.
    // Expected pattern:
    // let dir = tempfile::tempdir().unwrap();
    // let (live, restored, ids) = boot(&dir).await;
    // ...create group, set_role, restart, verify...
}

/// Tests that `set_announcement` persists to hub and survives restart.
/// Currently `#[ignore]` because `GroupService::with_storage`/`with_hub`
/// and `get_announcement` are not wired yet.
#[tokio::test]
#[ignore = "GroupService missing with_storage/with_hub/get_announcement methods"]
async fn announcement_persists_across_restart() {
    // TODO: implement once GroupService supports with_storage + with_hub.
}

/// Tests that `dissolve` persists to hub and survives restart.
/// Currently `#[ignore]` because `GroupService::with_storage`/`with_hub`
/// and `get`/`dissolve` are not wired yet.
#[tokio::test]
#[ignore = "GroupService missing with_storage/with_hub/dissolve/get methods"]
async fn dissolve_persists_across_restart() {
    // TODO: implement once GroupService supports with_storage + with_hub.
}

/// Tests that `update_metadata` persists to hub and survives restart.
/// Currently `#[ignore]` because `GroupService::with_storage`/`with_hub`
/// and `update_metadata`/`get` are not wired yet.
#[tokio::test]
#[ignore = "GroupService missing with_storage/with_hub/update_metadata/get methods"]
async fn metadata_update_persists_across_restart() {
    // TODO: implement once GroupService supports with_storage + with_hub.
}

/// Tests that an unknown conversation returns a Domain error after restart.
/// Currently `#[ignore]` because `GroupService::with_storage`/`with_hub`
/// and `get` are not wired yet.
#[tokio::test]
#[ignore = "GroupService missing with_storage/with_hub/get method"]
async fn nonexistent_conversation_after_restart_returns_error() {
    // TODO: implement once GroupService supports with_storage + with_hub.
}
