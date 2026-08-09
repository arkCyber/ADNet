//! iroh-gossip backed transport adapter.
//!
//! When the `iroh` feature is enabled, [`IrohGossipTransport`] is a
//! [`GossipTransport`] impl that wires [`iroh_gossip::net::Gossip`]
//! to ADNet's room/topic naming convention. Subscribers receive
//! payloads from iroh's HyParView+PlumTree epidemic broadcast trees
//! and forward them into a per-topic [`tokio::sync::broadcast`]
//! channel so the rest of the ADNet gossip stack sees the same
//! surface as it does over the in-process bus.
//!
//! ## Topic mapping
//!
//! ADNet topics are 32-byte BLAKE3 digests of labels like
//! `"adnet-room-{room}"`. iroh-gossip's `TopicId` is also 32 bytes but
//! is taken directly (no hashing). We bridge by re-hashing the
//! ADNet hex digest as 32 raw bytes — both sides treat the value as
//! opaque, so this preserves wire-format compatibility with any
//! other iroh node that constructs its `TopicId` from the same
//! 32-byte BLAKE3 output.
//!
//! ## Lifecycle
//!
//! `IrohGossipTransport::new` takes a shared
//! [`iroh_gossip::api::Gossip`] and stores it. Each call to
//! `subscribe(topic)`:
//! 1. Joins the iroh-gossip topic (idempotent — cached in an
//!    internal map).
//! 2. Spawns a task that reads `Event::Received` from the iroh
//!    subscription and forwards the payload into a per-topic
//!    `broadcast::Sender<AnnouncementPayload>`.
//! 3. Returns a `broadcast::Receiver` to the caller.
//!
//! `broadcast(topic, payload)` calls iroh's
//! `TopicSender::broadcast(bytes)`. `join`/`leave` track per-topic
//! state in an `RwLock<HashMap>` so a repeated `join` is cheap and
//! `leave` doesn't kill a concurrent subscription.
//!
//! ## Sync `subscribe()`
//!
//! The [`GossipTransport::subscribe`] trait method is synchronous
//! (`fn subscribe` returning a `broadcast::Receiver`). The iroh
//! `gossip.subscribe(topic, peers)` call is async. We bridge by
//! storing a `broadcast::Sender` in the topic slot under a write
//! lock and returning a receiver immediately; the actual iroh
//! subscription (which spawns the Event→payload forwarder) is
//! driven on a background task. If callers need the iroh topic to
//! already be joined before the first `broadcast` lands, they should
//! call `join()` first — which is exactly the contract `join()`
//! documents.

#[cfg(feature = "iroh")]
use std::collections::HashMap;
#[cfg(feature = "iroh")]
use std::sync::Arc;

#[cfg(feature = "iroh")]
use adnet_types::{AnnouncementPayload, NodeId, Topic};
#[cfg(feature = "iroh")]
use async_trait::async_trait;
#[cfg(feature = "iroh")]
use iroh::EndpointId;
#[cfg(feature = "iroh")]
use iroh_gossip::proto::TopicId as IrohTopicId;
#[cfg(feature = "iroh")]
use iroh_gossip::{Gossip as IrohGossip, api::Event};
#[cfg(feature = "iroh")]
use tokio::sync::{RwLock, broadcast};
#[cfg(feature = "iroh")]
use tracing::{debug, warn};

#[cfg(feature = "iroh")]
use crate::transport::GossipTransport;

/// Decode a hex-encoded ADNet `Topic` (64 hex chars = 32 bytes) into
/// the raw bytes iroh-gossip expects. Both representations are
/// 32 bytes wide, so the mapping is lossless.
#[cfg(feature = "iroh")]
fn topic_to_iroh_topic_id(topic: &Topic) -> IrohTopicId {
    let hex = topic.as_hex();
    let mut bytes = [0u8; 32];
    let raw = hex.as_bytes();
    debug_assert_eq!(raw.len(), 64);
    for i in 0..32 {
        let pair = std::str::from_utf8(&raw[i * 2..i * 2 + 2]).unwrap_or("00");
        bytes[i] = u8::from_str_radix(pair, 16).unwrap_or(0);
    }
    IrohTopicId::from_bytes(bytes)
}

/// Per-topic state held by the transport. The `Sender` is the
/// broadcast channel the local `subscribe()` callers tap into; the
/// iroh `GossipReceiver` half is what we read events from to feed
/// the channel. The `TopicSender` used for outgoing `broadcast()` is
/// re-derived on each call (cheap — iroh-gossip backs it with an
/// `irpc` channel).
#[cfg(feature = "iroh")]
#[derive(Debug)]
struct TopicSlot {
    tx: broadcast::Sender<AnnouncementPayload>,
    /// `Some` while the iroh subscribe task is alive; `None` while
    /// we're in the "channel-only" state before `ensure_joined`
    /// completes.
    _receiver_task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(feature = "iroh")]
#[derive(Debug, Clone)]
pub struct IrohGossipTransport {
    gossip: IrohGossip,
    local_node: NodeId,
    topics: Arc<RwLock<HashMap<Topic, TopicSlot>>>,
}

#[cfg(feature = "iroh")]
impl IrohGossipTransport {
    /// Build a new transport on top of an existing
    /// [`IrohGossip`]. Pass the local node id so it can be set as the
    /// `from_node` of outgoing payloads (it is otherwise opaque — the
    /// iroh-gossip protocol authenticates via the underlying QUIC
    /// connection, not the embedded node id).
    pub fn new(local_node: NodeId, gossip: IrohGossip) -> Self {
        Self {
            gossip,
            local_node,
            topics: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Borrow the underlying iroh-gossip handle for callers that
    /// need to register their own accept / protocol handlers (e.g.
    /// `adnet-node` wiring this transport into a `Router`).
    pub fn gossip(&self) -> &IrohGossip {
        &self.gossip
    }

    /// Local node id this transport was created with.
    pub fn local_node(&self) -> &NodeId {
        &self.local_node
    }

    /// Ensure the local node has joined the topic and the broadcast
    /// channel exists. Idempotent: returns the existing channel if
    /// already joined.
    async fn ensure_joined(
        &self,
        topic: &Topic,
    ) -> anyhow::Result<broadcast::Sender<AnnouncementPayload>> {
        {
            let guard = self.topics.read().await;
            if let Some(slot) = guard.get(topic) {
                return Ok(slot.tx.clone());
            }
        }
        let mut guard = self.topics.write().await;
        // Re-check under write lock: another task may have raced us.
        if let Some(slot) = guard.get(topic) {
            return Ok(slot.tx.clone());
        }
        let (tx, _initial_rx) = broadcast::channel::<AnnouncementPayload>(1024);
        let iroh_topic = topic_to_iroh_topic_id(topic);
        // Subscribe to the iroh swarm. Bootstrap peers are passed as
        // an empty list — the upstream `GossipBuilder` is responsible
        // for whatever address-lookup / Pkarr mechanism the
        // application wants to use. We forward incoming messages into
        // the broadcast channel and ignore the `GossipSender` half
        // (which we re-derive for outgoing `broadcast()` requests).
        let (_sender, mut receiver) = self
            .gossip
            .subscribe(iroh_topic, Vec::<EndpointId>::new())
            .await?
            .split();
        let tx_for_task = tx.clone();
        let topic_label = topic.as_hex().to_string();
        let handle = tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(event) = receiver.next().await {
                match event {
                    Ok(Event::Received(msg)) => {
                        let payload: AnnouncementPayload =
                            match serde_json::from_slice(&msg.content) {
                                Ok(p) => p,
                                Err(e) => {
                                    warn!(
                                        topic = %topic_label,
                                        "iroh gossip: failed to decode payload: {e}"
                                    );
                                    continue;
                                }
                            };
                        let _ = tx_for_task.send(payload);
                    }
                    Ok(Event::NeighborUp(peer)) => {
                        debug!(topic = %topic_label, peer = %short_hex(peer.as_bytes()), "neighbor up");
                    }
                    Ok(Event::NeighborDown(peer)) => {
                        debug!(topic = %topic_label, peer = %short_hex(peer.as_bytes()), "neighbor down");
                    }
                    Ok(Event::Lagged) => {
                        warn!(topic = %topic_label, "iroh gossip lagged behind");
                    }
                    Err(e) => {
                        warn!(topic = %topic_label, "iroh gossip stream error: {e}");
                        break;
                    }
                }
            }
        });
        guard.insert(
            topic.clone(),
            TopicSlot {
                tx: tx.clone(),
                _receiver_task: Some(handle),
            },
        );
        Ok(tx)
    }

    /// Get-or-create a `broadcast::Sender` for `topic` synchronously.
    /// The actual iroh subscription is spawned in the background —
    /// callers who need it fully joined should call `join()` first.
    fn ensure_channel_sync(&self, topic: &Topic) -> broadcast::Sender<AnnouncementPayload> {
        // Fast path: try a read lock.
        if let Ok(guard) = self.topics.try_read()
            && let Some(slot) = guard.get(topic)
        {
            return slot.tx.clone();
        }
        // Slow path: take the write lock and create the channel.
        // We block on the runtime here because we cannot return a
        // receiver until a `Sender` exists.
        let topics = Arc::clone(&self.topics);
        match topics.try_write() {
            Ok(mut guard) => {
                if let Some(slot) = guard.get(topic) {
                    return slot.tx.clone();
                }
                let (tx, _rx) = broadcast::channel::<AnnouncementPayload>(1024);
                guard.insert(
                    topic.clone(),
                    TopicSlot {
                        tx: tx.clone(),
                        _receiver_task: None,
                    },
                );
                // Spawn the iroh-side join in the background. The
                // channel we just installed will start receiving
                // messages as soon as the spawn lands.
                let transport = self.clone();
                let topic_for_task = topic.clone();
                tokio::spawn(async move {
                    if let Err(e) = transport.ensure_joined(&topic_for_task).await {
                        warn!(
                            topic = %topic_for_task.as_hex(),
                            "background iroh join failed: {e}"
                        );
                    }
                });
                tx
            }
            Err(_) => {
                // Someone else holds the write lock. Create a one-off
                // channel and let the next call populate the shared
                // slot. This is racy but only affects the very first
                // `subscribe()` call under contention.
                let (tx, _rx) = broadcast::channel::<AnnouncementPayload>(1024);
                tx
            }
        }
    }
}

#[cfg(feature = "iroh")]
fn short_hex(bytes: &[u8]) -> String {
    let n = bytes.len().min(6);
    hex::encode(&bytes[..n])
}

#[cfg(feature = "iroh")]
#[async_trait]
impl GossipTransport for IrohGossipTransport {
    async fn join(&self, topic: Topic, _node_id: NodeId) -> anyhow::Result<()> {
        self.ensure_joined(&topic).await?;
        debug!(
            topic = %topic.as_hex(),
            local = %self.local_node.short(),
            "joined iroh-gossip topic"
        );
        Ok(())
    }

    async fn leave(&self, topic: Topic) -> anyhow::Result<()> {
        let mut guard = self.topics.write().await;
        if guard.remove(&topic).is_some() {
            debug!(topic = %topic.as_hex(), "left iroh-gossip topic");
        }
        Ok(())
    }

    async fn broadcast(&self, topic: Topic, payload: AnnouncementPayload) -> anyhow::Result<()> {
        let iroh_topic = topic_to_iroh_topic_id(&topic);
        // Subscribe is cheap on a hot topic (iroh returns a clone of
        // the internal state). On the first broadcast we still pay
        // for the swarm handshake; subsequent broadcasts amortize
        // this. Callers who want the topic fully warm should call
        // `join()` first.
        let mut topic_handle = self
            .gossip
            .subscribe(iroh_topic, Vec::<EndpointId>::new())
            .await?;
        let bytes = serde_json::to_vec(&payload)?;
        topic_handle
            .broadcast(bytes.into())
            .await
            .map_err(|e| anyhow::anyhow!("iroh gossip broadcast: {e}"))?;
        Ok(())
    }

    fn subscribe(&self, topic: Topic) -> broadcast::Receiver<AnnouncementPayload> {
        let tx = self.ensure_channel_sync(&topic);
        tx.subscribe()
    }
}

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;

    /// The hex ↔ bytes bridge is the most error-prone mapping in the
    /// adapter, so it gets a dedicated unit test that doesn't need a
    /// running endpoint.
    #[test]
    fn topic_hex_to_iroh_topic_id_round_trip() {
        let topic = Topic::from_label("adnet-room-lobby");
        let iroh_topic = topic_to_iroh_topic_id(&topic);
        let bytes = iroh_topic.as_bytes();
        assert_eq!(bytes.len(), 32);
        // Re-encoding the bytes back to hex should match the ADNet
        // topic's hex representation (modulo any case differences).
        let reencoded = hex::encode(bytes);
        assert_eq!(reencoded.to_lowercase(), topic.as_hex().to_lowercase());
    }

    /// Constructing a transport does not require a network — it's a
    /// pure handle. We can't actually spawn a `Gossip` without an
    /// `iroh::Endpoint`, so this test only verifies the topic
    /// conversion in isolation.
    #[test]
    fn transport_topic_mapping_is_deterministic() {
        let a = Topic::from_label("adnet-room-lobby");
        let b = Topic::from_label("adnet-room-lobby");
        assert_eq!(
            topic_to_iroh_topic_id(&a).as_bytes(),
            topic_to_iroh_topic_id(&b).as_bytes()
        );
    }
}
