//! Server-pushed events — wrapped as JSON-RPC `notifications` over
//! the SSE endpoint `/rpc/stream`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::channel::{FeedItem, PublicAccount};
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
pub const NOTIFICATION_KIND_CONTACT_REQUEST_CANCELLED: &str = "contact.request.cancelled";
pub const NOTIFICATION_KIND_GROUP: &str = "group.member.joined";
pub const NOTIFICATION_KIND_GROUP_MEMBER_REMOVED: &str = "group.member.removed";
pub const NOTIFICATION_KIND_MESSAGE_RECALLED: &str = "chat.message.recalled";
pub const NOTIFICATION_KIND_MESSAGE_READ: &str = "chat.message.read";
pub const NOTIFICATION_KIND_MESSAGE_EDITED: &str = "chat.message.edited";
pub const NOTIFICATION_KIND_MESSAGE_DELETED: &str = "chat.message.deleted";
pub const NOTIFICATION_KIND_CHAT_TAP: &str = "chat.tap";
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
pub const NOTIFICATION_KIND_GROUP_MEMBER_PRESENCE: &str = "group.member.presence";
pub const NOTIFICATION_KIND_GROUP_TEMP_ADMIN_GRANTED: &str = "group.temp_admin.granted";
pub const NOTIFICATION_KIND_GROUP_TEMP_ADMIN_REVOKED: &str = "group.temp_admin.revoked";

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
pub const NOTIFICATION_KIND_MOMENTS_COMMENT_EDITED: &str = "moments.comment.edited";
pub const NOTIFICATION_KIND_MOMENTS_COMMENT_DELETED: &str = "moments.comment.deleted";
/// MN-07 — `@`-mention fan-out. Wire string is the same shape as
/// `chat.message.mention` so a single UI banner handler covers both.
pub const NOTIFICATION_KIND_MOMENTS_COMMENT_MENTION: &str = "moments.comment.mention";
pub const NOTIFICATION_KIND_MOMENTS_REACTION_TOGGLED: &str = "moments.reaction.toggled";
pub const NOTIFICATION_KIND_MOMENTS_POST_SHARED: &str = "moments.post.shared";
pub const NOTIFICATION_KIND_MOMENTS_POST_REPORTED: &str = "moments.post.reported";
pub const NOTIFICATION_KIND_MOMENTS_USER_BLOCKED: &str = "moments.user.blocked";

// Channel / 公众号 (F-09). The kind strings are stable wire
// contracts — SSE subscribers match against them to refresh their
// timelines / subscription list.
pub const NOTIFICATION_KIND_CHANNEL_ACCOUNT_REGISTERED: &str = "channel.account.registered";
pub const NOTIFICATION_KIND_CHANNEL_ACCOUNT_UPDATED: &str = "channel.account.updated";
pub const NOTIFICATION_KIND_CHANNEL_ACCOUNT_DELETED: &str = "channel.account.deleted";
pub const NOTIFICATION_KIND_CHANNEL_SUBSCRIBED: &str = "channel.subscribed";
pub const NOTIFICATION_KIND_CHANNEL_UNSUBSCRIBED: &str = "channel.unsubscribed";
pub const NOTIFICATION_KIND_CHANNEL_FEED_PUBLISHED: &str = "channel.feed.published";
pub const NOTIFICATION_KIND_CHANNEL_FEED_RETRACTED: &str = "channel.feed.retracted";

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

    /// A friend request was cancelled (by the sender) or rejected
    /// (by the addressee) before reaching an accepted state. Emitted
    /// alongside the lifecycle update on the roster store so SSE
    /// subscribers can drop any pending inbox rows.
    ContactRequestCancelled { request_id: String, by_user_id: UserId },

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

    /// F-14 — "拍一拍" tap-to-nudge. The actor lightly pokes the
    /// chat (optionally targeting a specific user). Does NOT
    /// produce a persisted message; the receiving UI is expected
    /// to render an animation on the latest bubble.
    ChatTap {
        user_id: UserId,
        conversation_id: ConversationId,
        /// `None` means the tap is directed at the whole conversation.
        target_user_id: Option<UserId>,
        actor_user_id: UserId,
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

    /// A comment was posted that mentions a user (MN-07 — `@`-mention
    /// fan-out). The receiving client checks its own
    /// `notification_settings::MentionsOnly` flag to decide whether
    /// to render a banner. The `user_id` field is the **mentioned**
    /// user (not the commenter), so subscribers filtered on `user_id`
    /// get one event per `@`.
    MomentsCommentMention {
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

    /// A comment was edited (post_id + comment_id reference the
    /// underlying post, `author_id` is the editing user).
    MomentsCommentEdited {
        user_id: UserId,
        post_id: String,
        comment_id: String,
        author_id: String,
    },

    /// A comment was deleted. We do not carry the post_id because
    /// subscribers only need the comment_id to remove their row.
    MomentsCommentDeleted {
        user_id: UserId,
        comment_id: String,
    },

    /// A user re-shared a post (or a comment). `target_type` is
    /// `"post"` or `"comment"`.
    MomentsPostShared {
        user_id: UserId,
        target_id: String,
        target_type: String,
        sharer_id: String,
    },

    /// A post (or comment) was reported for moderation. `reason`
    /// is one of [`crate::social_feed::ReportReason::as_str()`].
    MomentsPostReported {
        user_id: UserId,
        target_id: String,
        target_type: String,
        reason: String,
    },

    /// A user was added to the owner's blocklist.
    MomentsUserBlocked {
        user_id: UserId,
        blocked_user_id: String,
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

    /// A member's presence status changed (online/offline/last_seen updated).
    /// Published when a member sends a message or updates their presence.
    GroupMemberPresenceChanged {
        user_id: UserId,
        conversation_id: ConversationId,
        target_user_id: UserId,
        is_online: bool,
        last_seen: Option<DateTime<Utc>>,
    },

    /// Temporary admin privileges were granted to a member.
    GroupTempAdminGranted {
        user_id: UserId,
        conversation_id: ConversationId,
        target_user_id: UserId,
        granted_by: UserId,
        expires_at: DateTime<Utc>,
    },

    /// Temporary admin privileges were revoked from a member.
    GroupTempAdminRevoked {
        user_id: UserId,
        conversation_id: ConversationId,
        target_user_id: UserId,
        revoked_by: UserId,
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

    // ----- Channel / 公众号 (F-09) --------------------------------------
    // Lifecycle events for accounts the local node owns (`*owner*_*`)
    // and events fan-out to subscribers (`*Subscriber*`). All
    // events carry `user_id` of the local node so the SSE bus can
    // route per-user without re-deriving.
    ChannelAccountRegistered {
        user_id: UserId,
        account: PublicAccount,
    },

    ChannelAccountUpdated {
        user_id: UserId,
        account: PublicAccount,
    },

    ChannelAccountDeleted {
        user_id: UserId,
        account_id: String,
    },

    ChannelSubscribed {
        user_id: UserId,
        account_id: String,
    },

    ChannelUnsubscribed {
        user_id: UserId,
        account_id: String,
    },

    /// A feed item was published by an account the local user
    /// subscribes to. Carries the full record so subscribers can
    /// render the timeline without a follow-up `feed.get`.
    ChannelFeedPublished {
        user_id: UserId,
        account_id: String,
        feed: FeedItem,
    },

    /// An admin retracted a feed item. Subscribers should hide it
    /// from the timeline but keep the row in the local store so a
    /// future "show retracted" toggle works.
    ChannelFeedRetracted {
        user_id: UserId,
        account_id: String,
        feed_id: String,
        reason: String,
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
            A3chatEvent::ContactRequestCancelled { .. } => NOTIFICATION_KIND_CONTACT_REQUEST_CANCELLED,
            A3chatEvent::GroupMemberJoined { .. } => NOTIFICATION_KIND_GROUP,
            A3chatEvent::GroupMemberRemoved { .. } => NOTIFICATION_KIND_GROUP_MEMBER_REMOVED,
            A3chatEvent::ChatMessageRecalled { .. } => NOTIFICATION_KIND_MESSAGE_RECALLED,
            A3chatEvent::ChatMessageRead { .. } => NOTIFICATION_KIND_MESSAGE_READ,
            A3chatEvent::ChatMessageEdited { .. } => NOTIFICATION_KIND_MESSAGE_EDITED,
            A3chatEvent::ChatMessageDeleted { .. } => NOTIFICATION_KIND_MESSAGE_DELETED,
            A3chatEvent::ChatTap { .. } => NOTIFICATION_KIND_CHAT_TAP,
            A3chatEvent::GroupInvitationReceived { .. } => NOTIFICATION_KIND_GROUP_INVITATION,
            A3chatEvent::LinkBookmarkAdded { .. } => NOTIFICATION_KIND_LINK_BOOKMARK_ADDED,
            A3chatEvent::LinkBookmarkUpdated { .. } => NOTIFICATION_KIND_LINK_BOOKMARK_UPDATED,
            A3chatEvent::LinkBookmarkDeleted { .. } => NOTIFICATION_KIND_LINK_BOOKMARK_DELETED,
            A3chatEvent::MomentsPostCreated { .. } => NOTIFICATION_KIND_MOMENTS_POST_CREATED,
            A3chatEvent::MomentsPostDeleted { .. } => NOTIFICATION_KIND_MOMENTS_POST_DELETED,
            A3chatEvent::MomentsCommentAdded { .. } => NOTIFICATION_KIND_MOMENTS_COMMENT_ADDED,
            A3chatEvent::MomentsCommentMention { .. } => NOTIFICATION_KIND_MOMENTS_COMMENT_MENTION,
            A3chatEvent::MomentsReactionToggled { .. } => NOTIFICATION_KIND_MOMENTS_REACTION_TOGGLED,
            A3chatEvent::MomentsCommentEdited { .. } => NOTIFICATION_KIND_MOMENTS_COMMENT_EDITED,
            A3chatEvent::MomentsCommentDeleted { .. } => NOTIFICATION_KIND_MOMENTS_COMMENT_DELETED,
            A3chatEvent::MomentsPostShared { .. } => NOTIFICATION_KIND_MOMENTS_POST_SHARED,
            A3chatEvent::MomentsPostReported { .. } => NOTIFICATION_KIND_MOMENTS_POST_REPORTED,
            A3chatEvent::MomentsUserBlocked { .. } => NOTIFICATION_KIND_MOMENTS_USER_BLOCKED,
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
            A3chatEvent::GroupMemberPresenceChanged { .. } => NOTIFICATION_KIND_GROUP_MEMBER_PRESENCE,
            A3chatEvent::GroupTempAdminGranted { .. } => NOTIFICATION_KIND_GROUP_TEMP_ADMIN_GRANTED,
            A3chatEvent::GroupTempAdminRevoked { .. } => NOTIFICATION_KIND_GROUP_TEMP_ADMIN_REVOKED,
            A3chatEvent::PairingInvitationCreated { .. } => NOTIFICATION_KIND_PAIRING_INVITATION_CREATED,
            A3chatEvent::PairingTrustedDeviceAdded { .. } => NOTIFICATION_KIND_PAIRING_TRUSTED_ADDED,
            A3chatEvent::PairingTrustedDeviceRevoked { .. } => NOTIFICATION_KIND_PAIRING_TRUSTED_REVOKED,
            A3chatEvent::ChannelAccountRegistered { .. } => {
                NOTIFICATION_KIND_CHANNEL_ACCOUNT_REGISTERED
            }
            A3chatEvent::ChannelAccountUpdated { .. } => {
                NOTIFICATION_KIND_CHANNEL_ACCOUNT_UPDATED
            }
            A3chatEvent::ChannelAccountDeleted { .. } => {
                NOTIFICATION_KIND_CHANNEL_ACCOUNT_DELETED
            }
            A3chatEvent::ChannelSubscribed { .. } => NOTIFICATION_KIND_CHANNEL_SUBSCRIBED,
            A3chatEvent::ChannelUnsubscribed { .. } => NOTIFICATION_KIND_CHANNEL_UNSUBSCRIBED,
            A3chatEvent::ChannelFeedPublished { .. } => NOTIFICATION_KIND_CHANNEL_FEED_PUBLISHED,
            A3chatEvent::ChannelFeedRetracted { .. } => NOTIFICATION_KIND_CHANNEL_FEED_RETRACTED,
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
