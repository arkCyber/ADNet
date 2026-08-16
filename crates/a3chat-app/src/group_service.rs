//! GroupService — group creation, invitations, membership.

use std::sync::Arc;

use a3chat_core::error::A3chatError;
use a3chat_core::group::{Group, GroupInvitation, GroupMember, InvitationStatus, MemberRole};
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::rpc::A3chatRpcMethod;

use crate::error::{AppError, AppResult};
use crate::notification_bus::NotificationBus;

#[derive(Clone)]
pub struct GroupService {
    bus: NotificationBus,
}

impl GroupService {
    pub fn new(bus: NotificationBus) -> Self {
        Self { bus }
    }

    pub fn bus(&self) -> &NotificationBus {
        &self.bus
    }

    /// `a3chat.group.create` — owner creates a group conversation.
    pub async fn create(
        &self,
        owner: &UserId,
        name: String,
        description: String,
        is_private: bool,
    ) -> AppResult<Group> {
        if name.is_empty() {
            return Err(AppError::Domain("group name is empty".into()));
        }
        let now = chrono::Utc::now();
        let g = Group {
            conversation_id: a3chat_core::id::generate_group_conversation_id(),
            name,
            description,
            avatar_url: None,
            owner_id: owner.clone(),
            member_count: 1,
            created_at: now,
            last_activity: now,
            last_sequence: 0,
            is_private,
            is_dissolved: false,
        };
        g.validate()?;
        Ok(g)
    }

    /// `a3chat.group.invite` — owner invites a new member.
    pub async fn invite(
        &self,
        owner: &UserId,
        group_name: &str,
        invitee_id: &UserId,
        inviter_name: &str,
    ) -> AppResult<GroupInvitation> {
        let now = chrono::Utc::now();
        let inv = GroupInvitation {
            invitation_id: uuid::Uuid::new_v4().to_string(),
            conversation_id: a3chat_core::id::generate_group_conversation_id(),
            group_name: group_name.into(),
            inviter_id: owner.clone(),
            inviter_name: inviter_name.into(),
            invitee_id: invitee_id.clone(),
            status: InvitationStatus::Pending,
            created_at: now,
            expires_at: now + chrono::Duration::days(7),
        };
        inv.validate()?;
        self.bus
            .publish(a3chat_core::event::A3chatEvent::GroupInvitationReceived {
                invitation: inv.clone(),
            });
        Ok(inv)
    }

    /// `a3chat.group.join` — accept an invitation.
    pub async fn join(&self, owner: &UserId, invitation_id: &str) -> AppResult<GroupMember> {
        let _ = (owner, invitation_id);
        Err(AppError::NotInitialised("GroupService::join"))
    }

    /// `a3chat.group.member.add` — admin adds a member.
    pub async fn add_member(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        member: &UserId,
    ) -> AppResult<GroupMember> {
        let m = GroupMember {
            user_id: member.clone(),
            display_name: member.as_str().into(),
            role: MemberRole::Member,
            joined_at: chrono::Utc::now(),
            last_seen: None,
            is_online: false,
            nickname: None,
        };
        m.validate()?;
        let _ = (owner, conversation_id);
        self.bus
            .publish(a3chat_core::event::A3chatEvent::GroupMemberJoined {
                conversation_id: conversation_id.clone(),
                member: m.clone(),
            });
        Ok(m)
    }

    /// `a3chat.group.member.remove` — admin kicks a member.
    pub async fn remove_member(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        member: &UserId,
    ) -> AppResult<()> {
        let _ = (owner, conversation_id, member);
        Ok(())
    }

    /// `a3chat.group.member.role` — owner promotes/demotes a member.
    pub async fn set_role(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        member: &UserId,
        role: MemberRole,
    ) -> AppResult<GroupMember> {
        if member.as_str().is_empty() {
            return Err(AppError::Domain("member user_id is empty".into()));
        }
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }
        let _ = owner;
        let m = GroupMember {
            user_id: member.clone(),
            display_name: member.as_str().into(),
            role,
            joined_at: chrono::Utc::now(),
            last_seen: None,
            is_online: false,
            nickname: None,
        };
        m.validate()?;
        Ok(m)
    }

    /// `a3chat.group.announcement.set` — owner posts a pinned
    /// announcement.
    pub async fn set_announcement(
        &self,
        owner: &UserId,
        conversation_id: &ConversationId,
        text: String,
    ) -> AppResult<()> {
        if text.trim().is_empty() {
            return Err(AppError::Domain("announcement is empty".into()));
        }
        if text.len() > 1024 {
            return Err(AppError::Domain("announcement exceeds 1024 chars".into()));
        }
        if conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id is empty".into()));
        }
        let _ = (owner, conversation_id, text);
        Ok(())
    }
}

pub async fn dispatch(
    svc: Arc<GroupService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::GROUP_CREATE => {
            let name: String = serde_json::from_value(
                params
                    .get("name")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("name missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let description: String = params
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_private: bool = params
                .get("is_private")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let g = svc
                .create(owner, name, description, is_private)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(g).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_INVITE => {
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
                .unwrap_or("")
                .to_string();
            let inviter_name: String = params
                .get("inviter_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let inv = svc
                .invite(owner, &group_name, &invitee_id, &inviter_name)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(inv).map_err(A3chatError::from)
        }
        A3chatRpcMethod::GROUP_JOIN => {
            let invitation_id: String = serde_json::from_value(
                params
                    .get("invitation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("invitation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;
            let m = svc
                .join(owner, &invitation_id)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(m).map_err(A3chatError::from)
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
            serde_json::to_value(m).map_err(A3chatError::from)
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
            serde_json::to_value(m).map_err(A3chatError::from)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_emits_initial_owner_member() {
        let svc = GroupService::new(NotificationBus::default());
        let g = svc
            .create(&UserId::from("alice"), "team".into(), "eng".into(), true)
            .await
            .unwrap();
        assert_eq!(g.member_count, 1);
        assert_eq!(g.owner_id, UserId::from("alice"));
    }

    #[tokio::test]
    async fn create_rejects_empty_name() {
        let svc = GroupService::new(NotificationBus::default());
        let r = svc
            .create(&UserId::from("alice"), "".into(), "".into(), true)
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn invite_emits_event() {
        let svc = GroupService::new(NotificationBus::default());
        let mut rx = svc.bus().subscribe();
        let inv = svc
            .invite(
                &UserId::from("alice"),
                "team",
                &UserId::from("bob"),
                "Alice",
            )
            .await
            .unwrap();
        assert_eq!(inv.status, InvitationStatus::Pending);
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            a3chat_core::event::A3chatEvent::GroupInvitationReceived { .. }
        ));
    }

    #[tokio::test]
    async fn add_member_emits_event() {
        let svc = GroupService::new(NotificationBus::default());
        let mut rx = svc.bus().subscribe();
        svc.add_member(
            &UserId::from("alice"),
            &ConversationId::from("grp:x"),
            &UserId::from("bob"),
        )
        .await
        .unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("event some");
        assert!(matches!(
            evt,
            a3chat_core::event::A3chatEvent::GroupMemberJoined { .. }
        ));
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

    #[tokio::test]
    async fn dispatch_create() {
        let svc = Arc::new(GroupService::new(NotificationBus::default()));
        let r = dispatch(
            svc,
            A3chatRpcMethod::GROUP_CREATE,
            &UserId::from("alice"),
            serde_json::json!({
                "name": "team",
                "description": "eng",
                "is_private": true
            }),
        )
        .await
        .unwrap();
        assert_eq!(r["name"], "team");
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let svc = Arc::new(GroupService::new(NotificationBus::default()));
        let err = dispatch(
            svc,
            "a3chat.bogus",
            &UserId::from("alice"),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }

    #[tokio::test]
    async fn set_role_returns_validated_member_with_provided_role() {
        let svc = GroupService::new(NotificationBus::default());
        let m = svc
            .set_role(
                &UserId::from("alice"),
                &ConversationId::from("grp:x"),
                &UserId::from("bob"),
                MemberRole::Admin,
            )
            .await
            .unwrap();
        assert_eq!(m.user_id, UserId::from("bob"));
        assert_eq!(m.display_name, "bob");
        assert_eq!(m.role, MemberRole::Admin);
        // Should round-trip cleanly through validate().
        m.validate().expect("validate set_role output");
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
    async fn set_role_rejects_empty_conversation() {
        let svc = GroupService::new(NotificationBus::default());
        let r = svc
            .set_role(
                &UserId::from("alice"),
                &ConversationId::from(""),
                &UserId::from("bob"),
                MemberRole::Admin,
            )
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn announcement_rejects_empty_text() {
        let svc = GroupService::new(NotificationBus::default());
        let r = svc
            .set_announcement(
                &UserId::from("alice"),
                &ConversationId::from("grp:x"),
                "   ".into(),
            )
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn announcement_rejects_empty_conversation() {
        let svc = GroupService::new(NotificationBus::default());
        let r = svc
            .set_announcement(
                &UserId::from("alice"),
                &ConversationId::from(""),
                "hello".into(),
            )
            .await;
        assert!(matches!(r, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn announcement_accepts_normal_text() {
        let svc = GroupService::new(NotificationBus::default());
        let r = svc
            .set_announcement(
                &UserId::from("alice"),
                &ConversationId::from("grp:x"),
                "All hands at 10am".into(),
            )
            .await;
        assert!(r.is_ok());
    }
}
