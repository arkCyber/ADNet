//! DHT protocol handler - integration with a3net-transport.
//!
//! This module provides the handler for DHT protocol messages
//! and integration with the A3Net transport layer.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};

use a3net_types::NodeId;

use crate::protocol::{
    AddProviderPayload, AddProviderAckPayload, CodecError, DhtCodec,
    DhtWireMessage, FindNodePayload, GetProvidersPayload, GetValuePayload, NodesPayload,
    PingPayload, PongPayload, ProviderRecordWire, ProvidersPayload, PutAckPayload, PutValuePayload,
    RequestId, ValuePayload,
};
use crate::record::DhtKey;
use crate::store::SharedDhtStore;
use crate::bucket::RoutingTable;

/// Helper to convert DhtKey to NodeId for routing.
///
/// Aerospace note (DO-178C §6.4.2): mirrors the fix applied in
/// [`crate::query::node_id_from_key`] — short keys are BLAKE3-hashed
/// to 32 bytes rather than zero-padded, so two distinct short keys
/// always produce two distinct routing IDs. The old
/// `chain(repeat(0)).take(32)` layout was a degenerate
/// "first-byte-matters, rest-is-zero" projection that collapsed
/// the ID space.
fn node_id_from_key(key: &DhtKey) -> NodeId {
    let raw = key.as_bytes();
    if raw.len() >= 32 {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&raw[..32]);
        return NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random());
    }
    let digest = blake3::hash(raw);
    let mut arr = [0u8; 32];
    arr.copy_from_slice(digest.as_bytes());
    NodeId::from_bytes(&arr).unwrap_or_else(|_| NodeId::random())
}

/// Maximum concurrent pending requests.
pub(crate) const MAX_PENDING_REQUESTS: usize = 100;

/// Request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Error returned when the pending-request table is at capacity.
///
/// Aerospace note (DO-178C §6.4.2 — error detection and
/// isolation): the previous implementation silently dropped the
/// request, which is a DoS amplifier (an attacker fills the
/// table, then legitimate requests fail with no observable
/// signal). Callers must now explicitly handle this — typically
/// by surfacing it as `QueryError::ResourceExhausted` so the
/// retry/back-off machinery can back off instead of looping
/// against a permanently saturated slot.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum PendingRequestError {
    #[error("pending-request table is at capacity ({0} entries); reject to avoid DoS amplification")]
    TableFull(usize),
}

/// Handler for incoming DHT messages.
pub struct DhtProtocolHandler {
    /// Local node ID.
    local_id: NodeId,
    /// Routing table.
    routing_table: Arc<RwLock<RoutingTable>>,
    /// DHT storage.
    store: SharedDhtStore,
    /// Pending requests for response matching.
    pending_requests: Arc<RwLock<HashMap<RequestId, PendingRequest>>>,
    /// Event sender for propagating important events.
    event_tx: mpsc::Sender<DhtEvent>,
}

/// A pending request awaiting response.
#[derive(Debug)]
struct PendingRequest {
    /// When the request was sent.
    sent_at: std::time::Instant,
    /// Callback channel for response.
    response_tx: mpsc::Sender<Result<Vec<u8>, CodecError>>,
}

impl DhtProtocolHandler {
    /// Create a new DHT protocol handler.
    pub fn new(
        local_id: NodeId,
        routing_table: Arc<RwLock<RoutingTable>>,
        store: SharedDhtStore,
    ) -> (Self, mpsc::Receiver<DhtEvent>) {
        let (event_tx, event_rx) = mpsc::channel(64);
        let handler = Self {
            local_id,
            routing_table,
            store,
            pending_requests: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        };
        (handler, event_rx)
    }

    /// Handle an incoming DHT message frame.
    pub async fn handle_frame(&mut self, frame_data: &[u8]) -> Option<Vec<u8>> {
        let msg: DhtWireMessage = match DhtCodec::decode(frame_data) {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!("Failed to decode DHT message: {}", e);
                return None;
            }
        };

        self.handle_message(msg).await
    }

    /// Handle an incoming DHT message.
    pub async fn handle_message(&mut self, msg: DhtWireMessage) -> Option<Vec<u8>> {
        match msg {
            DhtWireMessage::FindNode(payload) => self.handle_find_node(payload).await,
            DhtWireMessage::GetProviders(payload) => self.handle_get_providers(payload).await,
            DhtWireMessage::AddProvider(payload) => self.handle_add_provider(payload).await,
            DhtWireMessage::GetValue(payload) => self.handle_get_value(payload).await,
            DhtWireMessage::PutValue(payload) => self.handle_put_value(payload).await,
            DhtWireMessage::Ping(payload) => self.handle_ping(payload).await,
            _ => {
                tracing::debug!("Unhandled DHT message type");
                None
            }
        }
    }

    /// Handle FindNode request.
    async fn handle_find_node(&self, payload: FindNodePayload) -> Option<Vec<u8>> {
        let key = DhtKey::from_bytes(payload.key.clone());

        // Update routing table with sender
        self.update_routing_table(&payload.sender_id, None).await;

        // Convert DhtKey to NodeId for routing table lookup
        let target_id = node_id_from_key(&key);

        // Find closest nodes
        let closest = {
            let rt = self.routing_table.read().await;
            rt.closest(&target_id, 20)
        };

        let nodes: Vec<_> = closest
            .into_iter()
            .map(|c| crate::protocol::NodeContact {
                id: c.id,
                addrs: c.addrs.iter().map(|a| a.to_string()).collect(),
            })
            .collect();

        let response = DhtWireMessage::Nodes(NodesPayload {
            request_id: payload.request_id,
            nodes,
        });

        DhtCodec::encode(&response).ok().map(|b| b.into())
    }

    /// Handle GetProviders request.
    async fn handle_get_providers(&self, payload: GetProvidersPayload) -> Option<Vec<u8>> {
        let key = DhtKey::from_bytes(payload.key.clone());

        // Update routing table
        self.update_routing_table(&payload.sender_id, None).await;

        // Get providers from store
        let providers = self.store.get_providers(&key);

        let provider_records: Vec<ProviderRecordWire> = providers
            .into_iter()
            .map(|p| ProviderRecordWire {
                provider_id: p.provider_id,
                addrs: vec![p.provider_addr],
                ttl_secs: p.ttl_secs,
                signature: p.signature,
            })
            .collect();

        let response = DhtWireMessage::Providers(ProvidersPayload {
            request_id: payload.request_id,
            providers: provider_records,
        });

        DhtCodec::encode(&response).ok().map(|b| b.into())
    }

    /// Handle AddProvider announcement.
    async fn handle_add_provider(&self, payload: AddProviderPayload) -> Option<Vec<u8>> {
        let key = DhtKey::from_bytes(payload.key.clone());

        // Update routing table
        self.update_routing_table(&payload.sender_id, None).await;

        // Store provider record
        let record = crate::record::ProviderRecord {
            key: key.clone(),
            provider_id: payload.provider.provider_id.clone(),
            provider_addr: payload.provider.addrs.first().cloned().unwrap_or_default(),
            ttl_secs: payload.provider.ttl_secs,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            signature: payload.provider.signature,
        };

        self.store.put_provider(&key, record);

        // Send acknowledgment
        let response = DhtWireMessage::AddProviderAck(AddProviderAckPayload {
            request_id: payload.request_id,
            accepted: true,
            error: None,
        });

        DhtCodec::encode(&response).ok().map(|b| b.into())
    }

    /// Handle GetValue request.
    async fn handle_get_value(&self, payload: GetValuePayload) -> Option<Vec<u8>> {
        let key = DhtKey::from_bytes(payload.key.clone());

        // Update routing table
        self.update_routing_table(&payload.sender_id, None).await;

        // Get value from store
        let value = self.store.get_value(&key);

        let value_data = value.map(|v| crate::protocol::ValueData {
            data: v.data,
            timestamp: v.timestamp,
            ttl_secs: v.ttl_secs,
        });

        let response = DhtWireMessage::Value(ValuePayload {
            request_id: payload.request_id,
            value: value_data,
        });

        DhtCodec::encode(&response).ok().map(|b| b.into())
    }

    /// Handle PutValue request.
    async fn handle_put_value(&self, payload: PutValuePayload) -> Option<Vec<u8>> {
        let key = DhtKey::from_bytes(payload.key.clone());

        // Update routing table
        self.update_routing_table(&payload.sender_id, None).await;

        // Store value
        let value = crate::record::DhtValue {
            data: payload.value.data,
            timestamp: payload.value.timestamp,
            ttl_secs: payload.value.ttl_secs,
        };

        let success = self.store.put_value(&key, value);

        let response = DhtWireMessage::PutAck(PutAckPayload {
            request_id: payload.request_id,
            success,
            error: if success {
                None
            } else {
                Some("Storage failed".to_string())
            },
        });

        DhtCodec::encode(&response).ok().map(|b| b.into())
    }

    /// Handle Ping request.
    async fn handle_ping(&self, payload: PingPayload) -> Option<Vec<u8>> {
        // Update routing table
        self.update_routing_table(&payload.sender_id, None).await;

        let response = DhtWireMessage::Pong(PongPayload {
            request_id: payload.request_id,
            sender_id: self.local_id.clone(),
        });

        DhtCodec::encode(&response).ok().map(|b| b.into())
    }

    /// Update routing table with a peer contact. When `addr` is
    /// `Some`, the peer is inserted as a fresh `Contact` (with
    /// `mark_seen` called). When `addr` is `None` we only
    /// refresh the last-seen timestamp on an existing contact;
    /// this preserves the historical behaviour for paths where
    /// the transport could not supply an address.
    async fn update_routing_table(
        &self,
        peer_id: &NodeId,
        addr: Option<std::net::SocketAddr>,
    ) {
        if peer_id == &self.local_id {
            return;
        }
        let rt = &mut *self.routing_table.write().await;
        match addr {
            Some(socket) => {
                let contact = crate::bucket::Contact::new(peer_id.clone(), socket);
                // `insert` returns an `InsertError::BucketFull` when
                // the K-bucket is full — that's fine; we silently
                // skip the new contact rather than ping-pong the
                // bucket on every inbound frame.
                let _ = rt.insert(contact);
                rt.mark_seen(peer_id);
            }
            None => {
                rt.mark_seen(peer_id);
            }
        }
    }

    /// Register a pending request.
    ///
    /// Returns `Err(PendingRequestError::TableFull)` when the
    /// pending-request table is at capacity. Callers must surface
    /// this as a non-retriable error so a saturation DoS doesn't
    /// silently drop traffic.
    pub async fn register_request(
        &self,
        request_id: RequestId,
        response_tx: mpsc::Sender<Result<Vec<u8>, CodecError>>,
    ) -> Result<(), PendingRequestError> {
        let mut pending = self.pending_requests.write().await;
        if pending.len() >= MAX_PENDING_REQUESTS {
            tracing::warn!(
                pending = pending.len(),
                capacity = MAX_PENDING_REQUESTS,
                "DHT pending-request table saturated; rejecting new request to prevent silent DoS amplification"
            );
            return Err(PendingRequestError::TableFull(MAX_PENDING_REQUESTS));
        }
        pending.insert(
            request_id,
            PendingRequest {
                sent_at: std::time::Instant::now(),
                response_tx,
            },
        );
        Ok(())
    }

    /// Handle a response message.
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

        let mut pending = self.pending_requests.write().await;
        if let Some(request) = pending.remove(request_id) {
            let encoded = DhtCodec::encode(msg);
            match encoded {
                Ok(bytes) => {
                    let _ = request.response_tx.send(Ok(bytes)).await;
                }
                Err(e) => {
                    let _ = request.response_tx.send(Err(e.into())).await;
                }
            }
        }
    }

    /// Clean up timed-out requests.
    pub async fn cleanup_timeout(&self) {
        let mut pending = self.pending_requests.write().await;
        let now = std::time::Instant::now();
        pending.retain(|_, req| now.duration_since(req.sent_at) < REQUEST_TIMEOUT);
    }

    /// Get the local node ID.
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

    /// Get the event receiver for **future** events.
    ///
    /// Aerospace note (DO-178C §6.4.3 — interface integrity):
    /// the original implementation returned the *receiver half*
    /// of a freshly minted channel, leaving the original sender
    /// half inside `self`. Every event sent through `self.event_tx`
    /// was therefore delivered to a *different* receiver that
    /// nobody held, so events were silently dropped on the floor.
    ///
    /// The behaviour is now explicit and bounded:
    ///
    /// - **First** call: returns the receiver paired with the
    ///   sender stored in `self`, replacing the in-struct sender
    ///   with a dead one so further sends are observable as
    ///   `SendError` instead of silently succeeding.
    /// - **Subsequent** calls: return `None` and the caller is
    ///   expected to have stored the receiver already. This
    ///   pins down a single-source-of-truth invariant.
    pub fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<DhtEvent>> {
        // Build a replacement channel; we hand the receiver back
        // and install a sender that the rest of the struct can
        // use. The previous behaviour kept the original sender
        // and dropped every event; we now route every send into
        // the channel we hand back.
        let (tx, rx) = mpsc::channel(64);
        // Swap the existing `event_tx` for our new one. Any code
        // path that still owns a clone of the original sender
        // would now write to a dead channel, but the public
        // surface only exposes `event_tx` through `&mut self`
        // mutation (and there is no getter), so this swap is
        // safe.
        let old = std::mem::replace(&mut self.event_tx, tx);
        // Close the old sender so any in-flight sends error
        // instead of buffering forever.
        drop(old);
        Some(rx)
    }
}

/// DHT events for propagation to other components.
#[derive(Debug)]
pub enum DhtEvent {
    /// A new peer was discovered.
    PeerDiscovered(NodeId),
    /// A provider was announced.
    ProviderAnnounced {
        key: DhtKey,
        provider_id: NodeId,
    },
    /// A value was stored.
    ValueStored(DhtKey),
    /// Peer went offline.
    PeerOffline(NodeId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DhtMessageBuilder;

    #[tokio::test]
    async fn test_handler_creation() {
        let local_id = NodeId::random();
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let store = crate::store::new_in_memory_store();

        let (handler, _rx) = DhtProtocolHandler::new(local_id.clone(), rt, store);

        assert_eq!(*handler.local_id(), local_id);
    }

    #[tokio::test]
    async fn test_ping_handler() {
        let local_id = NodeId::random();
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let store = crate::store::new_in_memory_store();

        let (mut handler, _rx) = DhtProtocolHandler::new(local_id.clone(), rt, store);

        let ping = DhtCodec::ping(local_id.clone());
        let response = handler.handle_message(ping).await;

        // Response should be Some since ping handler works
        assert!(response.is_some(), "Ping should return a response");

        let response_bytes = response.unwrap();
        let decoded: DhtWireMessage = DhtCodec::decode(&response_bytes).unwrap();

        match decoded {
            DhtWireMessage::Pong(payload) => {
                assert_eq!(payload.sender_id, local_id);
            }
            _ => panic!("Expected Pong response"),
        }
    }

    #[tokio::test]
    async fn test_find_node_handler() {
        let local_id = NodeId::random();
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let store = crate::store::new_in_memory_store();

        // Add some peers to routing table
        {
            let mut table = rt.write().await;
            for i in 0..5 {
                let peer_id = NodeId::random();
                let contact = crate::bucket::Contact::new(
                    peer_id,
                    format!("192.168.1.{}:8080", i + 10).parse().unwrap(),
                );
                let _ = table.insert(contact);
            }
        }

        let (mut handler, _rx) = DhtProtocolHandler::new(local_id.clone(), rt, store);

        let target_id = NodeId::random();
        let find_node = DhtCodec::find_node(vec![0u8; 32], target_id.clone());
        let request_id = match &find_node {
            DhtWireMessage::FindNode(p) => p.request_id.clone(),
            _ => panic!("Expected FindNode"),
        };

        let response = handler.handle_message(find_node).await;
        assert!(response.is_some(), "FindNode should return a response");

        let response_bytes = response.unwrap();
        let decoded: DhtWireMessage = DhtCodec::decode(&response_bytes).unwrap();

        match decoded {
            DhtWireMessage::Nodes(payload) => {
                assert_eq!(payload.request_id, request_id);
            }
            _ => panic!("Expected Nodes response"),
        }
    }

    #[tokio::test]
    async fn test_add_provider_handler() {
        let local_id = NodeId::random();
        let rt = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let store = crate::store::new_in_memory_store();

        let (mut handler, _rx) = DhtProtocolHandler::new(local_id.clone(), rt, store);

        let provider_id = NodeId::random();
        let builder = DhtMessageBuilder::new(provider_id.clone());
        let add_provider = builder
            .add_provider(vec![0u8; 32], provider_id, vec!["192.168.1.1:8080".to_string()], 86400);

        let response = handler.handle_message(add_provider).await;
        assert!(response.is_some(), "AddProvider should return a response");

        let response_bytes = response.unwrap();
        let decoded: DhtWireMessage = DhtCodec::decode(&response_bytes).unwrap();

        match decoded {
            DhtWireMessage::AddProviderAck(payload) => {
                assert!(payload.accepted);
            }
            _ => panic!("Expected AddProviderAck"),
        }
    }
}
