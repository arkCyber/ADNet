//! Chat messages — plaintext and end-to-end-encrypted bodies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::A3chatError;
use crate::id::{ConversationId, MessageId, UserId, validate_id};
use crate::validation::{
    MAX_CONTENT_LEN, validate_attachments, validate_content, validate_hex, validate_sequence,
};

/// Re-export of `validation::MAX_PREVIEW_LEN` for ergonomic `use`
/// at the `a3chat_core::message::*` namespace. Kept here as a
/// single canonical constant (DO-178C §6.3 — *single source of
/// truth*) so `MessageBody::preview` and `ConversationMeta`
/// validation agree on the byte length.
pub use crate::validation::MAX_PREVIEW_LEN;

/// Truncate `s` to at most `max` characters, appending
/// an ellipsis if it was longer. Used by `MessageBody::preview` and
/// (in P1) by the conversation list to render a snippet. Counts by
/// Unicode scalar values, not bytes, so multibyte UTF-8 input isn't
/// silently truncated mid-codepoint.
pub fn truncate_preview(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    let mut out = String::with_capacity(max + 3);
    for (i, ch) in s.chars().enumerate() {
        if i == max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Maximum per-sender sequence number. Matches `a3net_types::group_chat::MAX_SEQUENCE`.
pub const MAX_SEQUENCE: u32 = 9999;

/// Wire shape of a single message body. `Plain` is used for
/// self-notifications / system messages; `Encrypted` is the default
/// for every user-authored chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageBody {
    /// Visible plaintext (e.g. server-issued system message). Length
    /// bounded by [`MAX_CONTENT_LEN`].
    Plain { content: String },

    /// End-to-end-encrypted body. `algorithm` identifies the AEAD;
    /// `nonce` is per-message random; `ciphertext` is opaque bytes
    /// base64-encoded; `tag` is the AEAD authentication tag (separate
    /// from `ciphertext` so the receiver can verify before decrypting).
    Encrypted {
        algorithm: String,  // "chacha20-poly1305-v1"
        nonce: String,      // 24 hex chars (12 bytes)
        ciphertext: String, // base64
        tag: String,        // 32 hex chars (16 bytes)
    },
}

impl MessageBody {
    pub fn is_encrypted(&self) -> bool {
        matches!(self, MessageBody::Encrypted { .. })
    }

    pub fn is_plain(&self) -> bool {
        matches!(self, MessageBody::Plain { .. })
    }

    /// Short, plaintext preview suitable for the conversation-list UI.
    /// For encrypted bodies the caller should have already decrypted;
    /// otherwise we return a fixed placeholder so the UI can show
    /// "🔒 encrypted message" instead of leaking ciphertext bytes.
    pub fn preview(&self) -> String {
        match self {
            MessageBody::Plain { content } => truncate_preview(content, MAX_PREVIEW_LEN),
            MessageBody::Encrypted { .. } => "[encrypted]".to_string(),
        }
    }

    pub fn validate(&self) -> Result<(), A3chatError> {
        match self {
            MessageBody::Plain { content } => {
                validate_content("body.content", content)?;
            }
            MessageBody::Encrypted {
                algorithm,
                nonce,
                ciphertext,
                tag,
            } => {
                if algorithm.is_empty() || algorithm.len() > 64 {
                    return Err(A3chatError::InvalidInput(format!(
                        "body.algorithm: bad length {}",
                        algorithm.len()
                    )));
                }
                validate_hex("body.nonce", nonce, 24)?; // 12 bytes = 24 hex chars
                validate_hex("body.tag", tag, 32)?; // 16 bytes = 32 hex chars
                if ciphertext.is_empty() {
                    return Err(A3chatError::InvalidInput("body.ciphertext: empty".into()));
                }
                if ciphertext.len() > MAX_CONTENT_LEN * 4 / 3 + 32 {
                    return Err(A3chatError::InvalidInput(
                        "body.ciphertext: exceeds encoded length bound".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Type discriminator for [`ChatMessage::message_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Text,
    Image,
    File,
    Voice,
    Video,
    /// Server-issued, e.g. "Alice joined the group". Always plaintext.
    System,
    /// Call signalling / push-to-talk marker. Body is opaque.
    Call,
}

impl MessageType {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageType::Text => "text",
            MessageType::Image => "image",
            MessageType::File => "file",
            MessageType::Voice => "voice",
            MessageType::Video => "video",
            MessageType::System => "system",
            MessageType::Call => "call",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "text" => MessageType::Text,
            "image" => MessageType::Image,
            "file" => MessageType::File,
            "voice" => MessageType::Voice,
            "video" => MessageType::Video,
            "system" => MessageType::System,
            "call" => MessageType::Call,
            // Forward-compat: unknown kinds deserialize as None; the
            // strict path is `parse_strict`.
            _ => return None,
        })
    }
}

/// Attachment reference — `blob_hash` points to content addressed in
/// `a3net-blobstore`; `thumbnail_hash` is an optional preview blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Attachment {
    pub attachment_id: String,
    pub file_type: AttachmentKind,
    pub blob_hash: String,
    pub file_name: String,
    pub file_size: u64,
    pub thumbnail_hash: Option<String>,
}

impl Attachment {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("attachment_id", &self.attachment_id)?;
        // blob_hash length matches ContentHash::HEX_LEN = 64.
        validate_hex("blob_hash", &self.blob_hash, 64)?;
        if let Some(t) = &self.thumbnail_hash {
            validate_hex("thumbnail_hash", t, 64)?;
        }
        // file_name is required to be non-empty.
        if self.file_name.is_empty() {
            return Err(A3chatError::InvalidInput("file_name: empty".into()));
        }
        if self.file_name.len() > 256 {
            return Err(A3chatError::InvalidInput(format!(
                "file_name: length {} > 256",
                self.file_name.len()
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Audio,
    Video,
    File,
}

/// The full message envelope — what crosses the wire and lands in
/// SQLite. Includes read receipt fields and edit history so the
/// `ChatMessage` type doubles as the persistence row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatMessage {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub sender_id: UserId,
    /// Empty for group messages.
    pub receiver_id: UserId,
    pub message_type: MessageType,
    pub body: MessageBody,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<MessageId>,
    /// Per-sender monotonic sequence number. Used for gap detection.
    pub sequence: u32,
    /// Unix timestamp (seconds since epoch).
    pub timestamp: i64,
    /// RFC3339 — when the message was first read by the local user.
    /// `None` until ack.
    pub read_at: Option<DateTime<Utc>>,
    /// True if the message was edited. The edit must re-stamp the
    /// integrity hash (or for E2E, re-encrypt the body).
    pub is_edited: bool,
    /// RFC3339 of the most recent edit. Required iff `is_edited`.
    pub edited_at: Option<DateTime<Utc>>,
    /// SHA-256(sender | receiver | body_digest | sequence | timestamp).
    /// Detects in-transit tampering for non-E2E bodies; for E2E bodies
    /// the AEAD tag serves the same role so this is allowed to be
    /// `None`.
    pub integrity_hash: Option<String>,
    /// Set to `Some(_)` when the sender retracted the message. The
    /// body is still persisted for audit but UI must hide it.
    pub recalled_at: Option<DateTime<Utc>>,
}

impl ChatMessage {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("message_id", self.message_id.as_str())?;
        validate_id("conversation_id", self.conversation_id.as_str())?;
        validate_id("sender_id", self.sender_id.as_str())?;
        // For system messages the receiver is implicit (the
        // recipient set is "everyone in the conversation") so we
        // permit an empty receiver_id.
        if self.message_type != MessageType::System {
            validate_id("receiver_id", self.receiver_id.as_str())?;
        }
        self.body.validate()?;
        validate_attachments("attachments", self.attachments.len())?;
        for (i, a) in self.attachments.iter().enumerate() {
            a.validate().map_err(|e| match e {
                A3chatError::InvalidInput(msg) => {
                    A3chatError::InvalidInput(format!("attachments[{i}]: {msg}"))
                }
                other => other,
            })?;
        }
        if let Some(r) = &self.reply_to {
            validate_id("reply_to", r.as_str())?;
        }
        validate_sequence("sequence", self.sequence, MAX_SEQUENCE)?;
        if self.timestamp < 0 {
            return Err(A3chatError::InvalidInput(format!(
                "timestamp: negative {}",
                self.timestamp
            )));
        }
        if self.is_edited {
            let ea = self.edited_at.ok_or_else(|| {
                A3chatError::InvalidInput("is_edited=true with edited_at=None".into())
            })?;
            let ts_secs = self.timestamp;
            if ea.timestamp() < ts_secs {
                return Err(A3chatError::InvalidInput(format!(
                    "edited_at {} < timestamp {}",
                    ea, ts_secs
                )));
            }
        } else if self.edited_at.is_some() {
            return Err(A3chatError::InvalidInput(
                "edited_at set while is_edited=false".into(),
            ));
        }
        Ok(())
    }

    /// Convenience — caller-friendly constructor for plaintext system
    /// messages (no encryption). Returns `A3chatError::CryptoError` if
    /// you accidentally pass encrypted body here.
    pub fn new_system(
        conversation_id: ConversationId,
        sender_id: UserId,
        content: impl Into<String>,
        timestamp: i64,
        sequence: u32,
    ) -> Result<Self, A3chatError> {
        let body = MessageBody::Plain {
            content: content.into(),
        };
        body.validate()?;
        Ok(Self {
            message_id: crate::id::generate_message_id(sender_id.as_str()),
            conversation_id,
            sender_id,
            receiver_id: UserId::from(""),
            message_type: MessageType::System,
            body,
            attachments: vec![],
            reply_to: None,
            sequence,
            timestamp,
            read_at: None,
            is_edited: false,
            edited_at: None,
            integrity_hash: None,
            recalled_at: None,
        })
    }
}

/// Outbound-only envelope — what `chat.message.send` carries over the
/// wire from client → daemon. Excludes server-managed fields
/// (`read_at`, `integrity_hash`, …) which are filled in by the
/// `ChatService`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageEnvelope {
    pub conversation_id: ConversationId,
    pub receiver_id: UserId,
    pub message_type: MessageType,
    pub body: MessageBody,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<MessageId>,
    /// Client-supplied sequence number (per-sender monotonic). The
    /// daemon will reject out-of-order numbers.
    pub sequence: u32,
    pub timestamp: i64,
}

impl MessageEnvelope {
    pub fn validate(&self) -> Result<(), A3chatError> {
        validate_id("conversation_id", self.conversation_id.as_str())?;
        validate_id("receiver_id", self.receiver_id.as_str())?;
        self.body.validate()?;
        validate_attachments("attachments", self.attachments.len())?;
        for (i, a) in self.attachments.iter().enumerate() {
            a.validate().map_err(|e| match e {
                A3chatError::InvalidInput(msg) => {
                    A3chatError::InvalidInput(format!("attachments[{i}]: {msg}"))
                }
                other => other,
            })?;
        }
        if let Some(r) = &self.reply_to {
            validate_id("reply_to", r.as_str())?;
        }
        validate_sequence("sequence", self.sequence, MAX_SEQUENCE)?;
        if self.timestamp < 0 {
            return Err(A3chatError::InvalidInput(format!(
                "timestamp: negative {}",
                self.timestamp
            )));
        }
        // Reject far-future timestamps (> now + 5 min) — clients
        // sending timestamps wildly ahead of wall clock are usually
        // a misconfiguration or a malicious clock-skew attempt.
        let now = chrono::Utc::now().timestamp();
        let skew_limit: i64 = 5 * 60;
        if self.timestamp > now + skew_limit {
            return Err(A3chatError::InvalidInput(format!(
                "timestamp: {} is more than {skew_limit}s ahead of now ({now})",
                self.timestamp
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    fn envelope(ts: i64) -> MessageEnvelope {
        MessageEnvelope {
            conversation_id: ConversationId::from("dm:a:b"),
            receiver_id: UserId::from("bob-node"),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hi".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: ts,
        }
    }

    #[test]
    fn envelope_accepts_now_timestamp() {
        let now = chrono::Utc::now().timestamp();
        assert!(envelope(now).validate().is_ok());
    }

    #[test]
    fn envelope_rejects_negative_timestamp() {
        let err = envelope(-1).validate().unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[test]
    fn envelope_rejects_far_future_timestamp() {
        let now = chrono::Utc::now().timestamp();
        let skewed = now + 24 * 60 * 60; // 1 day ahead
        let err = envelope(skewed).validate().unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ConversationId, UserId};

    fn msg(body: MessageBody) -> ChatMessage {
        ChatMessage {
            message_id: crate::id::generate_message_id("node-1"),
            conversation_id: ConversationId::from("dm:a:b"),
            sender_id: UserId::from("alice-node"),
            receiver_id: UserId::from("bob-node"),
            message_type: MessageType::Text,
            body,
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
            read_at: None,
            is_edited: false,
            edited_at: None,
            integrity_hash: None,
            recalled_at: None,
        }
    }

    #[test]
    fn plaintext_message_validates() {
        let m = msg(MessageBody::Plain {
            content: "hello".into(),
        });
        assert!(m.validate().is_ok());
    }

    #[test]
    fn encrypted_message_validates_with_well_formed_nonce_tag() {
        let m = msg(MessageBody::Encrypted {
            algorithm: "chacha20-poly1305-v1".into(),
            nonce: "a".repeat(24),
            ciphertext: "ZW5jcnlwdGVk".into(),
            tag: "b".repeat(32),
        });
        assert!(m.validate().is_ok());
    }

    #[test]
    fn encrypted_message_rejects_bad_nonce() {
        let m = msg(MessageBody::Encrypted {
            algorithm: "chacha20-poly1305-v1".into(),
            nonce: "short".into(),
            ciphertext: "ZW5jcnlwdGVk".into(),
            tag: "b".repeat(32),
        });
        assert!(m.validate().is_err());
    }

    #[test]
    fn encrypted_message_rejects_bad_tag() {
        let m = msg(MessageBody::Encrypted {
            algorithm: "chacha20-poly1305-v1".into(),
            nonce: "a".repeat(24),
            ciphertext: "ZW5jcnlwdGVk".into(),
            tag: "short".into(),
        });
        assert!(m.validate().is_err());
    }

    #[test]
    fn edited_requires_edited_at() {
        let mut m = msg(MessageBody::Plain {
            content: "hi".into(),
        });
        m.is_edited = true;
        m.edited_at = None;
        assert!(m.validate().is_err());
        m.edited_at = Some(chrono::Utc::now());
        assert!(m.validate().is_ok());
    }

    #[test]
    fn message_type_round_trip() {
        for t in [
            MessageType::Text,
            MessageType::Image,
            MessageType::File,
            MessageType::Voice,
            MessageType::Video,
            MessageType::System,
            MessageType::Call,
        ] {
            assert_eq!(MessageType::parse(t.as_str()), Some(t));
        }
        assert_eq!(MessageType::parse("bogus"), None);
    }

    #[test]
    fn attachment_kind_serializes_snake_case() {
        let v = serde_json::to_value(AttachmentKind::Image).unwrap();
        assert_eq!(v, serde_json::json!("image"));
    }

    #[test]
    fn new_system_helper() {
        let m = ChatMessage::new_system(
            ConversationId::from("grp:x"),
            UserId::from("server"),
            "Alice joined the group",
            1_700_000_000,
            1,
        )
        .unwrap();
        assert!(m.validate().is_ok());
        assert_eq!(m.message_type, MessageType::System);
        assert!(m.body.is_plain());
    }

    #[test]
    fn message_envelope_validates() {
        let env = MessageEnvelope {
            conversation_id: ConversationId::from("dm:a:b"),
            receiver_id: UserId::from("bob-node"),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: "hi".into(),
            },
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1_700_000_000,
        };
        assert!(env.validate().is_ok());
    }

    #[test]
    fn plaintext_body_preview_keeps_short_content_verbatim() {
        let b = MessageBody::Plain {
            content: "hello".into(),
        };
        assert_eq!(b.preview(), "hello");
    }

    #[test]
    fn plaintext_body_preview_truncates_long_content() {
        let long = "a".repeat(MAX_PREVIEW_LEN + 10);
        let b = MessageBody::Plain { content: long };
        let p = b.preview();
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= MAX_PREVIEW_LEN + 1);
    }

    #[test]
    fn plaintext_body_preview_handles_multibyte_codepoints() {
        // Each "🦀" is 1 char but 4 bytes. Counting bytes alone
        // would truncate mid-codepoint; chars() must be used.
        let long = "🦀".repeat(MAX_PREVIEW_LEN + 50);
        let b = MessageBody::Plain { content: long };
        let p = b.preview();
        assert!(p.ends_with('…'));
        // Truncation yields at most MAX_PREVIEW_LEN chars + 1 ellipsis.
        assert!(p.chars().count() <= MAX_PREVIEW_LEN + 1);
    }

    #[test]
    fn encrypted_body_preview_hides_ciphertext() {
        let b = MessageBody::Encrypted {
            algorithm: "chacha20-poly1305-v1".into(),
            nonce: "a".repeat(24),
            ciphertext: "ZW5jcnlwdGVk".into(),
            tag: "b".repeat(32),
        };
        assert_eq!(b.preview(), "[encrypted]");
    }

    #[test]
    fn truncate_preview_short_input_is_returned_as_is() {
        assert_eq!(truncate_preview("hi", 10), "hi");
    }

    #[test]
    fn truncate_preview_keeps_max_chars_then_appends_ellipsis() {
        let s = "x".repeat(20);
        let p = truncate_preview(&s, 5);
        assert_eq!(p, "xxxxx…");
    }
}
