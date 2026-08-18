//! Tauri command registration glue.
//!
//! When the `desktop` feature is enabled, this module exposes a
//! `#[tauri::command]` wrapper per entry in [`COMMAND_CATALOG`] so
//! the React frontend can call them verbatim via `invoke()`.
//!
//! Each wrapper is a thin adapter that pulls the [`AppState`] out
//! of Tauri's managed state and forwards to the underlying
//! `tauri_cmd::ops` / `tauri_cmd::rcp` function. The wrappers
//! preserve the typed error contract so the frontend always sees
//! the same `{error_class, code, message, recovery_hint, fields}`
//! envelope regardless of which command was invoked.

#![cfg(feature = "desktop")]

use a3chat_core::id::UserId;

use crate::tauri_cmd::error::TauriCommandResult;
use crate::tauri_cmd::ops::{self, CancelHandle, StartDaemonRequest};
use crate::tauri_cmd::rcp;
use crate::tauri_cmd::state::AppState;

// ── Session / daemon ops ─────────────────────────────────────────────────

#[tauri::command]
pub async fn login(
    state: tauri::State<'_, AppState>,
    base_url: String,
    owner: String,
) -> TauriCommandResult<ops::SessionInfo> {
    let owner_id = UserId::from(owner);
    ops::login(state.inner().clone(), base_url, owner_id).await
}

#[tauri::command]
pub async fn logout(state: tauri::State<'_, AppState>) -> TauriCommandResult<()> {
    ops::logout(state.inner().clone()).await
}

#[tauri::command]
pub async fn session_info(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<ops::SessionInfo> {
    ops::session_info(state.inner().clone()).await
}

#[tauri::command]
pub async fn app_version() -> TauriCommandResult<ops::AppVersion> {
    ops::app_version().await
}

#[tauri::command]
pub async fn doctor(state: tauri::State<'_, AppState>) -> TauriCommandResult<ops::DoctorReport> {
    ops::doctor(state.inner().clone()).await
}

#[tauri::command]
pub async fn start_daemon(req: StartDaemonRequest) -> TauriCommandResult<CancelHandle> {
    ops::start_daemon(req).await
}

#[tauri::command]
pub async fn stop_daemon(handle: CancelHandle) -> TauriCommandResult<()> {
    ops::stop_daemon(handle).await
}

#[tauri::command]
pub async fn menu_bar(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<Vec<ops::TopLevelMenu>> {
    ops::menu_bar(state.inner().clone()).await
}

#[tauri::command]
pub async fn sidebar_tree(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<Vec<ops::TreeNode>> {
    ops::sidebar_tree(state.inner().clone()).await
}

#[tauri::command]
pub async fn command_cancel(_handle: CancelHandle) -> TauriCommandResult<()> {
    ops::command_cancel(_handle).await
}

// ── Conversations ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn chat_conversation_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_conversation_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn chat_conversation_open(
    state: tauri::State<'_, AppState>,
    conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_conversation_open(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_typing(
    state: tauri::State<'_, AppState>,
    conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_typing(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_search(
    state: tauri::State<'_, AppState>,
    needle: String,
    limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_search(state.inner().clone(), needle, limit).await
}

// ── Messages ──────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn chat_message_send(
    state: tauri::State<'_, AppState>,
    envelope: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_message_send(state.inner().clone(), envelope).await
}

#[tauri::command]
pub async fn chat_message_recall(
    state: tauri::State<'_, AppState>,
    message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_message_recall(state.inner().clone(), message_id).await
}

#[tauri::command]
pub async fn chat_message_ack(
    state: tauri::State<'_, AppState>,
    message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_message_ack(state.inner().clone(), message_id).await
}

#[tauri::command]
pub async fn chat_message_edit(
    state: tauri::State<'_, AppState>,
    message_id: String,
    body: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_message_edit(state.inner().clone(), message_id, body).await
}

#[tauri::command]
pub async fn chat_message_delete(
    state: tauri::State<'_, AppState>,
    message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_message_delete(state.inner().clone(), message_id).await
}

// ── Sync ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn chat_sync_snapshot(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_sync_snapshot(state.inner().clone()).await
}

#[tauri::command]
pub async fn chat_sync_delta(
    state: tauri::State<'_, AppState>,
    cursors: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_sync_delta(state.inner().clone(), cursors).await
}

#[tauri::command]
pub async fn chat_sync_compressed(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_sync_compressed(state.inner().clone()).await
}

// ── Profile ───────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn profile_get(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_get(state.inner().clone()).await
}

#[tauri::command]
pub async fn profile_put(
    state: tauri::State<'_, AppState>,
    profile: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_put(state.inner().clone(), profile).await
}

#[tauri::command]
pub async fn profile_preferences_put(
    state: tauri::State<'_, AppState>,
    prefs: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_preferences_put(state.inner().clone(), prefs).await
}

#[tauri::command]
pub async fn profile_digit_get(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_digit_get(state.inner().clone()).await
}

#[tauri::command]
pub async fn profile_public_key_add(
    state: tauri::State<'_, AppState>,
    algorithm: String,
    public_key: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_public_key_add(state.inner().clone(), algorithm, public_key).await
}

#[tauri::command]
pub async fn profile_public_key_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_public_key_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn profile_public_key_revoke(
    state: tauri::State<'_, AppState>,
    public_key: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_public_key_revoke(state.inner().clone(), public_key).await
}

#[tauri::command]
pub async fn profile_device_register(
    state: tauri::State<'_, AppState>,
    device_class: String,
    device_label: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_device_register(state.inner().clone(), device_class, device_label).await
}

#[tauri::command]
pub async fn profile_device_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_device_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn profile_avatar_set(
    state: tauri::State<'_, AppState>,
    blob_hash: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_avatar_set(state.inner().clone(), blob_hash).await
}

// ── Contact ───────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn contact_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn contact_add_request(
    state: tauri::State<'_, AppState>,
    to_user_id: String,
    message: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_add_request(state.inner().clone(), to_user_id, message).await
}

#[tauri::command]
pub async fn contact_accept_request(
    state: tauri::State<'_, AppState>,
    request_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_accept_request(state.inner().clone(), request_id).await
}

#[tauri::command]
pub async fn contact_block(
    state: tauri::State<'_, AppState>,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_block(state.inner().clone(), user_id).await
}

#[tauri::command]
pub async fn contact_unblock(
    state: tauri::State<'_, AppState>,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_unblock(state.inner().clone(), user_id).await
}

#[tauri::command]
pub async fn contact_qr_invite(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_qr_invite(state.inner().clone()).await
}

// ── Group ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn group_create(
    state: tauri::State<'_, AppState>,
    name: String,
    description: Option<String>,
    is_private: Option<bool>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_create(state.inner().clone(), name, description, is_private).await
}

#[tauri::command]
pub async fn group_invite(
    state: tauri::State<'_, AppState>,
    conversation_id: String,
    invitee_id: String,
    group_name: String,
    inviter_name: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_invite(
        state.inner().clone(),
        conversation_id,
        invitee_id,
        group_name,
        inviter_name,
    )
    .await
}

#[tauri::command]
pub async fn group_join(
    state: tauri::State<'_, AppState>,
    invitation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_join(state.inner().clone(), invitation_id).await
}

#[tauri::command]
pub async fn group_member_add(
    state: tauri::State<'_, AppState>,
    conversation_id: String,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_member_add(state.inner().clone(), conversation_id, user_id).await
}

#[tauri::command]
pub async fn group_member_remove(
    state: tauri::State<'_, AppState>,
    conversation_id: String,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_member_remove(state.inner().clone(), conversation_id, user_id).await
}

#[tauri::command]
pub async fn group_member_role(
    state: tauri::State<'_, AppState>,
    conversation_id: String,
    user_id: String,
    role: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_member_role(state.inner().clone(), conversation_id, user_id, role).await
}

#[tauri::command]
pub async fn group_announcement_set(
    state: tauri::State<'_, AppState>,
    conversation_id: String,
    text: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_announcement_set(state.inner().clone(), conversation_id, text).await
}

// ── Presence ─────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn presence_publish(
    state: tauri::State<'_, AppState>,
    status: String,
    status_message: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::presence_publish(state.inner().clone(), status, status_message).await
}

#[tauri::command]
pub async fn presence_subscribe(
    state: tauri::State<'_, AppState>,
    peers: Vec<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::presence_subscribe(state.inner().clone(), peers).await
}

// ── Moderation ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn moderation_check_content(
    state: tauri::State<'_, AppState>,
    text: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moderation_check_content(state.inner().clone(), text).await
}

#[tauri::command]
pub async fn moderation_check_attachment(
    state: tauri::State<'_, AppState>,
    hash: String,
    content_type: String,
    size: u64,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moderation_check_attachment(state.inner().clone(), hash, content_type, size).await
}

#[tauri::command]
pub async fn moderation_list_blocked(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moderation_list_blocked(state.inner().clone()).await
}

#[tauri::command]
pub async fn moderation_set_deny_default(
    state: tauri::State<'_, AppState>,
    on: bool,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moderation_set_deny_default(state.inner().clone(), on).await
}

#[tauri::command]
pub async fn moderation_stats(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moderation_stats(state.inner().clone()).await
}

// ── Media ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn media_health(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::media_health(state.inner().clone()).await
}

#[tauri::command]
pub async fn media_upload_init(
    state: tauri::State<'_, AppState>,
    mime: String,
    size: u64,
) -> TauriCommandResult<serde_json::Value> {
    rcp::media_upload_init(state.inner().clone(), mime, size).await
}

#[tauri::command]
pub async fn media_upload_chunk(
    state: tauri::State<'_, AppState>,
    token: String,
    chunk_index: u32,
    data_b64: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::media_upload_chunk(state.inner().clone(), token, chunk_index, data_b64).await
}

#[tauri::command]
pub async fn media_upload_finalize(
    state: tauri::State<'_, AppState>,
    token: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::media_upload_finalize(state.inner().clone(), token).await
}

#[tauri::command]
pub async fn media_download_get(
    state: tauri::State<'_, AppState>,
    hash: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::media_download_get(state.inner().clone(), hash).await
}

// ── E2E bundle ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn e2e_bundle_export(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::e2e_bundle_export(state.inner().clone()).await
}

#[tauri::command]
pub async fn e2e_bundle_import(
    state: tauri::State<'_, AppState>,
    bundle: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::e2e_bundle_import(state.inner().clone(), bundle).await
}

// ── Health (process-level liveness) ──────────────────────────────────────

#[tauri::command]
pub async fn healthz(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::healthz(state.inner().clone()).await
}

#[tauri::command]
pub async fn rpc_health(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::rpc_health(state.inner().clone()).await
}

// ── Audit (UI-only — no direct RPC) ──────────────────────────────────────

/// Generate a static audit report. The UI exposes this on the
/// Audit screen. The actual report is built client-side by walking
/// the React component tree / RPC trace and returning a structured
/// summary the dashboard can render.
pub async fn audit_report(_state: AppState) -> TauriCommandResult<serde_json::Value> {
    Ok(serde_json::json!({
        "toolchain": "a3chat/tauri",
        "do178c_section": "5.2 — traceability",
        "commands": crate::tauri_cmd::COMMAND_CATALOG
            .iter()
            .map(|c| serde_json::json!({
                "tauri_name": c.tauri_name,
                "screen": c.screen.as_str(),
                "rpc_method": c.rpc_method,
            }))
            .collect::<Vec<_>>(),
    }))
}

// ── Stream ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn stream_subscribe(
    state: tauri::State<'_, AppState>,
    topic: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::stream_subscribe(state.inner().clone(), topic).await
}

#[tauri::command]
pub async fn stream_unsubscribe(
    state: tauri::State<'_, AppState>,
    handle_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::stream_unsubscribe(state.inner().clone(), handle_id).await
}

#[tauri::command]
pub async fn stream_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::stream_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn peerfeedback_set_trust(
    state: tauri::State<'_, AppState>,
    target_user_id: String,
    score: f64,
    reason: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::peerfeedback_set_trust(state.inner().clone(), target_user_id, score, reason).await
}

#[tauri::command]
pub async fn peerfeedback_clear_trust(
    state: tauri::State<'_, AppState>,
    target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::peerfeedback_clear_trust(state.inner().clone(), target_user_id).await
}

#[tauri::command]
pub async fn peerfeedback_file_report(
    state: tauri::State<'_, AppState>,
    target_user_id: String,
    category: String,
    evidence: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::peerfeedback_file_report(state.inner().clone(), target_user_id, category, evidence).await
}

#[tauri::command]
pub async fn peerfeedback_fused_score(
    state: tauri::State<'_, AppState>,
    target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::peerfeedback_fused_score(state.inner().clone(), target_user_id).await
}

#[tauri::command]
pub async fn peerfeedback_list_trust(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::peerfeedback_list_trust(state.inner().clone()).await
}

#[tauri::command]
pub async fn peerfeedback_peer_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::peerfeedback_peer_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn peerfeedback_peer_get(
    state: tauri::State<'_, AppState>,
    target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::peerfeedback_peer_get(state.inner().clone(), target_user_id).await
}

// ── Moments / 朋友圈 (F-05) ─────────────────────────────────────────────────
//
// Each wrapper is a thin adapter that forwards the typed params
// to the corresponding `rcp::moments_*` helper. The React frontend
// invokes these via `invoke('moments_post_create', { post })` etc.
// See `tauri_cmd::rcp` for the JSON-RPC method mapping.

#[tauri::command]
pub async fn moments_node_info(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_node_info(state.inner().clone()).await
}

#[tauri::command]
pub async fn moments_post_create(
    state: tauri::State<'_, AppState>,
    post: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_post_create(state.inner().clone(), post).await
}

#[tauri::command]
pub async fn moments_post_update(
    state: tauri::State<'_, AppState>,
    post: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_post_update(state.inner().clone(), post).await
}

#[tauri::command]
pub async fn moments_post_delete(
    state: tauri::State<'_, AppState>,
    post_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_post_delete(state.inner().clone(), post_id).await
}

#[tauri::command]
pub async fn moments_post_get(
    state: tauri::State<'_, AppState>,
    post_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_post_get(state.inner().clone(), post_id).await
}

#[tauri::command]
pub async fn moments_posts_by_user(
    state: tauri::State<'_, AppState>,
    user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_posts_by_user(state.inner().clone(), user_id).await
}

#[tauri::command]
pub async fn moments_timeline(
    state: tauri::State<'_, AppState>,
    query: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_timeline(state.inner().clone(), query).await
}

#[tauri::command]
pub async fn moments_comment_add(
    state: tauri::State<'_, AppState>,
    comment: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_comment_add(state.inner().clone(), comment).await
}

#[tauri::command]
pub async fn moments_comments_list(
    state: tauri::State<'_, AppState>,
    post_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_comments_list(state.inner().clone(), post_id).await
}

#[tauri::command]
pub async fn moments_react(
    state: tauri::State<'_, AppState>,
    reaction: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_react(state.inner().clone(), reaction).await
}

#[tauri::command]
pub async fn moments_reactions_list(
    state: tauri::State<'_, AppState>,
    target_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_reactions_list(state.inner().clone(), target_id).await
}

#[tauri::command]
pub async fn moments_follow(
    state: tauri::State<'_, AppState>,
    following_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_follow(state.inner().clone(), following_id).await
}

#[tauri::command]
pub async fn moments_unfollow(
    state: tauri::State<'_, AppState>,
    following_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_unfollow(state.inner().clone(), following_id).await
}

#[tauri::command]
pub async fn moments_following_list(
    state: tauri::State<'_, AppState>,
    follower_id: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_following_list(state.inner().clone(), follower_id).await
}

#[tauri::command]
pub async fn moments_following_check(
    state: tauri::State<'_, AppState>,
    follower_id: String,
    following_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_following_check(state.inner().clone(), follower_id, following_id).await
}

#[tauri::command]
pub async fn moments_verify_post(
    state: tauri::State<'_, AppState>,
    post: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_verify_post(state.inner().clone(), post).await
}

#[tauri::command]
pub async fn moments_verify_comment(
    state: tauri::State<'_, AppState>,
    comment: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_verify_comment(state.inner().clone(), comment).await
}

#[tauri::command]
pub async fn moments_verify_reaction(
    state: tauri::State<'_, AppState>,
    reaction: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_verify_reaction(state.inner().clone(), reaction).await
}

// ── Link bookmarks / favorites (F-08) ────────────────────────────────────

#[tauri::command]
pub async fn link_bookmark_add(
    state: tauri::State<'_, AppState>,
    request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_add(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn link_bookmark_update(
    state: tauri::State<'_, AppState>,
    bookmark_id: String,
    request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_update(state.inner().clone(), bookmark_id, request).await
}

#[tauri::command]
pub async fn link_bookmark_get(
    state: tauri::State<'_, AppState>,
    bookmark_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_get(state.inner().clone(), bookmark_id).await
}

#[tauri::command]
pub async fn link_bookmark_get_by_url(
    state: tauri::State<'_, AppState>,
    url: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_get_by_url(state.inner().clone(), url).await
}

#[tauri::command]
pub async fn link_bookmark_list(
    state: tauri::State<'_, AppState>,
    filter: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_list(state.inner().clone(), filter).await
}

#[tauri::command]
pub async fn link_bookmark_search(
    state: tauri::State<'_, AppState>,
    query: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_search(state.inner().clone(), query).await
}

#[tauri::command]
pub async fn link_bookmark_delete(
    state: tauri::State<'_, AppState>,
    bookmark_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_delete(state.inner().clone(), bookmark_id).await
}

#[tauri::command]
pub async fn link_bookmark_set_pinned(
    state: tauri::State<'_, AppState>,
    bookmark_id: String,
    is_pinned: bool,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_set_pinned(state.inner().clone(), bookmark_id, is_pinned).await
}

#[tauri::command]
pub async fn link_bookmark_set_archived(
    state: tauri::State<'_, AppState>,
    bookmark_id: String,
    is_archived: bool,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_set_archived(state.inner().clone(), bookmark_id, is_archived).await
}

#[tauri::command]
pub async fn link_bookmark_touch_visit(
    state: tauri::State<'_, AppState>,
    bookmark_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_touch_visit(state.inner().clone(), bookmark_id).await
}

#[tauri::command]
pub async fn link_bookmark_tags(state: tauri::State<'_, AppState>) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_tags(state.inner().clone()).await
}

#[tauri::command]
pub async fn link_bookmark_folders(state: tauri::State<'_, AppState>) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_folders(state.inner().clone()).await
}

#[tauri::command]
pub async fn link_bookmark_count(state: tauri::State<'_, AppState>) -> TauriCommandResult<serde_json::Value> {
    rcp::link_bookmark_count(state.inner().clone()).await
}

/// Returns every `tauri::command` exported by this module. The
/// Tauri builder consumes this list verbatim via
/// `.invoke_handler(tauri::generate_handler![all_commands!()])`.
#[macro_export]
macro_rules! all_commands {
    () => {
        tauri::generate_handler![
            // session
            $crate::commands_tauri::login,
            $crate::commands_tauri::logout,
            $crate::commands_tauri::session_info,
            $crate::commands_tauri::app_version,
            $crate::commands_tauri::doctor,
            $crate::commands_tauri::start_daemon,
            $crate::commands_tauri::stop_daemon,
            $crate::commands_tauri::menu_bar,
            $crate::commands_tauri::sidebar_tree,
            $crate::commands_tauri::command_cancel,
            // conversation
            $crate::commands_tauri::chat_conversation_list,
            $crate::commands_tauri::chat_conversation_open,
            $crate::commands_tauri::chat_typing,
            $crate::commands_tauri::chat_search,
            // messages
            $crate::commands_tauri::chat_message_send,
            $crate::commands_tauri::chat_message_recall,
            $crate::commands_tauri::chat_message_ack,
            $crate::commands_tauri::chat_message_edit,
            $crate::commands_tauri::chat_message_delete,
            // sync
            $crate::commands_tauri::chat_sync_snapshot,
            $crate::commands_tauri::chat_sync_delta,
            $crate::commands_tauri::chat_sync_compressed,
            // profile
            $crate::commands_tauri::profile_get,
            $crate::commands_tauri::profile_put,
            $crate::commands_tauri::profile_preferences_put,
            $crate::commands_tauri::profile_digit_get,
            $crate::commands_tauri::profile_public_key_add,
            $crate::commands_tauri::profile_public_key_list,
            $crate::commands_tauri::profile_public_key_revoke,
            $crate::commands_tauri::profile_device_register,
            $crate::commands_tauri::profile_device_list,
            $crate::commands_tauri::profile_avatar_set,
            // contact
            $crate::commands_tauri::contact_list,
            $crate::commands_tauri::contact_add_request,
            $crate::commands_tauri::contact_accept_request,
            $crate::commands_tauri::contact_block,
            $crate::commands_tauri::contact_unblock,
            $crate::commands_tauri::contact_qr_invite,
            // group
            $crate::commands_tauri::group_create,
            $crate::commands_tauri::group_invite,
            $crate::commands_tauri::group_join,
            $crate::commands_tauri::group_member_add,
            $crate::commands_tauri::group_member_remove,
            $crate::commands_tauri::group_member_role,
            $crate::commands_tauri::group_announcement_set,
            // presence
            $crate::commands_tauri::presence_publish,
            $crate::commands_tauri::presence_subscribe,
            // moderation
            $crate::commands_tauri::moderation_check_content,
            $crate::commands_tauri::moderation_check_attachment,
            $crate::commands_tauri::moderation_list_blocked,
            $crate::commands_tauri::moderation_set_deny_default,
            $crate::commands_tauri::moderation_stats,
            // media
            $crate::commands_tauri::media_health,
            $crate::commands_tauri::media_upload_init,
            $crate::commands_tauri::media_upload_chunk,
            $crate::commands_tauri::media_upload_finalize,
            $crate::commands_tauri::media_download_get,
            // e2e
            $crate::commands_tauri::e2e_bundle_export,
            $crate::commands_tauri::e2e_bundle_import,
            // stream
            $crate::commands_tauri::stream_subscribe,
            $crate::commands_tauri::stream_unsubscribe,
            $crate::commands_tauri::stream_list,
            // moments / 朋友圈 (F-05)
            $crate::commands_tauri::moments_node_info,
            $crate::commands_tauri::moments_post_create,
            $crate::commands_tauri::moments_post_update,
            $crate::commands_tauri::moments_post_delete,
            $crate::commands_tauri::moments_post_get,
            $crate::commands_tauri::moments_posts_by_user,
            $crate::commands_tauri::moments_timeline,
            $crate::commands_tauri::moments_comment_add,
            $crate::commands_tauri::moments_comments_list,
            $crate::commands_tauri::moments_react,
            $crate::commands_tauri::moments_reactions_list,
            $crate::commands_tauri::moments_follow,
            $crate::commands_tauri::moments_unfollow,
            $crate::commands_tauri::moments_following_list,
            $crate::commands_tauri::moments_following_check,
            $crate::commands_tauri::moments_verify_post,
            $crate::commands_tauri::moments_verify_comment,
            $crate::commands_tauri::moments_verify_reaction,
            // link bookmarks / favorites (F-08)
            $crate::commands_tauri::link_bookmark_add,
            $crate::commands_tauri::link_bookmark_update,
            $crate::commands_tauri::link_bookmark_get,
            $crate::commands_tauri::link_bookmark_get_by_url,
            $crate::commands_tauri::link_bookmark_list,
            $crate::commands_tauri::link_bookmark_search,
            $crate::commands_tauri::link_bookmark_delete,
            $crate::commands_tauri::link_bookmark_set_pinned,
            $crate::commands_tauri::link_bookmark_set_archived,
            $crate::commands_tauri::link_bookmark_touch_visit,
            $crate::commands_tauri::link_bookmark_tags,
            $crate::commands_tauri::link_bookmark_folders,
            $crate::commands_tauri::link_bookmark_count,
            // peer feedback
            $crate::commands_tauri::peerfeedback_set_trust,
            $crate::commands_tauri::peerfeedback_clear_trust,
            $crate::commands_tauri::peerfeedback_file_report,
            $crate::commands_tauri::peerfeedback_fused_score,
            $crate::commands_tauri::peerfeedback_list_trust,
            $crate::commands_tauri::peerfeedback_peer_list,
            $crate::commands_tauri::peerfeedback_peer_get,
            // health (process-level liveness)
            $crate::commands_tauri::healthz,
            $crate::commands_tauri::rpc_health,
        ]
    };
}
