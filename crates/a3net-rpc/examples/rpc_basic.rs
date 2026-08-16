//! Tiny example: stand up an `RpcClient` against an in-memory `BlobStore`,
//! put a block, and read it back via `get_block` / `block_stat`.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-rpc --example rpc_basic
//! ```

use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_rpc::client::RpcClient;

#[tokio::main]
async fn main() {
    // 1. Build a fresh in-memory blob store.
    let store = Arc::new(BlobStore::new(std::path::Path::new("")).expect("store"));
    let client = RpcClient::new(store);

    // 2. Put a block.
    let payload = b"hello a3net-rpc";
    let cid = client
        .put_block(payload)
        .await
        .expect("put_block");
    println!("put_block -> {cid}");

    // 3. Read it back.
    let read = client.get_block(&cid).await.expect("get_block");
    println!("get_block -> {}", String::from_utf8_lossy(&read));
    assert_eq!(read, payload);

    // 4. Block stats.
    let stat = client.block_stat(&cid).await.expect("block_stat");
    println!("block_stat -> {stat:?}");
    assert_eq!(stat.size, payload.len() as u64);

    // 5. ID of the node (sanity-checks the placeholder).
    let info = client.node_id().await.expect("node_id");
    println!("node_id -> agent={}, protocol={}", info.agent_version, info.protocol_version);
}
