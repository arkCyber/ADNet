//! [`GossipBus`] — typed, room-aware facade over a [`GossipTransport`].

use std::sync::Arc;
use std::time::Instant;

use adnet_types::{Announcement, AnnouncementPayload, NodeId, RoomId, Topic, topic_name};
use tokio::sync::broadcast;

use crate::bridge;
use crate::dedup::{DedupeFilter, TtlTracker};
use crate::persistence::{MessagePersistence, StoredMessage};
use crate::transport::GossipTransport;

/// Default broadcast channel capacity.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

/// High-level facade. Owns the local node id and a transport, and exposes
/// pub/sub helpers keyed by [`RoomId`].
#[derive(Debug, Clone)]
pub struct GossipBus {
    local_node: NodeId,
    transport: Arc<dyn GossipTransport>,
}

impl GossipBus {
    pub fn new(local_node: NodeId, transport: Arc<dyn GossipTransport>) -> Self {
        Self {
            local_node,
            transport,
        }
    }

    pub fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    pub fn transport(&self) -> &Arc<dyn GossipTransport> {
        &self.transport
    }

    /// Resolve the canonical topic id for a room.
    pub fn topic_for(&self, room: &RoomId) -> Topic {
        Topic::from_label(&topic_name("room", room.as_str()))
    }

    /// Subscribe to a room's topic — returns a broadcast receiver yielding
    /// incoming [`Announcement`]s.
    pub fn subscribe(&self, room: &RoomId) -> broadcast::Receiver<Announcement> {
        let topic = self.topic_for(room);
        let raw_rx = self.transport.subscribe(topic);
        Self::decode_stream(raw_rx)
    }

    /// Subscribe to a room's topic, returning a receiver that filters duplicates
    /// and expired messages based on the provided deduplication filter and TTL tracker.
    pub fn subscribe_with_filter(
        &self,
        room: &RoomId,
        dedup_filter: Arc<parking_lot::RwLock<crate::dedup::DedupeFilter>>,
        ttl_tracker: Arc<parking_lot::RwLock<crate::dedup::TtlTracker>>,
    ) -> broadcast::Receiver<Announcement> {
        let topic = self.topic_for(room);
        let raw_rx = self.transport.subscribe(topic);
        Self::decode_stream_with_filter(raw_rx, dedup_filter, ttl_tracker)
    }

    /// Publish an announcement into a room topic. The local node is
    /// attributed as the sender.
    pub async fn publish(&self, room: &RoomId, ann: &Announcement) -> anyhow::Result<()> {
        let topic = self.topic_for(room);
        let payload = bridge::wrap(ann, &self.local_node);
        self.transport.broadcast(topic, payload).await
    }

    /// Convenience: subscribe to a room topic on the transport (best-effort).
    pub async fn join_room(&self, room: &RoomId) -> anyhow::Result<()> {
        let topic = self.topic_for(room);
        self.transport.join(topic, self.local_node.clone()).await
    }

    /// Convenience: leave a room topic on the transport (best-effort).
    pub async fn leave_room(&self, room: &RoomId) -> anyhow::Result<()> {
        let topic = self.topic_for(room);
        self.transport.leave(topic).await
    }

    /// Subscribe to a room with message persistence enabled.
    ///
    /// Messages will be stored using the provided persistence layer before
    /// being sent to subscribers.
    pub fn subscribe_with_persistence(
        &self,
        room: &RoomId,
        persistence: Arc<MessagePersistence>,
        dedup_filter: Arc<parking_lot::RwLock<DedupeFilter>>,
        ttl_tracker: Arc<parking_lot::RwLock<TtlTracker>>,
    ) -> broadcast::Receiver<Announcement> {
        let topic = self.topic_for(room);
        let raw_rx = self.transport.subscribe(topic);
        Self::decode_stream_with_persistence(raw_rx, persistence, dedup_filter, ttl_tracker)
    }

    /// Subscribe with persistence and no deduplication.
    pub fn subscribe_with_persistence_no_dedup(
        &self,
        room: &RoomId,
        persistence: Arc<MessagePersistence>,
    ) -> broadcast::Receiver<Announcement> {
        let topic = self.topic_for(room);
        let raw_rx = self.transport.subscribe(topic);
        Self::decode_stream_with_persistence_only(raw_rx, persistence)
    }

    /// Store a message to persistence layer.
    pub async fn persist_message(
        &self,
        persistence: &MessagePersistence,
        room: &RoomId,
        mut ann: Announcement,
    ) -> anyhow::Result<()> {
        let message_id = ann.get_or_generate_message_id();
        let content = serde_json::to_vec(&ann)?;

        let stored = StoredMessage {
            message_id,
            room_id: room.to_string(),
            content,
            received_at: Instant::now(),
            expires_at: ann.effective_expires_at(),
            sequence: 0,
        };

        persistence.store_message(room.as_str(), stored).await
    }

    fn decode_stream(
        mut raw_rx: broadcast::Receiver<AnnouncementPayload>,
    ) -> broadcast::Receiver<Announcement> {
        // Wrap raw payloads into a decoded broadcast channel.
        let (tx, rx) = broadcast::channel::<Announcement>(DEFAULT_CHANNEL_CAPACITY);
        tokio::spawn(async move {
            loop {
                match raw_rx.recv().await {
                    Ok(payload) => {
                        if let Some(ann) = bridge::unwrap(&payload) {
                            // If no subscribers, the send errors — that's fine.
                            let _ = tx.send(ann);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        rx
    }

    fn decode_stream_with_filter(
        mut raw_rx: broadcast::Receiver<AnnouncementPayload>,
        dedup_filter: Arc<parking_lot::RwLock<crate::dedup::DedupeFilter>>,
        ttl_tracker: Arc<parking_lot::RwLock<crate::dedup::TtlTracker>>,
    ) -> broadcast::Receiver<Announcement> {
        // Wrap raw payloads into a decoded broadcast channel with deduplication.
        let (tx, rx) = broadcast::channel::<Announcement>(DEFAULT_CHANNEL_CAPACITY);
        tokio::spawn(async move {
            loop {
                match raw_rx.recv().await {
                    Ok(payload) => {
                        if let Some(mut ann) = bridge::unwrap(&payload) {
                            // Generate message ID for deduplication check.
                            ann.get_or_generate_message_id();

                            // Check for duplicate.
                            {
                                let mut filter = dedup_filter.write();
                                if !filter.check_and_insert(&ann) {
                                    tracing::trace!(
                                        message_id = %ann.message_id.as_ref().unwrap_or(&"<none>".to_string()),
                                        "dropping duplicate announcement"
                                    );
                                    continue;
                                }
                            }

                            // Check TTL expiration.
                            if let Some(ref mid) = ann.message_id {
                                let tracker = ttl_tracker.read();
                                if tracker.is_expired(mid) {
                                    tracing::trace!(message_id = %mid, "dropping expired announcement");
                                    continue;
                                }
                            }

                            // If no subscribers, the send errors — that's fine.
                            let _ = tx.send(ann);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        rx
    }

    fn decode_stream_with_persistence(
        mut raw_rx: broadcast::Receiver<AnnouncementPayload>,
        persistence: Arc<MessagePersistence>,
        dedup_filter: Arc<parking_lot::RwLock<DedupeFilter>>,
        ttl_tracker: Arc<parking_lot::RwLock<TtlTracker>>,
    ) -> broadcast::Receiver<Announcement> {
        // Wrap raw payloads into a decoded broadcast channel with persistence.
        let (tx, rx) = broadcast::channel::<Announcement>(DEFAULT_CHANNEL_CAPACITY);
        tokio::spawn(async move {
            loop {
                match raw_rx.recv().await {
                    Ok(payload) => {
                        if let Some(mut ann) = bridge::unwrap(&payload) {
                            // Generate message ID for deduplication check.
                            let message_id = ann.get_or_generate_message_id();

                            // Check for duplicate.
                            {
                                let mut filter = dedup_filter.write();
                                if !filter.check_and_insert(&ann) {
                                    tracing::trace!(
                                        message_id = %ann.message_id.as_ref().unwrap_or(&"<none>".to_string()),
                                        "dropping duplicate announcement"
                                    );
                                    continue;
                                }
                            }

                            // Check TTL expiration.
                            if let Some(ref mid) = ann.message_id {
                                let tracker = ttl_tracker.read();
                                if tracker.is_expired(mid) {
                                    tracing::trace!(message_id = %mid, "dropping expired announcement");
                                    continue;
                                }
                            }

                            // Persist the message.
                            let content = match serde_json::to_vec(&ann) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!("Failed to serialize announcement: {}", e);
                                    continue;
                                }
                            };

                            let room_id = payload.from_node.to_string();
                            let stored = StoredMessage {
                                message_id,
                                room_id: room_id.clone(),
                                content,
                                received_at: Instant::now(),
                                expires_at: ann.effective_expires_at(),
                                sequence: 0,
                            };

                            if let Err(e) = persistence.store_message(&room_id, stored).await {
                                tracing::warn!("Failed to persist message: {}", e);
                            }

                            // If no subscribers, the send errors — that's fine.
                            let _ = tx.send(ann);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        rx
    }

    fn decode_stream_with_persistence_only(
        mut raw_rx: broadcast::Receiver<AnnouncementPayload>,
        persistence: Arc<MessagePersistence>,
    ) -> broadcast::Receiver<Announcement> {
        // Wrap raw payloads into a decoded broadcast channel with persistence only.
        let (tx, rx) = broadcast::channel::<Announcement>(DEFAULT_CHANNEL_CAPACITY);
        tokio::spawn(async move {
            loop {
                match raw_rx.recv().await {
                    Ok(payload) => {
                        if let Some(mut ann) = bridge::unwrap(&payload) {
                            // Generate message ID.
                            let message_id = ann.get_or_generate_message_id();

                            // Persist the message.
                            let content = match serde_json::to_vec(&ann) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!("Failed to serialize announcement: {}", e);
                                    continue;
                                }
                            };

                            let room_id = payload.from_node.to_string();
                            let stored = StoredMessage {
                                message_id,
                                room_id: room_id.clone(),
                                content,
                                received_at: Instant::now(),
                                expires_at: ann.effective_expires_at(),
                                sequence: 0,
                            };

                            if let Err(e) = persistence.store_message(&room_id, stored).await {
                                tracing::warn!("Failed to persist message: {}", e);
                            }

                            // If no subscribers, the send errors — that's fine.
                            let _ = tx.send(ann);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::InProcessGossip;
    use adnet_types::{CdnContentKind, ContentHash};
    use chrono::Utc;

    #[tokio::test]
    async fn bus_publish_subscribe_roundtrip() {
        let node = NodeId::random();
        let bus = GossipBus::new(node.clone(), Arc::new(InProcessGossip::new()));
        let room: RoomId = "lobby".into();
        bus.join_room(&room).await.unwrap();

        let mut rx = bus.subscribe(&room);
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"abc"),
            node_id: node.clone(),
            title: "T".into(),
            kind: CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            message_id: None,
            ttl_secs: None,
            signer: None,
            signature: None,
        };
        bus.publish(&room, &ann).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.content_hash, ann.content_hash);
    }
}
