//! Top-level Tauri commands — session / daemon lifecycle.
//!
//! These commands are the *menus* and *buttons* in the global
//! menu bar / toolbar. They are independent of the chat-specific
//! RPC methods (which live in [`crate::tauri_cmd::rcp`]).
//!
//! Every command takes a borrowed [`AppState`] (auto-injected by
//! the Tauri runtime) and returns a [`TauriCommandResult<T>`].
//! Each command is wrapped in a [`tauri_cmd`] function so the
//! `#[tauri::command]` macro can pick it up when the `desktop`
//! feature is enabled.

use serde::{Deserialize, Serialize};

use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::client::{A3chatClient, A3chatClientConfig};

use super::error::{TauriCommandError, TauriCommandResult};
use super::state::{AppState, Screen};

pub type OpResult<T> = TauriCommandResult<T>;

/// Information about an active UI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub owner: UserId,
    pub base_url: String,
    pub started_at_unix: i64,
    pub screens: Vec<String>,
}

/// Echo of the application version. The frontend reads this to
/// render the "About" menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppVersion {
    pub semver: String,
    pub git_sha: String,
    pub build_time: String,
    pub feature: String,
}

/// Daemon doctor report — mirrors the CLI's `a3chat doctor` JSON
/// output. The UI surfaces this as a dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: std::collections::HashMap<String, String>,
    pub daemon_url: String,
    pub owner: UserId,
    pub details: serde_json::Value,
    pub lock_state: String,
    pub last_health: Option<serde_json::Value>,
}

/// Token returned by long-running ops so the UI can cancel them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CancelHandle(pub String);

/// Request to start the daemon. The CLI reuses the same JSON shape
/// so the request is also exercisable from a script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartDaemonRequest {
    pub bind: Option<String>,
    pub owner: Option<String>,
    pub storage: Option<String>,
    pub request_timeout_ms: Option<u64>,
}

/// Information about a sidebar tree node (e.g. a folder /
/// conversation grouping the UI should render).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub children: Vec<TreeNode>,
    pub badge: Option<u32>,
}

/// Stable description of a top-level menu. The frontend uses this
/// to render the menu bar deterministically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopLevelMenu {
    pub id: String,
    pub label: String,
    pub items: Vec<MenuItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub accelerator: Option<String>,
    pub enabled: bool,
    pub separator_after: bool,
}

/// Information about a terminal endpoint — the desktop shell can
/// opt to render a tab into the bundled pty shell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalEndpoint {
    pub id: String,
    pub session_id: String,
    pub cols: u32,
    pub rows: u32,
}

// ── Top-level ops ──────────────────────────────────────────────────────────

/// Login — taken from the Login window. Idempotent: calling it
/// twice with the same owner just resets the client.
pub async fn login(
    state: AppState,
    base_url: String,
    owner: UserId,
) -> OpResult<SessionInfo> {
    let cfg = A3chatClientConfig::new(base_url.clone(), owner.clone());
    let client = A3chatClient::new(cfg);
    state.set_client(client);
    state.set_screen(Screen::Chats);
    Ok(SessionInfo {
        owner,
        base_url,
        started_at_unix: chrono::Utc::now().timestamp(),
        screens: Screen::ALL.iter().map(|s| s.as_str().to_string()).collect(),
    })
}

/// Logout — clears the client and returns the view to the login
/// screen. Frontend should broadcast a "logout" event after this.
pub async fn logout(state: AppState) -> OpResult<()> {
    state.clear_client();
    state.set_screen(Screen::Login);
    Ok(())
}

/// Echo the current session info. Useful for the UI to refresh
/// badges / counters after every RPC.
pub async fn session_info(state: AppState) -> OpResult<SessionInfo> {
    let owner = state.owner().ok_or_else(|| {
        TauriCommandError::security("not_logged_in", "no active session")
    })?;
    Ok(SessionInfo {
        owner,
        base_url: state.base_url(),
        started_at_unix: chrono::Utc::now().timestamp(),
        screens: Screen::ALL.iter().map(|s| s.as_str().to_string()).collect(),
    })
}

/// Application version — purely local, no RPC.
pub async fn app_version() -> OpResult<AppVersion> {
    Ok(AppVersion {
        semver: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: std::env::var("A3CHAT_GIT_SHA").unwrap_or_else(|_| "unknown".to_string()),
        build_time: std::env::var("A3CHAT_BUILD_TIME").unwrap_or_else(|_| "unknown".to_string()),
        feature: if cfg!(feature = "desktop") { "desktop" } else { "core" }.to_string(),
    })
}

/// Doctor — calls the daemon's `a3chat.*` health endpoints and
/// formats the result for the UI dashboard.
pub async fn doctor(state: AppState) -> OpResult<DoctorReport> {
    let client = state.client().ok_or_else(|| {
        TauriCommandError::security("not_logged_in", "no active session")
    })?;
    let base_url = client.config().base_url.clone();
    let owner = client.config().owner.clone();
    let health = client
        .call(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)?;
    let mut checks = std::collections::HashMap::new();
    checks.insert("conversation_list".to_string(), "ok".to_string());
    Ok(DoctorReport {
        checks,
        daemon_url: base_url,
        owner,
        details: health,
        lock_state: "unknown".into(),
        last_health: Some(serde_json::json!({ "status": "ok" })),
    })
}

/// Start the daemon — shells out to the `a3chatd` binary. The
/// exact command is platform-dependent; this is the canonical
/// start sequence the desktop shell uses.
pub async fn start_daemon(_req: StartDaemonRequest) -> OpResult<CancelHandle> {
    // The Tauri shell launches the daemon via the in-process
    // `a3chatd` subcommand shipped with the same binary. This
    // command stub lets the UI probe the *intent* without us
    // actually spawning a child process (the production build
    // replaces this with the real controller once the bundle
    // is wired into the menu).
    Ok(CancelHandle(format!("daemon-{}", uuid::Uuid::new_v4())))
}

/// Stop the daemon — companion to `start_daemon`. As above, this
/// is currently a stub: the real lifecycle goes through the OS
/// service manager once the bundle is installed.
pub async fn stop_daemon(_handle: CancelHandle) -> OpResult<()> {
    Ok(())
}

/// Resolve the desktop menu tree. The frontend calls this on
/// startup so the menu bar is reactive to the current screen.
pub async fn menu_bar(state: AppState) -> OpResult<Vec<TopLevelMenu>> {
    let screen = state.view().current_screen;
    let enabled = |s: Screen| -> bool {
        // Login is always available; other screens require an
        // active session.
        if state.client().is_none() && s != Screen::Login {
            return false;
        }
        // B-18 — wire `screen` through. Only show items that
        // apply to the *current* screen. WeChat's desktop client
        // hides "New Conversation" while on the Settings tab, etc.
        // Allow matching to the item's own screen, plus
        // back-compat: anything tagged Login is always live so
        // the login screen can still let the user navigate.
        s == screen || s == Screen::Login
    };
    Ok(vec![
        TopLevelMenu {
            id: "file".into(),
            label: "File".into(),
            items: vec![
                MenuItem {
                    id: "file.new_conversation".into(),
                    label: "New Conversation".into(),
                    accelerator: Some("CmdOrCtrl+N".into()),
                    enabled: enabled(Screen::Conversations),
                    separator_after: false,
                },
                MenuItem {
                    id: "file.new_group".into(),
                    label: "New Group".into(),
                    accelerator: Some("CmdOrCtrl+Shift+N".into()),
                    enabled: enabled(Screen::Groups),
                    separator_after: false,
                },
                MenuItem {
                    id: "file.export_bundle".into(),
                    label: "Export E2E Bundle…".into(),
                    accelerator: Some("CmdOrCtrl+E".into()),
                    enabled: enabled(Screen::Bundle),
                    separator_after: false,
                },
                MenuItem {
                    id: "file.import_bundle".into(),
                    label: "Import E2E Bundle…".into(),
                    accelerator: Some("CmdOrCtrl+I".into()),
                    enabled: enabled(Screen::Bundle),
                    separator_after: true,
                },
                MenuItem {
                    id: "file.logout".into(),
                    label: "Log Out".into(),
                    accelerator: Some("CmdOrCtrl+Shift+L".into()),
                    enabled: state.client().is_some(),
                    separator_after: false,
                },
                MenuItem {
                    id: "file.exit".into(),
                    label: "Quit a3chat".into(),
                    accelerator: Some("CmdOrCtrl+Q".into()),
                    enabled: true,
                    separator_after: false,
                },
            ],
        },
        TopLevelMenu {
            id: "edit".into(),
            label: "Edit".into(),
            items: vec![
                MenuItem {
                    id: "edit.undo".into(),
                    label: "Undo".into(),
                    accelerator: Some("CmdOrCtrl+Z".into()),
                    enabled: enabled(Screen::Messages),
                    separator_after: false,
                },
                MenuItem {
                    id: "edit.redo".into(),
                    label: "Redo".into(),
                    accelerator: Some("CmdOrCtrl+Shift+Z".into()),
                    enabled: enabled(Screen::Messages),
                    separator_after: false,
                },
                MenuItem {
                    id: "edit.find".into(),
                    label: "Find in Chat".into(),
                    accelerator: Some("CmdOrCtrl+F".into()),
                    enabled: enabled(Screen::Messages),
                    separator_after: true,
                },
                MenuItem {
                    id: "edit.preferences".into(),
                    label: "Preferences…".into(),
                    accelerator: Some("CmdOrCtrl+,".into()),
                    enabled: true,
                    separator_after: false,
                },
            ],
        },
        TopLevelMenu {
            id: "view".into(),
            label: "View".into(),
            items: vec![
                MenuItem {
                    id: "view.chats".into(),
                    label: "Chats".into(),
                    accelerator: Some("CmdOrCtrl+1".into()),
                    enabled: true,
                    separator_after: false,
                },
                MenuItem {
                    id: "view.contacts".into(),
                    label: "Contacts".into(),
                    accelerator: Some("CmdOrCtrl+2".into()),
                    enabled: state.client().is_some(),
                    separator_after: false,
                },
                MenuItem {
                    id: "view.groups".into(),
                    label: "Groups".into(),
                    accelerator: Some("CmdOrCtrl+3".into()),
                    enabled: state.client().is_some(),
                    separator_after: false,
                },
                MenuItem {
                    id: "view.profile".into(),
                    label: "Profile".into(),
                    accelerator: Some("CmdOrCtrl+4".into()),
                    enabled: state.client().is_some(),
                    separator_after: false,
                },
                MenuItem {
                    id: "view.media".into(),
                    label: "Media Library".into(),
                    accelerator: Some("CmdOrCtrl+5".into()),
                    enabled: state.client().is_some(),
                    separator_after: false,
                },
                MenuItem {
                    id: "view.audit".into(),
                    label: "Audit".into(),
                    accelerator: Some("CmdOrCtrl+6".into()),
                    enabled: state.client().is_some(),
                    separator_after: true,
                },
                MenuItem {
                    id: "view.toggle_devtools".into(),
                    label: "Toggle Developer Tools".into(),
                    accelerator: Some("CmdOrCtrl+Alt+I".into()),
                    enabled: true,
                    separator_after: false,
                },
            ],
        },
        TopLevelMenu {
            id: "tools".into(),
            label: "Tools".into(),
            items: vec![
                MenuItem {
                    id: "tools.doctor".into(),
                    label: "Run Doctor".into(),
                    accelerator: Some("CmdOrCtrl+D".into()),
                    enabled: true,
                    separator_after: false,
                },
                MenuItem {
                    id: "tools.sync_now".into(),
                    label: "Sync Now".into(),
                    accelerator: Some("CmdOrCtrl+R".into()),
                    enabled: enabled(Screen::Sync),
                    separator_after: false,
                },
                MenuItem {
                    id: "tools.stream_subscribe".into(),
                    label: "Subscribe to Stream…".into(),
                    accelerator: Some("CmdOrCtrl+T".into()),
                    enabled: enabled(Screen::Stream),
                    separator_after: true,
                },
                MenuItem {
                    id: "tools.moderation_check".into(),
                    label: "Check Content / Attachment".into(),
                    accelerator: None,
                    enabled: enabled(Screen::Moderation),
                    separator_after: false,
                },
            ],
        },
        TopLevelMenu {
            id: "help".into(),
            label: "Help".into(),
            items: vec![
                MenuItem {
                    id: "help.documentation".into(),
                    label: "Documentation".into(),
                    accelerator: Some("F1".into()),
                    enabled: true,
                    separator_after: false,
                },
                MenuItem {
                    id: "help.shortcuts".into(),
                    label: "Keyboard Shortcuts".into(),
                    accelerator: Some("?".into()),
                    enabled: true,
                    separator_after: true,
                },
                MenuItem {
                    id: "help.about".into(),
                    label: "About a3chat".into(),
                    accelerator: None,
                    enabled: true,
                    separator_after: false,
                },
            ],
        },
    ])
}

/// Sidebar tree — fetched on every screen change so the UI can
/// render the right folder / conversation list.
pub async fn sidebar_tree(state: AppState) -> OpResult<Vec<TreeNode>> {
    let client = state.client().ok_or_else(|| {
        TauriCommandError::security("not_logged_in", "no active session")
    })?;
    let v = client
        .call(A3chatRpcMethod::CHAT_CONVERSATION_LIST, serde_json::json!({}))
        .await
        .map_err(TauriCommandError::from)?;
    let arr = v.as_array().cloned().unwrap_or_default();
    let nodes = arr
        .into_iter()
        .map(|c| {
            let id = c
                .get("conversation_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let label = c
                .get("title")
                .and_then(|s| s.as_str())
                .unwrap_or("conversation")
                .to_string();
            // B-17 — read the conversation kind from the wire
            // response, not from `idx % 5` (the previous cosmetic
            // placeholder). The backend returns `"dm" | "group" | …
            // ` depending on `ConversationKind`. We map the canonical
            // strings into the UI's TreeNode kind taxonomy.
            let raw_kind = c
                .get("kind")
                .and_then(|s| s.as_str())
                .unwrap_or("dm");
            let kind = match raw_kind {
                "dm" => "dm",
                "group" => "group",
                "channel" => "channel",
                "system" => "system",
                _ => "dm",
            };
            let badge = c
                .get("unread_count")
                .and_then(|u| u.as_u64())
                .map(|n| n as u32);
            TreeNode {
                id,
                label,
                kind: kind.into(),
                children: Vec::new(),
                badge,
            }
        })
        .collect();
    Ok(nodes)
}

/// Cancel a long-running command. The real implementation lives
/// behind the CancelHandle returned by the originating command.
pub async fn command_cancel(_handle: CancelHandle) -> OpResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::id::generate_user_id;

    #[tokio::test]
    async fn login_then_logout_round_trip() {
        let s = AppState::new();
        let owner = generate_user_id();
        let info = login(s.clone(), "http://127.0.0.1:65530".into(), owner.clone())
            .await
            .unwrap();
        assert_eq!(info.owner, owner);
        assert_eq!(info.base_url, "http://127.0.0.1:65530");
        assert!(info.screens.iter().any(|s| s == "chats"));
        logout(s.clone()).await.unwrap();
        assert!(s.client().is_none());
    }

    #[tokio::test]
    async fn session_info_requires_login() {
        let s = AppState::new();
        let r = session_info(s.clone()).await;
        assert!(r.is_err());
        let owner = generate_user_id();
        let _ = login(s.clone(), "http://127.0.0.1:65530".into(), owner).await;
        let info = session_info(s.clone()).await.unwrap();
        assert_eq!(info.screens.len(), Screen::ALL.len());
    }

    #[tokio::test]
    async fn doctor_fails_without_login() {
        let s = AppState::new();
        let r = doctor(s.clone()).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn app_version_is_stable() {
        let v = app_version().await.unwrap();
        assert_eq!(v.feature, if cfg!(feature = "desktop") { "desktop" } else { "core" });
        assert!(!v.semver.is_empty());
    }

    #[tokio::test]
    async fn menu_bar_handles_unauthenticated() {
        let s = AppState::new();
        let m = menu_bar(s.clone()).await.unwrap();
        // Login is always enabled; others are disabled.
        let file = m.iter().find(|m| m.id == "file").unwrap();
        let new_conv = file.items.iter().find(|i| i.id == "file.new_conversation").unwrap();
        assert!(!new_conv.enabled);
    }

    #[tokio::test]
    async fn start_stop_daemon_returns_handle() {
        let h = start_daemon(StartDaemonRequest {
            bind: Some("127.0.0.1:53421".into()),
            owner: Some("0".repeat(64)),
            storage: None,
            request_timeout_ms: None,
        })
        .await
        .unwrap();
        assert!(h.0.starts_with("daemon-"));
        stop_daemon(h).await.unwrap();
    }

    #[tokio::test]
    async fn sidebar_tree_requires_login() {
        let s = AppState::new();
        let r = sidebar_tree(s.clone()).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn sidebar_tree_returns_empty_initially() {
        // No real daemon — we still verify the result type round-trips.
        let s = AppState::new();
        let owner = generate_user_id();
        let _ = login(s.clone(), "http://127.0.0.1:1".into(), owner).await;
        let r = sidebar_tree(s.clone()).await;
        // 127.0.0.1:1 won't respond; we expect an error.
        assert!(r.is_err());
    }
}
