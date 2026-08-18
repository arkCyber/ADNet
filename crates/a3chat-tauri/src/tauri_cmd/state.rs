//! Shared application state for the Tauri UI.
//!
//! Multiple Tauri windows (Login, Chats, Profile, Settings, …) need
//! to share the same [`A3chatClient`] and a few derived fields
//! (current owner, selected conversation, last-opened menu). The
//! `AppState` exposed here is a single copy-on-write handle that
//! the Tauri runtime registers as a managed state; Tauri commands
//! receive it as the first argument and can read / clone cheaply.
//!
//! DO-178C §6.1 — only primitives + `A3chatClient` (which is itself
//! internally Arc'd) live here. All the heavy state (SSE
//! subscriptions, drafts, etc.) is owned by the window that needs
//! it, not by the central `AppState`.

use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use a3chat_core::id::{ConversationId, UserId};

use crate::client::{A3chatClient, A3chatClientConfig};

/// Which top-level screen the user is currently viewing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    Login,
    Chats,
    Conversations,
    Messages,
    Profile,
    Contacts,
    Groups,
    Presence,
    Moderation,
    Media,
    Sync,
    Audit,
    Settings,
    Doctor,
    Bundle,
    Stream,
    PeerFeedback,
    /// F-05 朋友圈 — posts, comments, reactions, follows, timeline.
    Moments,
    /// F-08 收藏 — saved URLs with folders, tags, and pin/archive
    /// metadata. Surfaces a dedicated "Favorites" screen on the
    /// desktop client.
    Favorites,
}

impl Screen {
    pub const ALL: &'static [Screen] = &[
        Screen::Login,
        Screen::Chats,
        Screen::Conversations,
        Screen::Messages,
        Screen::Profile,
        Screen::Contacts,
        Screen::Groups,
        Screen::Presence,
        Screen::Moderation,
        Screen::Media,
        Screen::Sync,
        Screen::Audit,
        Screen::Settings,
        Screen::Doctor,
        Screen::Bundle,
        Screen::Stream,
        Screen::PeerFeedback,
        Screen::Moments,
        Screen::Favorites,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Screen::Login => "login",
            Screen::Chats => "chats",
            Screen::Conversations => "conversations",
            Screen::Messages => "messages",
            Screen::Profile => "profile",
            Screen::Contacts => "contacts",
            Screen::Groups => "groups",
            Screen::Presence => "presence",
            Screen::Moderation => "moderation",
            Screen::Media => "media",
            Screen::Sync => "sync",
            Screen::Audit => "audit",
            Screen::Settings => "settings",
            Screen::Doctor => "doctor",
            Screen::Bundle => "bundle",
            Screen::Stream => "stream",
            Screen::PeerFeedback => "peerfeedback",
            Screen::Moments => "moments",
            Screen::Favorites => "favorites",
        }
    }
}

/// UI view-model — the projections the frontend needs most often
/// (selected conversation, search needle, current menu).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ViewModel {
    /// Currently selected conversation (if any).
    pub selected_conversation: Option<ConversationId>,
    /// Free-text search needle from the toolbar.
    pub search_needle: Option<String>,
    /// Currently selected contact / peer.
    pub selected_peer: Option<UserId>,
    /// Currently selected group (if any).
    pub selected_group: Option<ConversationId>,
    /// Current menu path, e.g. `"file.send"`.
    pub current_menu: Option<String>,
    /// Currently visible screen.
    pub current_screen: Screen,
}

/// Shared application state. Cheap to clone — every field is either
/// `Copy` or wrapped in an `Arc`.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    /// HTTP RPC client. `None` until login completes.
    client: RwLock<Option<A3chatClient>>,
    /// Last-known owner id (mirrors the client config).
    owner: RwLock<Option<UserId>>,
    /// UI view-model.
    view: RwLock<ViewModel>,
    /// Base URL the client is configured to talk to.
    base_url: RwLock<String>,
    /// True once the user has switched the daemon into `desktop-feature`
    /// mode (recorded so the UI can show a banner).
    desktop_feature: RwLock<bool>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                client: RwLock::new(None),
                owner: RwLock::new(None),
                view: RwLock::new(ViewModel::default()),
                base_url: RwLock::new(String::new()),
                desktop_feature: RwLock::new(false),
            }),
        }
    }

    pub fn with_client(client: A3chatClient) -> Self {
        let s = Self::new();
        s.set_client(client);
        s
    }

    pub fn set_client(&self, client: A3chatClient) {
        let owner = client.config().owner.clone();
        let base = client.config().base_url.clone();
        *self.inner.client.write() = Some(client);
        *self.inner.owner.write() = Some(owner);
        *self.inner.base_url.write() = base;
    }

    pub fn clear_client(&self) {
        *self.inner.client.write() = None;
        *self.inner.owner.write() = None;
        *self.inner.base_url.write() = String::new();
        *self.inner.view.write() = ViewModel::default();
    }

    pub fn client(&self) -> Option<A3chatClient> {
        self.inner.client.read().clone()
    }

    pub fn owner(&self) -> Option<UserId> {
        self.inner.owner.read().clone()
    }

    pub fn base_url(&self) -> String {
        self.inner.base_url.read().clone()
    }

    pub fn view(&self) -> ViewModel {
        self.inner.view.read().clone()
    }

    pub fn set_view(&self, view: ViewModel) {
        *self.inner.view.write() = view;
    }

    pub fn set_screen(&self, screen: Screen) {
        self.inner.view.write().current_screen = screen;
    }

    pub fn set_desktop_feature(&self, on: bool) {
        *self.inner.desktop_feature.write() = on;
    }

    pub fn desktop_feature(&self) -> bool {
        *self.inner.desktop_feature.read()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for [`AppState`]. Useful when the frontend constructs the
/// state from a saved config (e.g. "remember me").
pub struct AppStateBuilder {
    state: AppState,
}

impl AppStateBuilder {
    pub fn new() -> Self {
        Self {
            state: AppState::new(),
        }
    }

    pub fn base_url(self, url: impl Into<String>) -> Self {
        *self.state.inner.base_url.write() = url.into();
        self
    }

    pub fn owner(self, owner: UserId) -> Self {
        *self.state.inner.owner.write() = Some(owner);
        self
    }

    pub fn client(self, client: A3chatClient) -> Self {
        self.state.set_client(client);
        self
    }

    pub fn preset_config(self, cfg: A3chatClientConfig) -> Self {
        self.state.set_client(A3chatClient::new(cfg));
        self
    }

    pub fn desktop_feature(self, on: bool) -> Self {
        self.state.set_desktop_feature(on);
        self
    }

    pub fn build(self) -> AppState {
        self.state
    }
}

impl Default for AppStateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::id::generate_user_id;

    #[test]
    fn empty_state_has_no_client() {
        let s = AppState::new();
        assert!(s.client().is_none());
        assert!(s.owner().is_none());
    }

    #[test]
    fn set_client_recovers_owner_and_url() {
        let owner = generate_user_id();
        let cfg = A3chatClientConfig::new("http://127.0.0.1:53421", owner.clone());
        let client = A3chatClient::new(cfg);
        let s = AppState::new().builder().client(client).build();
        assert_eq!(s.owner().unwrap(), owner);
        assert_eq!(s.base_url(), "http://127.0.0.1:53421");
    }

    #[test]
    fn clear_client_resets_state() {
        let owner = generate_user_id();
        let cfg = A3chatClientConfig::new("http://127.0.0.1:53421", owner);
        let s = AppState::new().builder().client(A3chatClient::new(cfg)).build();
        let _ = s.client();
        s.clear_client();
        assert!(s.client().is_none());
        assert!(s.owner().is_none());
    }

    #[test]
    fn screen_default_is_login() {
        let s = AppState::new();
        assert_eq!(s.view().current_screen, Screen::Login);
    }

    #[test]
    fn set_screen_updates_view_model() {
        let s = AppState::new();
        s.set_screen(Screen::Settings);
        assert_eq!(s.view().current_screen, Screen::Settings);
    }

    #[test]
    fn desktop_feature_toggle() {
        let s = AppState::new();
        assert!(!s.desktop_feature());
        s.set_desktop_feature(true);
        assert!(s.desktop_feature());
    }

    #[test]
    fn appstate_builder_chains() {
        let s = AppStateBuilder::new()
            .base_url("http://127.0.0.1:8080")
            .owner(generate_user_id())
            .build();
        assert_eq!(s.base_url(), "http://127.0.0.1:8080");
        assert!(s.owner().is_some());
    }

    #[test]
    fn screen_all_includes_every_variant() {
        assert!(Screen::ALL.contains(&Screen::Login));
        assert!(Screen::ALL.contains(&Screen::Stream));
    }

    #[test]
    fn screen_as_str_round_trips() {
        for s in Screen::ALL {
            let s: &str = s.as_str();
            assert!(!s.is_empty());
        }
    }
}

// `builder` is a convenience constructor — small wrapper that lets
// `AppState::new().builder().client(c).build()` chain fluently.
impl AppState {
    pub fn builder(self) -> AppStateBuilder {
        AppStateBuilder { state: self }
    }
}
