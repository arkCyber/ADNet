//! Pairing error type. Kept small and DO-178C-friendly: each variant
//! carries enough context to debug a failure without leaking secrets
//! (we never put raw signature bytes, nonces, or key material in here).

use thiserror::Error;

pub type PairingResult<T> = std::result::Result<T, PairingError>;

/// All pairing-protocol failures. Variants are categorised:
///
/// - `*Rejected` — remote sent us something malformed or invalid.
/// - `*Expired`  — nonce/credential expired (clock skew or stale).
/// - `*Mismatch` — cryptographic binding check failed.
/// - `*Revoked`  — operation refused because the credential is revoked.
/// - `*Storage`  — local persistence failure (store IO / serialisation).
/// - `*Config`   — caller-side configuration problem.
#[derive(Debug, Error)]
pub enum PairingError {
    #[error("malformed {what}: {reason}")]
    Malformed { what: &'static str, reason: String },

    #[error("invitation expired at {expired_at_unix}, now {now_unix}")]
    InvitationExpired { expired_at_unix: i64, now_unix: i64 },

    #[error("request expired at {expired_at_unix}, now {now_unix}")]
    RequestExpired { expired_at_unix: i64, now_unix: i64 },

    #[error("nonce already used (replay attempt): {nonce_prefix:?}")]
    NonceReplay { nonce_prefix: [u8; 8] },

    #[error("nonce in response does not match the request (possible replay)")]
    NonceMismatch,

    #[error("clock skew exceeds {max_seconds}s: peer={peer_unix}, now={now_unix}")]
    ClockSkew {
        max_seconds: i64,
        peer_unix: i64,
        now_unix: i64,
    },

    #[error("signature scheme {scheme_tag} not supported (need 0=secp256k1 EIP-191)")]
    UnsupportedScheme { scheme_tag: u8 },

    #[error("signature length mismatch: expected {expected}, got {got}")]
    SignatureLength { expected: usize, got: usize },

    #[error("issuer signature failed verification")]
    IssuerSignatureInvalid,

    #[error("transport signature failed verification")]
    TransportSignatureInvalid,

    #[error("NodeId does not match signed transport identity")]
    NodeIdMismatch,

    #[error(
        "credential_id mismatch (prefix={}...)",
        hex::encode(credential_id_prefix)
    )]
    CredentialIdMismatch { credential_id_prefix: [u8; 4] },

    #[error("capability not granted: {0}")]
    CapabilityNotGranted(&'static str),

    #[error("device is revoked (credential_id={0:?})")]
    DeviceRevoked([u8; 16]),

    #[error("device pairing expired at {expired_at_unix} (id_prefix={}...)", hex::encode(&id[..4]))]
    DeviceExpired { id: [u8; 16], expired_at_unix: i64 },

    #[error("device not found in trusted store (credential_id={0:?})")]
    DeviceNotFound([u8; 16]),

    #[error("trusted-device store error: {0}")]
    Storage(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("invalid config: {0}")]
    Config(String),
}

impl PairingError {
    /// Classify the error for retry / audit purposes. Mirrors the
    /// DO-178C recoverability ladder used in `a3net-mail`.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Storage(_) | Self::Config(_) => ErrorClass::Fatal,
            Self::Serialization(_) => ErrorClass::Fatal,
            Self::Malformed { .. }
            | Self::UnsupportedScheme { .. }
            | Self::SignatureLength { .. }
            | Self::IssuerSignatureInvalid
            | Self::TransportSignatureInvalid
            | Self::NodeIdMismatch
            | Self::CredentialIdMismatch { .. }
            | Self::CapabilityNotGranted(_) => ErrorClass::Rejected,
            Self::InvitationExpired { .. }
            | Self::RequestExpired { .. }
            | Self::NonceReplay { .. }
            | Self::NonceMismatch
            | Self::ClockSkew { .. }
            | Self::DeviceRevoked(_)
            | Self::DeviceExpired { .. }
            | Self::DeviceNotFound(_) => ErrorClass::Expired,
        }
    }
}

/// Recoverability class. Kept here rather than re-exported from
/// `a3net-mail` so the pairing crate has zero runtime dependencies
/// beyond `std` + `serde`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Remote sent something we refuse on principle. Retry only
    /// after the human fixes the input.
    Rejected,
    /// Time-bound check failed. Retry only when clock / freshness
    /// changes (the same data will never succeed).
    Expired,
    /// Local persistence or configuration problem. Retry only
    /// after operator intervention.
    Fatal,
}
