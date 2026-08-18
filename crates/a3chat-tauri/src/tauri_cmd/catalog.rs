//! Static catalog of every Tauri command exposed by a3chat-tauri.
//!
//! The frontend generator (or any operator) can introspect
//! [`COMMAND_CATALOG`] to enumerate every menu / button binding
//! without parsing the Rust source. Each entry records:
//!
//! - The Tauri command name (the frontend `invoke()` target).
//! - The underlying RPC method (if any).
//! - The screen the command belongs to.
//! - A human description and a stable taxonomy tag.
//!
//! DO-178C §5.2 — *traceability*: every UI action has a 1:1 entry
//! in this table, so a user complaint can be traced from the menu
//! label to the daemon handler in one grep.

use serde::Serialize;

use a3chat_core::rpc::A3chatRpcMethod;

use super::state::Screen;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CommandCatalogEntry {
    /// Frontend `invoke()` target — the name passed to `invoke('{name}', ...)`.
    pub tauri_name: &'static str,
    /// Underlying RPC method, if any. `None` for session / daemon ops.
    pub rpc_method: Option<&'static str>,
    /// Semantic group (e.g. `"chat.message"`).
    pub group: &'static str,
    /// Screen the command is primarily surfaced on.
    pub screen: Screen,
    /// Short human description.
    pub summary: &'static str,
    /// Path of input parameters (each is a `FieldError` path).
    pub param_fields: &'static [&'static str],
}

/// Enumerate every Tauri command. Static so the catalogue is
/// available at compile time — the frontend toolchain can read the
/// list before the application even launches.
pub const COMMAND_CATALOG: &[CommandCatalogEntry] = &[
    // ── Session / daemon ops ──────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "login",
        rpc_method: None,
        group: "session",
        screen: Screen::Login,
        summary: "Connect to the daemon and load the Chats window.",
        param_fields: &["base_url", "owner"],
    },
    CommandCatalogEntry {
        tauri_name: "logout",
        rpc_method: None,
        group: "session",
        screen: Screen::Chats,
        summary: "Disconnect and return to the login window.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "session_info",
        rpc_method: None,
        group: "session",
        screen: Screen::Chats,
        summary: "Echo the current session owner / base_url.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "app_version",
        rpc_method: None,
        group: "session",
        screen: Screen::Settings,
        summary: "Return the application version metadata.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "doctor",
        rpc_method: None,
        group: "session",
        screen: Screen::Doctor,
        summary: "Run the daemon health check.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "start_daemon",
        rpc_method: None,
        group: "session",
        screen: Screen::Settings,
        summary: "Spawn the daemon as a child process.",
        param_fields: &["bind", "owner", "storage"],
    },
    CommandCatalogEntry {
        tauri_name: "stop_daemon",
        rpc_method: None,
        group: "session",
        screen: Screen::Settings,
        summary: "Stop the daemon via the lifecycle handle.",
        param_fields: &["handle"],
    },
    CommandCatalogEntry {
        tauri_name: "menu_bar",
        rpc_method: None,
        group: "ui",
        screen: Screen::Chats,
        summary: "Resolve the top-level menu tree.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "sidebar_tree",
        rpc_method: Some(A3chatRpcMethod::CHAT_CONVERSATION_LIST),
        group: "ui",
        screen: Screen::Chats,
        summary: "Resolve the sidebar tree from the conversation list.",
        param_fields: &[],
    },
    // ── Conversations ─────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "chat_conversation_list",
        rpc_method: Some(A3chatRpcMethod::CHAT_CONVERSATION_LIST),
        group: "chat.conversation",
        screen: Screen::Conversations,
        summary: "List every conversation visible to the local user.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "chat_conversation_open",
        rpc_method: Some(A3chatRpcMethod::CHAT_CONVERSATION_OPEN),
        group: "chat.conversation",
        screen: Screen::Conversations,
        summary: "Open a conversation (fetch metadata).",
        param_fields: &["conversation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_conversation_create_direct",
        rpc_method: Some(A3chatRpcMethod::CHAT_CONVERSATION_CREATE_DIRECT),
        group: "chat.conversation",
        screen: Screen::Conversations,
        summary: "Open or re-use a 1-to-1 conversation with another user.",
        param_fields: &["peer"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_conversation_pin",
        rpc_method: Some(A3chatRpcMethod::CHAT_CONVERSATION_PIN),
        group: "chat.conversation",
        screen: Screen::Conversations,
        summary: "Pin or un-pin a conversation for the local user.",
        param_fields: &["conversation_id", "pinned"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_conversation_mute",
        rpc_method: Some(A3chatRpcMethod::CHAT_CONVERSATION_MUTE),
        group: "chat.conversation",
        screen: Screen::Conversations,
        summary: "Mute or un-mute notifications for a conversation.",
        param_fields: &["conversation_id", "muted"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_conversation_strong_notify",
        rpc_method: Some(A3chatRpcMethod::CHAT_CONVERSATION_STRONG_NOTIFY),
        group: "chat.conversation",
        screen: Screen::Conversations,
        summary: "Enable or disable strong-notify for a conversation.",
        param_fields: &["conversation_id", "strong_notify"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_typing",
        rpc_method: Some(A3chatRpcMethod::CHAT_TYPING),
        group: "chat.conversation",
        screen: Screen::Messages,
        summary: "Emit a typing notification.",
        param_fields: &["conversation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_search",
        rpc_method: Some(A3chatRpcMethod::CHAT_SEARCH),
        group: "chat.conversation",
        screen: Screen::Messages,
        summary: "Full-text search across messages.",
        param_fields: &["needle", "limit"],
    },
    // ── Messages ──────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "chat_message_send",
        rpc_method: Some(A3chatRpcMethod::CHAT_MESSAGE_SEND),
        group: "chat.message",
        screen: Screen::Messages,
        summary: "Send a message.",
        param_fields: &["envelope"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_message_recall",
        rpc_method: Some(A3chatRpcMethod::CHAT_MESSAGE_RECALL),
        group: "chat.message",
        screen: Screen::Messages,
        summary: "Recall a previously sent message.",
        param_fields: &["message_id"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_message_ack",
        rpc_method: Some(A3chatRpcMethod::CHAT_MESSAGE_ACK),
        group: "chat.message",
        screen: Screen::Messages,
        summary: "Mark a message read.",
        param_fields: &["message_id"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_message_edit",
        rpc_method: Some(A3chatRpcMethod::CHAT_MESSAGE_EDIT),
        group: "chat.message",
        screen: Screen::Messages,
        summary: "Edit a sent message.",
        param_fields: &["message_id", "body"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_message_delete",
        rpc_method: Some(A3chatRpcMethod::CHAT_MESSAGE_DELETE),
        group: "chat.message",
        screen: Screen::Messages,
        summary: "Delete a message for me.",
        param_fields: &["message_id"],
    },
    // ── Sync ──────────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "chat_sync_snapshot",
        rpc_method: Some(A3chatRpcMethod::CHAT_SYNC_SNAPSHOT),
        group: "chat.sync",
        screen: Screen::Sync,
        summary: "Fetch a full sync snapshot.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "chat_sync_delta",
        rpc_method: Some(A3chatRpcMethod::CHAT_SYNC_DELTA),
        group: "chat.sync",
        screen: Screen::Sync,
        summary: "Fetch a delta since the cursor.",
        param_fields: &["cursors"],
    },
    CommandCatalogEntry {
        tauri_name: "chat_sync_compressed",
        rpc_method: Some(A3chatRpcMethod::CHAT_SYNC_COMPRESSED),
        group: "chat.sync",
        screen: Screen::Sync,
        summary: "Fetch a zstd-compressed delta.",
        param_fields: &[],
    },
    // ── Profile ───────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "profile_get",
        rpc_method: Some(A3chatRpcMethod::PROFILE_GET),
        group: "profile",
        screen: Screen::Profile,
        summary: "Get the local user profile.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "profile_put",
        rpc_method: Some(A3chatRpcMethod::PROFILE_PUT),
        group: "profile",
        screen: Screen::Profile,
        summary: "Replace the local user profile.",
        param_fields: &["profile"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_preferences_put",
        rpc_method: Some(A3chatRpcMethod::PROFILE_PREFERENCES_PUT),
        group: "profile",
        screen: Screen::Profile,
        summary: "Update profile preferences.",
        param_fields: &["prefs"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_digit_get",
        rpc_method: Some(A3chatRpcMethod::PROFILE_DIGIT_GET),
        group: "profile",
        screen: Screen::Profile,
        summary: "Get the user's digit (display number).",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "profile_public_key_add",
        rpc_method: Some(A3chatRpcMethod::PROFILE_PUBLIC_KEY_ADD),
        group: "profile",
        screen: Screen::Profile,
        summary: "Add a public key to the profile.",
        param_fields: &["algorithm", "public_key"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_public_key_list",
        rpc_method: Some(A3chatRpcMethod::PROFILE_PUBLIC_KEY_LIST),
        group: "profile",
        screen: Screen::Profile,
        summary: "List the public keys on the profile.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "profile_public_key_revoke",
        rpc_method: Some(A3chatRpcMethod::PROFILE_PUBLIC_KEY_REVOKE),
        group: "profile",
        screen: Screen::Profile,
        summary: "Revoke a public key.",
        param_fields: &["public_key"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_device_register",
        rpc_method: Some(A3chatRpcMethod::PROFILE_DEVICE_REGISTER),
        group: "profile",
        screen: Screen::Profile,
        summary: "Register this device with the profile.",
        param_fields: &["device_class", "device_label"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_device_list",
        rpc_method: Some(A3chatRpcMethod::PROFILE_DEVICE_LIST),
        group: "profile",
        screen: Screen::Profile,
        summary: "List devices registered on the profile.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "profile_avatar_set",
        rpc_method: Some(A3chatRpcMethod::PROFILE_AVATAR_SET),
        group: "profile",
        screen: Screen::Profile,
        summary: "Set the profile avatar (by blob hash).",
        param_fields: &["blob_hash"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_avatar_upload",
        rpc_method: Some(A3chatRpcMethod::PROFILE_AVATAR_UPLOAD),
        group: "profile",
        screen: Screen::Profile,
        summary: "Upload raw avatar bytes (base64) and set the profile avatar.",
        param_fields: &["mime_type", "bytes_b64"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_avatar_get",
        rpc_method: Some(A3chatRpcMethod::PROFILE_AVATAR_GET),
        group: "profile",
        screen: Screen::Profile,
        summary: "Fetch the avatar bytes (base64) for the calling user.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "profile_avatar_remove",
        rpc_method: Some(A3chatRpcMethod::PROFILE_AVATAR_REMOVE),
        group: "profile",
        screen: Screen::Profile,
        summary: "Drop the avatar bytes and clear the profile reference.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "profile_kind_set",
        rpc_method: Some(A3chatRpcMethod::PROFILE_KIND_SET),
        group: "profile",
        screen: Screen::Profile,
        summary: "Set the account class (human/agent/iot/service/unknown).",
        param_fields: &["kind"],
    },
    CommandCatalogEntry {
        tauri_name: "profile_kind_get",
        rpc_method: Some(A3chatRpcMethod::PROFILE_KIND_GET),
        group: "profile",
        screen: Screen::Profile,
        summary: "Read the account class.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "profile_public_key_label",
        rpc_method: Some(A3chatRpcMethod::PROFILE_PUBLIC_KEY_LABEL),
        group: "profile",
        screen: Screen::Profile,
        summary: "Patch the human-readable label on an existing public key.",
        param_fields: &["key_id", "label"],
    },
    // ── Contact ───────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "contact_list",
        rpc_method: Some(A3chatRpcMethod::CONTACT_LIST),
        group: "contact",
        screen: Screen::Contacts,
        summary: "List the contact book + blocklist.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "contact_add_request",
        rpc_method: Some(A3chatRpcMethod::CONTACT_ADD_REQUEST),
        group: "contact",
        screen: Screen::Contacts,
        summary: "Send a friend request.",
        param_fields: &["to_user_id", "message"],
    },
    CommandCatalogEntry {
        tauri_name: "contact_accept_request",
        rpc_method: Some(A3chatRpcMethod::CONTACT_ACCEPT_REQUEST),
        group: "contact",
        screen: Screen::Contacts,
        summary: "Accept an inbound friend request.",
        param_fields: &["request_id"],
    },
    CommandCatalogEntry {
        tauri_name: "contact_block",
        rpc_method: Some(A3chatRpcMethod::CONTACT_BLOCK),
        group: "contact",
        screen: Screen::Contacts,
        summary: "Block a user.",
        param_fields: &["user_id"],
    },
    CommandCatalogEntry {
        tauri_name: "contact_unblock",
        rpc_method: Some(A3chatRpcMethod::CONTACT_UNBLOCK),
        group: "contact",
        screen: Screen::Contacts,
        summary: "Unblock a user.",
        param_fields: &["user_id"],
    },
    CommandCatalogEntry {
        tauri_name: "contact_qr_invite",
        rpc_method: Some(A3chatRpcMethod::CONTACT_QR_INVITE),
        group: "contact",
        screen: Screen::Contacts,
        summary: "Generate a QR-invite payload.",
        param_fields: &[],
    },
    // ── Group ─────────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "group_create",
        rpc_method: Some(A3chatRpcMethod::GROUP_CREATE),
        group: "group",
        screen: Screen::Groups,
        summary: "Create a new group conversation.",
        param_fields: &["name", "description", "is_private"],
    },
    CommandCatalogEntry {
        tauri_name: "group_list",
        rpc_method: Some(A3chatRpcMethod::GROUP_LIST),
        group: "group",
        screen: Screen::Groups,
        summary: "List groups visible to the local user.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "group_get",
        rpc_method: Some(A3chatRpcMethod::GROUP_GET),
        group: "group",
        screen: Screen::Groups,
        summary: "Fetch metadata for a single group.",
        param_fields: &["conversation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_invite",
        rpc_method: Some(A3chatRpcMethod::GROUP_INVITE),
        group: "group",
        screen: Screen::Groups,
        summary: "Invite a user to a group.",
        param_fields: &["conversation_id", "invitee_id", "group_name", "inviter_name"],
    },
    CommandCatalogEntry {
        tauri_name: "group_join",
        rpc_method: Some(A3chatRpcMethod::GROUP_JOIN),
        group: "group",
        screen: Screen::Groups,
        summary: "Accept an invitation.",
        param_fields: &["invitation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_leave",
        rpc_method: Some(A3chatRpcMethod::GROUP_LEAVE),
        group: "group",
        screen: Screen::Groups,
        summary: "Leave a group.",
        param_fields: &["conversation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_dissolve",
        rpc_method: Some(A3chatRpcMethod::GROUP_DISSOLVE),
        group: "group",
        screen: Screen::Groups,
        summary: "Dissolve a group (owner only).",
        param_fields: &["conversation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_transfer_ownership",
        rpc_method: Some(A3chatRpcMethod::GROUP_TRANSFER_OWNERSHIP),
        group: "group",
        screen: Screen::Groups,
        summary: "Transfer ownership to another member.",
        param_fields: &["conversation_id", "new_owner_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_metadata_update",
        rpc_method: Some(A3chatRpcMethod::GROUP_METADATA_UPDATE),
        group: "group",
        screen: Screen::Groups,
        summary: "Update group metadata (title, description, avatar, privacy).",
        param_fields: &["conversation_id", "title", "description", "avatar_url", "is_private"],
    },
    CommandCatalogEntry {
        tauri_name: "group_member_add",
        rpc_method: Some(A3chatRpcMethod::GROUP_MEMBER_ADD),
        group: "group",
        screen: Screen::Groups,
        summary: "Direct-add a member (admin).",
        param_fields: &["conversation_id", "user_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_member_remove",
        rpc_method: Some(A3chatRpcMethod::GROUP_MEMBER_REMOVE),
        group: "group",
        screen: Screen::Groups,
        summary: "Remove a member (admin).",
        param_fields: &["conversation_id", "user_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_member_role",
        rpc_method: Some(A3chatRpcMethod::GROUP_MEMBER_ROLE),
        group: "group",
        screen: Screen::Groups,
        summary: "Promote / demote a member.",
        param_fields: &["conversation_id", "user_id", "role"],
    },
    CommandCatalogEntry {
        tauri_name: "group_member_list",
        rpc_method: Some(A3chatRpcMethod::GROUP_MEMBER_LIST),
        group: "group",
        screen: Screen::Groups,
        summary: "List every member of a group.",
        param_fields: &["conversation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_member_get",
        rpc_method: Some(A3chatRpcMethod::GROUP_MEMBER_GET),
        group: "group",
        screen: Screen::Groups,
        summary: "Fetch a single group member.",
        param_fields: &["conversation_id", "user_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_announcement_set",
        rpc_method: Some(A3chatRpcMethod::GROUP_ANNOUNCEMENT_SET),
        group: "group",
        screen: Screen::Groups,
        summary: "Set a pinned announcement.",
        param_fields: &["conversation_id", "text"],
    },
    CommandCatalogEntry {
        tauri_name: "group_announcement_get",
        rpc_method: Some(A3chatRpcMethod::GROUP_ANNOUNCEMENT_GET),
        group: "group",
        screen: Screen::Groups,
        summary: "Fetch the current pinned announcement.",
        param_fields: &["conversation_id"],
    },
    CommandCatalogEntry {
        tauri_name: "group_announcement_clear",
        rpc_method: Some(A3chatRpcMethod::GROUP_ANNOUNCEMENT_CLEAR),
        group: "group",
        screen: Screen::Groups,
        summary: "Clear the pinned announcement.",
        param_fields: &["conversation_id"],
    },
    // ── Presence ──────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "presence_publish",
        rpc_method: Some(A3chatRpcMethod::PRESENCE_PUBLISH),
        group: "presence",
        screen: Screen::Presence,
        summary: "Publish the local user's presence.",
        param_fields: &["status", "status_message"],
    },
    CommandCatalogEntry {
        tauri_name: "presence_subscribe",
        rpc_method: Some(A3chatRpcMethod::PRESENCE_SUBSCRIBE),
        group: "presence",
        screen: Screen::Presence,
        summary: "Subscribe to remote presence updates.",
        param_fields: &["peers"],
    },
    // ── Moderation ────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "moderation_check_content",
        rpc_method: Some(A3chatRpcMethod::MODERATION_CHECK_CONTENT),
        group: "moderation",
        screen: Screen::Moderation,
        summary: "Run moderation on a piece of text.",
        param_fields: &["text"],
    },
    CommandCatalogEntry {
        tauri_name: "moderation_check_attachment",
        rpc_method: Some(A3chatRpcMethod::MODERATION_CHECK_ATTACHMENT),
        group: "moderation",
        screen: Screen::Moderation,
        summary: "Run moderation on an attachment.",
        param_fields: &["hash", "content_type", "size"],
    },
    CommandCatalogEntry {
        tauri_name: "moderation_list_blocked",
        rpc_method: Some(A3chatRpcMethod::MODERATION_LIST_BLOCKED),
        group: "moderation",
        screen: Screen::Moderation,
        summary: "List the locally blocked content hashes.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "moderation_set_deny_default",
        rpc_method: Some(A3chatRpcMethod::MODERATION_SET_DENY_DEFAULT),
        group: "moderation",
        screen: Screen::Moderation,
        summary: "Toggle deny-by-default for new content.",
        param_fields: &["on"],
    },
    CommandCatalogEntry {
        tauri_name: "moderation_stats",
        rpc_method: Some(A3chatRpcMethod::MODERATION_STATS),
        group: "moderation",
        screen: Screen::Moderation,
        summary: "Return moderation counters.",
        param_fields: &[],
    },
    // ── Media ─────────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "media_health",
        rpc_method: Some(A3chatRpcMethod::MEDIA_HEALTH),
        group: "media",
        screen: Screen::Media,
        summary: "Return the media store health report.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "media_upload_init",
        rpc_method: Some(A3chatRpcMethod::MEDIA_UPLOAD_INIT),
        group: "media",
        screen: Screen::Media,
        summary: "Open a resumable upload session.",
        param_fields: &["mime", "size"],
    },
    CommandCatalogEntry {
        tauri_name: "media_upload_chunk",
        rpc_method: Some(A3chatRpcMethod::MEDIA_UPLOAD_CHUNK),
        group: "media",
        screen: Screen::Media,
        summary: "Upload a chunk of an in-progress upload.",
        param_fields: &["token", "chunk_index", "data_b64"],
    },
    CommandCatalogEntry {
        tauri_name: "media_upload_finalize",
        rpc_method: Some(A3chatRpcMethod::MEDIA_UPLOAD_FINALIZE),
        group: "media",
        screen: Screen::Media,
        summary: "Close a resumable upload.",
        param_fields: &["token"],
    },
    CommandCatalogEntry {
        tauri_name: "media_download_get",
        rpc_method: Some(A3chatRpcMethod::MEDIA_DOWNLOAD_GET),
        group: "media",
        screen: Screen::Media,
        summary: "Resolve a download URL for a blob hash.",
        param_fields: &["hash"],
    },
    // ── Peer feedback / reputation ─────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "peerfeedback_set_trust",
        rpc_method: Some(A3chatRpcMethod::PEERFEEDBACK_SET_TRUST),
        group: "peerfeedback",
        screen: Screen::PeerFeedback,
        summary: "Set the local trust score for a peer.",
        param_fields: &["targetUserId", "score", "reason"],
    },
    CommandCatalogEntry {
        tauri_name: "peerfeedback_clear_trust",
        rpc_method: Some(A3chatRpcMethod::PEERFEEDBACK_CLEAR_TRUST),
        group: "peerfeedback",
        screen: Screen::PeerFeedback,
        summary: "Clear the local trust score for a peer.",
        param_fields: &["targetUserId"],
    },
    CommandCatalogEntry {
        tauri_name: "peerfeedback_file_report",
        rpc_method: Some(A3chatRpcMethod::PEERFEEDBACK_FILE_REPORT),
        group: "peerfeedback",
        screen: Screen::PeerFeedback,
        summary: "File a moderation report against a peer.",
        param_fields: &["targetUserId", "category", "evidence"],
    },
    CommandCatalogEntry {
        tauri_name: "peerfeedback_fused_score",
        rpc_method: Some(A3chatRpcMethod::PEERFEEDBACK_FUSED_SCORE),
        group: "peerfeedback",
        screen: Screen::PeerFeedback,
        summary: "Compute the fused reputation score for a peer.",
        param_fields: &["targetUserId"],
    },
    CommandCatalogEntry {
        tauri_name: "peerfeedback_list_trust",
        rpc_method: Some(A3chatRpcMethod::PEERFEEDBACK_LIST_TRUST),
        group: "peerfeedback",
        screen: Screen::PeerFeedback,
        summary: "List every trust record owned by the calling user.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "peerfeedback_peer_list",
        rpc_method: Some(A3chatRpcMethod::PEERFEEDBACK_PEER_LIST),
        group: "peerfeedback",
        screen: Screen::PeerFeedback,
        summary: "List peers with non-default reputation scores.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "peerfeedback_peer_get",
        rpc_method: Some(A3chatRpcMethod::PEERFEEDBACK_PEER_GET),
        group: "peerfeedback",
        screen: Screen::PeerFeedback,
        summary: "Fetch a single peer's reputation summary.",
        param_fields: &["targetUserId"],
    },
    // ── Audit (UI-only — no direct RPC) ───────────────────────────────
    CommandCatalogEntry {
        tauri_name: "audit_report",
        rpc_method: None,
        group: "audit",
        screen: Screen::Audit,
        summary: "Generate a static audit report of the workspace.",
        param_fields: &[],
    },
    // ── E2E bundle ─────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "e2e_bundle_export",
        rpc_method: Some(A3chatRpcMethod::E2E_BUNDLE_EXPORT),
        group: "e2e",
        screen: Screen::Bundle,
        summary: "Package the local E2E state into a portable bundle.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "e2e_bundle_import",
        rpc_method: Some(A3chatRpcMethod::E2E_BUNDLE_IMPORT),
        group: "e2e",
        screen: Screen::Bundle,
        summary: "Decrypt and merge a bundle into the local store.",
        param_fields: &["bundle"],
    },
    // ── Stream ────────────────────────────────────────────────────────
    CommandCatalogEntry {
        tauri_name: "stream_subscribe",
        rpc_method: Some(A3chatRpcMethod::STREAM_SUBSCRIBE),
        group: "stream",
        screen: Screen::Stream,
        summary: "Subscribe to a topic and return a handle.",
        param_fields: &["topic"],
    },
    CommandCatalogEntry {
        tauri_name: "stream_unsubscribe",
        rpc_method: Some(A3chatRpcMethod::STREAM_UNSUBSCRIBE),
        group: "stream",
        screen: Screen::Stream,
        summary: "Release a previously-acquired handle.",
        param_fields: &["handle_id"],
    },
    CommandCatalogEntry {
        tauri_name: "stream_list",
        rpc_method: Some(A3chatRpcMethod::STREAM_LIST),
        group: "stream",
        screen: Screen::Stream,
        summary: "List active subscriptions.",
        param_fields: &[],
    },
    // ── Moments / 朋友圈 (F-05) ────────────────────────────────────────
    // Each F-05 Tauri command has a 1:1 mapping to an
    // `a3chat.moments.*` RPC method, exposed to the React composer
    // under the `Moments` screen. The audit log uses `tauri_name`
    // + `rpc_method` to trace UI actions back to the daemon
    // handler.
    CommandCatalogEntry {
        tauri_name: "moments_node_info",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_NODE_INFO),
        group: "moments",
        screen: Screen::Moments,
        summary: "Probe the local Moments node identity / schema version.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "moments_post_create",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_POST_CREATE),
        group: "moments",
        screen: Screen::Moments,
        summary: "Publish a new post (gossip + local SQLite).",
        param_fields: &["post"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_post_update",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_POST_UPDATE),
        group: "moments",
        screen: Screen::Moments,
        summary: "Edit an existing post (re-stamps integrity hash).",
        param_fields: &["post"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_post_delete",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_POST_DELETE),
        group: "moments",
        screen: Screen::Moments,
        summary: "Delete a post (bookmark boundaries unaffected).",
        param_fields: &["post_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_post_get",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_POST_GET),
        group: "moments",
        screen: Screen::Moments,
        summary: "Fetch a single post by id (returns `{post: …}`).",
        param_fields: &["post_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_posts_by_user",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_POSTS_BY_USER),
        group: "moments",
        screen: Screen::Moments,
        summary: "List every post authored by the given user.",
        param_fields: &["user_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_timeline",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_TIMELINE),
        group: "moments",
        screen: Screen::Moments,
        summary: "Paginated timeline (viewer_id, scope, cursor).",
        param_fields: &["viewer_id", "scope", "limit", "before_cursor", "before_ts", "author_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_comment_add",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_COMMENT_ADD),
        group: "moments",
        screen: Screen::Moments,
        summary: "Add a comment to a post (moderation-checked).",
        param_fields: &["comment"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_comments_list",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_COMMENTS_LIST),
        group: "moments",
        screen: Screen::Moments,
        summary: "List every comment for a post.",
        param_fields: &["post_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_react",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_REACT),
        group: "moments",
        screen: Screen::Moments,
        summary: "Add or no-op a reaction (idempotent toggle).",
        param_fields: &["reaction"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_reactions_list",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_REACTIONS_LIST),
        group: "moments",
        screen: Screen::Moments,
        summary: "List reactions targeting a given post or comment.",
        param_fields: &["target_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_follow",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_FOLLOW),
        group: "moments",
        screen: Screen::Moments,
        summary: "Follow a user (idempotent).",
        param_fields: &["following_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_unfollow",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_UNFOLLOW),
        group: "moments",
        screen: Screen::Moments,
        summary: "Unfollow a user (idempotent).",
        param_fields: &["following_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_following_list",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_FOLLOWING_LIST),
        group: "moments",
        screen: Screen::Moments,
        summary: "List users the caller (or explicit id) follows.",
        param_fields: &["follower_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_following_check",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_FOLLOWING_CHECK),
        group: "moments",
        screen: Screen::Moments,
        summary: "Test whether `follower_id` follows `following_id`.",
        param_fields: &["follower_id", "following_id"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_verify_post",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_VERIFY_POST),
        group: "moments",
        screen: Screen::Moments,
        summary: "Recompute the integrity hash and compare against the stored one.",
        param_fields: &["post"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_verify_comment",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_VERIFY_COMMENT),
        group: "moments",
        screen: Screen::Moments,
        summary: "Recompute the comment integrity hash and compare.",
        param_fields: &["comment"],
    },
    CommandCatalogEntry {
        tauri_name: "moments_verify_reaction",
        rpc_method: Some(A3chatRpcMethod::MOMENTS_VERIFY_REACTION),
        group: "moments",
        screen: Screen::Moments,
        summary: "Recompute the reaction integrity hash and compare.",
        param_fields: &["reaction"],
    },
    // ── Link bookmarks / favorites (F-08) ──────────────────────────────
    // The desktop UI exposes a dedicated Favorites screen that
    // delegates every write to the daemon's `a3chat.link.bookmark.*`
    // JSON-RPC namespace. Reads are cached client-side; mutations
    // re-fetch on success.
    CommandCatalogEntry {
        tauri_name: "link_bookmark_add",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_ADD),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Add a new URL bookmark (full UpsertLinkBookmarkRequest payload).",
        param_fields: &["request"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_update",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_UPDATE),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Replace an existing bookmark; identity stable across edits.",
        param_fields: &["bookmark_id", "request"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_get",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_GET),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Fetch one bookmark by id.",
        param_fields: &["bookmark_id"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_get_by_url",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_GET_BY_URL),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Fetch the bookmark (if any) stored under a given URL.",
        param_fields: &["url"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_list",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_LIST),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "List bookmarks honouring folder/tag/pinned/archived filters.",
        param_fields: &["filter"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_search",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_SEARCH),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Fuzzy keyword search across title/description/url/tags.",
        param_fields: &["query"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_delete",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_DELETE),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Delete a bookmark by id (irreversible).",
        param_fields: &["bookmark_id"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_set_pinned",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Pin / unpin a bookmark (sticky in the UI list).",
        param_fields: &["bookmark_id", "is_pinned"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_set_archived",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_SET_ARCHIVED),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Archive / restore a bookmark.",
        param_fields: &["bookmark_id", "is_archived"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_touch_visit",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_TOUCH_VISIT),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Record that the user opened the bookmark.",
        param_fields: &["bookmark_id"],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_tags",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_TAGS),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Distinct tag list with per-tag row counts.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_folders",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_FOLDERS),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Folder tree with direct-child counts.",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "link_bookmark_count",
        rpc_method: Some(A3chatRpcMethod::LINK_BOOKMARK_COUNT),
        group: "favorites",
        screen: Screen::Favorites,
        summary: "Aggregate counts (total / pinned / archived).",
        param_fields: &[],
    },
    // ── Health (process-level liveness) ────────────────────────────────
    // Exposed via Tauri so the UI status bar can poll for daemon
    // health (matches the operator-facing `tools.doctor` command
    // conceptually, but stripped of the per-user checks). The
    // backend intercepts both names in `A3chatApp::dispatch` so
    // they don't depend on user initialisation.
    CommandCatalogEntry {
        tauri_name: "healthz",
        rpc_method: Some(A3chatRpcMethod::HEALTHZ),
        group: "session",
        screen: Screen::Chats,
        summary: "Process-level liveness probe (no user context).",
        param_fields: &[],
    },
    CommandCatalogEntry {
        tauri_name: "rpc_health",
        rpc_method: Some(A3chatRpcMethod::RPC_HEALTH),
        group: "session",
        screen: Screen::Chats,
        summary: "Alias of `healthz` for legacy callers.",
        param_fields: &[],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_summary() {
        for c in COMMAND_CATALOG {
            assert!(!c.summary.is_empty(), "{:?} has empty summary", c.tauri_name);
        }
    }

    #[test]
    fn rpc_methods_are_known() {
        for c in COMMAND_CATALOG {
            if let Some(m) = c.rpc_method {
                assert!(
                    A3chatRpcMethod::ALL.contains(&m),
                    "{:?} → unknown rpc {m}",
                    c.tauri_name
                );
            }
        }
    }

    #[test]
    fn tauri_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in COMMAND_CATALOG {
            assert!(seen.insert(c.tauri_name), "duplicate tauri_name {:?}", c.tauri_name);
        }
    }

    #[test]
    fn covers_all_rpc_methods() {
        let covered: std::collections::HashSet<&'static str> = COMMAND_CATALOG
            .iter()
            .filter_map(|c| c.rpc_method)
            .collect();
        for m in A3chatRpcMethod::ALL {
            assert!(
                covered.contains(m),
                "rpc method {m} is not bound to any Tauri command"
            );
        }
    }

    #[test]
    fn covers_all_screens() {
        let mut screens: std::collections::HashSet<Screen> = COMMAND_CATALOG
            .iter()
            .map(|c| c.screen)
            .collect();
        for s in Screen::ALL {
            assert!(screens.remove(s), "screen {s:?} has no commands");
        }
    }

    #[test]
    fn catalog_size_is_above_minimum() {
        // At least 50 commands — covers all 52 RPCs + 8 session ops ≥ 60.
        assert!(COMMAND_CATALOG.len() >= 60, "{}", COMMAND_CATALOG.len());
    }
}
