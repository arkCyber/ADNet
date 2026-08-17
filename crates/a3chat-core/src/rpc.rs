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
    pub const CHAT_MESSAGE_SEND: &'static str = "a3chat.chat.message.send";
    pub const CHAT_MESSAGE_RECALL: &'static str = "a3chat.chat.message.recall";
    pub const CHAT_MESSAGE_ACK: &'static str = "a3chat.chat.message.ack";
    pub const CHAT_MESSAGE_EDIT: &'static str = "a3chat.chat.message.edit";
    pub const CHAT_MESSAGE_DELETE: &'static str = "a3chat.chat.message.delete";
    pub const CHAT_SEARCH: &'static str = "a3chat.chat.search";
    pub const CHAT_TYPING: &'static str = "a3chat.chat.typing";

    // Contacts
    pub const CONTACT_LIST: &'static str = "a3chat.contact.list";
    pub const CONTACT_ADD_REQUEST: &'static str = "a3chat.contact.add_request";
    pub const CONTACT_ACCEPT_REQUEST: &'static str = "a3chat.contact.accept_request";
    pub const CONTACT_BLOCK: &'static str = "a3chat.contact.block";
    pub const CONTACT_UNBLOCK: &'static str = "a3chat.contact.unblock";
    pub const CONTACT_QR_INVITE: &'static str = "a3chat.contact.qr_invite";

    // Groups
    pub const GROUP_CREATE: &'static str = "a3chat.group.create";
    pub const GROUP_INVITE: &'static str = "a3chat.group.invite";
    pub const GROUP_JOIN: &'static str = "a3chat.group.join";
    pub const GROUP_MEMBER_ADD: &'static str = "a3chat.group.member.add";
    pub const GROUP_MEMBER_REMOVE: &'static str = "a3chat.group.member.remove";
    pub const GROUP_MEMBER_ROLE: &'static str = "a3chat.group.member.role";
    pub const GROUP_ANNOUNCEMENT_SET: &'static str = "a3chat.group.announcement.set";

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
    pub const MEDIA_UPLOAD_INIT: &'static str = "a3chat.media.upload_init";
    pub const MEDIA_UPLOAD_CHUNK: &'static str = "a3chat.media.upload_chunk";
    pub const MEDIA_UPLOAD_FINALIZE: &'static str = "a3chat.media.upload_finalize";
    pub const MEDIA_DOWNLOAD_GET: &'static str = "a3chat.media.download_get";
    pub const E2E_BUNDLE_EXPORT: &'static str = "a3chat.e2e.bundle.export";
    pub const E2E_BUNDLE_IMPORT: &'static str = "a3chat.e2e.bundle.import";

    // Stream (SSE)
    pub const STREAM_SUBSCRIBE: &'static str = "a3chat.stream.subscribe";

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

    /// Stable list of every method name. Useful for discovery.
    pub const ALL: &'static [&'static str] = &[
        Self::CHAT_CONVERSATION_LIST,
        Self::CHAT_CONVERSATION_OPEN,
        Self::CHAT_MESSAGE_SEND,
        Self::CHAT_MESSAGE_RECALL,
        Self::CHAT_MESSAGE_ACK,
        Self::CHAT_MESSAGE_EDIT,
        Self::CHAT_MESSAGE_DELETE,
        Self::CHAT_SEARCH,
        Self::CHAT_TYPING,
        Self::CONTACT_LIST,
        Self::CONTACT_ADD_REQUEST,
        Self::CONTACT_ACCEPT_REQUEST,
        Self::CONTACT_BLOCK,
        Self::CONTACT_UNBLOCK,
        Self::CONTACT_QR_INVITE,
        Self::GROUP_CREATE,
        Self::GROUP_INVITE,
        Self::GROUP_JOIN,
        Self::GROUP_MEMBER_ADD,
        Self::GROUP_MEMBER_REMOVE,
        Self::GROUP_MEMBER_ROLE,
        Self::GROUP_ANNOUNCEMENT_SET,
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
        Self::MEDIA_UPLOAD_INIT,
        Self::MEDIA_UPLOAD_CHUNK,
        Self::MEDIA_UPLOAD_FINALIZE,
        Self::MEDIA_DOWNLOAD_GET,
        Self::E2E_BUNDLE_EXPORT,
        Self::E2E_BUNDLE_IMPORT,
        Self::STREAM_SUBSCRIBE,
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
        // Bump floor when adding a new namespace. As of F-08 the
        // floor covers:
        //  - chat / contact / group / sync / presence (~24)
        //  - profile (~10)
        //  - media / e2e / stream (~6)
        //  - link bookmarks (~13)
        // Total ≈ 58.
        assert!(A3chatRpcMethod::ALL.len() >= 50);
    }
}
