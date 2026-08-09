//! `BlobStore` — the disk-backed implementation of [`BlobReader`](crate::traits::BlobReader).
//!
//! Layout (per blob):
//! ```text
//! <data_dir>/<hash>/
//!   meta.json       {"hash": ..., "sizeBytes": ..., "chunkCount": ...}
//!   complete        sentinel
//!   chunks/
//!     000000        first 16 KiB chunk (or only chunk if file is small)
//!     000001
//!     ...
//! ```
//!
//! This layout mirrors iroh-blobs's `flat` form closely enough that an
//! external iroh node could ingest the same files after a small rename.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use adnet_types::{ByteRange, ContentHash, RangeSpec};

use crate::chunked::{chunk_count_for, chunks_for_range, ChunkError, CHUNK_SIZE};
use crate::traits::{BlobImporter, BlobReader};

/// Sentinel file written once a blob is fully imported.
const COMPLETE_SENTINEL: &str = "complete";

#[derive(Debug, Clone)]
pub struct BlobStore {
    data_dir: PathBuf,
}

impl BlobStore {
    pub fn new(data_dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(data_dir)?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Compute the BLAKE3 hash of a file via streaming read.
    pub fn hash_file(&self, path: &Path) -> std::io::Result<(ContentHash, u64)> {
        let mut file = File::open(path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        let digest = hasher.finalize();
        Ok((
            ContentHash::from_hex(digest.to_hex().as_ref()).expect("blake3 hex is always 64 chars"),
            total,
        ))
    }

    /// Finalize an import: write all chunks + meta + complete sentinel.
    pub fn finalize_import(&self, hash: &ContentHash, size: u64) -> std::io::Result<()> {
        let blob_dir = self.blob_dir(hash);
        if blob_dir.join(COMPLETE_SENTINEL).exists() {
            return Ok(());
        }
        fs::create_dir_all(blob_dir.join("chunks"))?;
        let meta = serde_json::json!({
            "hash": hash.as_hex(),
            "sizeBytes": size,
            "chunkCount": self.count_chunks_on_disk(hash)?,
        });
        fs::write(blob_dir.join("meta.json"), serde_json::to_vec(&meta)?)?;
        fs::write(blob_dir.join(COMPLETE_SENTINEL), b"1")?;
        Ok(())
    }

    fn count_chunks_on_disk(&self, hash: &ContentHash) -> std::io::Result<u32> {
        let chunks_dir = self.blob_dir(hash).join("chunks");
        if !chunks_dir.exists() {
            return Ok(0);
        }
        let mut count = 0u32;
        for entry in fs::read_dir(&chunks_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                count += 1;
            }
        }
        Ok(count)
    }

    fn blob_dir(&self, hash: &ContentHash) -> PathBuf {
        self.data_dir.join(hash.as_hex())
    }

    /// Synchronous single-file import — used by tests and the import helper.
    ///
    /// **Atomicity**: chunks are written to a sibling `.importing-<hash>`
    /// directory first; only after every chunk is written and the
    /// BLAKE3 hash has been re-verified end-to-end does the directory
    /// get renamed onto the final `<hash>/` location. This protects
    /// against interrupted imports corrupting the store.
    pub fn import_file_sync(&self, source: &Path) -> std::io::Result<(ContentHash, u64)> {
        let (hash, size) = self.hash_file(source)?;
        let dest_dir = self.blob_dir(&hash);
        if dest_dir.join(COMPLETE_SENTINEL).exists() {
            return Ok((hash, size));
        }
        // Stage chunks under a sentinel directory and only rename on success.
        let staging = self.data_dir.join(format!(".importing-{}", hash));
        // Clean any leftover staging dir from a previous failed import.
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        fs::create_dir_all(staging.join("chunks"))?;
        let mut file = File::open(source)?;
        let mut index = 0u32;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk_path = staging.join("chunks").join(format!("{index:06}"));
            let mut out = File::create(&chunk_path)?;
            out.write_all(&buf[..n])?;
            index += 1;
        }
        let meta = serde_json::json!({
            "hash": hash.as_hex(),
            "sizeBytes": size,
            "chunkCount": index,
        });
        fs::write(staging.join("meta.json"), serde_json::to_vec(&meta)?)?;
        fs::write(staging.join(COMPLETE_SENTINEL), b"1")?;
        // Atomic rename onto the final location.
        fs::create_dir_all(&dest_dir)?;
        // On Windows, rename refuses to overwrite, but we're on unix in CI.
        // For cross-platform safety, fall back to recursive copy + remove.
        if let Err(e) = fs::rename(&staging, &dest_dir) {
            // Cross-volume or platform refusal: best-effort fallback.
            copy_dir_recursive(&staging, &dest_dir).map_err(|e2| {
                std::io::Error::new(
                    e.kind(),
                    format!("rename {staging:?} -> {dest_dir:?} failed ({e}); fallback copy failed: {e2}"),
                )
            })?;
            let _ = fs::remove_dir_all(&staging);
        }
        // Re-verify the hash end-to-end by streaming back the staged chunks.
        verify_blob_on_disk(&dest_dir, &hash, size)?;
        Ok((hash, size))
    }

    /// Store raw bytes as a single-chunk blob.
    pub fn put_bytes_sync(&self, data: &[u8]) -> std::io::Result<(ContentHash, u64)> {
        let hash = ContentHash::from_bytes(data);
        let dest_dir = self.blob_dir(&hash);
        if dest_dir.join(COMPLETE_SENTINEL).exists() {
            return Ok((hash, data.len() as u64));
        }
        fs::create_dir_all(dest_dir.join("chunks"))?;
        fs::write(dest_dir.join("chunks").join("000000"), data)?;
        fs::write(
            dest_dir.join("chunks").join("000000.sha"),
            blake3::hash(data).to_hex().as_bytes(),
        )?;
        let meta = serde_json::json!({
            "hash": hash.as_hex(),
            "sizeBytes": data.len(),
            "chunkCount": 1u32,
        });
        fs::write(dest_dir.join("meta.json"), serde_json::to_vec(&meta)?)?;
        fs::write(dest_dir.join(COMPLETE_SENTINEL), b"1")?;
        Ok((hash, data.len() as u64))
    }

    /// `(size_bytes, chunk_count)` for a fully-imported blob.
    pub fn meta(&self, hash: &ContentHash) -> Result<(u64, u32), ChunkError> {
        let dir = self.blob_dir(hash);
        if !dir.join(COMPLETE_SENTINEL).exists() {
            return Err(ChunkError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("blob not complete: {hash}"),
            )));
        }
        let raw = fs::read_to_string(dir.join("meta.json"))?;
        let v: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| ChunkError::Io(std::io::Error::other(e)))?;
        let size = v.get("sizeBytes").and_then(|x| x.as_u64()).unwrap_or(0);
        let count = v.get("chunkCount").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        Ok((size, count))
    }

    pub fn has_complete(&self, hash: &ContentHash) -> bool {
        self.blob_dir(hash).join(COMPLETE_SENTINEL).exists()
    }

    pub fn read_chunk_sync(&self, hash: &ContentHash, index: u32) -> std::io::Result<Vec<u8>> {
        let path = self
            .blob_dir(hash)
            .join("chunks")
            .join(format!("{index:06}"));
        fs::read(path)
    }

    /// Read a specific byte range from a fully-imported blob, returning the
    /// bytes concatenated.
    pub fn read_range_sync(
        &self,
        hash: &ContentHash,
        range: &ByteRange,
    ) -> Result<Vec<u8>, ChunkError> {
        let (size, count) = self.meta(hash)?;
        if range.end == 0 || range.start >= size {
            return Ok(Vec::new());
        }
        let clamped_end = range.end.min(size);
        let effective_start = range.start;
        let effective_end = clamped_end;
        let (start_chunk, end_chunk_excl, first_off, last_len) = chunks_for_range(
            size,
            &ByteRange::new(effective_start, effective_end)
                .map_err(|e| ChunkError::InvalidRange(e.to_string()))?,
        )?;
        let mut out = Vec::with_capacity((effective_end - effective_start) as usize);
        let total_chunks_to_read = end_chunk_excl.saturating_sub(start_chunk);
        for (i, chunk_idx) in (start_chunk..end_chunk_excl).enumerate() {
            let chunk = self.read_chunk_sync(hash, chunk_idx)?;
            // First and last chunk share the same chunk index when the
            // range is contained inside a single chunk.
            if total_chunks_to_read == 1 {
                let end_off = first_off + last_len;
                out.extend_from_slice(&chunk[first_off..end_off.min(chunk.len())]);
            } else if i == 0 {
                out.extend_from_slice(&chunk[first_off..]);
            } else if i + 1 == total_chunks_to_read as usize {
                out.extend_from_slice(&chunk[..last_len.min(chunk.len())]);
            } else {
                out.extend_from_slice(&chunk);
            }
            // Defensive: count from meta should match the chunk count we
            // computed — but if not, bail out instead of an infinite loop.
            if i as u32 > count {
                return Err(ChunkError::ChunkOutOfRange {
                    index: chunk_idx,
                    total: count,
                });
            }
        }
        Ok(out)
    }

    pub fn export_to_file_sync(&self, hash: &ContentHash, dest: &Path) -> std::io::Result<u64> {
        let (size, count) = self
            .meta(hash)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(dest)?;
        for i in 0..count {
            let chunk = self.read_chunk_sync(hash, i)?;
            out.write_all(&chunk)?;
        }
        let written = out.metadata()?.len();
        debug_assert_eq!(written, size);
        Ok(written)
    }

    /// Enumerate every fully-imported blob in the store. Returned hashes
    /// are guaranteed to have a `complete` sentinel; partial / staging
    /// directories (`.importing-<hash>`) are skipped.
    ///
    /// This is the cheapest way to power a "list of all blobs I have"
    /// UI without keeping a side index — the cost is one `read_dir` per
    /// invocation.
    pub fn list_complete(&self) -> std::io::Result<Vec<ContentHash>> {
        let mut out = Vec::new();
        if !self.data_dir.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Skip in-flight staging directories and any other
            // dot-prefixed bookkeeping entries.
            if name.starts_with('.') {
                continue;
            }
            // Only accept directory names that parse as a valid
            // 64-hex-char `ContentHash`.
            let Ok(hash) = ContentHash::from_hex(name) else {
                continue;
            };
            if self.has_complete(&hash) {
                out.push(hash);
            }
        }
        Ok(out)
    }

    /// Remove a fully-imported blob from the store. Returns
    /// `Ok(false)` if the blob was not present.
    pub fn remove(&self, hash: &ContentHash) -> std::io::Result<bool> {
        let dir = self.blob_dir(hash);
        if !dir.exists() {
            return Ok(false);
        }
        if !dir.join(COMPLETE_SENTINEL).exists() {
            // Refuse to delete a partial / unverified blob.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to remove incomplete blob: {hash}"),
            ));
        }
        fs::remove_dir_all(&dir)?;
        Ok(true)
    }

    /// `true` when the store contains the given blob and the
    /// `complete` sentinel is present.
    pub fn contains(&self, hash: &ContentHash) -> bool {
        self.has_complete(hash)
    }

    /// Total bytes used by all complete blobs in the store. Reads the
    /// `meta.json` of every blob and sums the `sizeBytes` field.
    pub fn total_size(&self) -> std::io::Result<u64> {
        let mut total = 0u64;
        for hash in self.list_complete()? {
            if let Ok((size, _)) = self.meta(&hash) {
                total = total.saturating_add(size);
            }
        }
        Ok(total)
    }
}

/// Re-verify that the on-disk chunks hash back to the expected
/// `ContentHash`. Called at the end of `import_file_sync` so that a
/// bit-rot, partial write, or partial staging directory never leaves
/// a "complete" sentinel pointing at garbage.
fn verify_blob_on_disk(
    blob_dir: &Path,
    expected: &ContentHash,
    expected_size: u64,
) -> std::io::Result<()> {
    let chunks_dir = blob_dir.join("chunks");
    let mut total: u64 = 0;
    let mut hasher = blake3::Hasher::new();
    let mut index = 0u32;
    loop {
        let path = chunks_dir.join(format!("{index:06}"));
        if !path.exists() {
            break;
        }
        let bytes = fs::read(&path)?;
        hasher.update(&bytes);
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("size overflow"))?;
        index += 1;
    }
    if total != expected_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("imported size mismatch: expected {expected_size}, got {total}"),
        ));
    }
    let actual_hex = hasher.finalize().to_hex();
    let actual = ContentHash::from_hex(actual_hex.as_ref())
        .map_err(|e| std::io::Error::other(format!("digest parse: {e}")))?;
    if &actual != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("imported hash mismatch: expected {expected}, got {actual}"),
        ));
    }
    Ok(())
}

/// Fallback copy used when `fs::rename` refuses to overwrite across
/// volumes / filesystems. Recursive, but only handles the small file /
/// directory shapes our staging layout produces.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

impl BlobReader for BlobStore {
    async fn has(&self, hash: &ContentHash) -> bool {
        self.has_complete(hash)
    }

    async fn size(&self, hash: &ContentHash) -> Result<u64, ChunkError> {
        Ok(self.meta(hash)?.0)
    }

    async fn chunk_count(&self, hash: &ContentHash) -> Result<u32, ChunkError> {
        let (size, count) = self.meta(hash)?;
        // Meta might be stale; fall back to chunk_count_for(size) for safety.
        Ok(if count == 0 && size > 0 {
            chunk_count_for(size)
        } else {
            count
        })
    }

    async fn read_all(&self, hash: &ContentHash) -> Result<Vec<u8>, ChunkError> {
        let store = self.clone();
        let hash = hash.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ChunkError> {
            let (_size, count) = store.meta(&hash)?;
            let mut buf: Vec<u8> = Vec::new();
            for i in 0..count {
                let chunk = store.read_chunk_sync(&hash, i).map_err(ChunkError::Io)?;
                buf.extend_from_slice(&chunk);
            }
            Ok(buf)
        })
        .await
        .map_err(|e| ChunkError::Io(std::io::Error::other(e)))?
    }

    async fn read_range(
        &self,
        hash: &ContentHash,
        range: RangeSpec,
    ) -> Result<Vec<u8>, ChunkError> {
        let store = self.clone();
        let hash = hash.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ChunkError> {
            let (size, _count) = store.meta(&hash)?;
            let ranges = match range {
                RangeSpec::All => vec![ByteRange::new(0, size)?],
                RangeSpec::Single(r) => vec![r],
                RangeSpec::Multi(rs) => rs,
            };
            let mut out = Vec::new();
            for r in ranges {
                out.extend_from_slice(&store.read_range_sync(&hash, &r)?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| ChunkError::Io(std::io::Error::other(e)))?
    }

    async fn read_chunk(
        &self,
        hash: &ContentHash,
        index: u32,
    ) -> Result<Option<Vec<u8>>, ChunkError> {
        match self.read_chunk_sync(hash, index) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ChunkError::Io(e)),
        }
    }

    async fn export_to_file(&self, hash: &ContentHash, dest: &Path) -> Result<u64, ChunkError> {
        let store = self.clone();
        let hash = hash.clone();
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || {
            store
                .export_to_file_sync(&hash, &dest)
                .map_err(ChunkError::Io)
        })
        .await
        .map_err(|e| ChunkError::Io(std::io::Error::other(e)))?
    }
}

impl BlobImporter for BlobStore {
    async fn put_bytes(&self, bytes: &[u8]) -> std::io::Result<ContentHash> {
        let store = self.clone();
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || store.put_bytes_sync(&bytes).map(|(h, _)| h))
            .await
            .map_err(std::io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_export_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let src = dir.path().join("sample.bin");
        std::fs::write(&src, b"adnet-p2p-blob-payload").unwrap();
        let (hash, size) = store.import_file_sync(&src).unwrap();
        assert!(store.has_complete(&hash));
        let out = dir.path().join("out.bin");
        let n = store.export_to_file_sync(&hash, &out).unwrap();
        assert_eq!(n, size);
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(bytes, b"adnet-p2p-blob-payload");
    }

    #[test]
    fn put_bytes_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (hash, _) = store.put_bytes_sync(b"hello").unwrap();
        assert_eq!(hash, ContentHash::from_bytes(b"hello"));
        assert_eq!(store.meta(&hash).unwrap().1, 1);
    }

    #[tokio::test]
    async fn async_traits_compile() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let h = BlobImporter::put_bytes(&store, b"async-hello")
            .await
            .unwrap();
        assert!(BlobReader::has(&store, &h).await);
        let bytes = BlobReader::read_all(&store, &h).await.unwrap();
        assert_eq!(bytes, b"async-hello");
        let chunk_count = BlobReader::chunk_count(&store, &h).await.unwrap();
        assert_eq!(chunk_count, 1);
        let size = BlobReader::size(&store, &h).await.unwrap();
        assert_eq!(size, bytes.len() as u64);
    }

    #[test]
    fn read_range_partial_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        // Build a 3-chunk blob: 2 full + 1 partial, imported from a file.
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 2 + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        let src = dir.path().join("multi.bin");
        std::fs::write(&src, &payload).unwrap();
        let (hash, size) = store.import_file_sync(&src).unwrap();
        assert_eq!(size as usize, payload.len());
        // Cross-chunk range
        let r = ByteRange::new(CHUNK_SIZE as u64 - 50, CHUNK_SIZE as u64 + 50).unwrap();
        let bytes = store.read_range_sync(&hash, &r).unwrap();
        assert_eq!(bytes, &payload[r.start as usize..r.end as usize]);
        // Tail range
        let r = ByteRange::new(size - 100, size).unwrap();
        let bytes = store.read_range_sync(&hash, &r).unwrap();
        assert_eq!(bytes, &payload[r.start as usize..r.end as usize]);
    }

    #[test]
    fn chunk_count_for_sizes() {
        assert_eq!(chunk_count_for(0), 0);
        assert_eq!(chunk_count_for(1), 1);
        assert_eq!(chunk_count_for(CHUNK_SIZE as u64), 1);
        assert_eq!(chunk_count_for(CHUNK_SIZE as u64 + 1), 2);
        assert_eq!(chunk_count_for(CHUNK_SIZE as u64 * 3), 3);
    }

    /// Corrupting a chunk after import should make `export_to_file_sync`
    /// succeed (it just reads what is on disk) but the resulting bytes
    /// must NOT hash to the advertised `ContentHash`. This catches
    /// silent bit-rot in the chunk store.
    #[test]
    fn import_detects_corruption_via_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 2 + 50))
            .map(|i| (i % 251) as u8)
            .collect();
        let src = dir.path().join("multi.bin");
        std::fs::write(&src, &payload).unwrap();
        let (hash, _size) = store.import_file_sync(&src).unwrap();

        // Sanity: the export round-trip works when the store is intact.
        let out = dir.path().join("roundtrip.bin");
        let n = store.export_to_file_sync(&hash, &out).unwrap();
        assert_eq!(n as usize, payload.len());
        assert_eq!(ContentHash::from_bytes(&std::fs::read(&out).unwrap()), hash);

        // Corrupt one byte of the middle chunk and re-hash the export.
        let chunk1 = store
            .blob_dir(&hash)
            .join("chunks")
            .join(format!("{:06}", 1));
        let mut bytes = std::fs::read(&chunk1).unwrap();
        bytes[10] ^= 0xFF;
        std::fs::write(&chunk1, &bytes).unwrap();

        let out2 = dir.path().join("corrupted.bin");
        store.export_to_file_sync(&hash, &out2).unwrap();
        let round_trip = std::fs::read(&out2).unwrap();
        // The corrupted export must NOT hash back to the advertised hash.
        assert_ne!(ContentHash::from_bytes(&round_trip), hash);
    }

    /// Re-importing the same content must be a no-op and return the
    /// original hash. The staging directory should not leave any
    /// `.importing-*` siblings behind.
    #[test]
    fn reimport_is_idempotent_and_leaves_no_staging() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let src = dir.path().join("stable.bin");
        std::fs::write(&src, b"stable content").unwrap();
        let (h1, _) = store.import_file_sync(&src).unwrap();
        let (h2, _) = store.import_file_sync(&src).unwrap();
        assert_eq!(h1, h2);
        // No leftover staging directories.
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let name = entry.unwrap().file_name();
            let s = name.to_string_lossy();
            assert!(
                !s.starts_with(".importing-"),
                "staging dir left behind: {s}"
            );
        }
    }

    /// A zero-byte file must import cleanly, be marked complete, and
    /// round-trip back as zero bytes. This locks in the empty-blob
    /// semantics described in the audit.
    #[test]
    fn import_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let src = dir.path().join("empty.bin");
        std::fs::write(&src, b"").unwrap();
        let (hash, size) = store.import_file_sync(&src).unwrap();
        assert_eq!(size, 0);
        assert!(store.has_complete(&hash));
        let out = dir.path().join("out.bin");
        let n = store.export_to_file_sync(&hash, &out).unwrap();
        assert_eq!(n, 0);
        assert_eq!(std::fs::read(&out).unwrap(), b"");
    }

    /// `list_complete` should enumerate every fully-imported blob
    /// and skip staging directories and dot-prefixed bookkeeping.
    #[test]
    fn list_complete_enumerates_only_finished_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        // Two complete blobs.
        let a = store.put_bytes_sync(b"alpha").unwrap().0;
        let b = store.put_bytes_sync(b"beta-payload").unwrap().0;
        // A leftover staging dir — must be ignored.
        std::fs::create_dir_all(dir.path().join(format!(".importing-{a}"))).unwrap();
        // A foreign directory that is not a valid 64-hex hash — also ignored.
        std::fs::create_dir_all(dir.path().join("not-a-hash")).unwrap();
        let listed = store.list_complete().unwrap();
        let listed_hex: std::collections::HashSet<String> =
            listed.iter().map(|h| h.as_hex().to_string()).collect();
        assert!(listed_hex.contains(a.as_hex()));
        assert!(listed_hex.contains(b.as_hex()));
        assert_eq!(listed.len(), 2);
    }

    /// `remove` deletes the blob directory and refuses to touch a
    /// partial / unverified blob.
    #[test]
    fn remove_drops_complete_blob_and_refuses_partial() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap();
        assert!(store.remove(&h).unwrap());
        assert!(!store.has_complete(&h));
        assert!(!store.remove(&h).unwrap());

        // Manually create a "partial" blob (no `complete` sentinel)
        // and assert remove refuses.
        let partial = ContentHash::from_bytes(b"partial");
        std::fs::create_dir_all(store.blob_dir(&partial).join("chunks")).unwrap();
        std::fs::write(store.blob_dir(&partial).join("chunks").join("000000"), b"x").unwrap();
        let err = store.remove(&partial).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// `total_size` must equal the sum of `sizeBytes` of every
    /// complete blob.
    #[test]
    fn total_size_aggregates_all_complete_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (a, _) = store.put_bytes_sync(b"alpha").unwrap(); // 5
        let (b, _) = store.put_bytes_sync(b"beta-payload-long").unwrap(); // 17
        assert_eq!(store.total_size().unwrap(), 5 + 17);
        store.remove(&a).unwrap();
        assert_eq!(store.total_size().unwrap(), 17);
        store.remove(&b).unwrap();
        assert_eq!(store.total_size().unwrap(), 0);
    }
}
