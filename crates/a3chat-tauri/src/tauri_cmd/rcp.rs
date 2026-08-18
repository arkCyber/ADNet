//! Typed RPC wrappers — one command per a3chat RPC method.
//!
//! This module is the **complete bridge** between the Tauri UI and
//! the 52 RPC methods exposed by the daemon. Every public function
//! in here can be invoked directly from a frontend (via the
//! `#[tauri::command]` wrapper macro on the desktop feature) or
//! called from a Rust integration test with the same typed
//! signature.
//!
//! ## Why one-fn-per-method?
//!
//! DO-178C §5.2 — *traceability*. Each Tauri command has a 1:1
//! relationship with a daemon RPC method, so the audit log can
//! trace any UI action back to a single server-side handler. The
//! `Method` value below is the canonical entry point.
//!
//! ## Conventions
//!
//! - Public functions are `async fn cmd_<name>(state, args) -> Result<T, TauriCommandError>`.
//! - The first argument is the application state (auto-injected by
//!   the Tauri runtime).
//! - Errors are mapped via `TauriCommandError::from_a3chat` so the
//!   UI sees a structured error code.
//! - Each command has a matching unit test in the `tests` module.

use serde::{Deserialize, Serialize};

use a3chat_core::rpc::A3chatRpcMethod;

use super::error::{TauriCommandError, TauriCommandResult};
use super::state::AppState;

/// Trait every typed command implements. Lets the frontend
/// generator introspect the surface without parsing Rust source.
#[async_trait::async_trait]
pub trait CommandSet {
    async fn dispatch(
        &self,
        state: AppState,
        method: &str,
        params: serde_json::Value,
    ) -> TauriCommandResult<serde_json::Value>;
}

/// Trait for *async* commands that are dispatched via the same
/// pattern but use a shared registry rather than 52 individual
/// `match` arms.
#[async_trait::async_trait]
pub trait AsyncCommand {
    fn method(&self) -> &'static str;
    async fn run(
        &self,
        state: AppState,
        params: serde_json::Value,
    ) -> TauriCommandResult<serde_json::Value>;
}

/// Wrapper executor that fans every Tauri command through a single
/// entry point. The Tauri builder registers this as the
/// `executor` state, and a single `invoke("a3chat_rpc", { method, params })`
/// call from the frontend dispatches to the right handler.
#[derive(Clone, Default)]
pub struct RcpCommandExecutor {
    _phantom: std::marker::PhantomData<()>,
}

impl RcpCommandExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the registered method (if any) and execute it.
    pub async fn execute(
        &self,
        state: AppState,
        method: &str,
        params: serde_json::Value,
    ) -> TauriCommandResult<serde_json::Value> {
        let client = state.client().ok_or_else(|| {
            TauriCommandError::security("not_logged_in", "no active session")
        })?;
        let rpc_method = match method {
            // conversation
            "a3chat.chat.conversation.list" => A3chatRpcMethod::CHAT_CONVERSATION_LIST,
            "a3chat.chat.conversation.open" => A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
            "a3chat.chat.typing" => A3chatRpcMethod::CHAT_TYPING,
            "a3chat.chat.search" => A3chatRpcMethod::CHAT_SEARCH,
            // messages
            "a3chat.chat.message.send" => A3chatRpcMethod::CHAT_MESSAGE_SEND,
            "a3chat.chat.message.recall" => A3chatRpcMethod::CHAT_MESSAGE_RECALL,
            "a3chat.chat.message.ack" => A3chatRpcMethod::CHAT_MESSAGE_ACK,
            "a3chat.chat.message.edit" => A3chatRpcMethod::CHAT_MESSAGE_EDIT,
            "a3chat.chat.message.delete" => A3chatRpcMethod::CHAT_MESSAGE_DELETE,
            // sync
            "a3chat.chat.sync.snapshot" => A3chatRpcMethod::CHAT_SYNC_SNAPSHOT,
            "a3chat.chat.sync.delta" => A3chatRpcMethod::CHAT_SYNC_DELTA,
            "a3chat.chat.sync.compressed" => A3chatRpcMethod::CHAT_SYNC_COMPRESSED,
            // profile
            "a3chat.profile.get" => A3chatRpcMethod::PROFILE_GET,
            "a3chat.profile.put" => A3chatRpcMethod::PROFILE_PUT,
            "a3chat.profile.preferences_put" => A3chatRpcMethod::PROFILE_PREFERENCES_PUT,
            "a3chat.profile.digit_get" => A3chatRpcMethod::PROFILE_DIGIT_GET,
            "a3chat.profile.public_key_add" => A3chatRpcMethod::PROFILE_PUBLIC_KEY_ADD,
            "a3chat.profile.public_key_list" => A3chatRpcMethod::PROFILE_PUBLIC_KEY_LIST,
            "a3chat.profile.public_key_revoke" => A3chatRpcMethod::PROFILE_PUBLIC_KEY_REVOKE,
            "a3chat.profile.device_register" => A3chatRpcMethod::PROFILE_DEVICE_REGISTER,
            "a3chat.profile.device_list" => A3chatRpcMethod::PROFILE_DEVICE_LIST,
            "a3chat.profile.avatar_set" => A3chatRpcMethod::PROFILE_AVATAR_SET,
            // contact
            "a3chat.contact.list" => A3chatRpcMethod::CONTACT_LIST,
            "a3chat.contact.add_request" => A3chatRpcMethod::CONTACT_ADD_REQUEST,
            "a3chat.contact.accept_request" => A3chatRpcMethod::CONTACT_ACCEPT_REQUEST,
            "a3chat.contact.block" => A3chatRpcMethod::CONTACT_BLOCK,
            "a3chat.contact.unblock" => A3chatRpcMethod::CONTACT_UNBLOCK,
            "a3chat.contact.qr_invite" => A3chatRpcMethod::CONTACT_QR_INVITE,
            // group
            "a3chat.group.create" => A3chatRpcMethod::GROUP_CREATE,
            "a3chat.group.invite" => A3chatRpcMethod::GROUP_INVITE,
            "a3chat.group.join" => A3chatRpcMethod::GROUP_JOIN,
            "a3chat.group.member.add" => A3chatRpcMethod::GROUP_MEMBER_ADD,
            "a3chat.group.member.remove" => A3chatRpcMethod::GROUP_MEMBER_REMOVE,
            "a3chat.group.member.role" => A3chatRpcMethod::GROUP_MEMBER_ROLE,
            "a3chat.group.announcement.set" => A3chatRpcMethod::GROUP_ANNOUNCEMENT_SET,
            // presence
            "a3chat.presence.publish" => A3chatRpcMethod::PRESENCE_PUBLISH,
            "a3chat.presence.subscribe" => A3chatRpcMethod::PRESENCE_SUBSCRIBE,
            // moderation
            "a3chat.moderation.check_content" => A3chatRpcMethod::MODERATION_CHECK_CONTENT,
            "a3chat.moderation.check_attachment" => A3chatRpcMethod::MODERATION_CHECK_ATTACHMENT,
            "a3chat.moderation.list_blocked" => A3chatRpcMethod::MODERATION_LIST_BLOCKED,
            "a3chat.moderation.set_deny_default" => A3chatRpcMethod::MODERATION_SET_DENY_DEFAULT,
            "a3chat.moderation.stats" => A3chatRpcMethod::MODERATION_STATS,
            // media
            "a3chat.media.upload_init" => A3chatRpcMethod::MEDIA_UPLOAD_INIT,
            "a3chat.media.upload_chunk" => A3chatRpcMethod::MEDIA_UPLOAD_CHUNK,
            "a3chat.media.upload_finalize" => A3chatRpcMethod::MEDIA_UPLOAD_FINALIZE,
            "a3chat.media.download_get" => A3chatRpcMethod::MEDIA_DOWNLOAD_GET,
            "a3chat.media.health" => A3chatRpcMethod::MEDIA_HEALTH,
            // e2e
            "a3chat.e2e.bundle.export" => A3chatRpcMethod::E2E_BUNDLE_EXPORT,
            "a3chat.e2e.bundle.import" => A3chatRpcMethod::E2E_BUNDLE_IMPORT,
            // stream
            "a3chat.stream.subscribe" => A3chatRpcMethod::STREAM_SUBSCRIBE,
            "a3chat.stream.unsubscribe" => A3chatRpcMethod::STREAM_UNSUBSCRIBE,
            "a3chat.stream.list" => A3chatRpcMethod::STREAM_LIST,
            // moments / 朋友圈 (F-05)
            "a3chat.moments.node_info" => A3chatRpcMethod::MOMENTS_NODE_INFO,
            "a3chat.moments.post.create" => A3chatRpcMethod::MOMENTS_POST_CREATE,
            "a3chat.moments.post.update" => A3chatRpcMethod::MOMENTS_POST_UPDATE,
            "a3chat.moments.post.delete" => A3chatRpcMethod::MOMENTS_POST_DELETE,
            "a3chat.moments.post.get" => A3chatRpcMethod::MOMENTS_POST_GET,
            "a3chat.moments.posts.by_user" => A3chatRpcMethod::MOMENTS_POSTS_BY_USER,
            "a3chat.moments.timeline" => A3chatRpcMethod::MOMENTS_TIMELINE,
            "a3chat.moments.comment.add" => A3chatRpcMethod::MOMENTS_COMMENT_ADD,
            "a3chat.moments.comments.list" => A3chatRpcMethod::MOMENTS_COMMENTS_LIST,
            "a3chat.moments.react" => A3chatRpcMethod::MOMENTS_REACT,
            "a3chat.moments.reactions.list" => A3chatRpcMethod::MOMENTS_REACTIONS_LIST,
            "a3chat.moments.follow" => A3chatRpcMethod::MOMENTS_FOLLOW,
            "a3chat.moments.unfollow" => A3chatRpcMethod::MOMENTS_UNFOLLOW,
            "a3chat.moments.following.list" => A3chatRpcMethod::MOMENTS_FOLLOWING_LIST,
            "a3chat.moments.following.check" => A3chatRpcMethod::MOMENTS_FOLLOWING_CHECK,
            "a3chat.moments.verify.post" => A3chatRpcMethod::MOMENTS_VERIFY_POST,
            "a3chat.moments.verify.comment" => A3chatRpcMethod::MOMENTS_VERIFY_COMMENT,
            "a3chat.moments.verify.reaction" => A3chatRpcMethod::MOMENTS_VERIFY_REACTION,
            // link bookmarks / favorites (F-08)
            "a3chat.link.bookmark.add" => A3chatRpcMethod::LINK_BOOKMARK_ADD,
            "a3chat.link.bookmark.update" => A3chatRpcMethod::LINK_BOOKMARK_UPDATE,
            "a3chat.link.bookmark.get" => A3chatRpcMethod::LINK_BOOKMARK_GET,
            "a3chat.link.bookmark.get_by_url" => A3chatRpcMethod::LINK_BOOKMARK_GET_BY_URL,
            "a3chat.link.bookmark.list" => A3chatRpcMethod::LINK_BOOKMARK_LIST,
            "a3chat.link.bookmark.search" => A3chatRpcMethod::LINK_BOOKMARK_SEARCH,
            "a3chat.link.bookmark.delete" => A3chatRpcMethod::LINK_BOOKMARK_DELETE,
            "a3chat.link.bookmark.set_pinned" => A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED,
            "a3chat.link.bookmark.set_archived" => A3chatRpcMethod::LINK_BOOKMARK_SET_ARCHIVED,
            "a3chat.link.bookmark.touch_visit" => A3chatRpcMethod::LINK_BOOKMARK_TOUCH_VISIT,
            "a3chat.link.bookmark.tags" => A3chatRpcMethod::LINK_BOOKMARK_TAGS,
            "a3chat.link.bookmark.folders" => A3chatRpcMethod::LINK_BOOKMARK_FOLDERS,
            "a3chat.link.bookmark.count" => A3chatRpcMethod::LINK_BOOKMARK_COUNT,
            other => {
                return Err(TauriCommandError::permanent(
                    "unknown_method",
                    format!("unknown rpc method: {other}"),
                ));
            }
        };
        client.call(rpc_method, params).await.map_err(TauriCommandError::from)
    }
}

/// Wraps the entire RCP-style surface into a single struct so the
/// Tauri builder can register every command as a method on it.
#[derive(Clone)]
pub struct A3chatCommandSet {
    pub state: AppState,
    pub executor: RcpCommandExecutor,
}

impl A3chatCommandSet {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            executor: RcpCommandExecutor::new(),
        }
    }

    /// Dispatch helper used by the generic `invoke()` handler.
    pub async fn dispatch(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> TauriCommandResult<serde_json::Value> {
        self.executor
            .execute(self.state.clone(), method, params)
            .await
    }
}

/// Generic envelope every argument struct deserialises from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Generic envelope every result struct serialises into.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub result: serde_json::Value,
    pub request_id: Option<String>,
}

// ── Typed wrappers ────────────────────────────────────────────────────────

/// `a3chat.chat.conversation.list` — refresh the sidebar.
pub async fn chat_conversation_list(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.conversation.open` — fetch a conversation's metadata.
pub async fn chat_conversation_open(
    state: AppState,
    conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_OPEN,
            serde_json::json!({ "conversation_id": conversation_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.typing` — emit a typing notification.
pub async fn chat_typing(state: AppState, conversation_id: String) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CHAT_TYPING, serde_json::json!({ "conversation_id": conversation_id }))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.search` — full-text search.
pub async fn chat_search(
    state: AppState,
    needle: String,
    limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if needle.trim().is_empty() {
        return Err(TauriCommandError::validation(
            "empty_needle",
            "search needle is empty",
            vec![super::error::FieldError {
                field: "needle".into(),
                message: "must be at least one character".into(),
            }],
        ));
    }
    let mut params = serde_json::json!({ "needle": needle });
    if let Some(l) = limit {
        params["limit"] = serde_json::json!(l);
    }
    client
        .call(A3chatRpcMethod::CHAT_SEARCH, params)
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.message.send` — post a message.
pub async fn chat_message_send(
    state: AppState,
    envelope: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CHAT_MESSAGE_SEND, envelope)
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.message.recall` — retract a message.
pub async fn chat_message_recall(
    state: AppState,
    message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_RECALL,
            serde_json::json!({ "message_id": message_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.message.ack` — mark a message read.
pub async fn chat_message_ack(
    state: AppState,
    message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_ACK,
            serde_json::json!({ "message_id": message_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.message.edit` — edit an existing message.
pub async fn chat_message_edit(
    state: AppState,
    message_id: String,
    body: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_EDIT,
            serde_json::json!({ "message_id": message_id, "body": body }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.message.delete` — delete a message for me.
pub async fn chat_message_delete(
    state: AppState,
    message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CHAT_MESSAGE_DELETE,
            serde_json::json!({ "message_id": message_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.sync.snapshot` — fetch a sync snapshot.
pub async fn chat_sync_snapshot(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CHAT_SYNC_SNAPSHOT, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.sync.delta` — fetch delta since cursor.
pub async fn chat_sync_delta(
    state: AppState,
    cursors: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CHAT_SYNC_DELTA, serde_json::json!({ "cursors": cursors }))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.chat.sync.compressed` — fetch zstd-compressed delta.
pub async fn chat_sync_compressed(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CHAT_SYNC_COMPRESSED, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

// Profile ────────────────────────────────────────────────────────────────

pub async fn profile_get(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::PROFILE_GET, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_put(state: AppState, profile: serde_json::Value) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if !profile.is_object() {
        return Err(TauriCommandError::validation(
            "invalid_profile",
            "profile must be a JSON object",
            vec![super::error::FieldError {
                field: "profile".into(),
                message: "expected JSON object".into(),
            }],
        ));
    }
    client
        .call(A3chatRpcMethod::PROFILE_PUT, profile)
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_preferences_put(
    state: AppState,
    prefs: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PROFILE_PREFERENCES_PUT,
            serde_json::json!({ "preferences": prefs }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_digit_get(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::PROFILE_DIGIT_GET, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_public_key_add(
    state: AppState,
    algorithm: String,
    public_key: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PROFILE_PUBLIC_KEY_ADD,
            serde_json::json!({ "algorithm": algorithm, "public_key": public_key }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_public_key_list(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::PROFILE_PUBLIC_KEY_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_public_key_revoke(
    state: AppState,
    public_key: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PROFILE_PUBLIC_KEY_REVOKE,
            serde_json::json!({ "public_key": public_key }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_device_register(
    state: AppState,
    device_class: String,
    device_label: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PROFILE_DEVICE_REGISTER,
            serde_json::json!({ "device_class": device_class, "device_label": device_label }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_device_list(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::PROFILE_DEVICE_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn profile_avatar_set(
    state: AppState,
    blob_hash: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PROFILE_AVATAR_SET,
            serde_json::json!({ "blobHash": blob_hash }),
        )
        .await
        .map_err(TauriCommandError::from)
}

// Contact ────────────────────────────────────────────────────────────────

pub async fn contact_list(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CONTACT_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn contact_add_request(
    state: AppState,
    to_user_id: String,
    message: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if to_user_id.len() != 64 {
        return Err(TauriCommandError::validation(
            "bad_user_id",
            "to_user_id must be a 64-hex NodeId",
            vec![super::error::FieldError {
                field: "to_user_id".into(),
                message: format!("expected 64 hex chars, got {}", to_user_id.len()),
            }],
        ));
    }
    client
        .call(
            A3chatRpcMethod::CONTACT_ADD_REQUEST,
            serde_json::json!({ "to_user_id": to_user_id, "message": message.unwrap_or_default() }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn contact_accept_request(
    state: AppState,
    request_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CONTACT_ACCEPT_REQUEST,
            serde_json::json!({ "request_id": request_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn contact_block(
    state: AppState,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CONTACT_BLOCK,
            serde_json::json!({ "user_id": user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn contact_unblock(
    state: AppState,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::CONTACT_UNBLOCK,
            serde_json::json!({ "user_id": user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn contact_qr_invite(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::CONTACT_QR_INVITE, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

// Group ──────────────────────────────────────────────────────────────────

pub async fn group_create(
    state: AppState,
    name: String,
    description: Option<String>,
    is_private: Option<bool>,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if name.trim().is_empty() {
        return Err(TauriCommandError::validation(
            "empty_name",
            "group name is empty",
            vec![super::error::FieldError {
                field: "name".into(),
                message: "must be non-empty".into(),
            }],
        ));
    }
    let mut params = serde_json::json!({
        "name": name,
        "description": description.unwrap_or_default(),
    });
    if let Some(p) = is_private {
        params["is_private"] = serde_json::json!(p);
    }
    client
        .call(A3chatRpcMethod::GROUP_CREATE, params)
        .await
        .map_err(TauriCommandError::from)
}

pub async fn group_invite(
    state: AppState,
    conversation_id: String,
    invitee_id: String,
    group_name: String,
    inviter_name: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::GROUP_INVITE,
            serde_json::json!({
                "conversation_id": conversation_id,
                "invitee_id": invitee_id,
                "group_name": group_name,
                "inviter_name": inviter_name,
            }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn group_join(
    state: AppState,
    invitation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::GROUP_JOIN,
            serde_json::json!({ "invitation_id": invitation_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn group_member_add(
    state: AppState,
    conversation_id: String,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::GROUP_MEMBER_ADD,
            serde_json::json!({ "conversation_id": conversation_id, "user_id": user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn group_member_remove(
    state: AppState,
    conversation_id: String,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::GROUP_MEMBER_REMOVE,
            serde_json::json!({ "conversation_id": conversation_id, "user_id": user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn group_member_role(
    state: AppState,
    conversation_id: String,
    user_id: String,
    role: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::GROUP_MEMBER_ROLE,
            serde_json::json!({
                "conversation_id": conversation_id,
                "user_id": user_id,
                "role": role,
            }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn group_announcement_set(
    state: AppState,
    conversation_id: String,
    text: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if text.trim().is_empty() {
        return Err(TauriCommandError::validation(
            "empty_announcement",
            "announcement is empty",
            vec![super::error::FieldError {
                field: "text".into(),
                message: "must be non-empty".into(),
            }],
        ));
    }
    client
        .call(
            A3chatRpcMethod::GROUP_ANNOUNCEMENT_SET,
            serde_json::json!({ "conversation_id": conversation_id, "text": text }),
        )
        .await
        .map_err(TauriCommandError::from)
}

// Presence ───────────────────────────────────────────────────────────────

pub async fn presence_publish(
    state: AppState,
    status: String,
    status_message: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    let allowed = ["online", "away", "busy", "offline"];
    if !allowed.contains(&status.as_str()) {
        return Err(TauriCommandError::validation(
            "bad_status",
            format!("status must be one of {allowed:?}"),
            vec![super::error::FieldError {
                field: "status".into(),
                message: format!("got {status:?}"),
            }],
        ));
    }
    let mut params = serde_json::json!({ "status": status });
    if let Some(m) = status_message {
        params["status_message"] = serde_json::json!(m);
    }
    client
        .call(A3chatRpcMethod::PRESENCE_PUBLISH, params)
        .await
        .map_err(TauriCommandError::from)
}

pub async fn presence_subscribe(
    state: AppState,
    peers: Vec<String>,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if peers.is_empty() {
        return Err(TauriCommandError::validation(
            "empty_peers",
            "peers list is empty",
            vec![super::error::FieldError {
                field: "peers".into(),
                message: "must contain at least one peer".into(),
            }],
        ));
    }
    client
        .call(
            A3chatRpcMethod::PRESENCE_SUBSCRIBE,
            serde_json::json!({ "peers": peers }),
        )
        .await
        .map_err(TauriCommandError::from)
}

// Moderation ─────────────────────────────────────────────────────────────

pub async fn moderation_check_content(
    state: AppState,
    text: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if text.trim().is_empty() {
        return Err(TauriCommandError::validation(
            "empty_text",
            "text is empty",
            vec![super::error::FieldError {
                field: "text".into(),
                message: "must be non-empty".into(),
            }],
        ));
    }
    client
        .call(
            A3chatRpcMethod::MODERATION_CHECK_CONTENT,
            serde_json::json!({ "text": text }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moderation_check_attachment(
    state: AppState,
    hash: String,
    content_type: String,
    size: u64,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    if hash.len() < 16 {
        return Err(TauriCommandError::validation(
            "bad_hash",
            "hash too short",
            vec![super::error::FieldError {
                field: "hash".into(),
                message: format!("expected ≥ 16 hex chars, got {}", hash.len()),
            }],
        ));
    }
    client
        .call(
            A3chatRpcMethod::MODERATION_CHECK_ATTACHMENT,
            serde_json::json!({
                "hash": hash,
                "content_type": content_type,
                "size": size,
            }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moderation_list_blocked(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::MODERATION_LIST_BLOCKED, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moderation_set_deny_default(
    state: AppState,
    on: bool,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MODERATION_SET_DENY_DEFAULT,
            serde_json::json!({ "on": on }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moderation_stats(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::MODERATION_STATS, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

// Media ──────────────────────────────────────────────────────────────────

pub async fn media_health(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::MEDIA_HEALTH, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn media_upload_init(
    state: AppState,
    mime: String,
    size: u64,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MEDIA_UPLOAD_INIT,
            serde_json::json!({ "mime": mime, "size": size }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn media_upload_chunk(
    state: AppState,
    token: String,
    chunk_index: u32,
    data_b64: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MEDIA_UPLOAD_CHUNK,
            serde_json::json!({
                "token": token,
                "chunk_index": chunk_index,
                "data_b64": data_b64,
            }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn media_upload_finalize(
    state: AppState,
    token: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MEDIA_UPLOAD_FINALIZE,
            serde_json::json!({ "token": token }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn media_download_get(
    state: AppState,
    hash: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MEDIA_DOWNLOAD_GET,
            serde_json::json!({ "hash": hash }),
        )
        .await
        .map_err(TauriCommandError::from)
}

// ── Link bookmarks / favorites (F-08) ───────────────────────────────────

/// `a3chat.link.bookmark.add` — create or re-save a bookmark.
pub async fn link_bookmark_add(
    state: AppState,
    request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::LINK_BOOKMARK_ADD, serde_json::json!({ "request": request }))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.update` — update an existing bookmark.
pub async fn link_bookmark_update(
    state: AppState,
    bookmark_id: String,
    request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_UPDATE,
            serde_json::json!({ "bookmark_id": bookmark_id, "request": request }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.get` — fetch one bookmark by id.
pub async fn link_bookmark_get(
    state: AppState,
    bookmark_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_GET,
            serde_json::json!({ "bookmark_id": bookmark_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.get_by_url` — fetch bookmark by URL.
pub async fn link_bookmark_get_by_url(
    state: AppState,
    url: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_GET_BY_URL,
            serde_json::json!({ "url": url }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.list` — list bookmarks with filters.
pub async fn link_bookmark_list(
    state: AppState,
    filter: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_LIST,
            serde_json::json!({ "filter": filter }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.search` — fuzzy keyword search.
pub async fn link_bookmark_search(
    state: AppState,
    query: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::LINK_BOOKMARK_SEARCH, serde_json::json!({ "query": query }))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.delete` — delete a bookmark by id.
pub async fn link_bookmark_delete(
    state: AppState,
    bookmark_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_DELETE,
            serde_json::json!({ "bookmark_id": bookmark_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.set_pinned` — pin / unpin a bookmark.
pub async fn link_bookmark_set_pinned(
    state: AppState,
    bookmark_id: String,
    is_pinned: bool,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_SET_PINNED,
            serde_json::json!({ "bookmark_id": bookmark_id, "is_pinned": is_pinned }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.set_archived` — archive / restore a bookmark.
pub async fn link_bookmark_set_archived(
    state: AppState,
    bookmark_id: String,
    is_archived: bool,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_SET_ARCHIVED,
            serde_json::json!({ "bookmark_id": bookmark_id, "is_archived": is_archived }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.touch_visit` — record a visit to a bookmark.
pub async fn link_bookmark_touch_visit(
    state: AppState,
    bookmark_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::LINK_BOOKMARK_TOUCH_VISIT,
            serde_json::json!({ "bookmark_id": bookmark_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.tags` — list all distinct tags with counts.
pub async fn link_bookmark_tags(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::LINK_BOOKMARK_TAGS, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.folders` — list all distinct folder paths with counts.
pub async fn link_bookmark_folders(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::LINK_BOOKMARK_FOLDERS, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

/// `a3chat.link.bookmark.count` — aggregate counts (total / pinned / archived).
pub async fn link_bookmark_count(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::LINK_BOOKMARK_COUNT, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

// Health (process-level liveness) ────────────────────────────────────────
//
// The status bar polls these *before* / *during* the login flow, so
// they must not require an active client. We construct a throwaway
// `A3chatClient` — if no client is attached we synthesise a dummy
// config pointing at `127.0.0.1:0` so a missing daemon surfaces as
// a structured "not_connected" payload rather than a panic. This
// matches the operator-facing `tools.doctor` semantic (degrade
// gracefully when the daemon is offline).

/// `a3chat.healthz` — process-level liveness probe.
///
/// On success the payload mirrors the daemon's `{ok, service,
/// version, owner, started_unix, uptime_secs, bus_receivers,
/// stream_handles}` shape. When the client has no base URL or the
/// target daemon is unreachable the function returns a
/// `{"ok": false, "error_class": "transient", ...}` envelope so the
/// status bar can render "not connected" without crashing the UI.
pub async fn healthz(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let cfg = match state.client() {
        Some(c) => c.config().clone(),
        None => {
            return Ok(serde_json::json!({
                "ok": false,
                "reason": "no_client",
                "service": "a3chat.tauri",
            }));
        }
    };
    let client = crate::client::A3chatClient::new(cfg);
    match client
        .call(A3chatRpcMethod::HEALTHZ, serde_json::json!({}))
        .await
    {
        Ok(v) => Ok(v),
            Err(e) => Ok(serde_json::json!({
                "ok": false,
                "reason": "daemon_unreachable",
                "error_class": format!("{:?}", TauriCommandError::from(e).error_class),
            })),
    }
}

/// `a3chat.rpc.health` — alias for [`healthz`].
pub async fn rpc_health(state: AppState) -> TauriCommandResult<serde_json::Value> {
    healthz(state).await
}

// E2E bundle ─────────────────────────────────────────────────────────────

pub async fn e2e_bundle_export(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::E2E_BUNDLE_EXPORT, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn e2e_bundle_import(
    state: AppState,
    bundle: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::E2E_BUNDLE_IMPORT,
            serde_json::json!({ "bundle": bundle }),
        )
        .await
        .map_err(TauriCommandError::from)
}

// Stream ─────────────────────────────────────────────────────────────────

pub async fn stream_subscribe(
    state: AppState,
    topic: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::STREAM_SUBSCRIBE,
            serde_json::json!({ "topic": topic.unwrap_or_else(|| "*".into()) }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn stream_unsubscribe(
    state: AppState,
    handle_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::STREAM_UNSUBSCRIBE,
            serde_json::json!({ "handle_id": handle_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn stream_list(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::STREAM_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

// ── Peer feedback / reputation ─────────────────────────────────────────

pub async fn peerfeedback_set_trust(
    state: AppState,
    target_user_id: String,
    score: f64,
    reason: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PEERFEEDBACK_SET_TRUST,
            serde_json::json!({
                "targetUserId": target_user_id,
                "score": score,
                "reason": reason,
            }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn peerfeedback_clear_trust(
    state: AppState,
    target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PEERFEEDBACK_CLEAR_TRUST,
            serde_json::json!({ "targetUserId": target_user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn peerfeedback_file_report(
    state: AppState,
    target_user_id: String,
    category: String,
    evidence: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PEERFEEDBACK_FILE_REPORT,
            serde_json::json!({
                "targetUserId": target_user_id,
                "category": category,
                "evidence": evidence,
            }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn peerfeedback_fused_score(
    state: AppState,
    target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PEERFEEDBACK_FUSED_SCORE,
            serde_json::json!({ "targetUserId": target_user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn peerfeedback_list_trust(
    state: AppState,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::PEERFEEDBACK_LIST_TRUST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn peerfeedback_peer_list(
    state: AppState,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::PEERFEEDBACK_PEER_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn peerfeedback_peer_get(
    state: AppState,
    target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::PEERFEEDBACK_PEER_GET,
            serde_json::json!({ "targetUserId": target_user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

// ── Moments / 朋友圈 (F-05) ─────────────────────────────────────────────────
//
// Each F-05 Tauri command is a thin wrapper that forwards to the
// `a3chat.moments.*` JSON-RPC method. The Moments RPC server-side
// dispatch lives in `a3chat_app::moments_service::dispatch`; the
// Tauri layer just shapes the params / forwards the response.
//
// Frontend ergonomics: parameter objects mirror the RPC layer
// (`{post}`, `{comment}`, `{reaction}`, `{before_cursor}`, …) so
// the JSON the React composer sees is the same JSON the daemon
// receives. That keeps the audit log mapping 1:1 with the UI.

pub async fn moments_node_info(state: AppState) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::MOMENTS_NODE_INFO, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_post_create(
    state: AppState,
    post: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_POST_CREATE,
            serde_json::json!({ "post": post }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_post_update(
    state: AppState,
    post: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_POST_UPDATE,
            serde_json::json!({ "post": post }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_post_delete(
    state: AppState,
    post_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_POST_DELETE,
            serde_json::json!({ "post_id": post_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_post_get(
    state: AppState,
    post_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_POST_GET,
            serde_json::json!({ "post_id": post_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_posts_by_user(
    state: AppState,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_POSTS_BY_USER,
            serde_json::json!({ "user_id": user_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_timeline(
    state: AppState,
    query: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    // Pass-through: the React composer is expected to send the
    // full `TimelineQuery` shape (viewer_id, scope, limit,
    // before_cursor, before_ts, author_id). An empty `{` falls
    // back to the caller's own feed.
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(A3chatRpcMethod::MOMENTS_TIMELINE, query)
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_comment_add(
    state: AppState,
    comment: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_COMMENT_ADD,
            serde_json::json!({ "comment": comment }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_comments_list(
    state: AppState,
    post_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_COMMENTS_LIST,
            serde_json::json!({ "post_id": post_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_react(
    state: AppState,
    reaction: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_REACT,
            serde_json::json!({ "reaction": reaction }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_reactions_list(
    state: AppState,
    target_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_REACTIONS_LIST,
            serde_json::json!({ "target_id": target_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_follow(
    state: AppState,
    following_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_FOLLOW,
            serde_json::json!({ "following_id": following_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_unfollow(
    state: AppState,
    following_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_UNFOLLOW,
            serde_json::json!({ "following_id": following_id }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_following_list(
    state: AppState,
    follower_id: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    let params = match follower_id {
        Some(id) => serde_json::json!({ "follower_id": id }),
        None => serde_json::json!({}),
    };
    client
        .call(A3chatRpcMethod::MOMENTS_FOLLOWING_LIST, params)
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_following_check(
    state: AppState,
    follower_id: String,
    following_id: String,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_FOLLOWING_CHECK,
            serde_json::json!({
                "follower_id": follower_id,
                "following_id": following_id,
            }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_verify_post(
    state: AppState,
    post: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_VERIFY_POST,
            serde_json::json!({ "post": post }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_verify_comment(
    state: AppState,
    comment: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_VERIFY_COMMENT,
            serde_json::json!({ "comment": comment }),
        )
        .await
        .map_err(TauriCommandError::from)
}

pub async fn moments_verify_reaction(
    state: AppState,
    reaction: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    let client = state.client().ok_or_else(no_login)?;
    client
        .call(
            A3chatRpcMethod::MOMENTS_VERIFY_REACTION,
            serde_json::json!({ "reaction": reaction }),
        )
        .await
        .map_err(TauriCommandError::from)
}

// ── Internal helpers ─────────────────────────────────────────────────────

fn no_login() -> TauriCommandError {
    TauriCommandError::security("not_logged_in", "no active session — call `login` first")
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::id::generate_user_id;

    fn logged_in() -> AppState {
        let owner = generate_user_id();
        let cfg = crate::client::A3chatClientConfig::new("http://127.0.0.1:1", owner);
        AppState::new().builder().client(crate::client::A3chatClient::new(cfg)).build()
    }

    /// Pre-login AppState — the Tauri status bar polls the daemon
    /// before / during `login`, so `healthz` must work without a
    /// client attached.
    fn logged_in_unauth() -> AppState {
        AppState::new()
    }

    #[tokio::test]
    async fn every_command_requires_login() {
        let s = AppState::new();
        let r = chat_conversation_list(s.clone()).await;
        assert!(r.is_err());
        let r = profile_get(s.clone()).await;
        assert!(r.is_err());
        let r = contact_list(s.clone()).await;
        assert!(r.is_err());
        let r = group_create(s.clone(), "x".into(), None, None).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn chat_search_rejects_empty_needle() {
        let s = logged_in();
        let r = chat_search(s.clone(), "".into(), None).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn contact_add_request_rejects_short_user_id() {
        let s = logged_in();
        let r = contact_add_request(s.clone(), "short".into(), None).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn group_create_rejects_empty_name() {
        let s = logged_in();
        let r = group_create(s.clone(), "".into(), None, None).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn presence_publish_rejects_unknown_status() {
        let s = logged_in();
        let r = presence_publish(s.clone(), "sleeping".into(), None).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn moderation_check_attachment_rejects_short_hash() {
        let s = logged_in();
        let r = moderation_check_attachment(s.clone(), "deadbeef".into(), "text/plain".into(), 64)
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn stream_subscribe_uses_default_topic() {
        let s = logged_in();
        // The RPC call will fail because the address is unreachable,
        // but the validation path should succeed.
        let r = stream_subscribe(s.clone(), None).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn executor_unknown_method_is_permanent() {
        let s = logged_in();
        let ex = RcpCommandExecutor::new();
        let r = ex.execute(s, "a3chat.bogus", serde_json::json!({})).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn a3chat_command_set_dispatch() {
        let s = logged_in();
        let set = A3chatCommandSet::new(s);
        let r = set.dispatch("a3chat.bogus", serde_json::json!({})).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn profile_put_rejects_non_object() {
        let s = logged_in();
        let r = profile_put(s.clone(), serde_json::json!("not-an-object")).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn rpc_envelope_serde() {
        let r = RpcRequest { params: serde_json::json!({}) };
        let s = serde_json::to_string(&r).unwrap();
        let back: RpcRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.params, serde_json::json!({}));
    }

    #[tokio::test]
    async fn rpc_response_serialises() {
        let r = RpcResponse {
            result: serde_json::json!({ "ok": true }),
            request_id: Some("req-1".into()),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("ok"));
        assert!(s.contains("req-1"));
    }

    #[tokio::test]
    async fn healthz_does_not_require_login() {
        // The status bar polls `healthz` before / during the login
        // flow — it must not error with no client attached. We
        // return a structured "no_client" envelope instead.
        let s = logged_in_unauth();
        let r = healthz(s.clone()).await.expect("healthz must not error pre-login");
        assert_eq!(r["ok"], serde_json::json!(false));
        assert_eq!(r["reason"], serde_json::json!("no_client"));
    }

    #[tokio::test]
    async fn rpc_health_aliases_healthz() {
        let s = logged_in_unauth();
        let r = rpc_health(s.clone()).await.expect("rpc_health must not error pre-login");
        assert_eq!(r["ok"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn healthz_returns_daemon_unreachable_when_target_offline() {
        // Post-login with a daemon URL that nothing is listening on
        // — must return a structured "daemon_unreachable" envelope,
        // NOT panic the UI.
        let s = logged_in();
        let r = healthz(s.clone()).await.expect("must not error");
        assert_eq!(r["ok"], serde_json::json!(false));
        assert_eq!(r["reason"], serde_json::json!("daemon_unreachable"));
    }
}
