//! Pubsub operations for the CLI.
//!
//! Provides IPFS-compatible pubsub commands:
//! - `pubsub ls` - List subscribed topics
//! - `pubsub peers` - List peers subscribed to a topic
//! - `pubsub sub` - Subscribe to a topic
//! - `pubsub pub` - Publish to a message to a topic
//!
//! ## Gossip Integration
//!
//! When `--gossip` feature is enabled (default), these commands integrate
//! with the a3net-gossip subsystem for real pub/sub messaging. Without
//! gossip, operations are recorded locally but not broadcast.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;

/// Pubsub error types.
#[derive(Debug, Error)]
pub enum PubsubError {
    #[error("topic not found: {0}")]
    TopicNotFound(String),

    #[error("not subscribed: {0}")]
    NotSubscribed(String),

    #[error("already subscribed: {0}")]
    AlreadySubscribed(String),

    #[error("operation failed: {0}")]
    Operation(String),

    #[error("timeout waiting for message")]
    Timeout,

    #[error("gossip error: {0}")]
    GossipError(String),
}

/// Subscription record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub topic: String,
    pub peer_count: usize,
    pub message_count: usize,
}

/// Peer information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub connected: bool,
    pub topics: Vec<String>,
}

/// Pubsub message received from gossip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubsubMessage {
    pub topic: String,
    pub from: String,
    pub data: Vec<u8>,
    pub received_at: i64,
}

impl PubsubMessage {
    pub fn new(topic: String, from: String, data: Vec<u8>) -> Self {
        Self {
            topic,
            from,
            data,
            received_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }
}

/// Pubsub state stored at `<data_dir>/pubsub/state.json`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PubsubState {
    #[serde(default)]
    pub subscriptions: Vec<String>,
    #[serde(default)]
    pub peers: HashMap<String, PeerInfo>,
    #[serde(default)]
    pub message_counts: HashMap<String, u64>,
}

/// Gossip-backed pubsub manager that provides real pub/sub over the network.
#[cfg(feature = "gossip")]
pub struct GossipPubsubManager {
    state: PubsubState,
    data_dir: std::path::PathBuf,
    gossip_bus: Arc<a3net_gossip::GossipBus>,
    local_node_id: a3net_types::NodeId,
}

#[cfg(feature = "gossip")]
impl GossipPubsubManager {
    /// Create a new gossip-backed pubsub manager.
    pub fn new(
        data_dir: &std::path::Path,
        gossip_bus: Arc<a3net_gossip::GossipBus>,
        local_node_id: a3net_types::NodeId,
    ) -> Self {
        Self {
            state: PubsubState::default(),
            data_dir: data_dir.to_path_buf(),
            gossip_bus,
            local_node_id,
        }
    }

    /// Load pubsub state from disk.
    pub fn load(&mut self) -> Result<(), PubsubError> {
        let state_path = self.data_dir.join("pubsub").join("state.json");
        if state_path.exists() {
            let content = std::fs::read_to_string(&state_path)
                .map_err(|e| PubsubError::Operation(e.to_string()))?;
            self.state = serde_json::from_str(&content)
                .map_err(|e| PubsubError::Operation(e.to_string()))?;
        }
        Ok(())
    }

    /// Save pubsub state to disk.
    pub fn save(&self) -> Result<(), PubsubError> {
        let pubsub_dir = self.data_dir.join("pubsub");
        std::fs::create_dir_all(&pubsub_dir)
            .map_err(|e| PubsubError::Operation(e.to_string()))?;
        let state_path = pubsub_dir.join("state.json");
        let content = serde_json::to_string_pretty(&self.state)
            .map_err(|e| PubsubError::Operation(e.to_string()))?;
        std::fs::write(&state_path, content)
            .map_err(|e| PubsubError::Operation(e.to_string()))?;
        Ok(())
    }

    /// List all subscriptions.
    pub fn list_subscriptions(&self) -> Vec<Subscription> {
        self.state.subscriptions.iter()
            .map(|topic| Subscription {
                topic: topic.clone(),
                peer_count: self.state.peers.values()
                    .filter(|p| p.topics.contains(topic))
                    .count(),
                message_count: *self.state.message_counts.get(topic).unwrap_or(&0),
            })
            .collect()
    }

    /// List peers subscribed to a topic.
    pub fn list_peers(&self, topic: Option<&str>) -> Vec<PeerInfo> {
        match topic {
            Some(t) => self.state.peers.values()
                .filter(|p| p.topics.contains(&t.to_string()))
                .cloned()
                .collect(),
            None => self.state.peers.values().cloned().collect(),
        }
    }

    /// Subscribe to a topic and join the gossip room.
    pub async fn subscribe(&mut self, topic: &str) -> Result<broadcast::Receiver<a3net_types::Announcement>, PubsubError> {
        // Convert topic to room ID for gossip
        let room_id = a3net_types::RoomId::from(topic.to_lowercase());

        // Join the gossip room
        self.gossip_bus.join_room(&room_id).await
            .map_err(|e| PubsubError::GossipError(e.to_string()))?;

        // Subscribe to receive messages
        let receiver = self.gossip_bus.subscribe(&room_id);

        // Record subscription locally
        if !self.state.subscriptions.contains(&topic.to_string()) {
            self.state.subscriptions.push(topic.to_string());
            self.save()?;
        }

        Ok(receiver)
    }

    /// Unsubscribe from a topic and leave the gossip room.
    pub async fn unsubscribe(&mut self, topic: &str) -> Result<(), PubsubError> {
        let room_id = a3net_types::RoomId::from(topic.to_lowercase());

        // Leave the gossip room
        self.gossip_bus.leave_room(&room_id).await
            .map_err(|e| PubsubError::GossipError(e.to_string()))?;

        // Remove from local state
        self.state.subscriptions.retain(|t| t != topic);
        self.save()?;

        Ok(())
    }

    /// Publish a message to a topic.
    pub async fn publish(&mut self, topic: &str, data: Vec<u8>) -> Result<(), PubsubError> {
        let room_id = a3net_types::RoomId::from(topic.to_lowercase());

        // Create announcement with the message data
        let payload = a3net_types::AnnouncementPayload::Message {
            content: data.clone(),
            mime_type: Some("application/octet-stream".to_string()),
        };

        let announcement = a3net_types::Announcement {
            id: None,
            sender: self.local_node_id.clone(),
            timestamp: a3net_types::Timestamp::now(),
            payload,
            expires_at: None,
        };

        // Publish via gossip
        self.gossip_bus.publish(&room_id, &announcement).await
            .map_err(|e| PubsubError::GossipError(e.to_string()))?;

        // Record locally
        *self.state.message_counts.entry(topic.to_string()).or_insert(0) += 1;
        self.save()?;

        Ok(())
    }

    /// Get message count for a topic.
    pub fn message_count(&self, topic: &str) -> u64 {
        *self.state.message_counts.get(topic).unwrap_or(&0)
    }
}

/// Pubsub manager for handling pubsub operations (non-gossip mode).
pub struct PubsubManager {
    state: PubsubState,
    data_dir: std::path::PathBuf,
}

impl PubsubManager {
    /// Create a new pubsub manager.
    pub fn new(data_dir: &std::path::Path) -> Self {
        Self {
            state: PubsubState::default(),
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// Load pubsub state from disk.
    pub fn load(&mut self) -> Result<(), PubsubError> {
        let state_path = self.data_dir.join("pubsub").join("state.json");
        if state_path.exists() {
            let content = std::fs::read_to_string(&state_path)
                .map_err(|e| PubsubError::Operation(e.to_string()))?;
            self.state = serde_json::from_str(&content)
                .map_err(|e| PubsubError::Operation(e.to_string()))?;
        }
        Ok(())
    }

    /// Save pubsub state to disk.
    pub fn save(&self) -> Result<(), PubsubError> {
        let pubsub_dir = self.data_dir.join("pubsub");
        std::fs::create_dir_all(&pubsub_dir)
            .map_err(|e| PubsubError::Operation(e.to_string()))?;
        let state_path = pubsub_dir.join("state.json");
        let content = serde_json::to_string_pretty(&self.state)
            .map_err(|e| PubsubError::Operation(e.to_string()))?;
        std::fs::write(&state_path, content)
            .map_err(|e| PubsubError::Operation(e.to_string()))?;
        Ok(())
    }

    /// List all subscriptions.
    pub fn list_subscriptions(&self) -> Vec<Subscription> {
        self.state.subscriptions.iter()
            .map(|topic| Subscription {
                topic: topic.clone(),
                peer_count: self.state.peers.values()
                    .filter(|p| p.topics.contains(topic))
                    .count(),
                message_count: 0,
            })
            .collect()
    }

    /// List peers subscribed to a topic.
    pub fn list_peers(&self, topic: Option<&str>) -> Vec<PeerInfo> {
        match topic {
            Some(t) => self.state.peers.values()
                .filter(|p| p.topics.contains(&t.to_string()))
                .cloned()
                .collect(),
            None => self.state.peers.values().cloned().collect(),
        }
    }

    /// Subscribe to a topic.
    pub fn subscribe(&mut self, topic: &str) -> Result<(), PubsubError> {
        if self.state.subscriptions.contains(&topic.to_string()) {
            return Err(PubsubError::AlreadySubscribed(topic.to_string()));
        }
        self.state.subscriptions.push(topic.to_string());
        self.save()?;
        Ok(())
    }

    /// Unsubscribe from a topic.
    pub fn unsubscribe(&mut self, topic: &str) -> Result<(), PubsubError> {
        if !self.state.subscriptions.contains(&topic.to_string()) {
            return Err(PubsubError::NotSubscribed(topic.to_string()));
        }
        self.state.subscriptions.retain(|t| t != topic);
        self.save()?;
        Ok(())
    }

    /// Record a peer.
    pub fn record_peer(&mut self, peer_id: &str, topics: Vec<String>) {
        self.state.peers.insert(peer_id.to_string(), PeerInfo {
            peer_id: peer_id.to_string(),
            connected: true,
            topics,
        });
        let _ = self.save();
    }

    /// Record a received message for a topic.
    pub fn record_message(&mut self, topic: &str) {
        *self.state.message_counts.entry(topic.to_string()).or_insert(0) += 1;
        let _ = self.save();
    }

    /// Get message count for a topic.
    pub fn message_count(&self, topic: &str) -> u64 {
        *self.state.message_counts.get(topic).unwrap_or(&0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Gossip Integration (for IPC/Named Pipe connections)
// ─────────────────────────────────────────────────────────────────────────────

/// Run pubsub with gossip integration via IPC.
///
/// This function is called when the CLI is connected to a running node
/// through IPC. It provides real pub/sub messaging over the network.
#[cfg(feature = "gossip")]
pub async fn run_pubsub_with_gossip(
    sub: &PubsubCmd,
    data_dir: &std::path::Path,
    gossip_bus: Arc<a3net_gossip::GossipBus>,
    local_node_id: a3net_types::NodeId,
) -> anyhow::Result<()> {
    let mut manager = GossipPubsubManager::new(data_dir, gossip_bus, local_node_id);
    manager.load()?;

    match sub {
        PubsubCmd::Ls { json } => {
            let subs = manager.list_subscriptions();
            if *json {
                println!("{}", serde_json::to_string_pretty(&subs)?);
            } else {
                if subs.is_empty() {
                    println!("(no subscriptions)");
                } else {
                    println!("Subscribed topics:");
                    for s in &subs {
                        let msg_count = manager.message_count(&s.topic);
                        println!("  {} ({} peers, {} messages)", s.topic, s.peer_count, msg_count);
                    }
                }
            }
        }

        PubsubCmd::Peers { topic, json } => {
            let peers = manager.list_peers(topic.as_deref());
            if *json {
                println!("{}", serde_json::to_string_pretty(&peers)?);
            } else {
                if peers.is_empty() {
                    println!("(no peers)");
                } else {
                    let topic_hint = topic.as_ref().map(|t| format!(" on {}", t)).unwrap_or_default();
                    println!("Peers{}:", topic_hint);
                    for p in &peers {
                        let connected = if p.connected { "✓" } else { "✗" };
                        println!("  {} {} ({})", connected, p.peer_id, p.topics.join(", "));
                    }
                }
            }
        }

        PubsubCmd::Sub { topic, discover: _, timeout } => {
            println!("Subscribing to: {}", topic);
            println!("Press Ctrl+C to unsubscribe");

            let mut receiver = manager.subscribe(topic).await?;

            // Set up timeout if specified
            let timeout_secs = *timeout as u64;
            if timeout_secs > 0 {
                println!("(subscription active for {} seconds)", timeout_secs);
                let timeout_duration = Duration::from_secs(timeout_secs);
                let deadline = tokio::time::Instant::now() + timeout_duration;

                loop {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        println!("Subscription timed out.");
                        break;
                    }

                    match tokio::time::timeout(remaining, receiver.recv()).await {
                        Ok(Ok(ann)) => {
                            if let a3net_types::AnnouncementPayload::Message { content, .. } = ann.payload {
                                println!("[{}] {}: {}",
                                    ann.sender,
                                    topic,
                                    String::from_utf8_lossy(&content)
                                );
                            }
                        }
                        Ok(Err(_)) => {
                            println!("Subscription ended.");
                            break;
                        }
                        Err(_) => {
                            println!("Subscription timed out.");
                            break;
                        }
                    }
                }
            } else {
                println!("(subscription active indefinitely)");
                // Block forever, printing received messages
                while let Ok(Some(ann)) = receiver.recv().await.recv().await {
                    if let a3net_types::AnnouncementPayload::Message { content, .. } = ann.payload {
                        println!("[{}] {}: {}",
                            ann.sender,
                            topic,
                            String::from_utf8_lossy(&content)
                        );
                    }
                }
            }

            manager.unsubscribe(topic).await?;
        }

        PubsubCmd::Pub { topic, message, json } => {
            let data = message.as_bytes().to_vec();
            manager.publish(topic, data).await?;

            if *json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "published": true,
                    "topic": topic,
                    "message": message,
                    "bytes": message.len()
                }))?);
            } else {
                println!("Published to {}: {}", topic, message);
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// CLI command handlers (offline mode)
// ─────────────────────────────────────────────────────────────────────────────

use crate::cli::PubsubCmd;

/// Run pubsub command in offline mode (no network connection).
///
/// When gossip is not available, this provides local-only pubsub tracking
/// that can be synced later when a network connection is established.
pub fn run_pubsub(sub: &PubsubCmd, data_dir: &std::path::Path) -> anyhow::Result<()> {
    let mut pubsub = PubsubManager::new(data_dir);
    pubsub.load()?;

    match sub {
        PubsubCmd::Ls { json } => {
            let subs = pubsub.list_subscriptions();
            if *json {
                println!("{}", serde_json::to_string_pretty(&subs)?);
            } else {
                if subs.is_empty() {
                    println!("(no subscriptions)");
                } else {
                    println!("Subscribed topics:");
                    for s in &subs {
                        let msg_count = pubsub.message_count(&s.topic);
                        println!("  {} ({} peers, {} messages)", s.topic, s.peer_count, msg_count);
                    }
                }
            }
        }

        PubsubCmd::Peers { topic, json } => {
            let peers = pubsub.list_peers(topic.as_deref());
            if *json {
                println!("{}", serde_json::to_string_pretty(&peers)?);
            } else {
                if peers.is_empty() {
                    println!("(no peers)");
                } else {
                    let topic_hint = topic.as_ref().map(|t| format!(" on {}", t)).unwrap_or_default();
                    println!("Peers{}:", topic_hint);
                    for p in &peers {
                        let connected = if p.connected { "✓" } else { "✗" };
                        println!("  {} {} ({})", connected, p.peer_id, p.topics.join(", "));
                    }
                }
            }
        }

        PubsubCmd::Sub { topic, discover, timeout } => {
            println!("Subscribing to: {}", topic);

            if *discover {
                println!("(discovery mode enabled)");
            }

            println!("Press Ctrl+C to unsubscribe");
            println!("(subscription active for {} seconds, 0 = indefinite)", timeout);

            // Record the subscription locally
            if let Err(e) = pubsub.subscribe(topic) {
                eprintln!("Warning: could not save subscription: {}", e);
            }

            #[cfg(feature = "gossip")]
            {
                println!();
                println!("Note: For network pubsub, use `a3net serve` with gossip enabled,");
                println!("      then use `a3net channel subscribe <topic>` for full integration.");
            }

            #[cfg(not(feature = "gossip"))]
            {
                eprintln!("Note: pubsub sub requires a running node with gossip enabled");
            }
        }

        PubsubCmd::Pub { topic, message, json } => {
            // Record the publish locally
            pubsub.record_message(topic);

            if *json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "published": true,
                    "topic": topic,
                    "message": message,
                    "bytes": message.len()
                }))?);
            } else {
                println!("Published to {}: {}", topic, message);
            }

            #[cfg(feature = "gossip")]
            {
                println!("Note: For network broadcast, use `a3net serve` with gossip enabled,");
                println!("      then use `a3net channel post --channel {} --message '...'`.", topic);
            }

            #[cfg(not(feature = "gossip"))]
            {
                eprintln!("Note: pubsub pub requires a running node with gossip enabled");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubsub_subscribe_unsubscribe() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut pubsub = PubsubManager::new(temp_dir.path());
        pubsub.load().unwrap();

        pubsub.subscribe("test-topic").unwrap();
        let subs = pubsub.list_subscriptions();
        assert!(subs.iter().any(|s| s.topic == "test-topic"));

        pubsub.unsubscribe("test-topic").unwrap();
        let subs = pubsub.list_subscriptions();
        assert!(!subs.iter().any(|s| s.topic == "test-topic"));
    }

    #[test]
    fn pubsub_list_peers() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut pubsub = PubsubManager::new(temp_dir.path());
        pubsub.load().unwrap();

        pubsub.record_peer("peer1", vec!["topic1".to_string()]);
        pubsub.record_peer("peer2", vec!["topic1".to_string(), "topic2".to_string()]);

        let peers = pubsub.list_peers(Some("topic1"));
        assert_eq!(peers.len(), 2);

        let peers = pubsub.list_peers(Some("topic2"));
        assert_eq!(peers.len(), 1);
    }

    #[test]
    fn pubsub_message_counting() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut pubsub = PubsubManager::new(temp_dir.path());
        pubsub.load().unwrap();

        pubsub.subscribe("test-topic").unwrap();
        assert_eq!(pubsub.message_count("test-topic"), 0);

        pubsub.record_message("test-topic");
        pubsub.record_message("test-topic");
        pubsub.record_message("test-topic");

        assert_eq!(pubsub.message_count("test-topic"), 3);
        assert_eq!(pubsub.message_count("other-topic"), 0);
    }

    #[test]
    fn pubsub_message_struct() {
        let msg = PubsubMessage::new(
            "test-topic".into(),
            "peer123".into(),
            b"hello world".to_vec(),
        );
        assert_eq!(msg.topic, "test-topic");
        assert_eq!(msg.from, "peer123");
        assert_eq!(msg.data, b"hello world");
        assert!(msg.received_at > 0);
    }

    #[test]
    fn pubsub_already_subscribed_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut pubsub = PubsubManager::new(temp_dir.path());
        pubsub.load().unwrap();

        pubsub.subscribe("test-topic").unwrap();
        let result = pubsub.subscribe("test-topic");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PubsubError::AlreadySubscribed(_)));
    }

    #[test]
    fn pubsub_not_subscribed_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut pubsub = PubsubManager::new(temp_dir.path());
        pubsub.load().unwrap();

        let result = pubsub.unsubscribe("nonexistent");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PubsubError::NotSubscribed(_)));
    }

    #[test]
    fn pubsub_persistence() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut pubsub = PubsubManager::new(temp_dir.path());
        pubsub.load().unwrap();

        // Add some data
        pubsub.subscribe("persistent-topic").unwrap();
        pubsub.record_peer("persistent-peer", vec!["persistent-topic".to_string()]);
        pubsub.record_message("persistent-topic");

        // Create a new manager and verify persistence
        let mut pubsub2 = PubsubManager::new(temp_dir.path());
        pubsub2.load().unwrap();

        let subs = pubsub2.list_subscriptions();
        assert!(subs.iter().any(|s| s.topic == "persistent-topic"));
    }
}

/// Tests for GossipPubsubManager (requires gossip feature)
#[cfg(feature = "gossip")]
mod gossip_tests {
    use super::*;

    #[tokio::test]
    async fn gossip_pubsub_manager_creation() {
        // This test verifies that GossipPubsubManager can be created
        // Actual network tests would require a full node setup
        use a3net_types::NodeId;
        use a3net_gossip::{GossipBus, InProcessGossip};

        let temp_dir = tempfile::tempdir().unwrap();
        let local_id = NodeId::from(a3net_crypto::PublicKey::random());

        // Create a simple in-process transport
        let transport = Arc::new(InProcessGossip::new());
        let gossip_bus = Arc::new(GossipBus::new(local_id.clone(), transport));

        let manager = GossipPubsubManager::new(temp_dir.path(), gossip_bus, local_id);
        assert!(manager.list_subscriptions().is_empty());
    }
}
