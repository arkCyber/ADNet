//! Strongly typed Tauri command surface.
//!
//! Every menu, button, and form in the a3chat desktop UI eventually
//! calls one of these commands. The contract is intentionally narrow:
//! each command takes a typed argument struct, returns a typed
//! result, and degrades gracefully on transport / parse errors so the
//! UI can render a meaningful error rather than crashing.
//!
//! ## DO-178C mapping
//!
//! * **§5.2 — Traceability.** Every command records a
//!   `X-A3Chat-Request-Id` so the daemon logs and the UI toast logs
//!   can be correlated.
//! * **§6.1 — Determinism.** Pure functions only — no hidden state
//!   inside the command body. Any stateful concern lives in
//!   [`crate::state::AppState`] and is passed by reference.
//! * **§6.3 — Fail-safe.** Every command returns
//!   `Result<T, TauriCommandError>`; the wrapper
//!   [`tauri::command`](tauri::command) macro converts the error
//!   into a serializable payload the frontend can render.
//!
//! ## Coverage
//!
//! 52 RPC methods × 1 typed command per method, plus 12 session /
//! daemon ops (login, logout, doctor, start/stop, settings, etc).
//! The complete matrix is enumerated in
//! [`COMMAND_CATALOG`](self::catalog::COMMAND_CATALOG) so the frontend
//! generator can introspect it.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod catalog;
pub mod error;
pub mod ops;
pub mod rcp;
pub mod state;

pub use catalog::{CommandCatalogEntry, COMMAND_CATALOG};
pub use error::{TauriCommandError, TauriCommandResult};
pub use ops::{
    AppVersion, CancelHandle, DoctorReport, OpResult, SessionInfo, StartDaemonRequest,
    TerminalEndpoint, TopLevelMenu, TreeNode, command_cancel,
};
pub use rcp::{A3chatCommandSet, AsyncCommand, CommandSet, RcpCommandExecutor};
pub use state::{AppState, AppStateBuilder, Screen, ViewModel};
