//! DHT protocol integration for adnet-transport.
//!
//! This module provides the integration layer between the DHT protocol
//! and the ADNet transport layer, enabling DHT messages to be sent
//! and received over QUIC connections.

use std::sync::Arc;

use adnet_types::NodeId;
use adnet_dht::{
    DhtCodec, DhtProtocolHandler, DhtWireMessage, RoutingTable,
    SharedDhtStore, new_in_memory_store,
};
use tokio::sync::RwLock;

/// ALPN for DHT protocol.
pub const DHT_ALPN: &[u8] = b"adnet/dht/1";

/// DHT integration context for a transport endpoint.
pub struct DhtTransportIntegration {
    /// Local node ID.
    local_id: NodeId,
    /// Routing table.
    routing_table: Arc<RwLock<RoutingTable>>,
    /// DHT storage.
    store: SharedDhtStore,
    /// Protocol handler.
    handler: Arc<RwLock<DhtProtocolHandler>>,
}

impl DhtTransportIntegration {
    /// Create a new DHT transport integration.
    pub fn new(local_id: NodeId) -> Self {
        let routing_table = Arc::new(RwLock::new(RoutingTable::new(local_id.clone())));
        let store = new_in_memory_store();

        let (handler, _) = DhtProtocolHandler::new(
            local_id.clone(),
            routing_table.clone(),
            store.clone(),
        );

        Self {
            local_id,
            routing_table,
            store,
            handler: Arc::new(RwLock::new(handler)),
        }
    }

    /// Create with custom routing table and store.
    pub fn with_components(
        local_id: NodeId,
        routing_table: Arc<RwLock<RoutingTable>>,
        store: SharedDhtStore,
    ) -> Self {
        let (handler, _) = DhtProtocolHandler::new(
            local_id.clone(),
            routing_table.clone(),
            store.clone(),
        );

        Self {
            local_id,
            routing_table,
            store,
            handler: Arc::new(RwLock::new(handler)),
        }
    }

    /// Handle an incoming DHT message frame.
    pub async fn handle_frame(&self, frame_data: &[u8]) -> Option<Vec<u8>> {
        let mut handler = self.handler.write().await;
        handler.handle_frame(frame_data).await
    }

    /// Handle an incoming DHT message.
    pub async fn handle_message(&self, msg: DhtWireMessage) -> Option<Vec<u8>> {
        let mut handler = self.handler.write().await;
        handler.handle_message(msg).await
    }

    /// Handle a response message (for request/response matching).
    pub async fn handle_response(&self, msg: &DhtWireMessage) {
        let handler = self.handler.read().await;
        handler.handle_response(msg).await;
    }

    /// Get the routing table.
    pub fn routing_table(&self) -> Arc<RwLock<RoutingTable>> {
        self.routing_table.clone()
    }

    /// Get the DHT store.
    pub fn store(&self) -> SharedDhtStore {
        self.store.clone()
    }

    /// Get the local node ID.
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

    /// Get the protocol handler.
    pub fn handler(&self) -> Arc<RwLock<DhtProtocolHandler>> {
        self.handler.clone()
    }

    /// Check if a message is a DHT message based on ALPN.
    pub fn is_dht_message(alpn: &[u8]) -> bool {
        alpn == DHT_ALPN
    }

    /// Encode a DHT message for transmission.
    pub fn encode_message(msg: &DhtWireMessage) -> Result<Vec<u8>, adnet_dht::CodecError> {
        DhtCodec::encode(msg)
    }

    /// Decode a DHT message from received bytes.
    pub fn decode_message(bytes: &[u8]) -> Result<DhtWireMessage, adnet_dht::CodecError> {
        DhtCodec::decode(bytes)
    }
}

/// Extension trait for adding DHT support to the transport.
pub trait WithDhtSupport {
    /// Get the DHT integration if available.
    fn dht(&self) -> Option<&DhtTransportIntegration>;

    /// Check if DHT is supported.
    fn supports_dht(&self) -> bool;
}

/// Helper for building DHT-integrated transport.
pub struct DhtTransportBuilder {
    local_id: NodeId,
    routing_table: Option<Arc<RwLock<RoutingTable>>>,
    store: Option<SharedDhtStore>,
}

impl DhtTransportBuilder {
    /// Create a new builder.
    pub fn new(local_id: NodeId) -> Self {
        Self {
            local_id,
            routing_table: None,
            store: None,
        }
    }

    /// Set a custom routing table.
    pub fn with_routing_table(mut self, rt: Arc<RwLock<RoutingTable>>) -> Self {
        self.routing_table = Some(rt);
        self
    }

    /// Set a custom DHT store.
    pub fn with_store(mut self, store: SharedDhtStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Build the DHT integration.
    pub fn build(self) -> DhtTransportIntegration {
        match (self.routing_table, self.store) {
            (Some(rt), Some(store)) => {
                DhtTransportIntegration::with_components(self.local_id, rt, store)
            }
            _ => DhtTransportIntegration::new(self.local_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alpn_matching() {
        assert!(DhtTransportIntegration::is_dht_message(DHT_ALPN));
        assert!(!DhtTransportIntegration::is_dht_message(b"other/alpn"));
    }

    #[tokio::test]
    async fn test_integration_creation() {
        let node_id = NodeId::random();
        let integration = DhtTransportIntegration::new(node_id.clone());

        assert_eq!(*integration.local_id(), node_id);
    }

    #[tokio::test]
    async fn test_message_encode_decode() {
        let sender = NodeId::random();
        let msg = DhtCodec::get_providers(vec![0u8; 32], sender.clone());

        let encoded = DhtTransportIntegration::encode_message(&msg).unwrap();
        let decoded = DhtTransportIntegration::decode_message(&encoded).unwrap();

        assert!(matches!(decoded, DhtWireMessage::GetProviders(_)));
    }
}
