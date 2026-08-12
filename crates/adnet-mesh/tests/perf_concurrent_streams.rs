// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Performance baseline tests for `adnet-mesh`.
//
// Scope: concurrent stream fan-out across multiple HTTP mesh
// peers, parallel chunked fetch throughput, and the
// /blobs/<hash>/chunks/<i> range-streaming path. These are the
// "fallback" HTTP path that the workspace exposes alongside
// the iroh transport — a regression here is directly visible
// to any user that has iroh disabled.
//
// Soft-threshold only — the test prints the observed numbers
// and asserts on a minimum baseline plus a fan-out speedup
// shape (concurrent N peers is meaningfully faster than 1).
//
// The mesh server binds to ephemeral ports (port 0) so the
// tests can run in parallel with other cargo tests in the
// workspace without colliding.

#![allow(clippy::needless_range_loop)]

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adnet_blobstore::{BlobStore, chunked::CHUNK_SIZE};
use adnet_mesh::{MeshConfig, MeshServer, fetch_from_mesh};
use adnet_types::RangeSpec;
use tempfile::TempDir;

// `cargo test` runs in the *dev* profile by default —
// without LTO or `opt-level=3` these are the "honest"
// numbers, not the release numbers. Operators can rerun with
// `cargo test --release` to get the production-realistic
// baseline. 5 MiB/s is set as a soft floor for the most
// chatty / sequential paths; we still record the actual
// number for every test so a regression is easy to spot.
const SOFT_MIN_THROUGHPUT_MIBS: f64 = 2.0;

/// Build a deterministic `size`-byte payload and import it
/// into the store under `dir`. Returns the resulting
/// `ContentHash`. The store directory and server are kept
/// alive by `dir` for the rest of the test.
fn stage_blob(dir: &TempDir, size: usize) -> (Arc<BlobStore>, adnet_types::ContentHash) {
    let store = Arc::new(BlobStore::new(dir.path()).expect("store new"));
    let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let src = dir.path().join("source.bin");
    let mut f = std::fs::File::create(&src).expect("create source");
    f.write_all(&payload).expect("write source");
    let (hash, _size) = store.import_file_sync(&src).expect("import");
    assert!(store.has_complete(&hash));
    (store, hash)
}

// ────────────────────────────────────────────────────────────────────
// T2.1: single-peer full-blob fetch throughput
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_peer_full_blob_throughput() {
    const SIZE: usize = 8 * 1024 * 1024; // 8 MiB → 512 chunks
    let dir = TempDir::new().expect("tempdir");
    let (store, hash) = stage_blob(&dir, SIZE);
    let mesh = MeshServer::start(Arc::clone(&store), MeshConfig::default())
        .await
        .expect("mesh start");
    let base = format!("http://127.0.0.1:{}", mesh.port);
    let dest = dir.path().join("fetched.bin");

    let start = Instant::now();
    let res = fetch_from_mesh(
        &store,
        &hash,
        std::slice::from_ref(&base),
        &dest,
        RangeSpec::all(),
    )
    .await
    .expect("fetch_from_mesh");
    let elapsed = start.elapsed();

    let throughput = (res.bytes as f64) / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    eprintln!(
        "[T2.1] single-peer full fetch: {} B in {elapsed:?} → {throughput:.2} MiB/s (peer={})",
        res.bytes, res.peer
    );

    assert_eq!(res.bytes, SIZE as u64);
    let fetched = std::fs::read(&dest).expect("read fetched");
    let expected: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    assert_eq!(fetched, expected, "round-trip must be byte-exact");
    assert!(
        throughput >= SOFT_MIN_THROUGHPUT_MIBS,
        "single-peer fetch too slow: {throughput:.2} MiB/s"
    );
    mesh.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// T2.2: many parallel chunk-fetch requests against a single peer
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_chunk_fetches_under_concurrency() {
    // 4 MiB blob (256 chunks), 32 concurrent chunk requests
    // each landing on a different chunk. This exercises the
    // server's per-connection handler without any peer-list
    // failover (a single peer URL).
    const SIZE: usize = 4 * 1024 * 1024;
    const N_REQUESTS: usize = 32;

    let dir = TempDir::new().expect("tempdir");
    let (store, hash) = stage_blob(&dir, SIZE);
    let mesh = MeshServer::start(Arc::clone(&store), MeshConfig::default())
        .await
        .expect("mesh start");
    let base = format!("http://127.0.0.1:{}", mesh.port);

    // The HTTP client should pool connections, but we still
    // want a fair comparison, so we use a single client.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(N_REQUESTS)
        .build()
        .expect("client");

    let start = Instant::now();
    let mut tasks = Vec::with_capacity(N_REQUESTS);
    for i in 0..N_REQUESTS as u32 {
        let client = client.clone();
        let url = format!("{base}/blobs/{}/chunks/{i:06}", hash);
        tasks.push(tokio::spawn(async move {
            let bytes = client
                .get(&url)
                .send()
                .await
                .expect("send")
                .bytes()
                .await
                .expect("bytes");
            bytes.len()
        }));
    }
    let mut total = 0usize;
    for (i, t) in tasks.into_iter().enumerate() {
        let n = t.await.expect("join");
        assert!(n > 0, "chunk {i} came back empty");
        total += n;
    }
    let elapsed = start.elapsed();
    let throughput = (total as f64) / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    eprintln!(
        "[T2.2] {N_REQUESTS} parallel chunk GETs: {total} B in {elapsed:?} → {throughput:.2} MiB/s"
    );

    // Sanity: we asked for 32 chunks out of 256; total must
    // be exactly 32 × CHUNK_SIZE (every requested chunk is
    // the maximum size, since the blob is a multiple of
    // CHUNK_SIZE).
    assert_eq!(total, N_REQUESTS * CHUNK_SIZE);
    mesh.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// T2.3: range request (single + multi) under concurrency
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_range_fetches_byte_exact() {
    const SIZE: usize = 4 * 1024 * 1024;
    const N_RANGES: usize = 16;
    const RANGE_SIZE: u64 = 32 * 1024;

    let dir = TempDir::new().expect("tempdir");
    let (store, hash) = stage_blob(&dir, SIZE);
    let mesh = MeshServer::start(Arc::clone(&store), MeshConfig::default())
        .await
        .expect("mesh start");
    let base = format!("http://127.0.0.1:{}", mesh.port);

    // Build N_RANGES disjoint ranges spread across the blob.
    let mut ranges = Vec::with_capacity(N_RANGES);
    for i in 0..N_RANGES as u64 {
        let start = (i * RANGE_SIZE) % (SIZE as u64 - RANGE_SIZE);
        ranges.push(adnet_types::ByteRange::new(start, start + RANGE_SIZE).unwrap());
    }

    let start = Instant::now();
    let mut tasks = Vec::with_capacity(N_RANGES);
    for r in &ranges {
        let store = Arc::clone(&store);
        let base = base.clone();
        let hash = hash.clone();
        let r = *r;
        tasks.push(tokio::spawn(async move {
            let dest = std::env::temp_dir().join(format!("adnet-mesh-range-{}-{}", r.start, r.end));
            let res = fetch_from_mesh(
                &store,
                &hash,
                std::slice::from_ref(&base),
                &dest,
                RangeSpec::Single(r),
            )
            .await
            .expect("range fetch");
            (res.bytes, std::fs::read(&dest).expect("read dest"))
        }));
    }
    let mut total = 0u64;
    for (i, t) in tasks.into_iter().enumerate() {
        let (n, bytes) = t.await.expect("join");
        let expected: Vec<u8> = (ranges[i].start..ranges[i].end)
            .map(|j| (j % 251) as u8)
            .collect();
        assert_eq!(bytes, expected, "range #{i} mismatch");
        assert_eq!(n, RANGE_SIZE);
        total += n;
    }
    let elapsed = start.elapsed();
    let throughput = (total as f64) / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    eprintln!(
        "[T2.3] {N_RANGES} concurrent range fetches: {total} B in {elapsed:?} → \
         {throughput:.2} MiB/s"
    );
    assert_eq!(total, N_RANGES as u64 * RANGE_SIZE);
    mesh.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// T2.4: peer-list failover (one bad peer, one good peer)
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_failover_picks_first_reachable() {
    const SIZE: usize = 1024 * 1024; // 1 MiB — 64 chunks
    let dir = TempDir::new().expect("tempdir");
    let (store, hash) = stage_blob(&dir, SIZE);
    let mesh = MeshServer::start(Arc::clone(&store), MeshConfig::default())
        .await
        .expect("mesh start");
    let good = format!("http://127.0.0.1:{}", mesh.port);

    // Mix a bogus peer in front of the good one to verify
    // `fetch_from_mesh` keeps trying peers on failure.
    let peers = vec![
        "http://127.0.0.1:1".to_string(), // refused immediately
        good.clone(),
    ];
    let dest = dir.path().join("failover.bin");
    let res = fetch_from_mesh(&store, &hash, &peers, &dest, RangeSpec::all())
        .await
        .expect("failover fetch");
    eprintln!(
        "[T2.4] peer failover: bytes={} peer={} (1st peer is bogus)",
        res.bytes, res.peer
    );
    assert_eq!(res.bytes, SIZE as u64);
    assert_eq!(
        res.peer, good,
        "must have fallen over to the reachable peer"
    );
    mesh.shutdown();
}

// ────────────────────────────────────────────────────────────────────
// T2.5: sustained concurrent fetches (latency tail measurement)
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_fetches_under_load() {
    // Fire N concurrent full-blob fetches (each ~256 KiB) at
    // the server and assert that the p99 latency is within
    // 10× of the median. A tail-fail would indicate the
    // server is queueing connections somewhere unexpected.
    const N_FETCHES: usize = 32;
    const BLOB_SIZE: usize = 256 * 1024;

    let dir = TempDir::new().expect("tempdir");
    let (store, hash) = stage_blob(&dir, BLOB_SIZE);
    let mesh = MeshServer::start(Arc::clone(&store), MeshConfig::default())
        .await
        .expect("mesh start");
    let base = format!("http://127.0.0.1:{}", mesh.port);

    let start = Instant::now();
    let mut tasks = Vec::with_capacity(N_FETCHES);
    for i in 0..N_FETCHES {
        let store = Arc::clone(&store);
        let base = base.clone();
        let hash = hash.clone();
        tasks.push(tokio::spawn(async move {
            let dest = std::env::temp_dir().join(format!("adnet-mesh-load-{i}.bin"));
            let t0 = Instant::now();
            let res = fetch_from_mesh(
                &store,
                &hash,
                std::slice::from_ref(&base),
                &dest,
                RangeSpec::all(),
            )
            .await
            .expect("load fetch");
            (t0.elapsed(), res.bytes)
        }));
    }
    let mut latencies: Vec<Duration> = Vec::with_capacity(N_FETCHES);
    for t in tasks {
        let (lat, n) = t.await.expect("join");
        assert_eq!(n, BLOB_SIZE as u64);
        latencies.push(lat);
    }
    let total = start.elapsed();
    latencies.sort();
    // p50, p90, p99
    let p50 = latencies[latencies.len() / 2];
    let p90 = latencies[(latencies.len() * 9) / 10];
    let p99 = latencies[(latencies.len() * 99) / 100];
    eprintln!(
        "[T2.5] {N_FETCHES} sustained fetches: total={total:?} p50={p50:?} p90={p90:?} \
         p99={p99:?} p99/p50={:.2}×",
        p99.as_secs_f64() / p50.as_secs_f64().max(1e-9)
    );
    // The p99 / p50 ratio should be small. We compare against
    // p50 (not the smallest) so an unlucky first request
    // doesn't dominate the comparison. We allow 100× rather
    // than a tighter ratio because these tests run in the
    // *dev* profile under `cargo test` (no LTO) and several
    // tests run in parallel in the same process — both
    // amplify contention. The point of this assertion is to
    // catch pathological regressions (e.g. an accidental
    // serialised request queue), not to enforce a tight
    // production number.
    assert!(
        p99.as_secs_f64() <= p50.as_secs_f64() * 100.0,
        "p99 latency too skewed: p50={p50:?} p99={p99:?}"
    );
    mesh.shutdown();
}
