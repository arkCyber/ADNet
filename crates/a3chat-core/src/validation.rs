//! Field-level validation helpers shared across all a3chat types.
//!
//! These mirror [`a3net_types::invariants`] but operate on the
//! `a3chat-error` domain. Higher layers should prefer these over the
//! raw `a3net_types` helpers so error mapping stays consistent.

use crate::error::A3chatError;

/// `MAX_NAME_LEN` — same ceiling as `a3net_types::invariants::MAX_NAME_LEN`.
pub const MAX_NAME_LEN: usize = 256;

/// `MAX_CONTENT_LEN` — same ceiling as `a3net_types::invariants::MAX_CONTENT_LEN`.
pub const MAX_CONTENT_LEN: usize = 16 * 1024;

/// `MAX_ATTACHMENTS` per message — keeps a single message bounded.
pub const MAX_ATTACHMENTS: usize = 32;

/// `MAX_MEMBERS` per group — operational ceiling.
pub const MAX_MEMBERS: usize = 500;

/// `MAX_MENTIONS` per message.
pub const MAX_MENTIONS: usize = 64;

/// `MAX_PREVIEW_LEN` for `ConversationMeta::last_message_preview`.
pub const MAX_PREVIEW_LEN: usize = 256;

/// Validate that `name` is non-empty, ≤ [`MAX_NAME_LEN`], and contains
/// no control characters.
pub fn validate_name(field: &str, name: &str) -> Result<(), A3chatError> {
    if name.is_empty() {
        return Err(A3chatError::InvalidInput(format!("{field}: empty name")));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: length {} > {MAX_NAME_LEN}",
            name.len()
        )));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: contains control characters"
        )));
    }
    Ok(())
}

/// Validate message content — non-empty, ≤ [`MAX_CONTENT_LEN`].
pub fn validate_content(field: &str, content: &str) -> Result<(), A3chatError> {
    if content.is_empty() {
        return Err(A3chatError::InvalidInput(format!("{field}: empty content")));
    }
    if content.len() > MAX_CONTENT_LEN {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: length {} > {MAX_CONTENT_LEN}",
            content.len()
        )));
    }
    Ok(())
}

/// Validate that `earlier <= later`. Both inputs are RFC3339 strings
/// compared lexicographically (RFC3339 is lex-sortable when
/// timezone-offset normalized; we accept any `chrono::DateTime<Utc>`).
pub fn validate_ordered(
    field: &str,
    earlier: chrono::DateTime<chrono::Utc>,
    later: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    if later < earlier {
        Err(format!("{field}: {later} < {earlier}"))
    } else {
        Ok(())
    }
}

/// Same as [`validate_ordered`] but for i64 Unix timestamps.
pub fn validate_ordered_ts(field: &str, earlier: i64, later: i64) -> Result<(), A3chatError> {
    if later < earlier {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: {later} < {earlier}"
        )));
    }
    Ok(())
}

/// Validate that `seq` is in `[0, max_seq)`.
pub fn validate_sequence(field: &str, seq: u32, max_seq: u32) -> Result<(), A3chatError> {
    if seq >= max_seq {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: sequence {seq} >= {max_seq}"
        )));
    }
    Ok(())
}

/// Validate a URL string — must start with `http://` or `https://`.
pub fn validate_url(field: &str, url: &str) -> Result<(), A3chatError> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: url must start with http:// or https://"
        )));
    }
    if url.len() > 2048 {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: url length {} > 2048",
            url.len()
        )));
    }
    Ok(())
}

/// Validate an attachment count against [`MAX_ATTACHMENTS`].
pub fn validate_attachments(field: &str, n: usize) -> Result<(), A3chatError> {
    if n > MAX_ATTACHMENTS {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: {n} attachments > {MAX_ATTACHMENTS}"
        )));
    }
    Ok(())
}

/// Validate a hex string of exact length.
pub fn validate_hex(field: &str, hex_str: &str, expected_len: usize) -> Result<(), A3chatError> {
    if hex_str.len() != expected_len {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: expected {expected_len} hex chars, got {}",
            hex_str.len()
        )));
    }
    if !hex_str.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: non-hex characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ts(s: i64) -> chrono::DateTime<chrono::Utc> {
        Utc.timestamp_opt(s, 0).unwrap()
    }

    #[test]
    fn name_validator_accepts_normal_and_rejects_empty() {
        assert!(validate_name("name", "Alice").is_ok());
        assert!(validate_name("name", "").is_err());
    }

    #[test]
    fn name_validator_rejects_oversize() {
        let huge = "x".repeat(MAX_NAME_LEN + 1);
        assert!(validate_name("name", &huge).is_err());
    }

    #[test]
    fn content_validator_rejects_empty_and_oversize() {
        assert!(validate_content("c", "ok").is_ok());
        assert!(validate_content("c", "").is_err());
        let huge = "x".repeat(MAX_CONTENT_LEN + 1);
        assert!(validate_content("c", &huge).is_err());
    }

    #[test]
    fn ordered_rejects_inverted() {
        assert!(validate_ordered("t", ts(100), ts(50)).is_err());
        assert!(validate_ordered("t", ts(50), ts(100)).is_ok());
    }

    #[test]
    fn sequence_validator() {
        assert!(validate_sequence("s", 9999, 10_000).is_ok());
        assert!(validate_sequence("s", 10_000, 10_000).is_err());
    }

    #[test]
    fn url_validator() {
        assert!(validate_url("u", "https://example.com").is_ok());
        assert!(validate_url("u", "http://x").is_ok());
        assert!(validate_url("u", "ftp://x").is_err());
        assert!(validate_url("u", "").is_err());
    }

    #[test]
    fn hex_validator() {
        assert!(validate_hex("h", "abcdef0123456789", 16).is_ok());
        assert!(validate_hex("h", "abc", 16).is_err());
        assert!(validate_hex("h", "zzzz", 4).is_err());
    }

    #[test]
    fn attachments_validator() {
        assert!(validate_attachments("a", 0).is_ok());
        assert!(validate_attachments("a", MAX_ATTACHMENTS).is_ok());
        assert!(validate_attachments("a", MAX_ATTACHMENTS + 1).is_err());
    }

    #[test]
    fn ordered_ts_validator() {
        assert!(validate_ordered_ts("t", 100, 50).is_err());
        assert!(validate_ordered_ts("t", 50, 50).is_ok());
        assert!(validate_ordered_ts("t", 50, 100).is_ok());
    }
}
