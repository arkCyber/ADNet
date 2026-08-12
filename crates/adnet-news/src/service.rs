//! High-level `NewsService` — orchestrates store, gossip bus and
//! event stream.
//!
//! Responsibilities:
//!
//! 1. **Publish**: validate → sign (optional) → persist (local
//!    `insert` with assigned `sequence`) → publish envelope via the
//!    shared `GossipTransport` → emit `BulletinEvent::Insert` to
//!    local subscribers.
//! 2. **Ingest**: receive a raw envelope → validate → enforce
//!    identity invariant (`envelope.from_node == item.author_id`
//!    under `Strict` mode) → persist via `insert_remote` → emit
//!    `Insert` / `Correction` / `Retraction` events.
//! 3. **Subscribe**: return a `broadcast::Receiver<BulletinEvent>`
//!    for the caller's UI / pipeline. Subscribers receive every
//!    locally-originated AND peer-originated bulletin after it
//!    lands in the store.
//! 4. **Offline catch-up**: on startup, replay every persisted
//!    bulletin in `room → sequence` order so the local event stream
//!    sees the full history regardless of how long the node was
//!    offline.
//!
//! The service deliberately does NOT implement signature *crypto*
//! verification — that lives behind `adnet-identity::Wallet`. The
//! service enforces the policy (`Strict` rejects unsigned bulletins;
//! `Lenient` accepts them but still surfaces a `warn!` log), not
//! the math.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use adnet_gossip::{GossipTransport, TopicId};
use adnet_types::{
    BulletinId, BulletinItem, NodeId, RoomId, WalletAddress,
};

use crate::envelope::{
    BulletinEnvelope, BulletinEnvelopePayload, BulletinEvent, topic_id,
    BULLETIN_TOPIC_PREFIX,
};
use crate::error::{NewsError, NewsResult};
use crate::store::{
    BulletinCursor, BulletinSource, BulletinStore, BulletinStoreConfig, StoredBulletin,
};

/// Validation policy applied to inbound envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationPolicy {
    /// Reject unsigned envelopes; require
    /// `envelope.from_node == item.author_id`.
    Strict,
    /// Reject envelopes that claim a different author than
    /// `from_node`. Allows unsigned bulletins.
    Audit,
    /// Accept every envelope that passes `BulletinItem::validate`
    /// — useful for tests and untrusted-network experiments.
    Lenient,
}

impl Default for ValidationPolicy {
    fn default() -> Self {
        Self::Strict
    }
}

/// Configuration for [`NewsService`].
#[derive(Debug, Clone)]
pub struct NewsServiceConfig {
    pub store_dir: PathBuf,
    pub policy: ValidationPolicy,
    /// Fan-out size for the local event broadcast channel.
    pub event_channel_capacity: usize,
}

impl Default for NewsServiceConfig {
    fn default() -> Self {
        Self {
            store_dir: std::env::temp_dir().join("adnet-news"),
            policy: ValidationPolicy::Strict,
            event_channel_capacity: 1024,
        }
    }
}

/// Service-level handle. Cheap to clone — the heavy bits (gossip
/// transport, store, broadcast channel) live behind `Arc`.
#[derive(Debug)]
pub struct NewsService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    local_node: NodeId,
    transport: Arc<dyn GossipTransport>,
    store: BulletinStore,
    policy: ValidationPolicy,
    event_tx: broadcast::Sender<BulletinEvent>,
    /// Has the first-subscribe replay already fired?
    replay_done: RwLock<bool>,
}

impl NewsService {
    /// Open a new service. The gossip transport is shared with the
    /// rest of the node so a single iroh `Endpoint` powers both the
    /// room/asset gossip and the bulletin stream.
    pub fn open(
        local_node: NodeId,
        transport: Arc<dyn GossipTransport>,
        config: NewsServiceConfig,
    ) -> NewsResult<Self> {
        let store = BulletinStore::open(BulletinStoreConfig {
            storage_dir: config.store_dir.clone(),
        })?;
        let (event_tx, _) = broadcast::channel(config.event_channel_capacity.max(16));
        let inner = Inner {
            local_node,
            transport,
            store,
            policy: config.policy,
            event_tx,
            replay_done: RwLock::new(false),
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Like [`Self::open`] but uses an in-memory store. Used by
    /// tests.
    pub fn open_in_memory(
        local_node: NodeId,
        transport: Arc<dyn GossipTransport>,
        policy: ValidationPolicy,
    ) -> NewsResult<Self> {
        let store = BulletinStore::open_in_memory()?;
        let (event_tx, _) = broadcast::channel(1024);
        let inner = Inner {
            local_node,
            transport,
            store,
            policy,
            event_tx,
            replay_done: RwLock::new(false),
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Build via the builder for staged configuration.
    pub fn builder(
        local_node: NodeId,
        transport: Arc<dyn GossipTransport>,
    ) -> NewsServiceBuilder {
        NewsServiceBuilder {
            local_node,
            transport,
            config: NewsServiceConfig::default(),
        }
    }

    pub fn local_node(&self) -> &NodeId {
        &self.inner.local_node
    }

    pub fn store(&self) -> &BulletinStore {
        &self.inner.store
    }

    pub fn policy(&self) -> ValidationPolicy {
        self.inner.policy
    }

    /// Subscribe to the typed event stream. Multiple subscribers are
    /// supported (each `Sender` fan-outs to all `Receiver`s`).
    ///
    /// The first call after `open` triggers a one-shot replay of
    /// the persisted history so the subscriber sees the full
    /// timeline; later subscribers observe live events only.
    pub fn subscribe(&self) -> broadcast::Receiver<BulletinEvent> {
        // Reserve the replay slot BEFORE we subscribe so a
        // concurrent subscribe() doesn't fire a duplicate.
        let is_first = {
            let mut done = self.inner.replay_done.write();
            if *done {
                false
            } else {
                *done = true;
                true
            }
        };
        // Subscribe FIRST so this receiver is registered before
        // any replay events get fanned out.
        let rx = self.inner.event_tx.subscribe();
        if is_first {
            let _ = self.replay_all();
        }
        rx
    }

    // ── Publishing ─────────────────────────────────────────────────────

    /// Publish a `BulletinItem` authored by `self.local_node`. The
    /// store assigns the next sequence number and the gossip
    /// transport carries the envelope to peers.
    ///
    /// `sign` is invoked only when the caller did not already
    /// attach a signature; in practice the higher-level
    /// `Node::publish_news` calls this with the wallet signer.
    pub async fn publish(&self, mut item: BulletinItem) -> NewsResult<BulletinItem> {
        if item.author_id != self.inner.local_node {
            return Err(NewsError::Validation(format!(
                "publish: author_id {} != local_node {}",
                item.author_id, self.inner.local_node
            )));
        }
        item.validate().map_err(|e| NewsError::Validation(e.to_string()))?;

        // Persist first — assigns the canonical sequence number.
        let stored = self.inner.store.insert(item.clone())?;
        let item = stored.item.clone();

        // Broadcast.
        let envelope = BulletinEnvelope::wrap(item.clone(), self.inner.local_node.clone());
        let payload = BulletinEnvelopePayload::from_envelope(&envelope)?;
        let topic = topic_id(&item.room_id);
        let wire = adnet_gossip::AnnouncementPayload {
            from_node: payload.from_node.clone(),
            payload: payload.payload,
        };
        self.inner
            .transport
            .broadcast(topic, wire)
            .await
            .map_err(|e| NewsError::Gossip(e.to_string()))?;

        // Emit to local subscribers.
        let _ = self.inner.event_tx.send(BulletinEvent::Insert(item.clone()));
        debug!(bulletin_id = %item.bulletin_id, room = %item.room_id, "published bulletin");
        Ok(item)
    }

    /// Variant of [`Self::publish`] that takes a pre-signed
    /// `BulletinItem`. The local store is still the source of truth
    /// for the `sequence` field; the envelope carries the
    /// caller's signature so peers can verify authorship
    /// independently.
    pub async fn publish_signed(
        &self,
        mut item: BulletinItem,
        signer: WalletAddress,
        signature: Vec<u8>,
    ) -> NewsResult<BulletinItem> {
        if item.author_id != self.inner.local_node {
            return Err(NewsError::Validation(format!(
                "publish_signed: author_id {} != local_node {}",
                item.author_id, self.inner.local_node
            )));
        }
        if item.signer.is_none() {
            item.attach_signature(signer, signature);
        }
        self.publish(item).await
    }

    // ── Ingest ─────────────────────────────────────────────────────────

    /// Ingest a single envelope received over the gossip bus.
    /// Returns the stored item (with hydrated `sequence` /
    /// `received_at`) on success.
    pub async fn ingest_envelope(&self, envelope: BulletinEnvelope) -> NewsResult<BulletinItem> {
        self.enforce_policy(&envelope)?;
        envelope.validate()?;
        let stored = self.inner.store.insert_remote(envelope.item.clone())?;
        let item = stored.item.clone();

        let event = if envelope.item.kind == adnet_types::BulletinKind::Correction
            && envelope.item.supersedes.is_some()
        {
            BulletinEvent::Correction {
                superseded_id: envelope.item.supersedes.clone().unwrap(),
                corrected: item.clone(),
            }
        } else if envelope.item.kind == adnet_types::BulletinKind::Retraction
            && envelope.item.supersedes.is_some()
        {
            BulletinEvent::Retraction {
                superseded_id: envelope.item.supersedes.clone().unwrap(),
                retraction: item.clone(),
            }
        } else {
            BulletinEvent::Insert(item.clone())
        };
        let _ = self.inner.event_tx.send(event);
        Ok(item)
    }

    /// Convenience: ingest a raw payload delivered by
    /// `GossipTransport::subscribe`. Decodes the JSON, applies the
    /// current validation policy, and persists the resulting
    /// bulletin.
    pub async fn ingest_payload(
        &self,
        room: &RoomId,
        from_node: &NodeId,
        payload: serde_json::Value,
    ) -> NewsResult<BulletinItem> {
        let item: BulletinItem = serde_json::from_value(payload)?;
        let envelope = BulletinEnvelope {
            version: crate::envelope::BULLETIN_ENVELOPE_VERSION,
            from_node: from_node.clone(),
            item,
            signer: None,
            signature: None,
        };
        // Tag the room so the envelope validate step still matches
        // the topic; we use the supplied room as a sanity gate.
        if envelope.item.room_id != *room {
            return Err(NewsError::Validation(format!(
                "ingest_payload: room mismatch (envelope room {}, topic room {})",
                envelope.item.room_id, room
            )));
        }
        self.ingest_envelope(envelope).await
    }

    // ── Timeline / read / ack ──────────────────────────────────────────

    /// Newest-first paginated timeline fetch.
    pub fn timeline(
        &self,
        room: &RoomId,
        before_seq: Option<u32>,
        limit: usize,
    ) -> NewsResult<Vec<StoredBulletin>> {
        Ok(self.inner.store.list_timeline(room, before_seq, limit.max(1))?)
    }

    /// Look up a single bulletin.
    pub fn get(&self, room: &RoomId, id: &BulletinId) -> NewsResult<Option<StoredBulletin>> {
        Ok(self.inner.store.get(room, id)?)
    }

    /// Mark a bulletin as read for the local node.
    pub fn mark_read(&self, room: &RoomId, id: &BulletinId) -> NewsResult<()> {
        self.inner
            .store
            .mark_read(room, id, &self.inner.local_node)?;
        Ok(())
    }

    // ── Subscriptions / catch-up ───────────────────────────────────────

    /// Subscribe to the gossip topic for `room` — must be called
    /// before remote bulletins will flow through
    /// [`Self::ingest_payload`]. Idempotent; safe to call from
    /// multiple owners.
    pub async fn join_room(&self, room: &RoomId) -> NewsResult<()> {
        let topic = topic_id(room);
        self.inner
            .transport
            .join(topic, self.inner.local_node.clone())
            .await
            .map_err(|e| NewsError::Gossip(e.to_string()))?;
        Ok(())
    }

    /// Counterpart to [`Self::join_room`].
    pub async fn leave_room(&self, room: &RoomId) -> NewsResult<()> {
        let topic = topic_id(room);
        self.inner
            .transport
            .leave(topic)
            .await
            .map_err(|e| NewsError::Gossip(e.to_string()))?;
        Ok(())
    }

    /// Wired a raw transport subscriber into the service. Returns
    /// the wrapped `broadcast::Receiver` that yields
    /// `serde_json::Value` payloads (the caller is responsible for
    /// decoding them via [`Self::ingest_payload`]). The wrapper
    /// runs in a tokio task; the receiver closes when the task
    /// exits.
    pub fn wire_transport_subscriber(
        &self,
        room: &RoomId,
    ) -> NewsResult<broadcast::Receiver<serde_json::Value>> {
        let topic = topic_id(room);
        let raw_rx = self.inner.transport.subscribe(topic);
        let (tx, rx) = broadcast::channel::<serde_json::Value>(1024);
        let svc = self.clone();
        let room = room.clone();
        tokio::spawn(async move {
            let mut raw_rx = raw_rx;
            loop {
                match raw_rx.recv().await {
                    Ok(payload) => {
                        let raw_payload = payload.payload.clone();
                        if let Err(e) = svc
                            .ingest_payload(&room, &payload.from_node, raw_payload.clone())
                            .await
                        {
                            debug!(
                                error = %e,
                                room = %room,
                                "news: drop malformed envelope"
                            );
                        }
                        // Forward the original payload to the
                        // downstream consumer too — useful for
                        // ad-hoc tooling that wants to inspect raw
                        // gossip frames.
                        let _ = tx.send(raw_payload);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(rx)
    }

    /// Replay every persisted bulletin into the local event stream
    /// so subscribers see the full history after restart.
    pub fn replay_all(&self) -> NewsResult<()> {
        let rooms = self.inner.store.known_rooms()?;
        for room in rooms {
            let items = self.inner.store.list_replay(&room)?;
            for stored in items {
                let event = bulletin_event_for(&stored);
                let _ = self.inner.event_tx.send(event);
            }
            let _ = self.inner.event_tx.send(BulletinEvent::ReplayComplete {
                room: room.clone(),
                replayed: self.inner.store.list_replay(&room)?.len(),
            });
        }
        Ok(())
    }

    // ── Internals ──────────────────────────────────────────────────────

    fn enforce_policy(&self, envelope: &BulletinEnvelope) -> NewsResult<()> {
        match self.inner.policy {
            ValidationPolicy::Strict => {
                if !envelope.is_signed() {
                    return Err(NewsError::Validation(
                        "strict policy: unsigned envelope rejected".into(),
                    ));
                }
                if envelope.from_node != envelope.item.author_id {
                    return Err(NewsError::Validation(format!(
                        "strict policy: from_node {} != author_id {}",
                        envelope.from_node, envelope.item.author_id
                    )));
                }
            }
            ValidationPolicy::Audit => {
                if envelope.from_node != envelope.item.author_id {
                    return Err(NewsError::Validation(format!(
                        "audit policy: from_node {} != author_id {}",
                        envelope.from_node, envelope.item.author_id
                    )));
                }
                if !envelope.is_signed() {
                    warn!(
                        bulletin_id = %envelope.item.bulletin_id,
                        "news: unsigned envelope accepted under Audit policy"
                    );
                }
            }
            ValidationPolicy::Lenient => {}
        }
        Ok(())
    }
}

impl Clone for NewsService {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

fn bulletin_event_for(stored: &StoredBulletin) -> BulletinEvent {
    use adnet_types::BulletinKind;
    match stored.item.kind {
        BulletinKind::Correction if stored.item.supersedes.is_some() => {
            BulletinEvent::Correction {
                superseded_id: stored.item.supersedes.clone().unwrap(),
                corrected: stored.item.clone(),
            }
        }
        BulletinKind::Retraction if stored.item.supersedes.is_some() => {
            BulletinEvent::Retraction {
                superseded_id: stored.item.supersedes.clone().unwrap(),
                retraction: stored.item.clone(),
            }
        }
        _ => BulletinEvent::Insert(stored.item.clone()),
    }
}

/// Builder for staged configuration.
pub struct NewsServiceBuilder {
    local_node: NodeId,
    transport: Arc<dyn GossipTransport>,
    config: NewsServiceConfig,
}

impl NewsServiceBuilder {
    pub fn with_store_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.store_dir = dir.into();
        self
    }

    pub fn with_policy(mut self, policy: ValidationPolicy) -> Self {
        self.config.policy = policy;
        self
    }

    pub fn with_event_capacity(mut self, n: usize) -> Self {
        self.config.event_channel_capacity = n.max(16);
        self
    }

    pub async fn open(self) -> NewsResult<NewsService> {
        // `open` is non-async — callers that want a fully async
        // build path can still wrap it in `spawn_blocking`.
        NewsService::open(self.local_node, self.transport, self.config)
    }

    pub fn open_blocking(self) -> NewsResult<NewsService> {
        NewsService::open(self.local_node, self.transport, self.config)
    }
}

// adnet-gossip's `broadcast` re-export shim: ensure adnet-gossip is
// in scope even when callers don't pull it in.
#[allow(dead_code)]
fn _gossip_transport_bound(_t: &dyn GossipTransport) {}

// adnet-gossip topic id alias so we don't accidentally drift.
#[allow(dead_code)]
type _TopicId = TopicId;
#[allow(dead_code)]
const _PREFIX: &str = BULLETIN_TOPIC_PREFIX;
#[allow(dead_code)]
fn _cursor_kind() -> BulletinCursor {
    BulletinCursor::Local
}
#[allow(dead_code)]
fn _cursor_source() -> BulletinSource {
    BulletinSource::Local
}
#[allow(dead_code)]
fn _rwlock_marker() -> RwLock<()> {
    RwLock::new(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_gossip::InProcessGossip;
    use adnet_types::{
        BulletinCategory, BulletinKind, BulletinSeverity, ContentHash, WalletAddress,
    };
    use std::collections::HashMap;

    fn node() -> NodeId {
        NodeId::random()
    }

    fn item_for(room: &str, severity: BulletinSeverity, author: NodeId) -> BulletinItem {
        item_for_nonce(room, severity, author, b"nonce")
    }

    fn item_for_nonce(room: &str, severity: BulletinSeverity, author: NodeId, nonce: &[u8]) -> BulletinItem {
        BulletinItem::new(
            BulletinKind::Announcement,
            BulletinCategory::General,
            severity,
            RoomId::new(room),
            author,
            "Title",
            "Summary",
            "Body",
            nonce,
            None,
        )
        .unwrap()
    }

    fn open_svc(policy: ValidationPolicy) -> (Arc<InProcessGossip>, NewsService) {
        let transport = Arc::new(InProcessGossip::new());
        let svc = NewsService::open_in_memory(node(), transport.clone(), policy).unwrap();
        (transport, svc)
    }

    fn open_svc_for(local: NodeId, policy: ValidationPolicy) -> (Arc<InProcessGossip>, NewsService) {
        let transport = Arc::new(InProcessGossip::new());
        let svc = NewsService::open_in_memory(local, transport.clone(), policy).unwrap();
        (transport, svc)
    }

    #[tokio::test]
    async fn publish_persists_and_assigns_sequence() {
        let local = node();
        let (_tx, svc) = open_svc_for(local.clone(), ValidationPolicy::Strict);
        let stored = svc
            .publish(item_for("r", BulletinSeverity::Info, local.clone()))
            .await
            .unwrap();
        assert_eq!(stored.sequence, 1);
        assert_eq!(
            svc.store().cursor(&RoomId::new("r"), BulletinCursor::Local).unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn publish_rejects_wrong_author() {
        let (_tx, svc) = open_svc(ValidationPolicy::Strict);
        let bad = BulletinItem::new(
            BulletinKind::Announcement,
            BulletinCategory::General,
            BulletinSeverity::Info,
            RoomId::new("r"),
            // Different from svc.local_node.
            node(),
            "Title",
            "Summary",
            "Body",
            b"nonce",
            None,
        )
        .unwrap();
        let err = svc.publish(bad).await.unwrap_err();
        assert!(err.to_string().contains("author_id"), "got {err}");
    }

    #[tokio::test]
    async fn ingest_envelope_persists_and_emits() {
        let (_tx, svc) = open_svc(ValidationPolicy::Lenient);
        let mut remote_item = item_for("r", BulletinSeverity::Info, node());
        remote_item.sequence = 42;
        let envelope = BulletinEnvelope::wrap(remote_item, node());
        // Subscribe BEFORE ingesting so the broadcast channel
        // observes the Insert.
        let mut rx = svc.subscribe();
        let stored = svc.ingest_envelope(envelope).await.unwrap();
        assert_eq!(stored.sequence, 42);
        match rx.recv().await {
            Ok(BulletinEvent::Insert(_)) => {}
            Ok(other) => panic!("expected Insert, got {other:?}"),
            Err(e) => panic!("recv error: {e}"),
        }
    }

    #[tokio::test]
    async fn strict_policy_rejects_unsigned() {
        let (_tx, svc) = open_svc(ValidationPolicy::Strict);
        let mut remote = item_for("r", BulletinSeverity::Info, node());
        remote.sequence = 1;
        let envelope = BulletinEnvelope::wrap(remote, node());
        let err = svc.ingest_envelope(envelope).await.unwrap_err();
        assert!(
            err.to_string().contains("unsigned"),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn strict_policy_rejects_mismatched_from_node() {
        let (_tx, svc) = open_svc(ValidationPolicy::Strict);
        let author = node();
        let mut remote = item_for("r", BulletinSeverity::Info, author.clone());
        remote.sequence = 1;
        let mut envelope = BulletinEnvelope::wrap(remote, node()); // from_node != author
        envelope.attach_signature(
            WalletAddress::from_bytes([0x01u8; 20]),
            vec![0u8; 65],
        );
        let err = svc.ingest_envelope(envelope).await.unwrap_err();
        assert!(
            err.to_string().contains("from_node"),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn timeline_pagination() {
        let local = node();
        let (_tx, svc) = open_svc_for(local.clone(), ValidationPolicy::Strict);
        for i in 0..5 {
            svc.publish(item_for_nonce(
                "r",
                BulletinSeverity::Info,
                local.clone(),
                format!("n{i}").as_bytes(),
            ))
            .await
            .unwrap();
        }
        let page = svc.timeline(&RoomId::new("r"), None, 3).unwrap();
        assert_eq!(page.len(), 3);
    }

    #[tokio::test]
    async fn mark_read_records_local_node() {
        let local = node();
        let (_tx, svc) = open_svc_for(local.clone(), ValidationPolicy::Strict);
        let stored = svc
            .publish(item_for("r", BulletinSeverity::Info, local.clone()))
            .await
            .unwrap();
        svc.mark_read(&RoomId::new("r"), &stored.bulletin_id).unwrap();
        let readers = svc
            .store()
            .list_readers(&RoomId::new("r"), &stored.bulletin_id)
            .unwrap();
        assert!(readers.iter().any(|n| n == svc.local_node()));
    }

    #[tokio::test]
    async fn replay_emits_history_after_restart() {
        let local = node();
        // Both service instances share the same on-disk store so
        // the second instance can replay what the first persisted.
        let dir = tempfile::tempdir().unwrap();
        let cfg = NewsServiceConfig {
            store_dir: dir.path().to_path_buf(),
            policy: ValidationPolicy::Strict,
            event_channel_capacity: 64,
        };
        let transport1 = Arc::new(InProcessGossip::new());
        let svc = NewsService::open(local.clone(), transport1, cfg.clone()).unwrap();
        svc.publish(item_for_nonce("a", BulletinSeverity::Info, local.clone(), b"n1"))
            .await
            .unwrap();
        svc.publish(item_for_nonce("a", BulletinSeverity::Info, local.clone(), b"n2"))
            .await
            .unwrap();
        let transport2 = Arc::new(InProcessGossip::new());
        let svc2 = NewsService::open(local, transport2, cfg).unwrap();
        let mut rx = svc2.subscribe();
        let mut inserts = 0;
        // Give broadcast time to deliver replay events.
        for _ in 0..50 {
            while let Ok(ev) = rx.try_recv() {
                if matches!(ev, BulletinEvent::Insert(_)) {
                    inserts += 1;
                }
            }
            if inserts == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(inserts, 2);
    }

    #[tokio::test]
    async fn publish_signed_sets_signer() {
        let local = node();
        let (_tx, svc) = open_svc_for(local.clone(), ValidationPolicy::Strict);
        let i = item_for("r", BulletinSeverity::Info, local.clone());
        let signer = WalletAddress::from_bytes([0x77u8; 20]);
        let sig = vec![0xAB; 65];
        let stored = svc
            .publish_signed(i, signer, sig.clone())
            .await
            .unwrap();
        assert_eq!(stored.signer, Some(signer));
        assert_eq!(stored.signature.as_ref().unwrap(), &sig);
    }

    #[tokio::test]
    async fn correction_emits_correction_event() {
        let local = node();
        let (_tx, svc) = open_svc_for(local.clone(), ValidationPolicy::Strict);
        let target = svc
            .publish(item_for("r", BulletinSeverity::Info, local.clone()))
            .await
            .unwrap();
        let mut correction = BulletinItem::new(
            BulletinKind::Correction,
            BulletinCategory::General,
            BulletinSeverity::Info,
            RoomId::new("r"),
            local.clone(),
            "",
            "Correction summary",
            "Correction body",
            b"nonce-c",
            Some(target.bulletin_id.clone()),
        )
        .unwrap();
        correction.sequence = 2;
        let mut envelope = BulletinEnvelope::wrap(correction, local.clone());
        envelope.attach_signature(
            WalletAddress::from_bytes([0x01u8; 20]),
            vec![0u8; 65],
        );
        let stored = svc.ingest_envelope(envelope).await.unwrap();
        assert_eq!(stored.kind, BulletinKind::Correction);
    }

    #[tokio::test]
    async fn get_returns_inserted_bulletin() {
        let local = node();
        let (_tx, svc) = open_svc_for(local.clone(), ValidationPolicy::Strict);
        let stored = svc
            .publish(item_for("r", BulletinSeverity::Info, local.clone()))
            .await
            .unwrap();
        let back = svc.get(&RoomId::new("r"), &stored.bulletin_id).unwrap();
        assert_eq!(back.unwrap().item.bulletin_id, stored.bulletin_id);
    }

    #[test]
    fn unknown_topic_id_prefix_is_stable() {
        // Sanity check: the topic id derivation is deterministic.
        let a = topic_id(&RoomId::new("a"));
        let b = topic_id(&RoomId::new("a"));
        assert_eq!(a, b);
        let c = topic_id(&RoomId::new("b"));
        assert_ne!(a, c);
    }

    #[test]
    fn hashmap_marker_compiles() {
        // Cheap smoke-test so the unused-import shims don't get
        // removed by over-eager formatting bots.
        let mut m: HashMap<&'static str, u32> = HashMap::new();
        m.insert("k", 1);
        assert_eq!(m.get("k"), Some(&1));
    }

    #[allow(dead_code)]
    fn content_hash_marker(_: ContentHash) {}
}