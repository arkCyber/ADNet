// SPDX-License-Identifier: MIT OR Apache-2.0
//
// End-to-end integration tests for `a3net-mesh`.
//
// These tests complement the unit tests inside `src/{client,server,lib}.rs`:
// they wire up a real `MeshServer`, drive it through `reqwest` and
// `fetch_from_mesh`, and assert behaviour at the public API boundary
// (round-trip byte equality, peer failover, error propagation, etc.).
//
// Every public function in `a3net-mesh` is exercised at least once by
// either this file or the in-module tests:
//
//   - `MeshServer::start`        → every test below
//   - `MeshServerHandle::port`   → every test below
//   - `MeshServerHandle::host`   → `handle_host_default_is_loopback_or_lan`
//   - `MeshServerHandle::shutdown` → `shutdown_terminates_server`
//   - `fetch_from_mesh`          → `full_blob_roundtrip`,
//                                  `multi_peer_failover`,
//                                  `bogus_hash_propagates_error`
//   - `MeshConfig`               → covered in `src/lib.rs`

#![allow(clippy::needless_range_loop)]

use std::io::Write;
use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_mesh::{MeshServer, MeshServerHandle, fetch_from_mesh};
use a3net_types::{ByteRange, ContentHash, RangeSpec};
use tempfile::TempDir;

/// Stage a deterministic `size`-byte payload in a tempdir-backed
/// `BlobStore` and return everything callers need to issue requests.
async fn stage_mesh(
    size: usize,
) -> (
    TempDir,
    Arc<BlobStore>,
    MeshServerHandle,
    Vec<u8>,
    ContentHash,
) {
    let dir = TempDir::new().expect("tempdir");
    let store = Arc::new(BlobStore::new(dir.path()).expect("store"));
    let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let src = dir.path().join("blob.bin");
    {
        let mut f = std::fs::File::create(&src).expect("create source");
        f.write_all(&payload).expect("write source");
    }
    let (hash, _) = store.import_file_sync(&src).expect("import");
    let handle = MeshServer::start(Arc::clone(&store), None).await.expect("mesh start");
    (dir, store, handle, payload, hash)
}

// ────────────────────────────────────────────────────────────────────
// Full-blob round-trip via `fetch_from_mesh` with RangeSpec::All.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn full_blob_roundtrip() {
    let (dir, store, handle, payload, hash) = stage_mesh(8 * 1024).await;
    let base = format!("http://127.0.0.1:{}", handle.port);
    let dest = dir.path().join("roundtrip.bin");

    let res = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &dest,
        RangeSpec::All,
    )
    .await
    .expect("full fetch");

    assert_eq!(res.bytes, payload.len() as u64);
    assert_eq!(res.peer, base);
    assert_eq!(std::fs::read(&dest).expect("read"), payload);
    handle.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// Single sub-range via `RangeSpec::Single`. Verifies the byte-exact
// slice end-to-end.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn single_range_roundtrip() {
    let (dir, store, handle, payload, hash) = stage_mesh(64 * 1024).await;
    let base = format!("http://127.0.0.1:{}", handle.port);
    let dest = dir.path().join("single.bin");
    let range = ByteRange::new(1024, 8192).unwrap();

    let res = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &dest,
        RangeSpec::Single(range),
    )
    .await
    .expect("range fetch");

    assert_eq!(res.bytes, (range.end - range.start));
    let bytes = std::fs::read(&dest).expect("read");
    assert_eq!(bytes, &payload[range.start as usize..range.end as usize]);
    handle.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// Multi-range (multipart/byteranges) round-trip — the trickiest path
// because both ends have to agree on the framing.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn multi_range_roundtrip() {
    let (dir, store, handle, payload, hash) = stage_mesh(128 * 1024).await;
    let base = format!("http://127.0.0.1:{}", handle.port);
    let dest = dir.path().join("multi.bin");
    let ranges = vec![
        ByteRange::new(0, 50).unwrap(),
        ByteRange::new(10_000, 11_000).unwrap(),
        ByteRange::new((payload.len() - 30) as u64, payload.len() as u64).unwrap(),
    ];
    let expected_total: u64 = ranges.iter().map(|r| r.end - r.start).sum();

    let res = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &dest,
        RangeSpec::Multi(ranges.clone()),
    )
    .await
    .expect("multi fetch");

    assert_eq!(res.bytes, expected_total);
    let body = std::fs::read(&dest).expect("read");
    assert_eq!(body.len() as u64, expected_total);

    // The extractor concatenates parts in declaration order, so we
    // can spot-check each range's bytes.
    let mut cursor = 0usize;
    for r in &ranges {
        let len = (r.end - r.start) as usize;
        assert_eq!(
            &body[cursor..cursor + len],
            &payload[r.start as usize..r.end as usize],
            "range {:?} mismatch",
            r
        );
        cursor += len;
    }
    handle.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// Peer-list failover: first base is bogus, second is reachable.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn multi_peer_failover() {
    let (dir, store, handle, payload, hash) = stage_mesh(32 * 1024).await;
    let good = format!("http://127.0.0.1:{}", handle.port);
    let peers = vec![
        "http://127.0.0.1:1".to_string(),
        "http://127.0.0.1:2".to_string(),
        good.clone(),
    ];
    let dest = dir.path().join("failover.bin");
    let res = fetch_from_mesh(&store, &hash, &peers, &dest, RangeSpec::All)
        .await
        .expect("failover");
    assert_eq!(res.bytes, payload.len() as u64);
    assert_eq!(res.peer, good);
    assert_eq!(std::fs::read(&dest).expect("read"), payload);
    handle.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// All peers unreachable → the call surfaces an error whose message
// starts with the canonical prefix.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn all_peers_unreachable() {
    let dir = TempDir::new().expect("tempdir");
    let store = BlobStore::new(dir.path()).expect("store");
    let dest = dir.path().join("x.bin");
    let peers = vec![
        "http://127.0.0.1:1".to_string(),
        "http://127.0.0.1:2".to_string(),
        "http://127.0.0.1:3".to_string(),
    ];
    let err = fetch_from_mesh(
        &store,
        &ContentHash::from_bytes(b"x"),
        &peers,
        &dest,
        RangeSpec::All,
    )
    .await
    .expect_err("must fail");
    assert!(
        err.starts_with("All mesh peers failed"),
        "unexpected error: {err}"
    );
}

// ────────────────────────────────────────────────────────────────────
// Empty peer list short-circuits without making any network call.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn empty_peer_list_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let store = BlobStore::new(dir.path()).expect("store");
    let dest = dir.path().join("x.bin");
    let err = fetch_from_mesh(
        &store,
        &ContentHash::from_bytes(b"x"),
        &[],
        &dest,
        RangeSpec::All,
    )
    .await
    .expect_err("must fail");
    assert!(err.contains("No peers"), "got: {err}");
}

// ────────────────────────────────────────────────────────────────────
// Fetching a blob that's never been imported → server returns 404,
// client surfaces a non-empty error.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn bogus_hash_propagates_error() {
    let (dir, store, handle, _payload, _hash) = stage_mesh(64).await;
    let base = format!("http://127.0.0.1:{}", handle.port);
    let bogus = ContentHash::from_bytes(b"never-imported");

    let err = fetch_from_mesh(
        &store,
        &bogus,
        std::slice::from_ref(&base),
        &dir.path().join("nope.bin"),
        RangeSpec::All,
    )
    .await
    .expect_err("must fail");
    assert!(!err.is_empty(), "error message must be non-empty");
    handle.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// `MeshServerHandle::host` defaults to either the loopback or a LAN
// address — we don't pin it, but we assert it is non-empty and parses
// as an IP.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn handle_host_default_is_loopback_or_lan() {
    let (_dir, _store, handle, _, _) = stage_mesh(32).await;
    assert!(!handle.host.is_empty());
    // Must round-trip through `IpAddr::from_str`. If a future change
    // returns something exotic (e.g. a hostname), this test catches it.
    let parsed: std::net::IpAddr = handle
        .host
        .parse()
        .unwrap_or_else(|e| panic!("handle.host={:?} must be an IP: {e}", handle.host));
    // We don't restrict to v4 or v6 specifically; just assert it's a
    // valid address.
    let _ = parsed;
    handle.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// `MeshServerHandle::shutdown` terminates the server: subsequent
// requests are refused.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn shutdown_terminates_server() {
    let (_dir, _store, handle, _, _) = stage_mesh(32).await;
    let port = handle.port;
    handle.shutdown();

    // The listener is closed; the connect attempt should fail. We give
    // the OS a brief grace period to release the socket.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .expect("client");
    let url = format!("http://127.0.0.1:{port}/health");
    let res = client.get(&url).send().await;
    assert!(
        res.is_err(),
        "post-shutdown GET must fail; got {:?}",
        res
    );
}

// ────────────────────────────────────────────────────────────────────
// Concurrent fetches against a single peer. Each fetch returns the
// same byte-exact payload, exercising the parallel chunk path many
// times in parallel.
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_full_blob_fetches_are_byte_exact() {
    const SIZE: usize = 32 * 1024; // 2 chunks
    const N: usize = 8;
    let (dir, store, handle, payload, hash) = stage_mesh(SIZE).await;
    let base = format!("http://127.0.0.1:{}", handle.port);
    // Clone once; each spawned task borrows its own copy so the outer
    // `payload` stays usable for the post-loop assertions.
    let payload_for_tasks = Arc::new(payload.clone());

    let mut tasks = Vec::with_capacity(N);
    for i in 0..N {
        let store = Arc::clone(&store);
        let base = base.clone();
        let hash = hash.clone();
        let expected = Arc::clone(&payload_for_tasks);
        let dest = dir.path().join(format!("c{i}.bin"));
        tasks.push(tokio::spawn(async move {
            let res = fetch_from_mesh(
                &store,
                &hash,
                std::slice::from_ref(&base),
                &dest,
                RangeSpec::All,
            )
            .await
            .expect("concurrent fetch");
            assert_eq!(res.bytes, expected.len() as u64);
            std::fs::read(&dest).expect("read dest")
        }));
    }
    for t in tasks {
        let bytes = t.await.expect("join");
        assert_eq!(bytes, *payload_for_tasks, "concurrent fetch byte mismatch");
    }
    handle.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// HEAD on a blob route must succeed (no body) so HTTP caches can
// probe the mesh without pulling bytes.
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn head_request_on_blob_route_succeeds() {
    let (_dir, _store, handle, _, hash) = stage_mesh(256).await;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/blobs/{hash}", handle.port);
    let resp = client.head(&url).send().await.expect("HEAD send");
    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        resp.bytes().await.expect("HEAD body").is_empty(),
        "HEAD must not return a body"
    );
    handle.shutdown();
}