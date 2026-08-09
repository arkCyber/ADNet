//! `adnet-blobstore` — disk-backed BLAKE3 chunked blob store.
//!
//! Layout (matches iroh-blobs so a future iroh-backed adapter can read the
//! same on-disk format directly):
//!
//! ```text
//! {data_dir}/{hash_hex}/
//!     meta.json          # { hash, sizeBytes, chunkCount }
//!     complete           # sentinel file; presence == blob is complete
//!     chunks/
//!         000000
//!         000001
//!         ...
//! ```
//!
//! Chunk size is 16 KiB — aligned with iroh-blobs' group granularity.
//!
//! This crate is intentionally sync (filesystem + blake3 are fast). The
//! higher layers wrap it in `tokio::task::spawn_blocking` for async use.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod chunked;
pub mod filename;
pub mod import;
#[cfg(feature = "iroh")]
pub mod iroh_store;
pub mod store;
pub mod traits;

pub use chunked::{ChunkReader, ChunkWriter, CHUNK_SIZE};
pub use filename::{safe_filename, MAX_FILENAME_LEN};
#[cfg(feature = "iroh")]
pub use iroh_store::{
    content_hash_to_iroh_hash, iroh_hash_to_content_hash, IrohBlobHash, IrohBlobStore,
    IrohBlobTicket, MAX_RANGE_BYTES,
};
pub use store::BlobStore;
pub use traits::{BlobImporter, BlobReader};
