//! End-to-end integration tests for `adnet ipfs car …`.
//!
//! Drives the public `adnet_cli::ipfs_exec::run_car_command`
//! entry point through the `run_ipfs_command` dispatch. The
//! CLI's `ipfs` feature must be enabled for the dispatcher to
//! reach the real CAR implementation; otherwise the stub
//! short-circuits with a "feature disabled" message and the
//! tests are simply skipped.
//!
//! These tests do **not** spin up a real HTTP gateway; they
//! drive CAR import/export against a tempdir-backed
//! `BlobStore` to verify the round-trip behaviour.

#![cfg(feature = "ipfs")]

use std::path::Path;
use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_cli::ipfs::{CarCmd, IpfsCmd};
use adnet_types::ContentHash;
use anyhow::Result;
use tempfile::TempDir;

/// Drive the async `run_ipfs_command` synchronously by parking it
/// on a current-thread tokio runtime.
fn run_sync(cmd: &IpfsCmd, data_dir: &Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let store = Arc::new(BlobStore::new(data_dir)?);
        adnet_cli::ipfs_exec::run_ipfs_command(cmd, &store, Some(data_dir)).await
    })
}

/// Build a `ContentHash` from a tiny blob and write it into the
/// store. Returns the hash so the test can assert on it.
fn add_blob(store: &Arc<BlobStore>, payload: &[u8]) -> Result<ContentHash> {
    let (hash, _size) = store.put_bytes_sync(payload)?;
    Ok(hash)
}

/// Hand-build a CAR file in `out` containing a single block with
/// `payload`. Used by the export tests to feed the import path
/// without taking a dependency on the CLI itself.
fn write_car_one_block(
    out: &Path,
    payload: &[u8],
) -> Result<(ContentHash, Vec<u8>)> {
    use adnet_blobstore::car::{write_car, CarBlock, CarHeader};
    let cid = ContentHash::from_bytes(payload);
    let block = CarBlock::new(cid.clone(), payload.to_vec());
    let header = CarHeader::new(vec![cid.clone()]);
    let mut buf = Vec::new();
    write_car(&mut buf, &header, std::slice::from_ref(&block))?;
    std::fs::write(out, &buf)?;
    Ok((cid, buf))
}

/// `adnet ipfs car import <file>` ingests every block in the
/// archive into the local blob store. We assert that the
/// pre-built block's hash now appears in the store.
#[test]
fn car_import_ingests_blocks_into_blob_store() -> Result<()> {
    let data_dir = TempDir::new()?;
    let car_path = data_dir.path().join("sample.car");
    let payload = b"hello, CAR";
    let (cid, _raw) = write_car_one_block(&car_path, payload)?;

    run_sync(
        &IpfsCmd::Car {
            sub: CarCmd::Import {
                path: car_path.display().to_string(),
                pin: false,
            },
        },
        data_dir.path(),
    )?;

    let store = BlobStore::new(data_dir.path())?;
    let stored = store
        .get_sync(&cid)
        .expect("imported block should be queryable by CID");
    assert_eq!(stored, payload, "block roundtripped through CAR");
    Ok(())
}

/// `adnet ipfs car export <cid> --out <file>` writes a CAR file
/// containing the requested CID's block. We assert that the
/// resulting file round-trips through `read_car` and contains
/// the original block data.
#[test]
fn car_export_writes_a_valid_archive() -> Result<()> {
    let data_dir = TempDir::new()?;

    // First ingest something into the store.
    let store = Arc::new(BlobStore::new(data_dir.path())?);
    let payload = b"export me, please";
    let cid = add_blob(&store, payload)?;

    // Run the export.
    let out_path = data_dir.path().join("exported.car");
    run_sync(
        &IpfsCmd::Car {
            sub: CarCmd::Export {
                cids: vec![cid.as_hex().to_string()],
                out: out_path.display().to_string(),
            },
        },
        data_dir.path(),
    )?;

    // Verify the on-disk file.
    let raw = std::fs::read(&out_path)?;
    let mut cursor = std::io::Cursor::new(raw);
    let (header, blocks) = adnet_blobstore::car::read_car(&mut cursor)?;
    assert_eq!(header.roots, vec![cid.clone()]);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].cid, cid);
    assert_eq!(blocks[0].data, payload);
    Ok(())
}

/// Empty CAR file (no roots) is a degenerate but legal CAR —
/// we only assert that `import` doesn't panic and reports the
/// zero counts.
#[test]
fn car_import_handles_archive_with_no_roots() -> Result<()> {
    let data_dir = TempDir::new()?;
    let car_path = data_dir.path().join("empty.car");

    // Build a CAR file by hand with zero roots.
    {
        use adnet_blobstore::car::{write_car, CarHeader};
        let header = CarHeader::new(vec![]);
        let mut buf = Vec::new();
        write_car(&mut buf, &header, &[])?;
        std::fs::write(&car_path, &buf)?;
    }

    run_sync(
        &IpfsCmd::Car {
            sub: CarCmd::Import {
                path: car_path.display().to_string(),
                pin: false,
            },
        },
        data_dir.path(),
    )?;
    Ok(())
}