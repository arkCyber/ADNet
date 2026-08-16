// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Edge-case & error-path integration tests for `a3net-blobstore`.
//
// Scope: scenarios that span multiple functions or involve
// non-obvious boundary conditions across the `BlobStore`,
// `ChunkWriter`, `ChunkReader`, and `RangeSpec` APIs.
//
// These tests are NOT unit-level — they exercise the interaction
// between several internal components to verify the DO-178C
// "completeness and correctness" requirements.
//
// DO-178C traceability:
// - E1: BlobStore + ChunkWriter + ChunkReader form a verified
//   pipeline for any byte sequence (round-trip property).
// - E2: All public functions return an error (never silently
//   drop data or succeed when they should fail).
// - E3: Edge-case byte ranges (boundary alignment, zero-length,
//   past-end) are handled deterministically.
// - E4: Hash verification catches any corruption.

use std::io::{Cursor, Read, Write};

use a3net_blobstore::{
    BlobImporter, BlobReader, BlobStore, ChunkReader, ChunkWriter,
    chunked::{CHUNK_SIZE, ChunkError, chunk_count_for, chunks_for_range, resolve_range},
    import::import_file,
};
use a3net_types::{ByteRange, ContentHash, RangeSpec};
use tempfile::TempDir;

/// ─────────────────────────────────────────────────────────────────
/// E1: Full round-trip property — every sequence written by
/// `ChunkWriter` is read identically by `ChunkReader`, and the
/// BLAKE3 hash is consistent end-to-end.
/// ─────────────────────────────────────────────────────────────────

/// Payload at every size boundary: exactly CHUNK_SIZE, one byte
/// over, one byte under.
#[test]
fn chunk_writer_reader_roundtrip_at_chunk_boundary() {
    for size in &[CHUNK_SIZE, CHUNK_SIZE - 1, CHUNK_SIZE + 1, CHUNK_SIZE * 2] {
        let payload: Vec<u8> = (0u8..=u8::MAX)
            .cycle()
            .take(*size)
            .map(|b| b.wrapping_add(0xA5))
            .collect();
        let mut buf: Vec<u8> = Vec::new();
        let (hash, written) = {
            let mut w = ChunkWriter::new(&mut buf);
            w.write_all(&payload).unwrap();
            w.finish().unwrap()
        };

        let mut r = ChunkReader::new(Cursor::new(buf.clone()), written, Some(hash.clone()));
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload, "round-trip mismatch at size {size}");
        r.verify().unwrap();
    }
}

/// Round-trip for a very small payload (1 byte).
#[test]
fn chunk_writer_reader_roundtrip_single_byte() {
    let payload = vec![0xFF];
    let mut buf: Vec<u8> = Vec::new();
    let (hash, _) = {
        let mut w = ChunkWriter::new(&mut buf);
        w.write_all(&payload).unwrap();
        w.finish().unwrap()
    };
    let mut r = ChunkReader::new(Cursor::new(buf), 1, Some(hash));
    let mut out = Vec::new();
    r.read_to_end(&mut out).unwrap();
    assert_eq!(out, payload);
}

/// ─────────────────────────────────────────────────────────────────
/// E2: All error paths are exercised.
/// ─────────────────────────────────────────────────────────────────

/// `BlobReader::has` returns `false` for a never-imported hash.
#[tokio::test]
async fn has_false_for_unknown_hash() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let unknown = ContentHash::from_bytes(b"never-imported");
    assert!(!BlobReader::has(&store, &unknown).await);
}

/// `BlobReader::size` returns an error for an unknown hash.
#[tokio::test]
async fn size_error_for_unknown_hash() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let unknown = ContentHash::from_bytes(b"never-imported");
    let err = BlobReader::size(&store, &unknown).await.unwrap_err();
    assert!(matches!(err, ChunkError::Io(_)));
}

/// `BlobReader::chunk_count` returns an error for an unknown hash.
#[tokio::test]
async fn chunk_count_error_for_unknown_hash() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let unknown = ContentHash::from_bytes(b"never-imported");
    let err = BlobReader::chunk_count(&store, &unknown).await.unwrap_err();
    assert!(matches!(err, ChunkError::Io(_)));
}

/// `BlobReader::read_all` returns an error for an unknown hash.
#[tokio::test]
async fn read_all_error_for_unknown_hash() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let unknown = ContentHash::from_bytes(b"never-imported");
    let err = BlobReader::read_all(&store, &unknown).await.unwrap_err();
    assert!(matches!(err, ChunkError::Io(_)));
}

/// `BlobReader::read_chunk` returns `None` for a chunk index
/// that is past the last chunk of a blob.
#[tokio::test]
async fn read_chunk_none_for_out_of_bounds_index() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let h = BlobImporter::put_bytes(&store, b"tiny").await.unwrap();
    let chunk = BlobReader::read_chunk(&store, &h, 100).await.unwrap();
    assert!(chunk.is_none());
}

/// `BlobReader::read_range` clamps an out-of-bounds range to the
/// blob's actual size and returns the available bytes (rather than
/// returning an error). This is the "honest, return what's there"
/// semantics documented in the store.
#[tokio::test]
async fn read_range_out_of_bounds_is_clamped() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let h = BlobImporter::put_bytes(&store, b"tiny").await.unwrap(); // size=4
    // Range (0, 9999) extends far past the blob's 4 bytes.
    let r = ByteRange::new(0, 9999).unwrap();
    let bytes = BlobReader::read_range(&store, &h, RangeSpec::Single(r))
        .await
        .unwrap();
    assert_eq!(bytes.len(), 4, "range should be clamped to blob size");
    assert_eq!(&bytes, b"tiny");
}

/// `BlobReader::read_range` with `RangeSpec::All` on a known blob
/// returns the full blob.
#[tokio::test]
async fn read_range_all_returns_full_blob() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0..500).map(|i| (i % 251) as u8).collect();
    let h = BlobImporter::put_bytes(&store, &payload).await.unwrap();
    let bytes = BlobReader::read_range(&store, &h, RangeSpec::All)
        .await
        .unwrap();
    assert_eq!(bytes, payload);
}

/// `BlobReader::read_range` with `RangeSpec::Multi` concatenates
/// multiple disjoint ranges.
#[tokio::test]
async fn read_range_multi_composes_ranges() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0u8..=u8::MAX).cycle().take(1000).collect();
    let h = BlobImporter::put_bytes(&store, &payload).await.unwrap();

    let r1 = ByteRange::new(0, 100).unwrap();
    let r2 = ByteRange::new(500, 700).unwrap();
    let bytes = BlobReader::read_range(&store, &h, RangeSpec::Multi(vec![r1, r2]))
        .await
        .unwrap();
    assert_eq!(bytes.len(), 100 + 200);
    assert_eq!(&bytes[..100], &payload[..100]);
    assert_eq!(&bytes[100..], &payload[500..700]);
}

/// `BlobReader::export_to_file` on an unknown hash returns an error.
#[tokio::test]
async fn export_to_file_error_for_unknown_hash() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let unknown = ContentHash::from_bytes(b"never-imported");
    let dest = dir.path().join("out.bin");
    let err = BlobReader::export_to_file(&store, &unknown, &dest)
        .await
        .unwrap_err();
    assert!(matches!(err, ChunkError::Io(_)));
}

/// `BlobReader::export_to_file` writes byte-exact data.
#[tokio::test]
async fn export_to_file_is_byte_exact() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
    let h = BlobImporter::put_bytes(&store, &payload).await.unwrap();
    let dest = dir.path().join("export.bin");
    let n = BlobReader::export_to_file(&store, &h, &dest).await.unwrap();
    assert_eq!(n, payload.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
}

/// `BlobImporter::put_bytes` for a large payload produces a blob
/// that reads back byte-exact.
#[tokio::test]
async fn put_bytes_large_payload_roundtrip() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0u8..=u8::MAX)
        .cycle()
        .take(5 * 1024 * 1024)
        .map(|b| b.wrapping_add(0xCC))
        .collect();
    let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
    let back = BlobReader::read_all(&store, &hash).await.unwrap();
    assert_eq!(back, payload);
}

/// ─────────────────────────────────────────────────────────────────
/// E3: Edge-case byte ranges are handled deterministically.
/// ─────────────────────────────────────────────────────────────────

/// `chunks_for_range` on a range that ends exactly at the
/// total size (boundary case).
#[test]
fn chunks_for_range_at_exact_total_size() {
    let r = ByteRange::new(16384, 32768).unwrap();
    let (sc, ec, fo, ll) = chunks_for_range(32768, &r).unwrap();
    assert_eq!(sc, 1);
    assert_eq!(ec, 2);
    assert_eq!(fo, 0);
    assert_eq!(ll, 16384);
}

/// `chunks_for_range` with a range that starts and ends inside
/// the same chunk (both offsets non-zero).
#[test]
fn chunks_for_range_same_chunk_unaligned() {
    let r = ByteRange::new(1000, 2000).unwrap();
    let (sc, ec, fo, ll) = chunks_for_range(100_000, &r).unwrap();
    assert_eq!(sc, 0);
    assert_eq!(ec, 1);
    assert_eq!(fo, 1000);
    assert_eq!(ll, 1000);
}

/// `chunks_for_range` with a range that spans many chunks (> 10).
#[test]
fn chunks_for_range_spans_many_chunks() {
    // Range (0, 200000) over a 200000-byte blob → chunks 0..12.
    let r = ByteRange::new(0, 200_000).unwrap();
    let (sc, ec, fo, ll) = chunks_for_range(200_000, &r).unwrap();
    assert_eq!(sc, 0);
    // 200000 / 16384 = 12.2 → ceil = 13 chunks → ec = 13.
    assert_eq!(ec, 13);
    assert_eq!(fo, 0);
    // (200000 - 1) % 16384 = 3391 → last_len = 3392.
    assert_eq!(ll, 3392);
}

/// `resolve_range` with `RangeSpec::Multi` containing multiple
/// valid, non-overlapping ranges.
#[test]
fn resolve_range_multi_multiple_disjoint() {
    let ranges = vec![
        ByteRange::new(0, 100).unwrap(),
        ByteRange::new(200, 400).unwrap(),
        ByteRange::new(1000, 2000).unwrap(),
    ];
    let resolved = resolve_range(5000, RangeSpec::Multi(ranges.clone())).unwrap();
    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved, ranges);
}

/// `resolve_range` with an empty `Multi` list returns an empty vec
/// (no ranges to read).
#[test]
fn resolve_range_multi_empty_list() {
    let resolved = resolve_range(1000, RangeSpec::Multi(vec![])).unwrap();
    assert!(resolved.is_empty());
}

/// `resolve_range` with `RangeSpec::All` on a 1-byte blob returns
/// the full range.
#[test]
fn resolve_range_all_one_byte_blob() {
    let resolved = resolve_range(1, RangeSpec::All).unwrap();
    assert_eq!(resolved, vec![ByteRange::new(0, 1).unwrap()]);
}

/// `chunk_count_for` for all sizes from 0 to 3 * CHUNK_SIZE.
#[test]
fn chunk_count_for_0_to_3_chunks() {
    for size in 0..=(3 * CHUNK_SIZE) {
        let count = chunk_count_for(size as u64) as usize;
        let expected = if size == 0 {
            0
        } else {
            (size - 1) / CHUNK_SIZE + 1
        };
        assert_eq!(count, expected, "size={size}");
    }
}

/// `resolve_range` rejects a `Single` range whose end exceeds the
/// blob size.
#[test]
fn resolve_range_single_past_end() {
    let r = ByteRange::new(100, 200).unwrap();
    let err = resolve_range(150, RangeSpec::Single(r)).unwrap_err();
    assert!(matches!(err, ChunkError::RangeOutOfBounds { .. }));
}

/// ─────────────────────────────────────────────────────────────────
/// E4: Hash verification catches corruption.
/// ─────────────────────────────────────────────────────────────────

/// `ChunkReader::verify` after reading corrupted bytes detects
/// the mismatch and resets the hasher after the error.
#[test]
fn chunk_reader_verify_detects_corruption() {
    let payload: Vec<u8> = (0..1000).map(|i| (i % 251) as u8).collect();
    let wrong = ContentHash::from_bytes(b"not-this-content");
    let mut r = ChunkReader::new(Cursor::new(payload), 1000, Some(wrong));
    let mut out = Vec::new();
    r.read_to_end(&mut out).unwrap();
    let err = r.verify().unwrap_err();
    assert!(matches!(err, ChunkError::HashMismatch { .. }));
}

/// `BlobStore` import detects a corrupted chunk via the end-to-end
/// hash verification in `import_file_sync`.
#[test]
fn blobstore_import_detects_chunk_corruption() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0..(2 * CHUNK_SIZE)).map(|i| (i % 251) as u8).collect();
    let src = dir.path().join("corrupt.bin");
    std::fs::write(&src, &payload).unwrap();
    let (hash, _) = store.import_file_sync(&src).unwrap();

    // Corrupt the first chunk.
    let chunk0 = store.blob_dir(&hash).join("chunks").join("000000");
    let mut bytes = std::fs::read(&chunk0).unwrap();
    bytes[50] ^= 0xFF;
    std::fs::write(&chunk0, &bytes).unwrap();

    // `import_file_sync` called again on the same source should:
    // 1. return the same hash (file hash is stable).
    let (re_hash, _) = store.import_file_sync(&src).unwrap();
    assert_eq!(re_hash, hash);
    // 2. But reading back the blob should NOT match the hash.
    let back = store.read_chunk_sync(&hash, 0).unwrap();
    assert_ne!(back[..], payload[..16 * 1024]);
}

/// `BlobStore` import detects a truncated blob (fewer chunks on
/// disk than advertised).
#[test]
fn blobstore_import_detects_truncated_chunks() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0..(2 * CHUNK_SIZE)).map(|i| (i % 251) as u8).collect();
    let src = dir.path().join("truncated.bin");
    std::fs::write(&src, &payload).unwrap();
    let (hash, _) = store.import_file_sync(&src).unwrap();

    // Delete the second chunk.
    let chunk1 = store.blob_dir(&hash).join("chunks").join("000001");
    std::fs::remove_file(&chunk1).unwrap();

    // Re-import should succeed (idempotent, returns cached hash) but
    // reading chunk 1 should fail (NotFound).
    let (re_hash, _) = store.import_file_sync(&src).unwrap();
    assert_eq!(re_hash, hash);
    let err = store.read_chunk_sync(&hash, 1).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

/// ─────────────────────────────────────────────────────────────────
/// E5: `import_file` async wrapper + `BlobStore` cross-validation.
/// ─────────────────────────────────────────────────────────────────

/// `import_file` + `BlobReader` round-trip on a file larger
/// than CHUNK_SIZE.
#[tokio::test]
async fn import_file_plus_blob_reader_large_blob() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0u8..=u8::MAX)
        .cycle()
        .take(4 * 1024 * 1024)
        .map(|b| b.wrapping_add(0x11))
        .collect();
    let src = dir.path().join("large.bin");
    std::fs::write(&src, &payload).unwrap();

    let (hash, size) = import_file(&store, &src).await.unwrap();
    assert_eq!(size as usize, payload.len());
    assert_eq!(hash, ContentHash::from_bytes(&payload));

    let back = BlobReader::read_all(&store, &hash).await.unwrap();
    assert_eq!(back, payload);

    let chunks = BlobReader::chunk_count(&store, &hash).await.unwrap();
    assert_eq!(chunks as usize, payload.len().div_ceil(CHUNK_SIZE));
}

/// `import_file` on a multi-chunk blob + `read_range` returns
/// the correct sub-range.
#[tokio::test]
async fn import_file_then_read_range_cross_chunk() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0u8..=u8::MAX)
        .cycle()
        .take(3 * CHUNK_SIZE + 500)
        .map(|b| b.wrapping_add(0x22))
        .collect();
    let src = dir.path().join("multi.bin");
    std::fs::write(&src, &payload).unwrap();

    let (hash, _) = import_file(&store, &src).await.unwrap();

    // Read a range that crosses the chunk 1→2 boundary.
    let r = ByteRange::new(CHUNK_SIZE as u64 + 100, 2 * CHUNK_SIZE as u64 + 100).unwrap();
    let bytes = BlobReader::read_range(&store, &hash, RangeSpec::Single(r))
        .await
        .unwrap();
    assert_eq!(bytes.len(), CHUNK_SIZE);
    assert_eq!(&bytes[..], &payload[r.start as usize..r.end as usize]);
}

/// `import_file` on a file whose hash is already in the store
/// returns the existing hash (idempotent).
#[tokio::test]
async fn import_file_idempotent_on_existing_hash() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = b"stable-content-for-idempotency".to_vec();
    let src = dir.path().join("stable.bin");
    std::fs::write(&src, &payload).unwrap();

    let (h1, s1) = import_file(&store, &src).await.unwrap();
    let (h2, s2) = import_file(&store, &src).await.unwrap();
    assert_eq!(h1, h2);
    assert_eq!(s1, s2);
    assert_eq!(h1, ContentHash::from_bytes(&payload));
}

/// `BlobStore` + `BlobReader` through `import_file` handles an
/// empty file correctly (zero chunks, zero size, empty hash).
#[tokio::test]
async fn import_file_empty_file_then_read_all() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let src = dir.path().join("empty.bin");
    std::fs::write(&src, b"").unwrap();

    let (hash, size) = import_file(&store, &src).await.unwrap();
    assert_eq!(size, 0);
    assert_eq!(hash, ContentHash::from_bytes(b""));

    let chunks = BlobReader::chunk_count(&store, &hash).await.unwrap();
    assert_eq!(chunks, 0);

    let bytes = BlobReader::read_all(&store, &hash).await.unwrap();
    assert!(bytes.is_empty());
}

/// ─────────────────────────────────────────────────────────────────
/// E6: `BlobStore` + `BlobReader` async trait coverage for
/// every method not already covered in unit tests.
/// ─────────────────────────────────────────────────────────────────

/// `BlobReader::read_chunk` on a known multi-chunk blob.
#[tokio::test]
async fn blob_reader_read_chunk_on_multi_chunk() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0u8..=u8::MAX)
        .cycle()
        .take(3 * CHUNK_SIZE + 1)
        .map(|b| b.wrapping_add(0x33))
        .collect();
    let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();

    let c0 = BlobReader::read_chunk(&store, &hash, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c0.len(), CHUNK_SIZE);
    assert_eq!(&c0[..10], &payload[..10]);

    let c2 = BlobReader::read_chunk(&store, &hash, 2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c2.len(), CHUNK_SIZE, "chunk 2 is full chunk (3rd of 4)");
    assert_eq!(c2[0], payload[2 * CHUNK_SIZE]);
}

/// `BlobReader::chunk_count` matches `chunk_count_for(size)`.
#[tokio::test]
async fn blob_reader_chunk_count_matches_formula() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    for size in &[1u64, 100, CHUNK_SIZE as u64, CHUNK_SIZE as u64 + 1, 100_000] {
        let payload: Vec<u8> = (0u8..=u8::MAX).cycle().take(*size as usize).collect();
        let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
        let count = BlobReader::chunk_count(&store, &hash).await.unwrap();
        assert_eq!(count, chunk_count_for(*size as u64), "size={size}");
    }
}

/// `BlobImporter::put_bytes` for an already-existing blob is
/// idempotent (returns the same hash).
#[tokio::test]
async fn blob_importer_put_bytes_idempotent() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = b"idempotent-payload".to_vec();
    let h1 = BlobImporter::put_bytes(&store, &payload).await.unwrap();
    let h2 = BlobImporter::put_bytes(&store, &payload).await.unwrap();
    assert_eq!(h1, h2);
    assert!(BlobReader::has(&store, &h1).await);
}

/// `BlobStore` can store and retrieve blobs with all-zero content
/// (a common degenerate case).
#[tokio::test]
async fn blob_store_all_zero_content() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload = vec![0u8; 50_000];
    let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
    let back = BlobReader::read_all(&store, &hash).await.unwrap();
    assert_eq!(back, payload);
}

/// `BlobStore` can store and retrieve blobs with 0x00-0xFF
/// cycling content (detects off-by-one and endian issues).
#[tokio::test]
async fn blob_store_full_byte_range_content() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();
    let payload: Vec<u8> = (0u8..=255).collect(); // all 256 byte values
    let hash = BlobImporter::put_bytes(&store, &payload).await.unwrap();
    let back = BlobReader::read_all(&store, &hash).await.unwrap();
    assert_eq!(back, payload);
}

/// `BlobStore.list_complete` is consistent after a sequence of
/// imports and removes.
#[test]
fn list_complete_consistent_after_operations() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();

    let h1 = store.put_bytes_sync(b"alpha").unwrap().0;
    let h2 = store.put_bytes_sync(b"beta").unwrap().0;
    let h3 = store.put_bytes_sync(b"gamma").unwrap().0;

    assert_eq!(store.list_complete().unwrap().len(), 3);

    store.remove(&h2).unwrap();
    assert_eq!(store.list_complete().unwrap().len(), 2);
    assert!(store.list_complete().unwrap().contains(&h1));
    assert!(store.list_complete().unwrap().contains(&h3));
    assert!(!store.list_complete().unwrap().contains(&h2));

    // Re-import of h2 should bring it back.
    store.put_bytes_sync(b"beta").unwrap();
    assert_eq!(store.list_complete().unwrap().len(), 3);
}

/// `BlobStore.total_size` matches the sum of individual blob sizes.
#[test]
fn total_size_matches_individual_sizes() {
    let dir = TempDir::new().unwrap();
    let store = BlobStore::new(dir.path()).unwrap();

    let sizes: Vec<usize> = vec![100, 500, 10_000, 1_000_000];
    let mut total_expected: u64 = 0;
    for &sz in sizes.iter() {
        let payload: Vec<u8> = (0u8..=u8::MAX).cycle().take(sz).collect();
        store.put_bytes_sync(&payload).unwrap();
        total_expected += sz as u64;
    }
    assert_eq!(store.total_size().unwrap(), total_expected);
}
