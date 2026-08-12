//! `adnet-share` — P2P file & directory sharing over iroh.
//!
//! This crate ports the sendme ([`n0-computer/sendme`]) `send` /
//! `receive` flow onto ADNet's existing primitives:
//!
//! - `Collection` — a `(relative_name → ContentHash)` manifest for a
//!   multi-file blob. Wire-compatible with `iroh_blobs::format::collection::Collection`
//!   so an `iroh-blobs` peer can consume our `ShareTicket`s and vice
//!   versa.
//! - `walk_import` — recursively ingest a file or directory into any
//!   `BlobImporter`-implementing backend (the legacy `BlobStore`, the
//!   iroh-backed `IrohBlobStore`, or the in-process `MemStore`).
//! - `ShareTicket` — a printable ticket carrying the sender's
//!   `NodeAddr` + the `Collection` hash + optional manifest preview.
//! - `metrics` — Prometheus-facing counters and a histogram for the
//!   `receive` path (`adnet_share_receive_bytes_total`,
//!   `_bytes_done`, `_files_total`, `_files_done`,
//!   `_errors_total`, `_seconds`).
//!
//! ## Why a separate crate?
//!
//! sendme is a 1.3k-line thin CLI wrapping the same iroh APIs we already
//! have in `adnet-transport` (iroh endpoint / Router) and
//! `adnet-blobstore::iroh_store` (Bao-verified FsStore). Pulling the
//! algorithms into their own crate lets `adnet-cli`, `adnet-ffi`, and
//! future embedders (Swift/Kotlin FFI, Tauri, the relay) share one
//! implementation without dragging the whole CLI binary along.
//!
//! [`n0-computer/sendme`]: https://github.com/n0-computer/sendme

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod collection;
pub mod error;
pub mod metrics;
pub mod pairing_bridge;
pub mod path;
pub mod receive;
pub mod resume;
pub mod ticket;
pub mod walk;

#[cfg(feature = "iroh")]
pub mod remote;

#[cfg(feature = "iroh")]
pub mod push;

pub use collection::{Collection, CollectionEntry, MAX_COLLECTION_ENTRIES};
pub use error::{ShareError, ShareResult};
pub use metrics::{ShareMetrics, record_receive_complete, record_receive_expected, share_metrics};
pub use path::{MAX_NAME_LEN, canonicalized_path_to_string, validate_path_component};
pub use receive::{ReceiveOptions, ReceiveStats, receive};
pub use resume::{
    HASH_SHORT_LEN, RESUME_MANIFEST_FILENAME, RESUME_STATE_FILENAME, ResumeFileProgress,
    ResumeState, ResumeStatus, clean, has_cached_manifest, list, load, manifest_path,
    resume_dir, save,
};
pub use ticket::{ShareTicket, SHARE_TICKET_PREFIX, MAX_TICKET_LEN};
pub use walk::{PutBytesFn, WalkOptions, WalkStats, walk_import};
pub use pairing_bridge::{
    PairingShareBridge, PairingTicketBuilder, PairingTicketOptions, PairingShareError,
};

#[cfg(feature = "iroh")]
pub use remote::{RemoteFetchOptions, RemoteFetchOutcome, remote_fetch, receive_p2p, discover_endpoints};

#[cfg(feature = "iroh")]
pub use push::{PushOptions, PushOutcome, push_blobs, push_p2p};