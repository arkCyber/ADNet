//! Error types for `adnet-invite`.

use thiserror::Error;

pub type InviteResult<T> = std::result::Result<T, InviteError>;

/// Errors from invitation email operations.
#[derive(Debug, Error)]
pub enum InviteError {
    #[error("no pairing invitation attachment found in message")]
    NoInvitation,

    #[error("invitation attachment too large: {size} bytes (max {max})")]
    AttachmentTooLarge { size: usize, max: usize },

    #[error("pairing parse error: {0}")]
    Pairing(#[from] adnet_pairing::error::PairingError),

    #[error("mail encoding error: {0}")]
    Mail(#[from] adnet_mail::error::MailError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("QR code generation error: {0}")]
    Qr(String),

    // ── Text Code Errors ────────────────────────────────────────────────
    #[error("invalid text code format: {0}")]
    InvalidTextCode(String),

    #[error("text code checksum mismatch: expected {expected}, got {got}")]
    TextCodeChecksumMismatch { expected: u8, got: u8 },

    #[error("text code too short: {actual} bytes (min {min})")]
    TextCodeTooShort { actual: usize, min: usize },
}
