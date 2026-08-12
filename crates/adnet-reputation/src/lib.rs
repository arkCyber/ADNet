//! # adnet-reputation
//!
//! Global peer reputation (PeerScore) for ADNet.
//!
//! This crate provides a unified, cross-subsystem reputation
//! abstraction that replaces the ad-hoc `peer_scores: HashMap<String,
//! usize>` counters scattered across the codebase (most visibly
//! `crates/adnet-blobstore/src/bitswap.rs`).
//!
//! ## Why a global PeerScore?
//!
//! ADNet has at least three subsystems that independently grade
//! peer behavior:
//!
//! - **Bitswap** — want-have coverage / delivery success / session
//!   lifetime.
//! - **Gossipsub** — valid / invalid / duplicate messages, mesh
//!   delivery, slow/inactive peers.
//! - **Pairing** — first contact produces a `CredentialId` that is a
//!   strong "trusted" signal; revocation flips it negative.
//! - **Chat** — users issue per-target trust levels and reports
//!   (spam, harassment, …) that should feed back into the global
//!   score so the rest of the system can refuse outbound to
//!   misbehaving peers.
//!
//! Without a shared abstraction every subsystem makes its own
//! inconsistent call (accept this peer / drop this peer) and
//! operators have no single place to inspect or remediate. This
//! crate is that place.
//!
//! ## Features
//!
//! - [`event::ReputationEvent`] — typed event model covering all
//!   four subsystems.
//! - [`score::PeerScore`] / [`score::PeerScoreTable`] — thread-safe
//!   per-peer scoring, sharded to keep write contention low under
//!   the gossip fire-hose.
//! - [`params::ReputationParams`] — tunable weights, caps, decay
//!   factors. Sensible defaults are exposed via
//!   [`params::ReputationParams::default`].
//! - [`decay::DecayLoop`] — background tick that decays every
//!   `(peer, topic)` score toward zero at a configurable rate.
//! - [`store::ReputationStore`] — append-only JSONL of
//!   [`event::ReputationDelta`] plus a periodic state snapshot;
//!   crash-safe by virtue of atomic rename.
//! - [`metrics::register_metrics`] — Prometheus
//!   `adnet_reputation_score{peer_hash}` gauge and
//!   `adnet_reputation_event_total{event}` counter.
//! - [`trust::TrustLevel`] / [`trust::TrustFusion`] — chat-side
//!   per-user trust level that fuses with the global score.
//! - [`reporter::GossipSignal`], [`reporter::BitswapSignal`],
//!   [`reporter::PairingSignal`] — ergonomic adapters each subsystem
//!   can call without knowing about the others.
//!
//! ## Quickstart
//!
//! ```rust
//! use adnet_reputation::{PeerScoreTable, ReputationEvent, ReputationParams};
//! use adnet_types::NodeId;
//!
//! let params = ReputationParams::default();
//! let table = PeerScoreTable::new(params);
//!
//! let peer = NodeId::random();
//! table.apply(ReputationEvent::ValidMessage {
//!     peer: peer.clone(),
//!     topic: None,
//!     size_bytes: 1024,
//! });
//! let score = table.score(&peer);
//! assert!(score.is_some());
//! ```
//!
//! ## Persistence
//!
//! Persistence is opt-in via [`store::ReputationStore`]. The on-disk
//! format is:
//!
//! - `reputation.jsonl` — one JSON-encoded
//!   [`event::ReputationDelta`] per line, append-only.
//! - `reputation.state.json` — full [`score::PeerScoreTable`]
//!   snapshot, written every [`store::SNAPSHOT_EVERY`] deltas.
//!   On startup the snapshot is loaded first, then the JSONL is
//!   re-played for deltas newer than the snapshot. This bounds the
//!   recovery time at startup regardless of how long the process
//!   has been running.
//!
//! ## Crate-level policies
//!
//! - **No `unsafe`**.
#![deny(unsafe_code)]
#![warn(missing_docs)]
//!
//! - No silent panics on malformed input. All errors are surfaced
//!   via [`error::ReputationError`].
//! - All `NodeId` values written to disk are validated; malformed
//!   entries are skipped and logged rather than crashing the
//!   process.

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod decay;
pub mod error;
pub mod event;
pub mod metrics;
pub mod params;
pub mod reporter;
pub mod score;
pub mod store;
pub mod trust;

// Re-export the most-used types at the crate root.
pub use error::{ReputationError, ReputationResult};
pub use event::{InvalidReason, ReputationDelta, ReputationEvent};
pub use params::{
    BehaviourKind, ReputationParams, ReportKind, MAX_SCORE, MIN_SCORE,
};
pub use reporter::{
    BitswapSignal, GossipSignal, PairingSignal, ReputationReporter,
};
pub use score::{
    PeerScore, PeerScoreTable, ScoreSnapshot, ShardIndex, TopicScore,
};
pub use event::TopicId;
pub use store::{ReputationStore, ReputationStoreConfig, SNAPSHOT_EVERY};
pub use trust::{TrustFusion, TrustLevel, TrustSignal, DEFAULT_TRUST_HALFLIFE_HOURS};

// Pull the global registry hooks in by default — they're free if
// no metrics are emitted.
#[cfg(feature = "metrics")]
pub use metrics::{register_metrics, ReputationMetrics};
