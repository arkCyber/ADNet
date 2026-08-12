//! Realistic example: walk a directory into a `Collection` manifest,
//! build a printable `ShareTicket`, then locally `receive` it into a
//! `BlobStore` and verify Prometheus metrics got recorded.
//!
//! This is the same end-to-end shape as `metrics_e2e.rs`, but keeps
//! the wiring self-contained and prints a readable summary.
//!
//! Run with:
//! ```bash
//! cargo run -p adnet-share --example share_app
//! ```

use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_share::{
    ReceiveOptions, ShareTicket, receive, share_metrics, walk_import, WalkOptions,
};
use adnet_types::{ContentHash, NodeAddr, NodeId};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Stage a source directory.
    let src = tempfile::tempdir()?;
    std::fs::write(src.path().join("a.txt"), b"alpha")?;
    std::fs::write(src.path().join("b.txt"), b"bravocharlie")?;
    std::fs::create_dir(src.path().join("nested"))?;
    std::fs::write(src.path().join("nested/c.txt"), b"charlie-nested")?;

    // 2. Walk + import into a closure-backed putter that feeds
    //    `ContentHash::from_bytes`.
    let put = Arc::new(|bytes: &[u8]| Ok(ContentHash::from_bytes(bytes)));
    let (manifest, manifest_hash, walk_stats) =
        walk_import(src.path(), put, WalkOptions::default()).await?;
    println!(
        "walk: {} file(s) total {} byte(s)",
        walk_stats.files_imported, walk_stats.total_bytes
    );
    assert_eq!(walk_stats.files_imported, 3);

    // 3. Persist the same bytes into a real BlobStore.
    let store_dir = tempfile::tempdir()?;
    let store = BlobStore::new(store_dir.path())?;
    for (name, _hash) in manifest.iter() {
        let bytes = std::fs::read(src.path().join(name))?;
        store.put_bytes_sync(&bytes)?;
    }
    println!("blobs in store: {}", store.list_complete()?.len());

    // 4. Build a printable ShareTicket.
    let node_id = NodeId::random();
    let endpoint = NodeAddr::new(node_id.clone());
    let ticket = ShareTicket::new(&node_id, &endpoint, &manifest_hash, &manifest, 0)?;
    let ticket_str = ticket.encode();
    println!("ticket: {ticket_str}");

    // 5. Receive into a new directory using the manifest as the
    //    truth-of-record. This path records metrics into the
    //    `share_metrics()` registry.
    let out_dir = tempfile::tempdir()?;
    let stats = receive(
        &ticket,
        &manifest,
        &store,
        ReceiveOptions {
            out_dir: Some(out_dir.path().to_path_buf()),
            overwrite: false,
        },
    )
    .await?;

    println!(
        "receive: files written={} bytes written={} elapsed_ms={}",
        stats.files_written, stats.bytes_written, stats.elapsed_ms,
    );
    assert_eq!(stats.files_written, 3);

    // 6. Inspect the recorded metrics.
    let m = share_metrics();
    println!(
        "metrics: bytes_total={} bytes_done={} files_total={} files_done={} errors={}",
        m.receive_bytes_total.get(),
        m.receive_bytes_done.get(),
        m.receive_files_total.get(),
        m.receive_files_done.get(),
        m.receive_errors.get(),
    );
    assert!(m.receive_files_total.get() >= 1);

    // 7. Confirm the files landed under the output directory.
    for (name, _) in manifest.iter() {
        let p = out_dir.path().join(name);
        assert!(p.exists(), "missing file {p:?}");
    }
    println!("ok");
    Ok(())
}