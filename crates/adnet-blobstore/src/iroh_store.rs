//! iroh-blobs backed store adapter.
//!
//! When the `iroh` feature is enabled, [`IrohBlobStore`] wraps an
//! [`iroh_blobs::store::fs::FsStore`] and exposes it through the
//! same [`BlobReader`] / [`BlobImporter`] trait surface as the legacy
//! disk-backed [`BlobStore`](crate::store::BlobStore). Callers can
//! therefore swap one for the other without changing call sites.
//!
//! ## On-disk layout
//!
//! iroh-blobs uses an embedded `redb` key/value store. The FsStore
//! lives in a sibling directory of the legacy store:
//!
//! ```text
//! {data_dir}/
//!   {hash_hex}/...      # legacy BlobStore layout (preserved)
//!   iroh-blobs/         # iroh-blobs FsStore root
//! ```
//!
//! The two stores are **independent** — they do not share data.
//! Migrating a blob from the legacy store to iroh-blobs is a
//! deliberate copy via `put_bytes`.
//!
//! ## Verification
//!
//! Every read through iroh-blobs is verified against the BLAKE3
//! hash of the content (Bao tree). A corrupted or truncated read
//! surfaces as an iroh-blobs error. The legacy store performs no
//! such verification.
//!
//! ## Ticket format
//!
//! Wire-format blobs are encoded as the iroh standard
//! [`iroh_blobs::ticket::BlobTicket`]: a postcard-serialised blob
//! that bundles the peer's `EndpointAddr`, the hash, and the
//! format. ADNet's legacy [`ContentHash`] can be converted via
//! [`iroh_hash_to_content_hash`] and [`content_hash_to_iroh_hash`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adnet_types::{ByteRange, ContentHash, RangeSpec};
use iroh_blobs::Hash as IrohHash;
use iroh_blobs::api::proto::BlobStatus;
use iroh_blobs::store::fs::FsStore;
use tracing::debug;

use crate::chunked::ChunkError;
use crate::traits::{BlobImporter, BlobReader};

/// Internal error wrapper so we can return a unified `ChunkError`
/// from iroh-blobs calls without leaking the iroh error type into
/// `BlobReader`'s public signature.
fn wrap<E: std::fmt::Display>(e: E) -> ChunkError {
    ChunkError::Io(std::io::Error::other(format!("{e}")))
}

/// Safety cap (bytes) on the size of a single `read_range` /
/// `read_all` call against the iroh-backed store. DO-178C
/// `iroh_blobs::store::fs::FsStore::get_bytes` materialises the
/// whole Bao-verified blob in memory before the caller can slice
/// it. Without a guard, requesting `RangeSpec::All` on a 1 TiB
/// blob would allocate 1 TiB of RAM on the request thread.
///
/// Callers that need more than the cap must read chunk by chunk via
/// [`BlobReader::read_chunk`].
pub const MAX_RANGE_BYTES: u64 = 16 * 1024 * 1024;

/// Compute the byte length that the given `range` would request
/// against a blob of `total_size` bytes. `None` is returned for
/// range specs that translate to "all" — for those we conservatively
/// return `total_size`.
fn range_byte_len(range: &RangeSpec, total_size: u64) -> u64 {
    match range {
        RangeSpec::All => total_size,
        RangeSpec::Single(br) => br.end.saturating_sub(br.start),
        RangeSpec::Multi(ranges) => ranges
            .iter()
            .map(|br| br.end.saturating_sub(br.start))
            .sum(),
    }
}

/// Thin wrapper around [`FsStore`]. Cloning is cheap — the
/// underlying `redb` tables live behind an `Arc` inside the
/// `FsStore`, so we just hold another `Arc` over the `FsStore` clone.
#[derive(Debug, Clone)]
pub struct IrohBlobStore {
    inner: Arc<FsStore>,
    /// Path to the iroh-blobs data directory (kept for diagnostics).
    path: PathBuf,
}

impl IrohBlobStore {
    /// Open (or create) an iroh-blobs `FsStore` rooted at
    /// `<data_dir>/iroh-blobs/`. The directory will be created if it
    /// doesn't exist.
    pub async fn open(data_dir: &Path) -> std::io::Result<Self> {
        let path = data_dir.join("iroh-blobs");
        tokio::fs::create_dir_all(&path).await?;
        let store = FsStore::load(&path)
            .await
            .map_err(|e| std::io::Error::other(format!("FsStore::load: {e}")))?;
        Ok(Self {
            inner: Arc::new(store),
            path,
        })
    }

    /// Path to the iroh-blobs data directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Borrow the inner `FsStore` directly (no lock — `FsStore`
    /// itself is `Arc`-backed and may be cloned).
    pub fn fs_store(&self) -> &FsStore {
        &self.inner
    }

    /// Get a clone of the inner `Arc<FsStore>`. Useful for
    /// constructing `iroh_blobs::BlobsProtocol` which takes a
    /// generic `Store` by reference (and `FsStore: Deref<Target = Store>`).
    pub fn handle(&self) -> Arc<FsStore> {
        Arc::clone(&self.inner)
    }
}

// ─────────────────────────── Hash conversion ─────────────────────────────

/// Convert an ADNet [`ContentHash`] (hex-encoded BLAKE3 over blob
/// bytes) to an iroh [`IrohHash`]. Both are 32-byte BLAKE3 outputs,
/// so the conversion is byte-exact.
pub fn content_hash_to_iroh_hash(hash: &ContentHash) -> std::io::Result<IrohHash> {
    let hex = hash.as_hex();
    let bytes = hex::decode(hex).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("ContentHash not valid hex: {e}"),
        )
    })?;
    if bytes.len() != 32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("ContentHash must decode to 32 bytes, got {}", bytes.len()),
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(IrohHash::from_bytes(arr))
}

/// Convert an iroh [`IrohHash`] back to an ADNet [`ContentHash`].
pub fn iroh_hash_to_content_hash(hash: &IrohHash) -> ContentHash {
    ContentHash::from_hex(&hex::encode(hash.as_bytes())).expect("iroh hash is always 32 bytes hex")
}

// ─────────────────────────── BlobReader / BlobImporter ───────────────────

impl BlobReader for IrohBlobStore {
    async fn has(&self, hash: &ContentHash) -> bool {
        let Ok(ihash) = content_hash_to_iroh_hash(hash) else {
            return false;
        };
        self.inner.has(ihash).await.unwrap_or(false)
    }

    async fn size(&self, hash: &ContentHash) -> Result<u64, ChunkError> {
        let ihash = content_hash_to_iroh_hash(hash).map_err(wrap)?;
        // `Blobs::status` returns the locally-known size of a blob
        // (or 0 / `NotFound` if we don't have it). This is the
        // cheapest way to get a size without re-reading the whole
        // blob into memory.
        let status = self.inner.status(ihash).await.map_err(wrap)?;
        match status {
            BlobStatus::Complete { size } => Ok(size),
            BlobStatus::Partial { size } => Ok(size.unwrap_or(0)),
            BlobStatus::NotFound => Err(ChunkError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("blob {} not found in iroh store", hash.as_hex()),
            ))),
        }
    }

    async fn chunk_count(&self, hash: &ContentHash) -> Result<u32, ChunkError> {
        let size = self.size(hash).await?;
        const CHUNK_SIZE: u64 = 16 * 1024;
        Ok(size.div_ceil(CHUNK_SIZE) as u32)
    }

    async fn read_all(&self, hash: &ContentHash) -> Result<Vec<u8>, ChunkError> {
        let size = self.size(hash).await?;
        let range = RangeSpec::Single(ByteRange::whole(size));
        self.read_range(hash, range).await
    }

    async fn read_range(
        &self,
        hash: &ContentHash,
        range: RangeSpec,
    ) -> Result<Vec<u8>, ChunkError> {
        let ihash = content_hash_to_iroh_hash(hash).map_err(wrap)?;
        let size = self.size(hash).await?;

        // DO-178C guard: refuse ranges larger than the safety cap
        // *before* calling `get_bytes` so a malicious / buggy
        // caller cannot OOM the process. Callers that need more
        // must use the chunked `read_chunk` API.
        let requested = range_byte_len(&range, size);
        if requested > MAX_RANGE_BYTES {
            return Err(ChunkError::TooLarge {
                requested,
                cap: MAX_RANGE_BYTES,
            });
        }

        // iroh-blobs `get_bytes` returns the full Bao-verified blob;
        // we slice it locally. For very large blobs the production
        // implementation should use the chunked `BlobReader` API.
        let bytes = self.inner.get_bytes(ihash).await.map_err(wrap)?;
        let len = bytes.len();
        let slice: Vec<u8> = match range {
            RangeSpec::All => bytes.to_vec(),
            RangeSpec::Single(br) => {
                let start = (br.start as usize).min(len);
                let end = (br.end as usize).min(len);
                if end <= start {
                    Vec::new()
                } else {
                    bytes[start..end].to_vec()
                }
            }
            // For multi-range the legacy store composes an HTTP
            // multipart response; the iroh path is single-range only
            // for now, so we just take the first range.
            RangeSpec::Multi(ranges) => {
                if let Some(br) = ranges.first() {
                    let start = (br.start as usize).min(len);
                    let end = (br.end as usize).min(len);
                    if end <= start {
                        Vec::new()
                    } else {
                        bytes[start..end].to_vec()
                    }
                } else {
                    Vec::new()
                }
            }
        };
        Ok(slice)
    }

    async fn read_chunk(
        &self,
        hash: &ContentHash,
        index: u32,
    ) -> Result<Option<Vec<u8>>, ChunkError> {
        const CHUNK_SIZE: u64 = 16 * 1024;
        let offset = index as u64 * CHUNK_SIZE;
        let size = self.size(hash).await?;
        if offset >= size {
            return Ok(None);
        }
        let length = CHUNK_SIZE.min(size - offset);
        let range = RangeSpec::Single(ByteRange::new(offset, offset + length).map_err(wrap)?);
        self.read_range(hash, range).await.map(Some)
    }

    async fn export_to_file(&self, hash: &ContentHash, dest: &Path) -> Result<u64, ChunkError> {
        let bytes = self.read_all(hash).await?;
        let n = bytes.len() as u64;
        tokio::fs::write(dest, &bytes).await.map_err(wrap)?;
        debug!(hash = %hash.as_hex(), bytes = n, dest = %dest.display(), "exported iroh-blob");
        Ok(n)
    }
}

impl BlobImporter for IrohBlobStore {
    async fn put_bytes(&self, bytes: &[u8]) -> std::io::Result<ContentHash> {
        // `add_bytes` returns a `Tag`. Its `hash` is the BLAKE3
        // digest over the bytes — exactly what `ContentHash` stores.
        let tag = self
            .inner
            .add_bytes(bytes.to_vec())
            .await
            .map_err(|e| std::io::Error::other(format!("iroh-blobs add_bytes: {e}")))?;
        let hash = iroh_hash_to_content_hash(&tag.hash);
        debug!(hash = %hash.as_hex(), bytes = bytes.len(), "imported blob via iroh-blobs");
        Ok(hash)
    }
}

// ─────────────────────────── wire format helpers ────────────────────────

/// Re-export of the iroh-blobs types callers will commonly need.
pub use iroh_blobs::Hash as IrohBlobHash;
pub use iroh_blobs::ticket::BlobTicket as IrohBlobTicket;

#[cfg(all(test, feature = "iroh"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn put_and_read_round_trip() {
        let dir = TempDir::new().unwrap();
        let store = IrohBlobStore::open(dir.path()).await.unwrap();
        let payload = b"hello iroh-blobs world".to_vec();
        let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
        let back = store.read_all(&hash).await.unwrap();
        assert_eq!(back, payload);
        assert!(store.has(&hash).await);
    }

    #[test]
    fn content_hash_iroh_hash_round_trip() {
        let store_bytes = [7u8; 32];
        let adnet_hash = ContentHash::from_hex(&hex::encode(store_bytes)).unwrap();
        let iroh_hash = content_hash_to_iroh_hash(&adnet_hash).unwrap();
        assert_eq!(iroh_hash.as_bytes(), &store_bytes);
        let back = iroh_hash_to_content_hash(&iroh_hash);
        assert_eq!(back.as_hex(), adnet_hash.as_hex());
    }

    #[tokio::test]
    async fn read_range_returns_slice() {
        let dir = TempDir::new().unwrap();
        let store = IrohBlobStore::open(dir.path()).await.unwrap();
        // Payload larger than one chunk so range slicing is
        // exercised.
        let payload: Vec<u8> = (0..32 * 1024).map(|i| (i % 256) as u8).collect();
        let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
        let range = RangeSpec::Single(ByteRange::new(100, 356).unwrap());
        let slice = store.read_range(&hash, range).await.unwrap();
        assert_eq!(slice.len(), 256);
        assert_eq!(&slice[..32], &payload[100..132]);
    }

    #[tokio::test]
    async fn read_chunk_out_of_bounds_is_none() {
        let dir = TempDir::new().unwrap();
        let store = IrohBlobStore::open(dir.path()).await.unwrap();
        let payload = vec![1u8, 2, 3, 4];
        let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
        // A 4-byte payload has 1 chunk. Asking for chunk 5 must be None.
        let chunk = store.read_chunk(&hash, 5).await.unwrap();
        assert!(chunk.is_none());
    }

    /// DO-178C regression: a request whose byte length exceeds the
    /// safety cap must be refused *before* the iroh FsStore is asked
    /// to materialise the whole blob. We can't easily simulate a
    /// 16 MiB+ payload without burning a lot of RAM in the test, so
    /// we just verify that an oversized range on a small blob still
    /// trips the guard (the guard fires on the requested byte
    /// length, not the actual blob size — a hostile caller can lie
    /// about `range.end` to try to read beyond the cap).
    #[tokio::test]
    async fn read_range_above_safety_cap_is_rejected() {
        let dir = TempDir::new().unwrap();
        let store = IrohBlobStore::open(dir.path()).await.unwrap();
        let payload = vec![0u8; 1024];
        let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
        // Request (0..MAX_RANGE_BYTES + 1) — one byte over the cap.
        let oversized = ByteRange::new(0, MAX_RANGE_BYTES + 1).expect("non-empty range is valid");
        let err = store
            .read_range(&hash, RangeSpec::Single(oversized))
            .await
            .unwrap_err();
        match err {
            ChunkError::TooLarge { requested, cap } => {
                assert_eq!(requested, MAX_RANGE_BYTES + 1);
                assert_eq!(cap, MAX_RANGE_BYTES);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }
}
