//! DHT network sender - sending DHT messages over the transport layer.
//!
//! This module provides the network sender for DHT queries,
//! integrating with adnet-transport's QUIC connection layer.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex as AsyncMutex, RwLock};
use tokio::time::{sleep, timeout};

use adnet_types::NodeId;

use crate::bucket::{Contact, RoutingTable};
use crate::protocol::{
    CodecError, DhtCodec, DhtMessageBuilder, DhtWireMessage, GetProvidersPayload,
    NodesPayload, ProvidersPayload, RequestId,
};
use crate::query::{DhtMessageSender, QueryError};
use crate::record::DhtKey;
use crate::retry::{is_transient, PeerFailureTracker, RetryPolicy};

/// Default query timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default parallelism (alpha in Kademlia).
const DEFAULT_ALPHA: usize = 3;

/// DHT network sender for sending queries to peers.
pub struct DhtNetworkSender {
    /// Local node ID.
    local_id: NodeId,
    /// Shared transport for sending messages.
    transport: Arc<dyn TransportDhtSender>,
    /// Routing table reference.
    routing_table: Arc<RwLock<RoutingTable>>,
    /// Pending requests.
    pending: Arc<RwLock<HashMap<RequestId, mpsc::Sender<Result<Vec<u8>, CodecError>>>>>,
    /// Per-peer failure tracker that gates retries to flapping
    /// peers. Shared behind an async mutex so the read-mostly hot
    /// path (`try_acquire` + `record_*`) doesn't need a sync guard.
    peer_failures: Arc<AsyncMutex<PeerFailureTracker>>,
    /// Retry policy. The `max_attempts` field includes the first
    /// try, so a policy with `max_attempts = 3` will retry twice.
    retry_policy: RetryPolicy,
}

/// Trait for the underlying transport to send DHT messages.
#[async_trait::async_trait]
pub trait TransportDhtSender: Send + Sync {
    /// Send raw bytes to a peer.
    async fn send_to(&self, peer: &NodeId, data: &[u8]) -> Result<(), QueryError>;

    /// Get addresses for a peer.
    async fn get_peer_addr(&self, peer: &NodeId) -> Option<String>;
}

impl DhtNetworkSender {
    /// Create a new DHT network sender with the default
    /// [`RetryPolicy`].
    pub fn new(
        local_id: NodeId,
        transport: Arc<dyn TransportDhtSender>,
        routing_table: Arc<RwLock<RoutingTable>>,
    ) -> Self {
        Self::with_policy(local_id, transport, routing_table, RetryPolicy::default())
    }

    /// Create a new DHT network sender with a custom retry policy.
    ///
    /// The policy governs transparent retries on transient errors
    /// (`Timeout`, `Network`) and per-peer cooldowns for flapping
    /// peers. See [`RetryPolicy`] for the exact knobs.
    pub fn with_policy(
        local_id: NodeId,
        transport: Arc<dyn TransportDhtSender>,
        routing_table: Arc<RwLock<RoutingTable>>,
        retry_policy: RetryPolicy,
    ) -> Self {
        let peer_failures = PeerFailureTracker::new(retry_policy.clone());
        Self {
            local_id,
            transport,
            routing_table,
            pending: Arc::new(RwLock::new(HashMap::new())),
            peer_failures: Arc::new(AsyncMutex::new(peer_failures)),
            retry_policy,
        }
    }

    /// Current retry policy.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry_policy
    }

    /// Inspect how many distinct peers the failure tracker knows
    /// about. Exposed for tests and metrics.
    pub async fn tracked_peers(&self) -> usize {
        self.peer_failures.lock().await.tracked_peers()
    }

    /// Send a raw message to a peer.
    pub async fn send_raw(&self, peer: &NodeId, msg: DhtWireMessage) -> Result<(), QueryError> {
        let bytes = DhtCodec::encode(&msg)
            .map_err(|e| QueryError::Network(e.to_string()))?;

        self.transport.send_to(peer, &bytes).await
    }

    /// Send a FindNode query and wait for response.
    ///
    /// Transient errors are retried per the configured
    /// [`RetryPolicy`] with jittered exponential backoff. Permanent
    /// errors (`PeerNotFound`, `InvalidResponse`) are surfaced
    /// immediately. Per-peer cooldowns gate retries so a flapping
    /// peer can't soak up the entire retry budget.
    pub async fn find_node(&self, peer: &NodeId, key: &DhtKey) -> Result<NodesPayload, QueryError> {
        let mut last_err: Option<QueryError> = None;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            // Reserve a slot against the failure tracker. If the
            // peer is in cooldown we surface a Timeout so callers
            // can move on to the next peer; we don't burn a retry
            // budget waiting for the cooldown to lift.
            let acquired = {
                let mut tracker = self.peer_failures.lock().await;
                match tracker.try_acquire(peer) {
                    Ok(_) => true,
                    Err(remaining) => {
                        tracing::trace!(
                            "DHT find_node peer {} in cooldown for {:?}; skipping",
                            peer.short(),
                            remaining
                        );
                        false
                    }
                }
            };
            if !acquired {
                return Err(last_err.unwrap_or(QueryError::Timeout));
            }
            let msg = DhtMessageBuilder::new(self.local_id.clone())
                .find_node(key.as_bytes().to_vec());
            let request_id = match &msg {
                DhtWireMessage::FindNode(p) => p.request_id.clone(),
                _ => return Err(QueryError::InvalidResponse),
            };

            let outcome: Result<NodesPayload, QueryError> = async {
                self.send_raw(peer, msg).await?;
                self.wait_for_response(request_id).await
            }
            .await;

            match outcome {
                Ok(payload) => {
                    let mut tracker = self.peer_failures.lock().await;
                    tracker.record_success(peer);
                    return Ok(payload);
                }
                Err(e) => {
                    let transient = is_transient(&e);
                    {
                        let mut tracker = self.peer_failures.lock().await;
                        tracker.record_failure(peer);
                    }
                    tracing::trace!(
                        "DHT find_node peer {} attempt {attempt} failed: {e} (transient={transient})",
                        peer.short()
                    );
                    if !transient {
                        return Err(e);
                    }
                    last_err = Some(e);
                    if !self.retry_policy.should_retry(attempt) {
                        return Err(last_err.unwrap_or(QueryError::Timeout));
                    }
                    let backoff = self.retry_policy.backoff_for(attempt);
                    sleep(backoff).await;
                }
            }
        }
    }

    /// Send a GetProviders query and wait for response.
    ///
    /// Retry semantics mirror [`find_node`](Self::find_node).
    pub async fn get_providers(
        &self,
        peer: &NodeId,
        key: &DhtKey,
    ) -> Result<ProvidersPayload, QueryError> {
        let mut last_err: Option<QueryError> = None;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let acquired = {
                let mut tracker = self.peer_failures.lock().await;
                match tracker.try_acquire(peer) {
                    Ok(_) => true,
                    Err(remaining) => {
                        tracing::trace!(
                            "DHT get_providers peer {} in cooldown for {:?}; skipping",
                            peer.short(),
                            remaining
                        );
                        false
                    }
                }
            };
            if !acquired {
                return Err(last_err.unwrap_or(QueryError::Timeout));
            }
            let msg = DhtMessageBuilder::new(self.local_id.clone())
                .get_providers(key.as_bytes().to_vec());
            let request_id = match &msg {
                DhtWireMessage::GetProviders(p) => p.request_id.clone(),
                _ => return Err(QueryError::InvalidResponse),
            };

            let outcome: Result<ProvidersPayload, QueryError> = async {
                self.send_raw(peer, msg).await?;
                self.wait_for_providers(request_id).await
            }
            .await;

            match outcome {
                Ok(payload) => {
                    let mut tracker = self.peer_failures.lock().await;
                    tracker.record_success(peer);
                    return Ok(payload);
                }
                Err(e) => {
                    let transient = is_transient(&e);
                    {
                        let mut tracker = self.peer_failures.lock().await;
                        tracker.record_failure(peer);
                    }
                    tracing::trace!(
                        "DHT get_providers peer {} attempt {attempt} failed: {e} (transient={transient})",
                        peer.short()
                    );
                    if !transient {
                        return Err(e);
                    }
                    last_err = Some(e);
                    if !self.retry_policy.should_retry(attempt) {
                        return Err(last_err.unwrap_or(QueryError::Timeout));
                    }
                    let backoff = self.retry_policy.backoff_for(attempt);
                    sleep(backoff).await;
                }
            }
        }
    }

    /// Announce that we provide content to a peer.
    pub async fn announce_provider(
        &self,
        peer: &NodeId,
        key: &DhtKey,
        ttl_secs: u64,
    ) -> Result<(), QueryError> {
        let msg = DhtMessageBuilder::new(self.local_id.clone())
            .add_provider(
                key.as_bytes().to_vec(),
                self.local_id.clone(),
                vec!["127.0.0.1:0".to_string()], // TODO: Get actual address
                ttl_secs,
            );

        self.send_raw(peer, msg).await
    }

    /// Announce a pre-built provider record to a peer. Used by
    /// the Kademlia "publish" half of `announce_content` — the
    /// caller has already populated `record.provider_addr` with
    /// the routable address and the record is signed.
    pub async fn send_add_provider(
        &self,
        peer: &NodeId,
        key: &DhtKey,
        record: &crate::record::ProviderRecord,
    ) -> Result<(), QueryError> {
        let msg = DhtMessageBuilder::new(self.local_id.clone()).add_provider(
            key.as_bytes().to_vec(),
            record.provider_id.clone(),
            vec![record.provider_addr.clone()],
            record.ttl_secs,
        );
        self.send_raw(peer, msg).await
    }

    /// Ping a peer.
    pub async fn ping(&self, peer: &NodeId) -> Result<(), QueryError> {
        let msg = DhtMessageBuilder::new(self.local_id.clone()).ping();
        self.send_raw(peer, msg).await
    }

    /// Wait for a response to a request.
    async fn wait_for_response(&self, request_id: RequestId) -> Result<NodesPayload, QueryError> {
        // Create a channel to receive the response
        let (tx, mut rx) = mpsc::channel(1);

        {
            let mut pending = self.pending.write().await;
            pending.insert(request_id.clone(), tx);
        }

        // Wait for response with timeout
        let bytes = match timeout(DEFAULT_TIMEOUT, rx.recv()).await {
            Ok(Some(Ok(b))) => b,
            Ok(Some(Err(e))) => return Err(QueryError::Network(e.to_string())),
            Ok(None) => return Err(QueryError::Timeout),
            Err(_) => return Err(QueryError::Timeout),
        };
        let msg = DhtCodec::decode(&bytes)
            .map_err(|e| QueryError::InvalidResponse)?;
        match msg {
            DhtWireMessage::Nodes(payload) => Ok(payload),
            _ => Err(QueryError::InvalidResponse),
        }
    }

    /// Wait for a providers response.
    async fn wait_for_providers(&self, request_id: RequestId) -> Result<ProvidersPayload, QueryError> {
        let (tx, mut rx) = mpsc::channel(1);

        {
            let mut pending = self.pending.write().await;
            pending.insert(request_id.clone(), tx);
        }

        let bytes = match timeout(DEFAULT_TIMEOUT, rx.recv()).await {
            Ok(Some(Ok(b))) => b,
            Ok(Some(Err(e))) => return Err(QueryError::Network(e.to_string())),
            Ok(None) => return Err(QueryError::Timeout),
            Err(_) => return Err(QueryError::Timeout),
        };
        let msg = DhtCodec::decode(&bytes)
            .map_err(|e| QueryError::InvalidResponse)?;
        match msg {
            DhtWireMessage::Providers(payload) => Ok(payload),
            _ => Err(QueryError::InvalidResponse),
        }
    }

    /// Handle an incoming response message.
    pub async fn handle_response(&self, msg: &DhtWireMessage) {
        let request_id = match msg {
            DhtWireMessage::Nodes(p) => &p.request_id,
            DhtWireMessage::Providers(p) => &p.request_id,
            DhtWireMessage::AddProviderAck(p) => &p.request_id,
            DhtWireMessage::Value(p) => &p.request_id,
            DhtWireMessage::PutAck(p) => &p.request_id,
            DhtWireMessage::Pong(p) => &p.request_id,
            _ => return,
        };

        let mut pending = self.pending.write().await;
        if let Some(tx) = pending.remove(request_id) {
            let bytes = DhtCodec::encode(msg);
            match bytes {
                Ok(b) => {
                    let _ = tx.send(Ok(b)).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(e.into())).await;
                }
            }
        }
    }

    /// Get the local node ID.
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }
}

/// Implement DhtMessageSender for DhtNetworkSender.
///
/// Both `send_find_node` and `send_get_providers` route through
/// [`DhtNetworkSender::find_node`] / [`DhtNetworkSender::get_providers`]
/// so that the request id is registered in the shared `pending` map
/// *before* the bytes go on the wire and the response is correlated
/// back via [`DhtNetworkSender::handle_response`].
#[async_trait::async_trait]
impl DhtMessageSender for DhtNetworkSender {
    async fn send_find_node(
        &self,
        peer: &Contact,
        key: &DhtKey,
        request_id: &str,
    ) -> Result<DhtWireMessage, QueryError> {
        // Re-issue the call with our own request id so the receiver
        // can match it. `find_node` already wires the request-id →
        // oneshot map; we then forward the resolved payload back.
        let _ = request_id; // tracked inside `find_node`
        let payload = self.find_node(&peer.id, key).await?;
        Ok(DhtWireMessage::Nodes(payload))
    }

    async fn send_get_providers(
        &self,
        peer: &Contact,
        key: &DhtKey,
        request_id: &str,
    ) -> Result<DhtWireMessage, QueryError> {
        let _ = request_id;
        let payload = self.get_providers(&peer.id, key).await?;
        Ok(DhtWireMessage::Providers(payload))
    }

    async fn send_add_provider(
        &self,
        peer: &Contact,
        key: &DhtKey,
        record: &crate::record::ProviderRecord,
    ) -> Result<(), QueryError> {
        let msg = DhtMessageBuilder::new(self.local_id.clone())
            .add_provider(
                key.as_bytes().to_vec(),
                record.provider_id.clone(),
                vec![record.provider_addr.clone()],
                record.ttl_secs,
            );

        self.send_raw(&peer.id, msg).await
    }

    async fn send_put_value(
        &self,
        peer: &Contact,
        key: &DhtKey,
        value: &crate::record::DhtValue,
        request_id: &str,
    ) -> Result<(), QueryError> {
        let msg = DhtWireMessage::PutValue(crate::protocol::PutValuePayload {
            key: key.as_bytes().to_vec(),
            value: crate::protocol::ValueData {
                data: value.data.clone(),
                timestamp: value.timestamp,
                ttl_secs: value.ttl_secs,
            },
            request_id: RequestId(request_id.to_string()),
            sender_id: self.local_id.clone(),
        });

        self.send_raw(&peer.id, msg).await
    }
}

/// Mock transport sender for testing.
#[cfg(test)]
pub struct MockTransportSender {
    pub sent_messages: Arc<RwLock<Vec<(NodeId, Vec<u8>)>>>,
}

#[cfg(test)]
impl MockTransportSender {
    pub fn new() -> Self {
        Self {
            sent_messages: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl TransportDhtSender for MockTransportSender {
    async fn send_to(&self, peer: &NodeId, data: &[u8]) -> Result<(), QueryError> {
        let mut messages = self.sent_messages.write().await;
        messages.push((peer.clone(), data.to_vec()));
        Ok(())
    }

    async fn get_peer_addr(&self, _peer: &NodeId) -> Option<String> {
        Some("127.0.0.1:8080".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bucket::RoutingTable;

    #[tokio::test]
    async fn test_send_raw_message() {
        let local_id = NodeId::random();
        let transport = Arc::new(MockTransportSender::new());
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));

        let sender = DhtNetworkSender::new(local_id.clone(), transport.clone(), rt);

        let peer_id = NodeId::random();
        let msg = DhtCodec::ping(local_id.clone());

        sender.send_raw(&peer_id, msg).await.unwrap();

        let messages = transport.sent_messages.read().await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, peer_id);
    }

    #[tokio::test]
    async fn test_ping() {
        let local_id = NodeId::random();
        let transport = Arc::new(MockTransportSender::new());
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));

        let sender = DhtNetworkSender::new(local_id.clone(), transport.clone(), rt);

        let peer_id = NodeId::random();
        sender.ping(&peer_id).await.unwrap();

        let messages = transport.sent_messages.read().await;
        assert_eq!(messages.len(), 1);

        // Decode and verify it's a ping
        let decoded = DhtCodec::decode(&messages[0].1).unwrap();
        assert!(matches!(decoded, DhtWireMessage::Ping(_)));
    }

    /// Flaky transport that returns `Network` for the first
    /// `fail_count` sends then succeeds (records the message).
    #[derive(Clone)]
    struct FlakyTransportSender {
        sent: Arc<RwLock<Vec<(NodeId, Vec<u8>)>>>,
        fail_count: Arc<RwLock<u32>>,
        send_to_calls: Arc<RwLock<u32>>,
        fail_target: u32,
    }

    #[async_trait::async_trait]
    impl TransportDhtSender for FlakyTransportSender {
        async fn send_to(&self, peer: &NodeId, data: &[u8]) -> Result<(), QueryError> {
            *self.send_to_calls.write().await += 1;
            let mut remaining = self.fail_count.write().await;
            if *remaining > 0 {
                *remaining -= 1;
                return Err(QueryError::Network("simulated failure".into()));
            }
            self.sent.write().await.push((peer.clone(), data.to_vec()));
            Ok(())
        }
        async fn get_peer_addr(&self, _peer: &NodeId) -> Option<String> {
            Some("127.0.0.1:8080".to_string())
        }
    }

    /// Build a sender whose retries complete fast — no real
    /// backoff sleeps in unit tests.
    fn fast_retry_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            backoff_multiplier: 2.0,
            jitter_ratio: 0.0,
            peer_cooldown_threshold: u32::MAX, // disable cooldown for unit tests
            peer_cooldown_min: Duration::ZERO,
            peer_cooldown_max: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn retries_then_succeeds_on_transient_network_error() {
        let local_id = NodeId::random();
        let peer = NodeId::random();
        let transport = Arc::new(FlakyTransportSender {
            sent: Arc::new(RwLock::new(Vec::new())),
            fail_count: Arc::new(RwLock::new(2)),
            send_to_calls: Arc::new(RwLock::new(0)),
            fail_target: 2,
        });
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let sender = Arc::new(DhtNetworkSender::with_policy(
            local_id.clone(),
            transport.clone() as Arc<dyn TransportDhtSender>,
            rt,
            fast_retry_policy(4),
        ));

        let key = DhtKey::from_bytes(b"retry-key".to_vec());
        let payload = sender.find_node(&peer, &key).await.unwrap();

        // 2 fails + 1 success → 3 sends total, 1 recorded as success.
        assert!(payload.nodes.is_empty());
        let sent = transport.sent.read().await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, peer);
        // Success should have cleared the failure record so
        // tracker is back to empty.
        assert_eq!(sender.tracked_peers().await, 0);
    }

    #[tokio::test]
    async fn retries_give_up_after_max_attempts() {
        let local_id = NodeId::random();
        let peer = NodeId::random();
        let transport = Arc::new(FlakyTransportSender {
            sent: Arc::new(RwLock::new(Vec::new())),
            fail_count: Arc::new(RwLock::new(u32::MAX)),
            send_to_calls: Arc::new(RwLock::new(0)),
            fail_target: u32::MAX,
        });
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let sender = DhtNetworkSender::with_policy(
            local_id.clone(),
            transport.clone() as Arc<dyn TransportDhtSender>,
            rt,
            fast_retry_policy(3),
        );

        let key = DhtKey::from_bytes(b"retry-key".to_vec());
        let err = sender.find_node(&peer, &key).await.unwrap_err();
        assert!(matches!(err, QueryError::Network(_)));
        // 3 attempts → transport.send_to called 3 times, none recorded.
        let sent = transport.sent.read().await;
        assert_eq!(sent.len(), 0);
        // send_to must have been hit exactly max_attempts times.
        assert_eq!(*transport.send_to_calls.read().await, 3);
    }

    #[tokio::test]
    async fn permanent_errors_are_not_retried() {
        // Drive `find_node` with a request, then push a
        // wrong-typed response through `handle_response`. The
        // inner decode produces `InvalidResponse`, which is not
        // transient → the loop returns immediately on the first
        // attempt with no retry.
        let local_id = NodeId::random();
        let peer = NodeId::random();
        let transport = Arc::new(MockTransportSender::new());
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let sender = Arc::new(DhtNetworkSender::with_policy(
            local_id.clone(),
            transport.clone() as Arc<dyn TransportDhtSender>,
            rt,
            fast_retry_policy(5),
        ));

        // Build the FindNode request directly so we have its
        // request_id, then race the send against handle_response.
        let key = DhtKey::from_bytes(b"perm-error".to_vec());
        let builder = DhtMessageBuilder::new(local_id.clone()).find_node(key.as_bytes().to_vec());
        let request_id = match &builder {
            DhtWireMessage::FindNode(p) => p.request_id.clone(),
            _ => unreachable!(),
        };
        let _ = builder; // silence unused

        // Spawn the find_node in the background — it will block
        // waiting for a response that we deliberately never send.
        let sender_for_call = sender.clone();
        let key_for_call = key.clone();
        let peer_for_call = peer.clone();
        let call = tokio::spawn(async move {
            sender_for_call.find_node(&peer_for_call, &key_for_call).await
        });

        // Give the call time to register the pending request id.
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Push a wrong-typed response: a `Pong` keyed on the same
        // request_id. `wait_for_response` decodes it, finds it
        // isn't a `Nodes` payload, and returns `InvalidResponse`.
        // That should not be retried — `is_transient` filters it
        // out.
        let wrong = DhtWireMessage::Pong(crate::protocol::PongPayload {
            request_id,
            sender_id: local_id.clone(),
        });
        sender.handle_response(&wrong).await;

        let result = call.await.unwrap();
        assert!(matches!(result, Err(QueryError::InvalidResponse)));

        // Tracker's record_success was NOT called (we never
        // succeeded), but record_failure was — so the peer IS
        // tracked. The retry loop ran exactly once (no retries).
        // The transport should have sent exactly one frame.
        let sent = transport.sent_messages.read().await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sender.tracked_peers().await, 1);
    }

    #[tokio::test]
    async fn per_peer_cooldown_kicks_in_after_threshold() {
        // Use a tiny cooldown so the test can observe it.
        let mut policy = fast_retry_policy(2);
        policy.peer_cooldown_threshold = 1;
        policy.peer_cooldown_min = Duration::from_secs(60);
        policy.peer_cooldown_max = Duration::from_secs(60);

        let local_id = NodeId::random();
        let peer = NodeId::random();
        let transport = Arc::new(FlakyTransportSender {
            sent: Arc::new(RwLock::new(Vec::new())),
            fail_count: Arc::new(RwLock::new(u32::MAX)),
            send_to_calls: Arc::new(RwLock::new(0)),
            fail_target: u32::MAX,
        });
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let sender = Arc::new(DhtNetworkSender::with_policy(
            local_id,
            transport as Arc<dyn TransportDhtSender>,
            rt,
            policy,
        ));

        let key = DhtKey::from_bytes(b"cooldown-key".to_vec());
        // First call: 2 attempts (max_attempts=2), both fail,
        // peer crosses cooldown threshold.
        let _ = sender.find_node(&peer, &key).await;
        // Tracker should know about the peer (>= 1 failure) and
        // have it cooled down.
        assert!(sender.tracked_peers().await >= 1);
        // Second call: peer is in cooldown, so the very first
        // try_acquire returns Err and find_node bails out
        // immediately with the last error (Timeout, because the
        // last_err is set to Timeout by the bail-out path).
        let res = sender.find_node(&peer, &key).await;
        assert!(matches!(res, Err(QueryError::Timeout)));
    }
}
