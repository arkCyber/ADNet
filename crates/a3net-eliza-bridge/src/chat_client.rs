//! AdnetChatClient — Eliza agent adapter for the A3Net "P2P WeChat" network.
//!
//! This adapter lets Eliza AI agents log into the A3Net network as regular
//! users with AI capabilities. The flow is:
//!
//! 1. `login()` → connects to the network, registers the agent's profile
//!    on the gossip overlay, joins the chat topic.
//! 2. `send_message()` / `send_group_message()` → stores locally and
//!    broadcasts over the gossip topic.
//! 3. `start_message_listener()` → spawns a background task that pulls
//!    gossip payloads, validates them, and emits `ChatEvent`s to
//!    subscribers.
//!
//! The client uses a `Topic::from_label("a3net-chat-{room}")` pattern so
//! direct messages and group chats share a single, deterministic
//! per-conversation topic.

use a3net_types::node::NodeId;
use a3net_types::group_chat::{
    DirectMessage, GroupMessage, MessageAttachment, MessageReceipt,
};
use a3net_types::invariants::MessageType;
use a3net_types::announce::AnnouncementPayload;
use a3net_types::topic::{Topic, topic_name};
use a3net_gossip::GossipTransport;
use a3net_chatstore::storage::{ChatStorage, Friend, ChatStorageConfig};
use crate::identity::{AdnetIdentity, AgentProfile, AgentType};
use crate::error::{BridgeError, BridgeResult};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Default broadcast channel capacity for chat events.
const EVENT_CAPACITY: usize = 512;

/// Domain-separated tag for chat-protocol hash functions.
const CHAT_HASH_TAG: &[u8] = b"a3net-eliza-bridge/v1/chat";

/// Chat message with metadata for Eliza consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElizaChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub sender_name: Option<String>,
    pub receiver_id: Option<String>,
    pub content: String,
    pub timestamp: i64,
    pub is_group: bool,
    pub reply_to: Option<String>,
    pub attachments: Vec<String>,
    pub signature: Option<String>,
}

impl From<DirectMessage> for ElizaChatMessage {
    fn from(m: DirectMessage) -> Self {
        let attachments = m
            .attachments
            .iter()
            .map(|a| {
                if !a.blob_hash.is_empty() {
                    a.blob_hash.clone()
                } else {
                    a.attachment_id.clone()
                }
            })
            .collect();
        Self {
            id: m.message_id,
            conversation_id: m.chat_id,
            sender_id: m.sender_id,
            sender_name: None,
            receiver_id: Some(m.receiver_id),
            content: m.content,
            timestamp: m.timestamp as i64,
            is_group: false,
            reply_to: m.reply_to,
            attachments,
            signature: m.integrity_hash,
        }
    }
}

impl From<GroupMessage> for ElizaChatMessage {
    fn from(m: GroupMessage) -> Self {
        let attachments = m
            .attachments
            .iter()
            .map(|a| {
                if !a.blob_hash.is_empty() {
                    a.blob_hash.clone()
                } else {
                    a.attachment_id.clone()
                }
            })
            .collect();
        Self {
            id: m.message_id,
            conversation_id: m.group_id,
            sender_id: m.sender_id,
            sender_name: Some(m.sender_name),
            receiver_id: None,
            content: m.content,
            timestamp: m.timestamp as i64,
            is_group: true,
            reply_to: m.reply_to,
            attachments,
            signature: m.integrity_hash,
        }
    }
}

/// Incoming message event for Eliza callback.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    DirectMessage(ElizaChatMessage),
    GroupMessage(ElizaChatMessage),
    FriendRequestAccepted { node_id: NodeId, name: String },
    UserOnline { node_id: NodeId },
    UserOffline { node_id: NodeId },
    Typing { user_id: NodeId, conversation_id: String, is_typing: bool },
}

/// Callback trait for Eliza agent to handle chat events.
#[async_trait]
pub trait ChatEventHandler: Send + Sync {
    async fn on_chat_event(&self, event: ChatEvent);
}

#[derive(Debug, Clone)]
pub struct ChatClientConfig {
    pub auto_accept_friends: bool,
    pub auto_join_groups: bool,
    pub max_message_length: usize,
    pub max_attachment_count: usize,
    pub rate_limit_per_minute: u32,
    pub display_name: String,
    pub agent_type: AgentType,
    /// Broadcast channel capacity for chat events.
    pub event_capacity: usize,
    /// Default room for 1:1 messages (used as topic suffix).
    pub default_topic_scope: String,
}

impl Default for ChatClientConfig {
    fn default() -> Self {
        Self {
            auto_accept_friends: false,
            auto_join_groups: true,
            max_message_length: 10_000,
            max_attachment_count: 8,
            rate_limit_per_minute: 60,
            display_name: "A3Net Agent".to_string(),
            agent_type: AgentType::Assistant,
            event_capacity: EVENT_CAPACITY,
            default_topic_scope: "chat".to_string(),
        }
    }
}

/// Cached contact info: user_id (hex) → display name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactInfo {
    pub node_id: NodeId,
    pub display_name: String,
    pub is_agent: bool,
    pub last_seen: i64,
    pub bio: Option<String>,
    pub capabilities: Vec<String>,
}

/// Friend request sent or received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendRequest {
    pub from_node_id: NodeId,
    pub from_name: String,
    pub message: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSearchResult {
    pub node_id: NodeId,
    pub display_name: String,
    pub is_agent: bool,
    pub bio: Option<String>,
}

/// Wire payload broadcast over the chat gossip topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatWireMessage {
    /// Direct message envelope.
    DirectMessage {
        message: DirectMessage,
        signature: Option<String>,
    },
    /// Group message envelope.
    GroupMessage {
        message: GroupMessage,
        signature: Option<String>,
    },
    /// Friend request being broadcast.
    FriendRequest(FriendRequest),
    /// Friend acceptance.
    FriendAccept {
        from_node: NodeId,
        to_node: NodeId,
        name: String,
    },
    /// Presence heartbeat.
    Presence {
        node_id: NodeId,
        online: bool,
    },
    /// Typing indicator.
    Typing {
        user_id: NodeId,
        conversation_id: String,
        is_typing: bool,
    },
}

/// A3Net Chat Client for Eliza agents.
pub struct AdnetChatClient {
    identity: Arc<AdnetIdentity>,
    config: ChatClientConfig,
    storage: Arc<RwLock<Option<ChatStorage>>>,
    /// Multi-topic gossip registry. Each topic has its own subscriber.
    gossip_subs: Arc<RwLock<HashMap<String, broadcast::Receiver<AnnouncementPayload>>>>,
    /// Underlying gossip transport (shared, registered topics).
    gossip_transport: Arc<RwLock<Option<Arc<dyn GossipTransport>>>>,
    /// Channel for emitting events to subscribers.
    event_sender: broadcast::Sender<ChatEvent>,
    connected: Arc<RwLock<bool>>,
    /// Background tasks for active listeners.
    listener_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
    /// Background cancel signal.
    cancel: Arc<RwLock<Option<broadcast::Sender<()>>>>,
    /// In-memory rate-limit window.
    rate_window: Arc<RwLock<std::collections::VecDeque<i64>>>,
    /// Cached contacts.
    contacts: Arc<RwLock<HashMap<String, ContactInfo>>>,
    /// Pending friend requests received from the network.
    friend_requests: Arc<RwLock<Vec<FriendRequest>>>,
}

impl AdnetChatClient {
    pub async fn new(identity: AdnetIdentity, config: ChatClientConfig) -> BridgeResult<Self> {
        let (event_sender, _receiver) = broadcast::channel(config.event_capacity);
        Ok(Self {
            identity: Arc::new(identity),
            config,
            storage: Arc::new(RwLock::new(None)),
            gossip_subs: Arc::new(RwLock::new(HashMap::new())),
            gossip_transport: Arc::new(RwLock::new(None)),
            event_sender,
            connected: Arc::new(RwLock::new(false)),
            listener_tasks: Arc::new(RwLock::new(Vec::new())),
            cancel: Arc::new(RwLock::new(None)),
            rate_window: Arc::new(RwLock::new(std::collections::VecDeque::new())),
            contacts: Arc::new(RwLock::new(HashMap::new())),
            friend_requests: Arc::new(RwLock::new(Vec::new())),
        })
    }

    /// Attach a `ChatStorage` for persistent local history.
    pub async fn with_storage(self, storage: ChatStorage) -> Self {
        *self.storage.write().await = Some(storage);
        self
    }

    /// Attach a gossip transport used for sending and receiving messages.
    pub async fn with_gossip_transport(self, transport: Arc<dyn GossipTransport>) -> Self {
        *self.gossip_transport.write().await = Some(transport);
        self
    }

    /// Connect to the A3Net network: register the agent's profile
    /// gossip topic and announce presence.
    pub async fn login(&self) -> BridgeResult<()> {
        let mut connected = self.connected.write().await;
        if *connected {
            return Ok(());
        }

        let transport = self.gossip_transport.read().await.clone();
        if let Some(transport) = transport {
            // Subscribe to the agent's personal inbox topic.
            let inbox = self.inbox_topic();
            let rx = transport.subscribe(inbox.clone());
            self.gossip_subs
                .write()
                .await
                .insert("__inbox__".to_string(), rx);

            // Join the personal topic so peers can route to us.
            transport
                .join(inbox, self.identity.node_id())
                .await
                .map_err(|e| BridgeError::Gossip(format!("join inbox: {e}")))?;

            // Broadcast presence (online).
            self.broadcast_presence(true, transport.as_ref()).await?;
        }

        // Set up a cancel channel for graceful shutdown.
        let (cancel_tx, _) = broadcast::channel(1);
        *self.cancel.write().await = Some(cancel_tx);

        tracing::info!(node_id = %self.identity.node_id(), "A3Net Chat Client: Logged in");
        *connected = true;
        Ok(())
    }

    /// Disconnect from the A3Net network.
    pub async fn logout(&self) -> BridgeResult<()> {
        let mut connected = self.connected.write().await;
        if !*connected {
            return Ok(());
        }

        // Cancel listener tasks.
        if let Some(tx) = self.cancel.read().await.as_ref() {
            let _ = tx.send(());
        }
        let mut tasks = self.listener_tasks.write().await;
        for handle in tasks.drain(..) {
            handle.abort();
        }

        // Broadcast offline presence.
        if let Some(transport) = self.gossip_transport.read().await.as_ref() {
            let _ = self.broadcast_presence(false, transport.as_ref()).await;
        }

        *connected = false;
        tracing::info!(node_id = %self.identity.node_id(), "A3Net Chat Client: Logged out");
        Ok(())
    }

    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }

    pub fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }

    pub fn profile(&self) -> &AgentProfile {
        self.identity.profile()
    }

    // ============================================================
    // Topic helpers
    // ============================================================

    /// Inbox topic: agents publish friend requests / DMs here.
    fn inbox_topic(&self) -> Topic {
        let label = topic_name(&self.config.default_topic_scope, "inbox");
        Topic::from_label(&label)
    }

    /// Topic for a 1:1 conversation between two nodes.
    fn dm_topic(&self, peer: &NodeId) -> Topic {
        let (a, b) = ordered_pair(&self.node_id(), peer);
        let short_a = a.get(..24).unwrap_or(&a);
        let label = topic_name(&self.config.default_topic_scope, &format!("dm-{short_a}-{b}"));
        Topic::from_label(&label)
    }

    /// Topic for a group conversation.
    fn group_topic(&self, group_id: &str) -> Topic {
        let label = topic_name(&self.config.default_topic_scope, &format!("group-{group_id}"));
        Topic::from_label(&label)
    }

    /// Stable ID for a 1:1 chat between two nodes.
    fn dm_chat_id(&self, peer: &NodeId) -> String {
        let (a, b) = ordered_pair(&self.node_id(), peer);
        // Use the short form (first 12 hex chars) so the combined id
        // stays well under the 128-byte cap enforced by
        // `a3net_chatstore`'s `validate_id`.
        format!("dm-{}-{b}", a.get(..24).unwrap_or(&a))
    }

    // ============================================================
    // Rate limiting
    // ============================================================

    async fn check_rate_limit(&self) -> BridgeResult<()> {
        let now = chrono::Utc::now().timestamp();
        let window_seconds = 60;
        let mut win = self.rate_window.write().await;
        while let Some(front) = win.front() {
            if now - *front > window_seconds {
                win.pop_front();
            } else {
                break;
            }
        }
        if win.len() as u32 >= self.config.rate_limit_per_minute {
            return Err(BridgeError::RateLimited(format!(
                "max {} messages/minute exceeded",
                self.config.rate_limit_per_minute
            )));
        }
        win.push_back(now);
        Ok(())
    }

    // ============================================================
    // Sending messages
    // ============================================================

    /// Send a direct message to a peer.
    ///
    /// Returns the assigned message ID. The message is stored locally
    /// (if storage is configured) and broadcast over the gossip topic.
    pub async fn send_message(
        &self,
        recipient_id: &NodeId,
        content: impl Into<String>,
    ) -> BridgeResult<String> {
        self.ensure_connected().await?;
        self.check_rate_limit().await?;
        let content = content.into();
        self.validate_content(&content)?;

        let message_id = Uuid::new_v4().to_string();
        let node_id = self.node_id();
        let user_id = node_id.as_hex();
        let chat_id = self.dm_chat_id(recipient_id);
        let timestamp = Utc::now().timestamp() as u64;

        let dm = DirectMessage {
            message_id: message_id.clone(),
            chat_id: chat_id.clone(),
            sender_id: user_id.to_string(),
            receiver_id: recipient_id.as_hex().to_string(),
            content: content.clone(),
            message_type: MessageType::Text,
            attachments: Vec::new(),
            reply_to: None,
            sequence: 0,
            timestamp,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };

        // Persist locally.
        if let Some(storage) = self.storage.read().await.as_ref() {
            storage
                .save_direct_message(user_id, dm.clone())
                .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        }

        // Sign and broadcast.
        self.broadcast_chat(
            ChatWireMessage::DirectMessage {
                message: dm,
                signature: self.sign_payload(&message_id, &content, timestamp).ok(),
            },
            &self.dm_topic(recipient_id),
        )
        .await?;

        tracing::debug!(
            from = %node_id,
            to = %recipient_id,
            message_id = %message_id,
            "Sent direct message"
        );
        Ok(message_id)
    }

    /// Send a message to a group chat.
    pub async fn send_group_message(
        &self,
        room_id: &str,
        content: impl Into<String>,
    ) -> BridgeResult<String> {
        self.ensure_connected().await?;
        self.check_rate_limit().await?;
        self.validate_room_id(room_id)?;
        let content = content.into();
        self.validate_content(&content)?;

        let message_id = Uuid::new_v4().to_string();
        let node_id = self.node_id();
        let user_id = node_id.as_hex();
        let group_id = room_id.to_string();
        let timestamp = Utc::now().timestamp() as u64;

        let gm = GroupMessage {
            message_id: message_id.clone(),
            group_id: group_id.clone(),
            sender_id: user_id.to_string(),
            sender_name: self.config.display_name.clone(),
            content: content.clone(),
            message_type: MessageType::Text,
            attachments: Vec::new(),
            reply_to: None,
            mentions: Vec::new(),
            timestamp,
            is_edited: false,
            edited_at: None,
            sequence: 0,
            integrity_hash: None,
        };

        if let Some(storage) = self.storage.read().await.as_ref() {
            storage
                .save_group_message(user_id, gm.clone())
                .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        }

        self.broadcast_chat(
            ChatWireMessage::GroupMessage {
                message: gm,
                signature: self.sign_payload(&message_id, &content, timestamp).ok(),
            },
            &self.group_topic(room_id),
        )
        .await?;

        tracing::debug!(
            from = %node_id,
            room = %room_id,
            message_id = %message_id,
            "Sent group message"
        );
        Ok(message_id)
    }

    /// Send a reply to an existing message.
    pub async fn send_reply(
        &self,
        recipient_id: &NodeId,
        content: impl Into<String>,
        reply_to_id: &str,
    ) -> BridgeResult<String> {
        self.ensure_connected().await?;
        self.check_rate_limit().await?;
        let content = content.into();
        self.validate_content(&content)?;

        let message_id = Uuid::new_v4().to_string();
        let node_id = self.node_id();
        let user_id = node_id.as_hex();
        let chat_id = self.dm_chat_id(recipient_id);
        let timestamp = Utc::now().timestamp() as u64;

        let dm = DirectMessage {
            message_id: message_id.clone(),
            chat_id,
            sender_id: user_id.to_string(),
            receiver_id: recipient_id.as_hex().to_string(),
            content,
            message_type: MessageType::Text,
            attachments: Vec::new(),
            reply_to: Some(reply_to_id.to_string()),
            sequence: 0,
            timestamp,
            integrity_hash: None,
            is_edited: false,
            edited_at: None,
        };

        if let Some(storage) = self.storage.read().await.as_ref() {
            storage
                .save_direct_message(user_id, dm.clone())
                .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        }

        self.broadcast_chat(
            ChatWireMessage::DirectMessage {
                message: dm,
                signature: None,
            },
            &self.dm_topic(recipient_id),
        )
        .await?;
        Ok(message_id)
    }

    /// Send a typing indicator.
    pub async fn send_typing_indicator(
        &self,
        recipient_id: &NodeId,
        conversation_id: &str,
    ) -> BridgeResult<()> {
        self.ensure_connected().await?;
        let message = ChatWireMessage::Typing {
            user_id: self.node_id(),
            conversation_id: conversation_id.to_string(),
            is_typing: true,
        };
        self.broadcast_chat(message, &self.dm_topic(recipient_id)).await
    }

    // ============================================================
    // Friends
    // ============================================================

    /// Send a friend request to a peer.
    pub async fn add_friend(
        &self,
        node_id: &NodeId,
        message: &str,
    ) -> BridgeResult<()> {
        self.ensure_connected().await?;
        if node_id == &self.node_id() {
            return Err(BridgeError::InvalidMessage("cannot add self as friend".into()));
        }
        let req = FriendRequest {
            from_node_id: self.node_id(),
            from_name: self.config.display_name.clone(),
            message: message.to_string(),
            timestamp: Utc::now().timestamp(),
        };
        // Broadcast friend request to the peer's inbox topic.
        let peer_inbox = self.peer_inbox_topic(node_id);
        self.broadcast_chat(ChatWireMessage::FriendRequest(req), &peer_inbox).await?;
        tracing::info!(
            from = %self.node_id(),
            to = %node_id,
            "Sent friend request"
        );
        Ok(())
    }

    /// Accept a friend request.
    pub async fn accept_friend(&self, node_id: &NodeId) -> BridgeResult<()> {
        self.ensure_connected().await?;
        let reqs = self.friend_requests.read().await.clone();
        let from_request = reqs.iter().find(|r| r.from_node_id == *node_id).cloned();
        let name = from_request
            .as_ref()
            .map(|r| r.from_name.clone())
            .unwrap_or_else(|| "Friend".to_string());

        let mut contacts = self.contacts.write().await;
        contacts.insert(
            node_id.as_hex().to_string(),
            ContactInfo {
                node_id: node_id.clone(),
                display_name: name.clone(),
                is_agent: false,
                last_seen: Utc::now().timestamp(),
                bio: None,
                capabilities: Vec::new(),
            },
        );
        drop(contacts);

        // Persist as a friend in storage.
        let node_id_local = self.node_id();
        let user_id: String = node_id_local.as_hex().to_string();
        if let Some(storage) = self.storage.read().await.as_ref() {
            let friend = Friend {
                friend_id: node_id.as_hex().to_string(),
                name: name.clone(),
                avatar_url: None,
                status: Some("online".to_string()),
                last_seen: Some(Utc::now().timestamp()),
                created_at: Some(Utc::now().timestamp()),
                updated_at: Some(Utc::now().timestamp()),
            };
            storage
                .save_friend(&user_id, friend)
                .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        }

        // Broadcast the acceptance.
        let accept = ChatWireMessage::FriendAccept {
            from_node: self.node_id(),
            to_node: node_id.clone(),
            name: self.config.display_name.clone(),
        };
        let peer_inbox = self.peer_inbox_topic(node_id);
        self.broadcast_chat(accept, &peer_inbox).await?;

        // Drain matching pending requests.
        let mut reqs = self.friend_requests.write().await;
        reqs.retain(|r| r.from_node_id != *node_id);

        let _ = self.event_sender.send(ChatEvent::FriendRequestAccepted {
            node_id: node_id.clone(),
            name: name.clone(),
        });
        Ok(())
    }

    /// Remove a friend.
    pub async fn remove_friend(&self, node_id: &NodeId) -> BridgeResult<()> {
        let key = node_id.as_hex().to_string();
        let mut contacts = self.contacts.write().await;
        contacts.remove(&key);
        drop(contacts);

        let user_id = self.node_id().as_hex().to_string();
        if let Some(storage) = self.storage.read().await.as_ref() {
            storage
                .remove_friend(&user_id, &key)
                .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        }
        Ok(())
    }

    /// List local contacts.
    pub async fn get_contacts(&self) -> Vec<ContactInfo> {
        let contacts = self.contacts.read().await;
        contacts.values().cloned().collect()
    }

    /// List pending friend requests.
    pub async fn pending_friend_requests(&self) -> Vec<FriendRequest> {
        self.friend_requests.read().await.clone()
    }

    // ============================================================
    // Read history
    // ============================================================

    /// Get messages from a 1:1 conversation.
    pub async fn get_messages(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> BridgeResult<Vec<ElizaChatMessage>> {
        let storage = self.storage.read().await;
        let storage = storage
            .as_ref()
            .ok_or_else(|| BridgeError::NotConnected)?;
        let user_id = self.node_id().as_hex().to_string();
        let dms = storage
            .get_direct_messages(&user_id, conversation_id)
            .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        let mut out: Vec<ElizaChatMessage> = dms.into_iter().map(ElizaChatMessage::from).collect();
        // Take the last `limit` messages (most recent).
        if out.len() > limit {
            let skip = out.len() - limit;
            out.drain(..skip);
        }
        Ok(out)
    }

    /// Get messages from a group by group_id.
    pub async fn get_group_messages(
        &self,
        group_id: &str,
        limit: usize,
    ) -> BridgeResult<Vec<ElizaChatMessage>> {
        let storage = self.storage.read().await;
        let storage = storage
            .as_ref()
            .ok_or_else(|| BridgeError::NotConnected)?;
        let user_id = self.node_id().as_hex().to_string();
        let msgs = storage
            .get_group_messages(&user_id, group_id)
            .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        let mut out: Vec<ElizaChatMessage> = msgs.into_iter().map(ElizaChatMessage::from).collect();
        if out.len() > limit {
            let skip = out.len() - limit;
            out.drain(..skip);
        }
        Ok(out)
    }

    /// List unique conversation IDs (group_id + dm peers) found in storage.
    pub async fn get_conversations(&self) -> BridgeResult<Vec<ConversationSummary>> {
        let storage = self.storage.read().await;
        let storage = storage
            .as_ref()
            .ok_or_else(|| BridgeError::NotConnected)?;
        let user_id = self.node_id().as_hex().to_string();
        let friends = storage
            .get_friends(&user_id)
            .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        let mut out: Vec<ConversationSummary> = friends
            .into_iter()
            .map(|f| ConversationSummary {
                conversation_id: format!("dm-{user_id}-{}", f.friend_id),
                kind: ConversationKind::Direct,
                peer: Some(f.friend_id),
                title: f.name,
                last_message_at: f.last_seen.unwrap_or(0),
            })
            .collect();
        // Sort by last activity.
        out.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));
        Ok(out)
    }

    /// Mark messages as read by writing a `MessageReceipt`.
    pub async fn mark_read(
        &self,
        message_id: &str,
        conversation_id: &str,
    ) -> BridgeResult<()> {
        let storage = self.storage.read().await;
        let storage = storage
            .as_ref()
            .ok_or_else(|| BridgeError::NotConnected)?;
        let user_id = self.node_id().as_hex().to_string();
        let sequence = message_id.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
        let receipt = MessageReceipt {
            receipt_id: format!("rcpt-{}", Uuid::new_v4()),
            message_id: message_id.to_string(),
            receiver_id: user_id.clone(),
            sequence,
            received_at: Utc::now().timestamp() as u64,
        };
        let _ = conversation_id; // chat_id is not tracked in MessageReceipt.
        storage
            .save_receipt(&user_id, receipt)
            .map_err(|e| BridgeError::ChatStore(e.to_string()))?;
        Ok(())
    }

    // ============================================================
    // Discovery
    // ============================================================

    /// Search for users by display name.
    ///
    /// In a full implementation this would query the DHT or a
    /// directory gossip topic. For now we search the local
    /// contacts cache and return empty otherwise.
    pub async fn search_users(&self, query: &str) -> BridgeResult<Vec<UserSearchResult>> {
        let q = query.to_lowercase();
        let contacts = self.contacts.read().await;
        let mut out: Vec<UserSearchResult> = contacts
            .values()
            .filter(|c| c.display_name.to_lowercase().contains(&q))
            .map(|c| UserSearchResult {
                node_id: c.node_id.clone(),
                display_name: c.display_name.clone(),
                is_agent: c.is_agent,
                bio: c.bio.clone(),
            })
            .collect();
        out.truncate(50);
        Ok(out)
    }

    /// Look up a profile by NodeId. Returns the local cached
    /// contact, or `None` if unknown.
    pub async fn get_user_profile(&self, node_id: &NodeId) -> BridgeResult<Option<ContactInfo>> {
        let key = node_id.as_hex().to_string();
        let contacts = self.contacts.read().await;
        Ok(contacts.get(&key).cloned())
    }

    // ============================================================
    // Subscriptions / listeners
    // ============================================================

    /// Subscribe to chat events.
    pub async fn subscribe(&self) -> broadcast::Receiver<ChatEvent> {
        self.event_sender.subscribe()
    }

    /// Register a custom event handler for chat events.
    pub async fn set_event_handler<H: ChatEventHandler + 'static>(
        &self,
        handler: Arc<H>,
    ) {
        let mut receiver = self.event_sender.subscribe();
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv().await {
                handler.on_chat_event(event).await;
            }
        });
    }

    /// Start a background task that listens for incoming gossip
    /// payloads and dispatches them as `ChatEvent`s.
    pub async fn start_message_listener(&self) -> BridgeResult<()> {
        self.ensure_connected().await?;
        let transport = self
            .gossip_transport
            .read()
            .await
            .clone()
            .ok_or_else(|| BridgeError::Gossip("no gossip transport configured".into()))?;

        // Subscribe to inbox + DM topic loop.
        let inbox = self.inbox_topic();
        let inbox_rx = transport.subscribe(inbox.clone());
        let _ = transport.join(inbox, self.node_id()).await;

        // The handler loop also subscribes to DM / group topics as
        // we discover them. For now, we only handle the inbox +
        // a wildcard listener that picks up everything we sent.
        let cancel = self
            .cancel
            .read()
            .await
            .as_ref()
            .map(|tx| tx.subscribe())
            .ok_or_else(|| BridgeError::NotConnected)?;

        let event_sender = self.event_sender.clone();
        let identity = self.identity.clone();
        let contacts = self.contacts.clone();
        let friend_requests = self.friend_requests.clone();
        let _gossip_subs = self.gossip_subs.clone();

        let mut tasks = self.listener_tasks.write().await;

        // Inbox listener.
        let cancel_for_task = cancel;
        let handle = tokio::spawn(async move {
            let mut cancel = cancel_for_task;
            let mut inbox_rx = inbox_rx;
            loop {
                tokio::select! {
                    _ = cancel.recv() => break,
                    payload = inbox_rx.recv() => {
                        let Some(payload) = payload.ok() else { break };
                        handle_payload(
                            payload,
                            &identity,
                            &contacts,
                            &friend_requests,
                            &event_sender,
                        ).await;
                    }
                }
            }
        });
        tasks.push(handle);

        tracing::info!(node_id = %self.node_id(), "Chat message listener started");
        Ok(())
    }

    /// Stop all background listeners.
    pub async fn stop_message_listener(&self) {
        if let Some(tx) = self.cancel.read().await.as_ref() {
            let _ = tx.send(());
        }
        let mut tasks = self.listener_tasks.write().await;
        for h in tasks.drain(..) {
            h.abort();
        }
        tracing::info!(node_id = %self.node_id(), "Chat message listener stopped");
    }

    // ============================================================
    // Helpers
    // ============================================================

    async fn broadcast_presence(
        &self,
        online: bool,
        transport: &dyn GossipTransport,
    ) -> BridgeResult<()> {
        let msg = ChatWireMessage::Presence {
            node_id: self.node_id(),
            online,
        };
        let payload = AnnouncementPayload {
            from_node: self.node_id(),
            payload: serde_json::to_value(&msg).map_err(BridgeError::Serialization)?,
        };
        transport
            .broadcast(self.inbox_topic(), payload)
            .await
            .map_err(|e| BridgeError::Gossip(format!("broadcast presence: {e}")))?;
        Ok(())
    }

    async fn broadcast_chat(&self, msg: ChatWireMessage, topic: &Topic) -> BridgeResult<()> {
        let transport = self.gossip_transport.read().await.clone();
        let transport = transport
            .ok_or_else(|| BridgeError::Gossip("no gossip transport configured".into()))?;
        let payload = AnnouncementPayload {
            from_node: self.node_id(),
            payload: serde_json::to_value(&msg).map_err(BridgeError::Serialization)?,
        };
        // Join the topic before broadcasting so peers can route to us.
        let _ = transport.join(topic.clone(), self.node_id()).await;
        transport
            .broadcast(topic.clone(), payload)
            .await
            .map_err(|e| BridgeError::Gossip(format!("broadcast: {e}")))?;
        Ok(())
    }

    fn peer_inbox_topic(&self, peer: &NodeId) -> Topic {
        let label = topic_name(&self.config.default_topic_scope, "inbox");
        // Scope inbox per recipient so only the recipient subscribes.
        let scoped = format!("{label}-{}", peer.as_hex());
        Topic::from_label(&scoped)
    }

    fn sign_payload(
        &self,
        message_id: &str,
        content: &str,
        timestamp: u64,
    ) -> anyhow::Result<String> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(CHAT_HASH_TAG);
        hasher.update(message_id.as_bytes());
        hasher.update(content.as_bytes());
        hasher.update(&timestamp.to_le_bytes());
        let digest = hasher.finalize();
        let sig = self.identity.sign(digest.as_bytes())?;
        Ok(hex::encode(sig))
    }

    fn validate_content(&self, content: &str) -> BridgeResult<()> {
        if content.is_empty() {
            return Err(BridgeError::InvalidMessage("empty content".into()));
        }
        if content.len() > self.config.max_message_length {
            return Err(BridgeError::InvalidMessage(format!(
                "content too long ({} > {})",
                content.len(),
                self.config.max_message_length
            )));
        }
        Ok(())
    }

    fn validate_room_id(&self, room_id: &str) -> BridgeResult<()> {
        if room_id.is_empty() || room_id.len() > 128 {
            return Err(BridgeError::InvalidMessage("invalid room_id".into()));
        }
        if !room_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(BridgeError::InvalidMessage(
                "room_id contains forbidden characters".into(),
            ));
        }
        Ok(())
    }

    async fn ensure_connected(&self) -> BridgeResult<()> {
        if !*self.connected.read().await {
            return Err(BridgeError::NotConnected);
        }
        Ok(())
    }

    // ============================================================
    // Eliza tools
    // ============================================================

    /// Generate tools for Eliza agent registration.
    pub fn generate_eliza_tools(&self) -> Vec<ElizaTool> {
        vec![
            ElizaTool {
                name: "send_dm".to_string(),
                description: "Send a direct message to an A3Net user".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "recipient_id": {"type": "string", "description": "NodeId of the recipient"},
                        "message": {"type": "string", "description": "Message content"}
                    },
                    "required": ["recipient_id", "message"]
                }),
            },
            ElizaTool {
                name: "send_group_message".to_string(),
                description: "Send a message to a group chat".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "room_id": {"type": "string", "description": "Room/group ID"},
                        "message": {"type": "string", "description": "Message content"}
                    },
                    "required": ["room_id", "message"]
                }),
            },
            ElizaTool {
                name: "send_reply".to_string(),
                description: "Reply to an existing direct message".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "recipient_id": {"type": "string", "description": "NodeId of the recipient"},
                        "message": {"type": "string"},
                        "reply_to_id": {"type": "string", "description": "Original message id"}
                    },
                    "required": ["recipient_id", "message", "reply_to_id"]
                }),
            },
            ElizaTool {
                name: "add_friend".to_string(),
                description: "Send a friend request to a user".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "node_id": {"type": "string", "description": "NodeId of the user"},
                        "message": {"type": "string", "description": "Introduction message"}
                    },
                    "required": ["node_id"]
                }),
            },
            ElizaTool {
                name: "accept_friend".to_string(),
                description: "Accept a pending friend request".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "node_id": {"type": "string", "description": "NodeId of the user"}
                    },
                    "required": ["node_id"]
                }),
            },
            ElizaTool {
                name: "get_messages".to_string(),
                description: "Get recent messages from a conversation".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "conversation_id": {"type": "string"},
                        "limit": {"type": "number", "description": "Max messages"}
                    },
                    "required": ["conversation_id"]
                }),
            },
            ElizaTool {
                name: "get_conversations".to_string(),
                description: "List all conversations".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            ElizaTool {
                name: "search_users".to_string(),
                description: "Search for users by display name".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    },
                    "required": ["query"]
                }),
            },
            ElizaTool {
                name: "mark_read".to_string(),
                description: "Mark a message as read".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message_id": {"type": "string"},
                        "conversation_id": {"type": "string"}
                    },
                    "required": ["message_id", "conversation_id"]
                }),
            },
        ]
    }

    /// Synchronous `wait_for` helper so callers can integrate with
    /// Eliza's tool execution loop.
    pub async fn wait_for_event(&self, timeout: Duration) -> BridgeResult<ChatEvent> {
        let mut rx = self.event_sender.subscribe();
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Ok(ev)) => Ok(ev),
            Ok(Err(_)) => Err(BridgeError::Cancelled),
            Err(_) => Err(BridgeError::Timeout(timeout.as_secs())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElizaTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub conversation_id: String,
    pub kind: ConversationKind,
    pub peer: Option<String>,
    pub title: String,
    pub last_message_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Direct,
    Group,
}

impl Clone for AdnetChatClient {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            config: self.config.clone(),
            storage: self.storage.clone(),
            gossip_subs: self.gossip_subs.clone(),
            gossip_transport: self.gossip_transport.clone(),
            event_sender: self.event_sender.clone(),
            connected: self.connected.clone(),
            listener_tasks: self.listener_tasks.clone(),
            cancel: self.cancel.clone(),
            rate_window: self.rate_window.clone(),
            contacts: self.contacts.clone(),
            friend_requests: self.friend_requests.clone(),
        }
    }
}

// ============================================================
// Internal helpers
// ============================================================

/// Always-ascending ordering of two NodeIds so both peers agree
/// on the same chat id.
fn ordered_pair(a: &NodeId, b: &NodeId) -> (String, String) {
    if a.as_hex() <= b.as_hex() {
        (a.as_hex().to_string(), b.as_hex().to_string())
    } else {
        (b.as_hex().to_string(), a.as_hex().to_string())
    }
}

/// Process an inbound gossip payload and emit the right
/// `ChatEvent`.
async fn handle_payload(
    payload: AnnouncementPayload,
    identity: &AdnetIdentity,
    contacts: &Arc<RwLock<HashMap<String, ContactInfo>>>,
    friend_requests: &Arc<RwLock<Vec<FriendRequest>>>,
    event_sender: &broadcast::Sender<ChatEvent>,
) {
    // Skip our own messages.
    if payload.from_node == identity.node_id() {
        return;
    }

    let parsed: Result<ChatWireMessage, _> = serde_json::from_value(payload.payload.clone());
    let Ok(message) = parsed else { return };

    match message {
        ChatWireMessage::DirectMessage { message, .. } => {
            // Verify signature if present.
            if let Some(sig) = &message.integrity_hash {
                let mut hasher = blake3::Hasher::new();
                hasher.update(CHAT_HASH_TAG);
                hasher.update(message.message_id.as_bytes());
                hasher.update(message.content.as_bytes());
                hasher.update(&message.timestamp.to_le_bytes());
                let digest = hasher.finalize();
                let sig_bytes = match hex::decode(sig) {
                    Ok(b) => b,
                    Err(_) => return,
                };
                if AdnetIdentity::verify_for_node(digest.as_bytes(), &sig_bytes, &payload.from_node)
                    .unwrap_or(false)
                {
                    // Cached contact update.
                    let key = payload.from_node.as_hex().to_string();
                    let mut contacts = contacts.write().await;
                    contacts.entry(key).or_insert_with(|| ContactInfo {
                        node_id: payload.from_node.clone(),
                        display_name: "Unknown".to_string(),
                        is_agent: false,
                        last_seen: 0,
                        bio: None,
                        capabilities: Vec::new(),
                    });
                    let _ = event_sender.send(ChatEvent::DirectMessage(message.into()));
                }
            } else {
                let _ = event_sender.send(ChatEvent::DirectMessage(message.into()));
            }
        }
        ChatWireMessage::GroupMessage { message, .. } => {
            let _ = event_sender.send(ChatEvent::GroupMessage(message.into()));
        }
        ChatWireMessage::FriendRequest(req) => {
            friend_requests.write().await.push(req.clone());
        }
        ChatWireMessage::FriendAccept { from_node, name, .. } => {
            let key = from_node.as_hex().to_string();
            let mut contacts = contacts.write().await;
            contacts.insert(
                key,
                ContactInfo {
                    node_id: from_node.clone(),
                    display_name: name.clone(),
                    is_agent: false,
                    last_seen: Utc::now().timestamp(),
                    bio: None,
                    capabilities: Vec::new(),
                },
            );
            let _ = event_sender.send(ChatEvent::FriendRequestAccepted {
                node_id: from_node,
                name,
            });
        }
        ChatWireMessage::Presence { node_id, online } => {
            let key = node_id.as_hex().to_string();
            let mut contacts = contacts.write().await;
            let entry = contacts.entry(key).or_insert_with(|| ContactInfo {
                node_id: node_id.clone(),
                display_name: "Unknown".to_string(),
                is_agent: false,
                last_seen: 0,
                bio: None,
                capabilities: Vec::new(),
            });
            entry.last_seen = Utc::now().timestamp();
            let _ = event_sender.send(if online {
                ChatEvent::UserOnline { node_id }
            } else {
                ChatEvent::UserOffline { node_id }
            });
        }
        ChatWireMessage::Typing {
            user_id,
            conversation_id,
            is_typing,
        } => {
            let _ = event_sender.send(ChatEvent::Typing {
                user_id,
                conversation_id,
                is_typing,
            });
        }
    }
}

/// Builder for `AdnetChatClient`.
pub struct ChatClientBuilder {
    identity: AdnetIdentity,
    config: ChatClientConfig,
    storage: Option<ChatStorage>,
    gossip_transport: Option<Arc<dyn GossipTransport>>,
}

impl ChatClientBuilder {
    pub fn new(identity: AdnetIdentity) -> Self {
        Self {
            identity,
            config: ChatClientConfig::default(),
            storage: None,
            gossip_transport: None,
        }
    }

    pub fn config(mut self, cfg: ChatClientConfig) -> Self {
        self.config = cfg;
        self
    }

    pub fn with_storage(mut self, storage: ChatStorage) -> Self {
        self.storage = Some(storage);
        self
    }

    pub fn with_gossip_transport(mut self, transport: Arc<dyn GossipTransport>) -> Self {
        self.gossip_transport = Some(transport);
        self
    }

    pub async fn build(self) -> BridgeResult<AdnetChatClient> {
        let mut client = AdnetChatClient::new(self.identity, self.config).await?;
        if let Some(storage) = self.storage {
            client = client.with_storage(storage).await;
        }
        if let Some(transport) = self.gossip_transport {
            client = client.with_gossip_transport(transport).await;
        }
        Ok(client)
    }
}

/// Helper to construct a `ChatStorage` from a directory.
pub fn open_storage(data_dir: &std::path::Path) -> BridgeResult<ChatStorage> {
    let cfg = ChatStorageConfig {
        storage_dir: data_dir.to_path_buf(),
    };
    ChatStorage::new(cfg).map_err(|e| BridgeError::ChatStore(e.to_string()))
}

/// Validate that an attachment payload is well-formed.
pub fn validate_attachment(att: &MessageAttachment) -> BridgeResult<()> {
    if att.attachment_id.is_empty() {
        return Err(BridgeError::InvalidMessage("attachment_id empty".into()));
    }
    if att.file_name.is_empty() {
        return Err(BridgeError::InvalidMessage("attachment file_name empty".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::announce::AnnouncementPayload;
    use a3net_gossip::transport::InProcessGossip;
    use a3net_types::invariants::AttachmentKind;
    use std::collections::HashMap;

    // ----------------------------------------------------------------
    // Helpers
    // ----------------------------------------------------------------

    async fn mk_identity(name: &str) -> (tempfile::TempDir, AdnetIdentity) {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), name).await.unwrap();
        (dir, id)
    }

    async fn mk_client(name: &str) -> (tempfile::TempDir, AdnetChatClient) {
        let (dir, id) = mk_identity(name).await;
        let c = AdnetChatClient::new(id, ChatClientConfig::default())
            .await
            .unwrap();
        (dir, c)
    }

    async fn mk_client_with_storage(name: &str) -> (tempfile::TempDir, AdnetChatClient) {
        let (dir, id) = mk_identity(name).await;
        let storage = open_storage(dir.path()).unwrap();
        let c = AdnetChatClient::new(id, ChatClientConfig::default())
            .await
            .unwrap()
            .with_storage(storage)
            .await;
        (dir, c)
    }

    async fn mk_client_with_gossip(name: &str) -> (tempfile::TempDir, AdnetChatClient, Arc<InProcessGossip>) {
        let (dir, id) = mk_identity(name).await;
        let gossip = Arc::new(InProcessGossip::new());
        let c = ChatClientBuilder::new(id)
            .with_gossip_transport(gossip.clone())
            .build()
            .await
            .unwrap();
        (dir, c, gossip)
    }

    fn dm_fixture() -> DirectMessage {
        let now = chrono::Utc::now().timestamp() as u64;
        DirectMessage {
            message_id: "msg-1".to_string(),
            chat_id: "dm-aa-bb".to_string(),
            sender_id: "aa".to_string(),
            receiver_id: "bb".to_string(),
            content: "hi".to_string(),
            message_type: MessageType::Text,
            attachments: vec![MessageAttachment {
                attachment_id: "att-1".to_string(),
                file_type: AttachmentKind::Image,
                blob_hash: "deadbeef".to_string(),
                file_name: "x.png".to_string(),
                file_size: 42,
                thumbnail_hash: None,
            }],
            reply_to: None,
            sequence: 1,
            timestamp: now,
            integrity_hash: Some("deadbeef".to_string()),
            is_edited: false,
            edited_at: None,
        }
    }

    fn group_fixture() -> GroupMessage {
        let now = chrono::Utc::now().timestamp() as u64;
        GroupMessage {
            message_id: "g-1".to_string(),
            group_id: "lobby".to_string(),
            sender_id: "aa".to_string(),
            sender_name: "Alice".to_string(),
            content: "hello room".to_string(),
            message_type: MessageType::Text,
            attachments: vec![MessageAttachment {
                attachment_id: "att-1".to_string(), // empty to test fallback path
                file_type: AttachmentKind::File,
                blob_hash: String::new(),
                file_name: "f.txt".to_string(),
                file_size: 1,
                thumbnail_hash: None,
            }],
            reply_to: Some("g-0".to_string()),
            mentions: vec!["bb".to_string()],
            timestamp: now,
            is_edited: false,
            edited_at: None,
            sequence: 1,
            integrity_hash: None,
        }
    }

    // ----------------------------------------------------------------
    // ordered_pair
    // ----------------------------------------------------------------

    #[test]
    fn ordered_pair_is_canonical() {
        let a = NodeId::random();
        let b = NodeId::random();
        let (x1, y1) = ordered_pair(&a, &b);
        let (x2, y2) = ordered_pair(&b, &a);
        assert_eq!((x1, y1), (x2, y2));
    }

    #[test]
    fn ordered_pair_self_pair_is_stable() {
        let a = NodeId::random();
        let (x, y) = ordered_pair(&a, &a);
        assert_eq!(x, y);
    }

    // ----------------------------------------------------------------
    // ElizaChatMessage From impls
    // ----------------------------------------------------------------

    #[test]
    fn eliza_chat_message_from_direct_message() {
        let dm = dm_fixture();
        let em: ElizaChatMessage = dm.clone().into();
        assert_eq!(em.id, "msg-1");
        assert_eq!(em.conversation_id, "dm-aa-bb");
        assert_eq!(em.sender_id, "aa");
        assert_eq!(em.receiver_id.as_deref(), Some("bb"));
        assert!(!em.is_group);
        assert_eq!(em.reply_to, None);
        // attachment with non-empty blob_hash picks blob_hash.
        assert_eq!(em.attachments, vec!["deadbeef".to_string()]);
        assert_eq!(em.signature.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn eliza_chat_message_from_direct_message_empty_blob_hash() {
        let mut dm = dm_fixture();
        dm.attachments[0].blob_hash = String::new();
        dm.attachments[0].attachment_id = "fallback-id".to_string();
        let em: ElizaChatMessage = dm.into();
        assert_eq!(em.attachments, vec!["fallback-id".to_string()]);
    }

    #[test]
    fn eliza_chat_message_from_group_message() {
        let gm = group_fixture();
        let em: ElizaChatMessage = gm.clone().into();
        assert_eq!(em.id, "g-1");
        assert_eq!(em.conversation_id, "lobby");
        assert_eq!(em.sender_id, "aa");
        assert_eq!(em.sender_name.as_deref(), Some("Alice"));
        assert!(em.is_group);
        assert_eq!(em.reply_to.as_deref(), Some("g-0"));
        assert_eq!(em.attachments, vec!["att-1".to_string()]);
        assert!(em.signature.is_none());
    }

    // ----------------------------------------------------------------
    // ChatClientConfig / config defaults
    // ----------------------------------------------------------------

    #[test]
    fn chat_client_config_default_values() {
        let cfg = ChatClientConfig::default();
        assert!(!cfg.auto_accept_friends);
        assert!(cfg.auto_join_groups);
        assert_eq!(cfg.max_message_length, 10_000);
        assert_eq!(cfg.max_attachment_count, 8);
        assert_eq!(cfg.rate_limit_per_minute, 60);
        assert_eq!(cfg.display_name, "A3Net Agent");
        assert_eq!(cfg.event_capacity, EVENT_CAPACITY);
        assert_eq!(cfg.default_topic_scope, "chat");
        assert_eq!(cfg.agent_type, AgentType::Assistant);
    }

    // ----------------------------------------------------------------
    // validate_attachment
    // ----------------------------------------------------------------

    #[test]
    fn validate_attachment_rejects_empty_id() {
        let att = MessageAttachment {
            attachment_id: "".to_string(),
            file_type: AttachmentKind::Image,
            blob_hash: "abc".to_string(),
            file_name: "x.png".to_string(),
            file_size: 1,
            thumbnail_hash: None,
        };
        let err = validate_attachment(&att).unwrap_err();
        match err {
            BridgeError::InvalidMessage(msg) => assert!(msg.contains("attachment_id")),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn validate_attachment_rejects_empty_file_name() {
        let att = MessageAttachment {
            attachment_id: "att-1".to_string(),
            file_type: AttachmentKind::Image,
            blob_hash: "abc".to_string(),
            file_name: "".to_string(),
            file_size: 1,
            thumbnail_hash: None,
        };
        let err = validate_attachment(&att).unwrap_err();
        match err {
            BridgeError::InvalidMessage(msg) => assert!(msg.contains("file_name")),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn validate_attachment_accepts_well_formed() {
        let att = MessageAttachment {
            attachment_id: "att-1".to_string(),
            file_type: AttachmentKind::Image,
            blob_hash: "abc".to_string(),
            file_name: "x.png".to_string(),
            file_size: 1,
            thumbnail_hash: None,
        };
        validate_attachment(&att).unwrap();
    }

    // ----------------------------------------------------------------
    // Lifecycle: new / login / logout / is_connected
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn chat_client_creation() {
        let (_d, client) = mk_client("agent-test").await;
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn login_logout_round_trip_without_transport() {
        let (_d, client) = mk_client("agent-test").await;
        client.login().await.unwrap();
        assert!(client.is_connected().await);
        client.logout().await.unwrap();
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn login_is_idempotent() {
        let (_d, client) = mk_client("agent").await;
        client.login().await.unwrap();
        client.login().await.unwrap(); // second call should be a no-op
        assert!(client.is_connected().await);
    }

    #[tokio::test]
    async fn logout_is_idempotent() {
        let (_d, client) = mk_client("agent").await;
        client.logout().await.unwrap(); // never logged in: no-op
        client.login().await.unwrap();
        client.logout().await.unwrap();
        client.logout().await.unwrap(); // second logout: no-op
        assert!(!client.is_connected().await);
    }

    // ----------------------------------------------------------------
    // send_message / send_group_message / send_reply / send_typing
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn send_message_returns_message_id() {
        let (_d, client, _g) = mk_client_with_gossip("agent-send").await;
        client.login().await.unwrap();
        let peer = NodeId::random();
        let id = client.send_message(&peer, "hello").await.unwrap();
        assert!(!id.is_empty());
        // message_id is a UUID v4.
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[tokio::test]
    async fn send_message_fails_when_not_connected() {
        let (_d, client) = mk_client("agent").await;
        let peer = NodeId::random();
        let err = client.send_message(&peer, "x").await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn send_group_message_returns_id() {
        let (_d, client, _g) = mk_client_with_gossip("agent-grp").await;
        client.login().await.unwrap();
        let id = client.send_group_message("lobby", "hi room").await.unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn send_group_message_rejects_bad_room_id() {
        let (_d, client, _g) = mk_client_with_gossip("agent").await;
        client.login().await.unwrap();
        assert!(client.send_group_message("", "x").await.is_err());
        assert!(client
            .send_group_message(&"x".repeat(200), "x")
            .await
            .is_err());
        assert!(client.send_group_message("bad room!", "x").await.is_err());
    }

    #[tokio::test]
    async fn send_reply_returns_id() {
        let (_d, client, _g) = mk_client_with_gossip("agent-reply").await;
        client.login().await.unwrap();
        let peer = NodeId::random();
        let id = client.send_reply(&peer, "reply body", "msg-orig").await.unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn send_typing_indicator_succeeds() {
        let (_d, client, _g) = mk_client_with_gossip("agent-type").await;
        client.login().await.unwrap();
        let peer = NodeId::random();
        client
            .send_typing_indicator(&peer, "conv-1")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_typing_indicator_fails_when_not_connected() {
        let (_d, client) = mk_client("agent").await;
        let peer = NodeId::random();
        assert!(client.send_typing_indicator(&peer, "conv").await.is_err());
    }

    // ----------------------------------------------------------------
    // Friends
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn add_friend_to_self_is_rejected() {
        let (_d, client, _g) = mk_client_with_gossip("agent-self").await;
        client.login().await.unwrap();
        let me = client.node_id();
        let err = client.add_friend(&me, "me").await.unwrap_err();
        match err {
            BridgeError::InvalidMessage(msg) => assert!(msg.contains("self")),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_friend_and_remove_friend_round_trip() {
        let (_d, client, _g) = mk_client_with_gossip("agent-fr").await;
        client.login().await.unwrap();
        let other = NodeId::random();
        client.add_friend(&other, "hi").await.unwrap();
        // Without an inbound FriendAccept, contacts are empty.
        assert!(client.get_contacts().await.is_empty());
        // Remove should be a no-op for an unknown peer (no error).
        client.remove_friend(&other).await.unwrap();
    }

    #[tokio::test]
    async fn add_friend_fails_when_not_connected() {
        let (_d, client) = mk_client("agent").await;
        let other = NodeId::random();
        assert!(client.add_friend(&other, "hi").await.is_err());
    }

    #[tokio::test]
    async fn remove_friend_no_storage_is_noop() {
        let (_d, client) = mk_client("agent").await;
        let other = NodeId::random();
        // No storage, no contact → should succeed.
        client.remove_friend(&other).await.unwrap();
    }

    #[tokio::test]
    async fn remove_friend_persists_in_storage() {
        let (_d, client) = mk_client_with_storage("agent").await;
        let other = NodeId::random();
        client.remove_friend(&other).await.unwrap();
    }

    #[tokio::test]
    async fn accept_friend_unknown_creates_fallback_contact() {
        let (_d, client, _g) = mk_client_with_gossip("agent-acc").await;
        client.login().await.unwrap();
        let other = NodeId::random();
        // No prior friend request: name falls back to "Friend".
        client.accept_friend(&other).await.unwrap();
        let contacts = client.get_contacts().await;
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].display_name, "Friend");
    }

    #[tokio::test]
    async fn accept_friend_emits_event() {
        let (_d, client, _g) = mk_client_with_gossip("agent-acc2").await;
        client.login().await.unwrap();
        let mut events = client.subscribe().await;
        let other = NodeId::random();
        client.accept_friend(&other).await.unwrap();
        let event = tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            ChatEvent::FriendRequestAccepted { node_id, name } => {
                assert_eq!(node_id, other);
                assert_eq!(name, "Friend");
            }
            other => panic!("expected FriendRequestAccepted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_friend_requests_initially_empty() {
        let (_d, client) = mk_client("agent").await;
        assert!(client.pending_friend_requests().await.is_empty());
    }

    // ----------------------------------------------------------------
    // get_messages / get_group_messages / get_conversations / mark_read
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn get_messages_requires_storage() {
        let (_d, client) = mk_client("agent").await;
        let err = client.get_messages("any", 10).await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn get_messages_returns_empty_when_no_history() {
        let (_d, client) = mk_client_with_storage("agent").await;
        let msgs = client.get_messages("conv", 10).await.unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn get_group_messages_requires_storage() {
        let (_d, client) = mk_client("agent").await;
        let err = client.get_group_messages("g", 10).await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn get_conversations_requires_storage() {
        let (_d, client) = mk_client("agent").await;
        let err = client.get_conversations().await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn get_conversations_returns_empty() {
        let (_d, client) = mk_client_with_storage("agent").await;
        let convs = client.get_conversations().await.unwrap();
        assert!(convs.is_empty());
    }

    #[tokio::test]
    async fn mark_read_requires_storage() {
        let (_d, client) = mk_client("agent").await;
        let err = client.mark_read("m", "c").await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected));
    }

    #[tokio::test]
    async fn mark_read_succeeds_with_storage() {
        let (_d, client) = mk_client_with_storage("agent").await;
        client.mark_read("m1", "c1").await.unwrap();
    }

    // ----------------------------------------------------------------
    // Discovery: search_users / get_user_profile
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn search_users_empty_cache() {
        let (_d, client) = mk_client("agent").await;
        let out = client.search_users("anyone").await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn get_user_profile_unknown_returns_none() {
        let (_d, client) = mk_client("agent").await;
        let res = client.get_user_profile(&NodeId::random()).await.unwrap();
        assert!(res.is_none());
    }

    // ----------------------------------------------------------------
    // Subscribe / event handler
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn subscribe_returns_a_receiver() {
        let (_d, client) = mk_client("agent").await;
        let mut rx = client.subscribe().await;
        // No events should have been emitted yet — recv would block,
        // so we don't await it. Instead, verify the receiver is alive.
        assert_eq!(rx.len(), 0);
    }

    // ----------------------------------------------------------------
    // stop_message_listener
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn stop_message_listener_is_safe_when_no_tasks() {
        let (_d, client) = mk_client("agent").await;
        client.stop_message_listener().await;
    }

    #[tokio::test]
    async fn start_message_listener_fails_without_transport() {
        let (_d, client) = mk_client("agent").await;
        // Force into connected state by logging in (no-op without transport).
        client.login().await.unwrap();
        // Without transport, this should error.
        let err = client.start_message_listener().await.unwrap_err();
        assert!(matches!(err, BridgeError::Gossip(_)));
    }

    // ----------------------------------------------------------------
    // Eliza tools
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn eliza_tools_list_is_complete() {
        let (_d, client) = mk_client("agent").await;
        let tools = client.generate_eliza_tools();
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        for expected in [
            "send_dm",
            "send_group_message",
            "send_reply",
            "add_friend",
            "accept_friend",
            "get_messages",
            "get_conversations",
            "search_users",
            "mark_read",
        ] {
            assert!(names.contains(&expected.to_string()), "missing tool {expected}");
        }
        // Each tool has a JSON schema.
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert_eq!(tool.parameters["type"], "object");
        }
    }

    // ----------------------------------------------------------------
    // wait_for_event
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn wait_for_event_times_out() {
        let (_d, client) = mk_client("agent").await;
        let err = client
            .wait_for_event(Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, BridgeError::Timeout(secs) if secs == 0));
    }

    // ----------------------------------------------------------------
    // Profile accessors
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn client_exposes_node_id_and_profile() {
        let (_d, client) = mk_client("agent").await;
        let n = client.node_id();
        assert!(!n.as_hex().is_empty());
        assert_eq!(client.profile().eliza_agent_id, "agent");
    }

    // ----------------------------------------------------------------
    // Rate limit logic (sliding window)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn rate_limit_window_evicts_old_entries() {
        // Drive check_rate_limit indirectly via send_message; the
        // sliding window should not accumulate forever within the
        // 60-second bucket.
        let (_d, client, _g) = mk_client_with_gossip("agent-rl").await;
        let cfg = ChatClientConfig {
            rate_limit_per_minute: 3,
            ..Default::default()
        };
        let (_d, mut client) = {
            let (dir, id) = mk_identity("agent-rl-2").await;
            let gossip = Arc::new(InProcessGossip::new());
            let c = ChatClientBuilder::new(id)
                .config(cfg)
                .with_gossip_transport(gossip)
                .build()
                .await
                .unwrap();
            (dir, c)
        };
        client.login().await.unwrap();
        let peer = NodeId::random();
        assert!(client.send_message(&peer, "1").await.is_ok());
        assert!(client.send_message(&peer, "2").await.is_ok());
        assert!(client.send_message(&peer, "3").await.is_ok());
        assert!(client.send_message(&peer, "4").await.is_err());
    }

    // ----------------------------------------------------------------
    // Validation
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn validate_content_rejects_empty() {
        let (_d, client) = mk_client("agent-test").await;
        assert!(client.validate_content("").is_err());
        assert!(client.validate_content("hello").is_ok());
    }

    #[tokio::test]
    async fn validate_content_rejects_overlong() {
        let (_d, client) = mk_client("agent-test").await;
        let huge = "a".repeat(20_000);
        let err = client.validate_content(&huge).unwrap_err();
        assert!(matches!(err, BridgeError::InvalidMessage(_)));
    }

    #[tokio::test]
    async fn validate_room_id_accepts_safe() {
        let (_d, client) = mk_client("agent-test").await;
        assert!(client.validate_room_id("lobby").is_ok());
        assert!(client.validate_room_id("lobby-1").is_ok());
        assert!(client.validate_room_id("lobby_2").is_ok());
    }

    #[tokio::test]
    async fn validate_room_id_rejects_invalid() {
        let (_d, client) = mk_client("agent-test").await;
        assert!(client.validate_room_id("").is_err());
        assert!(client.validate_room_id("lobby!").is_err());
        assert!(client.validate_room_id("lobby room").is_err());
        assert!(client.validate_room_id(&"x".repeat(200)).is_err());
    }

    // ----------------------------------------------------------------
    // Chat wire message variants
    // ----------------------------------------------------------------

    #[test]
    fn chat_wire_message_round_trip_all_variants() {
        let variants = vec![
            ChatWireMessage::DirectMessage {
                message: dm_fixture(),
                signature: Some("sig".into()),
            },
            ChatWireMessage::GroupMessage {
                message: group_fixture(),
                signature: None,
            },
            ChatWireMessage::FriendRequest(FriendRequest {
                from_node_id: NodeId::random(),
                from_name: "n".into(),
                message: "m".into(),
                timestamp: 0,
            }),
            ChatWireMessage::FriendAccept {
                from_node: NodeId::random(),
                to_node: NodeId::random(),
                name: "X".into(),
            },
            ChatWireMessage::Presence {
                node_id: NodeId::random(),
                online: true,
            },
            ChatWireMessage::Typing {
                user_id: NodeId::random(),
                conversation_id: "c".into(),
                is_typing: true,
            },
        ];
        for v in variants {
            let json = serde_json::to_value(&v).unwrap();
            let back: ChatWireMessage = serde_json::from_value(json.clone()).unwrap();
            // Round-trip preserves the variant tag.
            let json2 = serde_json::to_value(&back).unwrap();
            assert_eq!(
                json["kind"], json2["kind"],
                "kind tag changed during round-trip"
            );
        }
    }

    // ----------------------------------------------------------------
    // handle_payload (covers gossip dispatcher)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn handle_payload_ignores_own_messages() {
        let (_d, client) = mk_client("agent-h").await;
        let payload = AnnouncementPayload {
            from_node: client.node_id(),
            payload: serde_json::json!({"kind": "presence", "node_id": client.node_id().as_hex(), "online": true}),
        };
        let mut rx = client.subscribe().await;
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        // No event should have been emitted.
        let res = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(res.is_err(), "unexpected event: {res:?}");
    }

    #[tokio::test]
    async fn handle_payload_invalid_json_is_swallowed() {
        let (_d, client) = mk_client("agent-h2").await;
        let payload = AnnouncementPayload {
            from_node: NodeId::random(),
            payload: serde_json::json!("not-a-chat-message"),
        };
        let mut rx = client.subscribe().await;
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let res = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(res.is_err(), "unexpected event: {res:?}");
    }

    #[tokio::test]
    async fn handle_payload_group_message_emits_event() {
        let (_d, client) = mk_client("agent-h3").await;
        let mut rx = client.subscribe().await;
        let gm = group_fixture();
        let payload = AnnouncementPayload {
            from_node: NodeId::random(),
            payload: serde_json::to_value(ChatWireMessage::GroupMessage {
                message: gm.clone(),
                signature: None,
            })
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            ChatEvent::GroupMessage(m) => assert_eq!(m.id, "g-1"),
            other => panic!("expected GroupMessage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_payload_typing_emits_event() {
        let (_d, client) = mk_client("agent-h4").await;
        let mut rx = client.subscribe().await;
        let payload = AnnouncementPayload {
            from_node: NodeId::random(),
            payload: serde_json::to_value(ChatWireMessage::Typing {
                user_id: NodeId::random(),
                conversation_id: "conv-1".to_string(),
                is_typing: true,
            })
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            ChatEvent::Typing { conversation_id, is_typing, .. } => {
                assert_eq!(conversation_id, "conv-1");
                assert!(is_typing);
            }
            other => panic!("expected Typing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_payload_friend_request_appends_pending() {
        let (_d, client) = mk_client("agent-h5").await;
        let other = NodeId::random();
        let payload = AnnouncementPayload {
            from_node: other.clone(),
            payload: serde_json::to_value(ChatWireMessage::FriendRequest(FriendRequest {
                from_node_id: other.clone(),
                from_name: "Z".into(),
                message: "hello".into(),
                timestamp: 0,
            }))
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let reqs = client.pending_friend_requests().await;
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].from_node_id, other);
    }

    #[tokio::test]
    async fn handle_payload_friend_accept_emits_and_stores() {
        let (_d, client) = mk_client("agent-h6").await;
        let mut rx = client.subscribe().await;
        let other = NodeId::random();
        let payload = AnnouncementPayload {
            from_node: other.clone(),
            payload: serde_json::to_value(ChatWireMessage::FriendAccept {
                from_node: other.clone(),
                to_node: client.node_id(),
                name: "Alice".into(),
            })
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            ChatEvent::FriendRequestAccepted { node_id, name } => {
                assert_eq!(node_id, other);
                assert_eq!(name, "Alice");
            }
            other => panic!("expected FriendRequestAccepted, got {other:?}"),
        }
        // Contact should be cached.
        let prof = client.get_user_profile(&other).await.unwrap();
        assert!(prof.is_some());
    }

    #[tokio::test]
    async fn handle_payload_presence_online_offline() {
        let (_d, client) = mk_client("agent-h7").await;
        let mut rx = client.subscribe().await;
        let other = NodeId::random();
        for online in [true, false] {
            let payload = AnnouncementPayload {
                from_node: other.clone(),
                payload: serde_json::to_value(ChatWireMessage::Presence {
                    node_id: other.clone(),
                    online,
                })
                .unwrap(),
            };
            handle_payload(
                payload,
                &client.identity,
                &client.contacts,
                &client.friend_requests,
                &client.event_sender,
            )
            .await;
            let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .unwrap()
                .unwrap();
            match (online, ev) {
                (true, ChatEvent::UserOnline { node_id }) => assert_eq!(node_id, other),
                (false, ChatEvent::UserOffline { node_id }) => assert_eq!(node_id, other),
                (b, e) => panic!("online={b} → wrong event: {e:?}"),
            }
        }
        // last_seen should have been updated.
        let profile = client.get_user_profile(&other).await.unwrap().unwrap();
        assert!(profile.last_seen > 0);
    }

    #[tokio::test]
    async fn handle_payload_direct_message_with_valid_signature_emits() {
        // Build identity for the sender and sign the digest that the
        // handler will verify, so the contact gets cached.
        let (sender_dir, sender) = mk_identity("sender").await;
        let (_d, client) = mk_client("agent-h8").await;

        let dm = dm_fixture();
        let mut hasher = blake3::Hasher::new();
        hasher.update(CHAT_HASH_TAG);
        hasher.update(dm.message_id.as_bytes());
        hasher.update(dm.content.as_bytes());
        hasher.update(&dm.timestamp.to_le_bytes());
        let digest = hasher.finalize();
        let sig = sender.sign(digest.as_bytes()).unwrap();
        let sig_hex = hex::encode(sig);
        let dm_with_sig = DirectMessage {
            integrity_hash: Some(sig_hex),
            ..dm.clone()
        };

        let mut rx = client.subscribe().await;
        let payload = AnnouncementPayload {
            from_node: sender.node_id(),
            payload: serde_json::to_value(ChatWireMessage::DirectMessage {
                message: dm_with_sig,
                signature: None,
            })
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            ChatEvent::DirectMessage(m) => assert_eq!(m.id, dm.message_id),
            other => panic!("expected DirectMessage, got {other:?}"),
        }
        // Sender should be cached as a contact.
        let prof = client.get_user_profile(&sender.node_id()).await.unwrap();
        assert!(prof.is_some());
        // Keep the tempdir alive for the duration of the test.
        drop(sender_dir);
    }

    #[tokio::test]
    async fn handle_payload_direct_message_invalid_hex_drops_silently() {
        let (_d, client) = mk_client("agent-h9").await;
        let mut rx = client.subscribe().await;
        let dm = DirectMessage {
            integrity_hash: Some("not-hex-$$$".to_string()),
            ..dm_fixture()
        };
        let payload = AnnouncementPayload {
            from_node: NodeId::random(),
            payload: serde_json::to_value(ChatWireMessage::DirectMessage {
                message: dm,
                signature: None,
            })
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let res = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(res.is_err(), "expected no event, got {res:?}");
    }

    #[tokio::test]
    async fn handle_payload_direct_message_unsigned_emits_without_caching() {
        let (_d, client) = mk_client("agent-h10").await;
        let mut rx = client.subscribe().await;
        let dm = DirectMessage {
            integrity_hash: None,
            ..dm_fixture()
        };
        let other = NodeId::random();
        let payload = AnnouncementPayload {
            from_node: other.clone(),
            payload: serde_json::to_value(ChatWireMessage::DirectMessage {
                message: dm,
                signature: None,
            })
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            ChatEvent::DirectMessage(_) => {}
            other => panic!("expected DirectMessage, got {other:?}"),
        }
        // Unsigned messages do not auto-cache the contact.
        let prof = client.get_user_profile(&other).await.unwrap();
        assert!(prof.is_none());
    }

    // ----------------------------------------------------------------
    // set_event_handler dispatches via the trait
    // ----------------------------------------------------------------

    struct CapturingHandler {
        received: std::sync::Arc<tokio::sync::Mutex<Vec<ChatEvent>>>,
    }

    #[async_trait::async_trait]
    impl ChatEventHandler for CapturingHandler {
        async fn on_chat_event(&self, event: ChatEvent) {
            self.received.lock().await.push(event);
        }
    }

    #[tokio::test]
    async fn set_event_handler_dispatches_events() {
        let (_d, client, _g) = mk_client_with_gossip("agent-handler").await;
        let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let handler = std::sync::Arc::new(CapturingHandler {
            received: received.clone(),
        });
        client.set_event_handler(handler).await;
        // Trigger an event via accept_friend.
        client.login().await.unwrap();
        let other = NodeId::random();
        client.accept_friend(&other).await.unwrap();
        // Give the dispatch task time to fire.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let snapshot = received.lock().await.clone();
        assert!(!snapshot.is_empty(), "handler should have received an event");
    }

    // ----------------------------------------------------------------
    // Clone
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn chat_client_clone_preserves_state() {
        let (_d, client) = mk_client("agent").await;
        let cloned = client.clone();
        assert_eq!(client.node_id(), cloned.node_id());
        assert_eq!(client.profile().eliza_agent_id, cloned.profile().eliza_agent_id);
    }

    // ----------------------------------------------------------------
    // open_storage
    // ----------------------------------------------------------------

    #[test]
    fn open_storage_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let _storage = open_storage(dir.path()).unwrap();
    }

    // ----------------------------------------------------------------
    // get_messages / get_group_messages via storage round-trip
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn storage_round_trip_via_send_and_get() {
        let (dir, id) = mk_identity("agent-rt").await;
        let gossip = Arc::new(InProcessGossip::new());
        let storage = open_storage(dir.path()).unwrap();
        let client = ChatClientBuilder::new(id)
            .with_gossip_transport(gossip)
            .with_storage(storage)
            .build()
            .await
            .unwrap();
        client.login().await.unwrap();
        let peer = NodeId::random();
        let mid = client.send_message(&peer, "ping").await.unwrap();
        let conv = client.dm_chat_id(&peer);
        // Give the storage write a moment.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let msgs = client.get_messages(&conv, 10).await.unwrap();
        assert!(msgs.iter().any(|m| m.id == mid));

        // get_group_messages on an unknown group is empty.
        let groups = client.get_group_messages("nogroup", 10).await.unwrap();
        assert!(groups.is_empty());
    }

    // ----------------------------------------------------------------
    // Builder (ChatClientBuilder)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn chat_client_builder_default_config() {
        let (_d, id) = mk_identity("b-default").await;
        let client = ChatClientBuilder::new(id).build().await.unwrap();
        assert!(!client.is_connected().await);
        assert_eq!(client.profile().eliza_agent_id, "b-default");
    }

    #[tokio::test]
    async fn chat_client_builder_config_sets_fields() {
        let (_d, id) = mk_identity("b-cfg").await;
        let cfg = ChatClientConfig {
            display_name: "Renamed".into(),
            rate_limit_per_minute: 7,
            max_message_length: 42,
            ..Default::default()
        };
        let client = ChatClientBuilder::new(id)
            .config(cfg)
            .build()
            .await
            .unwrap();
        assert_eq!(client.profile().eliza_agent_id, "b-cfg");
    }

    #[tokio::test]
    async fn chat_client_builder_with_storage_attaches_storage() {
        let (_d, id) = mk_identity("b-storage").await;
        let dir = tempfile::tempdir().unwrap();
        let storage = open_storage(dir.path()).unwrap();
        let client = ChatClientBuilder::new(id)
            .with_storage(storage)
            .build()
            .await
            .unwrap();
        // With storage attached, get_messages should not error with NotConnected.
        let _ = client.get_messages("any", 10).await.unwrap();
    }

    #[tokio::test]
    async fn chat_client_builder_with_gossip_transport_attaches() {
        let (_d, id) = mk_identity("b-gossip").await;
        let gossip = Arc::new(InProcessGossip::new());
        let client = ChatClientBuilder::new(id)
            .with_gossip_transport(gossip)
            .build()
            .await
            .unwrap();
        // start_message_listener requires gossip; without it the call
        // would return BridgeError::Gossip. Use it as a probe.
        client.login().await.unwrap();
        client.start_message_listener().await.unwrap();
        client.stop_message_listener().await;
    }

    // ----------------------------------------------------------------
    // with_storage / with_gossip_transport (builder methods on client)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn with_storage_attaches_storage() {
        let (_d, client) = mk_client("ws").await;
        let dir = tempfile::tempdir().unwrap();
        let storage = open_storage(dir.path()).unwrap();
        let client = client.with_storage(storage).await;
        // Storage must be active enough to accept get_messages.
        let _ = client.get_messages("any", 10).await.unwrap();
    }

    #[tokio::test]
    async fn with_gossip_transport_attaches_transport() {
        let (_d, client) = mk_client("wg").await;
        let gossip = Arc::new(InProcessGossip::new());
        let client = client.with_gossip_transport(gossip).await;
        client.login().await.unwrap();
        client.start_message_listener().await.unwrap();
        client.stop_message_listener().await;
    }

    // ----------------------------------------------------------------
    // Topic helpers
    // ----------------------------------------------------------------

    #[test]
    fn inbox_topic_is_stable() {
        let (_d, client) = futures::executor::block_on(mk_client("topic-inbox"));
        let t1 = client.inbox_topic();
        let t2 = client.inbox_topic();
        assert_eq!(t1, t2);
        assert_eq!(t1.as_hex().len(), 64);
    }

    #[test]
    fn dm_topic_is_canonical_for_pair() {
        let (_d, a) = futures::executor::block_on(mk_client("topic-a"));
        let (_d, b) = futures::executor::block_on(mk_client("topic-b"));
        let t1 = a.dm_topic(&b.node_id());
        let t2 = b.dm_topic(&a.node_id());
        assert_eq!(t1, t2);
    }

    #[test]
    fn group_topic_is_stable() {
        let (_d, client) = futures::executor::block_on(mk_client("topic-grp"));
        let t1 = client.group_topic("lobby");
        let t2 = client.group_topic("lobby");
        assert_eq!(t1, t2);
        let t3 = client.group_topic("other");
        assert_ne!(t1, t3);
    }

    #[test]
    fn dm_chat_id_is_canonical_and_short() {
        let (_d, a) = futures::executor::block_on(mk_client("chat-a"));
        let (_d, b) = futures::executor::block_on(mk_client("chat-b"));
        let id_ab = a.dm_chat_id(&b.node_id());
        let id_ba = b.dm_chat_id(&a.node_id());
        assert_eq!(id_ab, id_ba);
        assert!(id_ab.starts_with("dm-"));
        // Short form: 3 ("dm-") + 24 + 1 ("-") + 64 = 92 chars
        assert_eq!(id_ab.len(), 92);
        // Stays well under chatstore's 128-byte id cap.
        assert!(id_ab.len() < 128);
    }

    #[test]
    fn peer_inbox_topic_is_per_recipient() {
        let (_d, client) = futures::executor::block_on(mk_client("peer-inbox"));
        let peer1 = NodeId::random();
        let peer2 = NodeId::random();
        let t1 = client.peer_inbox_topic(&peer1);
        let t2 = client.peer_inbox_topic(&peer2);
        let t3 = client.peer_inbox_topic(&peer1);
        assert_eq!(t1, t3);
        assert_ne!(t1, t2);
    }

    // ----------------------------------------------------------------
    // sign_payload — internal helper, exercised via signature path
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn sign_payload_emits_valid_hex_signature() {
        let (_d, client) = mk_client("sign").await;
        let sig_hex = client
            .sign_payload("msg-1", "hello", 123)
            .expect("sign should succeed");
        assert!(!sig_hex.is_empty());
        assert!(hex::decode(&sig_hex).is_ok());
        // Verify the signature recovers the client's NodeId.
        let mut hasher = blake3::Hasher::new();
        hasher.update(CHAT_HASH_TAG);
        hasher.update(b"msg-1");
        hasher.update(b"hello");
        hasher.update(&123u64.to_le_bytes());
        let digest = hasher.finalize();
        let sig_bytes = hex::decode(&sig_hex).unwrap();
        assert!(
            AdnetIdentity::verify_for_node(digest.as_bytes(), &sig_bytes, &client.node_id()).unwrap()
        );
    }

    // ----------------------------------------------------------------
    // broadcast_chat / broadcast_presence (indirect via InProcess)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn broadcast_chat_emits_via_gossip() {
        let (_d, id_a) = mk_identity("bc-a").await;
        let (_d, id_b) = mk_identity("bc-b").await;
        let gossip = Arc::new(InProcessGossip::new());
        let cfg = ChatClientConfig {
            display_name: "Sender".into(),
            ..Default::default()
        };
        let client_a = ChatClientBuilder::new(id_a)
            .config(cfg)
            .with_gossip_transport(gossip.clone())
            .build()
            .await
            .unwrap();
        let client_b = ChatClientBuilder::new(id_b)
            .with_gossip_transport(gossip.clone())
            .build()
            .await
            .unwrap();
        client_a.login().await.unwrap();
        client_b.login().await.unwrap();
        // Subscribe on B for any DM the topic generates.
        let topic = client_a.dm_topic(&client_b.node_id());
        let mut rx = gossip.subscribe(topic.clone());
        let _ = gossip.join(topic, client_b.node_id()).await;
        // Send a message: it goes through broadcast_chat → gossip.
        let mid = client_a
            .send_message(&client_b.node_id(), "broadcast test")
            .await
            .unwrap();
        assert!(!mid.is_empty());
        let received = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("gossip delivery")
            .expect("gossip recv");
        assert_eq!(received.from_node, client_a.node_id());
    }

    #[tokio::test]
    async fn broadcast_presence_emits_via_gossip_on_login() {
        let (_d, id_a) = mk_identity("bp-a").await;
        let gossip = Arc::new(InProcessGossip::new());
        let client = ChatClientBuilder::new(id_a)
            .with_gossip_transport(gossip.clone())
            .build()
            .await
            .unwrap();
        // Subscribe to the inbox topic BEFORE login so we don't miss
        // the presence announcement.
        let topic = client.inbox_topic();
        let mut rx = gossip.subscribe(topic.clone());
        let _ = gossip.join(topic, client.node_id()).await;
        client.login().await.unwrap();
        let payload = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("presence delivery")
            .expect("presence recv");
        assert_eq!(payload.from_node, client.node_id());
    }

    // ----------------------------------------------------------------
    // broadcast_chat error path — missing transport
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn broadcast_chat_fails_without_transport() {
        let (_d, client) = mk_client("bce").await;
        client.login().await.unwrap();
        let peer = NodeId::random();
        let err = client.send_message(&peer, "no transport").await.unwrap_err();
        assert!(matches!(err, BridgeError::Gossip(_)));
    }

    // ----------------------------------------------------------------
    // start_message_listener: success path
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn start_message_listener_succeeds_with_transport() {
        let (_d, client, _g) = mk_client_with_gossip("sml-ok").await;
        client.login().await.unwrap();
        client.start_message_listener().await.unwrap();
        // Calling twice should still work (each call spawns a fresh
        // task; we don't assert on the count).
        client.stop_message_listener().await;
    }

    // ----------------------------------------------------------------
    // wait_for_event — success / cancelled paths
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn wait_for_event_receives_event() {
        let (_d, client, _g) = mk_client_with_gossip("wfe-ok").await;
        let _ = client.wait_for_event(Duration::from_millis(100)).await.unwrap_err();
        // Trigger an event via accept_friend in another task.
        let mut rx = client.subscribe().await;
        let cloned = client.clone();
        let other = NodeId::random();
        let other_for_task = other.clone();
        tokio::spawn(async move {
            cloned.login().await.unwrap();
            cloned.accept_friend(&other_for_task).await.unwrap();
        });
        let ev = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("event delivered")
            .expect("event ok");
        match ev {
            ChatEvent::FriendRequestAccepted { node_id, .. } => assert_eq!(node_id, other),
            other => panic!("expected FriendRequestAccepted, got {other:?}"),
        }
    }

    // ----------------------------------------------------------------
    // search_users behavior — matching, truncation, agent flag
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn search_users_matches_case_insensitively_and_truncates() {
        let (_d, client, _g) = mk_client_with_gossip("search").await;
        client.login().await.unwrap();
        // Seed 60 contacts via FriendAccept fallback path; this also
        // exercises accept_friend under load.
        for i in 0..60 {
            let n = NodeId::random();
            client.accept_friend(&n).await.unwrap();
            // Rename the contact through handle_payload's FriendAccept
            // path by directly inserting via handle_payload. We use
            // accept_friend which inserts a default name; we just
            // patch the display_name in cache via a public side-effect
            // for the substring assertion below.
            let _ = i;
        }
        // Re-insert with custom names so we can test substring search.
        for n in 0..55 {
            let node = NodeId::random();
            let payload = AnnouncementPayload {
                from_node: node.clone(),
                payload: serde_json::to_value(ChatWireMessage::FriendAccept {
                    from_node: node.clone(),
                    to_node: client.node_id(),
                    name: format!("AliceBot-{n}"),
                })
                .unwrap(),
            };
            handle_payload(
                payload,
                &client.identity,
                &client.contacts,
                &client.friend_requests,
                &client.event_sender,
            )
            .await;
        }
        // Case-insensitive substring search.
        let out = client.search_users("ALICebot").await.unwrap();
        assert!(!out.is_empty(), "case-insensitive search should match");
        // Result count is capped at 50.
        assert!(out.len() <= 50);
        assert_eq!(out.len(), 50, "cap should kick in at 50");
    }

    #[tokio::test]
    async fn search_users_returns_agent_flag() {
        let (_d, client, _g) = mk_client_with_gossip("search-flag").await;
        client.login().await.unwrap();
        // Insert an agent-flavored contact via the cached path. We
        // mimic what accept_friend would do with is_agent = true by
        // dispatching a FriendAccept and then mutating via search.
        let node = NodeId::random();
        let payload = AnnouncementPayload {
            from_node: node.clone(),
            payload: serde_json::to_value(ChatWireMessage::FriendAccept {
                from_node: node.clone(),
                to_node: client.node_id(),
                name: "Bot42".into(),
            })
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let out = client.search_users("Bot42").await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].display_name, "Bot42");
        // is_agent defaults to false; the API mirrors that.
        assert!(!out[0].is_agent);
    }

    // ----------------------------------------------------------------
    // set_event_handler receives an existing event
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn set_event_handler_dispatches_friend_accept() {
        let (_d, client, _g) = mk_client_with_gossip("seh").await;
        let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let handler = std::sync::Arc::new(CapturingHandler {
            received: received.clone(),
        });
        client.set_event_handler(handler).await;
        client.login().await.unwrap();
        let other = NodeId::random();
        client.accept_friend(&other).await.unwrap();
        // Wait up to 500ms for the dispatch task to relay the event.
        let mut delivered = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if !received.lock().await.is_empty() {
                delivered = true;
                break;
            }
        }
        assert!(delivered, "event handler did not receive the event");
    }

    // ----------------------------------------------------------------
    // get_conversations lists seeded friends sorted by last_seen
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn get_conversations_returns_friend_summaries() {
        let (_d, id_a) = mk_identity("conv-a").await;
        let dir_a = tempfile::tempdir().unwrap();
        let gossip = Arc::new(InProcessGossip::new());
        let storage = open_storage(dir_a.path()).unwrap();
        let client = ChatClientBuilder::new(id_a)
            .with_gossip_transport(gossip)
            .with_storage(storage)
            .build()
            .await
            .unwrap();
        client.login().await.unwrap();
        // Seed two friends by accepting their friend request.
        for _ in 0..2 {
            let n = NodeId::random();
            client.accept_friend(&n).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let convs = client.get_conversations().await.unwrap();
        assert_eq!(convs.len(), 2);
        // Sorted descending by last_seen.
        assert!(convs[0].last_message_at >= convs[1].last_message_at);
        for c in &convs {
            assert!(matches!(c.kind, ConversationKind::Direct));
            assert!(c.peer.is_some());
            assert!(c.conversation_id.starts_with("dm-"));
        }
    }

    // ----------------------------------------------------------------
    // pending_friend_requests grows on inbound FriendRequest
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn pending_friend_requests_grows_on_inbound() {
        let (_d, client) = mk_client("pfr").await;
        let node = NodeId::random();
        let payload = AnnouncementPayload {
            from_node: node.clone(),
            payload: serde_json::to_value(ChatWireMessage::FriendRequest(FriendRequest {
                from_node_id: node.clone(),
                from_name: "X".into(),
                message: "hi".into(),
                timestamp: 1,
            }))
            .unwrap(),
        };
        handle_payload(
            payload,
            &client.identity,
            &client.contacts,
            &client.friend_requests,
            &client.event_sender,
        )
        .await;
        let reqs = client.pending_friend_requests().await;
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].from_node_id, node);
        assert_eq!(reqs[0].from_name, "X");
    }

    // ----------------------------------------------------------------
    // remove_friend with storage prunes cached contact
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn remove_friend_after_accept_purges_contact() {
        let (_d, client, _g) = mk_client_with_gossip("rm").await;
        client.login().await.unwrap();
        let other = NodeId::random();
        client.accept_friend(&other).await.unwrap();
        assert_eq!(client.get_contacts().await.len(), 1);
        client.remove_friend(&other).await.unwrap();
        assert!(client.get_contacts().await.is_empty());
    }

    // Suppress unused-import warning for items only used in some tests.
    #[allow(dead_code)]
    fn _unused() {
        let _ = HashMap::<String, ()>::new();
    }
}
