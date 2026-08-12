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
#[cfg(feature = "dht")]
pub mod dht;
#[cfg(feature = "dht")]
pub mod dht_bridge;
#[cfg(feature = "bitswap")]
pub mod bitswap;
#[cfg(feature = "bitswap")]
pub mod bitswap_transport;
#[cfg(feature = "bitswap")]
pub mod bitswap_wiring;
#[cfg(feature = "graphsync")]
pub mod graphsync;
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
    MeshBackend, TransferBackend, TransferOutcome, TransferProgress, TransferSettings,
    apply_throttle, build_checksum_report, run_chunked_download,
};
#[cfg(feature = "bitswap")]
pub use bitswap::{BitswapConfig, BitswapHandle, BitswapStats, BitswapBlockResult, ProviderRecord};
#[cfg(feature = "bitswap")]
pub use bitswap_transport::{
    BitswapNetworkAdapter, BitswapTransportBridge, MockBitswapTransport,
    BitswapBlockOutcome, BitswapMetrics, BITSWAP_ALPN,
};
#[cfg(feature = "bitswap")]
pub use bitswap_wiring::{wire_bitswap_to_transport, BitswapWiring};

// GraphSync service surface (gated behind the `graphsync` feature so
// default builds don't pull in the QUIC bridge / dispatcher).
#[cfg(feature = "graphsync")]
pub use graphsync::{
    graphsync_wire_len_hint, GraphSyncConfig, GraphSyncHandle, GraphSyncHello,
    GraphSyncQuicBridge, GraphSyncService, GraphSyncServiceError, GraphSyncStats,
    NodeBlockStore, DEFAULT_DIAL_TIMEOUT,
};

#[cfg(feature = "dht")]
pub use dht::{DhtConfig, DhtHandle, DhtMetrics, DhtStats, IpnConfig, IpnHandle};
