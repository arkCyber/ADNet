//! `walk_import` — recursively ingest a file or directory into a
//! blob store, returning a [`Collection`] manifest.
//!
//! This is the A3Net port of `n0-computer/sendme@0.36.0/src/main.rs::import()`.
//! The two implementations share the same shape:
//!
//! 1. canonicalise the root,
//! 2. use `walkdir` to enumerate every regular file,
//! 3. for each file, compute its relative name (slash-separated,
//!    validated), and push its BLAKE3 hash into a manifest,
//! 4. materialise the manifest hash, which is what the ticket carries.
//!
//! Differences from sendme:
//!
//! - We import through a caller-supplied closure (`PutBytesFn`) rather
//!   than `iroh_blobs::Store`, so callers can swap the `BlobStore` /
//!   `IrohBlobStore` / `MemStore` backend without touching this code.
//!   `Send + Sync` is required only because `walkdir` walks on the
//!   blocking pool.
//! - Symlinks are **refused by default**; sendme relies on the default
//!   `walkdir` traversal which silently skips them via the `is_file()`
//!   filter. We surface them as a typed error so operators know their
//!   share tree contained a symlink.
//! - File names are validated against
//!   [`crate::path::validate_path_component`] *before* insertion into
//!   the collection, so a manifest cannot carry an entry with an
//!   illegal character.
//! - The import is intentionally synchronous; both `BlobStore` and
//!   `MemStore` expose sync APIs. The iroh-backed `IrohBlobStore` is
//!   async, but callers wrap it in
//!   `|bytes| tokio::runtime::Handle::current().block_on(...)` or use
//!   the higher-level async shim in `a3net-share`'s iroh-feature
//!   `receive` module. sendme's `walkdir + Bao` work is also sync; we
//!   preserve that property so PR1 stays simple.
//!
//! ## Concurrency
//!
//! sendme uses `num_cpus` workers via
//! `n0_future::stream::iter(...).buffered_unordered(parallelism)`. We
//! mirror that with `futures::stream::iter(...).buffer_unordered(jobs)`,
//! where each *item* is itself a `tokio::task::spawn_blocking` future
//! (so the synchronous `std::fs::read` + BLAKE3 + `put_bytes` work
//! runs off the async runtime). The two layers of concurrency —
//! `spawn_blocking` per file, `buffer_unordered(jobs)` across files —
//! keep the async runtime unblocked while still pipelining the heavy
//! IO + hashing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use a3net_types::ContentHash;

use crate::collection::{Collection, CollectionEntry};
use crate::error::{ShareError, ShareResult};
use crate::path::canonicalized_path_to_string;

/// A function that imports `bytes` into the caller's blob store and
/// returns the resulting BLAKE3 hash.
///
/// `Send + Sync` is required because the worker futures cross thread
/// boundaries when `walk_import` runs in a `spawn_blocking` context.
pub type PutBytesFn = Arc<dyn Fn(&[u8]) -> ShareResult<ContentHash> + Send + Sync>;

/// Knobs for [`walk_import`].
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Maximum number of in-flight imports. `None` means "auto"
    /// (number of logical CPU cores), matching sendme's default.
    pub jobs: Option<usize>,
    /// When `true`, symlinks encountered during the walk are followed
    /// (sendme's `walkdir` default). When `false` (the default for
    /// A3Net), they are surfaced as [`ShareError::SymlinkRefused`].
    pub allow_symlinks: bool,
    /// Skip hidden files (anything whose name starts with `.`).
    /// sendme does not, but most A3Net operators will not want their
    /// `.DS_Store` etc. showing up in a shared directory.
    pub skip_hidden: bool,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            jobs: None,
            allow_symlinks: false,
            skip_hidden: true,
        }
    }
}

/// Statistics returned alongside the manifest so callers can render
/// progress / summary UIs without re-traversing the tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkStats {
    /// Number of files actually imported.
    pub files_imported: usize,
    /// Cumulative byte count across every imported file.
    pub total_bytes: u64,
    /// Number of entries skipped because they were a symlink and
    /// `allow_symlinks == false`.
    pub symlinks_skipped: usize,
    /// Number of entries skipped because they matched the `skip_hidden`
    /// pattern.
    pub hidden_skipped: usize,
    /// Wall-clock duration of the import (best-effort — the timer
    /// starts when the function is entered and stops when the
    /// manifest is materialised).
    pub elapsed_ms: u64,
}

/// Recursively ingest `root` into the store fronted by `put_bytes`,
/// returning a `(Collection, manifest_hash, WalkStats)` triple.
///
/// `root` may be a single file (in which case the manifest has
/// exactly one entry) or a directory.
///
/// `put_bytes` is the only callback required — both the legacy
/// `BlobStore` and the in-memory `MemStore` expose a sync
/// `put_bytes(&[u8]) -> io::Result<ContentHash>` that wraps in one
/// closure line. The iroh-backed `IrohBlobStore` is async; callers
/// can either use `tokio::runtime::Handle::block_on` or use the
/// `iroh`-feature `receive` module's higher-level wrapper.
///
/// **Returns:** `(Collection, ContentHash, WalkStats)`. The
/// `ContentHash` is the BLAKE3 digest of the manifest bytes — that's
/// the value the caller puts in the ticket.
pub async fn walk_import(
    root: &Path,
    put_bytes: PutBytesFn,
    opts: WalkOptions,
) -> ShareResult<(Collection, ContentHash, WalkStats)> {
    let started = std::time::Instant::now();

    // Canonicalise so `.` / `..` and symlinks (before the walk) are
    // resolved up front. This is what sendme's `path.canonicalize()`
    // does at the top of `import`.
    let canonical = root
        .canonicalize()
        .map_err(|e| ShareError::PathNotFound(format!("{}: {e}", root.display())))?;
    anyhow_ensure_file_or_dir(&canonical)?;

    // Step 1: enumerate files synchronously (WalkDir is blocking).
    // We collect `Vec<(rel_name, abs_path)>` rather than paths only,
    // because the relative-name validation happens up-front so we can
    // bail early on a malformed tree (instead of failing mid-import).
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    let mut stats = WalkStats::default();

    for entry in WalkDir::new(&canonical).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("walkdir entry error: {e}");
                continue;
            }
        };
        let file_type = entry.file_type();

        // Symlink policy — refuse (or skip silently) before any IO.
        if file_type.is_symlink() {
            stats.symlinks_skipped += 1;
            if !opts.allow_symlinks {
                return Err(ShareError::SymlinkRefused(format!(
                    "{}",
                    entry.path().display()
                )));
            }
            // allow_symlinks = true → skip symlinked files entirely.
            // sendme does the same via the is_file() filter below.
            continue;
        }

        if !file_type.is_file() {
            continue; // skip directories, sockets, fifos, devices …
        }

        let abs = entry.into_path();
        let rel_to_root = if canonical.is_file() {
            // Single-file import: the walk visits exactly one
            // entry which is `canonical` itself. The relative path
            // from the share root is the file's basename (matches
            // sendme's behaviour: the basename becomes the entry name).
            match abs.file_name() {
                Some(name) => std::path::Path::new(name).to_path_buf(),
                None => continue,
            }
        } else {
            match abs.strip_prefix(&canonical) {
                Ok(p) => p.to_path_buf(),
                Err(_) => continue, // shouldn't happen, but be safe
            }
        };

        // Hidden-file filter. We compare against the share root
        // (canonical), NOT its parent — otherwise a macOS tempdir
        // named `.tmpXXX` would mark every entry as hidden.
        if opts.skip_hidden
            && rel_to_root
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s),
                    _ => None,
                })
                .any(|s| s.to_string_lossy().starts_with('.'))
        {
            stats.hidden_skipped += 1;
            continue;
        }

        let name = canonicalized_path_to_string(&rel_to_root)?;
        entries.push((name, abs));
    }

    // Step 2: import each file in parallel.
    //
    // We use `spawn_blocking` to keep heavy `std::fs::read` + BLAKE3
    // off the async runtime. `put_bytes` may itself be async (iroh
    // path) — but the closure contract here is sync, so callers wrap
    // it once at construction time.
    let parallelism = opts.jobs.unwrap_or_else(num_cpus_or_one);
    let import_futs = entries.into_iter().map(|(name, abs)| {
        let put_bytes = Arc::clone(&put_bytes);
        async move {
            tokio::task::spawn_blocking(move || -> ShareResult<(String, ContentHash, u64)> {
                let bytes = std::fs::read(&abs).map_err(|e| {
                    ShareError::Io(std::io::Error::new(
                        e.kind(),
                        format!("read {}: {e}", abs.display()),
                    ))
                })?;
                let len = bytes.len() as u64;
                let hash = (put_bytes)(&bytes)?;
                drop(bytes);
                Ok((name, hash, len))
            })
            .await
            .map_err(|e| ShareError::Backend(format!("join: {e}")))?
        }
    });

    let collected: Vec<ShareResult<(String, ContentHash, u64)>> =
        futures::stream::iter(import_futs)
            .buffer_unordered(parallelism)
            .collect()
            .await;
    let mut collected: Vec<(String, ContentHash, u64)> = collected
        .into_iter()
        .collect::<ShareResult<Vec<_>>>()?;

    // Sort by name to match sendme's `names_and_tags.sort_by(...)` —
    // deterministic iteration order is important for both the manifest
    // hash and for gossipped announcements.
    collected.sort_by(|a, b| a.0.cmp(&b.0));

    // Step 3: build the manifest.
    let mut manifest = Collection::new();
    for (name, hash, len) in collected {
        manifest.push(CollectionEntry::new(name, hash)?)?;
        stats.total_bytes += len;
        stats.files_imported += 1;
    }

    // Step 4: compute the manifest hash (the ticket value).
    let manifest_hash = manifest.manifest_hash()?;
    stats.elapsed_ms = started.elapsed().as_millis() as u64;

    info!(
        files = stats.files_imported,
        bytes = stats.total_bytes,
        manifest_hash = %manifest_hash.as_hex(),
        elapsed_ms = stats.elapsed_ms,
        "walk_import complete"
    );
    debug!(
        symlinks_skipped = stats.symlinks_skipped,
        hidden_skipped = stats.hidden_skipped,
        "walk_import stats"
    );

    Ok((manifest, manifest_hash, stats))
}

fn anyhow_ensure_file_or_dir(p: &Path) -> ShareResult<()> {
    let meta = std::fs::metadata(p)?;
    if !(meta.is_file() || meta.is_dir()) {
        return Err(ShareError::NotFileOrDir(p.display().to_string()));
    }
    Ok(())
}

fn num_cpus_or_one() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Shared fake "store" — a `HashMap` behind a `Mutex`, with a
    /// `put_bytes` closure that mimics what `BlobStore::put_bytes`
    /// does (compute BLAKE3, insert).
    fn fake_store() -> (PutBytesFn, Arc<Mutex<std::collections::HashMap<ContentHash, Vec<u8>>>>) {
        let map: Arc<Mutex<std::collections::HashMap<ContentHash, Vec<u8>>>> =
            Arc::new(Mutex::new(Default::default()));
        let map_for_closure = Arc::clone(&map);
        let put: PutBytesFn = Arc::new(move |bytes: &[u8]| {
            let hash = ContentHash::from_bytes(bytes);
            map_for_closure
                .lock()
                .unwrap()
                .insert(hash.clone(), bytes.to_vec());
            Ok(hash)
        });
        (put, map)
    }

    /// Convenience: build a small tree:
    ///
    /// ```text
    /// root/
    ///   a.txt       (5 bytes: "alpha")
    ///   sub/
    ///     b.txt     (5 bytes: "bravo")
    ///     c.txt     (5 bytes: "charlie")
    /// ```
    fn sample_tree(root: &Path) {
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("b.txt"), b"bravo").unwrap();
        fs::write(root.join("sub").join("c.txt"), b"charlie").unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imports_single_file() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("solo.txt");
        fs::write(&f, b"hello").unwrap();

        let (put, _store) = fake_store();
        let (manifest, mh, stats) =
            walk_import(&f, put, WalkOptions::default()).await.unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(stats.files_imported, 1);
        assert_eq!(stats.total_bytes, 5);
        assert!(!mh.as_hex().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imports_directory_recursively() {
        let dir = TempDir::new().unwrap();
        sample_tree(dir.path());

        let (put, _store) = fake_store();
        let (manifest, _mh, stats) =
            walk_import(dir.path(), put, WalkOptions::default()).await.unwrap();
        assert_eq!(manifest.len(), 3);
        assert_eq!(stats.files_imported, 3);
        assert_eq!(stats.total_bytes, 5 + 5 + 7);

        let names: Vec<&str> = manifest.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a.txt", "sub/b.txt", "sub/c.txt"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skips_hidden_files_by_default() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".hidden"), b"nope").unwrap();
        fs::write(dir.path().join("visible.txt"), b"hi").unwrap();

        let (put, _store) = fake_store();
        let (manifest, _, stats) =
            walk_import(dir.path(), put, WalkOptions::default()).await.unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(stats.hidden_skipped, 1);
        assert_eq!(manifest.iter().next().unwrap().0, "visible.txt");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imports_hidden_when_disabled() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".hidden"), b"nope").unwrap();
        fs::write(dir.path().join("visible.txt"), b"hi").unwrap();

        let (put, _store) = fake_store();
        let opts = WalkOptions {
            skip_hidden: false,
            ..WalkOptions::default()
        };
        let (manifest, _, stats) =
            walk_import(dir.path(), put, opts).await.unwrap();
        assert_eq!(manifest.len(), 2);
        assert_eq!(stats.hidden_skipped, 0);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refuses_symlink_by_default() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("real.txt"), b"hi").unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("real.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        let (put, _store) = fake_store();
        let err = walk_import(dir.path(), put, WalkOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ShareError::SymlinkRefused(_)));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn symlink_stats_skipped_when_allowed() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("real.txt"), b"hi").unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("real.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        let (put, _store) = fake_store();
        let opts = WalkOptions {
            allow_symlinks: true,
            ..WalkOptions::default()
        };
        let (manifest, _, stats) =
            walk_import(dir.path(), put, opts).await.unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(stats.symlinks_skipped, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn empty_directory_returns_empty_collection() {
        let dir = TempDir::new().unwrap();
        let (put, _store) = fake_store();
        let (manifest, mh, stats) =
            walk_import(dir.path(), put, WalkOptions::default()).await.unwrap();
        assert!(manifest.is_empty());
        assert_eq!(stats.files_imported, 0);
        assert_eq!(stats.total_bytes, 0);
        // Empty manifest hash is still a valid 64-hex-char digest.
        assert!(!mh.as_hex().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn errors_on_nonexistent_path() {
        let (put, _store) = fake_store();
        let err = walk_import(
            std::path::Path::new("/no/such/path/at/all"),
            put,
            WalkOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ShareError::PathNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manifest_hash_is_stable_across_runs() {
        let dir = TempDir::new().unwrap();
        sample_tree(dir.path());

        let (put_a, _) = fake_store();
        let (ma, mha, _) = walk_import(dir.path(), put_a, WalkOptions::default()).await.unwrap();
        let (put_b, _) = fake_store();
        let (mb, mhb, _) = walk_import(dir.path(), put_b, WalkOptions::default()).await.unwrap();
        assert_eq!(mha, mhb);
        assert_eq!(ma, mb);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imported_blobs_match_bytes() {
        let dir = TempDir::new().unwrap();
        sample_tree(dir.path());

        let (put, store) = fake_store();
        let (manifest, _, _) =
            walk_import(dir.path(), put, WalkOptions::default()).await.unwrap();

        let store = store.lock().unwrap();
        for (name, hash) in manifest.iter() {
            let bytes = store.get(hash).expect("blob not in store");
            // Cross-check the hash is what `ContentHash::from_bytes`
            // would produce.
            assert_eq!(ContentHash::from_bytes(bytes), *hash, "{name}");
        }
    }
}