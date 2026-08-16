//! Crate-wide error type for `a3net-qr`.
//!
//! All variants are classified by [`QrError::class`] so callers can
//! decide whether to surface the failure to the user (`UserError`) or
//! retry / log (`Recoverable`).

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Result<T, E = QrError> = std::result::Result<T, E>;

/// Coarse classification of a [`QrError`] for upstream handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Caller-supplied data is invalid; do not retry.
    UserError,
    /// The QR payload itself is well-formed but unsupported (e.g.
    /// `DCLOGIN v=2`); do not retry, ask the operator to upgrade.
    Unsupported,
    /// Internal invariant violation; should be unreachable.
    Fatal,
}

/// Bounds applied during parsing. Defence-in-depth against adversarial
/// QR codes — every input limit here is enforced **before** allocation
/// of decoded bytes, so a hostile scanner cannot exhaust memory through
/// percent-decoding or base64 expansion.
///
/// Defaults are tuned for chatmail QR codes we expect to see in the
/// wild; tighten them in a `Default::default()`-override when wiring
/// the crate into an untrusted surface (e.g. a public kiosk).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ParseLimits {
    /// Hard upper bound on the **raw scanned bytes** we'll attempt to
    /// classify. Defaults to [`MAX_QR_CONTENT`] (2048). ISO/IEC 18004
    /// caps Version-40 / Q at ~1525 bytes of data; we default to a
    /// comfortable upper bound that leaves headroom for our own
    /// `a3net-…` envelopes while keeping allocation predictable.
    pub max_raw_bytes: usize,
    /// Maximum number of distinct query-string pairs we'll accept in a
    /// single `DCLOGIN` / `OPENPGP4FPR` payload.
    pub max_query_pairs: usize,
    /// Maximum bytes in any single query value (post-percent-decode).
    /// chatmail passwords are ~40 chars; an extreme user might use
    /// 256. We cap at 8 KiB to make allocation cost predictable.
    pub max_query_value_bytes: usize,
    /// Maximum bytes of base64 decoded from a Shadowsocks URL.
    pub max_shadowsocks_decoded_bytes: usize,
    /// Maximum bytes of an OPENPGP4FPR hex fingerprint (excluding the
    /// fragment). Modern fingerprints are 40 hex chars (v4) or 80
    /// (draft v5); we cap at 256 to reject embedded padding.
    pub max_fingerprint_bytes: usize,
    /// Maximum number of ranges in a single `BlobTicket::Multi`.
    /// `RangeSpec` already enforces its own `len` constraint upstream;
    /// this is a QR-side defence.
    pub max_blob_ranges: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_raw_bytes: MAX_QR_CONTENT,
            max_query_pairs: 64,
            max_query_value_bytes: 8 * 1024,
            max_shadowsocks_decoded_bytes: 4 * 1024,
            max_fingerprint_bytes: 256,
            max_blob_ranges: 256,
        }
    }
}

/// Errors produced by `a3net-qr`.
#[derive(Debug, Error)]
pub enum QrError {
    // ── User errors ──────────────────────────────────────────────────────
    /// The scanned text did not match any known QR scheme. The
    /// embedded prefix is **truncated** to the first 64 bytes — a
    /// full echo would let an attacker who scans a credential-bearing
    /// but malformed QR see the secret in our error log.
    #[error("unknown QR code: {0:?}")]
    Unknown(String),

    /// The scanned text matched a scheme but the payload was malformed.
    #[error("malformed QR payload in scheme {scheme}: {reason}")]
    Malformed {
        scheme: &'static str,
        reason: String,
    },

    /// Scanned text exceeded the configured [`ParseLimits::max_raw_bytes`]
    /// or the static [`MAX_QR_CONTENT`] ceiling.
    #[error("QR content too large: {actual} bytes (limit {limit})")]
    ContentTooLarge { actual: usize, limit: usize },

    /// A decoded field exceeded its per-field limit. The error
    /// message intentionally does **not** include the offending value
    /// (it may contain credentials).
    #[error("decoded field {field} exceeds {limit} bytes in scheme {scheme}")]
    FieldTooLarge {
        scheme: &'static str,
        field: String,
        limit: usize,
    },

    // ── Unsupported (newer-than-us) variants ──────────────────────────────
    /// QR payload uses a version newer than this build supports.
    #[error("unsupported QR payload version: {0}")]
    UnsupportedVersion(String),

    // ── Fatal / wrapping ─────────────────────────────────────────────────
    /// UTF-8 decoding of a percent-encoded payload failed.
    #[error("QR payload is not valid UTF-8: {0}")]
    NotUtf8(String),

    /// Underlying `qrcodegen` failure.
    #[error("QR code generation failed: {0}")]
    Generation(String),

    /// I/O failure (used by the `mail` feature when reading avatar
    /// blobs / etc.).
    #[error("QR i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error("QR serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// URL parsing failure.
    #[error("QR URL parse error: {0}")]
    Url(#[from] url::ParseError),
}

/// Maximum payload length we'll encode into a QR code. The chatmail
/// generator uses `QrCodeEcc::Medium` and a 512px viewBox; at that size,
/// 2953 ASCII characters is roughly the upper bound for Version 40 / M
/// ECC. We set the limit at a comfortable 2048 to leave room for
/// non-ASCII multi-byte input.
///
/// This is also the **default upper bound** for
/// [`ParseLimits::max_raw_bytes`] — the parser refuses anything
/// longer than this before allocation, matching the generator's
/// encoding ceiling.
pub const MAX_QR_CONTENT: usize = 2048;

impl QrError {
    /// Build an [`QrError::Unknown`] with the payload truncated to 64
    /// bytes. Use this anywhere we'd return `Unknown` from a public
    /// API surface so a malformed credential-bearing scan can't echo
    /// its secrets through our error log.
    pub fn unknown_truncated(raw: &str) -> Self {
        const TRUNC: usize = 64;
        let mut s = raw.to_string();
        if s.len() > TRUNC {
            s.truncate(TRUNC);
            s.push('…');
        }
        QrError::Unknown(s)
    }

    pub fn class(&self) -> ErrorClass {
        match self {
            QrError::Unknown(_)
            | QrError::Malformed { .. }
            | QrError::ContentTooLarge { .. }
            | QrError::FieldTooLarge { .. } => ErrorClass::UserError,
            QrError::UnsupportedVersion(_) => ErrorClass::Unsupported,
            QrError::NotUtf8(_)
            | QrError::Generation(_)
            | QrError::Io(_)
            | QrError::Serde(_)
            | QrError::Url(_) => ErrorClass::Fatal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_assignments() {
        assert_eq!(
            QrError::Unknown("foo".into()).class(),
            ErrorClass::UserError
        );
        assert_eq!(
            QrError::UnsupportedVersion("v=2".into()).class(),
            ErrorClass::Unsupported
        );
        assert_eq!(
            QrError::Generation("oops".into()).class(),
            ErrorClass::Fatal
        );
        assert_eq!(
            QrError::ContentTooLarge {
                actual: 5000,
                limit: 2048
            }
            .class(),
            ErrorClass::UserError
        );
        assert_eq!(
            QrError::FieldTooLarge {
                scheme: "DCLOGIN",
                field: "p".into(),
                limit: 1024
            }
            .class(),
            ErrorClass::UserError
        );
    }

    #[test]
    fn unknown_truncates_long_input() {
        let long = "x".repeat(200);
        let err = QrError::unknown_truncated(&long);
        match err {
            QrError::Unknown(s) => {
                assert!(s.len() <= 64 + 3);
                assert!(s.ends_with('…'));
                assert!(!s.contains(&"x".repeat(100)));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
