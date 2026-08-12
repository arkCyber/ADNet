//! DHT transport adapter for integrating with adnet-transport.
//!
//! This module provides the integration layer between the DHT and the
//! ADNet transport layer, enabling DHT messages to be sent over the
//! QUIC/network transport.

use std::sync::Arc;
use std::time::Duration;

use adnet_types::{NodeId, NodeAddr};
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::bucket::RoutingTable;
use crate::protocol::{
    CodecError, DhtCodec, DhtMessageBuilder, DhtWireMessage,
    GetProvidersPayload, NodesPayload, ProvidersPayload, RequestId,
};
use crate::query::QueryError;

/// ALPN identifier for DHT protocol.
/// Matches the pattern used by adnet-transport.
pub const DHT_ALPN: &[u8] = b"adnet/dht/1";

/// DHT message timeout.
const DHT_TIMEOUT: Duration = Duration::from_secs(5);

/// Adapter that bridges DHT protocol to adnet-transport.
pub struct DhtTransportAdapter {
    transport: Arc<dyn TransportBridge>,
    routing_table: Arc<RwLock<RoutingTable>>,
    pending: Arc<RwLock<std::collections::HashMap<RequestId, tokio::sync::oneshot::Sender<Result<Vec<u8>, CodecError>>>>>,
}

/// Trait for transport operations needed by DHT.
#[async_trait]
pub trait TransportBridge: Send + Sync {
    async fn dial_and_send(&self, peer: &NodeId, msg: Vec<u8>) -> Result<(), QueryError>;
    fn local_node_id(&self) -> &NodeId;
    async fn get_peer_addr(&self, peer: &NodeId) -> Option<NodeAddr>;
}

impl DhtTransportAdapter {
    /// Create a new DHT transport adapter.
    pub fn new(
        transport: Arc<dyn TransportBridge>,
        routing_table: Arc<RwLock<RoutingTable>>,
    ) -> Self {
        Self {
            transport,
            routing_table,
            pending: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Send a FindNode query to a peer.
    pub async fn find_node(&self, peer: &NodeId, key: Vec<u8>) -> Result<NodesPayload, QueryError> {
        let msg = DhtMessageBuilder::new(self.transport.local_node_id().clone())
            .find_node(key);

        let request_id = match &msg {
            DhtWireMessage::FindNode(p) => p.request_id.clone(),
            _ => return Err(QueryError::InvalidResponse),
        };

        self.send_message_and_wait_nodes(peer, msg, request_id).await
    }

    /// Send a GetProviders query to a peer.
    pub async fn get_providers(&self, peer: &NodeId, key: Vec<u8>) -> Result<ProvidersPayload, QueryError> {
        let msg = DhtMessageBuilder::new(self.transport.local_node_id().clone())
            .get_providers(key);

        let request_id = match &msg {
            DhtWireMessage::GetProviders(p) => p.request_id.clone(),
            _ => return Err(QueryError::InvalidResponse),
        };

        self.send_message_and_wait_providers(peer, msg, request_id).await
    }

    /// Announce that we provide content to a peer.
    pub async fn announce_provider(
        &self,
        peer: &NodeId,
        key: Vec<u8>,
        ttl_secs: u64,
    ) -> Result<(), QueryError> {
        let msg = DhtMessageBuilder::new(self.transport.local_node_id().clone())
            .add_provider(
                key,
                self.transport.local_node_id().clone(),
                vec!["127.0.0.1:0".to_string()],
                ttl_secs,
            );

        let bytes = DhtCodec::encode(&msg)
            .map_err(|e| QueryError::Network(e.to_string()))?;

        self.transport.dial_and_send(peer, bytes).await
    }

    /// Ping a peer to check liveness.
    pub async fn ping(&self, peer: &NodeId) -> Result<(), QueryError> {
        let msg = DhtMessageBuilder::new(self.transport.local_node_id().clone()).ping();

        let bytes = DhtCodec::encode(&msg)
            .map_err(|e| QueryError::Network(e.to_string()))?;

        self.transport.dial_and_send(peer, bytes).await
    }

    /// Handle an incoming DHT message.
    pub async fn handle_message(&self, peer_id: &NodeId, data: &[u8]) -> Result<Option<Vec<u8>>, CodecError> {
        let msg = DhtCodec::decode(data)?;

        match msg {
            DhtWireMessage::FindNode(payload) => {
                self.update_routing(peer_id).await;
                let target_id = self.key_to_node_id(&payload.key);
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

                Ok(Some(DhtCodec::encode(&response)?))
            }

            DhtWireMessage::GetProviders(payload) => {
                self.update_routing(peer_id).await;
                let response = DhtWireMessage::Providers(ProvidersPayload {
                    request_id: payload.request_id,
                    providers: Vec::new(),
                });
                Ok(Some(DhtCodec::encode(&response)?))
            }

            DhtWireMessage::Ping(payload) => {
                let response = DhtWireMessage::Pong(crate::protocol::PongPayload {
                    request_id: payload.request_id,
                    sender_id: self.transport.local_node_id().clone(),
                });
                Ok(Some(DhtCodec::encode(&response)?))
            }

            _ => Ok(None),
        }
    }

    async fn update_routing(&self, peer_id: &NodeId) {
        if let Some(addr) = self.transport.get_peer_addr(peer_id).await {
            if let Some(endpoint) = &addr.direct {
                // Parse the endpoint string (format: "host:port")
                if let Ok(socket_addr) = endpoint.to_string().parse::<std::net::SocketAddr>() {
                    let contact = crate::bucket::Contact::new(peer_id.clone(), socket_addr);
                    let mut rt = self.routing_table.write().await;
                    let _ = rt.insert(contact);
                }
            }
        }
    }

    fn key_to_node_id(&self, key: &[u8]) -> NodeId {
        let mut arr = [0u8; 32];
        for (i, &b) in key.iter().enumerate() {
            if i >= 32 { break; }
            arr[i] = b;
        }
        NodeId::from_bytes(&arr).unwrap_or_else(|_| self.transport.local_node_id().clone())
    }

    async fn send_message_and_wait_nodes(
        &self,
        peer: &NodeId,
        msg: DhtWireMessage,
        request_id: RequestId,
    ) -> Result<NodesPayload, QueryError> {
        let bytes = DhtCodec::encode(&msg)
            .map_err(|e| QueryError::Network(e.to_string()))?;

        let (tx, mut rx) = tokio::sync::oneshot::channel();

        {
            let mut pending = self.pending.write().await;
            pending.insert(request_id.clone(), tx);
        }

        self.transport.dial_and_send(peer, bytes).await?;

        match tokio::time::timeout(DHT_TIMEOUT, rx).await {
            Ok(Ok(Ok(bytes))) => {
                let msg = DhtCodec::decode(&bytes)
                    .map_err(|_| QueryError::InvalidResponse)?;
                match msg {
                    DhtWireMessage::Nodes(payload) => Ok(payload),
                    _ => Err(QueryError::InvalidResponse),
                }
            }
            Ok(Ok(Err(_))) => Err(QueryError::InvalidResponse),
            Ok(Err(_)) => Err(QueryError::Timeout),
            Err(_) => Err(QueryError::Timeout),
        }
    }

    async fn send_message_and_wait_providers(
        &self,
        peer: &NodeId,
        msg: DhtWireMessage,
        request_id: RequestId,
    ) -> Result<ProvidersPayload, QueryError> {
        let bytes = DhtCodec::encode(&msg)
            .map_err(|e| QueryError::Network(e.to_string()))?;

        let (tx, mut rx) = tokio::sync::oneshot::channel();

        {
            let mut pending = self.pending.write().await;
            pending.insert(request_id.clone(), tx);
        }

        self.transport.dial_and_send(peer, bytes).await?;

        match tokio::time::timeout(DHT_TIMEOUT, rx).await {
            Ok(Ok(Ok(bytes))) => {
                let msg = DhtCodec::decode(&bytes)
                    .map_err(|_| QueryError::InvalidResponse)?;
                match msg {
                    DhtWireMessage::Providers(payload) => Ok(payload),
                    _ => Err(QueryError::InvalidResponse),
                }
            }
            Ok(Ok(Err(_))) => Err(QueryError::InvalidResponse),
            Ok(Err(_)) => Err(QueryError::Timeout),
            Err(_) => Err(QueryError::Timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dht_alpn() {
        assert_eq!(DHT_ALPN, b"adnet/dht/1");
    }
}
