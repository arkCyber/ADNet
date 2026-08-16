//! `a3net-node` — the orchestration crate.
//!
//! A [`Node`](node::Node) bundles:
//! - [`BlobStore`](a3net_blobstore::BlobStore) — local blob storage
//! - [`GossipBus`](a3net_gossip::GossipBus) — room topic pub/sub
//! - [`MeshServer`](a3net_mesh::MeshServer) — HTTP fallback transport
//! - A [`Transport`](a3net_transport::Transport) — primary transport (QUIC today, iroh tomorrow)
//!
//! `Node` exposes the operations the rest of A3Net cares about:
//! - `announce` — publish an asset into a room topic
//! - `room_feed` — list known assets + peer sources
//! - `import_file` — import + announce a local file
//! - `fetch_blob` — locate peers + download (transport or mesh fallback)
//! - `subscribe` / `subscribe_room` — listen for incoming announcements

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod checksum;
pub mod contacts_manager;
pub mod download;
pub mod identity_ipc;
pub mod node_identity_store;
pub mod profile_page;
pub mod profile_server;
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
pub mod heartbeat_service;
pub mod peer_manager;
#[cfg(feature = "agent")]
pub mod agent;

pub use checksum::{ChecksumReport, ChunkChecksumEntry, ResumeState};
pub use contacts_manager::{ContactsManager, CONTACTS_FILE_NAME};
pub use node_identity_store::{
    NodeIdentityStore, NODE_IDENTITY_FILE_NAME, NODE_IDENTITY_FILE_VERSION,
};
pub use profile_page::{render_profile_html, ProfilePageInputs};
pub use profile_server::{
    ProfileServerBuilder, ProfileServerError, ProfileServerHandle, start_default,
};
pub use identity_ipc::{IdentityIpcConfig, IdentityIpcService, default_socket_path};
pub use download::{DownloadJob, DownloadProgress};
#[cfg(feature = "iroh")]
pub use iroh_runtime::IrohRuntime;
pub use node::{MeshEndpointInfo, Node, NodeBuilder, NodeConfig, NodeInfo, RelayEndpointInfo};
pub use heartbeat_service::{
    HeartbeatHandle, HeartbeatSender, HeartbeatService, MockHeartbeatSender,
};
pub use peer_manager::{
    HeartbeatMessage, HeartbeatStats, PeerEntry, PeerHeartbeatStats, PeerListSnapshot,
    PeerManager, PeerManagerConfig, PeerStatus, PendingPing,
    DEFAULT_HEARTBEAT_INTERVAL, DEFAULT_HEARTBEAT_JITTER_PERCENT, DEFAULT_HEARTBEAT_TIMEOUT,
    MAX_P2P_PEERS, MAX_PENDING_PINGS,
};
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
#[cfg(feature = "agent")]
pub use agent::{AgentAclMode, AgentEndpoint, NodeAgentBridge};
