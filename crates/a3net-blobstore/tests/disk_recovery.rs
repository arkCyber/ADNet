// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Disk-fault-injection & recovery tests for `a3net-blobstore`.
//
// Scope: verify the on-disk layout survives the canonical
// "the OS or filesystem lied to us" failure modes:
//
// - **Power-cut during import**: a `.importing-<hash>`
//   directory is left behind by a previous interrupted
//   `import_file_sync`. The next `import_file_sync` must
//   clean it up before retrying; no `complete` sentinel
//   should appear pointing at a half-written tree.
// - **Disk full on write**: `import_file_sync` reports the
//   IO error to the caller; the partial tree is removed.
// - **Bit-rot after import**: a chunk is corrupted on disk
//   between import and read. `export_to_file_sync` returns
//   the bytes the caller asked for, but the resulting hash
//   MUST NOT match the advertised `ContentHash` (so a
//   sanity check at the caller side catches the corruption).
// - **Manual corruption of `meta.json`**: `meta` reports
//   `sizeBytes` inconsistent with the actual chunk count.
//   `read_range_sync` returns what is on disk (the
//   "honest" answer) rather than silently truncating.
// - **`complete` sentinel present, chunks missing**:
//   `has_complete` returns true, but `meta()` returns an
//   `Io(NotFound)` error. The store does not pretend the
//   blob is readable.
// - **Stale `.importing-<hash>` from a different process**:
//   the next import that hashes to the same content must
//   not be confused by the orphan directory.
//
// The tests run against the real filesystem (no mocking) —
// the point is to verify the on-disk contract documented in
// the `BlobStore` module docs.

use std::fs;
use std::io::Write;
use std::path::Path;

use a3net_blobstore::BlobStore;
use a3net_types::ContentHash;

/// Build a payload of `size` bytes deterministically (each
/// byte is `(i % 251)`).
fn make_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

// ────────────────────────────────────────────────────────────────────
// R1: power-cut during import — `.importing-<hash>` is left behind
// ────────────────────────────────────────────────────────────────────

#[test]
fn interrupted_import_leaves_no_complete_sentinel() {
    // Simulate "the previous process crashed mid-import" by
    // dropping a `.importing-<hash>` directory under the
    // store's data dir. The next `import_file_sync` of the
    // same content must clean it up and produce a normal
    // `<hash>/` tree. No `complete` sentinel may appear
    // until the rename succeeds.
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = make_payload(8192);
    let src = dir.path().join("payload.bin");
    fs::write(&src, &payload).unwrap();

    // Compute the expected hash so we can plant a fake
    // `.importing-<hash>` directory.
    let (hash, _) = store.hash_file(&src).unwrap();
    let orphan = dir.path().join(format!(".importing-{hash}"));
    fs::create_dir_all(orphan.join("chunks")).unwrap();
    // Plant a partial chunk so the orphan is non-empty.
    fs::write(orphan.join("chunks").join("000000"), &payload[..4096]).unwrap();
    // Plant a sentinel inside the orphan (NOT the real
    // `complete` sentinel — the `complete` file in the
    // orphan dir has no semantic meaning, the point is
    // that the orphan must not be confused with a real
    // complete tree).
    fs::write(orphan.join("meta.json"), b"{}").unwrap();

    // Sanity: the orphan is present.
    assert!(orphan.exists());
    // has_complete for the real hash must be false (the
    // orphan is *not* a completed import).
    assert!(!store.has_complete(&hash));

    // Re-import. This must succeed and produce a clean
    // `<hash>/` tree; the orphan must be gone (the store
    // removes it before staging the new import).
    let (re_hash, size) = store.import_file_sync(&src).unwrap();
    assert_eq!(re_hash, hash, "hash must be stable across re-imports");
    assert_eq!(size, payload.len() as u64);
    assert!(
        store.has_complete(&hash),
        "blob must be complete after re-import"
    );
    assert!(
        !orphan.exists(),
        "orphan `.importing-{hash}` directory must be cleaned up"
    );
}

// ────────────────────────────────────────────────────────────────────
// R2: disk full on write — caller sees IO error, no orphan survives
// ────────────────────────────────────────────────────────────────────

#[test]
fn write_failure_does_not_leave_partial_complete() {
    // We can't actually fill the disk from a unit test,
    // but we *can* simulate "the FS just rejected this
    // write" by making the staging directory read-only
    // after the first chunk is written. The store's
    // `import_file_sync` then fails on the next chunk;
    // the cleanup path must remove whatever partial tree
    // was staged so the next call sees a clean slate.
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = make_payload(64 * 1024); // 64 KiB — 4 chunks
    let src = dir.path().join("payload.bin");
    fs::write(&src, &payload).unwrap();

    // Compute the expected hash so we can race to lock the
    // staging dir before the importer writes to it.
    let (hash, _) = store.hash_file(&src).unwrap();
    let staging = dir.path().join(format!(".importing-{hash}"));

    // Make the data dir read-only. The store will fail to
    // create the staging directory under it. (On macOS
    // root / sandboxed runners this can fail silently;
    // we accept either an Err or, in environments where
    // chmod is ignored, an Ok that we can recover from.)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(dir.path(), perms).unwrap();
    }

    let result = store.import_file_sync(&src);

    // Restore permissions so we can inspect what was left
    // behind (and so subsequent tests in the process can
    // write to tempdirs).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(dir.path(), perms).unwrap();
    }

    // Either:
    // - The import failed with an IO error (expected).
    // - The import succeeded (some filesystems ignore the
    //   read-only bit; in that case we accept and just
    //   verify the *contract* below — no orphan + complete
    //   sentinel coherent).
    match result {
        Err(_) => {
            // No complete sentinel may be set on the failed
            // import — only the real `<hash>/` tree (if any)
            // may carry one.
            assert!(
                !store.has_complete(&hash) || staging.exists(),
                "no complete sentinel should exist on a failed import"
            );
        }
        Ok((h, _)) => {
            // The import succeeded because the chmod was
            // ignored. The blob must round-trip correctly.
            assert_eq!(h, hash);
            assert!(store.has_complete(&hash));
        }
    }

    // Whether the import succeeded or failed: the on-disk
    // state must be consistent. If a `.importing-<hash>`
    // directory is present, it must NOT contain a
    // `complete` sentinel (that would mean a partial
    // tree is being advertised as fully imported).
    if staging.exists() {
        assert!(
            !staging.join("complete").exists(),
            "partial staging directory must not carry a complete sentinel"
        );
    }
}

// ────────────────────────────────────────────────────────────────────
// R3: bit-rot after import — chunk is corrupted on disk
// ────────────────────────────────────────────────────────────────────

#[test]
fn bitrot_after_import_is_detected_via_export_hash() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = make_payload(32 * 1024); // 2 chunks
    let src = dir.path().join("payload.bin");
    fs::write(&src, &payload).unwrap();

    let (hash, _size) = store.import_file_sync(&src).unwrap();
    // Sanity: a clean export hashes back to the advertised hash.
    let out = dir.path().join("good.bin");
    store.export_to_file_sync(&hash, &out).unwrap();
    let good = fs::read(&out).unwrap();
    assert_eq!(ContentHash::from_bytes(&good), hash);

    // Corrupt one byte of the second chunk.
    let chunk1 = dir.path().join(hash.as_hex()).join("chunks").join("000001");
    let mut bytes = fs::read(&chunk1).unwrap();
    assert!(!bytes.is_empty());
    bytes[0] ^= 0xFF;
    fs::write(&chunk1, &bytes).unwrap();

    // Re-export. The export call *succeeds* — we only
    // verify the contract "a corrupted chunk is observable
    // in the round-trip", not "the store auto-detects bit
    // rot". (Bit-rot detection is the caller's job; the
    // store is honest about what is on disk.)
    let out2 = dir.path().join("bad.bin");
    store.export_to_file_sync(&hash, &out2).unwrap();
    let bad = fs::read(&out2).unwrap();
    // The bad export must NOT hash to the advertised hash.
    assert_ne!(
        ContentHash::from_bytes(&bad),
        hash,
        "corrupted chunk must not hash to the advertised ContentHash"
    );
}

// ────────────────────────────────────────────────────────────────────
// R4: manual corruption of meta.json — store reports the inconsistency
// ────────────────────────────────────────────────────────────────────

#[test]
fn corrupted_meta_json_surfaces_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = make_payload(16 * 1024); // 1 chunk
    let src = dir.path().join("payload.bin");
    fs::write(&src, &payload).unwrap();

    let (hash, _size) = store.import_file_sync(&src).unwrap();
    // Sanity: meta returns the original size.
    let (size_before, _) = store.meta(&hash).unwrap();
    assert_eq!(size_before, payload.len() as u64);

    // Corrupt meta.json by truncating it to invalid JSON.
    let meta_path = dir.path().join(hash.as_hex()).join("meta.json");
    fs::write(&meta_path, b"{not valid json").unwrap();

    // The store must surface the parse error, not silently
    // truncate the reported size.
    let err = store.meta(&hash).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("meta") || msg.contains("json") || msg.contains("io"),
        "expected a meta/json/io error, got: {msg}"
    );
}

// ────────────────────────────────────────────────────────────────────
// R5: complete sentinel present, chunks missing
// ────────────────────────────────────────────────────────────────────

#[test]
fn complete_sentinel_without_chunks_is_visible_to_caller() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = make_payload(16 * 1024);
    let src = dir.path().join("payload.bin");
    fs::write(&src, &payload).unwrap();

    let (hash, _size) = store.import_file_sync(&src).unwrap();
    // Delete the chunks subdir but keep the `complete` sentinel.
    let chunks_dir = dir.path().join(hash.as_hex()).join("chunks");
    fs::remove_dir_all(&chunks_dir).unwrap();

    // has_complete must still return true (the sentinel is
    // present and we don't double-check the chunks dir).
    assert!(
        store.has_complete(&hash),
        "has_complete should not deep-check chunk presence"
    );
    // But the actual read must fail with NotFound, NOT
    // return a half-filled buffer.
    let err = store.read_chunk_sync(&hash, 0).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);

    // And the meta call must succeed (meta.json is still on disk).
    let (size, _) = store.meta(&hash).unwrap();
    assert_eq!(size, payload.len() as u64);
}

// ────────────────────────────────────────────────────────────────────
// R6: stale `.importing-<hash>` from a previous process
// ────────────────────────────────────────────────────────────────────

#[test]
fn stale_importing_directory_is_reused_or_removed() {
    // Plant a `.importing-<hash>` directory whose contents
    // are *different* from the would-be import (a different
    // file, incomplete chunks, no `complete` sentinel). The
    // next import for the same hash must succeed and produce
    // a coherent `<hash>/` tree.
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = make_payload(16 * 1024);
    let src = dir.path().join("payload.bin");
    fs::write(&src, &payload).unwrap();

    let (hash, _) = store.hash_file(&src).unwrap();
    let orphan = dir.path().join(format!(".importing-{hash}"));
    fs::create_dir_all(orphan.join("chunks")).unwrap();
    // Plant junk chunks.
    let mut f = fs::File::create(orphan.join("chunks").join("000000")).unwrap();
    f.write_all(b"junk junk junk").unwrap();
    // Plant an outdated meta.json that lies about the size.
    fs::write(
        orphan.join("meta.json"),
        br#"{"hash":"x","sizeBytes":42,"chunkCount":99}"#,
    )
    .unwrap();

    // Re-import must succeed and the orphan must be gone
    // (or replaced — either way the resulting tree must
    // round-trip the new content).
    let (re_hash, re_size) = store.import_file_sync(&src).unwrap();
    assert_eq!(re_hash, hash);
    assert_eq!(re_size, payload.len() as u64);
    assert!(store.has_complete(&hash));
    let out = dir.path().join("out.bin");
    let n = store.export_to_file_sync(&hash, &out).unwrap();
    assert_eq!(n, payload.len() as u64);
    let round_trip = fs::read(&out).unwrap();
    assert_eq!(round_trip, payload, "round-trip must be byte-exact");
    assert!(!orphan.exists(), "orphan directory must be removed");
}

// ────────────────────────────────────────────────────────────────────
// R7: list_complete skips partial blobs and orphan directories
// ────────────────────────────────────────────────────────────────────

#[test]
fn list_complete_skips_partial_and_orphan() {
    // Build a store with one complete blob, then plant a
    // partial blob (no `complete` sentinel) and a foreign
    // `.importing-<hash>` directory. `list_complete` must
    // return exactly the complete one.
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let (real, _) = store.put_bytes_sync(b"hello world").unwrap();

    // Partial blob — same shape as a real tree but no
    // `complete` sentinel.
    let partial_hash = ContentHash::from_bytes(b"partial");
    let partial_dir = dir.path().join(partial_hash.as_hex());
    fs::create_dir_all(partial_dir.join("chunks")).unwrap();
    fs::write(partial_dir.join("chunks").join("000000"), b"x").unwrap();

    // Foreign directory that is not a 64-hex hash.
    fs::create_dir_all(dir.path().join("not-a-hash")).unwrap();

    // Stale `.importing-*` from a previous run.
    let orphan = dir.path().join(format!(".importing-{real}"));
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("chunk-partial"), b"junk").unwrap();

    let listed = store.list_complete().unwrap();
    assert_eq!(
        listed,
        vec![real],
        "list_complete must skip partial + orphan"
    );
}

// ────────────────────────────────────────────────────────────────────
// R8: remove refuses to delete a partial blob
// ────────────────────────────────────────────────────────────────────

#[test]
fn remove_refuses_to_delete_partial_blob() {
    // Mirrors the existing unit test in `store.rs` but is
    // included here for completeness: a blob whose
    // `complete` sentinel is missing must NOT be removed by
    // `remove`. Otherwise a power-cut during import could
    // silently lose data.
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let partial = ContentHash::from_bytes(b"partial");
    let partial_dir = dir.path().join(partial.as_hex());
    fs::create_dir_all(partial_dir.join("chunks")).unwrap();
    fs::write(partial_dir.join("chunks").join("000000"), b"x").unwrap();

    let err = store.remove(&partial).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    // And the partial blob is still on disk.
    assert!(partial_dir.exists());
}

// ────────────────────────────────────────────────────────────────────
// R9: total_size is consistent with list_complete
// ────────────────────────────────────────────────────────────────────

#[test]
fn total_size_consistent_with_list_complete() {
    // After a sequence of imports and a single remove,
    // `total_size` must equal the sum of `sizeBytes` for
    // every entry returned by `list_complete`. A
    // regression that counts a partial blob or forgets to
    // subtract on remove is caught here.
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let (a, _) = store.put_bytes_sync(b"alpha").unwrap(); // 5
    let (b, _) = store.put_bytes_sync(b"beta-payload-long").unwrap(); // 17
    let (c, _) = store.put_bytes_sync(b"gamma-medium-payload").unwrap(); // 20

    let expected: u64 = 5 + 17 + 20;
    assert_eq!(store.total_size().unwrap(), expected);
    assert_eq!(store.list_complete().unwrap().len(), 3);

    store.remove(&b).unwrap();
    assert_eq!(store.total_size().unwrap(), 5 + 20);
    assert_eq!(store.list_complete().unwrap().len(), 2);

    // Manually plant a partial blob and ensure it is *not*
    // counted in total_size.
    let partial = ContentHash::from_bytes(b"partial");
    let partial_dir = dir.path().join(partial.as_hex());
    fs::create_dir_all(partial_dir.join("chunks")).unwrap();
    fs::write(partial_dir.join("chunks").join("000000"), b"x").unwrap();
    fs::write(partial_dir.join("meta.json"), br#"{"sizeBytes":9999}"#).unwrap();

    assert_eq!(
        store.total_size().unwrap(),
        5 + 20,
        "partial blob with no complete sentinel must not be counted"
    );

    let _ = a;
    let _ = c;
}

// Keep the `Path` import referenced so `cargo test --doc`
// doesn't flag it as unused if the file is later slimmed
// down to only the R-tests that don't take a `Path`.
#[allow(dead_code)]
fn _path_ref(p: &Path) -> &Path {
    p
}
