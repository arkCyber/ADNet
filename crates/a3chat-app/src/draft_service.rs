//! DraftService — message draft persistence per conversation.
//!
//! Stores unsent message drafts so users don't lose typed content when
//! switching conversations or when the app is killed unexpectedly.
//!
//! Drafts are persisted in [`crate::storage::ChatStorage`] so the
//! daemon restart preserves what was typed. The implementation keeps
//! an in-memory cache of the per-user draft rows for hot reads, but
//! the durable copy lives in SQLite and is rehydrated on first
//! access.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use a3chat_core::id::{ConversationId, UserId};

use crate::error::{AppError, AppResult};
use crate::storage::{ChatStorage, DraftRow};

/// A saved draft for a conversation. Field-compatible with the wire
/// shape (renamed to camelCase) so the RPC layer can serialise it
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Draft {
    /// The conversation this draft belongs to.
    pub conversation_id: ConversationId,
    /// The draft text content. Compared by character count (not bytes)
    /// via [`Draft::validate`] so emoji-heavy drafts aren't truncated
    /// early.
    pub content: String,
    /// Reply-to message ID if composing a reply.
    pub reply_to: Option<a3chat_core::id::MessageId>,
    /// When the draft was last updated.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Draft {
    #[must_use = "constructing a draft without saving it is a bug"]
    pub fn new(conversation_id: ConversationId, content: String) -> Self {
        Self {
            conversation_id,
            content,
            reply_to: None,
            updated_at: chrono::Utc::now(),
        }
    }

    #[must_use = "with_reply builds a new draft; do not discard the result"]
    pub fn with_reply(mut self, reply_to: a3chat_core::id::MessageId) -> Self {
        self.reply_to = Some(reply_to);
        self
    }

    /// Validate the draft. Catches the cases that would otherwise
    /// only be caught later in [`DraftService::save`].
    pub fn validate(&self) -> Result<(), AppError> {
        use a3chat_core::id::validate_id;
        if self.conversation_id.as_str().is_empty() {
            return Err(AppError::Domain("conversation_id must be non-empty".into()));
        }
        validate_id("conversation_id", self.conversation_id.as_str())
            .map_err(|e| AppError::Domain(e.to_string()))?;
        let char_count = self.content.chars().count();
        if char_count > MAX_DRAFT_LEN {
            return Err(AppError::Domain(format!(
                "draft content is {char_count} chars (max {MAX_DRAFT_LEN})"
            )));
        }
        if let Some(rt) = &self.reply_to {
            validate_id("reply_to", rt.as_str())
                .map_err(|e| AppError::Domain(e.to_string()))?;
        }
        Ok(())
    }
}

impl From<(ConversationId, DraftRow)> for Draft {
    fn from((conv_id, row): (ConversationId, DraftRow)) -> Self {
        Self {
            conversation_id: conv_id,
            content: row.content,
            reply_to: row.reply_to,
            updated_at: row.updated_at,
        }
    }
}

/// Maximum length of a draft message, in **characters** (not bytes).
/// 4096 chars matches the typical IM message cap and tolerates
/// emoji-heavy content.
pub const MAX_DRAFT_LEN: usize = 4096;

/// Hard cap on drafts per user. Caps memory and protects against a
/// client that floods the API with auto-saved drafts for fake
/// conversation ids.
pub const MAX_DRAFTS_PER_USER: usize = 1024;

/// Service for managing message drafts.
///
/// Two constructors are provided:
/// * [`DraftService::new`] — in-memory only, used by tests.
/// * [`DraftService::with_storage`] — backed by the shared
///   [`ChatStorage`] so drafts survive restarts.
#[derive(Clone)]
pub struct DraftService {
    storage: Option<Arc<ChatStorage>>,
    /// Per-user in-memory cache, lazily filled on first read for the
    /// hot path. The cache is authoritative for the lifetime of the
    /// daemon; the SQLite rows are used for cold-start hydration only.
    drafts: Arc<tokio::sync::RwLock<HashMap<String, HashMap<String, Draft>>>>,
}

impl Default for DraftService {
    fn default() -> Self {
        Self::new()
    }
}

impl DraftService {
    /// In-memory draft service. Used by tests.
    #[must_use = "constructing a draft service without using it is a bug"]
    pub fn new() -> Self {
        Self {
            storage: None,
            drafts: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Persisted draft service sharing the same [`ChatStorage`] as
    /// the rest of the app.
    #[must_use = "constructing a draft service without using it is a bug"]
    pub fn with_storage(storage: Arc<ChatStorage>) -> Self {
        Self {
            storage: Some(storage),
            drafts: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Save a draft for a conversation. Rejects empty content
    /// (which would silently overwrite a previous draft with a
    /// blank one — a UX trap).
    pub async fn save(
        &self,
        user_id: &UserId,
        draft: Draft,
    ) -> AppResult<()> {
        draft.validate()?;
        if draft.content.is_empty() {
            // Empty drafts are reconciled by `delete`, not `save`.
            // Prevents `save("", content="")` from clearing prior
            // content without the caller knowing.
            return self
                .delete(user_id, &draft.conversation_id)
                .await
                .map(|_| ());
        }
        // Per-user cap, before the insert. The cap mirrors
        // the in-memory limit so callers don't observe a different
        // ceiling when persistence is enabled.
        {
            let store = self.drafts.read().await;
            let user_drafts = store.get(user_id.as_str());
            if let Some(ud) = user_drafts {
                if !ud.contains_key(draft.conversation_id.as_str())
                    && ud.len() >= MAX_DRAFTS_PER_USER
                {
                    return Err(AppError::Domain(format!(
                        "draft limit reached ({MAX_DRAFTS_PER_USER}) per user"
                    )));
                }
            }
        }
        // Persist first — if the SQLite write fails we leave the
        // cache untouched so the user-visible state matches disk.
        if let Some(storage) = &self.storage {
            storage
                .save_draft(
                    user_id,
                    &draft.conversation_id,
                    &draft.content,
                    draft.reply_to.as_ref(),
                )
                .await?;
        }
        let mut store = self.drafts.write().await;
        let user_drafts = store
            .entry(user_id.as_str().to_string())
            .or_default();
        user_drafts.insert(
            draft.conversation_id.as_str().to_string(),
            draft,
        );
        Ok(())
    }

    /// Get the draft for a specific conversation. Reads the cache
    /// first; on a miss falls back to SQLite (and primes the cache).
    pub async fn get(
        &self,
        user_id: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<Option<Draft>> {
        // Cache fast path.
        {
            let store = self.drafts.read().await;
            if let Some(ud) = store.get(user_id.as_str()) {
                if let Some(d) = ud.get(conversation_id.as_str()) {
                    return Ok(Some(d.clone()));
                }
            }
        }
        // Fall through to storage.
        let Some(storage) = &self.storage else {
            return Ok(None);
        };
        let row = storage.get_draft(user_id, conversation_id).await?;
        let Some(row) = row else { return Ok(None) };
        let draft: Draft = (conversation_id.clone(), row).into();
        // Prime cache so subsequent reads don't re-read SQLite.
        let mut store = self.drafts.write().await;
        store
            .entry(user_id.as_str().to_string())
            .or_default()
            .insert(conversation_id.as_str().to_string(), draft.clone());
        Ok(Some(draft))
    }

    /// Delete a draft for a specific conversation.
    pub async fn delete(
        &self,
        user_id: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<bool> {
        let mut removed = false;
        {
            let mut store = self.drafts.write().await;
            if let Some(ud) = store.get_mut(user_id.as_str()) {
                removed = ud.remove(conversation_id.as_str()).is_some();
            }
        }
        if let Some(storage) = &self.storage {
            let disk_removed = storage.delete_draft(user_id, conversation_id).await?;
            removed = removed || disk_removed;
        }
        Ok(removed)
    }

    /// List all drafts for a user.
    pub async fn list(&self, user_id: &UserId) -> AppResult<Vec<Draft>> {
        if let Some(storage) = &self.storage {
            // Cold-start: prime cache from SQLite.
            let rows = storage.list_drafts(user_id).await?;
            let mut cache = self.drafts.write().await;
            let ud = cache.entry(user_id.as_str().to_string()).or_default();
            ud.clear();
            for (cid, row) in rows {
                ud.insert(cid.as_str().to_string(), (cid, row).into());
            }
        }
        let store = self.drafts.read().await;
        let user_drafts = match store.get(user_id.as_str()) {
            Some(d) => d,
            None => return Ok(vec![]),
        };
        Ok(user_drafts.values().cloned().collect())
    }

    /// Clear all drafts for a user.
    pub async fn clear_all(&self, user_id: &UserId) -> AppResult<()> {
        {
            let mut store = self.drafts.write().await;
            store.remove(user_id.as_str());
        }
        if let Some(storage) = &self.storage {
            storage.clear_drafts(user_id).await?;
        }
        Ok(())
    }

    /// Check if a conversation has a draft.
    pub async fn has_draft(
        &self,
        user_id: &UserId,
        conversation_id: &ConversationId,
    ) -> AppResult<bool> {
        Ok(self.get(user_id, conversation_id).await?.is_some())
    }
}

/// Dispatch helper for `a3chat-rpc`.
pub async fn dispatch(
    svc: Arc<DraftService>,
    method: &str,
    owner: &UserId,
    params: serde_json::Value,
) -> Result<serde_json::Value, a3chat_core::error::A3chatError> {
    use a3chat_core::error::A3chatError;

    match method {
        "a3chat.chat.draft.save" => {
            let conversation_id: ConversationId = serde_json::from_value(
                params.get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            // `content` is required and must be a string. Defaulting
            // to "" used to silently overwrite prior drafts — see
            // the audit-trail commit history.
            let content: String = params
                .get("content")
                .ok_or_else(|| A3chatError::InvalidInput("content missing".into()))
                .and_then(|v| {
                    serde_json::from_value(v.clone()).map_err(A3chatError::from)
                })?;

            // `reply_to` parsing now surfaces errors instead of
            // silently swallowing them.
            let reply_to: Option<a3chat_core::id::MessageId> = match params.get("reply_to") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => Some(
                    serde_json::from_value(v.clone())
                        .map_err(|e| A3chatError::InvalidInput(format!("reply_to: {e}")))?,
                ),
            };

            let mut draft = Draft::new(conversation_id, content);
            if let Some(rt) = reply_to {
                draft = draft.with_reply(rt);
            }

            svc.save(owner, draft).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "a3chat.chat.draft.get" => {
            let conversation_id: ConversationId = serde_json::from_value(
                params.get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            let draft = svc.get(owner, &conversation_id).await.map_err(A3chatError::from)?;
            match draft {
                Some(d) => serde_json::to_value(d).map_err(A3chatError::from),
                None => Ok(serde_json::json!(null)),
            }
        }
        "a3chat.chat.draft.delete" => {
            let conversation_id: ConversationId = serde_json::from_value(
                params.get("conversation_id")
                    .cloned()
                    .ok_or_else(|| A3chatError::InvalidInput("conversation_id missing".into()))?,
            )
            .map_err(A3chatError::from)?;

            let deleted = svc.delete(owner, &conversation_id).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "deleted": deleted }))
        }
        "a3chat.chat.draft.list" => {
            let drafts = svc.list(owner).await.map_err(A3chatError::from)?;
            serde_json::to_value(drafts).map_err(A3chatError::from)
        }
        "a3chat.chat.draft.clear" => {
            svc.clear_all(owner).await.map_err(A3chatError::from)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        _ => Err(A3chatError::Internal(format!(
            "DraftService does not handle {method}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> UserId {
        UserId::from("alice")
    }

    fn conversation_id() -> ConversationId {
        ConversationId::from("dm:alice:bob")
    }

    #[tokio::test]
    async fn save_and_get_draft() {
        let svc = DraftService::new();

        let draft = Draft::new(conversation_id(), "Hello there!".to_string());
        svc.save(&owner(), draft).await.unwrap();

        let retrieved = svc.get(&owner(), &conversation_id()).await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().content, "Hello there!");
    }

    #[tokio::test]
    async fn delete_draft() {
        let svc = DraftService::new();

        let draft = Draft::new(conversation_id(), "Test".to_string());
        svc.save(&owner(), draft).await.unwrap();

        let deleted = svc.delete(&owner(), &conversation_id()).await.unwrap();
        assert!(deleted);

        let retrieved = svc.get(&owner(), &conversation_id()).await.unwrap();
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn list_drafts() {
        let svc = DraftService::new();

        let draft1 = Draft::new(conversation_id(), "Draft 1".to_string());
        let draft2 = Draft::new(ConversationId::from("dm:alice:carol"), "Draft 2".to_string());

        svc.save(&owner(), draft1).await.unwrap();
        svc.save(&owner(), draft2).await.unwrap();

        let drafts = svc.list(&owner()).await.unwrap();
        assert_eq!(drafts.len(), 2);
    }

    #[tokio::test]
    async fn clear_all_drafts() {
        let svc = DraftService::new();

        let draft1 = Draft::new(conversation_id(), "Draft 1".to_string());
        let draft2 = Draft::new(ConversationId::from("dm:alice:carol"), "Draft 2".to_string());

        svc.save(&owner(), draft1).await.unwrap();
        svc.save(&owner(), draft2).await.unwrap();

        svc.clear_all(&owner()).await.unwrap();

        let drafts = svc.list(&owner()).await.unwrap();
        assert!(drafts.is_empty());
    }

    #[tokio::test]
    async fn draft_with_reply() {
        let svc = DraftService::new();

        let reply_id = a3chat_core::id::MessageId::from("msg-reply-to");
        let draft = Draft::new(conversation_id(), "Replying...".to_string())
            .with_reply(reply_id.clone());

        svc.save(&owner(), draft).await.unwrap();

        let retrieved = svc.get(&owner(), &conversation_id()).await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().reply_to, Some(reply_id));
    }

    #[tokio::test]
    async fn oversized_draft_rejected() {
        let svc = DraftService::new();

        let long_content = "x".repeat(MAX_DRAFT_LEN + 1);
        let draft = Draft::new(conversation_id(), long_content);

        let result = svc.save(&owner(), draft).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn has_draft() {
        let svc = DraftService::new();

        assert!(!svc.has_draft(&owner(), &conversation_id()).await.unwrap());

        let draft = Draft::new(conversation_id(), "Draft".to_string());
        svc.save(&owner(), draft).await.unwrap();

        assert!(svc.has_draft(&owner(), &conversation_id()).await.unwrap());
    }

    #[tokio::test]
    async fn different_users_have_separate_drafts() {
        let svc = DraftService::new();

        let bob = UserId::from("bob");

        let draft_alice = Draft::new(conversation_id(), "Alice's draft".to_string());
        let draft_bob = Draft::new(conversation_id(), "Bob's draft".to_string());

        svc.save(&owner(), draft_alice).await.unwrap();
        svc.save(&bob, draft_bob).await.unwrap();

        let alice_draft = svc.get(&owner(), &conversation_id()).await.unwrap();
        let bob_draft = svc.get(&bob, &conversation_id()).await.unwrap();

        assert_eq!(alice_draft.unwrap().content, "Alice's draft");
        assert_eq!(bob_draft.unwrap().content, "Bob's draft");
    }
}
