//! One-process smoke test for the PR4 metric integration.
use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_share::{
    ReceiveOptions, ShareTicket, receive, share_metrics, walk_import, WalkOptions,
};
use adnet_types::{ContentHash, NodeAddr, NodeId};
use tempfile::TempDir;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let src = TempDir::new().unwrap();
    std::fs::write(src.path().join("a.txt"), b"alpha").unwrap();
    std::fs::write(src.path().join("b.txt"), b"bravocharlie").unwrap();

    let put = Arc::new(|bytes: &[u8]| Ok(ContentHash::from_bytes(bytes)));
    let (manifest, mh, _stats) = walk_import(src.path(), put, WalkOptions::default())
        .await
        .unwrap();

    let store = BlobStore::new(TempDir::new().unwrap().path()).unwrap();
    for (name, _hash) in manifest.iter() {
        let bytes = std::fs::read(src.path().join(name)).unwrap();
        store.put_bytes_sync(&bytes).unwrap();
    }

    let node_id = NodeId::random();
    let endpoint = NodeAddr::new(node_id.clone());
    let ticket = ShareTicket::new(&node_id, &endpoint, &mh, &manifest, 0).unwrap();

    let out = TempDir::new().unwrap();
    let stats = receive(
        &ticket,
        &manifest,
        &store,
        ReceiveOptions {
            out_dir: Some(out.path().to_path_buf()),
            overwrite: false,
        },
    )
    .await
    .unwrap();

    println!("STATS: {stats:?}");
    let m = share_metrics();
    println!("bytes_total:   {}", m.receive_bytes_total.get());
    println!("bytes_done:    {}", m.receive_bytes_done.get());
    println!("files_total:   {}", m.receive_files_total.get());
    println!("files_done:    {}", m.receive_files_done.get());
    println!("errors:        {}", m.receive_errors.get());
    println!("hist_count:    {}", m.receive_seconds.count());
    println!("hist_sum:      {}", m.receive_seconds.sum());
}
