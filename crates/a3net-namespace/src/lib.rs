//! `a3net-namespace` — IPNS (InterPlanetary Naming System) for A3Net.
//!
//! This crate provides mutable naming for immutable content in A3Net.
//! IPNS allows nodes to maintain a mutable pointer to content while keeping
//! the same name.
//!
//! ## Overview
//!
//! IPNS is the naming system that makes content mutable. While IPFS CIDs always
//! point to the same content (immutable hash), IPNS allows you to update
//! what a name points to over time.
//!
//! ## Key Concepts
//!
//! - **Self-Certifying Names**: Names are derived from public keys
//! - **Signed Records**: Updates are cryptographically signed
//! - **Sequence Numbers**: Prevent replay attacks
//! - **TTL**: Control cache duration
//!
//! ## Usage
//!
//! ```rust,ignore
//! use a3net_namespace::{IpnPublisher, IpnResolver, Ed25519SecretKey};
//!
//! // Create a publisher with a secret key
//! let secret_key = Arc::new(Ed25519SecretKey::generate());
//! let publisher = IpnPublisher::new(secret_key.clone());
//!
//! // Publish a name
//! let name = publisher.public_key().to_ipns_name();
//! let record = publisher.publish(&name, "/ipfs/QmNewContent...").unwrap();
//!
//! // Create a resolver
//! let resolver = IpnResolver::new(Duration::from_secs(3600));
//!
//! // Cache and resolve
//! resolver.cache_record(record);
//! let value = resolver.resolve(&name).await;
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod ipns;
pub mod pubsub;
pub mod transport;
pub mod dnslink;

#[cfg(test)]
mod transport_tests;

pub use ipns::{
    IpnRecord, IpnPublisher, IpnResolver, IpnsError,
    SecretKey, Verifier, Ed25519SecretKey, Ed25519Verifier,
    public_key_to_ipns_name, TrustLevel,
};
pub use pubsub::{
    IpnGossipPayload, PubsubIpnsResolver, PubsubSubscription,
    IPNS_PUBSUB_ROOM, publish_payload,
};
pub use transport::{
    IpnTransport, IpnRecordStream, TransportHealth, SharedIpnBus,
    default_transports,
};
pub use transport::disk::DiskJournalTransport;
pub use transport::pkarr::{PkarrConfig, PkarrRelay, PkarrTransport, PkarrLookup, PkarrPublisher};
pub use transport::gossip::{GossipIpnTransport, IPNS_TOPIC};
pub use transport::multi::MultiTransport;
#[cfg(feature = "dht")]
pub use transport::dht::{
    DhtIpnTransport, DhtBackend, DhtQueryBackend, LocalDhtBackend,
    encode_ipns_record, decode_ipns_record, DEFAULT_IPNS_DHT_TTL,
};
pub use dnslink::{DnsLinkError, DnsLinkPath, DnsLinkResolver, DnsLookup, InMemoryLookup};
