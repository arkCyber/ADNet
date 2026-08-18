//! Tauri command set — every menu, button, and form in the UI.
//!
//! This module is the **complete bridge** between the
//! `a3chat-tauri` React frontend and the `a3chatd` daemon. It
//! contains:
//!
//! - [`error`] — structured error type that maps to UI flashes /
//!   toasts / field-level validators.
//! - [`state`] — shared application state across windows.
//! - [`ops`] — top-level session / daemon ops (login, logout,
//!   doctor, menu bar, sidebar tree).
//! - [`rcp`] — typed wrappers for every a3chat RPC method.
//! - [`catalog`] — static catalogue of every command, so the
//!   frontend generator can introspect the surface.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod catalog;
pub mod error;
pub mod ops;
pub mod rcp;
pub mod state;

pub use catalog::{CommandCatalogEntry, COMMAND_CATALOG};
pub use error::{ErrorClass, FieldError, TauriCommandError, TauriCommandResult};
pub use ops::{
    AppVersion, CancelHandle, DoctorReport, MenuItem, OpResult, SessionInfo, StartDaemonRequest,
    TerminalEndpoint, TopLevelMenu, TreeNode, app_version, command_cancel, doctor, login,
    logout, menu_bar, session_info, sidebar_tree, start_daemon, stop_daemon,
};
pub use rcp::{
    A3chatCommandSet, AsyncCommand, CommandSet, RcpCommandExecutor, RpcRequest, RpcResponse,
    chat_conversation_list, chat_conversation_open, chat_message_ack, chat_message_delete,
    chat_message_edit, chat_message_recall, chat_message_send, chat_search, chat_sync_compressed,
    chat_sync_delta, chat_sync_snapshot, chat_typing, contact_accept_request, contact_add_request,
    contact_block, contact_list, contact_qr_invite, contact_unblock, e2e_bundle_export,
    e2e_bundle_import, group_announcement_set, group_create, group_invite, group_join,
    group_member_add, group_member_remove, group_member_role, healthz, media_download_get,
    media_health, media_upload_chunk, media_upload_finalize, media_upload_init, moderation_check_attachment,
    moderation_check_content, moderation_list_blocked, moderation_set_deny_default, moderation_stats,
    presence_publish, presence_subscribe, profile_avatar_set, profile_device_list,
    profile_device_register, profile_digit_get, profile_get, profile_preferences_put,
    profile_public_key_add, profile_public_key_list, profile_public_key_revoke, profile_put,
    rpc_health, stream_list, stream_subscribe, stream_unsubscribe,
};
pub use state::{AppState, AppStateBuilder, Screen, ViewModel};
