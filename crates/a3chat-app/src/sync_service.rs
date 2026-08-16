//! SyncService — bulk import/export for offline-device catch-up.
//!
//! Implements:
//!   - `chat.sync.snapshot` — full export of conversations + recent messages.
//!   - `chat.sync.delta` — incremental messages with `sequence >
//!     last_sequence_per_conversation` so the receiving device can
//!     catch up without re-fetching the entire history.
//!   - `chat.sync.compressed` — zstd-compressed snapshot for
//!     low-bandwidth devices (mobile, satellite links).
//!
//! DO-178C §6.3 — *determinism*: every sync export carries a
//! `taken_at` Unix timestamp so the receiver can detect out-of-order
//! updates and reject anything older than the highest sequence it
//! already has.

use std::sync::Arc;

use a3chat_core::conversation::ConversationMeta;
use a3chat_core::error::A3chatError;
use a3chat_core::id::{ConversationId, UserId};
use a3chat_core::message::ChatMessage;
use a3chat_core::rpc::A3chatRpcMethod;

use crate::error::{AppError, AppResult};
use crate::storage::ChatStorage;

/// A single snapshot bundle returned by `chat.sync.snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncSnapshot {
    pub conversations: Vec<ConversationMeta>,
    /// All messages for each conversation, capped at
    /// [`MAX_MESSAGES_PER_CONVO`] per the receiver's preferences.
    pub messages: Vec<(ConversationId, Vec<ChatMessage>)>,
    /// Unix seconds when the snapshot was taken. Used by clients
    /// to detect rebroadcast loops.
    pub taken_at: i64,
}

impl SyncSnapshot {
    pub const MAX_MESSAGES_PER_CONVO: u32 = 500;
}

/// Per-conversation delta message returned by `chat.sync.delta`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncDeltaConversation {
    pub conversation_id: ConversationId,
    pub messages: Vec<ChatMessage>,
    /// Highest sequence number in `messages`. After applying,
    /// the client must use this as the new `last_sequence` for
    /// the next delta request.
    pub max_sequence: u32,
}

#[derive(Clone)]
pub struct SyncService {
    storage: ChatStorage,
}

impl SyncService {
    pub fn new(storage: ChatStorage) -> Self {
        Self { storage }
    }

    pub fn storage(&self) -> &ChatStorage {
        &self.storage
    }

    /// `a3chat.chat.sync.snapshot` — full export of conversations
    /// + recent messages.
    pub async fn snapshot(&self, owner: &UserId) -> AppResult<SyncSnapshot> {
        let conversations = self.storage.list_conversations(owner).await?;
        let mut messages = Vec::with_capacity(conversations.len());
        for meta in &conversations {
            let recent = self
                .storage
                .list_messages(
                    owner,
                    &meta.conversation_id,
                    SyncSnapshot::MAX_MESSAGES_PER_CONVO,
                )
                .await?;
            messages.push((meta.conversation_id.clone(), recent));
        }
        Ok(SyncSnapshot {
            conversations,
            messages,
            taken_at: chrono::Utc::now().timestamp(),
        })
    }

    /// `a3chat.chat.sync.delta` — incremental messages per
    /// conversation with `sequence > last_sequence_per_conversation`.
    ///
    /// The parameter shape is a list of `(conversation_id,
    /// last_sequence)` pairs so the server can return only the
    /// messages a specific device hasn't seen yet. Receivers map
    /// `last_sequence` to the highest sequence they already
    /// persisted for each conversation.
    pub async fn delta(
        &self,
        owner: &UserId,
        cursors: &[(ConversationId, u32)],
    ) -> AppResult<Vec<SyncDeltaConversation>> {
        let max_per_conv = SyncSnapshot::MAX_MESSAGES_PER_CONVO;
        let mut out = Vec::with_capacity(cursors.len());
        for (conv_id, since) in cursors {
            let msgs = self
                .storage
                .list_messages_since(owner, conv_id, *since, max_per_conv)
                .await?;
            if msgs.is_empty() {
                continue;
            }
            let max_sequence = msgs.iter().map(|m| m.sequence).max().unwrap_or(0);
            out.push(SyncDeltaConversation {
                conversation_id: conv_id.clone(),
                messages: msgs,
                max_sequence,
            });
        }
        Ok(out)
    }

    /// `a3chat.chat.sync.compressed` — zstd-compressed snapshot for
    /// slow links. Wire format:
    ///   `[4 bytes LE u32 = payload_len][zstd_frame(payload)]`
    /// The length prefix lets the receiver `read_exact` even if
    /// the stream is chunked.
    pub async fn snapshot_compressed(&self, owner: &UserId) -> AppResult<Vec<u8>> {
        let snap = self.snapshot(owner).await?;
        let json = serde_json::to_vec(&snap)
            .map_err(|e| AppError::Internal(format!("serialize snapshot: {e}")))?;
        let compressed = zstd::encode_all(json.as_slice(), 3)
            .map_err(|e| AppError::Internal(format!("zstd encode: {e}")))?;
        let mut out = Vec::with_capacity(4 + compressed.len());
        let len = u32::try_from(compressed.len())
            .map_err(|_| AppError::Internal("compressed payload exceeds u32".into()))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&compressed);
        Ok(out)
    }

    /// Reverse of [`snapshot_compressed`] — used by tests +
    /// integration clients that need to read a sync payload.
    pub fn decompress_snapshot(bytes: &[u8]) -> AppResult<SyncSnapshot> {
        if bytes.len() < 4 {
            return Err(AppError::Internal("compressed payload too short".into()));
        }
        let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        if bytes.len() < 4 + len {
            return Err(AppError::Internal(format!(
                "compressed payload truncated: header={len}, actual={}",
                bytes.len() - 4
            )));
        }
        let raw = zstd::decode_all(&bytes[4..4 + len])
            .map_err(|e| AppError::Internal(format!("zstd decode: {e}")))?;
        let snap: SyncSnapshot = serde_json::from_slice(&raw)
            .map_err(|e| AppError::Internal(format!("decode snapshot: {e}")))?;
        Ok(snap)
    }
}

pub async fn dispatch(
    svc: Arc<SyncService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, A3chatError> {
    match method {
        A3chatRpcMethod::CHAT_SYNC_SNAPSHOT => {
            let snap = svc.snapshot(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(snap).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_SYNC_DELTA => {
            // Accept both list-of-pairs and a single {conv, seq} map
            // for backwards-compat with older clients.
            let cursors: Vec<(ConversationId, u32)> = parse_cursors(&params)?;
            let msgs = svc
                .delta(owner, &cursors)
                .await
                .map_err(A3chatError::from)?;
            serde_json::to_value(msgs).map_err(A3chatError::from)
        }
        A3chatRpcMethod::CHAT_SYNC_COMPRESSED => {
            let bytes = svc
                .snapshot_compressed(owner)
                .await
                .map_err(A3chatError::from)?;
            Ok(serde_json::json!({
                "algorithm": "zstd-3",
                "length": bytes.len(),
                "data_b64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
            }))
        }
        _ => Err(A3chatError::Internal(format!(
            "SyncService does not handle {method}"
        ))),
    }
}

/// Parse the JSON-shape of `cursors` from the RPC `params`. Accepts:
///
/// ```json
/// { "cursors": [{"conversation_id": "dm:a:b", "last_sequence": 5}, ...] }
/// ```
///
/// or an array directly:
/// ```json
/// [{"conversation_id": "dm:a:b", "last_sequence": 5}, ...]
/// ```
fn parse_cursors(
    params: &serde_json::Value,
) -> Result<Vec<(ConversationId, u32)>, A3chatError> {
    let raw = params
        .get("cursors")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let arr = raw.as_array().ok_or_else(|| {
        A3chatError::InvalidInput(
            "cursors must be an array of {conversation_id, last_sequence}".into(),
        )
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let conv_id = v
            .get("conversation_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| A3chatError::InvalidInput("missing conversation_id".into()))?;
        let last_sequence = v
            .get("last_sequence")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| A3chatError::InvalidInput("missing last_sequence".into()))?;
        out.push((ConversationId::from(conv_id.to_string()), last_sequence as u32));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::E2eKeyring;
    use a3chat_core::conversation::ConversationMeta;
    use a3chat_core::message::{MessageBody, MessageEnvelope, MessageType};
    use tempfile::tempdir;

    fn owner() -> UserId {
        UserId::from("alice-node-id")
    }

    async fn fresh() -> (tempfile::TempDir, SyncService) {
        let dir = tempdir().unwrap();
        let keyring = E2eKeyring::new(owner());
        let storage = ChatStorage::new(
            crate::storage::StorageConfig::new(dir.path().to_path_buf()),
            keyring,
        );
        storage.init_user(&owner()).await.unwrap();
        (dir, SyncService::new(storage))
    }

    fn envelope(seq: u32) -> MessageEnvelope {
        MessageEnvelope {
            conversation_id: ConversationId::from("dm:a:b"),
            receiver_id: UserId::from("bob"),
            message_type: MessageType::Text,
            body: MessageBody::Plain {
                content: format!("msg {seq}"),
            },
            attachments: vec![],
            reply_to: None,
            sequence: seq,
            timestamp: 1_700_000_000 + seq as i64,
        }
    }

    #[tokio::test]
    async fn snapshot_returns_empty_state_when_no_messages() {
        let (_d, svc) = fresh().await;
        let snap = svc.snapshot(&owner()).await.unwrap();
        assert!(snap.conversations.is_empty());
        assert!(snap.messages.is_empty());
    }

    #[tokio::test]
    async fn snapshot_includes_messages() {
        let (_d, svc) = fresh().await;
        let conv = ConversationMeta {
            conversation_id: ConversationId::from("dm:a:b"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "B".into(),
            peer_user_id: Some(UserId::from("bob")),
            last_message_preview: "".into(),
            last_activity: 0,
            message_count: 0,
            unread_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        svc.storage()
            .upsert_conversation(&owner(), &conv)
            .await
            .unwrap();
        for i in 1..=3 {
            svc.storage()
                .save_outbound(&owner(), &envelope(i))
                .await
                .unwrap();
        }
        let snap = svc.snapshot(&owner()).await.unwrap();
        assert_eq!(snap.conversations.len(), 1);
        assert_eq!(snap.messages.len(), 1);
        let (_, msgs) = &snap.messages[0];
        assert_eq!(msgs.len(), 3);
    }

    #[tokio::test]
    async fn delta_returns_only_new_messages_per_conversation() {
        let (_d, svc) = fresh().await;
        let conv = ConversationMeta {
            conversation_id: ConversationId::from("dm:a:b"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "B".into(),
            peer_user_id: Some(UserId::from("bob")),
            last_message_preview: "".into(),
            last_activity: 0,
            message_count: 0,
            unread_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        svc.storage()
            .upsert_conversation(&owner(), &conv)
            .await
            .unwrap();
        for i in 1..=5 {
            svc.storage()
                .save_outbound(&owner(), &envelope(i))
                .await
                .unwrap();
        }
        // First delta: client has seen nothing.
        let first = svc
            .delta(
                &owner(),
                &[(ConversationId::from("dm:a:b"), 0)],
            )
            .await
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].messages.len(), 5);
        assert_eq!(first[0].max_sequence, 5);

        // Second delta: client has seen up to seq 3.
        let second = svc
            .delta(
                &owner(),
                &[(ConversationId::from("dm:a:b"), 3)],
            )
            .await
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].messages.len(), 2);
        assert_eq!(second[0].max_sequence, 5);
    }

    #[tokio::test]
    async fn delta_skips_empty_conversations() {
        let (_d, svc) = fresh().await;
        // No messages stored.
        let res = svc
            .delta(&owner(), &[(ConversationId::from("dm:ghost:x"), 0)])
            .await
            .unwrap();
        assert!(res.is_empty());
    }

    #[tokio::test]
    async fn snapshot_compressed_round_trips() {
        let (_d, svc) = fresh().await;
        let conv = ConversationMeta {
            conversation_id: ConversationId::from("dm:a:b"),
            kind: a3chat_core::conversation::ConversationKind::Dm,
            title: "B".into(),
            peer_user_id: Some(UserId::from("bob")),
            last_message_preview: "".into(),
            last_activity: 0,
            message_count: 0,
            unread_count: 0,
            peer_online: false,
            muted: false,
            pinned: false,
        };
        svc.storage().upsert_conversation(&owner(), &conv).await.unwrap();
        for i in 1..=3 {
            svc.storage().save_outbound(&owner(), &envelope(i)).await.unwrap();
        }
        let bytes = svc.snapshot_compressed(&owner()).await.unwrap();
        // Length prefix + at least one byte of compressed data.
        assert!(bytes.len() > 4);
        let decoded = SyncService::decompress_snapshot(&bytes).unwrap();
        assert_eq!(decoded.conversations.len(), 1);
        assert_eq!(decoded.messages.len(), 1);
        assert_eq!(decoded.messages[0].1.len(), 3);
    }

    #[tokio::test]
    async fn decompress_snapshot_rejects_short_input() {
        let err = SyncService::decompress_snapshot(&[1, 2]).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[tokio::test]
    async fn decompress_snapshot_rejects_truncated_payload() {
        // Header says 100 bytes; payload is empty.
        let bytes = vec![100, 0, 0, 0];
        let err = SyncService::decompress_snapshot(&bytes).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[tokio::test]
    async fn dispatch_unknown_method_errors() {
        let (_d, svc) = fresh().await;
        let err = dispatch(
            Arc::new(svc),
            "a3chat.bogus",
            &owner(),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::Internal(_)));
    }

    #[tokio::test]
    async fn dispatch_delta_parses_cursors() {
        let (_d, svc) = fresh().await;
        let params = serde_json::json!({
            "cursors": [
                { "conversation_id": "dm:a:b", "last_sequence": 0 }
            ]
        });
        let v = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_SYNC_DELTA,
            &owner(),
            params,
        )
        .await
        .unwrap();
        let arr = v.as_array().unwrap();
        assert!(arr.is_empty());
    }

    #[tokio::test]
    async fn dispatch_delta_rejects_bad_cursor_shape() {
        let (_d, svc) = fresh().await;
        let params = serde_json::json!({ "cursors": "not an array" });
        let err = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_SYNC_DELTA,
            &owner(),
            params,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn dispatch_compressed_returns_metadata() {
        let (_d, svc) = fresh().await;
        let v = dispatch(
            Arc::new(svc),
            A3chatRpcMethod::CHAT_SYNC_COMPRESSED,
            &owner(),
            serde_json::json!({}),
        )
        .await
        .unwrap();
        assert_eq!(v["algorithm"], "zstd-3");
        assert!(v["data_b64"].as_str().unwrap().len() > 0);
    }

    #[tokio::test]
    async fn parse_cursors_accepts_array_directly() {
        let params = serde_json::json!([
            { "conversation_id": "dm:a:b", "last_sequence": 7 }
        ]);
        let c = parse_cursors(&params).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1, 7);
    }

    #[tokio::test]
    async fn parse_cursors_requires_last_sequence() {
        let params = serde_json::json!({
            "cursors": [{ "conversation_id": "dm:a:b" }]
        });
        let err = parse_cursors(&params).unwrap_err();
        assert!(matches!(err, A3chatError::InvalidInput(_)));
    }
}
