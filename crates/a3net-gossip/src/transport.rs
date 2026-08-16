//! Pluggable transport for gossip payloads.
//!
//! The default implementation [`InProcessGossip`] keeps messages inside the
//! current process — perfect for tests, single-node demos, and unit-style
//! integration. A future `IrohGossipTransport` (gated by an `iroh` feature
//! flag) will satisfy the same trait and route messages through iroh-net.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use a3net_types::{AnnouncementPayload, NodeId, Topic};
use async_trait::async_trait;
use tokio::sync::broadcast;

/// Convenience alias: `TopicId` is a BLAKE3-derived 32-byte id, same shape as
/// `iroh_gossip::proto::TopicId`.
pub type TopicId = Topic;

/// Abstract transport for gossip messages.
#[async_trait]
pub trait GossipTransport: Send + Sync + Debug + 'static {
    /// Join the overlay for the given topic. Returns once the local node is
    /// (insofar as the transport can guarantee) subscribed.
    async fn join(&self, topic: TopicId, node_id: NodeId) -> anyhow::Result<()>;

    /// Leave the topic.
    async fn leave(&self, topic: TopicId) -> anyhow::Result<()>;

    /// Broadcast a payload to every subscriber of `topic`.
    async fn broadcast(&self, topic: TopicId, payload: AnnouncementPayload) -> anyhow::Result<()>;

    /// Subscribe to incoming payloads on `topic`. The returned broadcast
    /// receiver yields every payload published after this call.
    fn subscribe(&self, topic: TopicId) -> broadcast::Receiver<AnnouncementPayload>;
}

/// In-process implementation: every node on the same `InProcessGossip` sees
/// every other node's broadcasts. Useful for tests and as the default when
/// no network transport has been wired.
#[derive(Debug, Clone)]
pub struct InProcessGossip {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    topics: Mutex<HashMap<TopicId, broadcast::Sender<AnnouncementPayload>>>,
}

impl Default for InProcessGossip {
    fn default() -> Self {
        Self::new()
    }
}

impl InProcessGossip {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner::default()),
        }
    }

    fn channel_for(&self, topic: TopicId) -> broadcast::Sender<AnnouncementPayload> {
        let mut guard = self.inner.topics.lock().expect("gossip topics lock");
        guard
            .entry(topic)
            .or_insert_with(|| broadcast::channel(1024).0)
            .clone()
    }
}

#[async_trait]
impl GossipTransport for InProcessGossip {
    async fn join(&self, _topic: TopicId, _node_id: NodeId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn leave(&self, _topic: TopicId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn broadcast(&self, topic: TopicId, payload: AnnouncementPayload) -> anyhow::Result<()> {
        let tx = self.channel_for(topic);
        // It's fine if there are no subscribers — the broadcast just drops.
        let _ = tx.send(payload);
        Ok(())
    }

    fn subscribe(&self, topic: TopicId) -> broadcast::Receiver<AnnouncementPayload> {
        self.channel_for(topic).subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::Topic;

    #[tokio::test]
    async fn broadcast_delivers_to_subscriber() {
        let g = InProcessGossip::new();
        let topic = Topic::from_label("a3net-room-test");
        let mut rx = g.subscribe(topic.clone());
        let payload = AnnouncementPayload {
            from_node: NodeId::random(),
            payload: serde_json::json!({"hello": "world"}),
        };
        g.broadcast(topic, payload.clone()).await.unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.payload, payload.payload);
    }
}
