//! Spin up a `MeshServer`, import a blob, fetch it back through the mesh
//! client with and without a `Range:` request, and verify byte equality.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-mesh --example server_and_client
//! ```

use std::io::Write;
use std::sync::Arc;

use a3net_blobstore::{BlobReader, BlobStore, CHUNK_SIZE};
use a3net_mesh::{MeshServer, fetch_from_mesh};
use a3net_types::{ByteRange, ContentHash, RangeSpec};
use tempfile::tempdir;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let dir = tempdir().unwrap();
    let store = Arc::new(BlobStore::new(dir.path()).unwrap());

    // Build a multi-chunk payload.
    let payload: Vec<u8> = (0..(CHUNK_SIZE * 3 + 257))
        .map(|i| ((i * 17) ^ 0x77) as u8)
        .collect();
    let src = dir.path().join("payload.bin");
    {
        let mut f = std::fs::File::create(&src).unwrap();
        f.write_all(&payload).unwrap();
        f.sync_all().unwrap();
    }
    let (hash, _size) = store.import_file_sync(&src).expect("import");
    println!("imported hash : {hash}");

    // Boot the mesh HTTP server.
    let server = MeshServer::start(store.clone()).await.expect("server");
    let base = format!("http://127.0.0.1:{}", server.port);
    println!("mesh server   : {base}");

    // --- 1. Whole-blob fetch ------------------------------------------
    let dest = dir.path().join("fetched.bin");
    let res = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &dest,
        RangeSpec::All,
    )
    .await
    .expect("whole fetch");
    let fetched = std::fs::read(&dest).unwrap();
    assert_eq!(fetched, payload);
    println!("whole fetch   : {} bytes from {} (ok)", res.bytes, res.peer);

    // --- 2. Single sub-range fetch ------------------------------------
    let r_dest = dir.path().join("range.bin");
    let range =
        RangeSpec::Single(ByteRange::new(CHUNK_SIZE as u64, CHUNK_SIZE as u64 * 2 + 100).unwrap());
    let res = fetch_from_mesh(&store, &hash, std::slice::from_ref(&base), &r_dest, range)
        .await
        .expect("range fetch");
    let slice = std::fs::read(&r_dest).unwrap();
    let expected_len = (CHUNK_SIZE * 2 + 100) - CHUNK_SIZE;
    assert_eq!(res.bytes, expected_len as u64);
    assert_eq!(slice, &payload[CHUNK_SIZE..CHUNK_SIZE * 2 + 100]);
    println!("range fetch   : {} bytes from {} (ok)", res.bytes, res.peer);

    // --- 3. Multi-range fetch (multipart/byteranges) ------------------
    let m_dest = dir.path().join("multi.bin");
    let multi = RangeSpec::Multi(vec![
        ByteRange::new(0, 50).unwrap(),
        ByteRange::new(CHUNK_SIZE as u64, CHUNK_SIZE as u64 + 50).unwrap(),
        ByteRange::new(payload.len() as u64 - 30, payload.len() as u64).unwrap(),
    ]);
    let res = fetch_from_mesh(&store, &hash, std::slice::from_ref(&base), &m_dest, multi)
        .await
        .expect("multi fetch");
    let m = std::fs::read(&m_dest).unwrap();
    let expected_total = 50 + 50 + 30;
    assert_eq!(res.bytes, expected_total);
    assert!(m.starts_with(&payload[..50]));
    assert!(m.ends_with(&payload[payload.len() - 30..]));
    println!("multi fetch   : {expected_total} bytes (multipart stripped, ok)");

    // --- 4. Hash mismatch / not-found -> error path --------------------
    let bogus = ContentHash::from_bytes(b"not imported");
    let err = fetch_from_mesh(
        &store,
        &bogus,
        std::slice::from_ref(&base),
        &dir.path().join("missing.bin"),
        RangeSpec::All,
    )
    .await;
    assert!(err.is_err());
    println!("bogus fetch   : rejected ({})", err.unwrap_err());

    // Sanity: blob still readable via BlobReader trait.
    let size = BlobReader::size(store.as_ref(), &hash).await.unwrap();
    let count = BlobReader::chunk_count(store.as_ref(), &hash)
        .await
        .unwrap();
    println!("readback      : size={size} chunks={count}");

    server.shutdown();
    println!("\nALL OK");
}
