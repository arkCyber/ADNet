//! Crate-wide error type.
//!
//! Modelled on `a3net-chatstore::error`: every variant is classified by
//! [`MailError::recoverability`] so callers don't have to string-match
//! the `Display` output to decide whether to retry.
//!
//! # Recoverability matrix
//!
//! | Class         | Variants                                                                | Caller behaviour                          |
//! |---------------|-------------------------------------------------------------------------|-------------------------------------------|
//! | `UserError`   | `InvalidAddr`, `InvalidHeader`, `EmptyRecipients`, `EmptyFrom`, `Unsupported` | Surface to user; do not retry.        |
//! | `Recoverable` | `Transient`, `IdleInterrupted`, `Dns`, `Io`                             | Retry with backoff.                      |
//! | `Fatal`       | `Auth`, `Tls`, `Config`, `Parse`, `Build`, `Internal`                   | Refuse to continue; require operator.    |
//!
//! The transport-level `async_smtp::error::Error` and `async_imap::error::Error`
//! values are preserved (not stringified) via the `source()` chain so they
//! can still drive operator dashboards without polluting the typed error
//! surface.

use thiserror::Error;

pub type Result<T, E = MailError> = std::result::Result<T, E>;

/// Coarse classification of an error variant for upstream handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Transient or contention-induced failure; safe to retry with backoff.
    Recoverable,
    /// Caller passed invalid input; do not retry.
    UserError,
    /// Internal invariant broken or unrecoverable backend failure.
    Fatal,
}

/// Errors produced by the `a3net-mail` crate.
#[derive(Debug, Error)]
pub enum MailError {
    // ── User errors ────────────────────────────────────────────────────────
    /// Email address is syntactically invalid (RFC 5321 / 5322 mailbox).
    #[error("invalid email address: {0}")]
    InvalidAddr(String),

    /// A MIME header is malformed in a way the user is responsible for.
    #[error("invalid header {name:?}: {reason}")]
    InvalidHeader { name: String, reason: String },

    /// `send_message` was called with zero recipients.
    #[error("empty recipient list")]
    EmptyRecipients,

    /// `send_message` was called without a `From:` address.
    #[error("missing From: address")]
    EmptyFrom,

    /// The configured server asked for a feature we don't implement
    /// (e.g. `OBJECTID`, `URLFETCH`, server-side filters).
    #[error("unsupported server feature: {0}")]
    Unsupported(String),

    // ── Recoverable (transient) errors ─────────────────────────────────────
    /// Remote returned 4xx SMTP / IMAP — temporary, retry later.
    #[error("transient mail error: {0}")]
    Transient(String),

    /// Caller asked the IMAP IDLE loop to exit (e.g. switching folders,
    /// shutting down the account). Not really an error, but our API
    /// surfaces it as one to keep `idle()` total.
    #[error("idle interrupted by caller")]
    IdleInterrupted,

    /// DNS lookup failed for the configured server hostname.
    #[error("dns lookup failed for {host}: {source}")]
    Dns {
        host: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    // ── Fatal errors ───────────────────────────────────────────────────────
    /// Authentication failed: wrong username / password / OAuth token.
    /// IMAP and SMTP both surface this as a 5xx-flavoured condition; we
    /// collapse it into one variant so retries are obviously useless.
    #[error("authentication failed for {user} on {host}")]
    Auth { user: String, host: String },

    /// TLS handshake, certificate validation, or STARTTLS upgrade failed.
    #[error("tls error against {host}: {reason}")]
    Tls { host: String, reason: String },

    /// Configuration file or CLI argument was malformed.
    #[error("configuration error: {0}")]
    Config(String),

    /// Parsing an incoming MIME message failed (bad bytes, header syntax).
    #[error("mime parse error: {0}")]
    Parse(String),

    /// Building an outgoing MIME message failed (missing required part,
    /// oversized attachment, attachment I/O failure).
    #[error("mime build error: {0}")]
    Build(String),

    /// An invariant the crate relies on was violated; this *should* be
    /// unreachable. We expose the variant instead of panicking so the
    /// caller can decide whether to crash the process or keep going.
    #[error("internal invariant violated: {0}")]
    Internal(String),

    // ── Foreign-error wrappers ─────────────────────────────────────────────
    /// Any `std::io::Error` from disk / socket reads.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Wrapper around `async_smtp::Error` (SMTP transport).
    #[error("smtp transport error: {0}")]
    Smtp(#[from] async_smtp::error::Error),

    /// Wrapper around `async_imap::Error` (IMAP transport).
    #[error("imap transport error: {0}")]
    Imap(#[from] async_imap::error::Error),

    /// Wrapper around `serde_json` failures when serialising config blobs.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl MailError {
    /// Classify the error for upstream retry / alerting logic.
    pub fn recoverability(&self) -> ErrorClass {
        match self {
            MailError::InvalidAddr(_)
            | MailError::InvalidHeader { .. }
            | MailError::EmptyRecipients
            | MailError::EmptyFrom
            | MailError::Unsupported(_) => ErrorClass::UserError,

            MailError::Transient(_)
            | MailError::IdleInterrupted
            | MailError::Dns { .. }
            | MailError::Io(_) => ErrorClass::Recoverable,

            MailError::Auth { .. }
            | MailError::Tls { .. }
            | MailError::Config(_)
            | MailError::Parse(_)
            | MailError::Build(_)
            | MailError::Internal(_)
            | MailError::Smtp(_)
            | MailError::Imap(_)
            | MailError::Json(_) => ErrorClass::Fatal,
        }
    }

    /// Convenience: is this error worth retrying?
    pub fn is_retryable(&self) -> bool {
        matches!(self.recoverability(), ErrorClass::Recoverable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoverability_matrix() {
        let user = MailError::InvalidAddr("nope".into());
        assert_eq!(user.recoverability(), ErrorClass::UserError);
        assert!(!user.is_retryable());

        let retry = MailError::Transient("451 try again".into());
        assert_eq!(retry.recoverability(), ErrorClass::Recoverable);
        assert!(retry.is_retryable());

        let auth = MailError::Auth {
            user: "alice".into(),
            host: "imap.example.com".into(),
        };
        assert_eq!(auth.recoverability(), ErrorClass::Fatal);
        assert!(!auth.is_retryable());
    }

    #[test]
    fn display_does_not_leak_password() {
        // Regression guard: the Display impl must never echo a password,
        // even if a future refactor embeds `user@host:password` somewhere.
        let err = MailError::Auth {
            user: "alice".into(),
            host: "imap.example.com".into(),
        };
        let s = format!("{err}");
        assert!(!s.contains("password"));
        assert!(!s.contains("secret"));
        assert!(s.contains("alice"));
        assert!(s.contains("imap.example.com"));
        // Avoid `alice@imap.example.com` — the @ is misleading when
        // `user` is itself a full email address (OAuth case).
        assert!(
            !s.contains('@'),
            "Auth Display must not use `user@host` form: {s:?}"
        );
    }
}
