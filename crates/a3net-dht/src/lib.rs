//! `a3net-dht` — Kademlia-style DHT routing for A3Net.
//!
//! This crate implements a simplified Kademlia DHT for content routing and peer discovery.
//! It complements the existing Pkarr/DNS/mDNS discovery in A3Net by adding:
//! - Provider record storage and lookup (content → peers)
//! - IPNS-like mutable naming (for variable content pointers)
//! - K-Bucket routing tables for O(log N) lookups
//!
//! ## Design
//!
//! The DHT is designed to integrate with A3Net's existing infrastructure:
//! - Uses `ContentHash` (BLAKE3) as DHT keys
//! - Uses `NodeId` (Ed25519-derived) as peer IDs
//! - Compatible with the `iroh` transport layer
//!
//! ## Features
//!
//! - **K-Bucket Routing Table**: Maintains sorted peer contacts by XOR distance
//! - **Provider Records**: Announce content availability (like libp2p/IPFS)
//! - **IPNS Records**: Mutable name records with sequence versioning
//! - **Iterative Queries**: Parallel α-queries for efficient lookups
//!
//! ## Example
//!
//! ```rust,ignore
//! use a3net_dht::{DhtNode, DhtKey, RoutingTable};
//! use a3net_types::NodeId;
//!
//! // Create a DHT node
//! let local_id = NodeId::random();
//! let dht = DhtNode::new(local_id);
//!
//! // Add a provider for content
//! let key = DhtKey::from_content_hash_hex("abc123...");
//! dht.add_provider(&key, "127.0.0.1:8080").await;
//!
//! // Find providers for content
//! let providers = dht.find_providers(&key).await;
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod bucket;
pub mod record;
pub mod store;
pub mod query;
pub mod node;
pub mod protocol;
pub mod handler;
pub mod network;
pub mod retry;
pub mod service;
pub mod transport;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod audit_tests;

#[cfg(test)]
mod properties;

#[cfg(test)]
mod chaos;

#[cfg(test)]
mod perf;

pub use bucket::{KBucket, KBUCKET_SIZE, RoutingTable, Contact, InsertError};
pub use record::{
    DhtKey, ProviderRecord, IpnRecord, DhtMessage, DhtValue, NodeInfo,
    Signer, Verifier,
};
pub use store::{DhtStorage, InMemoryDhtStore, SharedDhtStore, new_in_memory_store, cleanup_task};
pub use query::{DhtQuery, QueryResult, QueryError, DhtMessageSender, node_id_from_key, node_id_from_key_str};
pub use node::{DhtNode, DhtConfig, DhtTransport};
pub use protocol::{
    DhtCodec, DhtWireMessage, DhtMessageBuilder, DHT_ALPN, DHT_VERSION,
    CodecError, RequestId,
    FindNodePayload, NodesPayload, NodeContact,
    GetProvidersPayload, ProvidersPayload, ProviderRecordWire,
    AddProviderPayload, AddProviderAckPayload,
    GetValuePayload, ValuePayload, ValueData,
    PutValuePayload, PutAckPayload,
    PingPayload, PongPayload,
};
pub use handler::{DhtProtocolHandler, DhtEvent, PendingRequestError};
pub use network::{DhtNetworkSender, TransportDhtSender};
pub use retry::{RetryPolicy, PeerFailureTracker, is_transient};
pub use service::{DhtService, DhtServiceConfig, DhtServiceTask};
pub use transport::{DhtTransportAdapter, DynResponseSink, TransportBridge};
