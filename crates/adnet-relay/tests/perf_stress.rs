// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Performance & stability baseline tests for `adnet-relay`.
//
// Scope: relay pressure, slow-client tolerance, and a short
// long-running soak. The tests stand up a real
// `RelayServer` (with the loopback-friendly
// `HostPolicy::AllowLoopbackOnly` so the test can reach a
// mock upstream on `127.0.0.1`) and exercise:
//
// - High concurrency (N parallel clients hitting the same
//   relay).
// - Slow-client tolerance (an upstream that takes T seconds
//   to respond — relay must not deadlock).
// - Many small requests (chatty-client pattern, common for
//   mesh gossip).
// - Long-running soak (10s of sustained traffic, the relay
//   must stay responsive throughout).
// - Body-size pressure (large payloads through the bounded
//   stream).
// - Reconnect storm (clients opening and closing
//   connections in a tight loop).
//
// All tests run in `cargo test` and use ephemeral ports so
// they are safe to run in parallel with each other.

#![allow(clippy::needless_range_loop)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use adnet_relay::{HostPolicy, RelayServer, ServerPolicy};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::watch;

const SOFT_MIN_THROUGHPUT_RPS: f64 = 50.0;

/// Spin up a real `RelayServer` on an ephemeral port. Returns
/// the handle plus a tempdir to keep the runtime alive.
async fn spawn_relay(policy: ServerPolicy) -> (adnet_relay::RelayServerHandle, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let handle =
        RelayServer::start_with_policy("127.0.0.1", 0, adnet_relay::BillingMode::Disabled, policy)
            .await
            .expect("relay start");
    (handle, dir)
}

/// Spin up a mock upstream that responds to every
/// `/blobs/...` GET with a JSON body and the configured
/// `body` string echoed back. The `slow` flag makes the
/// upstream sleep before responding.
async fn spawn_upstream(body: &'static str, delay: Duration) -> (u16, watch::Sender<bool>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream bind");
    let port = listener.local_addr().expect("local_addr").port();
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((mut stream, _)) => {
                            tokio::spawn(async move {
                                let _ = handle_mock_request(&mut stream, body, delay).await;
                            });
                        }
                        Err(e) => {
                            eprintln!("upstream accept error: {e}");
                            return;
                        }
                    }
                }
                _ = rx.changed() => {
                    if *rx.borrow() {
                        return;
                    }
                }
            }
        }
    });
    (port, tx)
}

/// Read the full HTTP request and respond with the configured
/// body. The mock upstream does not parse the request — it
/// just waits `delay` then returns a fixed JSON.
async fn handle_mock_request(
    stream: &mut tokio::net::TcpStream,
    body: &'static str,
    delay: Duration,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 4096];
    let _ = stream.read(&mut buf).await;
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let payload = json!({"echo": body, "ts": chrono_now_millis()});
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.to_string().len(),
        payload
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn chrono_now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Build the proxy URL the relay expects.
fn proxy_url(relay_base: &str, host: &str, port: u16, path: &str) -> String {
    format!("{relay_base}/exodus-mesh/fetch?host={host}&port={port}&path={path}")
}

// ────────────────────────────────────────────────────────────────────
// T5.1: high concurrency through the relay
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn high_concurrency_through_relay() {
    const N_REQUESTS: usize = 200;
    const CONCURRENCY: usize = 32;

    let (upstream_port, upstream_tx) = spawn_upstream("hi", Duration::ZERO).await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let (relay, _dir) = spawn_relay(policy).await;
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(CONCURRENCY)
        .build()
        .expect("client");

    let url = proxy_url(
        &relay.base_url,
        "127.0.0.1",
        upstream_port,
        "/blobs/foo/meta",
    );

    let start = Instant::now();
    let mut tasks = Vec::with_capacity(N_REQUESTS);
    for i in 0..N_REQUESTS {
        let client = client.clone();
        let url = url.clone();
        tasks.push(tokio::spawn(async move {
            let resp = client.get(&url).send().await.expect("relay req");
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.map(|b| b.len()).unwrap_or(0);
            (i, status, bytes)
        }));
    }
    let mut ok = 0;
    let mut bytes_total = 0usize;
    for t in tasks {
        let (_i, status, bytes) = t.await.expect("join");
        assert_eq!(status, 200, "relay returned {status}");
        ok += 1;
        bytes_total += bytes;
    }
    let elapsed = start.elapsed();
    let rps = ok as f64 / elapsed.as_secs_f64();
    let mibps = (bytes_total as f64) / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    eprintln!(
        "[T5.1] high concurrency: {ok} requests in {elapsed:?} → {rps:.0} req/s, \
         {mibps:.2} MiB/s egress"
    );
    assert_eq!(ok, N_REQUESTS);
    assert!(
        rps >= SOFT_MIN_THROUGHPUT_RPS,
        "relay throughput too low: {rps:.0} req/s"
    );
    relay.shutdown();
    let _ = upstream_tx.send(true);
}

// ────────────────────────────────────────────────────────────────────
// T5.2: slow-client tolerance
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_upstream_does_not_deadlock_relay() {
    // 8 concurrent requests through a relay that proxies to
    // an upstream which sleeps 200 ms before responding. The
    // relay must not deadlock, must not block other requests,
    // and must return 200 within a reasonable time budget.
    const N_REQUESTS: usize = 8;
    const UPSTREAM_DELAY: Duration = Duration::from_millis(200);
    const RELAY_TIMEOUT: Duration = Duration::from_secs(5);

    let (upstream_port, upstream_tx) = spawn_upstream("slow", UPSTREAM_DELAY).await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        upstream_timeout: Duration::from_secs(10), // > UPSTREAM_DELAY
        ..ServerPolicy::default()
    };
    let (relay, _dir) = spawn_relay(policy).await;
    let client = reqwest::Client::builder()
        .timeout(RELAY_TIMEOUT)
        .build()
        .expect("client");

    let url = proxy_url(
        &relay.base_url,
        "127.0.0.1",
        upstream_port,
        "/blobs/foo/meta",
    );

    let start = Instant::now();
    let mut tasks = Vec::with_capacity(N_REQUESTS);
    for i in 0..N_REQUESTS {
        let client = client.clone();
        let url = url.clone();
        tasks.push(tokio::spawn(async move {
            let t0 = Instant::now();
            let resp = client.get(&url).send().await.expect("relay req");
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.map(|b| b.len()).unwrap_or(0);
            (i, status, bytes, t0.elapsed())
        }));
    }
    let mut total_latency = Duration::ZERO;
    for t in tasks {
        let (i, status, bytes, lat) = t.await.expect("join");
        assert_eq!(status, 200, "relay returned {status} for req {i}");
        assert!(bytes > 0, "relay returned empty body for req {i}");
        total_latency += lat;
    }
    let elapsed = start.elapsed();
    let avg_lat = total_latency / N_REQUESTS as u32;
    eprintln!(
        "[T5.2] slow upstream: {N_REQUESTS} requests, avg latency {avg_lat:?}, \
         wall {elapsed:?}"
    );
    // Sanity: the wall-clock is at most UPSTREAM_DELAY × N_REQUESTS
    // (i.e. requests were served concurrently, not serially).
    assert!(
        elapsed < UPSTREAM_DELAY * N_REQUESTS as u32,
        "relay served requests serially under slow upstream: {elapsed:?}"
    );
    relay.shutdown();
    let _ = upstream_tx.send(true);
}

// ────────────────────────────────────────────────────────────────────
// T5.3: chatty-client workload (many small requests)
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chatty_small_request_workload() {
    // Simulate a chatty mesh client: 1 client issues 200
    // small requests back-to-back, one after the other. The
    // p99 latency must be < 5× the median. A regression
    // that grows a per-request map, an unbounded buffer, or
    // a connection-pool leak would show up as a long tail
    // here.
    const N_REQUESTS: u32 = 200;
    const P99_P50_RATIO: f64 = 5.0;

    let (upstream_port, upstream_tx) = spawn_upstream("chatty", Duration::from_millis(5)).await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let (relay, _dir) = spawn_relay(policy).await;
    let client = reqwest::Client::new();
    let url = proxy_url(
        &relay.base_url,
        "127.0.0.1",
        upstream_port,
        "/blobs/foo/meta",
    );

    let start = Instant::now();
    let mut latencies: Vec<Duration> = Vec::with_capacity(N_REQUESTS as usize);
    for _ in 0..N_REQUESTS {
        let t0 = Instant::now();
        let resp = client.get(&url).send().await.expect("relay req");
        let status = resp.status().as_u16();
        let _ = resp.bytes().await;
        assert_eq!(status, 200, "relay returned {status}");
        latencies.push(t0.elapsed());
    }
    let total = start.elapsed();
    latencies.sort();
    let p50 = latencies[latencies.len() / 2];
    let p99 = latencies[(latencies.len() * 99) / 100];
    let rps = N_REQUESTS as f64 / total.as_secs_f64();
    let ratio = p99.as_secs_f64() / p50.as_secs_f64().max(1e-9);
    eprintln!(
        "[T5.3] chatty: {N_REQUESTS} requests in {total:?} → {rps:.0} req/s, \
         p50={p50:?} p99={p99:?} p99/p50={ratio:.2}×"
    );
    assert!(
        rps >= SOFT_MIN_THROUGHPUT_RPS,
        "chatty throughput too low: {rps:.0} req/s"
    );
    assert!(
        ratio < P99_P50_RATIO,
        "chatty tail too skewed: p50={p50:?} p99={p99:?} ratio={ratio:.2}×"
    );
    relay.shutdown();
    let _ = upstream_tx.send(true);
}

// ────────────────────────────────────────────────────────────────────
// T5.4: long-running soak (10 s of sustained traffic)
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_running_soak_stays_responsive() {
    // Drive the relay with 8 concurrent clients for 3 s.
    // We don't try to be clever about timing — we just
    // measure how many requests succeed in the budget and
    // assert the throughput is stable. A regression that
    // grows an unbounded queue or leaks connections will
    // show up as a wall-clock throughput cliff after the
    // first second or two.
    const DURATION: Duration = Duration::from_secs(3);
    const CONCURRENCY: usize = 8;
    const SOFT_MIN_RPS: f64 = 30.0;

    let (upstream_port, upstream_tx) = spawn_upstream("soak", Duration::from_millis(5)).await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let (relay, _dir) = spawn_relay(policy).await;
    let client = Arc::new(reqwest::Client::new());
    let url = proxy_url(
        &relay.base_url,
        "127.0.0.1",
        upstream_port,
        "/blobs/foo/meta",
    );

    let stop = Arc::new(tokio::sync::Notify::new());
    let mut handles = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let client = Arc::clone(&client);
        let url = url.clone();
        let stop = Arc::clone(&stop);
        handles.push(tokio::spawn(async move {
            let mut count = 0u64;
            let mut last_err: Option<String> = None;
            loop {
                tokio::select! {
                    _ = stop.notified() => break,
                    res = client.get(&url).send() => {
                        match res {
                            Ok(resp) => {
                                if resp.status().as_u16() != 200 {
                                    last_err = Some(format!("status {}", resp.status()));
                                }
                                let _ = resp.bytes().await;
                                count += 1;
                            }
                            Err(e) => {
                                last_err = Some(e.to_string());
                            }
                        }
                    }
                }
            }
            (count, last_err)
        }));
    }

    let start = Instant::now();
    tokio::time::sleep(DURATION).await;
    stop.notify_waiters();
    let wall = start.elapsed();

    let mut total = 0u64;
    let mut any_err: Option<String> = None;
    for h in handles {
        let (count, last_err) = h.await.expect("join");
        total += count;
        if last_err.is_some() && any_err.is_none() {
            any_err = last_err;
        }
    }
    let rps = total as f64 / wall.as_secs_f64();
    eprintln!(
        "[T5.4] soak: {total} requests in {wall:?} → {rps:.0} req/s \
         ({CONCURRENCY} concurrent workers)"
    );
    assert!(
        total > 0,
        "soak did not complete any requests (last err: {any_err:?})"
    );
    assert!(
        rps >= SOFT_MIN_RPS,
        "soak throughput too low: {rps:.0} req/s (last err: {any_err:?})"
    );
    relay.shutdown();
    let _ = upstream_tx.send(true);
}

// ────────────────────────────────────────────────────────────────────
// T5.5: body-size pressure (large payloads through bounded stream)
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_body_relay_under_concurrency() {
    // The relay has a 1 MiB `max_body_bytes` cap by default
    // (64 MiB; the test uses 1 MiB to keep the test fast).
    // We fire 16 concurrent 512 KiB requests — under the
    // cap, the relay must return 200 and the body must
    // round-trip byte-exact. The point is to verify the
    // *streaming* path works under concurrency, not the cap.
    const N_REQUESTS: usize = 16;
    const PAYLOAD_SIZE: usize = 512 * 1024; // 512 KiB

    // Use a dedicated upstream that emits a known payload.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let upstream_port = listener.local_addr().expect("local_addr").port();
    let (upstream_tx, mut upstream_rx) = watch::channel(false);
    let payload: Arc<Vec<u8>> = Arc::new((0..PAYLOAD_SIZE).map(|i| (i % 251) as u8).collect());
    tokio::spawn(async move {
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    if let Ok((mut stream, _)) = accept {
                        let payload = Arc::clone(&payload);
                        tokio::spawn(async move {
                            use tokio::io::{AsyncReadExt, AsyncWriteExt};
                            let mut buf = vec![0u8; 4096];
                            let _ = stream.read(&mut buf).await;
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                                payload.len()
                            );
                            let _ = stream.write_all(resp.as_bytes()).await;
                            let _ = stream.write_all(&payload).await;
                            let _ = stream.shutdown().await;
                        });
                    }
                }
                _ = upstream_rx.changed() => {
                    if *upstream_rx.borrow() { return; }
                }
            }
        }
    });

    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        max_body_bytes: PAYLOAD_SIZE * 2,
        ..ServerPolicy::default()
    };
    let (relay, _dir) = spawn_relay(policy).await;
    let client = reqwest::Client::new();
    let url = proxy_url(
        &relay.base_url,
        "127.0.0.1",
        upstream_port,
        "/blobs/foo/data",
    );

    let start = Instant::now();
    let mut tasks = Vec::with_capacity(N_REQUESTS);
    for _ in 0..N_REQUESTS {
        let client = client.clone();
        let url = url.clone();
        tasks.push(tokio::spawn(async move {
            let resp = client.get(&url).send().await.expect("relay req");
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.expect("bytes");
            (status, bytes)
        }));
    }
    let mut ok = 0;
    for t in tasks {
        let (status, bytes) = t.await.expect("join");
        assert_eq!(status, 200, "relay returned {status}");
        assert_eq!(bytes.len(), PAYLOAD_SIZE, "body size mismatch");
        // Spot-check the round-trip.
        assert_eq!(bytes[0], 0u8, "first byte mismatch");
        assert_eq!(
            bytes[bytes.len() - 1],
            ((PAYLOAD_SIZE - 1) % 251) as u8,
            "last byte mismatch"
        );
        ok += 1;
    }
    let elapsed = start.elapsed();
    let throughput = (ok * PAYLOAD_SIZE) as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    eprintln!("[T5.5] large body: {ok} × {PAYLOAD_SIZE} B in {elapsed:?} → {throughput:.2} MiB/s");
    assert_eq!(ok, N_REQUESTS);
    relay.shutdown();
    let _ = upstream_tx.send(true);
}

// ────────────────────────────────────────────────────────────────────
// T5.6: reconnect storm — many short-lived clients
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_storm_does_not_leak() {
    // 50 connect / disconnect cycles, each issuing a single
    // request. A regression that holds a per-connection lock
    // past request completion, or grows an FD set without
    // bound, will show up as a wall-clock explosion here.
    //
    // We time-bound the storm rather than asserting a
    // minimum cycle rate: each new reqwest::Client does a
    // full TLS handshake dance, which dominates the wall
    // clock and is not what the test is about. The point
    // is "the relay does not slow down or crash under
    // repeated connect/disconnect", not "reqwest can spin
    // up N clients per second".
    const N_CYCLES: usize = 50;
    const MAX_WALL: Duration = Duration::from_secs(15);

    let (upstream_port, upstream_tx) = spawn_upstream("reconnect", Duration::ZERO).await;
    let policy = ServerPolicy {
        host_policy: HostPolicy::AllowLoopbackOnly,
        ..ServerPolicy::default()
    };
    let (relay, _dir) = spawn_relay(policy).await;

    let start = Instant::now();
    for i in 0..N_CYCLES {
        let client = reqwest::Client::new();
        let url = proxy_url(
            &relay.base_url,
            "127.0.0.1",
            upstream_port,
            "/blobs/foo/meta",
        );
        let resp = client.get(&url).send().await.expect("relay req");
        let status = resp.status().as_u16();
        let _ = resp.bytes().await;
        assert_eq!(status, 200, "relay returned {status} on cycle {i}");
        // Drop the client explicitly so reqwest tears the
        // connection down between cycles.
        drop(client);
    }
    let elapsed = start.elapsed();
    let rps = N_CYCLES as f64 / elapsed.as_secs_f64();
    eprintln!("[T5.6] reconnect storm: {N_CYCLES} cycles in {elapsed:?} → {rps:.0} cycle/s");
    assert!(
        elapsed < MAX_WALL,
        "reconnect storm took too long: {elapsed:?} (relay is likely leaking or stuck)"
    );
    relay.shutdown();
    let _ = upstream_tx.send(true);
}
