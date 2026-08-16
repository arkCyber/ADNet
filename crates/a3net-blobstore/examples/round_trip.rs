//! Round-trip a small file through the `BlobStore`: write it as chunked
//! content, look it up by hash, read it back, and assert byte equality.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-blobstore --example round_trip
//! ```

use std::io::Write;

use a3net_blobstore::{BlobReader, BlobStore, CHUNK_SIZE};
use a3net_types::{ByteRange, RangeSpec};
use tempfile::tempdir;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("rt");

    rt.block_on(async {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path()).expect("open store");

        // --- 1. Build a payload that spans multiple chunks --------------
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 3 + 117))
            .map(|i| ((i * 31) ^ 0xA5) as u8)
            .collect();
        let (hash, size) = store
            .put_bytes_sync(&payload)
            .expect("import single-buffer");
        println!("imported hash : {hash}");
        println!("payload size : {size}");
        // put_bytes_sync writes one chunk regardless of size — by design.
        println!("chunks       : 1 (put_bytes_sync writes one chunk)");

        // --- 2. has() + size() + chunk_count() --------------------------
        assert!(store.has(&hash).await);
        let stored_size = BlobReader::size(&store, &hash).await.expect("size");
        let stored_chunks = BlobReader::chunk_count(&store, &hash)
            .await
            .expect("chunk count");
        assert_eq!(stored_size, size);
        assert_eq!(stored_chunks, 1);
        println!("size         : {stored_size}");
        println!("chunk_count  : {stored_chunks}");

        // --- 3. Read whole blob -----------------------------------------
        let full = BlobReader::read_all(&store, &hash)
            .await
            .expect("read full");
        assert_eq!(full, payload);
        println!("full read    : {} bytes (matches)", full.len());

        // --- 4. Read sub-range via read_range + RangeSpec ---------------
        // put_bytes_sync stores the whole payload in a single chunk, so
        // we pick a sub-range within that single chunk.
        let range = RangeSpec::Single(ByteRange::new(10, 20).unwrap());
        let slice = BlobReader::read_range(&store, &hash, range)
            .await
            .expect("range");
        assert_eq!(slice, &payload[10..20]);
        println!("range read   : {} bytes (matches)", slice.len());

        // --- 5. Read chunk 0 (single-chunk blob) ------------------------
        let chunk0 = BlobReader::read_chunk(&store, &hash, 0)
            .await
            .expect("c0")
            .expect("exists");
        // put_bytes_sync stores the entire payload in a single chunk, so
        // the on-disk chunk is the full payload (potentially > CHUNK_SIZE).
        assert_eq!(chunk0, payload);
        println!(
            "chunk[0]     : {} bytes (matches full payload)",
            chunk0.len()
        );

        // --- 5a. Out-of-chunk-index on single-chunk blob ----------------
        let missing = BlobReader::read_chunk(&store, &hash, 99)
            .await
            .expect("c99");
        assert!(missing.is_none());
        println!("chunk[99]    : None (out of range, ok)");

        // --- 6. Out-of-range -> error -----------------------------------
        let bad_range = RangeSpec::Single(ByteRange::new(0, payload.len() as u64 + 1).unwrap());
        let bad = BlobReader::read_range(&store, &hash, bad_range).await;
        assert!(bad.is_err());
        println!("out-of-range : rejected");

        // --- 7. Import from disk (multi-chunk file) ---------------------
        // Use a different payload so the hash differs from `hash` above.
        let multi_payload: Vec<u8> = (0..(CHUNK_SIZE * 3 + 117))
            .map(|i| ((i * 47) ^ 0x33) as u8)
            .collect();
        let tmp = tempdir().unwrap();
        let mut file = std::fs::File::create(tmp.path().join("data.bin")).unwrap();
        file.write_all(&multi_payload).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let (multi_hash, multi_size) = store
            .import_file_sync(&tmp.path().join("data.bin"))
            .expect("file import");
        assert_eq!(multi_size, multi_payload.len() as u64);
        let multi_chunks = BlobReader::chunk_count(&store, &multi_hash)
            .await
            .expect("multi chunks");
        assert_eq!(
            multi_chunks as usize,
            multi_payload.len().div_ceil(CHUNK_SIZE)
        );
        let multi_full = BlobReader::read_all(&store, &multi_hash)
            .await
            .expect("multi read");
        assert_eq!(multi_full, multi_payload);
        println!(
            "file import  : {multi_hash} ({} bytes, {} chunks, matches)",
            multi_size, multi_chunks
        );

        // --- 8. Multi-chunk range read ----------------------------------
        let big_range = RangeSpec::Single(
            ByteRange::new(CHUNK_SIZE as u64, CHUNK_SIZE as u64 * 2 + 50).unwrap(),
        );
        let big_slice = BlobReader::read_range(&store, &multi_hash, big_range)
            .await
            .expect("multi range");
        assert_eq!(big_slice, &multi_payload[CHUNK_SIZE..CHUNK_SIZE * 2 + 50]);
        println!(
            "multi range  : {} bytes (spans 2 chunks, matches)",
            big_slice.len()
        );

        println!("\nALL OK");
    });
}
