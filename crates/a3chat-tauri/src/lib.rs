//! `a3chat-tauri` — desktop shell wrapper.
//!
//! P1 ships two surfaces:
//!
//! * [`client`] — a thin HTTP JSON-RPC client used by frontends and
//!   the [`commands`] module to talk to a running `a3chat-rpc`
//!   daemon.
//!
//! * [`commands`] — the bare-minimum set of Tauri commands (just
//!   Rust fn bodies — no React/web frontend yet) that a UI would
//!   `invoke()`. P1 wires the *first* two-window happy path:
//!
//!   ```text
//!   ┌──────────────────┐    ┌──────────────────┐
//!   │ Window 1: Login  │ →→ │ Window 2: Chats  │
//!   └──────────────────┘    └──────────────────┘
//!   ```
//!
//!   Login calls `a3chat.profile.whoami`, then Chats calls
//!   `a3chat.chat.conversation.list`. P1 has no real React frontend
//!   yet — the Tauri builder in [`run_minimal_two_window`] opens two
//!   `WebviewWindow`s pointing at bundled placeholder HTML so the
//!   wiring can be smoke-tested end-to-end without Node / npm.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod client;
pub mod commands;

pub use client::A3chatClient;
pub use commands::{ChatsBootstrapPayload, LoginBootstrapPayload, run_minimal_two_window};
