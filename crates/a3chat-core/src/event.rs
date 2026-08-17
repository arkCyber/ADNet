//! Server-pushed events — wrapped as JSON-RPC `notifications` over
//! the SSE endpoint `/rpc/stream`.

use serde::{Deserialize, Serialize};

use crate::group::{GroupInvitation, GroupMember};
use crate::id::{ConversationId, MessageId, UserId};
use crate::link_bookmark::LinkBookmark;
use crate::message::ChatMessage;
use crate::notification_settings::DndSettings;
use crate::presence::{PresenceEvent, PresenceStatus};

/// Notification kind tag — clients subscribe to one or more kinds
/// (see [`A3chatRpcMethod::StreamSubscribe`] for the wire shape).
pub const NOTIFICATION_KIND_CHAT: &str = "chat.message.received";
pub const NOTIFICATION_KIND_PRESENCE: &str = "presence.changed";
pub const NOTIFICATION_KIND_TYPING: &str = "chat.typing";
pub const NOTIFICATION_KIND_CONTACT: &str = "contact.request.received";
pub const NOTIFICATION_KIND_CONTACT_ADDED: &str = "contact.added";
pub const NOTIFICATION_KIND_CONTACT_REMOVED: &str = "contact.removed";
pub const NOTIFICATION_KIND_CONTACT_UPDATED: &str = "contact.updated";
pub const NOTIFICATION_KIND_CONTACT_BLOCKED: &str = "contact.blocked";
pub const NOTIFICATION_KIND_CONTACT_UNBLOCKED: &str = "contact.unblocked";
pub const NOTIFICATION_KIND_CONTACT_FAVORITE_TOGGLED: &str = "contact.favorite.toggled";
pub const NOTIFICATION_KIND_CONTACT_REQUEST_ACCEPTED: &str = "contact.request.accepted";
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

// Forward / Pin / Notification / Device events
pub const NOTIFICATION_KIND_CONVERSATION_PIN_CHANGED: &str = "conversation.pin.changed";
pub const NOTIFICATION_KIND_NOTIFICATION_SETTINGS_CHANGED: &str = "notification.settings.changed";
pub const NOTIFICATION_KIND_MESSAGE_FORWARDED: &str = "chat.message.forwarded";
pub const NOTIFICATION_KIND_DEVICE_REGISTERED: &str = "device.registered";
pub const NOTIFICATION_KIND_DEVICE_REVOKED: &str = "device.revoked";
pub const NOTIFICATION_KIND_DEVICE_PRIMARY_CHANGED: &str = "device.primary.changed";
pub const NOTIFICATION_KIND_GROUP_ANNOUNCEMENT_CHANGED: &str = "group.announcement.changed";
pub const NOTIFICATION_KIND_GROUP_DISSOLVED: &str = "group.dissolved";
pub const NOTIFICATION_KIND_GROUP_ROLE_CHANGED: &str = "group.member.role.changed";
pub const NOTIFICATION_KIND_GROUP_MUTE_CHANGED: &str = "group.mute.changed";
pub const NOTIFICATION_KIND_GROUP_NICKNAME_CHANGED: &str = "group.nickname.changed";

// Pairing (P2P device linking)
pub const NOTIFICATION_KIND_PAIRING_INVITATION_CREATED: &str = "pairing.invitation.created";
pub const NOTIFICATION_KIND_PAIRING_TRUSTED_ADDED: &str = "pairing.trusted.added";
pub const NOTIFICATION_KIND_PAIRING_TRUSTED_REVOKED: &str = "pairing.trusted.revoked";

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

    /// A contact was added to the roster.
    ContactAdded { contact_id: String },

    /// A contact was removed from the roster.
    ContactRemoved { contact_id: String },

    /// A contact was updated (name, notes, tags, etc.).
    ContactUpdated { contact_id: String },

    /// A contact was blocked.
    ContactBlocked { user_id: UserId },

    /// A contact was unblocked.
    ContactUnblocked { user_id: UserId },

    /// A contact's favorite status was toggled.
    ContactFavoriteToggled { contact_id: String, is_favorite: bool },

    /// A friend request was accepted and a contact was created.
    ContactRequestAccepted { request_id: String, contact_id: String },

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

    /// A reaction was added or removed from a chat message.
    ChatMessageReactionToggled {
        user_id: UserId,
        conversation_id: ConversationId,
        message_id: MessageId,
        reactor_id: UserId,
        reaction_type: String,
        is_added: bool,
    },

    /// A conversation's pinned state changed.
    ConversationPinChanged {
        user_id: UserId,
        conversation_id: ConversationId,
        pinned: bool,
    },

    /// Notification settings changed (global DND or per-conversation).
    NotificationSettingsChanged {
        user_id: UserId,
        conversation_id: Option<ConversationId>,
        global_dnd: Option<DndSettings>,
    },

    /// A new device was registered.
    DeviceRegistered {
        user_id: UserId,
        device_id: String,
    },

    /// A device was revoked.
    DeviceRevoked {
        user_id: UserId,
        device_id: String,
    },

    /// A device was promoted to or demoted from primary.
    DevicePrimaryChanged {
        user_id: UserId,
        device_id: String,
    },

    /// A group's pinned announcement was created, updated, or cleared.
    /// Emitted by the group service so SSE subscribers refresh the
    /// banner.
    GroupAnnouncementChanged {
        user_id: UserId,
        conversation_id: ConversationId,
        /// `None` clears the announcement (an empty-string write is
        /// the canonical "clear" signal).
        text: Option<String>,
        actor_user_id: UserId,
    },

    /// A group was permanently dissolved by its owner. Members
    /// listening on SSE should drop the conversation from their UI.
    GroupDissolved {
        user_id: UserId,
        conversation_id: ConversationId,
        actor_user_id: UserId,
        dissolved_at_unix: i64,
    },

    /// A member's role within a group was changed (e.g. promoted
    /// to admin). The role string mirrors the canonical
    /// [`crate::group::MemberRole::as_str`] values.
    GroupMemberRoleChanged {
        user_id: UserId,
        conversation_id: ConversationId,
        member_user_id: UserId,
        new_role: String,
        actor_user_id: UserId,
    },

    /// Per-member mute state changed inside a group. `is_muted =
    /// false` means the mute was lifted.
    GroupMuteChanged {
        user_id: UserId,
        conversation_id: ConversationId,
        muted_user_id: UserId,
        is_muted: bool,
        /// Unix seconds at which the mute auto-lifts. `None` means
        /// "indefinite / until manually cleared".
        muted_until_unix: Option<i64>,
        actor_user_id: UserId,
    },

    /// The whole group was muted (`is_muted = true`) or un-muted.
    GroupMuteAllChanged {
        user_id: UserId,
        conversation_id: ConversationId,
        is_muted: bool,
        actor_user_id: UserId,
    },

    /// A member's per-group nickname was set or updated. `None`
    /// clears the override.
    GroupNicknameChanged {
        user_id: UserId,
        conversation_id: ConversationId,
        member_user_id: UserId,
        nickname: Option<String>,
        actor_user_id: UserId,
    },

    /// A pairing invitation was created locally and signed by the
    /// issuer's wallet. SSE subscribers should surface it to the
    /// user (e.g. show a QR code) so the invitee can scan it.
    PairingInvitationCreated {
        user_id: UserId,
        issuer_node_id: String,
        expires_at_unix: i64,
    },

    /// A trusted-device record was added to the local store. This
    /// fires on both `issuer` and `invitee` sides of a pairing so
    /// that UIs can refresh their device list without polling.
    PairingTrustedDeviceAdded {
        user_id: UserId,
        credential_id: String,
        /// Either `"issuer"` or `"invitee"`.
        role: String,
        device_name: String,
    },

    /// A trusted-device record was revoked. SSE subscribers should
    /// remove the device from any UI lists and immediately drop any
    /// cached Noise sessions associated with the revoked
    /// credential_id (the next handshake will fail anyway, but a
    /// clean drop avoids confusing "I sent a message that bounced"
    /// behaviour in the UI).
    PairingTrustedDeviceRevoked {
        user_id: UserId,
        credential_id: String,
    },
}

impl A3chatEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            A3chatEvent::ChatMessageReceived { .. } => NOTIFICATION_KIND_CHAT,
            A3chatEvent::PresenceChanged { .. } => NOTIFICATION_KIND_PRESENCE,
            A3chatEvent::ChatTyping { .. } => NOTIFICATION_KIND_TYPING,
            A3chatEvent::ContactRequestReceived { .. } => NOTIFICATION_KIND_CONTACT,
            A3chatEvent::ContactAdded { .. } => NOTIFICATION_KIND_CONTACT_ADDED,
            A3chatEvent::ContactRemoved { .. } => NOTIFICATION_KIND_CONTACT_REMOVED,
            A3chatEvent::ContactUpdated { .. } => NOTIFICATION_KIND_CONTACT_UPDATED,
            A3chatEvent::ContactBlocked { .. } => NOTIFICATION_KIND_CONTACT_BLOCKED,
            A3chatEvent::ContactUnblocked { .. } => NOTIFICATION_KIND_CONTACT_UNBLOCKED,
            A3chatEvent::ContactFavoriteToggled { .. } => NOTIFICATION_KIND_CONTACT_FAVORITE_TOGGLED,
            A3chatEvent::ContactRequestAccepted { .. } => NOTIFICATION_KIND_CONTACT_REQUEST_ACCEPTED,
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
            A3chatEvent::ChatMessageReactionToggled { .. } => "chat.message.reaction.toggled",
            A3chatEvent::ConversationPinChanged { .. } => NOTIFICATION_KIND_CONVERSATION_PIN_CHANGED,
            A3chatEvent::NotificationSettingsChanged { .. } => NOTIFICATION_KIND_NOTIFICATION_SETTINGS_CHANGED,
            A3chatEvent::DeviceRegistered { .. } => NOTIFICATION_KIND_DEVICE_REGISTERED,
            A3chatEvent::DeviceRevoked { .. } => NOTIFICATION_KIND_DEVICE_REVOKED,
            A3chatEvent::DevicePrimaryChanged { .. } => NOTIFICATION_KIND_DEVICE_PRIMARY_CHANGED,
            A3chatEvent::GroupAnnouncementChanged { .. } => NOTIFICATION_KIND_GROUP_ANNOUNCEMENT_CHANGED,
            A3chatEvent::GroupDissolved { .. } => NOTIFICATION_KIND_GROUP_DISSOLVED,
            A3chatEvent::GroupMemberRoleChanged { .. } => NOTIFICATION_KIND_GROUP_ROLE_CHANGED,
            A3chatEvent::GroupMuteChanged { .. } => NOTIFICATION_KIND_GROUP_MUTE_CHANGED,
            A3chatEvent::GroupMuteAllChanged { .. } => NOTIFICATION_KIND_GROUP_MUTE_CHANGED,
            A3chatEvent::GroupNicknameChanged { .. } => NOTIFICATION_KIND_GROUP_NICKNAME_CHANGED,
            A3chatEvent::PairingInvitationCreated { .. } => NOTIFICATION_KIND_PAIRING_INVITATION_CREATED,
            A3chatEvent::PairingTrustedDeviceAdded { .. } => NOTIFICATION_KIND_PAIRING_TRUSTED_ADDED,
            A3chatEvent::PairingTrustedDeviceRevoked { .. } => NOTIFICATION_KIND_PAIRING_TRUSTED_REVOKED,
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
