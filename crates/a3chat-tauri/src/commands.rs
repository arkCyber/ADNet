//! Tauri command set + minimal two-window bootstrapper.
//!
//! These are the pure-logic command handlers — they take a borrowed
//! [`A3chatClient`] and a JSON payload, return JSON. When wired into
//! a real Tauri builder (see [`run_minimal_two_window`]) they become
//! `#[tauri::command]` entry points that the React frontend calls
//! via `invoke()`.
//!
//! The two-window bootstrap opens one "login" window (placeholder
//! HTML) and a "chats" window. P1 ships this without an actual
//! frontend bundle — the bootstrap will refuse to run unless
//! `tauri = { ... feature = "desktop" }` is enabled, which is what
//! keeps `cargo test` fast.

use serde::{Deserialize, Serialize};

use a3chat_core::error::A3chatError;
use a3chat_core::id::UserId;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::client::{A3chatClient, A3chatClientConfig, ping};

// -- Payload types ---------------------------------------------------------

/// Returned by [`login_bootstrap`] — the client config that the
/// Chats window needs to keep talking to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginBootstrapPayload {
    /// Echo of the owner id — useful for the UI to render "Logged in
    /// as Alice".
    pub owner: UserId,
    /// Confirmed base URL (the daemon is reachable).
    pub base_url: String,
    /// Health payload echoed straight from `/rpc/health`.
    pub health: serde_json::Value,
}

/// Returned by [`chats_bootstrap`] — the list of conversations the
/// UI should render in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatsBootstrapPayload {
    pub owner: UserId,
    pub conversations: serde_json::Value,
}

// -- Command handlers ------------------------------------------------------

/// Login bootstrap — pings the daemon's `/rpc/health` to confirm
/// reachability and returns the client config the Chats window
/// needs. Pure function: no Tauri runtime involved, easy to unit test.
pub async fn login_bootstrap(
    base_url: impl Into<String>,
    owner: UserId,
) -> Result<LoginBootstrapPayload, A3chatError> {
    let cfg = A3chatClientConfig::new(base_url, owner.clone());
    let health = ping(&cfg).await?;
    Ok(LoginBootstrapPayload {
        owner,
        base_url: cfg.base_url.clone(),
        health,
    })
}

/// Chats bootstrap — issues the first chat.* RPC call to populate the
/// conversation list. Pure function, no Tauri runtime involved.
pub async fn chats_bootstrap(client: &A3chatClient) -> Result<ChatsBootstrapPayload, A3chatError> {
    let conversations = client
        .call(
            A3chatRpcMethod::CHAT_CONVERSATION_LIST,
            serde_json::json!({}),
        )
        .await?;
    Ok(ChatsBootstrapPayload {
        owner: client.config().owner.clone(),
        conversations,
    })
}

// -- Minimal two-window Tauri bootstrap -------------------------------------

/// Configuration for [`run_minimal_two_window`].
#[derive(Debug, Clone)]
pub struct TwoWindowConfig {
    pub base_url: String,
    pub owner: UserId,
}

#[cfg(feature = "desktop")]
/// Open two Tauri windows pointing at placeholder HTML:
///   * window 1 — Login (calls `/rpc/health` via the embedded client)
///   * window 2 — Chats (calls `a3chat.chat.conversation.list`)
///
/// On success returns the [`ChatsBootstrapPayload`] so the caller can
/// confirm the wiring end-to-end. The actual React frontend lives in
/// P1.1 — this function exists so the desktop shell can be smoke-
/// tested without any frontend code.
pub async fn run_minimal_two_window(
    cfg: TwoWindowConfig,
) -> Result<ChatsBootstrapPayload, Box<dyn std::error::Error>> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

    let payload = login_bootstrap(cfg.base_url.clone(), cfg.owner.clone()).await?;

    let app = tauri::Builder::default()
        .setup(move |app| {
            // Window 1 — Login. Uses the embedded placeholder HTML.
            let _login =
                WebviewWindowBuilder::new(app, "login", WebviewUrl::App("login.html".into()))
                    .title("a3chat — Login")
                    .inner_size(360.0, 540.0)
                    .build()?;

            // Window 2 — Chats.
            let _chats =
                WebviewWindowBuilder::new(app, "chats", WebviewUrl::App("chats.html".into()))
                    .title("a3chat — Chats")
                    .inner_size(960.0, 640.0)
                    .build()?;

            Ok(())
        })
        .build(tauri::generate_context!())?;

    let client = A3chatClient::new(A3chatClientConfig::new(
        payload.base_url.clone(),
        payload.owner.clone(),
    ));
    let chats_payload = chats_bootstrap(&client).await?;

    // We deliberately don't `.run()` the app here — this function is
    // designed for tests that want to verify the bootstrap chain
    // without blocking on a UI loop. Production callers should use
    // `tauri::Builder::default().run(...)` directly.
    let _ = app;

    Ok(chats_payload)
}

#[cfg(not(feature = "desktop"))]
/// Disabled stub — kept so `cargo check` / `cargo test` work without
/// the optional Tauri dependency.
pub async fn run_minimal_two_window(
    _cfg: TwoWindowConfig,
) -> Result<ChatsBootstrapPayload, Box<dyn std::error::Error>> {
    Err(Box::new(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "a3chat-tauri built without the `desktop` feature — re-enable it to use Tauri",
    )))
}

// -- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_app::A3chatApp;
    use a3chat_app::storage::StorageConfig;
    use a3chat_rpc::{RpcServer, RpcServerConfig};

    fn owner() -> UserId {
        UserId::from("alice")
    }

    #[tokio::test]
    async fn login_bootstrap_pings_health() {
        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        app.init_user(&owner()).await.unwrap();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let payload = login_bootstrap(&base, owner()).await.unwrap();
        assert_eq!(payload.owner, owner());
        assert_eq!(payload.health["status"], "ok");
        handle.stop().await;
    }

    #[tokio::test]
    async fn chats_bootstrap_returns_empty_array_initially() {
        let dir = tempfile::tempdir().unwrap();
        let app = A3chatApp::new(StorageConfig::new(dir.path().to_path_buf()), owner()).unwrap();
        app.init_user(&owner()).await.unwrap();
        let server = RpcServer::new(app, RpcServerConfig::default());
        let handle = server.start().await.unwrap();
        let base = format!("http://{}", handle.local_addr);

        let client = A3chatClient::new(A3chatClientConfig::new(&base, owner()));
        let payload = chats_bootstrap(&client).await.unwrap();
        assert!(payload.conversations.is_array());
        assert_eq!(payload.conversations.as_array().unwrap().len(), 0);
        handle.stop().await;
    }

    #[tokio::test]
    async fn login_bootstrap_fails_against_unreachable_daemon() {
        let r = login_bootstrap("http://127.0.0.1:1", owner()).await;
        assert!(r.is_err());
    }

    #[test]
    fn two_window_config_round_trips() {
        let cfg = TwoWindowConfig {
            base_url: "http://127.0.0.1:53421".into(),
            owner: owner(),
        };
        assert_eq!(cfg.base_url, "http://127.0.0.1:53421");
        assert_eq!(cfg.owner, owner());
    }

    #[tokio::test]
    async fn run_minimal_two_window_without_desktop_feature_returns_error() {
        // We always run with `default` features only — the stub
        // branch is the one under test here.
        let r = run_minimal_two_window(TwoWindowConfig {
            base_url: "http://127.0.0.1:53421".into(),
            owner: owner(),
        })
        .await;
        assert!(r.is_err(), "expected stub to error without desktop feature");
    }
}
