//! Receive-side primitives — fetch a shared blob/directory by
//! [`crate::ticket::ShareTicket`].
//!
//! ## Status
//!
//! This module is a **stubs layer** in PR1. The full
//! `iroh::Endpoint`-backed pull is implemented in PR3 (see
//! `PLAN_OPS_PERFORMANCE.md`). PR1 ships:
//!
//! - [`ReceiveOptions`] — knobs (concurrency, output directory, allow
//!   overwrite).
//! - [`ReceiveStats`] — what `receive` returns alongside the manifest.
//! - [`receive`] — a *local* path that decodes a ticket, walks the
//!   manifest, and reads every `(name, hash)` pair out of an
//!   `BlobReader`-implementing store. This is the path the
//!   `a3net-blobstore`-HTTP fallback uses; it also serves as the unit
//!   test target for [`crate::collection::Collection`] round-trips.
//!
//! PR3 fills in the `iroh` feature-gated `remote_fetch` that connects
//! to a sender's `Endpoint`, runs `iroh_blobs::get::run`, and stores
//! each chunk back into `IrohBlobStore`. The local path below is
//! unchanged by PR3.

use std::path::PathBuf;
use std::time::Instant;

use tracing::info;

use a3net_blobstore::traits::BlobReader;
use a3net_types::ContentHash;

use crate::collection::Collection;
use crate::error::{ShareError, ShareResult};
use crate::metrics::{record_receive_complete, record_receive_expected};
use crate::ticket::ShareTicket;

/// Knobs for [`receive`].
#[derive(Debug, Clone)]
pub struct ReceiveOptions {
    /// Directory under which to lay out the received files. When
    /// `None`, defaults to the current working directory (matches
    /// sendme).
    pub out_dir: Option<PathBuf>,
    /// When `true`, overwrite existing files. Default `false` mirrors
    /// sendme (which errors on collision with `target already exists`).
    pub overwrite: bool,
}

impl Default for ReceiveOptions {
    fn default() -> Self {
        Self {
            out_dir: None,
            overwrite: false,
        }
    }
}

/// Returned alongside the [`Collection`] so callers can render
/// summary UIs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceiveStats {
    pub files_written: usize,
    pub bytes_written: u64,
    pub elapsed_ms: u64,
}

/// Receive a previously-imported collection from a local `BlobReader`.
///
/// This is the "already on this machine" path: the sender and receiver
/// are the same node (or the receiver already pulled the bytes via
/// another mechanism such as the HTTP mesh fallback) and we just need
/// to lay out files on disk.
///
/// The remote pull is the iroh-backed `remote_fetch` (PR3).
///
/// `manifest` is the manifest the caller obtained from the ticket's
/// `manifest_hash`. `reader` reads any of the per-file hashes from
/// local storage.
pub async fn receive<R>(
    ticket: &ShareTicket,
    manifest: &Collection,
    reader: &R,
    opts: ReceiveOptions,
) -> ShareResult<ReceiveStats>
where
    R: BlobReader + Send + Sync,
{
    let started = Instant::now();
    let out_dir = opts
        .out_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Pre-compute the expected size so the
    // `share_receive_bytes_total` / `_files_total` counters
    // are bumped once per receive — even if the loop fails
    // partway through. We don't have per-file sizes in the
    // manifest itself (it's just `(name, hash)` pairs), so we
    // ask the underlying reader for each entry's size. A
    // missing size is treated as `0` — better than skipping
    // the bump entirely.
    let mut expected_bytes: u64 = 0;
    for (_name, hash) in manifest.iter() {
        if let Ok(size) = reader.size(hash).await {
            expected_bytes = expected_bytes.saturating_add(size);
        }
    }
    record_receive_expected(manifest.len(), expected_bytes);

    let mut stats = ReceiveStats::default();
    let result: ShareResult<()> = async {
        for (name, hash) in manifest.iter() {
            let bytes = BlobReader::read_all(reader, hash)
                .await
                .map_err(|e| ShareError::Backend(format!("read {name}: {e}")))?;

            let dest = join_under(&out_dir, name)?;
            if dest.exists() && !opts.overwrite {
                return Err(ShareError::Backend(format!(
                    "target {} exists (use overwrite = true)",
                    dest.display()
                )));
            }
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&dest, &bytes).await?;

            stats.files_written += 1;
            stats.bytes_written += bytes.len() as u64;
        }
        Ok(())
    }
    .await;

    stats.elapsed_ms = started.elapsed().as_millis() as u64;

    match &result {
        Err(e) => {
            record_receive_complete(
                started.elapsed(),
                stats.files_written,
                stats.bytes_written,
                true,
            );
            info!(
                files = stats.files_written,
                bytes = stats.bytes_written,
                elapsed_ms = stats.elapsed_ms,
                sender = %ticket.node_id.short(),
                error = %e,
                "receive failed (local)"
            );
        }
        Ok(()) => {
            record_receive_complete(
                started.elapsed(),
                stats.files_written,
                stats.bytes_written,
                false,
            );
            info!(
                files = stats.files_written,
                bytes = stats.bytes_written,
                elapsed_ms = stats.elapsed_ms,
                sender = %ticket.node_id.short(),
                "receive complete (local)"
            );
        }
    }

    result?;
    Ok(stats)
}

/// Join a relative collection entry name (slash-separated) under an
/// output directory, refusing components that escape the root.
fn join_under(root: &std::path::Path, name: &str) -> ShareResult<PathBuf> {
    use std::path::Component;
    let mut out = root.to_path_buf();
    for c in std::path::Path::new(name).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => {
                return Err(ShareError::InvalidPathComponent(format!(
                    "refused component in {name:?}"
                )));
            }
        }
    }
    Ok(out)
}

/// Trait gate: ensures `ContentHash` is `Send + Sync` (it always is,
/// but the explicit reference keeps `pub use` happy when the
/// `iroh` feature is off).
#[allow(dead_code)]
fn _content_hash_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ContentHash>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{Collection, CollectionEntry};
    use a3net_blobstore::chunked::ChunkError;
    use a3net_blobstore::traits::BlobReader;
    use a3net_types::RangeSpec;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Minimal in-memory `BlobReader` for tests. Mirrors `MemStore` but
    /// implements the trait (so we can pass it to `receive` without a
    /// filesystem backend).
    #[derive(Default)]
    struct MemBlobReader {
        map: Mutex<HashMap<ContentHash, Vec<u8>>>,
    }

    impl MemBlobReader {
        fn put(&self, bytes: &[u8]) -> ContentHash {
            let h = ContentHash::from_bytes(bytes);
            self.map.lock().unwrap().insert(h.clone(), bytes.to_vec());
            h
        }
    }

    impl BlobReader for MemBlobReader {
        async fn has(&self, hash: &ContentHash) -> bool {
            self.map.lock().unwrap().contains_key(hash)
        }

        async fn size(&self, hash: &ContentHash) -> Result<u64, ChunkError> {
            self.map
                .lock()
                .unwrap()
                .get(hash)
                .map(|v| v.len() as u64)
                .ok_or_else(|| {
                    ChunkError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("blob {} not found", hash),
                    ))
                })
        }

        async fn chunk_count(&self, hash: &ContentHash) -> Result<u32, ChunkError> {
            Ok((self.size(hash).await? as u32).div_ceil(16 * 1024))
        }

        async fn read_all(&self, hash: &ContentHash) -> Result<Vec<u8>, ChunkError> {
            self.map.lock().unwrap().get(hash).cloned().ok_or_else(|| {
                ChunkError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("blob {} not found", hash),
                ))
            })
        }

        async fn read_range(
            &self,
            hash: &ContentHash,
            range: RangeSpec,
        ) -> Result<Vec<u8>, ChunkError> {
            let buf = self.read_all(hash).await?;
            match range {
                RangeSpec::All => Ok(buf),
                RangeSpec::Single(r) => {
                    let _len = buf.len() as u64;
                    let s = (r.start as usize).min(buf.len());
                    let e = (r.end as usize).min(buf.len()).max(s);
                    Ok(buf[s..e].to_vec())
                }
                RangeSpec::Multi(rs) => {
                    let mut out = Vec::new();
                    for r in rs {
                        let s = (r.start as usize).min(buf.len());
                        let e = (r.end as usize).min(buf.len()).max(s);
                        out.extend_from_slice(&buf[s..e]);
                    }
                    Ok(out)
                }
            }
        }

        async fn read_chunk(
            &self,
            hash: &ContentHash,
            index: u32,
        ) -> Result<Option<Vec<u8>>, ChunkError> {
            let buf = self.read_all(hash).await?;
            let start = (index as usize) * 16 * 1024;
            if start >= buf.len() {
                return Ok(None);
            }
            let end = (start + 16 * 1024).min(buf.len());
            Ok(Some(buf[start..end].to_vec()))
        }

        async fn export_to_file(
            &self,
            _hash: &ContentHash,
            _dest: &std::path::Path,
        ) -> Result<u64, ChunkError> {
            Err(ChunkError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "MemBlobReader does not implement export_to_file",
            )))
        }
    }

    fn manifest() -> (Collection, Vec<(String, Vec<u8>)>) {
        let mut c = Collection::new();
        let entries = vec![
            ("a.txt".to_string(), b"alpha".to_vec()),
            ("sub/b.txt".to_string(), b"bravo".to_vec()),
        ];
        for (name, bytes) in &entries {
            let hash = ContentHash::from_bytes(bytes);
            c.push(CollectionEntry::new(name, hash).unwrap()).unwrap();
        }
        (c, entries)
    }

    fn ticket() -> ShareTicket {
        let id = a3net_types::NodeId::random();
        let addr = a3net_types::NodeAddr::new(id.clone());
        let mh = ContentHash::from_bytes(b"placeholder");
        ShareTicket::new(&id, &addr, &mh, &Collection::new(), 0).unwrap()
    }

    #[tokio::test]
    async fn writes_files_under_out_dir() {
        let (manifest, entries) = manifest();
        let reader = MemBlobReader::default();
        for (_, bytes) in &entries {
            reader.put(bytes);
        }

        let dir = TempDir::new().unwrap();
        let stats = receive(
            &ticket(),
            &manifest,
            &reader,
            ReceiveOptions {
                out_dir: Some(dir.path().to_path_buf()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(stats.files_written, 2);
        assert_eq!(stats.bytes_written, (5 + 5));

        let a = std::fs::read(dir.path().join("a.txt")).unwrap();
        assert_eq!(a, b"alpha");
        let b = std::fs::read(dir.path().join("sub").join("b.txt")).unwrap();
        assert_eq!(b, b"bravo");
    }

    #[tokio::test]
    async fn refuses_existing_target_without_overwrite() {
        let (manifest, entries) = manifest();
        let reader = MemBlobReader::default();
        for (_, bytes) in &entries {
            reader.put(bytes);
        }
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"existing").unwrap();
        let err = receive(
            &ticket(),
            &manifest,
            &reader,
            ReceiveOptions {
                out_dir: Some(dir.path().to_path_buf()),
                overwrite: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ShareError::Backend(_)));
    }

    #[tokio::test]
    async fn overwrites_when_requested() {
        let (manifest, entries) = manifest();
        let reader = MemBlobReader::default();
        for (_, bytes) in &entries {
            reader.put(bytes);
        }
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"existing").unwrap();
        let stats = receive(
            &ticket(),
            &manifest,
            &reader,
            ReceiveOptions {
                out_dir: Some(dir.path().to_path_buf()),
                overwrite: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(stats.files_written, 2);
        let a = std::fs::read(dir.path().join("a.txt")).unwrap();
        assert_eq!(a, b"alpha");
    }

    #[tokio::test]
    async fn join_under_rejects_parent_traversal() {
        let err = join_under(std::path::Path::new("/tmp"), "../etc/passwd").unwrap_err();
        assert!(matches!(err, ShareError::InvalidPathComponent(_)));
    }
}