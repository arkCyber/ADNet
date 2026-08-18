//! Error types for the mailbox crate.
//!
//! Every error variant carries a [`MailboxErrorClass`] tag so callers
//! can decide *retry vs abort* without parsing strings (DO-178C §6.3
//! *fail-safe*).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Result alias for the mailbox crate.
pub type MailboxResult<T> = Result<T, MailboxError>;

/// Coarse error class — mirror of `a3chat-core::error::ErrorClass`.
///
/// Wire-stable: serialised as snake_case strings so RPC clients can
/// mirror the enum in their own language. The variant order is part
/// of the public API and **must not change** without a major bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxErrorClass {
    /// Client-side error that will never succeed by retrying
    /// (oversized, malformed, invalid signature, …).
    Permanent,
    /// Transient failure — retry with backoff has a chance.
    Transient,
    /// Authentication / authorization failure — never retry.
    Security,
    /// Internal server error — may be worth retrying once.
    Internal,
}

impl MailboxErrorClass {
    /// Whether the caller should consider retrying this error.
    pub fn is_retryable(self) -> bool {
        matches!(self, MailboxErrorClass::Transient | MailboxErrorClass::Internal)
    }
}

/// Errors raised by the mailbox server, client, and storage layers.
#[derive(Debug, Error)]
pub enum MailboxError {
    /// The recipient id is malformed (not a valid 20-byte hex address).
    #[error("invalid recipient id: {0}")]
    InvalidRecipientId(String),

    /// The envelope exceeds the configured per-message byte cap.
    #[error("envelope too large: {size} bytes (max {max})")]
    EnvelopeTooLarge { size: usize, max: usize },

    /// The recipient has exhausted the per-user message-count or byte
    /// quota. The caller should retry later or purge.
    #[error("recipient quota exhausted: {0}")]
    QuotaExceeded(String),

    /// The sender signature failed to verify against the resolved
    /// sender public key.
    #[error("invalid sender signature")]
    InvalidSignature,

    /// The recipient signature (used for `pull` / `ack`) failed to
    /// verify against the resolved recipient public key.
    #[error("invalid recipient signature")]
    InvalidRecipientSignature,

    /// The message id is malformed, missing, or fails the internal
    /// validator.
    #[error("invalid message id: {0}")]
    InvalidMessageId(String),

    /// The provided timestamp is invalid (e.g. signed_at_unix is in the future).
    #[error("invalid timestamp")]
    InvalidTimestamp,

    /// Replay protection: the sender's signature is too old
    /// (captured-replay attack prevention).
    #[error("stale signature: age {age_secs}s exceeds max {max_age_secs}s")]
    StaleSignature { age_secs: i64, max_age_secs: i64 },

    /// Replay protection: the same `(sender, recipient, msg_id)` triple
    /// has already been accepted. The server returns 200 with the
    /// original `queued_at` timestamp; this variant is reserved for the
    /// storage layer.
    #[error("duplicate envelope: {msg_id}")]
    Duplicate { msg_id: String },

    /// The requested message id is not in the recipient's queue.
    #[error("message not found: {0}")]
    NotFound(String),

    /// The message storage is unreachable or returned an inconsistent
    /// state.
    #[error("storage error: {0}")]
    Storage(String),

    /// The remote server returned an error response.
    #[error("remote error: status={status} body={body}")]
    Remote { status: u16, body: String },

    /// The request could not be sent (network, DNS, TLS, ...).
    #[error("transport error: {0}")]
    Transport(String),

    /// The configuration is invalid.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// Catch-all for unspecified internal errors.
    #[error("internal error: {0}")]
    Internal(String),
}

impl MailboxError {
    /// Coarse retry classification.
    ///
    /// DO-178C §6.3 *fail-safe*: callers should call
    /// `is_retryable()` on every error from the mailbox layer and
    /// only retry when it returns `true`. The mapping is:
    ///
    /// - `Permanent` (`InvalidRecipientId`, `EnvelopeTooLarge`,
    ///   `InvalidMessageId`, `Config`, `InvalidRecipientSignature`):
    ///   never retry, the request will never succeed.
    /// - `Security` (`InvalidSignature`, `QuotaExceeded`): never
    ///   retry, the caller is doing something wrong.
    /// - `Transient` (`Transport`, `Remote { status: 5xx }`,
    ///   `NotFound`, `Duplicate`): retry with backoff.
    /// - `Internal` (`Storage`, `Remote { status: 4xx }`, `Internal`):
    ///   retry once, then escalate.
    pub fn error_class(&self) -> MailboxErrorClass {
        match self {
            MailboxError::InvalidRecipientId(_)
            | MailboxError::EnvelopeTooLarge { .. }
            | MailboxError::InvalidMessageId(_)
            | MailboxError::Config(_) => MailboxErrorClass::Permanent,
            MailboxError::InvalidSignature
            | MailboxError::InvalidRecipientSignature
            | MailboxError::QuotaExceeded(_)
            | MailboxError::StaleSignature { .. } => MailboxErrorClass::Security,
            MailboxError::InvalidTimestamp => MailboxErrorClass::Permanent,
            MailboxError::Transport(_) => MailboxErrorClass::Transient,
            MailboxError::Remote { status, .. } => {
                if *status >= 500 && *status < 600 {
                    MailboxErrorClass::Transient
                } else {
                    // 4xx client errors should not be retried
                    // normally — but the classification is a hint, not
                    // a contract.
                    MailboxErrorClass::Internal
                }
            }
            MailboxError::NotFound(_) | MailboxError::Duplicate { .. } => {
                MailboxErrorClass::Transient
            }
            MailboxError::Storage(_) | MailboxError::Internal(_) => {
                MailboxErrorClass::Internal
            }
        }
    }

    /// Convenience wrapper around [`error_class`].
    pub fn is_retryable(&self) -> bool {
        self.error_class().is_retryable()
    }

    /// Map a `reqwest::Error` to a `Transport` variant.
    pub fn from_reqwest(e: reqwest::Error) -> Self {
        MailboxError::Transport(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_recipient_id_is_permanent() {
        let e = MailboxError::InvalidRecipientId("foo".into());
        assert_eq!(e.error_class(), MailboxErrorClass::Permanent);
        assert!(!e.is_retryable());
    }

    #[test]
    fn envelope_too_large_is_permanent() {
        let e = MailboxError::EnvelopeTooLarge { size: 10, max: 5 };
        assert_eq!(e.error_class(), MailboxErrorClass::Permanent);
        assert!(!e.is_retryable());
    }

    #[test]
    fn invalid_signature_is_security() {
        let e = MailboxError::InvalidSignature;
        assert_eq!(e.error_class(), MailboxErrorClass::Security);
        assert!(!e.is_retryable());
    }

    #[test]
    fn invalid_recipient_signature_is_security() {
        let e = MailboxError::InvalidRecipientSignature;
        assert_eq!(e.error_class(), MailboxErrorClass::Security);
        assert!(!e.is_retryable());
    }

    #[test]
    fn quota_is_security() {
        let e = MailboxError::QuotaExceeded("inflight".into());
        assert_eq!(e.error_class(), MailboxErrorClass::Security);
        assert!(!e.is_retryable());
    }

    #[test]
    fn transport_is_transient() {
        let e = MailboxError::Transport("dns".into());
        assert_eq!(e.error_class(), MailboxErrorClass::Transient);
        assert!(e.is_retryable());
    }

    #[test]
    fn remote_5xx_is_transient() {
        let e = MailboxError::Remote { status: 503, body: "x".into() };
        assert_eq!(e.error_class(), MailboxErrorClass::Transient);
        assert!(e.is_retryable());
    }

    #[test]
    fn remote_4xx_is_internal() {
        let e = MailboxError::Remote { status: 400, body: "x".into() };
        assert_eq!(e.error_class(), MailboxErrorClass::Internal);
        assert!(e.is_retryable());
    }

    #[test]
    fn not_found_is_transient() {
        let e = MailboxError::NotFound("m".into());
        assert_eq!(e.error_class(), MailboxErrorClass::Transient);
        assert!(e.is_retryable());
    }

    #[test]
    fn storage_is_internal() {
        let e = MailboxError::Storage("disk".into());
        assert_eq!(e.error_class(), MailboxErrorClass::Internal);
        assert!(e.is_retryable());
    }

    #[test]
    fn error_class_serializes_to_snake_case() {
        let cases = [
            (MailboxErrorClass::Permanent, "\"permanent\""),
            (MailboxErrorClass::Transient, "\"transient\""),
            (MailboxErrorClass::Security, "\"security\""),
            (MailboxErrorClass::Internal, "\"internal\""),
        ];
        for (v, want) in cases {
            let s = serde_json::to_string(&v).unwrap();
            assert_eq!(s, want, "variant {v:?} should serialize as {want}");
        }
    }
}
