//! `a3net-news` — bulletin (announcement + news) service.
//!
//! A single crate covers both **authoritative announcements** and
//! **news feed entries** through the unified [`a3net_types::BulletinItem`]
//! record. The service:
//!
//! 1. Persists every published / received bulletin to a local SQLite
//!    database (`<data_dir>/news.db`).
//! 2. Forwards local bulletins through the existing
//!    [`a3net_gossip::GossipTransport`] overlay (same `InProcessGossip`
//!    or `IrohGossipTransport` the rest of the node uses) on a
//!    dedicated topic name (`a3net-news-{room}`).
//! 3. Subscribes to every room it's tracking, validates every
//!    incoming envelope, persists it, and emits a typed event to the
//!    caller's `broadcast::Receiver`.
//! 4. Supports **offline catch-up** via a per-room `last_seq` cursor
//!    so a node that reconnects after a network partition can replay
//!    the bulletin timeline it missed from its own SQLite store.
//!
//! ## Wire format
//!
//! A [`BulletinEnvelope`] wraps a [`BulletinItem`] with the sender's
//! [`a3net_types::NodeId`] and an optional wallet signature. Envelopes
//! are JSON-encoded with `camelCase` so they can travel over the
//! existing `AnnouncementPayload` → `serde_json::Value` pipe in
//! `a3net-gossip` without an extra codec.
//!
//! ## Aerospace-grade invariants (DO-178C)
//!
//! - All public APIs return [`Result`] with a typed error.
//! - `validate()` is called on every entry into the service —
//!   `publish`, `ingest`, and the SQLite bootstrap replay path all
//!   reject malformed records at the boundary.
//! - The store never accepts a bulletin whose `sequence` is not
//!   strictly greater than the persisted `last_seq` for that room —
//!   monotonic ordering is enforced at the SQL layer, not the
//!   application layer, so a crash mid-write cannot corrupt the
//!   ordering invariant.
//!
//! ## Layering
//!
//! ```text
//! a3net-cli ───────┐
//!                  │
//! a3net-ffi ──┐    │    ┌────────────────┐
//!             ▼    ▼    ▼                │
//!         a3net-news::NewsService ──► BulletinStore (SQLite)
//!                  │
//!                  ▼
//!             BulletinBus ──► a3net_gossip::GossipTransport
//! ```
//!
//! [`BulletinItem`]: a3net_types::BulletinItem
//! [`BulletinEnvelope`]: crate::envelope::BulletinEnvelope

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod envelope;
pub mod error;
pub mod service;
pub mod store;

pub use envelope::{BulletinEnvelope, BulletinEnvelopePayload, BulletinEvent, BULLETIN_TOPIC_PREFIX};
pub use error::{NewsError, NewsResult};
pub use service::{NewsService, NewsServiceBuilder, NewsServiceConfig, ValidationPolicy};
pub use store::{BulletinCursor, BulletinStore, BulletinStoreConfig, StoredBulletin};

// Re-export the in-process gossip transport so embedders
// (CLI, FFI, tests) don't need to take a direct dependency on
// `a3net-gossip` just to wire a stand-alone `NewsService`.
pub use a3net_gossip::InProcessGossip;

// Re-export the user-facing payload types from `a3net_types` so
// callers can build bulletin items without depending on
// `a3net-types` themselves.
pub use a3net_types::{
    BulletinAttachment, BulletinCategory, BulletinId, BulletinItem, BulletinKind,
    BulletinSeverity,
};