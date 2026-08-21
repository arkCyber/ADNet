//! `a3net-mailbox` — offline message store-and-forward for A3Net applications.
//!
//! Companion crate to [`a3net-relay`]. Where the relay is a *transparent*
//! HTTP forward proxy (no state, no identity), the mailbox is a *stateful*
//! in-box service: it accepts envelopes on behalf of recipients who are
//! currently offline, persists them under the recipient's identity, and
//! hands them out when the recipient comes back online.
//!
//! ## Crate pillars
//!
//! - **End-to-end opaque**: the server never reads envelope plaintext. It
//!   only validates the sender's signature and routes by recipient id.
//! - **Pull-only delivery (Phase 1)**: recipients poll when they come
//!   online. No push notifications, no WebSocket fan-out. This keeps the
//!   server provably stateless from the network's perspective and matches
//!   the operational target of a small fixed-IP VPS.
//! - **Per-recipient quota / TTL**: each `recipient_id` is bounded by
//!   queue depth, total bytes, and per-message TTL. Quotas are enforced
//!   by the [`policy`] module.
//! - **Plug-in storage**: [`storage::MailboxStore`] is a trait. The default
//!   in-memory implementation is [`storage::MemoryStore`]; the production
//!   implementation is [`SqliteStore`] (WAL mode, per-recipient connection pool).
//! - **Reuse a3net-relay idioms**: `axum` router, `ServerPolicy::from_config`,
//!   `MailboxServerHandle::Drop` graceful shutdown, `MailboxMetrics::get()`
//!   singleton — same patterns, different payload.
//! - **Optional billing** (mirrors `a3net-relay`'s `billing` feature):
//!   when enabled, the operator can accept signed pledges in exchange for
//!   larger per-recipient quotas. Off by default; the default mailbox is
//!   free to use.
//!
//! ## DO-178C §6.3 compliance
//!
//! - **Traceability**: every RPC method maps to a `MailboxClient` method
//!   and a `MailboxServer` handler. Every public type is reachable from
//!   the canonical `lib.rs` re-exports.
//! - **Determinism**: every envelope carries a stable `msg_id` and a
//!   monotonic per-recipient sequence; watermarks are persistent.
//! - **Fail-safe**: [`error::MailboxError::error_class`] exposes
//!   `Permanent / Transient / Security / Internal` clients can branch on
//!   without parsing strings.
//! - **Reproducibility**: `EnqueueOutcome::queued_at` and `expires_at`
//!   are server-assigned UTC timestamps; sequence numbers are monotonic
//!   per recipient.
//! - **Defensive programming**: every wire-level string is validated
//!   before reaching the signature layer (see [`auth::validate_recipient_id`]
//!   and [`auth::validate_msg_id`]).
//! - **Verifiability**: enqueue and pull are guarded by EIP-191
//!   signatures; rejection paths increment `MailboxMetrics::enqueues_rejected`
//!   and the audit trail is in `EnqueueOutcome::duplicate`.
//!
//! ## Layout
//!
//! - [`config`]   — [`config::MailboxConfig`] (persistable), default
//!   values, and `bind` helper.
//! - [`client`]   — [`client::MailboxClient`], a `reqwest`-based client
//!   for `enqueue` / `pull` / `ack`.
//! - [`server`]   — [`server::MailboxServer`], the `axum` server itself.
//! - [`storage`]  — [`storage::MailboxStore`] trait + `MemoryStore` /
//!   `SqliteStore` (Phase 1+) implementations.
//! - [`policy`]   — [`policy::SizePolicy`], [`policy::QuotaPolicy`],
//!   [`policy::TtlPolicy`].
//! - [`auth`]     — EIP-191 signature verification + recipient / msg_id
//!   validation (DO-178C §6.3 *defensive programming*).
//! - [`metrics`]  — [`metrics::MailboxMetrics`] singleton (gauge +
//!   counters).
//! - [`error`]    — [`error::MailboxError`] + [`error::MailboxErrorClass`].
//!
//! ## Status
//!
//! **Phase 1 implemented.** All four HTTP handlers are real. Ed25519 /
//! secp256k1 signature verification is enforced. Quota / TTL / watermark
//! policies are wired. Property tests and HTTP integration tests live
//! under `tests/`.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod auth;
#[cfg(feature = "billing")]
pub mod billing;
pub mod client;
pub mod config;
pub mod error;
pub mod metrics;
pub mod policy;
pub mod rate_limit;
pub mod server;
pub mod sqlite_store;
pub mod storage;

pub use client::{
    canonical_ack_bytes, canonical_pull_bytes, AckRequest, AckResponse, EnqueueRequest,
    EnqueueResponse, MailboxClient, PullResponse,
};
pub use config::{MailboxConfig, MailboxServerInfo, StorageBackend};
pub use error::{MailboxError, MailboxErrorClass, MailboxResult};
pub use policy::{QuotaCheck, QuotaDecision, QuotaPolicy, SizePolicy, TtlPolicy};
pub use rate_limit::{RateLimitConfig, RateLimitRegistry, RateLimitResult, TrustedProxy};
pub use server::{
    ErrorBody, MailboxServer, MailboxServerHandle, ServerPolicy, ServerState,
};
pub use sqlite_store::SqliteStore;
pub use storage::{
    EnqueueOutcome, MailboxStore, MemoryStore, QuotaUsage, StoredEnvelope, Watermark,
};
