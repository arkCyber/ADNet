//! Realistic example: drive `a3net-rpc::commands::dag_put` / `dag_get` /
//! `block_put` against an in-memory blob store, exactly the way the
//! gateway's `/api/v0/...` handler would.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-rpc --example rpc_app
//! ```

use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_rpc::{
    block_get, block_put, block_rm, block_stat, dag_get, dag_put, version,
};

#[tokio::main]
async fn main() {
    let store = Arc::new(BlobStore::new(std::path::Path::new("")).expect("store"));

    // 1. Version info — same shape the IPFS CLI returns.
    let v = version().await.expect("version");
    println!("version: {v}");

    // 2. `dag/put` — add a node, get the CID back.
    let payload = b"a3net-rpc app example".to_vec();
    let put = dag_put(&store, payload.clone(), false).await.expect("dag_put");
    let cid = put["Cid"]["/"].as_str().unwrap().to_string();
    println!("dag_put -> {put}");
    assert_eq!(put["Size"].as_u64(), Some(payload.len() as u64));

    // 3. `dag/get` — fetch the same node.
    let got = dag_get(&store, &cid, None).await.expect("dag_get");
    let data_b64 = got["data"].as_str().unwrap();
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        data_b64,
    )
    .unwrap();
    println!("dag_get -> {} bytes", decoded.len());
    assert_eq!(decoded, payload);

    // 4. `block/put` / `block/get` / `block/stat` — same shape, different
    //    naming. Same store underneath.
    let block = b"another block".to_vec();
    let put = block_put(&store, block.clone(), false).await.expect("block_put");
    let block_cid = put["Key"].as_str().unwrap().to_string();
    println!("block_put -> {put}");

    let stat = block_stat(&store, &block_cid).await.expect("block_stat");
    println!("block_stat -> {stat}");

    let got = block_get(&store, &block_cid).await.expect("block_get");
    assert!(got["data"].as_str().unwrap().len() > 0);

    // 5. `block/rm` — remove the block.
    let rm = block_rm(&store, &block_cid, false).await.expect("block_rm");
    println!("block_rm -> {rm}");
    assert_eq!(rm["Removed"].as_bool(), Some(true));
}
