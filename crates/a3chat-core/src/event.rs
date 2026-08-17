//! Server-pushed events — wrapped as JSON-RPC `notifications` over
//! the SSE endpoint `/rpc/stream`.

use serde::{Deserialize, Serialize};

use crate::group::{GroupInvitation, GroupMember};
use crate::id::{ConversationId, MessageId, UserId};
use crate::link_bookmark::LinkBookmark;
use crate::message::ChatMessage;
use crate::presence::{PresenceEvent, PresenceStatus};

/// Notification kind tag — clients subscribe to one or more kinds
/// (see [`A3chatRpcMethod::StreamSubscribe`] for the wire shape).
pub const NOTIFICATION_KIND_CHAT: &str = "chat.message.received";
pub const NOTIFICATION_KIND_PRESENCE: &str = "presence.changed";
pub const NOTIFICATION_KIND_TYPING: &str = "chat.typing";
pub const NOTIFICATION_KIND_CONTACT: &str = "contact.request.received";
pub const NOTIFICATION_KIND_GROUP: &str = "group.member.joined";
pub const NOTIFICATION_KIND_GROUP_MEMBER_REMOVED: &str = "group.member.removed";
pub const NOTIFICATION_KIND_MESSAGE_RECALLED: &str = "chat.message.recalled";
pub const NOTIFICATION_KIND_MESSAGE_READ: &str = "chat.message.read";
pub const NOTIFICATION_KIND_MESSAGE_EDITED: &str = "chat.message.edited";
pub const NOTIFICATION_KIND_MESSAGE_DELETED: &str = "chat.message.deleted";
pub const NOTIFICATION_KIND_GROUP_INVITATION: &str = "group.invitation.received";
pub const NOTIFICATION_KIND_LINK_BOOKMARK_ADDED: &str = "link.bookmark.added";
pub const NOTIFICATION_KIND_LINK_BOOKMARK_UPDATED: &str = "link.bookmark.updated";
pub const NOTIFICATION_KIND_LINK_BOOKMARK_DELETED: &str = "link.bookmark.deleted";

// Moments / 朋友圈 (F-05). The `kind` strings are what SSE subscribers
// match against when deciding to refresh their timeline; keeping them
// stable is a public-API contract.
pub const NOTIFICATION_KIND_MOMENTS_POST_CREATED: &str = "moments.post.created";
pub const NOTIFICATION_KIND_MOMENTS_POST_DELETED: &str = "moments.post.deleted";
pub const NOTIFICATION_KIND_MOMENTS_COMMENT_ADDED: &str = "moments.comment.added";
pub const NOTIFICATION_KIND_MOMENTS_REACTION_TOGGLED: &str = "moments.reaction.toggled";

/// All a3chat server-pushed events. The discriminator is `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum A3chatEvent {
    /// A new chat message landed in the local store. Body is the
    /// full [`ChatMessage`] record (already decrypted if the local
    /// user is the intended recipient; ciphertext otherwise).
    ChatMessageReceived {
        user_id: UserId,
        conversation_id: ConversationId,
        message: ChatMessage,
    },

    /// A contact's presence changed.
    PresenceChanged { event: PresenceEvent },

    /// The remote side is typing in `conversation_id`. Typing
    /// notifications are best-effort and time-bounded.
    ChatTyping {
        user_id: UserId,
        conversation_id: ConversationId,
        /// Unix seconds — clients should stop showing the indicator
        /// if `now > expires_at`.
        expires_at: i64,
    },

    /// A friend request was received.
    ContactRequestReceived { request_id: String },

    /// Group membership changed.
    GroupMemberJoined {
        conversation_id: ConversationId,
        member: GroupMember,
    },

    /// A member was removed from a group (kicked, left, or by
    /// cascade after `dissolve`). `actor_user_id` is the user who
    /// triggered the removal (`None` when no actor — e.g. cascade).
    GroupMemberRemoved {
        conversation_id: ConversationId,
        user_id: UserId,
        actor_user_id: Option<UserId>,
        removed_at_unix: i64,
    },

    /// A previously-sent message was recalled by the sender.
    ChatMessageRecalled {
        user_id: UserId,
        conversation_id: ConversationId,
        message_id: MessageId,
        recalled_at_unix: i64,
    },

    /// A message was marked as read.
    ChatMessageRead {
        user_id: UserId,
        conversation_id: ConversationId,
        message_id: MessageId,
        read_at_unix: i64,
    },

    /// A previously-sent message was edited by the sender.
    ChatMessageEdited {
        user_id: UserId,
        conversation_id: ConversationId,
        message: ChatMessage,
    },

    /// A message was deleted locally ("delete for me"). Note this
    /// is a *local* event — servers do not propagate.
    ChatMessageDeleted {
        user_id: UserId,
        conversation_id: ConversationId,
        message_id: MessageId,
    },

    /// A new group invitation was received.
    GroupInvitationReceived { invitation: GroupInvitation },

    /// A link bookmark was added by the local user (or by SSE push
    /// from another device).
    LinkBookmarkAdded {
        user_id: UserId,
        bookmark: LinkBookmark,
    },

    /// A link bookmark was updated — title / description / tags /
    /// folder / pinned / archived flags.
    LinkBookmarkUpdated {
        user_id: UserId,
        bookmark: LinkBookmark,
    },

    /// A link bookmark was deleted. The full record is *not* sent
    /// over the wire (faster + smaller) but the URL is included so
    /// SSE clients can remove the row from their local cache.
    LinkBookmarkDeleted {
        user_id: UserId,
        bookmark_id: String,
        url: String,
    },

    // ----- Moments / 朋友圈 (F-05) ---------------------------------------
    // All four events carry the `user_id` of the *owner of the local
    // node*, so the SSE bus can fan out to subscribed devices without
    // re-deriving it. `author_id` is the gossip-side author of the
    // underlying record (same as `user_id` for posts created by the
    // local owner; may differ for inbound gossiped records).
    MomentsPostCreated {
        user_id: UserId,
        post_id: String,
        author_id: String,
        /// Visibility string (one of `a3net_types::invariants::Visibility`):
        /// `"public"`, `"friends"`, `"private"`, etc. Carried as a
        /// string so the SSE layer does not need to depend on the
        /// gossip crate's enum.
        visibility: String,
    },

    MomentsPostDeleted {
        user_id: UserId,
        post_id: String,
        author_id: String,
    },

    MomentsCommentAdded {
        user_id: UserId,
        post_id: String,
        comment_id: String,
        author_id: String,
    },

    /// `is_added = false` when an existing reaction is removed (so
    /// the receiving client can decrement its count atomically).
    MomentsReactionToggled {
        user_id: UserId,
        target_id: String,
        actor_id: String,
        reaction_type: String,
        is_added: bool,
    },
}

impl A3chatEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            A3chatEvent::ChatMessageReceived { .. } => NOTIFICATION_KIND_CHAT,
            A3chatEvent::PresenceChanged { .. } => NOTIFICATION_KIND_PRESENCE,
            A3chatEvent::ChatTyping { .. } => NOTIFICATION_KIND_TYPING,
            A3chatEvent::ContactRequestReceived { .. } => NOTIFICATION_KIND_CONTACT,
            A3chatEvent::GroupMemberJoined { .. } => NOTIFICATION_KIND_GROUP,
            A3chatEvent::GroupMemberRemoved { .. } => NOTIFICATION_KIND_GROUP_MEMBER_REMOVED,
            A3chatEvent::ChatMessageRecalled { .. } => NOTIFICATION_KIND_MESSAGE_RECALLED,
            A3chatEvent::ChatMessageRead { .. } => NOTIFICATION_KIND_MESSAGE_READ,
            A3chatEvent::ChatMessageEdited { .. } => NOTIFICATION_KIND_MESSAGE_EDITED,
            A3chatEvent::ChatMessageDeleted { .. } => NOTIFICATION_KIND_MESSAGE_DELETED,
            A3chatEvent::GroupInvitationReceived { .. } => NOTIFICATION_KIND_GROUP_INVITATION,
            A3chatEvent::LinkBookmarkAdded { .. } => NOTIFICATION_KIND_LINK_BOOKMARK_ADDED,
            A3chatEvent::LinkBookmarkUpdated { .. } => NOTIFICATION_KIND_LINK_BOOKMARK_UPDATED,
            A3chatEvent::LinkBookmarkDeleted { .. } => NOTIFICATION_KIND_LINK_BOOKMARK_DELETED,
            A3chatEvent::MomentsPostCreated { .. } => NOTIFICATION_KIND_MOMENTS_POST_CREATED,
            A3chatEvent::MomentsPostDeleted { .. } => NOTIFICATION_KIND_MOMENTS_POST_DELETED,
            A3chatEvent::MomentsCommentAdded { .. } => NOTIFICATION_KIND_MOMENTS_COMMENT_ADDED,
            A3chatEvent::MomentsReactionToggled { .. } => NOTIFICATION_KIND_MOMENTS_REACTION_TOGGLED,
        }
    }

    pub fn is_presence(&self) -> bool {
        matches!(self, A3chatEvent::PresenceChanged { .. })
    }
}

/// The wire wrapper sent over SSE. The `kind` field is the JSON-RPC
/// notification method name; `payload` is the serialised event body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct A3chatNotification {
    pub kind: String,
    pub payload: serde_json::Value,
    /// RFC3339 — when the event was emitted by the daemon.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl A3chatNotification {
    pub fn from_event(event: A3chatEvent) -> Result<Self, serde_json::Error> {
        let kind = event.kind().to_string();
        let payload = serde_json::to_value(&event)?;
        Ok(Self {
            kind,
            payload,
            timestamp: chrono::Utc::now(),
        })
    }

    pub fn decode(&self) -> Result<A3chatEvent, serde_json::Error> {
        serde_json::from_value(self.payload.clone())
    }
}

/// Helper: extract the presence status if this notification is a
/// presence update.
pub fn presence_status(notif: &A3chatNotification) -> Option<PresenceStatus> {
    if notif.kind == NOTIFICATION_KIND_PRESENCE {
        notif.decode().ok().and_then(|e| {
            if let A3chatEvent::PresenceChanged { event } = e {
                Some(event.status)
            } else {
                None
            }
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{GroupInvitation, InvitationStatus};
    use crate::id::ConversationId;

    #[test]
    fn kind_matches_variant() {
        let e = A3chatEvent::ChatTyping {
            user_id: UserId::from("alice"),
            conversation_id: ConversationId::from("dm:a:b"),
            expires_at: 0,
        };
        assert_eq!(e.kind(), NOTIFICATION_KIND_TYPING);
        assert!(!e.is_presence());
    }

    #[test]
    fn round_trip_through_notification() {
        let e = A3chatEvent::PresenceChanged {
            event: PresenceEvent {
                user_id: UserId::from("alice"),
                status: PresenceStatus::Away,
                status_message: Some("brb".into()),
                timestamp: chrono::Utc::now(),
            },
        };
        let n = A3chatNotification::from_event(e.clone()).unwrap();
        assert_eq!(n.kind, NOTIFICATION_KIND_PRESENCE);
        let decoded = n.decode().unwrap();
        assert_eq!(decoded, e);
        assert!(matches!(presence_status(&n), Some(PresenceStatus::Away)));
    }

    #[test]
    fn presence_status_returns_none_for_other_kinds() {
        let e = A3chatEvent::ChatTyping {
            user_id: UserId::from("alice"),
            conversation_id: ConversationId::from("dm:a:b"),
            expires_at: 0,
        };
        let n = A3chatNotification::from_event(e).unwrap();
        assert!(presence_status(&n).is_none());
    }

    #[test]
    fn group_invitation_kind_constant_matches_event() {
        // The constant must equal the wire string and the value
        // emitted by `A3chatEvent::kind()` so the front-end and
        // the daemon agree on the notification kind.
        let e = A3chatEvent::GroupInvitationReceived {
            invitation: GroupInvitation {
                invitation_id: "inv-1".into(),
                conversation_id: ConversationId::from("grp:x"),
                group_name: "team".into(),
                inviter_id: UserId::from("alice"),
                inviter_name: "Alice".into(),
                invitee_id: UserId::from("bob"),
                status: InvitationStatus::Pending,
                created_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::days(7),
            },
        };
        assert_eq!(e.kind(), NOTIFICATION_KIND_GROUP_INVITATION);
        assert_eq!(NOTIFICATION_KIND_GROUP_INVITATION, "group.invitation.received");
    }
}
