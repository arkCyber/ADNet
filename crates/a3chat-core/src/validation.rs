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
/// no control characters or visually-ambiguous unicode (zero-width
/// / RTL-override / BOM).
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
    if let Some(c) = name.chars().find(|c| is_dangerous_char(*c)) {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: contains forbidden character (U+{:04X})",
            u32::from(c)
        )));
    }
    Ok(())
}

/// Validate message content — non-empty, ≤ [`MAX_CONTENT_LEN`], and
/// free of `is_dangerous_char` codepoints.
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
    if let Some(c) = content.chars().find(|c| is_dangerous_char(*c)) {
        return Err(A3chatError::InvalidInput(format!(
            "{field}: contains forbidden character (U+{:04X})",
            u32::from(c)
        )));
    }
    Ok(())
}

/// Audit issue #12: classify a character as "dangerous" for chat
/// purposes. The set covers:
///
/// - **C0 control chars** (U+0000..=U+001F) — non-printable, can
///   break terminals and inject log noise.
/// - **DEL** (U+007F) — also a control char, often used to
///   obscure filenames.
/// - **Zero-width / joiner characters** (U+200B, U+200C, U+200D,
///   U+FEFF) — invisible but legal, allowing two semantically
///   different names to look identical in the UI.
/// - **Bidirectional-override codepoints** (U+202A..=U+202E,
///   U+2066..=U+2069) — used to spoof file extensions
///   (`"<U+202E>txt.exe"` displays as `exe.txt`).
/// - **Word-joiner / Mongolian variation separator** (U+2060,
///   U+180E) — also invisible.
fn is_dangerous_char(c: char) -> bool {
    let cp = u32::from(c);
    matches!(cp, 0x0000..=0x001F)
        || cp == 0x007F
        || matches!(cp, 0x200B | 0x200C | 0x200D | 0xFEFF)
        || matches!(cp, 0x202A..=0x202E)
        || matches!(cp, 0x2060 | 0x2066..=0x2069)
        || cp == 0x180E
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

    // Audit issue #12: zero-width, RTL-override, and BOM chars
    // must be rejected in both names and content so that
    // visually-spoofed strings (e.g. "exe.txt\u{202E}\u{202D}.exe")
    // cannot be stored.
    #[test]
    fn content_validator_rejects_zero_width_and_rtl_chars() {
        let zero_width_space = "hello\u{200B}world";
        assert!(validate_content("c", zero_width_space).is_err());
        let rtl_override = "fake.exe\u{202E}txt.exe";
        assert!(validate_content("c", rtl_override).is_err());
        let bom = "\u{FEFF}leading-bom";
        assert!(validate_content("c", bom).is_err());
        let word_joiner = "ab\u{2060}cd";
        assert!(validate_content("c", word_joiner).is_err());
    }

    #[test]
    fn name_validator_rejects_zero_width_chars() {
        let spoof = "Alice\u{200B}Imposter";
        assert!(validate_name("n", spoof).is_err());
    }

    #[test]
    fn content_validator_accepts_normal_unicode() {
        // Real non-ASCII non-dangerous characters (CJK, emoji, etc.)
        // must still pass.
        assert!(validate_content("c", "你好世界").is_ok());
        assert!(validate_content("c", "🇨🇳沿海城市").is_ok());
        assert!(validate_content("c", "café").is_ok());
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
