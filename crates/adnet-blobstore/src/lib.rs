//! `adnet-blobstore` — disk-backed BLAKE3 chunked blob store.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod async_block_store;
#[cfg(feature = "car")]
pub mod car;
pub mod chunked;
pub mod dag_block_store;
pub mod filename;
#[cfg(feature = "graphsync")]
pub mod graph_sync_ext;
#[cfg(feature = "graphsync")]
pub mod graphsync;
#[cfg(feature = "graphsync")]
pub mod graphsync_requester;
pub mod import;
#[cfg(feature = "iroh")]
pub mod iroh_store;
pub mod metrics;
pub mod store;
#[cfg(feature = "scope")]
pub mod scope;
pub mod traits;
#[cfg(feature = "bitswap")]
pub mod bitswap;

#[cfg(feature = "bitswap")]
pub mod bitswap_codec;

#[cfg(feature = "bitswap")]
pub mod bitswap_wantlist;

// Bao tree and swarm download modules
pub mod bao_tree;
pub mod swarm_download;

// Bao tree and swarm download re-exports
pub use bao_tree::{
    BaoLeaf, BaoProof, BaoTree, BaoTreeBuilder, BaoTreeError,
    MerkleSibling, MerklePath,
};
// Note: BaoTreeBuilder is already exported from bao_tree module above
pub use swarm_download::{
    ChunkFetcher, SwarmDownloader, SwarmDownloadService, SwarmError,
    SwarmMetrics, SwarmProgress, SwarmResult, PeerInfo, Piece, PieceState,
    PieceSelectionStrategy, SwarmLedger, MAX_CONCURRENT_DOWNLOADS,
    DEFAULT_CHUNK_TIMEOUT, MAX_CHUNK_RETRIES, MAX_DOWNLOADS_PER_PEER,
    ENDGAME_THRESHOLD, SR_TAG_SWARM_1, SR_TAG_SWARM_2,
};

// NAS namespace for WebDAV and workspace integration
pub mod namespace;
pub mod pin_set;
pub use namespace::{
    AuditContext, Clock, Entry, Manifest, MockClock, Nas, NamespaceError,
    NamespaceRead, NamespaceWrite, NoopQuota, PathSegments, QuotaHook,
    SystemClock, MAX_CHILDREN_PER_DIR, MAX_DEPTH, MAX_PATH_RAW_LEN,
    AuditRecord, TrashEntry, VersionMeta,
};
pub use pin_set::{now_unix as blob_now_unix, PinKind, PinRecord, PinSet};

// Encrypted-blob-store wrapper (optional, opt-in via `app.toml`
// `storage.encrypt.enabled = true`). The wrapper is feature-free —
// encryption is a runtime decision made by the CLI, not a compile
// time toggle — so we always expose it.
pub mod encrypted;
pub use encrypted::{
    EncryptedBlobStore, EncryptionError, EncryptionKey, KeyFile, KeyFileKdf,
    KeyFileKdfParams, KeyStore, KeyWriteAccess, AEAD_OVERHEAD, META_ENCRYPTED_FIELD,
};

// Re-export GraphSync types from adnet-types
#[cfg(feature = "graphsync")]
pub use adnet_types::graphsync as graphsync_types;

// Re-export common types
pub use async_block_store::{AsyncBlockStore, AsyncBlockStoreAdapter, AsyncBlockStoreError, AsyncResult};
pub use chunked::{CHUNK_SIZE, ChunkReader, ChunkWriter};
pub use dag_block_store::{BlobStoreAdapter, DagBlockStore, DagBlockStoreError, DagBlockStoreStats, MemBlobStoreAdapter};
pub use filename::{MAX_FILENAME_LEN, safe_filename};
#[cfg(feature = "scope")]
pub use scope::{
    BlobStoreScope, ByteRange, DEFAULT_PRIVATE_FRACTION, DEFAULT_SHARED_FRACTION,
    QuotaPolicy, StorageTopology, TopologyError, TopologyUsage,
};

#[cfg(feature = "iroh")]
pub use iroh_store::{
    IrohBlobHash, IrohBlobStore, IrohBlobTicket, MAX_RANGE_BYTES, content_hash_to_iroh_hash,
    iroh_hash_to_content_hash,
};
pub use store::BlobStore;
pub use traits::{BlobImporter, BlobReader};
#[cfg(feature = "bitswap")]
pub use bitswap::{BitswapEngine, BitswapMessage, LedgerStats, SessionStats};
#[cfg(feature = "bitswap")]
pub use bitswap_wantlist::{WantlistManager, WantEntry, WantType, PeerWantlist, WantlistError, WantlistStats};
#[cfg(feature = "bitswap")]
pub use bitswap_codec::{BitswapCodec, CodecError};

// GraphSync requester
#[cfg(feature = "graphsync")]
pub use graphsync_requester::{GraphSyncRequester, RequestOptions, Priority, RequestStatus, RequestStats, RequesterEvent, RequesterError, MAX_CONCURRENT_REQUESTS};

#[cfg(feature = "graphsync")]
pub use graph_sync_ext::{dag_subtree_size, DagExt};

// GraphSync wire envelope / client / server / bridge trait / mocks
#[cfg(feature = "graphsync")]
pub use graphsync::{
    GraphSyncClient, GraphSyncRequestHandle, GraphSyncServer, GraphSyncTransportBridge,
    GraphSyncTransportError, GraphSyncWire, MemDagStore, MockGraphSyncTransport,
    GRAPHSYNC_ALPN, MAX_FRAME_SIZE, DEFAULT_REQUEST_TIMEOUT,
};

// CAR types (optional)
#[cfg(feature = "car")]
pub use car::{DagBlock, DagCarWriter, DagWalker, export_dag, DagBlockStoreExt, CarReader, CarWriter, BatchedCarWriter, WriteCarExt, CarBlock, CarHeader, CarError, read_car, write_car};
