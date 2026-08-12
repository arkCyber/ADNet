//! `adnet-socialfeed` — Social feed (朋友圈 / Moments) runtime for ADNet.
//!
//! This crate is the port of the social feed service from
//! `Exodus@src-backup/src-tauri/src/microservice/social_feed_service.rs`
//! (and the matching `social_feed_commands.rs` Tauri command layer) into
//! the ADNet workspace. The original code only had:
//!
//! - a single-machine Unix-socket JSON-RPC service holding
//!   `posts`, `comments`, `reactions`, `follows` in `HashMap`s;
//! - a set of Tauri commands routing `social_post_create` etc.
//!   through that socket;
//! - a TypeScript surface in `socialTimeline.ts` exposing the same
//!   operations to the Svelte front-end.
//!
//! The port re-shapes that into ADNet's layered architecture:
//!
//! - **Types** live in [`adnet_types::social_feed`] (already in
//!   place, DO-178C-grade invariants). This crate re-exports them
//!   for downstream callers.
//! - **Persistence** — [`storage::SocialFeedStorage`]: a
//!   SQLite-backed store with `journal_mode=WAL`, mirroring
//!   [`adnet_chatstore::ChatStorage`]. One DB file per node, all
//!   state keyed by `user_id`.
//! - **IPC** — [`ipc::SocialFeedIpcService`]: a JSON-RPC service
//!   over Unix sockets following the same plumbing as
//!   [`adnet_ipc::group_chat_service::GroupChatIpcService`].
//! - **Gossip** — [`gossip::SocialFeedBridge`]: envelopes
//!   `SocialPost` / `SocialComment` / `SocialReaction` over
//!   [`adnet_gossip::GossipTransport`] topics.
//! - **Service** — [`service::SocialFeedService`]: the
//!   high-level facade exposed to the CLI / FFI / IPC consumers.
//! - **CLI** (in `adnet-cli`, not this crate) — `adnet moments …`
//!   one-shot commands and `/moments` REPL slash commands.
//! - **FFI** (in `adnet-ffi`, not this crate) — typed JSON-in /
//!   JSON-out C-ABI helpers for mobile embedders.
//!
//! # Error type
//!
//! Every fallible public function returns
//! [`error::SocialFeedError`], a thin `thiserror` enum with the
//! usual `Database`, `Lock`, `NotFound`, `Validation`, `Ipc`,
//! `Gossip` variants. Lock poisoning is normalised to
//! `SocialFeedError::Lock` so callers don't have to plumb
//! `MutexGuardErr` plumbing.
//!
//! # Aerospace-grade invariants
//!
//! All persistence operations are guarded by the typed records'
//! `validate()` methods (see [`adnet_types::social_feed`]). The
//! SQLite layer rejects `INSERT`s / `UPDATE`s whose payload fails
//! validation, so on-disk corruption can never exceed the
//! validation surface. `integrity_hash` is recomputed on every
//! write so replayed records from gossip can be detected even if
//! schema field ordering drifts.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod bridge;
pub mod error;
pub mod gossip;
pub mod ipc;
pub mod service;
pub mod storage;
mod storage_schema;

pub use bridge::{Envelope, EnvelopeKind, SocialFeedBridge};
pub use error::{ErrorClass, Result, SocialFeedError};
pub use gossip::{SocialFeedGossipConfig, SocialFeedSubscriber};
pub use ipc::{SocialFeedIpcConfig, SocialFeedIpcService};
pub use service::{
    SocialFeedService, SocialFeedServiceConfig, TimelineCursor, TimelinePage, TimelineQuery,
    TimelineScope,
};
pub use storage::{SocialFeedStorage, SocialFeedStorageConfig};
pub use storage_schema::SCHEMA_VERSION;

// Re-export the typed wire records so downstream users only need
// to depend on this crate, not `adnet-types` directly, when they
// only care about the social-feed surface.
pub use adnet_types::social_feed::{
    FollowRelationship, PostAttachment, SocialComment, SocialPost, SocialReaction, VIS_FRIENDS,
    VIS_PRIVATE, VIS_PUBLIC,
};
