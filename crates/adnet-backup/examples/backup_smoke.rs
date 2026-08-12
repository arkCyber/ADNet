//! End-to-end snapshot / restore demo.
//!
//! Builds a fake `data_dir` with three files, snapshots it,
//! then restores into a fresh directory and asserts the
//! restored files match the originals.
//!
//! Run with: `cargo run -p adnet-backup --example backup_smoke`.

use adnet_backup::{describe, restore, snapshot, verify};
use std::path::PathBuf;

fn write(dir: &PathBuf, rel: &str, body: &[u8]) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join("adnet-backup-smoke");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base)?;
    let src = base.join("src");
    let snap_out = base.join("snap.adnet-snap");
    let restored = base.join("restored");

    write(&src, "identity/keys.bin", b"node-id material");
    write(&src, "gossip/spool.jsonl", b"{\"room\":\"lobby\"}\n");
    write(&src, "blobs/a/b.txt", b"hello");

    println!("snapshotting {} -> {}", src.display(), snap_out.display());
    let manifest = snapshot(&src, &snap_out)?;
    println!("{}", describe(&manifest));

    println!("verifying {}", snap_out.display());
    let verified = verify(&snap_out)?;
    assert_eq!(verified, manifest);

    println!("restoring -> {}", restored.display());
    let restored_manifest = restore(&snap_out, &restored)?;
    assert_eq!(restored_manifest, manifest);

    let expected = b"hello";
    let actual = std::fs::read(restored.join("blobs/a/b.txt"))?;
    assert_eq!(actual, expected, "restored content mismatch");
    println!("round-trip OK");
    Ok(())
}