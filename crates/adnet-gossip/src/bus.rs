//! [`GossipBus`] — typed, room-aware facade over a [`GossipTransport`].

use std::sync::Arc;

use adnet_types::{Announcement, AnnouncementPayload, NodeId, RoomId, Topic, topic_name};
use tokio::sync::broadcast;

use crate::bridge;
use crate::transport::GossipTransport;

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

    fn decode_stream(
        mut raw_rx: broadcast::Receiver<AnnouncementPayload>,
    ) -> broadcast::Receiver<Announcement> {
        // Wrap raw payloads into a decoded broadcast channel.
        let (tx, rx) = broadcast::channel::<Announcement>(1024);
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
            signer: None,
            signature: None,
        };
        bus.publish(&room, &ann).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.content_hash, ann.content_hash);
    }
}
