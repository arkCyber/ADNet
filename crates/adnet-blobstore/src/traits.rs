//! Async traits — pluggable blob reader/importer for any backend.
//!
//! Async method style is **RPITIT** (native `async fn` in trait). For
//! iroh-style pluggability the trait still works with `Box<dyn ...>`
//! provided the crate uses the `dyn-compat` style — `async_trait` users can
//! add a thin shim.

use std::path::Path;

use adnet_types::{ContentHash, RangeSpec};

use crate::chunked::ChunkError;

/// Trait for any backend that can yield blob bytes for a known hash.
pub trait BlobReader: Send + Sync + 'static {
    /// True if the backend has a complete local copy.
    fn has(&self, hash: &ContentHash) -> impl std::future::Future<Output = bool> + Send;

    /// Total bytes for the blob (after import).
    fn size(
        &self,
        hash: &ContentHash,
    ) -> impl std::future::Future<Output = Result<u64, ChunkError>> + Send;

    /// Number of 16 KiB chunks (or chunk count + partial).
    fn chunk_count(
        &self,
        hash: &ContentHash,
    ) -> impl std::future::Future<Output = Result<u32, ChunkError>> + Send;

    /// Read the full blob into a freshly-allocated buffer.
    fn read_all(
        &self,
        hash: &ContentHash,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, ChunkError>> + Send;

    /// Read a sub-range of the blob.
    ///
    /// Mirrors `iroh-blobs`' "provide bytes by range" — useful for HTTP
    /// mesh fallback (`Range:` headers) and selective download of large
    /// files. `range` is interpreted relative to the full blob.
    fn read_range(
        &self,
        hash: &ContentHash,
        range: RangeSpec,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, ChunkError>> + Send;

    /// Read a single 16 KiB chunk by index (0-based).
    fn read_chunk(
        &self,
        hash: &ContentHash,
        index: u32,
    ) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, ChunkError>> + Send;

    /// Export the blob to a destination file.
    fn export_to_file(
        &self,
        hash: &ContentHash,
        dest: &Path,
    ) -> impl std::future::Future<Output = Result<u64, ChunkError>> + Send;
}

/// Trait for any backend that can accept blob bytes.
pub trait BlobImporter: Send + Sync + 'static {
    /// Store `bytes` as a single-chunk blob, returning the hash.
    fn put_bytes(
        &self,
        bytes: &[u8],
    ) -> impl std::future::Future<Output = std::io::Result<ContentHash>> + Send;
}
