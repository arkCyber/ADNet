//! `a3net-chatstore` — SQLite-backed persistence for chat data.
//!
//! This crate is a port (and a clean-room rewrite) of two related SQLite
//! storage modules from the legacy codebase:
//!
//! 1. `Exodus/src-backup/src-tauri/src/microservice/chat_storage.rs`
//!    — per-user friend lists, direct messages, group messages,
//!    per-(user,target) sequence tracking and message receipts.
//! 2. `Exodus/src-backup/exodus-hub-server/src/manager.rs`
//!    — canonical hub server storage: users, conversations,
//!    group membership, sender-side sequence generation, pending
//!    offline messages, sync, and zstd+bincode compression for
//!    bulk history transfer.
//!
//! # Design
//!
//! - One SQLite file per node, opened with `journal_mode=WAL` so the
//!   IPC layer can read concurrently with gossip writes.
//! - All state is keyed by `user_id` (or `sender_id`) so multiple local
//!   users can coexist on a single node.
//! - Data types are the typed records from [`a3net_types::group_chat`] —
//!   no `String` / `i64` / `Vec<u8>` "stringly typed" persistence leaks.
//! - Async API on top of a synchronous SQLite handle (the executor
//!   dispatches blocking calls onto `tokio::task::spawn_blocking`).
//!   This matches the `exodus-hub-server` model and avoids pulling in
//!   the whole `tokio` runtime machinery on a per-connection basis.
//!
//! # Errors
//!
//! All public functions return [`ChatStoreError`], which is a thin
//! `thiserror` enum. `std::sync::Mutex` poisoning is normalised to a
//! regular `ChatStoreError::Lock` so callers don't have to deal with
//! `Result<_, MutexGuardErr>` plumbing.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod error;
pub mod im;
pub mod link_bookmark;
pub mod schema;
pub mod storage;
pub mod trust;

pub use error::{ChatStoreError, Result};
pub use im::{
    ChatType, Conversation, GroupMember, ImManager, MAX_SEQUENCE, Message, MessageReceipt,
    PendingMessage, SenderSequence, SyncRequest, SyncResponse, User, UserSequence,
};
pub use link_bookmark::{
    compute_bookmark_id, BookmarkSource, LinkBookmark, LinkBookmarkStore,
    LinkBookmarkStoreConfig, ListFilter as LinkBookmarkFilter,
    CountFilter as LinkBookmarkCountFilter,
};
pub use schema::SCHEMA_VERSION;
pub use storage::{ChatStorage, ChatStorageConfig, Friend, MessageAttachment};
pub use trust::{init_trust_schema, ChatTrustRecord, ChatTrustStore};

/// Phase 5a — iroh-docs backed message sync. Available only when
/// the `iroh` feature is enabled.
#[cfg(feature = "iroh")]
pub mod docs_bridge;
#[cfg(feature = "iroh")]
pub use docs_bridge::{
    ConversationTicket, DocHandle, DocsBridgeError, DocsBridgeResult, IrohDocsChat, MessageEvent,
};

// Re-export iroh-docs symbols we use in the public surface.
#[cfg(feature = "iroh")]
pub use iroh_docs::{AuthorId as IrohAuthorId, DocTicket as IrohDocTicket};

#[cfg(test)]
mod tests;
