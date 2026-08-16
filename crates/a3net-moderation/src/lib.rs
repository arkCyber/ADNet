//! `a3net-moderation` — content moderation for the A3Net distributed store.
//!
//! ## Why this crate exists
//!
//! Content-addressed storage (BLAKE3 / CID) is **immutable by design** —
//! there is no `UPDATE` operation, so traditional "delete this blob"
//! semantics do not exist. The moderation crate implements the four
//! layers that an A3Net deployment can use to keep unlawful content
//! off its public surface:
//!
//! 1. **Blocklist** [`blocklist::Blocklist`] — a persistent registry of
//!    banned content hashes keyed by [`a3net_types::ContentHash`].
//!    Backed by `blocklist.json` on disk. Supports add / remove / list
//!    / import-from-NCMEC and audit timestamps.
//!
//! 2. **Policy** [`policy::ModerationPolicy`] — the gateway's
//!    pre-serve / pre-`pin` decision engine. Combines a [`Blocklist`]
//!    with optional classifier hooks and a default-deny switch. The
//!    gateway calls [`ModerationPolicy::check_read`] /
//!    [`ModerationPolicy::check_write`] before any store / DAG / pin
//!    operation.
//!
//! 3. **Takedown** [`takedown::TakedownService`] — the local-erase
//!    primitive. Removes a pin from the [`a3net_blobstore::PinSet`]
//!    and triggers `gc_unpinned` so the bytes are physically dropped
//!    from disk. Optionally destroys the encrypted-blob-store key
//!    (crypto-shredding) and emits a [`a3net_security::SecurityEvent`]
//!    for auditing.
//!
//! 4. **Reputation bridge** [`reputation_bridge::apply_violation`] —
//!    the cross-subsystem feedback loop. Triggers a
//!    [`a3net_reputation::ReputationEvent::BehaviourPenalty`] with
//!    [`BehaviourKind::ContentViolation`] against the publishing node
//!    so other A3Net nodes graylist / refuse to peer.
//!
//! ## HTTP status
//!
//! The gateway returns **HTTP 451 Unavailable For Legal Reasons**
//! (RFC 7725) when a read is blocked, not `403` / `404`. This is the
//! correct status for "we have this content, but a takedown order
//! prevents us from serving it" and is the same status major public
//! gateways use.
//!
//! ## Concurrency
//!
//! [`Blocklist`] and [`ModerationPolicy`] use `parking_lot::RwLock` for
//! interior mutability. Reads are O(1) (hash lookup). Writes take an
//! exclusive lock and persist to disk atomically (write-temp +
//! rename).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod blocklist;
pub mod error;
pub mod policy;
pub mod reputation_bridge;
pub mod takedown;

pub use blocklist::{
    Blocklist, BlocklistEntry, BlocklistSource, BlocklistStats, TakedownReason,
    DEFAULT_BLOCKLIST_FILENAME,
};
pub use error::{ModerationError, ModerationResult};
pub use policy::{ModerationPolicy, PolicyDecision, PolicyDecisionKind};
pub use reputation_bridge::apply_violation;
pub use takedown::{
    TakedownOutcome, TakedownReport, TakedownService, TakedownServiceConfig, TakedownTarget,
};
