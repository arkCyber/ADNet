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

use crate::chunked::{CHUNK_SIZE, ChunkError, chunk_count_for, chunks_for_range};
use crate::traits::{BlobImporter, BlobReader};

/// Sentinel file written once a blob is fully imported.
const COMPLETE_SENTINEL: &str = "complete";

/// Storage statistics for monitoring and health checks.
#[derive(Debug, Clone)]
pub struct StorageStats {
    pub data_dir: PathBuf,
    pub total_blobs: usize,
    pub total_size_bytes: u64,
}

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

    /// Check if the blob store is healthy (data directory is accessible).
    /// Returns true if the data directory exists and is readable.
    pub fn is_healthy(&self) -> bool {
        // Check if data directory exists and is accessible
        match fs::metadata(&self.data_dir) {
            Ok(meta) => meta.is_dir(),
            Err(_) => false,
        }
    }

    /// Get storage statistics for monitoring.
    pub fn storage_stats(&self) -> StorageStats {
        let total_blobs = self.list_complete().unwrap_or_default().len();
        let total_size = self.total_size().unwrap_or(0);
        
        StorageStats {
            data_dir: self.data_dir.clone(),
            total_blobs,
            total_size_bytes: total_size,
        }
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

    /// Returns the directory path for a given content hash.
    pub fn blob_dir(&self, hash: &ContentHash) -> PathBuf {
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
            // Idempotent: blob already complete — still count as an import.
            crate::metrics::blob_metrics().imports.inc();
            return Ok((hash, size));
        }

        // Stage chunks under a sentinel directory and only rename on success.
        let staging = self.data_dir.join(format!(".importing-{}", hash));
        // Clean any leftover staging dir from a previous failed import.
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }

        let result = (|| -> std::io::Result<(ContentHash, u64)> {
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
        })();

        match result {
            Ok((h, s)) => {
                crate::metrics::blob_metrics().imports.inc();
                Ok((h, s))
            }
            Err(e) => {
                crate::metrics::blob_metrics().import_errors.inc();
                Err(e)
            }
        }
    }

    /// Store raw bytes as a chunked blob (split into CHUNK_SIZE pieces).
    pub fn put_bytes_sync(&self, data: &[u8]) -> std::io::Result<(ContentHash, u64)> {
        let hash = ContentHash::from_bytes(data);
        let dest_dir = self.blob_dir(&hash);
        if dest_dir.join(COMPLETE_SENTINEL).exists() {
            // Idempotent: already complete.
            crate::metrics::blob_metrics().put_bytes.inc();
            return Ok((hash, data.len() as u64));
        }
        let result = (|| -> std::io::Result<(ContentHash, u64)> {
            fs::create_dir_all(dest_dir.join("chunks"))?;
            // Write data in CHUNK_SIZE chunks, mirroring import_file_sync.
            let mut index = 0u32;
            for chunk in data.chunks(CHUNK_SIZE) {
                let chunk_path = dest_dir.join("chunks").join(format!("{index:06}"));
                fs::write(&chunk_path, chunk)?;
                fs::write(
                    chunk_path.with_extension("sha"),
                    blake3::hash(chunk).to_hex().as_bytes(),
                )?;
                index += 1;
            }
            let meta = serde_json::json!({
                "hash": hash.as_hex(),
                "sizeBytes": data.len(),
                "chunkCount": index,
            });
            fs::write(dest_dir.join("meta.json"), serde_json::to_vec(&meta)?)?;
            fs::write(dest_dir.join(COMPLETE_SENTINEL), b"1")?;
            Ok((hash, data.len() as u64))
        })();
        match result {
            Ok(r) => {
                crate::metrics::blob_metrics().put_bytes.inc();
                Ok(r)
            }
            Err(e) => {
                crate::metrics::blob_metrics().import_errors.inc();
                Err(e)
            }
        }
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

    /// Read the entire blob into a `Vec<u8>`. Returns `None` if the
    /// blob is not yet complete on disk. Used by the gateway's DAG service.
    pub fn get_sync(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        if !self.has_complete(hash) {
            return None;
        }
        let (size, _) = self.meta(hash).ok()?;
        let range = ByteRange::new(0, size).ok()?;
        self.read_range_sync(hash, &range).ok()
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
        let result = (|| -> std::io::Result<u64> {
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
        })();
        match result {
            Ok(n) => {
                crate::metrics::blob_metrics().exports.inc();
                Ok(n)
            }
            Err(e) => {
                crate::metrics::blob_metrics().import_errors.inc();
                Err(e)
            }
        }
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
            crate::metrics::blob_metrics().import_errors.inc();
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to remove incomplete blob: {hash}"),
            ));
        }
        fs::remove_dir_all(&dir)?;
        crate::metrics::blob_metrics().removes.inc();
        Ok(true)
    }

    /// Garbage-collect every blob that is **not** in `pins`. Returns
    /// the hashes that were deleted, in lexicographic order so the
    /// result is deterministic for tests.
    ///
    /// This is the primitive that wires `PinSet` to actual on-disk
    /// pruning — see [`crate::pin_set`] for the model.
    ///
    /// Behaviour:
    /// * **Incomplete** blobs (no `complete` sentinel) are skipped,
    ///   matching `remove`'s safety check.
    /// * **Errors** during a single `remove` are **not** fatal — the
    ///   offending hash is omitted from the result and the loop
    ///   continues. This avoids a half-deleted store after a
    ///   transient filesystem error.
    /// * A `tracing::warn!` is emitted for every failed deletion so
    ///   operators can correlate the GC result against their logs.
    pub fn gc_orphans(&self, pins: &crate::pin_set::PinSet) -> std::io::Result<Vec<ContentHash>> {
        let all_hex: Vec<String> = self
            .list_complete()?
            .iter()
            .map(|h| h.as_hex().to_string())
            .collect();
        let mut removed = Vec::new();
        for orphan_hex in pins.orphans(&all_hex) {
            // Re-parse so we use the canonical ContentHash for `remove`.
            let Ok(h) = ContentHash::from_hex(orphan_hex) else {
                tracing::warn!(orphan_hex, "gc_orphans: invalid hex in pin set, skipping");
                continue;
            };
            match self.remove(&h) {
                Ok(true) => removed.push(h),
                Ok(false) => {} // already gone
                Err(e) => {
                    tracing::warn!(hash = %h, error = %e, "gc_orphans: failed to remove orphan");
                }
            }
        }
        // Deterministic output for tests + logs.
        removed.sort_by(|a, b| a.as_hex().cmp(b.as_hex()));
        Ok(removed)
    }

    /// Like [`gc_orphans`] but the caller passes the pinned set
    /// directly as an iterable of hex CIDs. Useful when the
    /// [`crate::pin_set::PinSet`] is loaded elsewhere (e.g. the CLI's
    /// `repo gc --prune-unpinned` path).
    pub fn gc_unpinned<I, S>(&self, pinned: I) -> std::io::Result<Vec<ContentHash>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let pin_set: std::collections::HashSet<String> =
            pinned.into_iter().map(|s| s.as_ref().to_string()).collect();
        let all = self.list_complete()?;
        let mut removed = Vec::new();
        for h in all {
            // Compare against the hex form to avoid needing the
            // caller to coerce to `String` — the explicit
            // `pin_set.contains::<String>(&h_hex)` keeps the type
            // checker happy without losing the borrow API.
            let h_hex = h.as_hex();
            // `HashSet::contains` can't infer the borrow target
            // when handed a `&str` (it might want `String`,
            // `&str`, or `&String`), so we do an explicit
            // string comparison against the pre-built set.
            let keep = pin_set.iter().any(|p| p == h_hex);
            if !keep {
                match self.remove(&h) {
                    Ok(true) => removed.push(h),
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(hash = %h, error = %e, "gc_unpinned: failed to remove");
                    }
                }
            }
        }
        removed.sort_by(|a, b| a.as_hex().cmp(b.as_hex()));
        Ok(removed)
    }

    /// Drop **every** blob from the store. Used by `adbnet repo gc
    /// --prune-all` which is the operator's "reset" button. Returns
    /// the hashes that were deleted, in deterministic order.
    pub fn gc_all(&self) -> std::io::Result<Vec<ContentHash>> {
        let all = self.list_complete()?;
        let mut removed = Vec::new();
        for h in all {
            match self.remove(&h) {
                Ok(true) => removed.push(h),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(hash = %h, error = %e, "gc_all: failed to remove");
                }
            }
        }
        removed.sort_by(|a, b| a.as_hex().cmp(b.as_hex()));
        Ok(removed)
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

    /// Refresh the `adnet_blob_store_size_bytes` and
    /// `adnet_blob_blobs_total` gauges from the current store
    /// state. The other blob metrics (`imports_total`,
    /// `import_errors_total`, `put_bytes_total`, `exports_total`,
    /// `removes_total`) are wired at the call sites; only the
    /// gauges need a sweep because they reflect *current state*
    /// rather than cumulative counters.
    ///
    /// On any I/O error the gauges are left at their previous
    /// values — gauges are best-effort and a partial sweep must
    /// never block a /metrics scrape. The error is returned so
    /// callers (typically a background refresh task) can log it.
    pub fn refresh_gauge_metrics(&self) -> std::io::Result<()> {
        use crate::metrics::blob_metrics;
        let complete = self.list_complete()?;
        let mut total = 0u64;
        for hash in &complete {
            if let Ok((size, _)) = self.meta(hash) {
                total = total.saturating_add(size);
            }
        }
        let m = blob_metrics();
        m.store_size_bytes.set(total as i64);
        m.blobs_total.set(complete.len() as i64);
        Ok(())
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

    // ─────────────────────── constructor & accessors ───────────────────────

    /// `BlobStore::new` creates the data directory if it does not exist.
    #[test]
    fn new_creates_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        assert!(dir.path().exists());
        assert_eq!(store.data_dir(), dir.path());
    }

    /// `data_dir` returns the path passed to `new`.
    #[test]
    fn data_dir_returns_original_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        assert_eq!(store.data_dir(), dir.path());
    }

    // ─────────────────────── hash_file ────────────────────────

    /// `hash_file` correctly computes the BLAKE3 hash and byte size
    /// of a regular file.
    #[test]
    fn hash_file_computes_correct_hash_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"hello world from hash_file".to_vec();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, &payload).unwrap();
        let (hash, size) = store.hash_file(&path).unwrap();
        assert_eq!(size, payload.len() as u64);
        assert_eq!(hash, ContentHash::from_bytes(&payload));
    }

    /// `hash_file` returns an error for a non-existent path.
    #[test]
    fn hash_file_error_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let result = store.hash_file(&dir.path().join("nonexistent.bin"));
        assert!(result.is_err());
    }

    /// `hash_file` computes the same hash as `ContentHash::from_bytes`
    /// for any file content.
    #[test]
    fn hash_file_hash_matches_direct() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload: Vec<u8> = (0..50_000).map(|i| (i % 251) as u8).collect();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, &payload).unwrap();
        let (hash, size) = store.hash_file(&path).unwrap();
        assert_eq!(hash, ContentHash::from_bytes(&payload));
        assert_eq!(size, 50_000);
    }

    // ─────────────────────── contains / has_complete ───────────────────────

    /// `contains` is an alias for `has_complete` — both return `true`
    /// only when the `complete` sentinel is present.
    #[test]
    fn contains_matches_has_complete() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap();
        assert!(store.contains(&h));
        assert_eq!(store.contains(&h), store.has_complete(&h));
        store.remove(&h).unwrap();
        assert!(!store.contains(&h));
        assert_eq!(store.contains(&h), store.has_complete(&h));
    }

    /// `contains` returns `false` for a hash that was never imported.
    #[test]
    fn contains_false_for_unknown_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let unknown = ContentHash::from_bytes(b"unknown-blob");
        assert!(!store.contains(&unknown));
        assert!(!store.has_complete(&unknown));
    }

    // ─────────────────────── meta ────────────────────────

    /// `meta` returns `(size, chunk_count)` for a complete blob.
    #[test]
    fn meta_returns_size_and_chunk_count() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        // Single-chunk blob via put_bytes_sync.
        let (h, _) = store.put_bytes_sync(b"hello").unwrap();
        let (size, count) = store.meta(&h).unwrap();
        assert_eq!(size, 5);
        assert_eq!(count, 1);
    }

    /// `meta` returns an error for a blob that was never imported.
    #[test]
    fn meta_error_for_unknown_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let unknown = ContentHash::from_bytes(b"unknown");
        let err = store.meta(&unknown).unwrap_err();
        assert!(matches!(err, ChunkError::Io(_)));
        let io_err = match err {
            ChunkError::Io(e) => e,
            other => panic!("expected Io error, got {other:?}"),
        };
        assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
    }

    /// `meta` returns an error for a partial blob (no `complete` sentinel).
    #[test]
    fn meta_error_for_partial_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        // Plant a partial blob (chunks + meta.json but no sentinel).
        let partial = ContentHash::from_bytes(b"partial");
        let blob_dir = dir.path().join(partial.as_hex());
        std::fs::create_dir_all(blob_dir.join("chunks")).unwrap();
        std::fs::write(blob_dir.join("chunks").join("000000"), b"x").unwrap();
        let meta = serde_json::json!({
            "hash": partial.as_hex(),
            "sizeBytes": 1,
            "chunkCount": 1,
        });
        std::fs::write(
            blob_dir.join("meta.json"),
            serde_json::to_vec(&meta).unwrap(),
        )
        .unwrap();
        // No "complete" sentinel — meta must fail.
        let err = store.meta(&partial).unwrap_err();
        assert!(matches!(err, ChunkError::Io(_)));
    }

    /// `meta` surfaces a parse error when `meta.json` is corrupted.
    #[test]
    fn meta_error_on_corrupted_meta_json() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap();
        let meta_path = dir.path().join(h.as_hex()).join("meta.json");
        std::fs::write(meta_path, b"not-json-at-all").unwrap();
        let err = store.meta(&h).unwrap_err();
        assert!(matches!(err, ChunkError::Io(_)));
    }

    // ─────────────────────── read_chunk_sync ────────────────────────

    /// `read_chunk_sync` returns the correct chunk bytes for a
    /// complete multi-chunk blob.
    #[test]
    fn read_chunk_sync_returns_correct_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload: Vec<u8> = (0..(32 * 1024)).map(|i| (i % 251) as u8).collect();
        let src = dir.path().join("data.bin");
        std::fs::write(&src, &payload).unwrap();
        let (h, _) = store.import_file_sync(&src).unwrap();
        // Chunk 0 is the first 16 KiB.
        let chunk0 = store.read_chunk_sync(&h, 0).unwrap();
        assert_eq!(chunk0.len(), 16 * 1024);
        assert_eq!(chunk0[..100], payload[..100]);
        // Chunk 1 is the next 16 KiB.
        let chunk1 = store.read_chunk_sync(&h, 1).unwrap();
        assert_eq!(chunk1.len(), 16 * 1024);
        assert_eq!(chunk1[..100], payload[16 * 1024..16 * 1024 + 100]);
    }

    /// `read_chunk_sync` returns `NotFound` for an unknown blob.
    #[test]
    fn read_chunk_sync_error_on_unknown_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let unknown = ContentHash::from_bytes(b"unknown");
        let err = store.read_chunk_sync(&unknown, 0).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    /// `read_chunk_sync` returns `NotFound` for a chunk index that
    /// is out of range.
    #[test]
    fn read_chunk_sync_error_for_out_of_range_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap(); // 1 chunk
        let err = store.read_chunk_sync(&h, 99).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // ─────────────────────── read_range_sync ────────────────────────

    /// `read_range_sync` returns the exact bytes for a range within
    /// a single chunk.
    #[test]
    fn read_range_sync_single_chunk_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        // Multi-chunk blob: 32 KiB.
        let payload: Vec<u8> = (0..(32 * 1024)).map(|i| (i % 251) as u8).collect();
        let src = dir.path().join("data.bin");
        std::fs::write(&src, &payload).unwrap();
        let (h, _) = store.import_file_sync(&src).unwrap();
        // Read bytes 500..700 from the first chunk.
        let r = ByteRange::new(500, 700).unwrap();
        let bytes = store.read_range_sync(&h, &r).unwrap();
        assert_eq!(bytes.len(), 200);
        assert_eq!(bytes[..], payload[500..700]);
    }

    /// `read_range_sync` returns an empty vec for a range that
    /// starts at the blob's end (zero bytes). Note: we can't
    /// construct `ByteRange::new(5, 5)` — the constructor rejects
    /// zero-length ranges — so we test this by reading a range
    /// whose effective length is 0 because the blob is too small.
    #[test]
    fn read_range_sync_empty_range_when_blob_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap(); // size=5
        // Range (5, 10) — after clamping to blob size becomes (5, 5),
        // which conceptually has zero length. read_range_sync handles
        // this by clamping and returning an empty vec.
        let r = ByteRange::new(5, 10).unwrap();
        let bytes = store.read_range_sync(&h, &r).unwrap();
        assert!(bytes.is_empty());
    }

    /// `read_range_sync` returns an empty vec when `range.start >= size`.
    #[test]
    fn read_range_sync_start_beyond_end_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap(); // size=5
        let r = ByteRange::new(10, 20).unwrap();
        let bytes = store.read_range_sync(&h, &r).unwrap();
        assert!(bytes.is_empty());
    }

    /// `read_range_sync` clamps a range that extends past the blob end.
    #[test]
    fn read_range_sync_clamps_range_past_blob_end() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"0123456789".to_vec(); // 10 bytes
        let (h, _) = store.put_bytes_sync(&payload).unwrap();
        // Range (5, 100) extends past the blob size of 10.
        let r = ByteRange::new(5, 100).unwrap();
        let bytes = store.read_range_sync(&h, &r).unwrap();
        // Clamped to (5, 10) → 5 bytes.
        assert_eq!(bytes.len(), 5);
        assert_eq!(bytes, b"56789");
    }

    /// `read_range_sync` error for an unknown blob.
    #[test]
    fn read_range_sync_error_on_unknown_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let unknown = ContentHash::from_bytes(b"unknown");
        let r = ByteRange::new(0, 10).unwrap();
        let err = store.read_range_sync(&unknown, &r).unwrap_err();
        assert!(matches!(err, ChunkError::Io(_)));
    }

    // ─────────────────────── export_to_file_sync ────────────────────────

    /// `export_to_file_sync` creates parent directories if they do not exist.
    #[test]
    fn export_to_file_sync_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap();
        let dest = dir.path().join("subdir").join("nested").join("out.bin");
        let n = store.export_to_file_sync(&h, &dest).unwrap();
        assert_eq!(n, 5);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    }

    /// `export_to_file_sync` error for a non-existent blob.
    #[test]
    fn export_to_file_sync_error_on_unknown_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let unknown = ContentHash::from_bytes(b"unknown");
        let dest = dir.path().join("out.bin");
        // `.unwrap_err()` confirms an error is returned.
        store.export_to_file_sync(&unknown, &dest).unwrap_err();
    }

    // ─────────────────────── list_complete ────────────────────────

    /// `list_complete` on an empty store returns an empty vec.
    #[test]
    fn list_complete_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        assert!(store.list_complete().unwrap().is_empty());
    }

    /// `list_complete` on a store with many blobs returns all of them.
    #[test]
    fn list_complete_returns_all_complete_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let mut hashes = Vec::new();
        for i in 0..10u8 {
            let payload = vec![i; 100];
            let (h, _) = store.put_bytes_sync(&payload).unwrap();
            hashes.push(h);
        }
        let listed = store.list_complete().unwrap();
        assert_eq!(listed.len(), 10);
        for h in &hashes {
            assert!(listed.contains(h), "missing hash {h}");
        }
    }

    // ─────────────────────── remove ────────────────────────

    /// `remove` returns `false` for a hash that was never in the store.
    #[test]
    fn remove_returns_false_for_unknown_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let unknown = ContentHash::from_bytes(b"unknown");
        assert!(!store.remove(&unknown).unwrap());
    }

    /// `remove` returns `false` for a blob that was already removed.
    #[test]
    fn remove_returns_false_after_first_removal() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"hello").unwrap();
        assert!(store.remove(&h).unwrap());
        assert!(!store.remove(&h).unwrap());
    }

    // ─────────────────────── finalize_import ────────────────────────

    /// `finalize_import` is idempotent — calling it twice on the same
    /// hash does not produce an error or duplicate structure.
    #[test]
    fn finalize_import_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"idempotent test".to_vec();
        let src = dir.path().join("data.bin");
        std::fs::write(&src, &payload).unwrap();
        let (hash, size) = store.import_file_sync(&src).unwrap();
        // The first import already called finalize_import. Calling it
        // again must be a no-op.
        store.finalize_import(&hash, size).unwrap();
        assert!(store.has_complete(&hash));
        let (size2, count) = store.meta(&hash).unwrap();
        assert_eq!(size2, size);
        assert_eq!(count, 1);
    }

    /// `finalize_import` on a never-imported blob creates the
    /// directory structure and `complete` sentinel.
    #[test]
    fn finalize_import_creates_structure() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"finalize test".to_vec();
        let src = dir.path().join("data.bin");
        std::fs::write(&src, &payload).unwrap();
        let (hash, size) = store.hash_file(&src).unwrap();
        // Pre-condition: blob dir does not exist.
        assert!(!store.blob_dir(&hash).exists());
        // Manually stage a blob and then call finalize_import.
        let blob_dir = store.blob_dir(&hash);
        std::fs::create_dir_all(blob_dir.join("chunks")).unwrap();
        std::fs::write(blob_dir.join("chunks").join("000000"), &payload).unwrap();
        store.finalize_import(&hash, size).unwrap();
        assert!(store.has_complete(&hash));
        assert!(store.meta(&hash).is_ok());
    }

    // ─────────────────────── trait impl dispatch ────────────────────────
    //
    // Verify that `BlobReader` and `BlobImporter` are implemented
    // for `BlobStore` and the trait methods produce the correct results.
    // (The traits use `impl Trait` RPITIT return types, so they are
    // NOT dyn-safe; these tests use concrete dispatch.)

    /// `BlobReader::size` returns the correct size through trait dispatch.
    #[tokio::test]
    async fn blob_reader_trait_impl_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (h, _) = store.put_bytes_sync(b"dispatch-test").unwrap();
        // Call through the trait directly (concrete dispatch).
        assert!(BlobReader::has(&store, &h).await);
        assert_eq!(BlobReader::size(&store, &h).await.unwrap(), 13);
        assert_eq!(BlobReader::chunk_count(&store, &h).await.unwrap(), 1);
        assert_eq!(
            BlobReader::read_all(&store, &h).await.unwrap(),
            b"dispatch-test"
        );
    }

    /// `BlobImporter::put_bytes` through trait dispatch produces a readable blob.
    #[tokio::test]
    async fn blob_importer_trait_impl_put_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let hash = BlobImporter::put_bytes(&store, b"trait-impl")
            .await
            .unwrap();
        assert_eq!(hash, ContentHash::from_bytes(b"trait-impl"));
        // And it can be read back via the reader trait.
        assert_eq!(
            BlobReader::read_all(&store, &hash).await.unwrap(),
            b"trait-impl"
        );
    }

    // ─────────────────────── BlobImporter impl ────────────────────────

    /// `BlobImporter::put_bytes` on the `BlobStore` produces a blob
    /// that is readable via `BlobReader`.
    #[tokio::test]
    async fn blob_importer_put_bytes_produces_readable_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let payload = b"async importer payload".to_vec();
        let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
        assert!(BlobReader::has(&store, &hash).await);
        assert_eq!(BlobReader::read_all(&store, &hash).await.unwrap(), payload);
        assert_eq!(
            BlobReader::size(&store, &hash).await.unwrap(),
            payload.len() as u64
        );
    }

    // ─────────────────────── cross-chunk range read ────────────────────────

    /// `read_range_sync` across the boundary between two chunks returns
    /// the correct bytes.
    #[test]
    fn read_range_sync_crosses_chunk_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        // Build a blob where we know the chunk boundary.
        let payload: Vec<u8> = (0..(16 * 1024 + 500)).map(|i| (i % 251) as u8).collect();
        let src = dir.path().join("data.bin");
        std::fs::write(&src, &payload).unwrap();
        let (h, _) = store.import_file_sync(&src).unwrap();
        // Range that spans chunk 0 (end) and chunk 1 (start).
        let r = ByteRange::new(16 * 1024 - 100, 16 * 1024 + 100).unwrap();
        let bytes = store.read_range_sync(&h, &r).unwrap();
        assert_eq!(bytes.len(), 200);
        assert_eq!(bytes[..], payload[r.start as usize..r.end as usize]);
    }

    // ─────────────────────── refresh_gauge_metrics ────────────────────────

    /// `refresh_gauge_metrics` reflects the current store state in
    /// `adnet_blob_store_size_bytes` and `adnet_blob_blobs_total`.
    #[test]
    fn refresh_gauge_metrics_reports_current_state() {
        use crate::metrics::blob_metrics;
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();

        let m = blob_metrics();
        let size_before = m.store_size_bytes.get();
        let count_before = m.blobs_total.get();

        store.put_bytes_sync(b"alpha").unwrap();
        store.put_bytes_sync(b"bravo-bravo").unwrap();
        store.put_bytes_sync(b"charlie-charlie-charlie").unwrap();
        store.refresh_gauge_metrics().unwrap();

        let expected_size = 5 + 11 + 23;
        assert_eq!(
            m.store_size_bytes.get() - size_before,
            expected_size as i64,
            "size gauge should increase by exact payload bytes"
        );
        assert_eq!(
            m.blobs_total.get() - count_before,
            3,
            "blobs_total gauge should increase by 3"
        );

        // Remove one and re-sweep: gauges should drop accordingly.
        let list = store.list_complete().unwrap();
        store.remove(&list[0]).unwrap();
        store.refresh_gauge_metrics().unwrap();
        assert_eq!(
            m.blobs_total.get() - count_before,
            2,
            "blobs_total should drop to 2 after a remove"
        );
    }

    /// `refresh_gauge_metrics` on an empty store leaves both gauges
    /// at zero (relative to whatever they were before the sweep —
    /// the absolute values may be > 0 if other tests already touched
    /// the global registry, but the *delta* from an empty sweep
    /// against an empty store must be 0).
    #[test]
    fn refresh_gauge_metrics_empty_store_is_noop_delta() {
        use crate::metrics::blob_metrics;
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();

        let m = blob_metrics();
        let size_before = m.store_size_bytes.get();
        let count_before = m.blobs_total.get();

        store.refresh_gauge_metrics().unwrap();
        assert_eq!(m.store_size_bytes.get(), size_before);
        assert_eq!(m.blobs_total.get(), count_before);
    }

    // ─────────────────────────────────────────────────────────────────
    //  GC integration tests — pin_set / gc_orphans / gc_unpinned / gc_all
    // ─────────────────────────────────────────────────────────────────

    /// Two blobs in the store, one pinned → GC drops the other.
    #[test]
    fn gc_orphans_removes_only_unpinned_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (kept, _) = store.put_bytes_sync(b"keep me").unwrap();
        let (gone, _) = store.put_bytes_sync(b"drop me").unwrap();
        assert_eq!(store.list_complete().unwrap().len(), 2);

        let mut pins = crate::pin_set::PinSet::new();
        assert!(pins.add(&kept, false, std::collections::BTreeSet::new(), 1));

        let removed = store.gc_orphans(&pins).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], gone);
        assert!(store.has_complete(&kept));
        assert!(!store.has_complete(&gone));
    }

    /// No pins → everything is an orphan and gets dropped.
    #[test]
    fn gc_orphans_with_no_pins_drops_everything() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        store.put_bytes_sync(b"a").unwrap();
        store.put_bytes_sync(b"b").unwrap();
        store.put_bytes_sync(b"c").unwrap();
        assert_eq!(store.list_complete().unwrap().len(), 3);

        let pins = crate::pin_set::PinSet::new();
        let removed = store.gc_orphans(&pins).unwrap();
        assert_eq!(removed.len(), 3);
        assert!(store.list_complete().unwrap().is_empty());
    }

    /// All blobs pinned → GC is a no-op.
    #[test]
    fn gc_orphans_keeps_all_when_every_blob_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (a, _) = store.put_bytes_sync(b"a").unwrap();
        let (b, _) = store.put_bytes_sync(b"b").unwrap();
        let mut pins = crate::pin_set::PinSet::new();
        pins.add(&a, false, std::collections::BTreeSet::new(), 1);
        pins.add(&b, false, std::collections::BTreeSet::new(), 1);
        assert!(store.gc_orphans(&pins).unwrap().is_empty());
        assert_eq!(store.list_complete().unwrap().len(), 2);
    }

    /// gc_unpinned takes the pinned hex set as an iterable — used by
    /// the CLI's `repo gc --prune-unpinned` path which already has
    /// a hex list at hand.
    #[test]
    fn gc_unpinned_uses_supplied_hex_set() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (kept, _) = store.put_bytes_sync(b"kept").unwrap();
        store.put_bytes_sync(b"dropped").unwrap();
        let removed = store.gc_unpinned([kept.as_hex().to_string()]).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(store.has_complete(&kept));
    }

    /// gc_all is the operator's "nuke the repo" button. It returns
    /// every previously-present hash.
    #[test]
    fn gc_all_returns_every_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        store.put_bytes_sync(b"a").unwrap();
        store.put_bytes_sync(b"b").unwrap();
        let removed = store.gc_all().unwrap();
        assert_eq!(removed.len(), 2);
        assert!(store.list_complete().unwrap().is_empty());
    }

    /// GC pass is idempotent — running it twice doesn't double-delete
    /// or panic.
    #[test]
    fn gc_orphans_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        store.put_bytes_sync(b"a").unwrap();
        let pins = crate::pin_set::PinSet::new();
        assert_eq!(store.gc_orphans(&pins).unwrap().len(), 1);
        // Second pass: nothing to remove.
        assert!(store.gc_orphans(&pins).unwrap().is_empty());
    }

    /// Recursive pin + chunk-only pin survives GC together — this is
    /// the scenario from `PinSet::sweep_orphan_chunks` mirrored on
    /// the on-disk side.
    #[test]
    fn gc_orphans_preserves_implicit_chunk_pins() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let (chunk, _) = store.put_bytes_sync(b"chunk-bytes").unwrap();
        let (root, _) = store.put_bytes_sync(b"root-bytes").unwrap();

        let mut pins = crate::pin_set::PinSet::new();
        pins.add_chunk(&chunk, 1);
        let mut desc = std::collections::BTreeSet::new();
        desc.insert(chunk.as_hex().to_string());
        pins.add(&root, true, desc, 1);

        // Neither should be considered an orphan.
        let removed = store.gc_orphans(&pins).unwrap();
        assert!(removed.is_empty(), "both pins must keep their blobs alive");
        assert!(store.has_complete(&chunk));
        assert!(store.has_complete(&root));
    }
}
