//! `adnet-gossip` — topic-based pub/sub overlay.
//!
//! Three layers:
//!
//! 1. [`GossipTransport`] — pluggable transport trait. The default in-process
//!    implementation is [`InProcessGossip`]. Future implementations will wrap
//!    `iroh-gossip::net::Gossip` (iroh-net) without changing call sites.
//!
//! 2. [`GossipBus`] — a typed, room-aware facade that decodes payloads into
//!    [`Announcement`](adnet_types::Announcement) and routes them by
//!    [`Topic`](adnet_types::Topic).
//!
//! 3. Subscription API — [`GossipBus::subscribe`] returns a
//!    [`tokio::sync::broadcast`] receiver for fan-out to consumers.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod access;
pub mod bridge;
pub mod bus;
pub mod dedup;
pub mod persistence;
pub mod priority;
pub mod transport;

#[cfg(feature = "iroh")]
pub mod iroh_transport;

pub use access::{
    AccessControl, AccessCheckResult, CredentialType, RoomAccessPolicy, RoomCredential,
};
pub use bridge::GossipBridge;
pub use bus::GossipBus;
#[cfg(feature = "iroh")]
pub use iroh_transport::IrohGossipTransport;
pub use persistence::{
    MessagePersistence, MessageStore, PersistenceConfig, PersistenceStats, StoredMessage,
};
pub use priority::{MessageSource, RetrievalStrategy, determine_strategy};
pub use transport::{GossipTransport, InProcessGossip, TopicId};

pub use adnet_types::{Announcement, AnnouncementPayload, NodeId, Topic};
