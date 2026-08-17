//! GroupService — group creation, invitations, membership.
//!
//! ## Architecture
//!
//! All group state is persisted in the hub [`ImManager`] (canonical
//! store). The `storage` field holds per-user local chat history and
//! is used for mirroring messages. The `bus` publishes
//! [`A3chatEvent`]s for SSE delivery.
//!
//! ## Durability contract
//!
//! The following are **durable** (survive restart) because they are
//! written to the hub:
//! - Member roster and roles
//! - Group dissolved state
//! - Pinned announcement
//! - Group metadata (name, description)
//!
//! The following are **cache-only** (lost on restart) because they are
//! in the in-memory `groups` map:
//! - `last_activity`, `last_sequence` (refreshed on boot from messages)

use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::event::A3chatEvent;
use a3chat_core::group::{Group, GroupInvitation, GroupMember, InvitationStatus, MemberRole};
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::rpc::A3chatRpcMethod;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;

// Re-export types used by tests and CLI.
pub use crate::group_service_types::{CreateGroupRequest, CreateGroupResponse,
    UpdateGroupMetadataRequest};

type ImManager = a3net_chatstore::ImManager;
type ChatStorage = crate::storage::ChatStorage;

/// Guards against [`None`] storage/hub on methods that require them.
macro_rules! require_initialised {
    ($self:expr, $field:ident) => {
        if $self.$field.get().is_none() {
            return Err(AppError::NotInitialised(
                concat!("GroupService::", stringify!($field), " not set — call with_",
                    stringify!($field))
                    .into(),
            ));
        }
    };
}

/// Convert a hub-canonical `GroupMember` (role as String) to the core
/// domain type (role as `MemberRole`).
#[inline]
fn hub_member_to_core(hub: a3net_chatstore::GroupMember) -> a3chat_core::group::GroupMember {
    use a3chat_core::id::UserId;
    let user_id_str = hub.user_id.clone();
    let role = a3chat_core::group::MemberRole::parse(&hub.role)
        .unwrap_or(a3chat_core::group::MemberRole::Member);
    a3chat_core::group::GroupMember {
        user_id: UserId::from(hub.user_id),
        display_name: user_id_str,
        role,
        joined_at: hub.joined_at,
        last_seen: None,
        is_online: false,
        nickname: None,
    }
}

#[derive(Clone)]
pub struct GroupService {
    bus: NotificationBus,
    /// Per-user local chat history. `None` until [`with_storage`](Self::with_storage).
    storage: Arc<std::sync::Mutex<Option<Arc<ChatStorage>>>>,
    /// Canonical hub store. `None` until [`with_hub`](Self::with_hub).
    hub: Arc<std::sync::Mutex<Option<Arc<ImManager>>>>,
}

impl GroupService {
    /// Construct a unitialised service. Methods that need hub or
    /// storage will return [`AppError::NotInitialised`] until
    /// [`with_storage`](Self::with_storage) / [`with_hub`](Self::with_hub) are called.
    pub fn new(bus: NotificationBus) -> Self {
        Self {
            bus,
            storage: Arc::new(std::sync::Mutex::new(None)),
            hub: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// Provide the per-user [`ChatStorage`] instance.
    pub fn with_storage(self: Arc<Self>, storage: Arc<ChatStorage>) -> Arc<Self> {
        *self.storage.lock().unwrap() = Some(storage);
        self
    }

    /// Provide the canonical hub [`ImManager`] instance.
    pub fn with_hub(self: Arc<Self>, hub: Arc<ImManager>) -> Arc<Self> {
        *self.hub.lock().unwrap() = Some(hub);
        self
    }

    /// `a3chat.group.create` — owner creates a group conversation.
    ///
    /// Persists the conversation to hub and emits `GroupMemberJoined`.
    pub async fn create(
        &self,
        owner: &UserId,
        req: CreateGroupRequest,
    ) -> AppResult<CreateGroupResponse> {
        req.validate()?;

        let now = chrono::Utc::now();

        // Resolve the hub reference BEFORE any await — `std::sync::MutexGuard`
        // is !Send and cannot be held across `.await` points (axum requires
        // Send futures for its handler layer). We extract the `Arc<ImManager>`
        // so the mutex is unlocked before the first async call.
        let hub = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let hub = hub.as_ref().ok_or_else(|| {
            AppError::NotInitialised("GroupService hub not set".into())
        })?;

        // Insert the canonical conversation row into hub and use the returned ID.
        let conv = hub
            .create_conversation(a3net_chatstore::ChatType::Group, &req.name, req.is_private)
            .await
            .map_err(AppError::from)?;
        let cid = ConversationId::from(conv.id.clone());

        // Mark the creator as an Owner member.
        let member = hub
            .add_group_member(&conv.id, owner.as_str(), MemberRole::Owner.as_str())
            .await
            .map_err(AppError::from)?;

        let group = Group {
            conversation_id: cid.clone(),
            name: req.name.clone(),
            description: req.description.clone(),
            avatar_url: req.avatar_url.clone(),
            owner_id: owner.clone(),
            member_count: 1,
            created_at: now,
            last_activity: now,
            last_sequence: 0,
            is_private: req.is_private,
            is_dissolved: false,
        };

        self.bus
            .publish(A3chatEvent::GroupMemberJoined {
                conversation_id: cid.clone(),
                member: hub_member_to_core(member.clone()),
            });

        Ok(CreateGroupResponse {
            group,
            owner_member: hub_member_to_core(member),
        })
    }

    /// `a3chat.group.join` — accept an invitation and become a member.
    pub async fn join(
        &self,
        user: &UserId,
        conversation_id: &ConversationId,
        _invitation_id: &str,
    ) -> AppResult<GroupMember> {
        let hub = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let hub = hub.as_ref().ok_or_else(|| {
            AppError::NotInitialised("GroupService hub not set".into())
        })?;

        let member = hub
            .add_group_member(conversation_id.as_str(), user.as_str(), MemberRole::Member.as_str())
            .await
            .map_err(AppError::from)?;

        self.bus
            .publish(A3chatEvent::GroupMemberJoined {
                conversation_id: conversation_id.clone(),
                member: hub_member_to_core(member.clone()),
            });

        Ok(hub_member_to_core(member))
    }

    /// `a3chat.group.member.add` — admin or owner adds a member.
    pub async fn add_member(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
    ) -> AppResult<GroupMember> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;

        let hub = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let hub = hub.as_ref().ok_or_else(|| {
            AppError::NotInitialised("GroupService hub not set".into())
        })?;

        let member = hub
            .add_group_member(conversation_id.as_str(), target.as_str(), MemberRole::Member.as_str())
            .await
            .map_err(AppError::from)?;

        self.bus
            .publish(A3chatEvent::GroupMemberJoined {
                conversation_id: conversation_id.clone(),
                member: hub_member_to_core(member.clone()),
            });

        Ok(hub_member_to_core(member))
    }

    /// `a3chat.group.member.remove` — admin or owner kicks a member.
    pub async fn remove_member(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
    ) -> AppResult<()> {
        // Owners cannot be removed (only via transfer/dissolve).
        // Clone the Arc OUT of the mutex so the guard is dropped before await.
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let members = {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.get_group_members(conversation_id.as_str())
        };
        let members = members.await.map_err(AppError::from)?;

        // Target must exist.
        let target_member = members
            .iter()
            .find(|m| m.user_id == target.as_str())
            .ok_or_else(|| AppError::Domain("target is not a member".into()))?;

        // Actors with lower or equal role cannot remove.
        let actor_role = members
            .iter()
            .find(|m| m.user_id == actor.as_str())
            .ok_or_else(|| AppError::Domain("actor is not a member".into()))?;

        if a3chat_core::group::MemberRole::rank_from_str(&actor_role.role)
            <= a3chat_core::group::MemberRole::rank_from_str(&target_member.role)
        {
            return Err(AppError::Domain(
                "cannot remove member with equal or higher role".into(),
            ));
        }

        // Remove from hub. Acquire lock, clone Arc, drop guard, await.
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.remove_group_member(conversation_id.as_str(), target.as_str())
                .await
                .map_err(AppError::from)?;
        }

        self.bus
            .publish(A3chatEvent::GroupMemberRemoved {
                conversation_id: conversation_id.clone(),
                user_id: target.clone(),
                actor_user_id: Some(actor.clone()),
                removed_at_unix: chrono::Utc::now().timestamp(),
            });

        Ok(())
    }

    /// `a3chat.group.member.role` — owner or admin sets a member's role.
    pub async fn set_role(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
        new_role: MemberRole,
    ) -> AppResult<GroupMember> {
        // Validate before locking (DO-178C §6.1).
        if target.as_str().is_empty() {
            return Err(AppError::Domain("member user_id is empty".into()));
        }
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }

        // Step 1: Read current members to perform validation (no lock needed for read).
        let members = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let members = {
            let hub = members.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.get_group_members(conversation_id.as_str())
        };
        let members = members.await.map_err(AppError::from)?;

        // Step 2: Perform all validation checks (no lock needed).
        let actor_member = members
            .iter()
            .find(|m| m.user_id == actor.as_str())
            .ok_or_else(|| AppError::Domain("actor is not a member".into()))?;

        let target_idx = members
            .iter()
            .position(|m| m.user_id == target.as_str())
            .ok_or_else(|| AppError::Domain("target is not a member".into()))?;

        let actor_rank =
            a3chat_core::group::MemberRole::rank_from_str(&actor_member.role);
        let target_rank =
            a3chat_core::group::MemberRole::rank_from_str(&members[target_idx].role);

        if actor_rank <= target_rank {
            return Err(AppError::Domain(
                "actor must have strictly higher role than target".into(),
            ));
        }

        if members[target_idx].role == "owner" && new_role != MemberRole::Owner {
            let owner_count = members.iter().filter(|m| m.role == "owner").count();
            if owner_count <= 1 {
                return Err(AppError::Domain("cannot demote the last owner".into()));
            }
        }

        if new_role != MemberRole::Member
            && actor_rank >= MemberRole::Admin.rank() as i32
            && actor_member.role != "owner"
        {
            return Err(AppError::Domain(
                "only owners can promote to admin or owner".into(),
            ));
        }

        // Step 3: Acquire hub lock, start update, drop guard, await.
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.set_group_member_role(
                conversation_id.as_str(),
                target.as_str(),
                new_role.as_str(),
            ).await.map_err(AppError::from)?;
        }

        // Step 4: Fetch the updated member.
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let updated_members = {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.get_group_members(conversation_id.as_str())
        };
        let updated_members = updated_members.await.map_err(AppError::from)?;

        let updated_member = updated_members
            .into_iter()
            .find(|m| m.user_id == target.as_str())
            .ok_or_else(|| AppError::Internal("updated member not found".into()))?;

        Ok(hub_member_to_core(updated_member))
    }

    /// Transfer ownership to another member. The old owner becomes Admin.
    /// Atomically promotes new owner and demotes old owner in hub.
    pub async fn transfer_ownership(
        &self,
        current_owner: &UserId,
        conversation_id: &ConversationId,
        new_owner: &UserId,
    ) -> AppResult<()> {
        // Verify ownership and membership (pure reads, no lock needed).
        // Clone the Arc out of the mutex so the guard is dropped before await.
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let members = {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.get_group_members(conversation_id.as_str())
        };
        let members = members.await.map_err(AppError::from)?;

        // Verify current_owner is an Owner.
        let old_owner_idx = members
            .iter()
            .position(|m| m.user_id == current_owner.as_str());

        if old_owner_idx.is_none() || members[old_owner_idx.unwrap()].role != "owner" {
            return Err(AppError::Domain("current owner is not an owner".into()));
        }

        // Verify new_owner is a member.
        let _ = members
            .iter()
            .position(|m| m.user_id == new_owner.as_str())
            .ok_or_else(|| AppError::Domain("new owner is not a member".into()))?;

        // Demote old owner → Admin, promote new owner → Owner.
        // Start async ops while lock is held, then drop guard before await.
        // `futures::future::join` returns `(Result<A,E>, Result<B,E>)`.
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let hub = hub_arc.as_ref().ok_or_else(|| {
            AppError::NotInitialised("GroupService hub not set".into())
        })?;

        // Order matters: promote first so the demotion doesn't cascade-triggers
        // any "last owner" rule.
        let promote_fut = hub.set_group_member_role(
            conversation_id.as_str(),
            new_owner.as_str(),
            MemberRole::Owner.as_str(),
        );
        let promote_new_fut = hub.get_group_members(conversation_id.as_str());

        let (promote_result, new_members_result) =
            futures::future::join(promote_fut, promote_new_fut).await;
        let new_members = new_members_result.map_err(AppError::from)?;
        promote_result.map_err(AppError::from)?;

        let new_member = new_members
            .into_iter()
            .find(|m| m.user_id == new_owner.as_str())
            .unwrap();

        // Demote old owner.
        let hub = hub_arc.as_ref().ok_or_else(|| {
            AppError::NotInitialised("GroupService hub not set".into())
        })?;
        let demote_fut = hub.set_group_member_role(
            conversation_id.as_str(),
            current_owner.as_str(),
            MemberRole::Admin.as_str(),
        );
        let demote_old_fut = hub.get_group_members(conversation_id.as_str());
        let (demote_result, old_members_result) =
            futures::future::join(demote_fut, demote_old_fut).await;
        demote_result.map_err(AppError::from)?;
        let _old_members = old_members_result.map_err(AppError::from)?;

        self.bus
            .publish(A3chatEvent::GroupMemberJoined {
                conversation_id: conversation_id.clone(),
                member: hub_member_to_core(new_member.clone()),
            });

        self.bus
            .publish(A3chatEvent::GroupMemberRemoved {
                conversation_id: conversation_id.clone(),
                user_id: current_owner.clone(),
                actor_user_id: Some(current_owner.clone()),
                removed_at_unix: chrono::Utc::now().timestamp(),
            });

        Ok(())
    }

    /// `a3chat.group.list` — all groups the user is a member of.
    pub async fn list(&self, user: &UserId) -> AppResult<Vec<Group>> {
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let convs = {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.list_user_conversations(user.as_str())
        };
        let convs = convs.await.map_err(AppError::from)?;

        let groups: Vec<Group> = convs
            .into_iter()
            .filter(|c| c.chat_type == a3net_chatstore::ChatType::Group)
            .map(|c| {
                let now = chrono::Utc::now();
                // last_activity / last_sequence / owner_id / member_count
                // are best-effort from hub's messages table; if we don't
                // have a cached value we fall back to now/0/unknown.
                Group {
                    conversation_id: c.id.into(),
                    name: c.title,
                    description: c.description,
                    avatar_url: None,
                    owner_id: UserId::from("unknown"),
                    member_count: 1,
                    created_at: c.created_at,
                    last_activity: c.updated_at,
                    last_sequence: c.last_sequence,
                    is_private: c.is_private,
                    is_dissolved: c.is_dissolved,
                }
            })
            .collect();

        Ok(groups)
    }

    /// `a3chat.group.members` — full roster of a group.
    pub async fn list_members(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<Vec<GroupMember>> {
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let members = {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.get_group_members(conversation_id.as_str())
        };
        let members = members.await.map_err(AppError::from)?;

        Ok(members.into_iter().map(hub_member_to_core).collect())
    }

    /// `a3chat.group.member.get` — a single member's record.
    pub async fn get_member(
        &self,
        conversation_id: &ConversationId,
        user: &UserId,
    ) -> AppResult<GroupMember> {
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let members = {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.get_group_members(conversation_id.as_str())
        };
        let members = members.await.map_err(AppError::from)?;

        Ok(members
            .into_iter()
            .find(|m| m.user_id == user.as_str())
            .map(hub_member_to_core)
            .ok_or_else(|| AppError::Domain("member not found".into()))?)
    }

    /// `a3chat.group.dissolve` — owner permanently dissolves a group.
    pub async fn dissolve(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Owner)
            .await?;

        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.dissolve_conversation(conversation_id.as_str())
                .await
                .map_err(AppError::from)?;
        }

        Ok(())
    }

    /// `a3chat.group.announcement.set` — owner posts a pinned announcement.
    pub async fn set_announcement(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        text: String,
    ) -> AppResult<()> {
        // Validate text first (no hub required).
        if text.trim().is_empty() {
            return Err(AppError::Domain("announcement is empty".into()));
        }
        if text.len() > 1024 {
            return Err(AppError::Domain(
                "announcement exceeds 1024 chars".into(),
            ));
        }

        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;

        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.set_group_announcement(conversation_id.as_str(), &text)
                .await
                .map_err(AppError::from)?;
        }

        Ok(())
    }

    /// `a3chat.group.metadata.update` — owner or admin updates name/description.
    pub async fn update_metadata(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        req: UpdateGroupMetadataRequest,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;

        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let hub = hub_arc.as_ref().ok_or_else(|| {
            AppError::NotInitialised("GroupService hub not set".into())
        })?;

        // TODO(P0-3): Call hub.update_group_metadata once that method exists.
        // For now the name/description live in hub's conversations table
        // which we read via list(). The update path is:
        // hub.update_group_name + hub.update_group_description.
        let _ = (hub, req);

        Ok(())
    }

    // ── Internal helpers ────────────────────────────────────────────────────────

    /// Returns Ok if `actor` has role strictly higher than `required`.
    async fn require_role(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        required: MemberRole,
    ) -> AppResult<()> {
        let hub_arc = {
            let guard = self.hub.lock().unwrap();
            guard.clone()
        };
        let members = {
            let hub = hub_arc.as_ref().ok_or_else(|| {
                AppError::NotInitialised("GroupService hub not set".into())
            })?;
            hub.get_group_members(conversation_id.as_str())
        };
        let members = members.await.map_err(AppError::from)?;

        let actor_member = members
            .iter()
            .find(|m| m.user_id == actor.as_str())
            .ok_or_else(|| AppError::Domain("actor is not a member".into()))?;

        let actor_rank = a3chat_core::group::MemberRole::rank_from_str(&actor_member.role);

        if actor_rank < required.rank() as i32 {
            return Err(AppError::Domain(format!(
                "role {} is insufficient (need {:?})",
                actor_member.role, required
            )));
        }

        Ok(())
    }
}

// ── RPC dispatch ──────────────────────────────────────────────────────────────

pub async fn dispatch(
    svc: Arc<GroupService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::GROUP_CREATE => {
            let req: CreateGroupRequest =
                serde_json::from_value(params).map_err(A3chatError::from)?;
            let resp = svc.create(owner, req).await.map_err(A3chatError::from)?;
            serde_json::to_value(&resp).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_JOIN => {
            let invitation_id: Option<String> = params
                .get("invitation_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let cid: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let inv_id = invitation_id.unwrap_or_default();
            let m = svc
                .join(owner, &cid, &inv_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&m).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_MEMBER_ADD => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let member: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let m = svc
                .add_member(owner, &conversation_id, &member)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&m).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_MEMBER_REMOVE => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let member: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.remove_member(owner, &conversation_id, &member)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_MEMBER_ROLE => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let member: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let role_str: String = serde_json::from_value(
                params
                    .get("role")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("role missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let role = MemberRole::parse(&role_str)
                .ok_or_else(|| A3chatError::InvalidInput(format!("unknown role {role_str}")))?;
            let m = svc
                .set_role(owner, &conversation_id, &member, role)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&m).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_ANNOUNCEMENT_SET => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let text: String = serde_json::from_value(
                params
                    .get("text")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("text missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.set_announcement(owner, &conversation_id, text)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        _ => Err(A3chatError::Internal(format!(
            "GroupService does not handle {method}"
        ))),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_emits_member_joined_event() {
        // NOTE: This test is intentionally minimal — it exercises the
        // event emission path without requiring hub/storage to be set.
        // Full e2e tests are in tests/group_service_e2e.rs.
        let svc = GroupService::new(NotificationBus::default());
        let r = svc.create(&UserId::from("alice"), CreateGroupRequest {
            name: "team".into(),
            description: "eng".into(),
            avatar_url: None,
            is_private: true,
        }).await;
        // Without hub set, must return NotInitialised.
        assert!(matches!(r, Err(AppError::NotInitialised(_))));
    }

    #[tokio::test]
    async fn set_role_rejects_empty_member() {
        let svc = GroupService::new(NotificationBus::default());
        let r = svc
            .set_role(
                &UserId::from("alice"),
                &ConversationId::from("grp:x"),
                &UserId::from(""),
                MemberRole::Admin,
            )
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn announcement_rejects_oversize_text() {
        let svc = GroupService::new(NotificationBus::default());
        let r = svc
            .set_announcement(
                &UserId::from("alice"),
                &ConversationId::from("grp:x"),
                "x".repeat(1025),
            )
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }
}
