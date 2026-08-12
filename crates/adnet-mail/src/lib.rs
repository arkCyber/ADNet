//! `adnet-mail` — SMTP send + IMAP receive for the ADNet workspace.
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
//! The E2EE / Autocrypt layers live in chatmail@core unchanged. If a
//! downstream caller wants encrypted mail, they wire our crate's
//! `Mail::from_wire_bytes` / `Mail::to_wire_bytes` to whatever
//! decryption frontend they prefer (chatmail, rpgp, etc.).
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
}

// Bring `Result` into the crate root namespace.
pub use error::{ErrorClass, MailError, Result};
