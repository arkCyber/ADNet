//! `adnet-node` — the orchestration crate.
//!
//! A [`Node`](node::Node) bundles:
//! - [`BlobStore`](adnet_blobstore::BlobStore) — local blob storage
//! - [`GossipBus`](adnet_gossip::GossipBus) — room topic pub/sub
//! - [`MeshServer`](adnet_mesh::MeshServer) — HTTP fallback transport
//! - A [`Transport`](adnet_transport::Transport) — primary transport (QUIC today, iroh tomorrow)
//!
//! `Node` exposes the operations the rest of ADNet cares about:
//! - `announce` — publish an asset into a room topic
//! - `room_feed` — list known assets + peer sources
//! - `import_file` — import + announce a local file
//! - `fetch_blob` — locate peers + download (transport or mesh fallback)
//! - `subscribe` / `subscribe_room` — listen for incoming announcements

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod checksum;
pub mod download;
#[cfg(feature = "iroh")]
pub mod iroh_runtime;
pub mod node;
pub mod state;
pub mod transfer;

pub use checksum::{ChecksumReport, ChunkChecksumEntry, ResumeState};
pub use download::{DownloadJob, DownloadProgress};
#[cfg(feature = "iroh")]
pub use iroh_runtime::IrohRuntime;
pub use node::{MeshEndpointInfo, Node, NodeBuilder, NodeConfig, NodeInfo, RelayEndpointInfo};
pub use state::{RoomFeed, SwarmIndex};
pub use transfer::{
    apply_throttle, build_checksum_report, run_chunked_download, MeshBackend, TransferBackend,
    TransferOutcome, TransferProgress, TransferSettings,
};
