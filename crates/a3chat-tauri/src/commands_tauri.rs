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


#[tauri::command]
pub async fn chat_draft_save(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        body: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_draft_save(state.inner().clone(), conversation_id, body).await
}

#[tauri::command]
pub async fn chat_draft_get(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_draft_get(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_draft_delete(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_draft_delete(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_draft_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_draft_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn chat_draft_clear(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_draft_clear(state.inner().clone()).await
}

#[tauri::command]
pub async fn chat_reaction_add(
    state: tauri::State<'_, AppState>,
        message_id: String,
        reaction: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_reaction_add(state.inner().clone(), message_id, reaction).await
}

#[tauri::command]
pub async fn chat_reaction_remove(
    state: tauri::State<'_, AppState>,
        message_id: String,
        reaction: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_reaction_remove(state.inner().clone(), message_id, reaction).await
}

#[tauri::command]
pub async fn chat_reaction_get(
    state: tauri::State<'_, AppState>,
        message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_reaction_get(state.inner().clone(), message_id).await
}

#[tauri::command]
pub async fn chat_notification_set_dnd(
    state: tauri::State<'_, AppState>,
        enabled: bool,
        until_unix: Option<i64>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_notification_set_dnd(state.inner().clone(), enabled, until_unix).await
}

#[tauri::command]
pub async fn chat_notification_get_dnd(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_notification_get_dnd(state.inner().clone()).await
}

#[tauri::command]
pub async fn chat_notification_set_conversation(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        muted: bool,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_notification_set_conversation(state.inner().clone(), conversation_id, muted).await
}

#[tauri::command]
pub async fn chat_notification_get_conversation(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_notification_get_conversation(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_notification_mute(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        until_unix: Option<i64>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_notification_mute(state.inner().clone(), conversation_id, until_unix).await
}

#[tauri::command]
pub async fn chat_notification_unmute(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_notification_unmute(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_notification_list_muted(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_notification_list_muted(state.inner().clone()).await
}

#[tauri::command]
pub async fn chat_conversation_pin(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_conversation_pin(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_conversation_unpin(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_conversation_unpin(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_conversation_toggle_pin(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_conversation_toggle_pin(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn chat_conversation_create_direct(
    state: tauri::State<'_, AppState>,
        peer_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_conversation_create_direct(state.inner().clone(), peer_user_id).await
}

#[tauri::command]
pub async fn chat_message_forward(
    state: tauri::State<'_, AppState>,
        message_id: String,
        target_conversation_ids: Vec<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_message_forward(state.inner().clone(), message_id, target_conversation_ids).await
}

#[tauri::command]
pub async fn chat_message_forward_merge(
    state: tauri::State<'_, AppState>,
        message_ids: Vec<String>,
        target_conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_message_forward_merge(state.inner().clone(), message_ids, target_conversation_id).await
}

#[tauri::command]
pub async fn chat_tap(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        target_user_id: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_tap(state.inner().clone(), conversation_id, target_user_id).await
}

#[tauri::command]
pub async fn chat_thread_list(
    state: tauri::State<'_, AppState>,
        root_message_id: String,
        limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_thread_list(state.inner().clone(), root_message_id, limit).await
}

#[tauri::command]
pub async fn chat_thread_get(
    state: tauri::State<'_, AppState>,
        root_message_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::chat_thread_get(state.inner().clone(), root_message_id).await
}

#[tauri::command]
pub async fn profile_avatar_upload(
    state: tauri::State<'_, AppState>,
        envelope: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_avatar_upload(state.inner().clone(), envelope).await
}

#[tauri::command]
pub async fn profile_avatar_get(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_avatar_get(state.inner().clone()).await
}

#[tauri::command]
pub async fn profile_avatar_remove(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_avatar_remove(state.inner().clone()).await
}

#[tauri::command]
pub async fn profile_public_key_label(
    state: tauri::State<'_, AppState>,
        key_id: String,
        label: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_public_key_label(state.inner().clone(), key_id, label).await
}

#[tauri::command]
pub async fn profile_kind_get(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_kind_get(state.inner().clone()).await
}

#[tauri::command]
pub async fn profile_kind_set(
    state: tauri::State<'_, AppState>,
        kind: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::profile_kind_set(state.inner().clone(), kind).await
}

#[tauri::command]
pub async fn contact_add(
    state: tauri::State<'_, AppState>,
        contact: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_add(state.inner().clone(), contact).await
}

#[tauri::command]
pub async fn contact_remove(
    state: tauri::State<'_, AppState>,
        user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_remove(state.inner().clone(), user_id).await
}

#[tauri::command]
pub async fn contact_get(
    state: tauri::State<'_, AppState>,
        user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_get(state.inner().clone(), user_id).await
}

#[tauri::command]
pub async fn contact_search(
    state: tauri::State<'_, AppState>,
        needle: String,
        limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_search(state.inner().clone(), needle, limit).await
}

#[tauri::command]
pub async fn contact_toggle_favorite(
    state: tauri::State<'_, AppState>,
        user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_toggle_favorite(state.inner().clone(), user_id).await
}

#[tauri::command]
pub async fn contact_update(
    state: tauri::State<'_, AppState>,
        contact: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::contact_update(state.inner().clone(), contact).await
}

#[tauri::command]
pub async fn group_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn group_members(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_members(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn group_member_get(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_member_get(state.inner().clone(), conversation_id, user_id).await
}

#[tauri::command]
pub async fn group_metadata_update(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_metadata_update(state.inner().clone(), conversation_id, request).await
}

#[tauri::command]
pub async fn group_transfer_ownership(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        new_owner_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_transfer_ownership(state.inner().clone(), conversation_id, new_owner_id).await
}

#[tauri::command]
pub async fn group_dissolve(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_dissolve(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn group_leave(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_leave(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn group_mute_member(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        user_id: String,
        muted_until_secs: i64,
        reason: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_mute_member(state.inner().clone(), conversation_id, user_id, muted_until_secs, reason).await
}

#[tauri::command]
pub async fn group_unmute_member(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_unmute_member(state.inner().clone(), conversation_id, user_id).await
}

#[tauri::command]
pub async fn group_mute_all(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_mute_all(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn group_unmute_all(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_unmute_all(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn group_list_muted(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_list_muted(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn group_nickname_set(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        user_id: String,
        nickname: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_nickname_set(state.inner().clone(), conversation_id, user_id, nickname).await
}

#[tauri::command]
pub async fn group_nickname_get(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
        user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_nickname_get(state.inner().clone(), conversation_id, user_id).await
}

#[tauri::command]
pub async fn group_nickname_list(
    state: tauri::State<'_, AppState>,
        conversation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_nickname_list(state.inner().clone(), conversation_id).await
}

#[tauri::command]
pub async fn group_mention_parse(
    state: tauri::State<'_, AppState>,
        body: String,
        nicknames: Option<serde_json::Value>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::group_mention_parse(state.inner().clone(), body, nicknames).await
}

#[tauri::command]
pub async fn channel_account_register(
    state: tauri::State<'_, AppState>,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_account_register(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn channel_account_update(
    state: tauri::State<'_, AppState>,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_account_update(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn channel_account_get(
    state: tauri::State<'_, AppState>,
        account_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_account_get(state.inner().clone(), account_id).await
}

#[tauri::command]
pub async fn channel_account_get_by_owner(
    state: tauri::State<'_, AppState>,
        owner_node_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_account_get_by_owner(state.inner().clone(), owner_node_id).await
}

#[tauri::command]
pub async fn channel_account_list(
    state: tauri::State<'_, AppState>,
        limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_account_list(state.inner().clone(), limit).await
}

#[tauri::command]
pub async fn channel_account_search(
    state: tauri::State<'_, AppState>,
        needle: String,
        limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_account_search(state.inner().clone(), needle, limit).await
}

#[tauri::command]
pub async fn channel_account_delete(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_account_delete(state.inner().clone()).await
}

#[tauri::command]
pub async fn channel_subscribe(
    state: tauri::State<'_, AppState>,
        account_id: String,
        alias: Option<String>,
        notify_mode: Option<String>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_subscribe(state.inner().clone(), account_id, alias, notify_mode).await
}

#[tauri::command]
pub async fn channel_unsubscribe(
    state: tauri::State<'_, AppState>,
        account_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_unsubscribe(state.inner().clone(), account_id).await
}

#[tauri::command]
pub async fn channel_subscriptions_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_subscriptions_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn channel_subscriptions_of_account(
    state: tauri::State<'_, AppState>,
        account_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_subscriptions_of_account(state.inner().clone(), account_id).await
}

#[tauri::command]
pub async fn channel_subscription_set_notify(
    state: tauri::State<'_, AppState>,
        account_id: String,
        notify_mode: Option<String>,
        is_muted: Option<bool>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_subscription_set_notify(state.inner().clone(), account_id, notify_mode, is_muted).await
}

#[tauri::command]
pub async fn channel_subscription_set_pinned(
    state: tauri::State<'_, AppState>,
        account_id: String,
        is_pinned: bool,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_subscription_set_pinned(state.inner().clone(), account_id, is_pinned).await
}

#[tauri::command]
pub async fn channel_feed_publish(
    state: tauri::State<'_, AppState>,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_feed_publish(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn channel_feed_retract(
    state: tauri::State<'_, AppState>,
        feed_id: String,
        reason: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_feed_retract(state.inner().clone(), feed_id, reason).await
}

#[tauri::command]
pub async fn channel_feed_get(
    state: tauri::State<'_, AppState>,
        account_id: String,
        feed_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_feed_get(state.inner().clone(), account_id, feed_id).await
}

#[tauri::command]
pub async fn channel_feed_list(
    state: tauri::State<'_, AppState>,
        account_id: String,
        before_sequence: Option<u32>,
        limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_feed_list(state.inner().clone(), account_id, before_sequence, limit).await
}

#[tauri::command]
pub async fn channel_feed_timeline(
    state: tauri::State<'_, AppState>,
        before_sequence: Option<u32>,
        limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_feed_timeline(state.inner().clone(), before_sequence, limit).await
}

#[tauri::command]
pub async fn channel_feed_mark_read(
    state: tauri::State<'_, AppState>,
        account_id: String,
        last_read_seq: u32,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_feed_mark_read(state.inner().clone(), account_id, last_read_seq).await
}

#[tauri::command]
pub async fn channel_feed_unread_count(
    state: tauri::State<'_, AppState>,
        account_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_feed_unread_count(state.inner().clone(), account_id).await
}

#[tauri::command]
pub async fn channel_health(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_health(state.inner().clone()).await
}

#[tauri::command]
pub async fn channel_analytics_summary(
    state: tauri::State<'_, AppState>,
        account_id: String,
        window_days: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_analytics_summary(state.inner().clone(), account_id, window_days).await
}

#[tauri::command]
pub async fn channel_analytics_timeline(
    state: tauri::State<'_, AppState>,
        account_id: String,
        days: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_analytics_timeline(state.inner().clone(), account_id, days).await
}

#[tauri::command]
pub async fn channel_analytics_audit(
    state: tauri::State<'_, AppState>,
        account_id: String,
        cursor: Option<i64>,
        limit: Option<u32>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_analytics_audit(state.inner().clone(), account_id, cursor, limit).await
}

#[tauri::command]
pub async fn channel_analytics_audit_verify(
    state: tauri::State<'_, AppState>,
        account_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::channel_analytics_audit_verify(state.inner().clone(), account_id).await
}

#[tauri::command]
pub async fn pairing_invitation_create(
    state: tauri::State<'_, AppState>,
        payload: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_invitation_create(state.inner().clone(), payload).await
}

#[tauri::command]
pub async fn pairing_invitation_verify(
    state: tauri::State<'_, AppState>,
        payload: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_invitation_verify(state.inner().clone(), payload).await
}

#[tauri::command]
pub async fn pairing_invitation_parse(
    state: tauri::State<'_, AppState>,
        invitation_code: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_invitation_parse(state.inner().clone(), invitation_code).await
}

#[tauri::command]
pub async fn pairing_invitation_accept(
    state: tauri::State<'_, AppState>,
        payload: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_invitation_accept(state.inner().clone(), payload).await
}

#[tauri::command]
pub async fn pairing_invitation_revoke(
    state: tauri::State<'_, AppState>,
        invitation_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_invitation_revoke(state.inner().clone(), invitation_id).await
}

#[tauri::command]
pub async fn pairing_trusted_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_trusted_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn pairing_trusted_get(
    state: tauri::State<'_, AppState>,
        trusted_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_trusted_get(state.inner().clone(), trusted_id).await
}

#[tauri::command]
pub async fn pairing_trusted_revoke(
    state: tauri::State<'_, AppState>,
        trusted_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_trusted_revoke(state.inner().clone(), trusted_id).await
}

#[tauri::command]
pub async fn pairing_code_create(
    state: tauri::State<'_, AppState>,
        payload: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_code_create(state.inner().clone(), payload).await
}

#[tauri::command]
pub async fn pairing_code_parse(
    state: tauri::State<'_, AppState>,
        code: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_code_parse(state.inner().clone(), code).await
}

#[tauri::command]
pub async fn pairing_health(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::pairing_health(state.inner().clone()).await
}

#[tauri::command]
pub async fn device_register(
    state: tauri::State<'_, AppState>,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::device_register(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn device_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::device_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn device_get(
    state: tauri::State<'_, AppState>,
        device_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::device_get(state.inner().clone(), device_id).await
}

#[tauri::command]
pub async fn device_revoke(
    state: tauri::State<'_, AppState>,
        device_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::device_revoke(state.inner().clone(), device_id).await
}

#[tauri::command]
pub async fn device_set_primary(
    state: tauri::State<'_, AppState>,
        device_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::device_set_primary(state.inner().clone(), device_id).await
}

#[tauri::command]
pub async fn device_get_current(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::device_get_current(state.inner().clone()).await
}

#[tauri::command]
pub async fn device_touch(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::device_touch(state.inner().clone()).await
}

#[tauri::command]
pub async fn e2e_handshake_initiate(
    state: tauri::State<'_, AppState>,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::e2e_handshake_initiate(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn e2e_handshake_respond(
    state: tauri::State<'_, AppState>,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::e2e_handshake_respond(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn e2e_handshake_complete(
    state: tauri::State<'_, AppState>,
        request: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::e2e_handshake_complete(state.inner().clone(), request).await
}

#[tauri::command]
pub async fn e2e_handshake_needs_rehandshake(
    state: tauri::State<'_, AppState>,
        peer_node_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::e2e_handshake_needs_rehandshake(state.inner().clone(), peer_node_id).await
}

#[tauri::command]
pub async fn e2e_handshake_is_complete(
    state: tauri::State<'_, AppState>,
        peer_node_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::e2e_handshake_is_complete(state.inner().clone(), peer_node_id).await
}

#[tauri::command]
pub async fn moments_comment_edit(
    state: tauri::State<'_, AppState>,
        comment: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_comment_edit(state.inner().clone(), comment).await
}

#[tauri::command]
pub async fn moments_comment_delete(
    state: tauri::State<'_, AppState>,
        comment_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_comment_delete(state.inner().clone(), comment_id).await
}

#[tauri::command]
pub async fn moments_unreact(
    state: tauri::State<'_, AppState>,
        reaction: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_unreact(state.inner().clone(), reaction).await
}

#[tauri::command]
pub async fn moments_block(
    state: tauri::State<'_, AppState>,
        target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_block(state.inner().clone(), target_user_id).await
}

#[tauri::command]
pub async fn moments_unblock(
    state: tauri::State<'_, AppState>,
        target_user_id: String,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_unblock(state.inner().clone(), target_user_id).await
}

#[tauri::command]
pub async fn moments_blocklist_list(
    state: tauri::State<'_, AppState>,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_blocklist_list(state.inner().clone()).await
}

#[tauri::command]
pub async fn moments_share(
    state: tauri::State<'_, AppState>,
        share: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_share(state.inner().clone(), share).await
}

#[tauri::command]
pub async fn moments_report(
    state: tauri::State<'_, AppState>,
        report: serde_json::Value,
) -> TauriCommandResult<serde_json::Value> {
    rcp::moments_report(state.inner().clone(), report).await
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
            $crate::commands_tauri::channel_account_delete,
            $crate::commands_tauri::channel_account_get,
            $crate::commands_tauri::channel_account_get_by_owner,
            $crate::commands_tauri::channel_account_list,
            $crate::commands_tauri::channel_account_register,
            $crate::commands_tauri::channel_account_search,
            $crate::commands_tauri::channel_account_update,
            $crate::commands_tauri::channel_analytics_audit,
            $crate::commands_tauri::channel_analytics_audit_verify,
            $crate::commands_tauri::channel_analytics_summary,
            $crate::commands_tauri::channel_analytics_timeline,
            $crate::commands_tauri::channel_feed_get,
            $crate::commands_tauri::channel_feed_list,
            $crate::commands_tauri::channel_feed_mark_read,
            $crate::commands_tauri::channel_feed_publish,
            $crate::commands_tauri::channel_feed_retract,
            $crate::commands_tauri::channel_feed_timeline,
            $crate::commands_tauri::channel_feed_unread_count,
            $crate::commands_tauri::channel_health,
            $crate::commands_tauri::channel_subscribe,
            $crate::commands_tauri::channel_subscription_set_notify,
            $crate::commands_tauri::channel_subscription_set_pinned,
            $crate::commands_tauri::channel_subscriptions_list,
            $crate::commands_tauri::channel_subscriptions_of_account,
            $crate::commands_tauri::channel_unsubscribe,
            $crate::commands_tauri::chat_conversation_create_direct,
            $crate::commands_tauri::chat_conversation_pin,
            $crate::commands_tauri::chat_conversation_toggle_pin,
            $crate::commands_tauri::chat_conversation_unpin,
            $crate::commands_tauri::chat_draft_clear,
            $crate::commands_tauri::chat_draft_delete,
            $crate::commands_tauri::chat_draft_get,
            $crate::commands_tauri::chat_draft_list,
            $crate::commands_tauri::chat_draft_save,
            $crate::commands_tauri::chat_message_forward,
            $crate::commands_tauri::chat_message_forward_merge,
            $crate::commands_tauri::chat_notification_get_conversation,
            $crate::commands_tauri::chat_notification_get_dnd,
            $crate::commands_tauri::chat_notification_list_muted,
            $crate::commands_tauri::chat_notification_mute,
            $crate::commands_tauri::chat_notification_set_conversation,
            $crate::commands_tauri::chat_notification_set_dnd,
            $crate::commands_tauri::chat_notification_unmute,
            $crate::commands_tauri::chat_reaction_add,
            $crate::commands_tauri::chat_reaction_get,
            $crate::commands_tauri::chat_reaction_remove,
            $crate::commands_tauri::chat_tap,
            $crate::commands_tauri::chat_thread_get,
            $crate::commands_tauri::chat_thread_list,
            $crate::commands_tauri::contact_add,
            $crate::commands_tauri::contact_get,
            $crate::commands_tauri::contact_remove,
            $crate::commands_tauri::contact_search,
            $crate::commands_tauri::contact_toggle_favorite,
            $crate::commands_tauri::contact_update,
            $crate::commands_tauri::device_get,
            $crate::commands_tauri::device_get_current,
            $crate::commands_tauri::device_list,
            $crate::commands_tauri::device_register,
            $crate::commands_tauri::device_revoke,
            $crate::commands_tauri::device_set_primary,
            $crate::commands_tauri::device_touch,
            $crate::commands_tauri::e2e_handshake_complete,
            $crate::commands_tauri::e2e_handshake_initiate,
            $crate::commands_tauri::e2e_handshake_is_complete,
            $crate::commands_tauri::e2e_handshake_needs_rehandshake,
            $crate::commands_tauri::e2e_handshake_respond,
            $crate::commands_tauri::group_dissolve,
            $crate::commands_tauri::group_leave,
            $crate::commands_tauri::group_list,
            $crate::commands_tauri::group_list_muted,
            $crate::commands_tauri::group_member_get,
            $crate::commands_tauri::group_members,
            $crate::commands_tauri::group_mention_parse,
            $crate::commands_tauri::group_metadata_update,
            $crate::commands_tauri::group_mute_all,
            $crate::commands_tauri::group_mute_member,
            $crate::commands_tauri::group_nickname_get,
            $crate::commands_tauri::group_nickname_list,
            $crate::commands_tauri::group_nickname_set,
            $crate::commands_tauri::group_transfer_ownership,
            $crate::commands_tauri::group_unmute_all,
            $crate::commands_tauri::group_unmute_member,
            $crate::commands_tauri::moments_block,
            $crate::commands_tauri::moments_blocklist_list,
            $crate::commands_tauri::moments_comment_delete,
            $crate::commands_tauri::moments_comment_edit,
            $crate::commands_tauri::moments_report,
            $crate::commands_tauri::moments_share,
            $crate::commands_tauri::moments_unblock,
            $crate::commands_tauri::moments_unreact,
            $crate::commands_tauri::pairing_code_create,
            $crate::commands_tauri::pairing_code_parse,
            $crate::commands_tauri::pairing_health,
            $crate::commands_tauri::pairing_invitation_accept,
            $crate::commands_tauri::pairing_invitation_create,
            $crate::commands_tauri::pairing_invitation_parse,
            $crate::commands_tauri::pairing_invitation_revoke,
            $crate::commands_tauri::pairing_invitation_verify,
            $crate::commands_tauri::pairing_trusted_get,
            $crate::commands_tauri::pairing_trusted_list,
            $crate::commands_tauri::pairing_trusted_revoke,
            $crate::commands_tauri::profile_avatar_get,
            $crate::commands_tauri::profile_avatar_remove,
            $crate::commands_tauri::profile_avatar_upload,
            $crate::commands_tauri::profile_kind_get,
            $crate::commands_tauri::profile_kind_set,
            $crate::commands_tauri::profile_public_key_label,
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
