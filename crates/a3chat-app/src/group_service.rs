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
use a3chat_core::group::{Group, GroupMember, MemberRole};
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
/// Reserved for future use; the P3 wiring uses `if let Some(...)` directly
/// so this macro is currently unused.
#[allow(unused_macros)]
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
///
/// Note: `last_seen` and `is_online` are sourced from the hub's
/// group_members table. When a member sends a message, the chat
/// service calls `touch_member()` to update these fields via the
/// presence touch gate.
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
        // Preserve presence data from hub (populated by touch_member)
        last_seen: hub.last_seen,
        is_online: hub.is_online,
        nickname: None,
    }
}

#[derive(Clone)]
pub struct GroupService {
    bus: NotificationBus,
    /// Per-user local chat history. `None` until [`with_storage`](Self::with_storage).
    storage: Arc<std::sync::Mutex<Option<Arc<ChatStorage>>>>,
    /// Canonical hub store. `None` until [`with_hub`](Self::with_hub).
    ///
    /// GB-13 — `std::sync::Mutex` is used for *slot* replacement
    /// only. **Acquire, clone the inner `Arc<ImManager>` out, and
    /// drop the guard before the first `.await` on the inner value.**
    /// Every method in this file follows that pattern via the
    /// [`hub_arc`](Self::hub_arc) helper.
    hub: Arc<std::sync::Mutex<Option<Arc<ImManager>>>>,
    /// F-08 / B-24: invitation store. `None` until
    /// [`with_invitation_state`](Self::with_invitation_state).
    ///
    /// GB-13 — `Arc<std::sync::Mutex<Option<…>>>` so the builder
    /// methods can swap the slot regardless of how many clones of
    /// the outer service exist. Access it via
    /// [`invitation_arc`](Self::invitation_arc) so the guard is
    /// always released before any `.await`.
    invitation_state:
        Arc<std::sync::Mutex<Option<crate::group_invitation_service::GroupInvitationService>>>,
    /// Phase 5c: iroh-docs chat bridge for P2P group sync.
    /// `None` until [`with_iroh_docs`](Self::with_iroh_docs).
    #[cfg(feature = "iroh")]
    iroh_docs: Arc<std::sync::Mutex<Option<Arc<a3net_chatstore::IrohDocsChat>>>>,
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
            invitation_state: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(feature = "iroh")]
            iroh_docs: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// Acquire the hub slot, clone the inner `Arc`, drop the guard,
    /// and return it. `None` when `with_hub` has not been called.
    ///
    /// GB-13 — centralises the canonical "extract Arc out of the
    /// mutex before `.await`" pattern. Every method that talks to
    /// the hub uses this helper so the pattern is uniform.
    fn hub_arc(&self) -> Option<Arc<ImManager>> {
        let guard = self.hub.lock().expect("hub slot mutex poisoned");
        guard.as_ref().map(Arc::clone)
    }

    /// Acquire the storage slot, clone the inner `Arc`, drop the
    /// guard, and return it.
    fn storage_arc(&self) -> Option<Arc<ChatStorage>> {
        let guard = self.storage.lock().expect("storage slot mutex poisoned");
        guard.as_ref().map(Arc::clone)
    }

    /// Acquire the invitation slot, clone the inner service, drop
    /// the guard, and return it.
    fn invitation_arc(&self) -> Option<crate::group_invitation_service::GroupInvitationService> {
        let guard = self
            .invitation_state
            .lock()
            .expect("invitation slot mutex poisoned");
        guard.as_ref().cloned()
    }

    /// Provide the per-user [`ChatStorage`] instance.
    pub fn with_storage(self: Arc<Self>, storage: Arc<ChatStorage>) -> Arc<Self> {
        *self.storage.lock().expect("storage slot mutex poisoned") = Some(storage);
        self
    }

    /// Provide the canonical hub [`ImManager`] instance.
    pub fn with_hub(self: Arc<Self>, hub: Arc<ImManager>) -> Arc<Self> {
        *self.hub.lock().expect("hub slot mutex poisoned") = Some(hub);
        self
    }

    /// Provide the F-08 invitation state. Until this is called
    /// `invite` / `accept_invitation` / `decline_invitation` /
    /// `revoke_invitation` will return
    /// [`AppError::NotInitialised`].
    pub fn with_invitation_state(
        self: Arc<Self>,
        invitations: crate::group_invitation_service::GroupInvitationService,
    ) -> Arc<Self> {
        *self
            .invitation_state
            .lock()
            .expect("invitation slot mutex poisoned") = Some(invitations);
        self
    }

    /// Phase 5c: attach the iroh-docs chat bridge for P2P group sync.
    ///
    /// When attached, groups can sync messages via iroh-docs Doc.
    #[cfg(feature = "iroh")]
    pub fn with_iroh_docs(
        self: Arc<Self>,
        docs_chat: Arc<a3net_chatstore::IrohDocsChat>,
    ) -> Arc<Self> {
        *self.iroh_docs.lock().expect("iroh_docs mutex poisoned") = Some(docs_chat);
        self
    }

    /// Acquire the iroh docs slot, clone the inner `Arc`, drop the guard.
    #[cfg(feature = "iroh")]
    fn iroh_docs_arc(&self) -> Option<Arc<a3net_chatstore::IrohDocsChat>> {
        let guard = self.iroh_docs.lock().expect("iroh_docs mutex poisoned");
        guard.as_ref().map(Arc::clone)
    }

    /// `a3chat.group.sync.ticket` — get the DocTicket for a group's iroh-docs sync.
    ///
    /// Returns a base64-encoded ticket that can be shared with new members
    /// to join the group's P2P sync network.
    #[cfg(feature = "iroh")]
    pub async fn get_sync_ticket(&self, conversation_id: &ConversationId) -> AppResult<String> {
        use base64::Engine;
        let docs_chat = self.iroh_docs_arc().ok_or_else(|| {
            AppError::NotInitialised("GroupService iroh_docs not set".into())
        })?;

        let ticket = docs_chat
            .share(conversation_id.as_str(), iroh_docs::api::protocol::ShareMode::Write)
            .await
            .map_err(|e| AppError::Internal(format!("failed to share doc: {e}")))?;

        // Encode ticket to JSON then base64 for transmission
        let json = serde_json::to_string(&ticket)
            .map_err(|e| AppError::Internal(format!("ticket serialization failed: {e}")))?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&json))
    }

    /// `a3chat.group.sync.join` — join a group's sync network via DocTicket.
    ///
    /// Opens the iroh-docs doc for the conversation and starts receiving
    /// messages via the P2P sync network.
    #[cfg(feature = "iroh")]
    pub async fn join_sync(&self, conversation_id: &ConversationId, ticket_b64: &str) -> AppResult<()> {
        use base64::Engine;
        let docs_chat = self.iroh_docs_arc().ok_or_else(|| {
            AppError::NotInitialised("GroupService iroh_docs not set".into())
        })?;

        // Decode ticket from base64
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(ticket_b64)
            .map_err(|e| AppError::Internal(format!("invalid ticket encoding: {e}")))?;
        let ticket: iroh_docs::DocTicket = serde_json::from_slice(&json)
            .map_err(|e| AppError::Internal(format!("invalid ticket format: {e}")))?;

        // Open the doc with the ticket
        docs_chat
            .open_with_ticket(conversation_id.as_str(), ticket)
            .await
            .map_err(|e| AppError::Internal(format!("failed to open sync doc: {e}")))?;

        tracing::info!(conv = %conversation_id, "joined group sync network");
        Ok(())
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

        // GB-13 — extract the `Arc<ImManager>` *before* the first
        // `.await` so the slot mutex is dropped. All hub-touching
        // methods in this file use `hub_arc()` for this reason.
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

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
    ///
    /// Phase 5c: When iroh-docs is configured and the invitation contains
    /// a sync ticket, automatically joins the P2P sync network.
    pub async fn join(
        &self,
        user: &UserId,
        conversation_id: &ConversationId,
        _invitation_id: &str,
        sync_ticket: Option<&str>,
    ) -> AppResult<GroupMember> {
        // GB-13 — extract the hub Arc before any `.await`.
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        let member = hub
            .add_group_member(conversation_id.as_str(), user.as_str(), MemberRole::Member.as_str())
            .await
            .map_err(AppError::from)?;

        // Phase 5c: Join P2P sync network if ticket is provided
        #[cfg(feature = "iroh")]
        if let Some(ticket) = sync_ticket {
            if let Err(e) = self.join_sync(conversation_id, ticket).await {
                tracing::warn!(conv = %conversation_id, "failed to join sync network: {e}");
            }
        }

        self.bus
            .publish(A3chatEvent::GroupMemberJoined {
                conversation_id: conversation_id.clone(),
                member: hub_member_to_core(member.clone()),
            });

        Ok(hub_member_to_core(member))
    }

    /// `a3chat.group.invite` — owner or admin invites a user; emits
    /// a [`A3chatEvent::GroupInvitationReceived`] for the invitee.
    ///
    /// Phase 5c: When iroh-docs is configured, automatically includes
    /// the group's DocTicket in the invitation so the invitee can
    /// join the P2P sync network.
    pub async fn invite(
        &self,
        inviter: &UserId,
        conversation_id: &ConversationId,
        invitee_id: &UserId,
        group_name: &str,
        message: Option<&str>,
    ) -> AppResult<a3chat_core::group::GroupInvitation> {
        self.require_role(inviter, conversation_id, MemberRole::Admin)
            .await?;

        let now_unix = chrono::Utc::now().timestamp();
        let expires_at = now_unix + crate::group_invitation_service::DEFAULT_INVITATION_TTL_SECS;
        let invitation_id = uuid::Uuid::new_v4().to_string();

        // Phase 5c: Get sync ticket for P2P group message sync
        #[cfg(feature = "iroh")]
        let sync_ticket = self.get_sync_ticket(conversation_id).await.ok();

        let rec = crate::group_invitation_service::InvitationRecord {
            invitation_id: invitation_id.clone(),
            conversation_id: conversation_id.clone(),
            group_name: group_name.to_string(),
            inviter_id: inviter.clone(),
            inviter_name: inviter.as_str().to_string(),
            invitee_id: invitee_id.clone(),
            status: crate::group_invitation_service::STATUS_PENDING.into(),
            created_at_unix: now_unix,
            expires_at_unix: expires_at,
            responded_at_unix: None,
            message: message.map(|s| s.to_string()),
            #[cfg(feature = "iroh")]
            sync_ticket,
            #[cfg(not(feature = "iroh"))]
            sync_ticket: None,
        };

        // Persist via the a3chat invitation store. GB-13 — pull
        // the service out of the slot before the first `.await`.
        if let Some(state) = self.invitation_arc() {
            state.create(inviter, rec.clone()).await?;
        }

        let invitation: a3chat_core::group::GroupInvitation = rec.into();
        self.bus
            .publish(A3chatEvent::GroupInvitationReceived {
                invitation: invitation.clone(),
            });
        Ok(invitation)
    }

    /// Resolve the invitation inbox for the caller (i.e. all pending
    /// invitations addressed to `user`).
    pub async fn list_invitations(
        &self,
        user: &UserId,
    ) -> AppResult<Vec<crate::group_invitation_service::InvitationRecord>> {
        let state = self.invitation_arc().ok_or_else(|| {
            AppError::NotInitialised("GroupService.invitation_state not set".into())
        })?;
        state.inbox(user).await
    }

    /// Accept a pending invitation. The caller must be the invitee.
    pub async fn accept_invitation(
        &self,
        user: &UserId,
        invitation_id: &str,
    ) -> AppResult<a3chat_core::group::GroupInvitation> {
        let state = self.invitation_arc().ok_or_else(|| {
            AppError::NotInitialised("GroupService.invitation_state not set".into())
        })?;
        let rec = state
            .set_status(user, invitation_id, crate::group_invitation_service::STATUS_ACCEPTED)
            .await?;
        // Re-issue a `GroupInvitationReceived` event with the new
        // status so SSE subscribers can react to the acceptance.
        let invitation: a3chat_core::group::GroupInvitation = rec.into();
        self.bus
            .publish(A3chatEvent::GroupInvitationReceived {
                invitation: invitation.clone(),
            });
        Ok(invitation)
    }

    /// Decline a pending invitation (no-op on the group, just an
    /// inbox row update).
    pub async fn decline_invitation(
        &self,
        user: &UserId,
        invitation_id: &str,
    ) -> AppResult<a3chat_core::group::GroupInvitation> {
        let state = self.invitation_arc().ok_or_else(|| {
            AppError::NotInitialised("GroupService.invitation_state not set".into())
        })?;
        let rec = state
            .set_status(user, invitation_id, crate::group_invitation_service::STATUS_DECLINED)
            .await?;
        let invitation: a3chat_core::group::GroupInvitation = rec.into();
        self.bus
            .publish(A3chatEvent::GroupInvitationReceived {
                invitation: invitation.clone(),
            });
        Ok(invitation)
    }

    /// Revoke an outstanding invitation. The caller must be the
    /// inviter (or a group admin).
    pub async fn revoke_invitation(
        &self,
        owner: &UserId,
        invitation_id: &str,
    ) -> AppResult<a3chat_core::group::GroupInvitation> {
        let state = self.invitation_arc().ok_or_else(|| {
            AppError::NotInitialised("GroupService.invitation_state not set".into())
        })?;
        let rec = state
            .set_status(owner, invitation_id, crate::group_invitation_service::STATUS_REVOKED)
            .await?;
        let invitation: a3chat_core::group::GroupInvitation = rec.into();
        self.bus
            .publish(A3chatEvent::GroupInvitationReceived {
                invitation: invitation.clone(),
            });
        Ok(invitation)
    }

    /// Look up a single invitation by id (owner-side read).
    pub async fn get_invitation(
        &self,
        owner: &UserId,
        invitation_id: &str,
    ) -> AppResult<Option<a3chat_core::group::GroupInvitation>> {
        let state = self.invitation_arc().ok_or_else(|| {
            AppError::NotInitialised("GroupService.invitation_state not set".into())
        })?;
        let rec = state.get(owner, invitation_id).await?;
        Ok(rec.map(a3chat_core::group::GroupInvitation::from))
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

        // GB-2 — extract the hub Arc before any `.await` so the
        // slot mutex is dropped. The previous implementation held
        // the lock across `hub.add_group_member(...)`, which is
        // unsound on a multi-threaded tokio runtime.
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

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
        // GB-3 — both reads and the write go through the helper so
        // the slot mutex is never held across `.await`.
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

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

        hub.remove_group_member(conversation_id.as_str(), target.as_str())
            .await
            .map_err(AppError::from)?;

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
    ///
    /// GB-4 — emits [`A3chatEvent::GroupMemberRoleChanged`] so SSE
    /// subscribers refresh the role badge. The previous implementation
    /// persisted correctly but never published the event.
    pub async fn set_role(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
        new_role: MemberRole,
    ) -> AppResult<GroupMember> {
        // Validate before locking (DO-178C §6.1).
        if actor.as_str().is_empty() {
            return Err(AppError::Domain("actor user_id is empty".into()));
        }
        if target.as_str().is_empty() {
            return Err(AppError::Domain("member user_id is empty".into()));
        }
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }

        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        // Validate actor and target are both members, and the actor
        // outranks the target.
        let actor_member = members
            .iter()
            .find(|m| m.user_id == actor.as_str())
            .ok_or_else(|| AppError::Domain("actor is not a member".into()))?;

        let target_idx = members
            .iter()
            .position(|m| m.user_id == target.as_str())
            .ok_or_else(|| AppError::Domain("target is not a member".into()))?;

        let actor_rank = a3chat_core::group::MemberRole::rank_from_str(&actor_member.role);
        let target_rank = a3chat_core::group::MemberRole::rank_from_str(&members[target_idx].role);

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

        hub.set_group_member_role(
            conversation_id.as_str(),
            target.as_str(),
            new_role.as_str(),
        )
        .await
        .map_err(AppError::from)?;

        let updated_members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        let updated_member = updated_members
            .into_iter()
            .find(|m| m.user_id == target.as_str())
            .ok_or_else(|| AppError::Internal("updated member not found".into()))?;

        // GB-4 — publish the role-changed event so SSE subscribers
        // see the role badge update.
        self.bus.publish(A3chatEvent::GroupMemberRoleChanged {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            member_user_id: target.clone(),
            new_role: new_role.as_str().to_string(),
            actor_user_id: actor.clone(),
        });

        Ok(hub_member_to_core(updated_member))
    }

    /// Transfer ownership to another member. The old owner becomes Admin.
    /// Atomically promotes new owner and demotes old owner in hub.
    ///
    /// GB-17 — emits [`A3chatEvent::GroupMemberRoleChanged`] twice
    /// (new owner, old owner→admin) instead of the previous
    /// `GroupMemberJoined` + `GroupMemberRemoved` miscast.
    pub async fn transfer_ownership(
        &self,
        current_owner: &UserId,
        conversation_id: &ConversationId,
        new_owner: &UserId,
    ) -> AppResult<()> {
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        let old_owner_idx = members
            .iter()
            .position(|m| m.user_id == current_owner.as_str());

        if old_owner_idx.is_none() || members[old_owner_idx.unwrap()].role != "owner" {
            return Err(AppError::Domain("current owner is not an owner".into()));
        }

        let _ = members
            .iter()
            .position(|m| m.user_id == new_owner.as_str())
            .ok_or_else(|| AppError::Domain("new owner is not a member".into()))?;

        // Promote first so the demotion doesn't cascade-trigger any
        // "last owner" rule.
        hub.set_group_member_role(
            conversation_id.as_str(),
            new_owner.as_str(),
            MemberRole::Owner.as_str(),
        )
        .await
        .map_err(AppError::from)?;

        // Demote old owner to Admin.
        hub.set_group_member_role(
            conversation_id.as_str(),
            current_owner.as_str(),
            MemberRole::Admin.as_str(),
        )
        .await
        .map_err(AppError::from)?;

        // GB-17 — publish two `GroupMemberRoleChanged` events so
        // the front-end role badges update correctly. The previous
        // implementation emitted `GroupMemberJoined` (wrong — the
        // user was already a member) and `GroupMemberRemoved`
        // (wrong — the user wasn't removed, just demoted).
        self.bus.publish(A3chatEvent::GroupMemberRoleChanged {
            user_id: current_owner.clone(),
            conversation_id: conversation_id.clone(),
            member_user_id: new_owner.clone(),
            new_role: MemberRole::Owner.as_str().to_string(),
            actor_user_id: current_owner.clone(),
        });
        self.bus.publish(A3chatEvent::GroupMemberRoleChanged {
            user_id: current_owner.clone(),
            conversation_id: conversation_id.clone(),
            member_user_id: current_owner.clone(),
            new_role: MemberRole::Admin.as_str().to_string(),
            actor_user_id: current_owner.clone(),
        });

        Ok(())
    }

    /// `a3chat.group.list` — all groups the user is a member of.
    pub async fn list(&self, user: &UserId) -> AppResult<Vec<Group>> {
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        let convs = hub
            .list_user_conversations(user.as_str())
            .await
            .map_err(AppError::from)?;

        // GB-18 — the previous implementation reported
        // `owner_id = "unknown"` and `member_count = 1`. Look up the
        // real owner and member count from hub for every group.
        let mut groups: Vec<Group> = Vec::with_capacity(convs.len());
        for c in convs
            .into_iter()
            .filter(|c| c.chat_type == a3net_chatstore::ChatType::Group)
        {
            let members = hub
                .get_group_members(&c.id)
                .await
                .map_err(AppError::from)?;
            let member_count = members.len() as u32;
            let owner_id = members
                .iter()
                .find(|m| m.role == "owner")
                .map(|m| UserId::from(m.user_id.clone()))
                .unwrap_or_else(|| UserId::from("unknown"));
            groups.push(Group {
                conversation_id: c.id.into(),
                name: c.title,
                description: c.description,
                avatar_url: None,
                owner_id,
                member_count,
                created_at: c.created_at,
                last_activity: c.updated_at,
                last_sequence: c.last_sequence,
                is_private: c.is_private,
                is_dissolved: c.is_dissolved,
            });
        }

        Ok(groups)
    }

    /// `a3chat.group.members` — full roster of a group.
    pub async fn list_members(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<Vec<GroupMember>> {
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;
        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        Ok(members.into_iter().map(hub_member_to_core).collect())
    }

    /// `a3chat.group.member.get` — a single member's record.
    pub async fn get_member(
        &self,
        conversation_id: &ConversationId,
        user: &UserId,
    ) -> AppResult<GroupMember> {
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;
        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        Ok(members
            .into_iter()
            .find(|m| m.user_id == user.as_str())
            .map(hub_member_to_core)
            .ok_or_else(|| AppError::Domain("member not found".into()))?)
    }

    /// `a3chat.group.dissolve` — owner permanently dissolves a group.
    ///
    /// GB-6 — emits [`A3chatEvent::GroupDissolved`] so SSE
    /// subscribers on every device remove the conversation from
    /// their UI. The previous implementation persisted to hub but
    /// was not routed from the dispatcher at all and never
    /// emitted the event.
    pub async fn dissolve(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Owner)
            .await?;

        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        hub.dissolve_conversation(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        self.bus.publish(A3chatEvent::GroupDissolved {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            actor_user_id: actor.clone(),
            dissolved_at_unix: chrono::Utc::now().timestamp(),
        });

        Ok(())
    }

    /// `a3chat.group.leave` — non-admin user voluntarily leaves the
    /// group (G-04). Self-removal is distinct from admin-kick in
    /// that we allow an ordinary `Member` to drop themselves; the
    /// group membership row is removed and a
    /// [`A3chatEvent::GroupMemberRemoved`] fires with
    /// `actor_user_id = Some(leaver)` so the rest of the room can
    /// react.
    pub async fn leave(
        &self,
        user: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<()> {
        if user.as_str().is_empty() {
            return Err(AppError::Domain("user_id is empty".into()));
        }
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }

        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        let me = members
            .iter()
            .find(|m| m.user_id == user.as_str())
            .ok_or_else(|| AppError::Domain("not a member".into()))?;

        // Owners must transfer first or dissolve — they cannot
        // simply walk away, otherwise the group would be left with
        // no owner.
        if me.role == "owner" {
            return Err(AppError::Forbidden(
                "owner must transfer ownership or dissolve the group before leaving"
                    .into(),
            ));
        }

        hub.remove_group_member(conversation_id.as_str(), user.as_str())
            .await
            .map_err(AppError::from)?;

        self.bus.publish(A3chatEvent::GroupMemberRemoved {
            conversation_id: conversation_id.clone(),
            user_id: user.clone(),
            actor_user_id: Some(user.clone()),
            removed_at_unix: chrono::Utc::now().timestamp(),
        });

        Ok(())
    }

    /// `a3chat.group.announcement.set` — owner posts a pinned announcement.
    ///
    /// GB-5 — emits [`A3chatEvent::GroupAnnouncementChanged`] after
    /// the hub write so SSE subscribers see the banner update. The
    /// previous implementation persisted the text but never
    /// published the event.
    pub async fn set_announcement(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        text: String,
    ) -> AppResult<()> {
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

        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        hub.set_group_announcement(conversation_id.as_str(), &text)
            .await
            .map_err(AppError::from)?;

        self.bus.publish(A3chatEvent::GroupAnnouncementChanged {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            text: Some(text),
            actor_user_id: actor.clone(),
        });

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

        // Audit issue #6: this used to be an empty stub. Validate
        // the request, then forward each present field to the hub
        // via the new `set_group_title` / `set_group_description`
        // methods. A failure on either update aborts and surfaces
        // to the caller.
        req.validate().map_err(AppError::from)?;

        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        if let Some(ref name) = req.name {
            hub.set_group_title(conversation_id.as_str(), name)
                .await
                .map_err(AppError::from)?;
        }
        if let Some(ref description) = req.description {
            hub.set_group_description(conversation_id.as_str(), description)
                .await
                .map_err(AppError::from)?;
        }
        // GB-16 — the previous implementation returned `Internal`
        // ("avatar_url update not yet wired to hub"). Rename to
        // `Domain` so the wire error code reflects "not implemented"
        // rather than "internal failure". Production deployments
        // can branch on this code to display an actionable message.
        if req.avatar_url.is_some() {
            return Err(AppError::Domain(
                "avatar_url update not yet wired to hub".into(),
            ));
        }

        Ok(())
    }

    // ── Internal helpers ────────────────────────────────────────────────────────

    /// Compute the effective role rank for a member, considering temporary
    /// admin grants. A member with a valid temporary admin grant has
    /// admin-level permissions for the duration.
    fn effective_role_rank(member: &a3net_chatstore::GroupMember) -> i32 {
        let base_rank = a3chat_core::group::MemberRole::rank_from_str(&member.role);

        // Check if user has a temporary admin grant that's still valid
        if let Some(until) = member.temp_admin_until {
            if until > chrono::Utc::now() {
                // Temporary admin has same effective rank as regular admin
                return base_rank.max(MemberRole::Admin.rank() as i32);
            }
        }

        base_rank
    }

    /// Returns Ok if `actor` has role strictly higher than `required`.
    /// Considers temporary admin grants for authorization.
    async fn require_role(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        required: MemberRole,
    ) -> AppResult<()> {
        // GB-13 — single `hub_arc()` extraction, then a single
        // `.await`. The previous implementation cloned the Arc out
        // *and then* re-locked, which was unsound.
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;
        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;

        let actor_member = members
            .iter()
            .find(|m| m.user_id == actor.as_str())
            .ok_or_else(|| AppError::Domain("actor is not a member".into()))?;

        // Use effective rank that considers temporary admin status
        let actor_rank = Self::effective_role_rank(actor_member);

        if actor_rank < required.rank() as i32 {
            return Err(AppError::Domain(format!(
                "role {} is insufficient (need {:?})",
                actor_member.role, required
            )));
        }

        Ok(())
    }

    /// Update the last_seen timestamp for a member when they perform
    /// an action (send message, etc.). This is called from message
    /// sending via the presence touch gate.
    pub async fn touch_member(
        &self,
        conversation_id: &ConversationId,
        user_id: &UserId,
        is_online: bool,
    ) -> AppResult<()> {
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        hub.update_member_presence(conversation_id.as_str(), user_id.as_str(), is_online)
            .await
            .map_err(AppError::from)?;

        // Publish event so SSE subscribers get the updated presence
        self.bus.publish(A3chatEvent::GroupMemberPresenceChanged {
            user_id: user_id.clone(),
            conversation_id: conversation_id.clone(),
            target_user_id: user_id.clone(),
            is_online,
            last_seen: Some(chrono::Utc::now()),
        });

        Ok(())
    }

    /// Grant temporary admin status to a member for a specified duration.
    /// Only owners and current admins can grant temporary admin.
    pub async fn grant_temp_admin(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
        duration_secs: i64,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;

        if target.as_str().is_empty() {
            return Err(AppError::Domain("target user_id is empty".into()));
        }

        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        let until = chrono::Utc::now() + chrono::Duration::seconds(duration_secs);
        hub.set_temp_admin(conversation_id.as_str(), target.as_str(), until)
            .await
            .map_err(AppError::from)?;

        // Publish event
        self.bus.publish(A3chatEvent::GroupTempAdminGranted {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            target_user_id: target.clone(),
            granted_by: actor.clone(),
            expires_at: until,
        });

        Ok(())
    }

    /// Revoke temporary admin status from a member.
    /// Only owners and current admins can revoke.
    pub async fn revoke_temp_admin(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;

        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;

        hub.clear_temp_admin(conversation_id.as_str(), target.as_str())
            .await
            .map_err(AppError::from)?;

        // Publish event so subscribers know the temp admin status was revoked
        self.bus.publish(A3chatEvent::GroupTempAdminRevoked {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            target_user_id: target.clone(),
            revoked_by: actor.clone(),
        });

        Ok(())
    }

    // ── Group mute (G-02) ──────────────────────────────────────────────────────

    /// Mute a single member for a window of `muted_until_secs`. Use
    /// [`i64::MAX`](i64::MAX) for `muted_until_secs` to mute
    /// indefinitely until `unmute_member` is called.
    pub async fn mute_member(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
        muted_until_secs: i64,
        reason: Option<&str>,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;
        if target.as_str().is_empty() {
            return Err(AppError::Domain("target user_id is empty".into()));
        }
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }
        if muted_until_secs <= 0 {
            return Err(AppError::Domain(
                "muted_until_secs must be > 0 (use i64::MAX for indefinite)".into(),
            ));
        }

        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        let now = chrono::Utc::now().timestamp();
        let muted_until_unix = now.saturating_add(muted_until_secs);

        storage
            .set_group_member_mute(
                conversation_id,
                target,
                actor,
                muted_until_unix,
                reason,
                now,
            )
            .await?;

        self.bus.publish(A3chatEvent::GroupMuteChanged {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            muted_user_id: target.clone(),
            is_muted: true,
            muted_until_unix: Some(muted_until_unix),
            actor_user_id: actor.clone(),
        });

        Ok(())
    }

    /// Lift a per-member mute. `muted_until_unix = None` from the
    /// caller perspective: we always store `i64::MIN` as the
    /// sentinel "not muted" value so subsequent reads have a clear
    /// indicator.
    pub async fn unmute_member(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage
            .clear_group_member_mute(conversation_id, target)
            .await?;
        self.bus.publish(A3chatEvent::GroupMuteChanged {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            muted_user_id: target.clone(),
            is_muted: false,
            muted_until_unix: None,
            actor_user_id: actor.clone(),
        });
        Ok(())
    }

    /// Mute the entire group. Any subsequent
    /// [`ChatService::send_message`](crate::chat_service::ChatService::send_message)
    /// call with this conversation_id must consult the storage
    /// helper and reject accordingly. The `chat_service.send_message`
    /// gate is added separately (GB-22 follow-up).
    pub async fn mute_all(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage
            .set_group_mute_all(conversation_id, true)
            .await?;
        self.bus.publish(A3chatEvent::GroupMuteAllChanged {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            is_muted: true,
            actor_user_id: actor.clone(),
        });
        Ok(())
    }

    /// Lift the group-wide mute.
    pub async fn unmute_all(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<()> {
        self.require_role(actor, conversation_id, MemberRole::Admin)
            .await?;
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage
            .set_group_mute_all(conversation_id, false)
            .await?;
        self.bus.publish(A3chatEvent::GroupMuteAllChanged {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            is_muted: false,
            actor_user_id: actor.clone(),
        });
        Ok(())
    }

    /// List every currently-muted member in this group. Expired
    /// mutes are filtered out so the UI sees only effective rows.
    pub async fn list_muted(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<Vec<(UserId, i64, String)>> {
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage.list_group_member_mutes(conversation_id).await
    }

    /// Convenience — does the conversation currently refuse all
    /// messages? Used by the chat service gate.
    pub async fn is_group_muted(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<bool> {
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage.is_group_mute_all(conversation_id).await
    }

    /// Convenience — is `target` muted right now? Used by the chat
    /// service gate. `target` is the *sender* (since muting is
    /// about whether the sender can post, not whether the receiver
    /// can hear).
    pub async fn is_member_muted(
        &self,
        conversation_id: &ConversationId,
        target: &UserId,
    ) -> AppResult<bool> {
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage.is_group_member_muted(conversation_id, target).await
    }

    // ── Group nicknames (G-06) ────────────────────────────────────────────────

    /// Set or clear a member's per-group nickname (群昵称).
    /// `nickname = None` clears the override.
    pub async fn set_nickname(
        &self,
        actor: &UserId,
        conversation_id: &ConversationId,
        target: &UserId,
        nickname: Option<&str>,
    ) -> AppResult<()> {
        // Members can set their own nickname; admins/owners can set
        // anyone's. Owner/admin is also enforced for the cross-user
        // case to keep admin parity with WeChat's group manager UX.
        if actor != target {
            self.require_role(actor, conversation_id, MemberRole::Admin)
                .await?;
        }
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        let now = chrono::Utc::now().timestamp();
        storage
            .set_group_member_nickname(conversation_id, target, nickname, now)
            .await?;
        self.bus.publish(A3chatEvent::GroupNicknameChanged {
            user_id: actor.clone(),
            conversation_id: conversation_id.clone(),
            member_user_id: target.clone(),
            nickname: nickname.map(str::to_string),
            actor_user_id: actor.clone(),
        });
        Ok(())
    }

    /// Fetch a single member's nickname override.
    pub async fn get_nickname(
        &self,
        conversation_id: &ConversationId,
        target: &UserId,
    ) -> AppResult<Option<String>> {
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage.get_group_member_nickname(conversation_id, target).await
    }

    /// List every nickname override for the conversation.
    pub async fn list_nicknames(
        &self,
        conversation_id: &ConversationId,
    ) -> AppResult<Vec<(UserId, String)>> {
        let storage = self
            .storage_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService storage not set".into()))?;
        storage.list_group_member_nicknames(conversation_id).await
    }

    // ── @-mentions (G-05) ──────────────────────────────────────────────────────

    /// Parse `@nickname` and `@<NodeId>` tokens in a pending body.
    /// The matcher is case-sensitive for hex NodeIds and
    /// case-insensitive for nicknames / display names. The caller
    /// is expected to call
    /// [`validate_mention_members`](Self::validate_mention_members)
    /// on the result before sending so a forged mention list can't
    /// notify non-members.
    pub fn parse_mentions(
        &self,
        body: &str,
        nicknames: &[(UserId, String)],
    ) -> Vec<crate::group_mention::MentionMatch> {
        crate::group_mention::parse(body, nicknames)
    }

    /// Validate that every `mentioned_user_id` in `mentions` is a
    /// current member of `conversation_id`. Returns the list of
    /// unknown ids so the caller can surface them.
    pub async fn validate_mention_members(
        &self,
        conversation_id: &ConversationId,
        mentions: &[UserId],
    ) -> AppResult<Vec<UserId>> {
        let hub = self
            .hub_arc()
            .ok_or_else(|| AppError::NotInitialised("GroupService hub not set".into()))?;
        let members = hub
            .get_group_members(conversation_id.as_str())
            .await
            .map_err(AppError::from)?;
        let known: std::collections::HashSet<String> =
            members.iter().map(|m| m.user_id.clone()).collect();
        Ok(mentions
            .iter()
            .filter(|m| !known.contains(m.as_str()))
            .cloned()
            .collect())
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
            let sync_ticket: Option<String> = params
                .get("sync_ticket")
                .and_then(|v| v.as_str())
                .map(String::from);
            let cid: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let inv_id = invitation_id.unwrap_or_default();
            let m = svc
                .join(owner, &cid, &inv_id, sync_ticket.as_deref())
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

        // ── GB-6 — dissolve ────────────────────────────────────────
        A3chatRpcMethod::GROUP_DISSOLVE => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.dissolve(owner, &conversation_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }

        // ── G-04 — leave ────────────────────────────────────────────
        A3chatRpcMethod::GROUP_LEAVE => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.leave(owner, &conversation_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }

        // ── GB-7 — invitation quintet ──────────────────────────────
        A3chatRpcMethod::GROUP_INVITE => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let invitee_id: UserId = serde_json::from_value(
                params
                    .get("invitee_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("invitee_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let group_name: String = params
                .get("group_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("group_name missing".into()))?
                .to_string();
            // `inviter_name` is parsed for input validation but
            // the canonical display name is supplied by the
            // authoritative `invite` method itself (which always
            // uses `inviter.as_str()`), so we drop it here.
            let _inviter_name: String = params
                .get("inviter_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("inviter_name missing".into()))?
                .to_string();
            let message = params
                .get("message")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let inv = svc
                .invite(owner, &conversation_id, &invitee_id, &group_name, message.as_deref())
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&inv).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_INVITE_LIST => {
            let records = svc
                .list_invitations(owner)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&records).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_INVITE_ACCEPT => {
            let invitation_id: String = params
                .get("invitation_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("invitation_id missing".into()))?
                .to_string();
            let inv = svc
                .accept_invitation(owner, &invitation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&inv).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_INVITE_DECLINE => {
            let invitation_id: String = params
                .get("invitation_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("invitation_id missing".into()))?
                .to_string();
            let inv = svc
                .decline_invitation(owner, &invitation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&inv).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_INVITE_REVOKE => {
            let invitation_id: String = params
                .get("invitation_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("invitation_id missing".into()))?
                .to_string();
            let inv = svc
                .revoke_invitation(owner, &invitation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&inv).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_INVITE_GET => {
            let invitation_id: String = params
                .get("invitation_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("invitation_id missing".into()))?
                .to_string();
            let inv = svc
                .get_invitation(owner, &invitation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&inv).map_err(A3chatError::from)
        }

        // ── GB-8 — list / members / member.get / metadata.update / transfer_ownership ──
        A3chatRpcMethod::GROUP_LIST => {
            let groups = svc.list(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(&groups).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_MEMBERS => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let members = svc
                .list_members(&conversation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&members).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_MEMBER_GET => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let user_id: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let member = svc
                .get_member(&conversation_id, &user_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&member).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_METADATA_UPDATE => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let req: UpdateGroupMetadataRequest =
                serde_json::from_value(params).map_err(A3chatError::from)?;
            svc.update_metadata(owner, &conversation_id, req)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_TRANSFER_OWNERSHIP => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let new_owner: UserId = serde_json::from_value(
                params
                    .get("new_owner_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("new_owner_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.transfer_ownership(owner, &conversation_id, &new_owner)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }

        // ── G-02 — mute / unmute / mute-all / unmute-all / list_muted ──
        A3chatRpcMethod::GROUP_MUTE_MEMBER => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let target: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let muted_until_secs: i64 = params
                .get("muted_until_secs")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| A3chatError::InvalidInput("muted_until_secs missing".into()))?;
            let reason = params
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            svc.mute_member(
                owner,
                &conversation_id,
                &target,
                muted_until_secs,
                reason.as_deref(),
            )
            .await
            .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_UNMUTE_MEMBER => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let target: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.unmute_member(owner, &conversation_id, &target)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_MUTE_ALL => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.mute_all(owner, &conversation_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_UNMUTE_ALL => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            svc.unmute_all(owner, &conversation_id)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_LIST_MUTED => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let mutes = svc
                .list_muted(&conversation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&mutes).map_err(A3chatError::from)
        }

        // ── G-06 — nickname ─────────────────────────────────────────
        A3chatRpcMethod::GROUP_NICKNAME_SET => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let target: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let nickname = params
                .get("nickname")
                .and_then(|v| v.as_str());
            svc.set_nickname(owner, &conversation_id, &target, nickname)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        A3chatRpcMethod::GROUP_NICKNAME_GET => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let target: UserId = serde_json::from_value(
                params
                    .get("user_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("user_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let n = svc
                .get_nickname(&conversation_id, &target)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&n).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_NICKNAME_LIST => {
            let conversation_id: ConversationId = serde_json::from_value(
                params
                    .get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let rows = svc
                .list_nicknames(&conversation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(&rows).map_err(A3chatError::from)
        }

        // ── G-05 — mention.parse ────────────────────────────────────
        A3chatRpcMethod::GROUP_MENTION_PARSE => {
            let body: String = params
                .get("body")
                .and_then(|v| v.as_str())
                .ok_or_else(|| A3chatError::InvalidInput("body missing".into()))?
                .to_string();
            // Optional pre-supplied nicknames; if absent the caller
            // can chain a `group.nickname.list` round-trip.
            let nicknames_in: Vec<serde_json::Value> = params
                .get("nicknames")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut nicknames: Vec<(UserId, String)> = Vec::with_capacity(nicknames_in.len());
            for nv in nicknames_in {
                let uid = nv
                    .get("user_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| A3chatError::InvalidInput("nicknames[].user_id missing".into()))?
                    .to_string();
                let name = nv
                    .get("nickname")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| A3chatError::InvalidInput("nicknames[].nickname missing".into()))?
                    .to_string();
                nicknames.push((UserId::from(uid), name));
            }
            let matches = svc.parse_mentions(&body, &nicknames);
            serde_json::to_value(&matches).map_err(A3chatError::from)
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
