//! `a3net-mesh` 应用示例：导入多个 blob 启动 mesh server，然后从另一个
//! peer base 列表里并发 fetch。这是 `a3net-node` 在传输层不可用时的兜底逻辑。
//!
//! 运行：`cargo run -p a3net-mesh --example mesh_app`

use std::io::Write;
use std::sync::Arc;

use a3net_blobstore::{BlobStore, CHUNK_SIZE};
use a3net_mesh::{MeshServer, fetch_from_mesh};
use a3net_types::{ByteRange, ContentHash, RangeSpec};
use tempfile::tempdir;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = Arc::new(BlobStore::new(dir.path())?);

    // 1. 导入一个大于 1 chunk 的 payload。
    let payload: Vec<u8> = (0..(CHUNK_SIZE * 4 + 17)).map(|i| i as u8).collect();
    let src = dir.path().join("payload.bin");
    std::fs::File::create(&src)?.write_all(&payload)?;
    let (hash, size) = store.import_file_sync(&src)?;
    println!("imported {hash} (size={size})");

    // 2. 启动 mesh server。
    let handle = MeshServer::start(store.clone()).await?;
    let base = format!("http://127.0.0.1:{}", handle.port);
    println!("mesh server at {base}");

    // 3. 全量 fetch。
    let dest = dir.path().join("copy.bin");
    let result = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &dest,
        RangeSpec::All,
    )
    .await?;
    let read = std::fs::read(&dest)?;
    assert_eq!(read, payload);
    println!("full fetch: {} bytes from {}", result.bytes, result.peer);

    // 4. Range fetch（单段）。
    let range_dest = dir.path().join("range.bin");
    let range = RangeSpec::Single(ByteRange::new(CHUNK_SIZE as u64, (CHUNK_SIZE * 2 + 100) as u64)?);
    let result = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &range_dest,
        range,
    )
    .await?;
    let slice = std::fs::read(&range_dest)?;
    assert_eq!(
        slice,
        &payload[CHUNK_SIZE..CHUNK_SIZE * 2 + 100]
    );
    println!("range fetch: {} bytes", result.bytes);

    // 5. 多段 fetch（multipart/byteranges）。
    let multi_dest = dir.path().join("multi.bin");
    let multi = RangeSpec::Multi(vec![
        ByteRange::new(0, 32)?,
        ByteRange::new((CHUNK_SIZE * 3) as u64, (CHUNK_SIZE * 3 + 64) as u64)?,
        ByteRange::new((payload.len() - 16) as u64, payload.len() as u64)?,
    ]);
    let result = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &multi_dest,
        multi,
    )
    .await?;
    let multi_bytes = std::fs::read(&multi_dest)?;
    println!(
        "multi fetch: {} bytes (got {} payload bytes)",
        result.bytes,
        multi_bytes.len()
    );

    // 6. Not-found error path.
    let bogus = ContentHash::from_bytes(b"nope");
    let err = fetch_from_mesh(
        &store,
        &bogus,
        std::slice::from_ref(&base),
        &dir.path().join("missing.bin"),
        RangeSpec::All,
    )
    .await;
    println!("bogus fetch: rejected ({})", err.err().unwrap());

    handle.shutdown();
    Ok(())
}