//! `adnet-mail` — SMTP/IMAP client + spam filtering for the ADNet workspace.
//!
//! ## Scope (vs. chatmail@core)
//!
//! `chatmail@core` (Delta Chat) is a 95-file, ~14 000-line crate that
//! does everything: SQLite-backed chat state, OpenPGP / Autocrypt /
//! SecureJoin, P2P Iroh, QR-code provisioning, webxdc, and on top of
//! that a full SMTP/IMAP/MIME implementation. This crate deliberately
//! copies **only** the email-transport subset, and only what the ADNet
//! workspace actually needs:
//!
//! | chatmail@core (kept)               | chatmail@core (dropped)                |
//! |------------------------------------|----------------------------------------|
//! | `smtp.rs` / `smtp/connect.rs` etc. | `pgp.rs`, `securejoin.rs`, `aheader.rs` |
//! | `imap.rs` / `imap/{client,idle,fetch}.rs` | `provider.rs` (provider-db)    |
//! | `mimefactory.rs` / `mimeparser.rs` | `chat.rs`, `contact.rs`, `context.rs`   |
//! | `login_param.rs`                   | `peer_channels.rs`, `webxdc/`, `ephemeral/`, `receive_imf.rs` |
//! | `transport.rs` (Socket enum)       | `scheduler.rs`, `securejoin.rs`         |
//!
//! ## New ADNet-specific modules
//!
//! - [`spam`] — Client-side spam filtering with header/content analysis.
//! - [`user_integration`] — Bridge to ADNet identity and user systems.
//!
//! ## Crate layout
//!
//! - [`error`] — typed errors with DO-178C-style recoverability
//!   classification (`UserError` / `Recoverable` / `Fatal`).
//! - [`login_param`] — `Account` (server config) + helpers.
//! - [`mime`] — `Mail` struct: parse / emit RFC 5322 wire bytes.
//! - [`imap`] — IMAP connect / IDLE / fetch.
//! - [`smtp`] — SMTP connect / send.
//! - [`account`] — `MailAccount` high-level facade (both transports).
//! - [`spam`] — Spam filtering with multi-signal analysis.
//! - [`user_integration`] — ADNet identity and user system integration.
//!
//! See `examples/` for a runnable end-to-end demo.

#![warn(unused_must_use)]

pub mod account;
pub mod error;
pub mod imap;
pub mod login_param;
pub mod mime;
pub mod provider;
pub mod retry;
pub mod smtp;
pub mod spam;
pub mod user_integration;
mod tls_danger;

/// Convenience re-exports.
pub mod prelude {
    pub use crate::account::{MailAccount, MailAccountBuilder, MailAccountOnline};
    pub use crate::error::{ErrorClass, MailError, Result};
    pub use crate::imap::{FetchHandle, FetchedMessage, IdleEvent, IdleHandle, ImapSession};
    pub use crate::login_param::{
        Account, CertificateChecks, ImapLoginParam, SmtpLoginParam, SocketSecurity,
        is_valid_address,
    };
    pub use crate::mime::{Address, Attachment, Disposition, Mail};
    pub use crate::provider::{BUILTIN_PROVIDERS, Provider, ServerTemplate, auto_configure};
    pub use crate::retry::{RetryPolicy, send_with_retry, send_with_retry_infallible};
    pub use crate::smtp::{SendOutcome, connect as smtp_connect, send as smtp_send};
    pub use crate::spam::{SpamFilter, SpamFilterConfig, SpamScore, SpamSignals};
    pub use crate::user_integration::{EmailIdentity, IdentityResolver};
}

// Bring `Result` into the crate root namespace.
pub use error::{ErrorClass, MailError, Result};
