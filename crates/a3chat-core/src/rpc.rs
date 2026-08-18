//! JSON-RPC method constants and the transport-agnostic
//! [`RpcClient`] trait.
//!
//! The trait is intentionally minimal — it exposes only
//! `call_json(method, params) -> Result<Json>`. Higher layers in
//! `a3chat-rpc` and `a3chat-app` wrap that into typed methods
//! (`chat.send`, `chat.conversation.list`, …).

use serde::{Deserialize, Serialize};

/// Stable list of every JSON-RPC method name served by the a3chat
/// daemon. Kept as `const` so external UIs can programmatically check
/// support without parsing docs.
pub struct A3chatRpcMethod;

impl A3chatRpcMethod {
    // Conversations
    pub const CHAT_CONVERSATION_LIST: &'static str = "a3chat.chat.conversation.list";
    pub const CHAT_CONVERSATION_OPEN: &'static str = "a3chat.chat.conversation.open";
    pub const CHAT_CONVERSATION_CREATE_DIRECT: &'static str =
        "a3chat.chat.conversation.create_direct";
    pub const CHAT_MESSAGE_SEND: &'static str = "a3chat.chat.message.send";
    pub const CHAT_MESSAGE_RECALL: &'static str = "a3chat.chat.message.recall";
    pub const CHAT_MESSAGE_ACK: &'static str = "a3chat.chat.message.ack";
    pub const CHAT_MESSAGE_EDIT: &'static str = "a3chat.chat.message.edit";
    pub const CHAT_MESSAGE_DELETE: &'static str = "a3chat.chat.message.delete";
    pub const CHAT_MESSAGE_FORWARD: &'static str = "a3chat.chat.message.forward";
    pub const CHAT_SEARCH: &'static str = "a3chat.chat.search";
    pub const CHAT_TYPING: &'static str = "a3chat.chat.typing";

    // Conversation pin
    pub const CHAT_CONVERSATION_PIN: &'static str = "a3chat.chat.conversation.pin";
    pub const CHAT_CONVERSATION_UNPIN: &'static str = "a3chat.chat.conversation.unpin";
    pub const CHAT_CONVERSATION_TOGGLE_PIN: &'static str = "a3chat.chat.conversation.toggle_pin";
    pub const CHAT_CONVERSATION_LIST_PINNED: &'static str = "a3chat.chat.conversation.list_pinned";

    // Notification settings / DND
    pub const CHAT_NOTIFICATION_SET_DND: &'static str = "a3chat.chat.notification.set_dnd";
    pub const CHAT_NOTIFICATION_GET_DND: &'static str = "a3chat.chat.notification.get_dnd";
    pub const CHAT_NOTIFICATION_SET_CONVERSATION: &'static str = "a3chat.chat.notification.set_conversation";
    pub const CHAT_NOTIFICATION_GET_CONVERSATION: &'static str = "a3chat.chat.notification.get_conversation";
    pub const CHAT_NOTIFICATION_MUTE: &'static str = "a3chat.chat.notification.mute";
    pub const CHAT_NOTIFICATION_UNMUTE: &'static str = "a3chat.chat.notification.unmute";
    pub const CHAT_NOTIFICATION_LIST_MUTED: &'static str = "a3chat.chat.notification.list_muted";

    // E2E encryption
    pub const E2E_ENCRYPT: &'static str = "a3chat.e2e.encrypt";
    pub const E2E_DECRYPT: &'static str = "a3chat.e2e.decrypt";
    pub const E2E_INITIATE_HANDSHAKE: &'static str = "a3chat.e2e.handshake.initiate";
    pub const E2E_RESPOND_HANDSHAKE: &'static str = "a3chat.e2e.handshake.respond";
    pub const E2E_COMPLETE_HANDSHAKE: &'static str = "a3chat.e2e.handshake.complete";

    // Contacts
    pub const CONTACT_LIST: &'static str = "a3chat.contact.list";
    pub const CONTACT_ADD_REQUEST: &'static str = "a3chat.contact.add_request";
    pub const CONTACT_ACCEPT_REQUEST: &'static str = "a3chat.contact.accept_request";
    pub const CONTACT_BLOCK: &'static str = "a3chat.contact.block";
    pub const CONTACT_UNBLOCK: &'static str = "a3chat.contact.unblock";
    pub const CONTACT_QR_INVITE: &'static str = "a3chat.contact.qr_invite";
    // CRUD on the contact roster. These were already implemented in
    // `ContactService` but never wired through the RPC dispatcher,
    // so CLI / Tauri clients had to talk to them via in-process
    // calls only. Wired here for parity with the rest of the
    // `a3chat.*` surface.
    pub const CONTACT_ADD: &'static str = "a3chat.contact.add";
    pub const CONTACT_REMOVE: &'static str = "a3chat.contact.remove";
    pub const CONTACT_GET: &'static str = "a3chat.contact.get";
    pub const CONTACT_SEARCH: &'static str = "a3chat.contact.search";
    pub const CONTACT_TOGGLE_FAVORITE: &'static str = "a3chat.contact.toggle_favorite";
    pub const CONTACT_UPDATE: &'static str = "a3chat.contact.update";

    // Pairing (P2P device linking — wraps `a3net-pairing`).
    // The service signs invitations with the issuer's wallet
    // (EIP-191) and persists TrustedDeviceRecords in
    // `a3net-pairing::TrustedDeviceStore`. Used for the first
    // step of every P2P link establishment: issue an invitation,
    // verify it, accept, and persist the credential.
    pub const PAIRING_INVITATION_CREATE: &'static str = "a3chat.pairing.invitation.create";
    pub const PAIRING_INVITATION_VERIFY: &'static str = "a3chat.pairing.invitation.verify";
    pub const PAIRING_INVITATION_PARSE: &'static str = "a3chat.pairing.invitation.parse";
    pub const PAIRING_INVITATION_ACCEPT: &'static str = "a3chat.pairing.invitation.accept";
    pub const PAIRING_INVITATION_REVOKE: &'static str = "a3chat.pairing.invitation.revoke";
    pub const PAIRING_TRUSTED_LIST: &'static str = "a3chat.pairing.trusted.list";
    pub const PAIRING_TRUSTED_GET: &'static str = "a3chat.pairing.trusted.get";
    pub const PAIRING_TRUSTED_REVOKE: &'static str = "a3chat.pairing.trusted.revoke";
    pub const PAIRING_CODE_CREATE: &'static str = "a3chat.pairing.code.create";
    pub const PAIRING_CODE_PARSE: &'static str = "a3chat.pairing.code.parse";
    pub const PAIRING_HEALTH: &'static str = "a3chat.pairing.health";

    // Groups
    pub const GROUP_CREATE: &'static str = "a3chat.group.create";
    pub const GROUP_INVITE: &'static str = "a3chat.group.invite";
    pub const GROUP_INVITE_LIST: &'static str = "a3chat.group.invite.list";
    pub const GROUP_INVITE_ACCEPT: &'static str = "a3chat.group.invite.accept";
    pub const GROUP_INVITE_DECLINE: &'static str = "a3chat.group.invite.decline";
    pub const GROUP_INVITE_REVOKE: &'static str = "a3chat.group.invite.revoke";
    pub const GROUP_INVITE_GET: &'static str = "a3chat.group.invite.get";
    pub const GROUP_JOIN: &'static str = "a3chat.group.join";
    pub const GROUP_LEAVE: &'static str = "a3chat.group.leave";
    pub const GROUP_MEMBER_ADD: &'static str = "a3chat.group.member.add";
    pub const GROUP_MEMBER_REMOVE: &'static str = "a3chat.group.member.remove";
    pub const GROUP_MEMBER_ROLE: &'static str = "a3chat.group.member.role";
    pub const GROUP_LIST: &'static str = "a3chat.group.list";
    pub const GROUP_MEMBERS: &'static str = "a3chat.group.members";
    pub const GROUP_MEMBER_GET: &'static str = "a3chat.group.member.get";
    pub const GROUP_METADATA_UPDATE: &'static str = "a3chat.group.metadata.update";
    pub const GROUP_TRANSFER_OWNERSHIP: &'static str = "a3chat.group.transfer_ownership";
    pub const GROUP_ANNOUNCEMENT_SET: &'static str = "a3chat.group.announcement.set";
    pub const GROUP_DISSOLVE: &'static str = "a3chat.group.dissolve";
    pub const GROUP_MUTE_MEMBER: &'static str = "a3chat.group.mute.member";
    pub const GROUP_MUTE_ALL: &'static str = "a3chat.group.mute.all";
    pub const GROUP_UNMUTE_MEMBER: &'static str = "a3chat.group.unmute.member";
    pub const GROUP_UNMUTE_ALL: &'static str = "a3chat.group.unmute.all";
    pub const GROUP_LIST_MUTED: &'static str = "a3chat.group.list_muted";
    pub const GROUP_NICKNAME_SET: &'static str = "a3chat.group.nickname.set";
    pub const GROUP_NICKNAME_GET: &'static str = "a3chat.group.nickname.get";
    pub const GROUP_NICKNAME_LIST: &'static str = "a3chat.group.nickname.list";
    pub const GROUP_MENTION_PARSE: &'static str = "a3chat.group.mention.parse";

    // Sync
    pub const CHAT_SYNC_SNAPSHOT: &'static str = "a3chat.chat.sync.snapshot";
    pub const CHAT_SYNC_DELTA: &'static str = "a3chat.chat.sync.delta";
    pub const CHAT_SYNC_COMPRESSED: &'static str = "a3chat.chat.sync.compressed";

    // Presence
    pub const PRESENCE_PUBLISH: &'static str = "a3chat.presence.publish";
    pub const PRESENCE_SUBSCRIBE: &'static str = "a3chat.presence.subscribe";

    // Profile — bridge to a3net-userstore
    pub const PROFILE_GET: &'static str = "a3chat.profile.get";
    pub const PROFILE_PUT: &'static str = "a3chat.profile.put";
    pub const PROFILE_PREFERENCES_PUT: &'static str = "a3chat.profile.preferences_put";
    pub const PROFILE_PUBLIC_KEY_ADD: &'static str = "a3chat.profile.public_key_add";
    pub const PROFILE_PUBLIC_KEY_LIST: &'static str = "a3chat.profile.public_key_list";
    pub const PROFILE_PUBLIC_KEY_REVOKE: &'static str = "a3chat.profile.public_key_revoke";
    pub const PROFILE_DEVICE_REGISTER: &'static str = "a3chat.profile.device_register";
    pub const PROFILE_DEVICE_LIST: &'static str = "a3chat.profile.device_list";
    pub const PROFILE_DIGIT_GET: &'static str = "a3chat.profile.digit_get";
    pub const PROFILE_AVATAR_SET: &'static str = "a3chat.profile.avatar_set";
    pub const PROFILE_AVATAR_UPLOAD: &'static str = "a3chat.profile.avatar.upload";
    pub const PROFILE_AVATAR_GET: &'static str = "a3chat.profile.avatar.get";
    pub const PROFILE_AVATAR_REMOVE: &'static str = "a3chat.profile.avatar.remove";
    pub const PROFILE_PUBLIC_KEY_LABEL: &'static str = "a3chat.profile.public_key.label";
    pub const PROFILE_KIND_GET: &'static str = "a3chat.profile.kind.get";
    pub const PROFILE_KIND_SET: &'static str = "a3chat.profile.kind.set";

    // Media / crypto
    pub const MEDIA_HEALTH: &'static str = "a3chat.media.health";
    pub const MEDIA_UPLOAD_INIT: &'static str = "a3chat.media.upload_init";
    pub const MEDIA_UPLOAD_CHUNK: &'static str = "a3chat.media.upload_chunk";
    pub const MEDIA_UPLOAD_FINALIZE: &'static str = "a3chat.media.upload_finalize";
    pub const MEDIA_DOWNLOAD_GET: &'static str = "a3chat.media.download_get";
    pub const E2E_BUNDLE_EXPORT: &'static str = "a3chat.e2e.bundle.export";
    pub const E2E_BUNDLE_IMPORT: &'static str = "a3chat.e2e.bundle.import";

    // Moderation (content / attachment policy gate). Names match
    // the routing table in `a3chat_app::moderation_service`.
    pub const MODERATION_CHECK_CONTENT: &'static str = "a3chat.moderation.check_content";
    pub const MODERATION_CHECK_ATTACHMENT: &'static str = "a3chat.moderation.check_attachment";
    pub const MODERATION_LIST_BLOCKED: &'static str = "a3chat.moderation.list_blocked";
    pub const MODERATION_SET_DENY_DEFAULT: &'static str = "a3chat.moderation.set_deny_default";
    pub const MODERATION_STATS: &'static str = "a3chat.moderation.stats";

    // Stream (SSE)
    pub const STREAM_SUBSCRIBE: &'static str = "a3chat.stream.subscribe";
    pub const STREAM_UNSUBSCRIBE: &'static str = "a3chat.stream.unsubscribe";
    pub const STREAM_LIST: &'static str = "a3chat.stream.list";

    // Link bookmarks / favorites (F-08) ------------------------------
    // CRUD + listing/search + maintenance. `add` and `update` share
    // the `UpsertLinkBookmarkRequest` shape so the client uses the
    // same form for both; the dispatcher differentiates via method.
    pub const LINK_BOOKMARK_ADD: &'static str = "a3chat.link.bookmark.add";
    pub const LINK_BOOKMARK_UPDATE: &'static str = "a3chat.link.bookmark.update";
    pub const LINK_BOOKMARK_GET: &'static str = "a3chat.link.bookmark.get";
    pub const LINK_BOOKMARK_GET_BY_URL: &'static str = "a3chat.link.bookmark.get_by_url";
    pub const LINK_BOOKMARK_LIST: &'static str = "a3chat.link.bookmark.list";
    pub const LINK_BOOKMARK_SEARCH: &'static str = "a3chat.link.bookmark.search";
    pub const LINK_BOOKMARK_DELETE: &'static str = "a3chat.link.bookmark.delete";
    pub const LINK_BOOKMARK_SET_PINNED: &'static str = "a3chat.link.bookmark.set_pinned";
    pub const LINK_BOOKMARK_SET_ARCHIVED: &'static str = "a3chat.link.bookmark.set_archived";
    pub const LINK_BOOKMARK_TOUCH_VISIT: &'static str = "a3chat.link.bookmark.touch_visit";
    pub const LINK_BOOKMARK_TAGS: &'static str = "a3chat.link.bookmark.tags";
    pub const LINK_BOOKMARK_FOLDERS: &'static str = "a3chat.link.bookmark.folders";
    pub const LINK_BOOKMARK_COUNT: &'static str = "a3chat.link.bookmark.count";

    // Moments / 朋友圈 (F-05). Mirrors the 19-method
    // `a3chat.moments.*` JSON-RPC namespace that
    // `a3chat_app::moments_service::dispatch` serves. Without
    // these constants in `ALL`, the discovery helpers and the CLI
    // parsing table silently drop every Moments method.
    pub const MOMENTS_NODE_INFO: &'static str = "a3chat.moments.node_info";
    pub const MOMENTS_POST_CREATE: &'static str = "a3chat.moments.post.create";
    pub const MOMENTS_POST_UPDATE: &'static str = "a3chat.moments.post.update";
    pub const MOMENTS_POST_DELETE: &'static str = "a3chat.moments.post.delete";
    pub const MOMENTS_POST_GET: &'static str = "a3chat.moments.post.get";
    pub const MOMENTS_POSTS_BY_USER: &'static str = "a3chat.moments.posts.by_user";
    pub const MOMENTS_TIMELINE: &'static str = "a3chat.moments.timeline";
    pub const MOMENTS_COMMENT_ADD: &'static str = "a3chat.moments.comment.add";
    pub const MOMENTS_COMMENT_EDIT: &'static str = "a3chat.moments.comment.edit";
    pub const MOMENTS_COMMENT_DELETE: &'static str = "a3chat.moments.comment.delete";
    pub const MOMENTS_COMMENTS_LIST: &'static str = "a3chat.moments.comments.list";
    pub const MOMENTS_REACT: &'static str = "a3chat.moments.react";
    pub const MOMENTS_UNREACT: &'static str = "a3chat.moments.unreact";
    pub const MOMENTS_REACTIONS_LIST: &'static str = "a3chat.moments.reactions.list";
    pub const MOMENTS_FOLLOW: &'static str = "a3chat.moments.follow";
    pub const MOMENTS_UNFOLLOW: &'static str = "a3chat.moments.unfollow";
    pub const MOMENTS_FOLLOWERS_LIST: &'static str = "a3chat.moments.followers.list";
    pub const MOMENTS_FOLLOWING_LIST: &'static str = "a3chat.moments.following.list";
    pub const MOMENTS_FOLLOWING_CHECK: &'static str = "a3chat.moments.following.check";
    pub const MOMENTS_BLOCK: &'static str = "a3chat.moments.block";
    pub const MOMENTS_UNBLOCK: &'static str = "a3chat.moments.unblock";
    pub const MOMENTS_BLOCKLIST_LIST: &'static str = "a3chat.moments.blocklist.list";
    pub const MOMENTS_SHARE: &'static str = "a3chat.moments.share";
    pub const MOMENTS_REPORT: &'static str = "a3chat.moments.report";
    pub const MOMENTS_VERIFY_POST: &'static str = "a3chat.moments.verify.post";
    pub const MOMENTS_VERIFY_COMMENT: &'static str = "a3chat.moments.verify.comment";
    pub const MOMENTS_VERIFY_REACTION: &'static str = "a3chat.moments.verify.reaction";

    // Channel / 公众号 (F-09). Backed by `a3net-news::NewsService`
    // (gossip fan-out, monotonic per-room sequence) with a
    // friendlier `account_id` / `feed_id` surface in front. The
    // dispatcher in `a3chat_app::channel_service::dispatch`
    // owns the actual routing — keeping these constants next to
    // the rest of the `a3chat.*` namespace so the CLI help
    // text and discovery helpers enumerate them automatically.
    pub const CHANNEL_ACCOUNT_REGISTER: &'static str = "a3chat.channel.account.register";
    pub const CHANNEL_ACCOUNT_UPDATE: &'static str = "a3chat.channel.account.update";
    pub const CHANNEL_ACCOUNT_GET: &'static str = "a3chat.channel.account.get";
    pub const CHANNEL_ACCOUNT_GET_BY_OWNER: &'static str = "a3chat.channel.account.get_by_owner";
    pub const CHANNEL_ACCOUNT_LIST: &'static str = "a3chat.channel.account.list";
    pub const CHANNEL_ACCOUNT_SEARCH: &'static str = "a3chat.channel.account.search";
    pub const CHANNEL_ACCOUNT_DELETE: &'static str = "a3chat.channel.account.delete";
    pub const CHANNEL_SUBSCRIBE: &'static str = "a3chat.channel.subscribe";
    pub const CHANNEL_UNSUBSCRIBE: &'static str = "a3chat.channel.unsubscribe";
    pub const CHANNEL_SUBSCRIPTIONS_LIST: &'static str = "a3chat.channel.subscriptions.list";
    pub const CHANNEL_SUBSCRIPTIONS_OF_ACCOUNT: &'static str =
        "a3chat.channel.subscriptions.of_account";
    pub const CHANNEL_SUBSCRIPTION_SET_NOTIFY: &'static str =
        "a3chat.channel.subscription.set_notify";
    pub const CHANNEL_SUBSCRIPTION_SET_PINNED: &'static str =
        "a3chat.channel.subscription.set_pinned";
    pub const CHANNEL_FEED_PUBLISH: &'static str = "a3chat.channel.feed.publish";
    pub const CHANNEL_FEED_RETRACT: &'static str = "a3chat.channel.feed.retract";
    pub const CHANNEL_FEED_GET: &'static str = "a3chat.channel.feed.get";
    pub const CHANNEL_FEED_LIST: &'static str = "a3chat.channel.feed.list";
    pub const CHANNEL_FEED_TIMELINE: &'static str = "a3chat.channel.feed.timeline";
    pub const CHANNEL_FEED_MARK_READ: &'static str = "a3chat.channel.feed.mark_read";
    pub const CHANNEL_FEED_UNREAD_COUNT: &'static str = "a3chat.channel.feed.unread_count";
    pub const CHANNEL_HEALTH: &'static str = "a3chat.channel.health";

    // SSE notification event names (emitted on `/rpc/stream`).
    // The frontend subscribes to these via EventSource.
    pub const NOTIFICATION_CHAT_MESSAGE_RECEIVED: &'static str = "a3chat.chat.message.received";
    pub const NOTIFICATION_CHAT_MESSAGE_RECALLED: &'static str = "a3chat.chat.message.recalled";
    pub const NOTIFICATION_CHAT_MESSAGE_READ: &'static str = "a3chat.chat.message.read";
    pub const NOTIFICATION_CHAT_MESSAGE_EDITED: &'static str = "a3chat.chat.message.edited";
    pub const NOTIFICATION_CHAT_MESSAGE_DELETED: &'static str = "a3chat.chat.message.deleted";
    pub const NOTIFICATION_CHAT_TYPING: &'static str = "a3chat.chat.typing";
    pub const NOTIFICATION_PRESENCE_CHANGED: &'static str = "a3chat.presence.changed";
    pub const NOTIFICATION_GROUP_MEMBER_JOINED: &'static str = "a3chat.group.member.joined";
    pub const NOTIFICATION_GROUP_MEMBER_REMOVED: &'static str = "a3chat.group.member.removed";
    pub const NOTIFICATION_GROUP_INVITATION_RECEIVED: &'static str =
        "a3chat.group.invitation.received";
    pub const NOTIFICATION_CONTACT_REQUEST_RECEIVED: &'static str =
        "a3chat.contact.request.received";

    // ── F-07 Chat drafts (per-conversation draft persistence) ──
    pub const CHAT_DRAFT_SAVE: &'static str = "a3chat.chat.draft.save";
    pub const CHAT_DRAFT_GET: &'static str = "a3chat.chat.draft.get";
    pub const CHAT_DRAFT_DELETE: &'static str = "a3chat.chat.draft.delete";
    pub const CHAT_DRAFT_LIST: &'static str = "a3chat.chat.draft.list";
    pub const CHAT_DRAFT_CLEAR: &'static str = "a3chat.chat.draft.clear";

    // ── F-07 Chat reactions (emoji reactions on messages) ──
    pub const CHAT_REACTION_ADD: &'static str = "a3chat.chat.reaction.add";
    pub const CHAT_REACTION_REMOVE: &'static str = "a3chat.chat.reaction.remove";
    pub const CHAT_REACTION_GET: &'static str = "a3chat.chat.reaction.get";

    // ── F-07 Device management (per-user multi-device) ──
    pub const DEVICE_REGISTER: &'static str = "a3chat.device.register";
    pub const DEVICE_LIST: &'static str = "a3chat.device.list";
    pub const DEVICE_GET: &'static str = "a3chat.device.get";
    pub const DEVICE_REVOKE: &'static str = "a3chat.device.revoke";
    pub const DEVICE_SET_PRIMARY: &'static str = "a3chat.device.set_primary";
    pub const DEVICE_GET_CURRENT: &'static str = "a3chat.device.get_current";
    pub const DEVICE_TOUCH: &'static str = "a3chat.device.touch";

    // ── F-07 Process-level liveness probes ──
    pub const HEALTHZ: &'static str = "a3chat.healthz";
    pub const RPC_HEALTH: &'static str = "a3chat.rpc.health";

    // ── F-07 E2E session handshake (introspection helpers) ──
    pub const E2E_NEEDS_REHANDSHAKE: &'static str = "a3chat.e2e.handshake.needs_rehandshake";
    pub const E2E_IS_HANDSHAKE_COMPLETE: &'static str = "a3chat.e2e.handshake.is_complete";

    /// Stable list of every method name. Useful for discovery.
    pub const ALL: &'static [&'static str] = &[
        Self::CHAT_CONVERSATION_LIST,
        Self::CHAT_CONVERSATION_OPEN,
        Self::CHAT_CONVERSATION_CREATE_DIRECT,
        Self::CHAT_MESSAGE_SEND,
        Self::CHAT_MESSAGE_RECALL,
        Self::CHAT_MESSAGE_ACK,
        Self::CHAT_MESSAGE_EDIT,
        Self::CHAT_MESSAGE_DELETE,
        Self::CHAT_MESSAGE_FORWARD,
        Self::CHAT_SEARCH,
        Self::CHAT_TYPING,
        Self::CHAT_CONVERSATION_PIN,
        Self::CHAT_CONVERSATION_UNPIN,
        Self::CHAT_CONVERSATION_TOGGLE_PIN,
        Self::CHAT_CONVERSATION_LIST_PINNED,
        Self::CHAT_NOTIFICATION_SET_DND,
        Self::CHAT_NOTIFICATION_GET_DND,
        Self::CHAT_NOTIFICATION_SET_CONVERSATION,
        Self::CHAT_NOTIFICATION_GET_CONVERSATION,
        Self::CHAT_NOTIFICATION_MUTE,
        Self::CHAT_NOTIFICATION_UNMUTE,
        Self::CHAT_NOTIFICATION_LIST_MUTED,
        Self::E2E_ENCRYPT,
        Self::E2E_DECRYPT,
        Self::E2E_INITIATE_HANDSHAKE,
        Self::E2E_RESPOND_HANDSHAKE,
        Self::E2E_COMPLETE_HANDSHAKE,
        Self::CONTACT_LIST,
        Self::CONTACT_ADD_REQUEST,
        Self::CONTACT_ACCEPT_REQUEST,
        Self::CONTACT_BLOCK,
        Self::CONTACT_UNBLOCK,
        Self::CONTACT_QR_INVITE,
        Self::CONTACT_ADD,
        Self::CONTACT_REMOVE,
        Self::CONTACT_GET,
        Self::CONTACT_SEARCH,
        Self::CONTACT_TOGGLE_FAVORITE,
        Self::CONTACT_UPDATE,
        // Pairing namespace
        Self::PAIRING_INVITATION_CREATE,
        Self::PAIRING_INVITATION_VERIFY,
        Self::PAIRING_INVITATION_PARSE,
        Self::PAIRING_INVITATION_ACCEPT,
        Self::PAIRING_INVITATION_REVOKE,
        Self::PAIRING_TRUSTED_LIST,
        Self::PAIRING_TRUSTED_GET,
        Self::PAIRING_TRUSTED_REVOKE,
        Self::PAIRING_CODE_CREATE,
        Self::PAIRING_CODE_PARSE,
        Self::PAIRING_HEALTH,
        Self::GROUP_CREATE,
        Self::GROUP_INVITE,
        Self::GROUP_INVITE_LIST,
        Self::GROUP_INVITE_ACCEPT,
        Self::GROUP_INVITE_DECLINE,
        Self::GROUP_INVITE_REVOKE,
        Self::GROUP_INVITE_GET,
        Self::GROUP_JOIN,
        Self::GROUP_LEAVE,
        Self::GROUP_MEMBER_ADD,
        Self::GROUP_MEMBER_REMOVE,
        Self::GROUP_MEMBER_ROLE,
        Self::GROUP_LIST,
        Self::GROUP_MEMBERS,
        Self::GROUP_MEMBER_GET,
        Self::GROUP_METADATA_UPDATE,
        Self::GROUP_TRANSFER_OWNERSHIP,
        Self::GROUP_ANNOUNCEMENT_SET,
        Self::GROUP_DISSOLVE,
        Self::GROUP_MUTE_MEMBER,
        Self::GROUP_MUTE_ALL,
        Self::GROUP_UNMUTE_MEMBER,
        Self::GROUP_UNMUTE_ALL,
        Self::GROUP_LIST_MUTED,
        Self::GROUP_NICKNAME_SET,
        Self::GROUP_NICKNAME_GET,
        Self::GROUP_NICKNAME_LIST,
        Self::GROUP_MENTION_PARSE,
        Self::CHAT_SYNC_SNAPSHOT,
        Self::CHAT_SYNC_DELTA,
        Self::CHAT_SYNC_COMPRESSED,
        Self::PRESENCE_PUBLISH,
        Self::PRESENCE_SUBSCRIBE,
        Self::PROFILE_GET,
        Self::PROFILE_PUT,
        Self::PROFILE_PREFERENCES_PUT,
        Self::PROFILE_PUBLIC_KEY_ADD,
        Self::PROFILE_PUBLIC_KEY_LIST,
        Self::PROFILE_PUBLIC_KEY_REVOKE,
        Self::PROFILE_DEVICE_REGISTER,
        Self::PROFILE_DEVICE_LIST,
        Self::PROFILE_DIGIT_GET,
        Self::PROFILE_AVATAR_SET,
        Self::PROFILE_AVATAR_UPLOAD,
        Self::PROFILE_AVATAR_GET,
        Self::PROFILE_AVATAR_REMOVE,
        Self::PROFILE_PUBLIC_KEY_LABEL,
        Self::PROFILE_KIND_GET,
        Self::PROFILE_KIND_SET,
        Self::MEDIA_HEALTH,
        Self::MEDIA_UPLOAD_INIT,
        Self::MEDIA_UPLOAD_CHUNK,
        Self::MEDIA_UPLOAD_FINALIZE,
        Self::MEDIA_DOWNLOAD_GET,
        Self::E2E_BUNDLE_EXPORT,
        Self::E2E_BUNDLE_IMPORT,
        Self::MODERATION_CHECK_CONTENT,
        Self::MODERATION_CHECK_ATTACHMENT,
        Self::MODERATION_LIST_BLOCKED,
        Self::MODERATION_SET_DENY_DEFAULT,
        Self::MODERATION_STATS,
        Self::STREAM_SUBSCRIBE,
        Self::STREAM_UNSUBSCRIBE,
        Self::STREAM_LIST,
        Self::LINK_BOOKMARK_ADD,
        Self::LINK_BOOKMARK_UPDATE,
        Self::LINK_BOOKMARK_GET,
        Self::LINK_BOOKMARK_GET_BY_URL,
        Self::LINK_BOOKMARK_LIST,
        Self::LINK_BOOKMARK_SEARCH,
        Self::LINK_BOOKMARK_DELETE,
        Self::LINK_BOOKMARK_SET_PINNED,
        Self::LINK_BOOKMARK_SET_ARCHIVED,
        Self::LINK_BOOKMARK_TOUCH_VISIT,
        Self::LINK_BOOKMARK_TAGS,
        Self::LINK_BOOKMARK_FOLDERS,
        Self::LINK_BOOKMARK_COUNT,
        // Moments / 朋友圈 (F-05) — 27 methods total
        Self::MOMENTS_NODE_INFO,
        Self::MOMENTS_POST_CREATE,
        Self::MOMENTS_POST_UPDATE,
        Self::MOMENTS_POST_DELETE,
        Self::MOMENTS_POST_GET,
        Self::MOMENTS_POSTS_BY_USER,
        Self::MOMENTS_TIMELINE,
        Self::MOMENTS_COMMENT_ADD,
        Self::MOMENTS_COMMENT_EDIT,
        Self::MOMENTS_COMMENT_DELETE,
        Self::MOMENTS_COMMENTS_LIST,
        Self::MOMENTS_REACT,
        Self::MOMENTS_UNREACT,
        Self::MOMENTS_REACTIONS_LIST,
        Self::MOMENTS_FOLLOW,
        Self::MOMENTS_UNFOLLOW,
        Self::MOMENTS_FOLLOWERS_LIST,
        Self::MOMENTS_FOLLOWING_LIST,
        Self::MOMENTS_FOLLOWING_CHECK,
        Self::MOMENTS_BLOCK,
        Self::MOMENTS_UNBLOCK,
        Self::MOMENTS_BLOCKLIST_LIST,
        Self::MOMENTS_SHARE,
        Self::MOMENTS_REPORT,
        Self::MOMENTS_VERIFY_POST,
        Self::MOMENTS_VERIFY_COMMENT,
        Self::MOMENTS_VERIFY_REACTION,
        // F-09 Channel / 公众号 — 20 methods total
        Self::CHANNEL_ACCOUNT_REGISTER,
        Self::CHANNEL_ACCOUNT_UPDATE,
        Self::CHANNEL_ACCOUNT_GET,
        Self::CHANNEL_ACCOUNT_GET_BY_OWNER,
        Self::CHANNEL_ACCOUNT_LIST,
        Self::CHANNEL_ACCOUNT_SEARCH,
        Self::CHANNEL_ACCOUNT_DELETE,
        Self::CHANNEL_SUBSCRIBE,
        Self::CHANNEL_UNSUBSCRIBE,
        Self::CHANNEL_SUBSCRIPTIONS_LIST,
        Self::CHANNEL_SUBSCRIPTIONS_OF_ACCOUNT,
        Self::CHANNEL_SUBSCRIPTION_SET_NOTIFY,
        Self::CHANNEL_SUBSCRIPTION_SET_PINNED,
        Self::CHANNEL_FEED_PUBLISH,
        Self::CHANNEL_FEED_RETRACT,
        Self::CHANNEL_FEED_GET,
        Self::CHANNEL_FEED_LIST,
        Self::CHANNEL_FEED_TIMELINE,
        Self::CHANNEL_FEED_MARK_READ,
        Self::CHANNEL_FEED_UNREAD_COUNT,
        Self::CHANNEL_HEALTH,
        // F-07 newly wired
        Self::CHAT_DRAFT_SAVE,
        Self::CHAT_DRAFT_GET,
        Self::CHAT_DRAFT_DELETE,
        Self::CHAT_DRAFT_LIST,
        Self::CHAT_DRAFT_CLEAR,
        Self::CHAT_REACTION_ADD,
        Self::CHAT_REACTION_REMOVE,
        Self::CHAT_REACTION_GET,
        Self::DEVICE_REGISTER,
        Self::DEVICE_LIST,
        Self::DEVICE_GET,
        Self::DEVICE_REVOKE,
        Self::DEVICE_SET_PRIMARY,
        Self::DEVICE_GET_CURRENT,
        Self::DEVICE_TOUCH,
        Self::HEALTHZ,
        Self::RPC_HEALTH,
        Self::E2E_NEEDS_REHANDSHAKE,
        Self::E2E_IS_HANDSHAKE_COMPLETE,
    ];
}

/// Transport-agnostic RPC client. Implemented by:
/// - `a3chat-app` server-side (in-process call)
/// - `a3chat-tauri` desktop client (HTTP over localhost)
/// - `mobile/a3chat` Flutter client (HTTP over LAN / remote)
/// - tests (`MockRpcClient` for hermetic integration)
#[async_trait::async_trait]
pub trait RpcClient: Send + Sync {
    /// Issue a JSON-RPC call. `params` is the JSON params object
    /// (`{}` if none). Returns the parsed `result` field on success
    /// or an error with `code` + `message` + `kind` on failure.
    async fn call_json(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, crate::error::A3chatError>;

    /// Convenience — typed round-trip via `serde::Serialize` /
    /// `Deserialize`.
    async fn call<P: Serialize + Send + Sync, R: for<'de> Deserialize<'de> + Send>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, crate::error::A3chatError> {
        let v = serde_json::to_value(params)
            .map_err(|e| crate::error::A3chatError::Internal(format!("serialize params: {e}")))?;
        let r = self.call_json(method, v).await?;
        serde_json::from_value(r)
            .map_err(|e| crate::error::A3chatError::Internal(format!("deserialize result: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_methods_have_a3chat_prefix() {
        for m in A3chatRpcMethod::ALL {
            assert!(
                m.starts_with("a3chat."),
                "method {m} must start with a3chat."
            );
        }
    }

    #[test]
    fn all_methods_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in A3chatRpcMethod::ALL {
            assert!(seen.insert(*m), "duplicate method name: {m}");
        }
    }

    #[test]
    fn all_list_is_not_empty() {
        assert!(!A3chatRpcMethod::ALL.is_empty());
        // Bump floor when adding a new namespace. As of F-09 the
        // floor covers:
        //  - chat / contact / group / sync / presence (~24)
        //  - profile (~18)
        //  - media / e2e / stream (~7)
        //  - link bookmarks (~13)
        //  - chat drafts / reactions (~8)
        //  - device (~7)
        //  - notifications (~7)
        //  - health + e2e handshake (~4)
        //  - moments / 朋友圈 (~27)
        //  - channel / 公众号 (~20)
        // Total ≈ 135.
        assert!(A3chatRpcMethod::ALL.len() >= 130);
    }
}
