//! IPNS-over-gossip (PubSub) resolver.
//!
//! `PubsubIpnsResolver` listens on a stable gossip topic (typically
//! `adnet-ipns-v1`) and ingests every [`IpnRecord`] it sees into an
//! [`IpnResolver`]. Combined with [`crate::transport::gossip::GossipIpnTransport`]
//! this gives ADNet a libp2p-floodsub-style IPNS-over-PubSub path:
//! rooms that share the same gossip overlay see name updates
//! immediately, without round-tripping the DHT or pkarr relays.
//!
//! The resolver runs as a tokio task that you start once with
//! [`PubsubIpnsResolver::run`] and cancel via
//! [`PubsubSubscription::shutdown`]. Re-running it after a shutdown
//! returns `Err` — the typical pattern is
//! `let h = resolver.run(bus); ...; h.shutdown().await;`.
//!
//! ## Wire format
//!
//! The gossip payload is a JSON envelope matching the existing
//! `AnnouncementPayload` convention (`from_node` + `payload: Value`):
//!
//! ```json
//! {
//!   "from_node": "<hex>",
//!   "payload": { "kind": "ipns", "from_node": "<hex>", "payload": <IpnRecord> }
//! }
//! ```
//!
//! The outer `payload.kind` discriminates this from non-IPNS traffic
//! that may share the same room in the future (e.g. content
//! announcements). Records are verified by the same
//! sequence-monotonicity rule [`IpnResolver::cache_record`] enforces,
//! and a malformed record is dropped silently — gossip is best-effort.

use std::sync::Arc;

use adnet_gossip::GossipBus;
use adnet_types::{AnnouncementPayload, RoomId};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::ipns::{IpnRecord, IpnsError, IpnResolver};

/// Stable room id used for IPNS-over-gossip. Operators that want a
/// custom name can pass their own [`RoomId`] to
/// [`PubsubIpnsResolver::new`]; the default is this constant.
pub const IPNS_PUBSUB_ROOM: &str = "adnet-ipns-v1";

/// Wire envelope sent on the IPNS gossip topic.
///
/// `kind` discriminates this payload from non-IPNS traffic that may
/// share the same room in the future (e.g. content announcements).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpnGossipPayload {
    pub kind: String,
    pub from_node: String,
    pub payload: serde_json::Value,
}

impl IpnGossipPayload {
    /// Build a publish-side envelope from a signed [`IpnRecord`].
    pub fn from_record(record: &IpnRecord, from_node: &str) -> Result<Self, IpnsError> {
        let payload = serde_json::to_value(record)
            .map_err(|e| IpnsError::Deserialize(format!("record encode: {e}")))?;
        Ok(Self {
            kind: "ipns".to_string(),
            from_node: from_node.to_string(),
            payload,
        })
    }

    /// Serialise this envelope to a `serde_json::Value` ready to drop
    /// into [`AnnouncementPayload::payload`].
    pub fn into_announcement_value(self) -> Result<serde_json::Value, IpnsError> {
        serde_json::to_value(&self)
            .map_err(|e| IpnsError::Deserialize(format!("envelope encode: {e}")))
    }

    /// Parse the payload back into an [`IpnRecord`].
    pub fn decode_record(&self) -> Result<IpnRecord, IpnsError> {
        if self.kind != "ipns" {
            return Err(IpnsError::Deserialize(format!(
                "wrong gossip kind: {:?}",
                self.kind
            )));
        }
        serde_json::from_value(self.payload.clone())
            .map_err(|e| IpnsError::Deserialize(format!("record decode: {e}")))
    }

    /// Convenience: build the outer `AnnouncementPayload` ready for
    /// [`crate::transport::gossip::GossipIpnTransport::publish`] or
    /// direct gossip transmission.
    pub fn into_announcement_payload(
        self,
        from_node: adnet_types::NodeId,
    ) -> Result<AnnouncementPayload, IpnsError> {
        let value = self.into_announcement_value()?;
        Ok(AnnouncementPayload {
            from_node,
            payload: value,
        })
    }
}

/// A running IPNS-over-gossip subscription. Cancels cleanly via
/// [`PubsubSubscription::shutdown`]; the gossip subscription is
/// dropped on the next loop iteration.
pub struct PubsubSubscription {
    cancel: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl PubsubSubscription {
    /// Cooperative shutdown — sends a single-shot signal that the
    /// task awaits before returning. Idempotent: calling shutdown
    /// twice is a no-op on the second call.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// Resolver that listens to a gossip room for IPNS records.
///
/// Cheap to construct — it doesn't subscribe until [`Self::run`] is
/// called. Construct one, share it (it carries an `Arc<IpnResolver>`)
/// across the binary, and start the subscription once on startup.
#[derive(Debug, Clone)]
pub struct PubsubIpnsResolver {
    resolver: Arc<IpnResolver>,
    room_id: RoomId,
}

impl PubsubIpnsResolver {
    /// Wrap an existing [`IpnResolver`]. Uses the default
    /// [`IPNS_PUBSUB_ROOM`] topic.
    pub fn new(resolver: Arc<IpnResolver>) -> Self {
        Self::with_room(resolver, RoomId::from(IPNS_PUBSUB_ROOM.to_string()))
    }

    /// Wrap an existing resolver with a custom room id.
    pub fn with_room(resolver: Arc<IpnResolver>, room_id: RoomId) -> Self {
        Self { resolver, room_id }
    }

    /// Borrow the underlying [`IpnResolver`].
    pub fn resolver(&self) -> &IpnResolver {
        &self.resolver
    }

    /// Room id the resolver subscribes to.
    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// Start the gossip subscription task. Returns a handle whose
    /// [`PubsubSubscription::shutdown`] cancels the loop.
    ///
    /// The task:
    /// 1. Joins the room.
    /// 2. Subscribes to inbound `AnnouncementPayload`s directly on
    ///    the bus's underlying transport (so we read the JSON
    ///    envelope instead of the typed `Announcement` round-trip).
    /// 3. For every envelope whose `kind == "ipns"`, parses the
    ///    record and feeds it to [`IpnResolver::cache_record`]
    ///    (which enforces sequence-monotonicity).
    /// 4. Repeats until cancelled.
    pub fn run(self, bus: GossipBus) -> PubsubSubscription {
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let resolver = self.resolver.clone();
        let room_id = self.room_id.clone();

        let task = tokio::spawn(async move {
            // Join the room first — `subscribe` returns a Receiver
            // but we need the bus to actually fan-in incoming
            // payloads from peers.
            if let Err(e) = bus.join_room(&room_id).await {
                tracing::warn!("IPN pubsub: join_room failed: {e}");
                return;
            }

            // Subscribe at the transport layer so we read the
            // raw `AnnouncementPayload` (from_node + JSON envelope)
            // without going through the `Announcement` round-trip.
            let topic = bus.topic_for(&room_id);
            let mut rx = bus.transport().subscribe(topic);

            loop {
                let payload = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        tracing::debug!("IPN pubsub: cancellation received");
                        break;
                    }
                    result = rx.recv() => {
                        match result {
                            Ok(p) => p,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(lagged = n, "IPN pubsub: gossip lagged");
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                tracing::debug!("IPN pubsub: bus closed");
                                break;
                            }
                        }
                    }
                };

                let envelope: IpnGossipPayload = match serde_json::from_value(payload.payload) {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::trace!("IPN pubsub: skipping non-IPNS envelope ({e})");
                        continue;
                    }
                };
                match envelope.decode_record() {
                    Ok(record) => {
                        resolver.cache_record(record);
                    }
                    Err(e) => {
                        tracing::trace!("IPN pubsub: bad record dropped: {e}");
                    }
                }
            }

            if let Err(e) = bus.leave_room(&room_id).await {
                tracing::debug!("IPN pubsub: leave_room failed: {e}");
            }
        });

        PubsubSubscription {
            cancel: Some(cancel_tx),
            task: Some(task),
        }
    }
}

/// Build a publish-side announcement payload for a signed IPNS
/// record. Convenience wrapper used by callers (CLI, agent) that
/// already hold a [`GossipBus`] and want to broadcast their own
/// records.
pub fn publish_payload(
    record: &IpnRecord,
    from_node: &str,
    sender_node: adnet_types::NodeId,
) -> Result<AnnouncementPayload, IpnsError> {
    IpnGossipPayload::from_record(record, from_node)?.into_announcement_payload(sender_node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipns::{Ed25519SecretKey, IpnPublisher};
    use std::time::Duration;

    fn make_signed_record() -> IpnRecord {
        let secret = Ed25519SecretKey::generate();
        let publisher = IpnPublisher::new(std::sync::Arc::new(secret));
        publisher
            .publish(
                "k51qzi5uqu5dkkciu33khkzbcmxtyhn2abc",
                "abc123".to_string(),
                Duration::from_secs(60),
            )
            .expect("publish")
    }

    #[test]
    fn envelope_round_trips_record() {
        let record = make_signed_record();
        let env = IpnGossipPayload::from_record(&record, "deadbeef").expect("from_record");
        assert_eq!(env.kind, "ipns");
        assert_eq!(env.from_node, "deadbeef");

        let value = env.clone().into_announcement_value().expect("value");
        let parsed: IpnGossipPayload = serde_json::from_value(value).expect("back");
        assert_eq!(parsed, env);

        let back = parsed.decode_record().expect("decode");
        assert_eq!(back.name, record.name);
        assert_eq!(back.value, record.value);
        assert_eq!(back.sequence, record.sequence);
    }

    #[test]
    fn envelope_rejects_wrong_kind() {
        let env = IpnGossipPayload {
            kind: "content".to_string(),
            from_node: "node".to_string(),
            payload: serde_json::json!({}),
        };
        let err = env.decode_record().unwrap_err();
        assert!(matches!(err, IpnsError::Deserialize(_)));
    }

    #[test]
    fn envelope_decode_rejects_malformed_record() {
        let env = IpnGossipPayload {
            kind: "ipns".to_string(),
            from_node: "node".to_string(),
            // Missing required fields: signature/value/name.
            payload: serde_json::json!({"bogus": true}),
        };
        let err = env.decode_record().unwrap_err();
        assert!(matches!(err, IpnsError::Deserialize(_)));
    }

    #[test]
    fn publish_payload_returns_announcement_payload() {
        use adnet_types::NodeId;
        let record = make_signed_record();
        let payload =
            publish_payload(&record, "nodeid", NodeId::random()).expect("publish_payload");
        let env: IpnGossipPayload = serde_json::from_value(payload.payload).expect("envelope");
        assert_eq!(env.kind, "ipns");
    }

    /// End-to-end: a publisher on bus A gossips a record; the
    /// PubsubIpnsResolver on bus B (sharing the same underlying
    /// transport — we reuse `InProcessGossip` with shared state)
    /// ingests it.
    #[tokio::test]
    async fn resolver_ingests_record_via_shared_bus() {
        use adnet_gossip::transport::InProcessGossip;
        use adnet_types::NodeId;
        use std::sync::Arc;

        // Build two buses that share one InProcessGossip so a
        // publish on one is observed on the other.
        let transport = Arc::new(InProcessGossip::new());
        let node_a = NodeId::random();
        let bus_a = GossipBus::new(node_a.clone(), transport.clone());
        let bus_b = GossipBus::new(NodeId::random(), transport.clone());

        let resolver = Arc::new(IpnResolver::new(Duration::from_secs(60)));
        let sub_resolver = PubsubIpnsResolver::new(resolver.clone());
        let room = sub_resolver.room_id().clone();

        let _sub = sub_resolver.run(bus_b.clone());

        // Give the subscription task a moment to join the room.
        tokio::time::sleep(Duration::from_millis(30)).await;

        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(secret));
        let record = publisher
            .publish(
                &name,
                "deadbeef".to_string(),
                Duration::from_secs(60),
            )
            .expect("publish");

        // Publish via bus_a using the same envelope shape as a
        // real caller.
        let payload =
            publish_payload(&record, &node_a.short().to_string(), node_a.clone())
                .expect("publish_payload");
        bus_a
            .transport()
            .broadcast(bus_a.topic_for(&room), payload)
            .await
            .expect("broadcast");

        // Wait for the resolver to ingest.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if resolver.get_cached(&name).is_some() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("resolver did not ingest the record within 2s");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let cached = resolver.get_cached(&name).expect("cached");
        assert_eq!(cached.value, "deadbeef");
    }

    /// Cancellation: after `shutdown`, the subscription stops
    /// ingesting new records.
    #[tokio::test]
    async fn shutdown_stops_ingestion() {
        use adnet_gossip::transport::InProcessGossip;
        use adnet_types::NodeId;
        use std::sync::Arc;

        let transport = Arc::new(InProcessGossip::new());
        let bus_a = GossipBus::new(NodeId::random(), transport.clone());
        let bus_b = GossipBus::new(NodeId::random(), transport.clone());

        let resolver = Arc::new(IpnResolver::new(Duration::from_secs(60)));
        let sub_resolver = PubsubIpnsResolver::new(resolver.clone());
        let room = sub_resolver.room_id().clone();

        let sub = sub_resolver.run(bus_b.clone());
        tokio::time::sleep(Duration::from_millis(30)).await;
        sub.shutdown().await;

        let secret = Ed25519SecretKey::generate();
        let name = secret.ipns_name();
        let publisher = IpnPublisher::new(Arc::new(secret));
        let record = publisher
            .publish(&name, "v1".to_string(), Duration::from_secs(60))
            .expect("publish");
        let payload = publish_payload(&record, "node", NodeId::random()).expect("payload");
        let _ = bus_a
            .transport()
            .broadcast(bus_a.topic_for(&room), payload)
            .await;

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            resolver.get_cached(&name).is_none(),
            "post-shutdown records must not be ingested",
        );
    }
}
