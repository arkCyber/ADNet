//! `@`-mention parser for group chat bodies (G-05).
//!
//! Used by [`crate::group_service::GroupService::parse_mentions`]
//! and the `a3chat.group.mention.parse` RPC.
//!
//! ## Format
//!
//! * `@<NodeId>` — exact match against a 64-char hex NodeId. The
//!   matcher is case-insensitive on hex.
//! * `@<nickname>` — case-insensitive match against the
//!   per-conversation nickname table. A body like
//!   `"hi @alice, please review"` resolves to `(user_id, offset,
//!   length)` pairs the caller can hand to `MessageEnvelope.mentions`.
//!
//! ## Security
//!
//! The parser does NOT verify membership. Callers must follow up
//! with
//! [`crate::group_service::GroupService::validate_mention_members`]
//! before letting the mention list trigger push notifications.
//!
//! ## Traceability
//!
//! DO-178C §6.1 — every parsing rule is a pure function whose
//! output is a `Vec<MentionMatch>` with byte offsets that can be
//! round-tripped through `validate_mentions` without re-parsing.

use a3chat_core::id::UserId;
use serde::{Deserialize, Serialize};

/// One resolved `@`-mention in a body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionMatch {
    /// The resolved user id (the actual NodeId of the mentioned
    /// member). Already verified to exist in the caller's roster.
    pub user_id: UserId,
    /// Byte offset of the `@`-character in the input body.
    pub offset: usize,
    /// Byte length of the token including the leading `@`.
    pub length: usize,
}

/// Parse `@<token>` mentions from `body`. `nicknames` is the
/// `(user_id, nickname)` table for the current conversation.
///
/// The parser is a single forward pass: every `@` is examined; if
/// the token is in `nicknames` (case-insensitive) or matches a
/// 64-char hex NodeId (case-insensitive), a `MentionMatch` is
/// produced. Embedded `@` inside a word (e.g. `email@host`) is
/// ignored because we require the `@` to be at the start of a
/// token (preceded by whitespace, start-of-string, or
/// punctuation).
pub fn parse(body: &str, nicknames: &[(UserId, String)]) -> Vec<MentionMatch> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // Boundary check: require start-of-string or non-identifier
        // char on the left, so we don't match `email@host`.
        if i > 0 && is_ident_char(bytes[i - 1]) {
            i += 1;
            continue;
        }
        // Read the token.
        let start = i;
        i += 1;
        while i < bytes.len() && !is_token_boundary(bytes[i]) {
            // Tokens end at whitespace, punctuation, or end of body.
            i += 1;
        }
        let token = match std::str::from_utf8(&bytes[start..i]) {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Token includes the `@`. Try matches.
        if let Some(uid) = resolve_token(&token[1..], nicknames) {
            out.push(MentionMatch {
                user_id: uid,
                offset: start,
                length: i - start,
            });
        }
    }
    out
}

/// Resolve a token (without the leading `@`) to a `UserId`.
/// Resolution order: 64-char hex NodeId > nickname (CI).
fn resolve_token(token: &str, nicknames: &[(UserId, String)]) -> Option<UserId> {
    if token.is_empty() {
        return None;
    }
    // 64-char hex NodeId match.
    if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        // Canonicalise to lowercase to avoid case-mismatch
        // surprises later.
        let lowered = token.to_ascii_lowercase();
        return Some(UserId::from(lowered));
    }
    // Nickname match (case-insensitive).
    let lower = token.to_ascii_lowercase();
    nicknames
        .iter()
        .find(|(_, name)| name.to_ascii_lowercase() == lower)
        .map(|(uid, _)| uid.clone())
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_token_boundary(b: u8) -> bool {
    b.is_ascii_whitespace() || (b.is_ascii_punctuation() && b != b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nicks() -> Vec<(UserId, String)> {
        vec![
            (UserId::from("alice"), "Alice".to_string()),
            (UserId::from("bob"), "Bob".to_string()),
        ]
    }

    #[test]
    fn parses_hex_node_id() {
        let hex = "0".repeat(64);
        let body = format!("hi @{hex} how are you");
        let m = parse(&body, &[]);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].user_id, UserId::from(hex.to_ascii_lowercase()));
        assert_eq!(m[0].offset, 3);
        assert_eq!(m[0].length, 65);
    }

    #[test]
    fn parses_nickname() {
        let m = parse("hello @alice!", &nicks());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].user_id, UserId::from("alice"));
    }

    #[test]
    fn case_insensitive_nickname() {
        let m = parse("cc @ALICE please review", &nicks());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].user_id, UserId::from("alice"));
    }

    #[test]
    fn ignores_email_inside_word() {
        let m = parse("ping alice@example.com please", &nicks());
        assert!(m.is_empty(), "email@host should not be a mention");
    }

    #[test]
    fn parses_multiple_mentions() {
        let m = parse("@alice @bob done", &nicks());
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].user_id, UserId::from("alice"));
        assert_eq!(m[1].user_id, UserId::from("bob"));
    }

    #[test]
    fn unknown_nickname_dropped() {
        let m = parse("hi @charlie", &nicks());
        assert!(m.is_empty());
    }

    #[test]
    fn empty_token_skipped() {
        let m = parse("hi @ please", &nicks());
        assert!(m.is_empty(), "@<space> is not a valid mention");
    }
}