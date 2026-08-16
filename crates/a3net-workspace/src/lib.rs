//! `a3net-workspace` — per-node shared folder for P2P file exchange.
//!
//! Ported from `Exodus@src-backup/.../exodus_workspace.rs`. Files are
//! grouped into three folders:
//!
//! - `shared/` — files published to peers.
//! - `inbox/`  — files received from peers.
//! - `outbox/` — staged outbound copies.
//!
//! A JSON manifest (`workspace.json`) tracks every shared file. Peers
//! discover manifest entries via the gossip topic returned by
//! [`workspace_room_topic`].

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

mod workspace;

pub use workspace::{
    DIR_INBOX, DIR_OUTBOX, DIR_SHARED, WORKSPACE_ROOM_ID, Workspace, WorkspaceFileEntry,
    WorkspaceManifest, split_name_ext,
};

/// Gossip topic name used to announce workspace manifest updates.
///
/// Mirrors the `a3net-room-{room}` convention used by the rest of the
/// stack (`a3net-gossip::topic`).
pub fn workspace_room_topic() -> String {
    format!("a3net-room-{WORKSPACE_ROOM_ID}")
}
