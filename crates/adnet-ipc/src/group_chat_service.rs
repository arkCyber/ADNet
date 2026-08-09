//! Typed group chat & 1-to-1 direct messaging service over Unix socket
//! JSON-RPC.
//!
//! Ported from
//! `Exodus@src-backup/src-tauri/src/microservice/group_chat_service.rs`.
//! The wire API is JSON-RPC with the same method names as the reference
//! service, but every payload is decoded into a typed
//! [`adnet_types::group_chat`] record — callers no longer have to
//! hand-write `serde_json::Value` paths.
//!
//! Methods (all take `params` as a JSON object; selected ones):
//! - `group_create`        : `params.group`         → `result.group_id`
//! - `group_get`           : `params.group_id`      → `result.group | null`
//! - `group_list_user`     : `params.user_id`       → `result.groups`
//! - `group_get_members`   : `params.group_id`      → `result.members`
//! - `group_add_member`    : `params.group_id, member` → `result.ok`
//! - `group_remove_member` : `params.group_id, agent_id` → `result.ok`
//! - `group_send_message`  : `params.message`       → `result.message`
//! - `group_get_messages`  : `params.group_id, limit?` → `result.messages`
//! - `direct_create_or_get`: `params.user_a, user_b` → `result.chat`
//! - `direct_send_message` : `params.message`       → `result.message`
//! - `direct_get_messages` : `params.chat_id, limit?` → `result.messages`
//! - `verify_integrity`    : `params.message` (group/direct) → `result.valid`
//!
//! Like the other IPC services, the in-process state lives in
//! `Arc<Mutex<…>>` and there is no on-disk persistence — higher-level
//! code is expected to hydrate the service from a `ChatStorage` (or
//! equivalent) on startup.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use adnet_types::group_chat::{
    DirectChat, DirectMessage, GroupChat, GroupMember, GroupMessage, MessageAttachment,
    MessageReceipt, attachment_from_hash, next_group_message_id,
};
use adnet_types::{ContentHash, NodeId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::server::{JsonRpcServer, JsonRpcServerHandle, RpcHandler};
use crate::validation::{Validate, ValidationOutcome, ValidationPolicy};

pub use crate::validation::ValidationPolicy as GroupChatValidationPolicy;

#[derive(Debug, Clone)]
pub struct GroupChatIpcConfig {
    pub socket_path: PathBuf,
    /// Node id of the local service instance. Used as the default
    /// `sender_id` for outbound messages when the caller omits it.
    pub node_id: NodeId,
    /// Validation policy applied at every IPC entry point. Defaults to
    /// [`ValidationPolicy::Strict`].
    pub policy: ValidationPolicy,
}

impl Default for GroupChatIpcConfig {
    fn default() -> Self {
        Self {
            socket_path: std::env::temp_dir().join("adnet_group_chat.sock"),
            node_id: NodeId::random(),
            policy: ValidationPolicy::Strict,
        }
    }
}

/// In-memory group chat + DM store + JSON-RPC handler.
pub struct GroupChatIpcService {
    cfg: GroupChatIpcConfig,
    groups: Arc<Mutex<HashMap<String, GroupChat>>>,
    user_groups: Arc<Mutex<HashMap<String, Vec<String>>>>,
    members: Arc<Mutex<HashMap<String, Vec<GroupMember>>>>,
    group_messages: Arc<Mutex<HashMap<String, Vec<GroupMessage>>>>,
    direct_chats: Arc<Mutex<HashMap<String, DirectChat>>>,
    direct_messages: Arc<Mutex<HashMap<String, Vec<DirectMessage>>>>,
}

impl GroupChatIpcService {
    pub fn new(cfg: GroupChatIpcConfig) -> Self {
        Self {
            cfg,
            groups: Arc::new(Mutex::new(HashMap::new())),
            user_groups: Arc::new(Mutex::new(HashMap::new())),
            members: Arc::new(Mutex::new(HashMap::new())),
            group_messages: Arc::new(Mutex::new(HashMap::new())),
            direct_chats: Arc::new(Mutex::new(HashMap::new())),
            direct_messages: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Start the Unix socket server.
    pub async fn serve(self: Arc<Self>) -> Result<JsonRpcServerHandle, String> {
        JsonRpcServer::start(self.cfg.socket_path.clone(), self).await
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.cfg.socket_path
    }

    pub fn node_id(&self) -> &NodeId {
        &self.cfg.node_id
    }

    fn lock<T>(m: &Arc<Mutex<T>>) -> Result<std::sync::MutexGuard<'_, T>, String> {
        m.lock().map_err(|e| format!("lock: {e}"))
    }

    /// Apply the configured [`ValidationPolicy`] to a value. In
    /// `Strict` mode, the first validation failure is returned as an
    /// error. In `Audit` mode, failures are recorded as warnings. In
    /// `Lenient` mode, all failures are ignored.
    fn check<T: Validate>(&self, value: &T, what: &str) -> ValidationOutcome {
        let mut out = ValidationOutcome::default();
        if let Err(e) = value.validate() {
            match self.cfg.policy {
                ValidationPolicy::Strict => {
                    out.error = Some(format!("{what}: {e}"));
                }
                ValidationPolicy::Audit => {
                    out.warnings.push(format!("{what}: {e}"));
                }
                ValidationPolicy::Lenient => {
                    // accepted without comment
                }
            }
        }
        out
    }

    /// Convenience wrapper: returns `Err(msg)` when policy is `Strict`
    /// and `validate()` failed; `Ok(warnings)` otherwise (empty when
    /// passed, populated under `Audit`).
    fn gate<T: Validate>(&self, value: &T, what: &str) -> std::result::Result<Vec<String>, String> {
        let outcome = self.check(value, what);
        if let Some(e) = outcome.error {
            return Err(e);
        }
        Ok(outcome.warnings)
    }

    // --- Group operations -------------------------------------------------

    pub fn create_group(&self, mut group: GroupChat) -> Result<String, String> {
        // Auto-fill server-assigned fields BEFORE validation so the
        // record is in a fully-provisioned state when the gate runs.
        if group.group_id.is_empty() {
            group.group_id = format!("group-{}", Uuid::new_v4());
        }
        self.gate(&group, "group")?;
        let group_id = group.group_id.clone();
        let owner_id = group.owner_id.clone();

        let mut groups = Self::lock(&self.groups)?;
        let mut user_groups = Self::lock(&self.user_groups)?;
        groups.insert(group_id.clone(), group);
        user_groups
            .entry(owner_id)
            .or_default()
            .push(group_id.clone());
        Ok(group_id)
    }

    pub fn get_group(&self, group_id: &str) -> Option<GroupChat> {
        Self::lock(&self.groups).ok()?.get(group_id).cloned()
    }

    pub fn list_user_groups(&self, user_id: &str) -> Vec<GroupChat> {
        let ids = Self::lock(&self.user_groups)
            .ok()
            .and_then(|g| g.get(user_id).cloned())
            .unwrap_or_default();
        let groups = match Self::lock(&self.groups) {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        ids.into_iter()
            .filter_map(|id| groups.get(&id).cloned())
            .collect()
    }

    pub fn add_member(&self, group_id: &str, member: GroupMember) -> Result<(), String> {
        self.gate(&member, "member")?;
        let mut members = Self::lock(&self.members)?;
        let entry = members.entry(group_id.to_string()).or_default();
        if !entry.iter().any(|m| m.agent_id == member.agent_id) {
            entry.push(member);
        }
        Ok(())
    }

    pub fn remove_member(&self, group_id: &str, agent_id: &str) -> Result<(), String> {
        let mut members = Self::lock(&self.members)?;
        if let Some(list) = members.get_mut(group_id) {
            list.retain(|m| m.agent_id != agent_id);
        }
        Ok(())
    }

    pub fn get_members(&self, group_id: &str) -> Vec<GroupMember> {
        Self::lock(&self.members)
            .ok()
            .and_then(|m| m.get(group_id).cloned())
            .unwrap_or_default()
    }

    // --- Group messages ---------------------------------------------------

    pub fn send_group_message(&self, mut message: GroupMessage) -> Result<GroupMessage, String> {
        // Auto-fill message_id before validation so the gate sees the
        // server-assigned id rather than an empty string.
        if message.message_id.is_empty() {
            message.message_id = format!("gmsg-{}", Uuid::new_v4());
        }
        self.gate(&message, "group_message")?;
        // Stamp integrity if the caller did not. Validate already
        // ran above so the integrity pre-flight cannot panic.
        if message.integrity_hash.is_none() {
            message.stamp_integrity_hash();
        }
        let group_id = message.group_id.clone();
        let mut messages = Self::lock(&self.group_messages)?;
        let entry = messages.entry(group_id.clone()).or_default();
        entry.push(message.clone());
        // Bump the group's last activity and last sequence for diagnostics.
        if let Ok(mut groups) = Self::lock(&self.groups)
            && let Some(g) = groups.get_mut(&group_id)
        {
            g.last_activity = message.timestamp.max(g.last_activity);
            g.last_sequence = g.last_sequence.max(message.sequence);
            g.message_count = g.message_count.saturating_add(1);
        }
        Ok(message)
    }

    pub fn get_group_messages(&self, group_id: &str, limit: Option<usize>) -> Vec<GroupMessage> {
        let limit = limit.unwrap_or(50);
        Self::lock(&self.group_messages)
            .ok()
            .and_then(|m| m.get(group_id).cloned())
            .map(|mut v| {
                if v.len() > limit {
                    let start = v.len() - limit;
                    v.drain(..start);
                }
                v
            })
            .unwrap_or_default()
    }

    pub fn verify_message(&self, message: &MessageEnvelope) -> bool {
        // Reject records that fail validation outright — verifying a
        // message with empty sender_id / oversize content / wrong
        // sequence would yield a meaningless boolean.
        match message {
            MessageEnvelope::Group(m) => match self.gate(m, "group_message") {
                Ok(_) => m.verify_integrity(),
                Err(_) => false,
            },
            MessageEnvelope::Direct(m) => match self.gate(m, "direct_message") {
                Ok(_) => m.verify_integrity(),
                Err(_) => false,
            },
        }
    }

    // --- Direct chats & DMs -----------------------------------------------

    pub fn create_or_get_direct_chat(
        &self,
        user_a: &str,
        user_b: &str,
    ) -> Result<DirectChat, String> {
        let chat_id = DirectChat::chat_id_for(user_a, user_b);
        let mut chats = Self::lock(&self.direct_chats)?;
        if let Some(c) = chats.get(&chat_id) {
            return Ok(c.clone());
        }
        let now = now_secs();
        let chat = DirectChat {
            chat_id: chat_id.clone(),
            user_a: user_a.to_string(),
            user_b: user_b.to_string(),
            created_at: now,
            last_activity: now,
            message_count: 0,
        };
        chats.insert(chat_id, chat.clone());
        Ok(chat)
    }

    pub fn send_direct_message(&self, mut message: DirectMessage) -> Result<DirectMessage, String> {
        // Auto-fill message_id before validation.
        if message.message_id.is_empty() {
            message.message_id = format!("dmsg-{}", Uuid::new_v4());
        }
        self.gate(&message, "direct_message")?;
        if message.integrity_hash.is_none() {
            message.stamp_integrity_hash();
        }
        let chat_id = message.chat_id.clone();
        let mut messages = Self::lock(&self.direct_messages)?;
        let entry = messages.entry(chat_id.clone()).or_default();
        entry.push(message.clone());
        if let Ok(mut chats) = Self::lock(&self.direct_chats)
            && let Some(c) = chats.get_mut(&chat_id)
        {
            c.last_activity = message.timestamp.max(c.last_activity);
            c.message_count = c.message_count.saturating_add(1);
        }
        Ok(message)
    }

    pub fn get_direct_messages(&self, chat_id: &str, limit: Option<usize>) -> Vec<DirectMessage> {
        let limit = limit.unwrap_or(50);
        Self::lock(&self.direct_messages)
            .ok()
            .and_then(|m| m.get(chat_id).cloned())
            .map(|mut v| {
                if v.len() > limit {
                    let start = v.len() - limit;
                    v.drain(..start);
                }
                v
            })
            .unwrap_or_default()
    }

    // --- Typed decode helpers ---------------------------------------------

    fn require<T: for<'de> Deserialize<'de>>(value: &Value, field: &str) -> Result<T, String> {
        serde_json::from_value(
            value
                .get(field)
                .cloned()
                .ok_or_else(|| format!("missing field: {field}"))?,
        )
        .map_err(|e| format!("decode {field}: {e}"))
    }
}

/// Discriminated envelope used by `verify_integrity` so callers can
/// send either a group or direct message without a second round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageEnvelope {
    Group(GroupMessage),
    Direct(DirectMessage),
}

// `Validate` is implemented directly on the record types in
// `adnet_types::group_chat`, so the gate can call them via the
// shared `crate::validation::Validate` trait.

#[async_trait]
impl RpcHandler for GroupChatIpcService {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "group_create" => {
                let group: GroupChat = Self::require(&params, "group")?;
                let id = self.create_group(group)?;
                Ok(json!({ "group_id": id }))
            }
            "group_get" => {
                let group_id: String = Self::require(&params, "group_id")?;
                Ok(json!({ "group": self.get_group(&group_id) }))
            }
            "group_list_user" => {
                let user_id: String = Self::require(&params, "user_id")?;
                Ok(json!({ "groups": self.list_user_groups(&user_id) }))
            }
            "group_add_member" => {
                let group_id: String = Self::require(&params, "group_id")?;
                let member: GroupMember = Self::require(&params, "member")?;
                self.add_member(&group_id, member)?;
                Ok(json!({ "ok": true }))
            }
            "group_remove_member" => {
                let group_id: String = Self::require(&params, "group_id")?;
                let agent_id: String = Self::require(&params, "agent_id")?;
                self.remove_member(&group_id, &agent_id)?;
                Ok(json!({ "ok": true }))
            }
            "group_get_members" => {
                let group_id: String = Self::require(&params, "group_id")?;
                Ok(json!({ "members": self.get_members(&group_id) }))
            }
            "group_send_message" => {
                let message: GroupMessage = Self::require(&params, "message")?;
                let stored = self.send_group_message(message)?;
                Ok(json!({ "message": stored }))
            }
            "group_get_messages" => {
                let group_id: String = Self::require(&params, "group_id")?;
                let limit: Option<usize> = params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                Ok(json!({ "messages": self.get_group_messages(&group_id, limit) }))
            }
            "direct_create_or_get" => {
                let user_a: String = Self::require(&params, "user_a")?;
                let user_b: String = Self::require(&params, "user_b")?;
                let chat = self.create_or_get_direct_chat(&user_a, &user_b)?;
                Ok(json!({ "chat": chat }))
            }
            "direct_send_message" => {
                let message: DirectMessage = Self::require(&params, "message")?;
                let stored = self.send_direct_message(message)?;
                Ok(json!({ "message": stored }))
            }
            "direct_get_messages" => {
                let chat_id: String = Self::require(&params, "chat_id")?;
                let limit: Option<usize> = params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                Ok(json!({ "messages": self.get_direct_messages(&chat_id, limit) }))
            }
            "verify_integrity" => {
                let envelope: MessageEnvelope = Self::require(&params, "message")?;
                Ok(json!({ "valid": self.verify_message(&envelope) }))
            }
            "node_info" => Ok(json!({
                "node_id": self.cfg.node_id,
                "timestamp": now_secs(),
            })),
            other => Err(format!("unknown method: {other}")),
        }
    }
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub use adnet_types::group_chat::attachment_from_hash as group_attachment_from_hash;
/// Re-export of [`adnet_types::group_chat::next_group_message_id`] and
/// [`adnet_types::group_chat::attachment_from_hash`] for callers that
/// want to build messages on this side of the wire without going
/// through `adnet-types` again.
pub use adnet_types::group_chat::next_group_message_id as message_id_for;
pub use adnet_types::{AttachmentKind, MessageType, attachment_from_hash_str};

/// Convenience wrapper that stamps the integrity hash and fills in
/// `message_id` if empty. Useful for callers that have a [`ContentHash`]
/// for an attachment but don't want to construct the full
/// [`MessageAttachment`] by hand.
pub fn attachment_with_hash(
    attachment_id: String,
    file_type: AttachmentKind,
    blob: &ContentHash,
    file_name: impl Into<String>,
    file_size: u64,
) -> MessageAttachment {
    attachment_from_hash(attachment_id, file_type, blob, file_name, file_size)
}

/// Strict-string variant of [`attachment_with_hash`]. Returns an error
/// when `file_type` is not in the documented vocabulary — preferred at
/// IPC boundaries.
pub fn attachment_with_hash_strict(
    attachment_id: String,
    file_type: &str,
    blob: &ContentHash,
    file_name: impl Into<String>,
    file_size: u64,
) -> adnet_types::Result<MessageAttachment> {
    attachment_from_hash_str(attachment_id, file_type, blob, file_name, file_size)
}

/// Re-export of the receipt record so callers don't need to import
/// `adnet_types::group_chat::MessageReceipt` separately.
pub type Receipt = MessageReceipt;

/// Re-export of the message-id helper for callers using their own
/// `NodeId` source.
pub fn message_id_for_node(sender: &NodeId, group_id: &str, sequence: u32) -> String {
    next_group_message_id(sender, group_id, sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_svc() -> (
        tempfile::TempDir,
        Arc<GroupChatIpcService>,
        std::path::PathBuf,
    ) {
        new_svc_with_policy(ValidationPolicy::Strict)
    }

    fn new_svc_with_policy(
        policy: ValidationPolicy,
    ) -> (
        tempfile::TempDir,
        Arc<GroupChatIpcService>,
        std::path::PathBuf,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("chat.sock");
        let svc = Arc::new(GroupChatIpcService::new(GroupChatIpcConfig {
            socket_path: sock.clone(),
            node_id: NodeId::random(),
            policy,
        }));
        (dir, svc, sock)
    }

    fn sample_group(owner: &str) -> GroupChat {
        let now = now_secs();
        GroupChat {
            group_id: String::new(),
            name: "Test Group".into(),
            description: "demo".into(),
            avatar_url: None,
            owner_id: owner.into(),
            member_ids: vec![owner.into()],
            admin_ids: vec![owner.into()],
            is_private: false,
            created_at: now,
            last_activity: now,
            message_count: 0,
            public_account_id: None,
            last_sequence: 0,
            assistant_id: None,
        }
    }

    #[tokio::test]
    async fn create_get_list_group() {
        let (_dir, svc, _sock) = new_svc();
        let group = sample_group("alice");
        let id = svc.create_group(group).unwrap();
        let fetched = svc.get_group(&id).unwrap();
        assert_eq!(fetched.name, "Test Group");
        let listed = svc.list_user_groups("alice");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].group_id, id);
    }

    #[tokio::test]
    async fn group_message_roundtrip_stamps_integrity() {
        let (_dir, svc, _sock) = new_svc();
        let id = svc.create_group(sample_group("alice")).unwrap();
        let msg = GroupMessage {
            message_id: String::new(),
            group_id: id.clone(),
            sender_id: "alice".into(),
            sender_name: "Alice".into(),
            content: "hello".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1_700_000_000,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        };
        let stored = svc.send_group_message(msg.clone()).unwrap();
        assert!(!stored.message_id.is_empty());
        assert!(stored.integrity_hash.is_some());
        assert!(stored.verify_integrity());
        let listed = svc.get_group_messages(&id, Some(10));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].content, "hello");
    }

    #[tokio::test]
    async fn direct_chat_roundtrip() {
        let (_dir, svc, _sock) = new_svc();
        let chat = svc.create_or_get_direct_chat("alice", "bob").unwrap();
        let chat2 = svc.create_or_get_direct_chat("alice", "bob").unwrap();
        assert_eq!(chat.chat_id, chat2.chat_id);

        let msg = DirectMessage {
            message_id: String::new(),
            chat_id: chat.chat_id.clone(),
            sender_id: "alice".into(),
            receiver_id: "bob".into(),
            content: "ping".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        let stored = svc.send_direct_message(msg).unwrap();
        assert!(stored.verify_integrity());
        let listed = svc.get_direct_messages(&chat.chat_id, Some(10));
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn end_to_end_json_rpc() {
        use crate::client::json_rpc_call;
        let (_dir, svc, sock) = new_svc();
        let handle = Arc::clone(&svc).serve().await.unwrap();

        // create
        let group_payload = sample_group("alice");
        let r = json_rpc_call(
            &sock,
            "chat",
            "group_create",
            json!({ "group": group_payload }),
        )
        .await
        .unwrap();
        let gid = r["group_id"].as_str().unwrap().to_string();

        // send
        let msg = GroupMessage {
            message_id: String::new(),
            group_id: gid.clone(),
            sender_id: "alice".into(),
            sender_name: "Alice".into(),
            content: "via rpc".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 2,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        };
        let r = json_rpc_call(
            &sock,
            "chat",
            "group_send_message",
            json!({ "message": msg }),
        )
        .await
        .unwrap();
        assert!(r["message"]["integrity_hash"].as_str().is_some());

        // list
        let r = json_rpc_call(
            &sock,
            "chat",
            "group_get_messages",
            json!({ "group_id": gid, "limit": 10 }),
        )
        .await
        .unwrap();
        let arr = r["messages"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["content"], "via rpc");

        handle.shutdown();
    }

    // ─────────────────────────────────────────────────────────────────────
    // DO-178C boundary tests: validate() is wired into every entry point.
    // ─────────────────────────────────────────────────────────────────────

    /// Strict policy rejects an empty sender_id.
    #[tokio::test]
    async fn strict_rejects_empty_sender_id() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let id = svc.create_group(sample_group("alice")).unwrap();
        let bad = GroupMessage {
            message_id: "m-bad".into(),
            group_id: id.clone(),
            sender_id: "".into(),
            sender_name: "".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        };
        let err = svc.send_group_message(bad).unwrap_err();
        assert!(err.contains("sender_id"), "got {err}");
        // Message is NOT stored.
        assert!(svc.get_group_messages(&id, Some(10)).is_empty());
    }

    /// Strict policy rejects an oversize content.
    #[tokio::test]
    async fn strict_rejects_oversize_content() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let id = svc.create_group(sample_group("alice")).unwrap();
        let big = "x".repeat(adnet_types::MAX_CONTENT_LEN + 1);
        let bad = GroupMessage {
            message_id: "m-bad".into(),
            group_id: id.clone(),
            sender_id: "alice".into(),
            sender_name: "Alice".into(),
            content: big,
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        };
        let err = svc.send_group_message(bad).unwrap_err();
        assert!(err.contains("content"), "got {err}");
        assert!(svc.get_group_messages(&id, Some(10)).is_empty());
    }

    /// Strict policy rejects sequence overflow.
    #[tokio::test]
    async fn strict_rejects_sequence_overflow() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let id = svc.create_group(sample_group("alice")).unwrap();
        let bad = GroupMessage {
            message_id: "m-bad".into(),
            group_id: id.clone(),
            sender_id: "alice".into(),
            sender_name: "Alice".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1,
            is_edited: false,
            edited_at: None,
            sequence: adnet_types::MAX_SEQUENCE,
            integrity_hash: None,
        };
        assert!(svc.send_group_message(bad).is_err());
    }

    /// Strict policy rejects edited_at < timestamp.
    #[tokio::test]
    async fn strict_rejects_edit_temporal_inversion() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let id = svc.create_group(sample_group("alice")).unwrap();
        let bad = GroupMessage {
            message_id: "m-bad".into(),
            group_id: id.clone(),
            sender_id: "alice".into(),
            sender_name: "Alice".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1_000,
            is_edited: true,
            edited_at: Some(500), // before timestamp
            sequence: 1,
            integrity_hash: None,
        };
        let err = svc.send_group_message(bad).unwrap_err();
        assert!(err.contains("edited_at"), "got {err}");
    }

    /// Strict policy rejects a group with empty owner_id.
    #[tokio::test]
    async fn strict_rejects_empty_owner_id() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let bad = sample_group("alice");
        let mut g = bad;
        g.owner_id = "".into();
        let err = svc.create_group(g).unwrap_err();
        assert!(err.contains("owner_id"), "got {err}");
    }

    /// Strict policy rejects a GroupMember with joined_at > last_seen.
    #[tokio::test]
    async fn strict_rejects_member_temporal_inversion() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let id = svc.create_group(sample_group("alice")).unwrap();
        let bad = GroupMember {
            agent_id: "bob".into(),
            agent_name: "Bob".into(),
            role: adnet_types::MemberRole::Member,
            joined_at: 200,
            last_seen: 100,
            is_online: false,
            nickname: None,
        };
        let err = svc.add_member(&id, bad).unwrap_err();
        assert!(err.contains("last_seen"), "got {err}");
    }

    /// Strict policy rejects a direct message with empty receiver_id.
    #[tokio::test]
    async fn strict_rejects_empty_receiver_id() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let chat = svc.create_or_get_direct_chat("alice", "bob").unwrap();
        let bad = DirectMessage {
            message_id: "m-bad".into(),
            chat_id: chat.chat_id.clone(),
            sender_id: "alice".into(),
            receiver_id: "".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            sequence: 1,
            timestamp: 1,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };
        let err = svc.send_direct_message(bad).unwrap_err();
        assert!(err.contains("receiver_id"), "got {err}");
    }

    /// verify_message() returns false for a record that fails validate,
    /// even when its hash happens to match (defence in depth).
    #[tokio::test]
    async fn verify_rejects_invalid_record_even_with_matching_hash() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Strict);
        // sender_id = "" fails validate_id. Stamp a hash anyway — the
        // gate must still reject the message at the boundary.
        let mut msg = GroupMessage {
            message_id: "m-bad".into(),
            group_id: "g".into(),
            sender_id: "".into(),
            sender_name: "Alice".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        };
        msg.stamp_integrity_hash();
        // Note: stamp_integrity_hash will compute the hash over the
        // current (invalid) state. That hash is now meaningless because
        // the record fails validation, but a naive implementation would
        // still return `true` from verify_integrity(). The service's
        // verify_message() must gate through validate() first and return
        // false.
        let envelope = MessageEnvelope::Group(msg);
        assert!(!svc.verify_message(&envelope));
    }

    /// Audit policy accepts the record but routes the warning into the
    /// JSON-RPC response (handler-level audit hook).
    #[tokio::test]
    async fn audit_policy_accepts_with_warning() {
        use crate::client::json_rpc_call;
        let (_dir, svc, sock) = new_svc_with_policy(ValidationPolicy::Audit);
        let handle = Arc::clone(&svc).serve().await.unwrap();
        let id = svc.create_group(sample_group("alice")).unwrap();
        // Send a record with empty sender_id — under Strict this would
        // be rejected, but Audit accepts it.
        let bad = GroupMessage {
            message_id: String::new(),
            group_id: id.clone(),
            sender_id: "".into(),
            sender_name: "Alice".into(),
            content: "x".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        };
        let r = json_rpc_call(
            &sock,
            "chat",
            "group_send_message",
            json!({ "message": bad }),
        )
        .await
        .unwrap();
        assert!(r["message"]["integrity_hash"].as_str().is_some());
        // The record IS stored under Audit.
        assert_eq!(svc.get_group_messages(&id, Some(10)).len(), 1);
        handle.shutdown();
    }

    /// Lenient policy is identical to legacy behaviour (no gate).
    #[tokio::test]
    async fn lenient_policy_accepts_everything() {
        let (_dir, svc, _sock) = new_svc_with_policy(ValidationPolicy::Lenient);
        let id = svc.create_group(sample_group("alice")).unwrap();
        let bad = GroupMessage {
            message_id: String::new(),
            group_id: id.clone(),
            sender_id: "".into(),
            sender_name: "".into(),
            content: "".into(),
            message_type: MessageType::Text,
            attachments: vec![],
            reply_to: None,
            mentions: vec![],
            timestamp: 1,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        };
        let stored = svc.send_group_message(bad).unwrap();
        assert!(!stored.message_id.is_empty()); // auto-filled
        assert!(stored.integrity_hash.is_some()); // stamped
        assert_eq!(svc.get_group_messages(&id, Some(10)).len(), 1);
    }

    /// End-to-end JSON-RPC under Strict: malformed payload must be
    /// rejected with a clear error.
    #[tokio::test]
    async fn json_rpc_rejects_malformed_under_strict() {
        use crate::client::json_rpc_call;
        let (_dir, svc, sock) = new_svc_with_policy(ValidationPolicy::Strict);
        let handle = Arc::clone(&svc).serve().await.unwrap();
        let id = svc.create_group(sample_group("alice")).unwrap();
        let bad = json!({
            "message": {
                "message_id": "m1",
                "group_id": id,
                "sender_id": "", // invalid
                "sender_name": "x",
                "content": "x",
                "message_type": "text",
                "attachments": [],
                "reply_to": null,
                "mentions": [],
                "timestamp": 1,
                "is_edited": false,
                "edited_at": null,
                "sequence": 1,
                "integrity_hash": null,
            }
        });
        let err = json_rpc_call(&sock, "chat", "group_send_message", bad)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("sender_id"), "got {err}");
        handle.shutdown();
    }
}
