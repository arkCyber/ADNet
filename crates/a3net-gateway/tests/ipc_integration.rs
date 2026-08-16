//! End-to-end integration tests for the gateway internal IPC service.
//!
//! These tests:
//! - stand up a real `GatewayIpcService` bound to a Unix socket,
//! - drive it through `GatewayIpcClient`,
//! - exercise every method in the public surface (ping, version,
//!   stats, pin ops, cid ops, error paths).
//!
//! No HTTP listener is involved; the IPC server runs in isolation
//! so tests are fast and deterministic.

use std::sync::Arc;
use std::time::Duration;

use a3net_blobstore::BlobStore;
use a3net_gateway::{
    DagService, DhtService, GatewayIpcClient, GatewayIpcConfig, GatewayIpcService, IpnService,
    PinService, StatsService,
};

/// Build a fresh `GatewayIpcService` against a per-test tempdir.
async fn boot_service() -> (GatewayIpcService, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let blob_store = Arc::new(BlobStore::new(dir.path()).expect("blob store"));
    let dag_service = Arc::new(DagService::new(blob_store.clone()));
    let pin_service = Arc::new(PinService::new(blob_store.clone(), dir.path().to_path_buf()));
    let dht_service = Arc::new(DhtService::new("local".to_string(), vec![]));
    let ipns_service = Arc::new(IpnService::new(
        blob_store.clone(),
        dir.path().to_path_buf(),
        None,
    ));
    let stats_service = Arc::new(StatsService::new(
        blob_store.clone(),
        dir.path().to_string_lossy().to_string(),
        1024 * 1024 * 1024,
    ));
    let svc = GatewayIpcService::new(
        blob_store,
        dag_service,
        pin_service,
        dht_service,
        ipns_service,
        stats_service,
    );
    (svc, dir)
}

/// Start the IPC service on `<dir>/gateway.ipc.sock`, return the
/// service handle and a connected client. The caller is responsible
/// for dropping the handle (which removes the socket file).
async fn boot_with_handle() -> (
    tempfile::TempDir,
    a3net_gateway::GatewayIpcServiceHandle,
    GatewayIpcClient,
) {
    let (svc, dir) = boot_service().await;
    let cfg = GatewayIpcConfig {
        socket_path: dir.path().join("gateway.ipc.sock"),
        notification_capacity: 32,
    };
    let handle = svc.start(cfg.clone()).await.expect("start IPC server");
    // The listener task may take a moment to actually call `bind`,
    // so retry briefly if a connect fails.
    let client = GatewayIpcClient::connect(cfg.socket_path);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if client.ping().await.is_ok() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("IPC server never came online");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (dir, handle, client)
}

/// Write a small blob directly to the per-test blob store and
/// return its hex CID. Lets us exercise `cid.exists` / `cid.meta` /
/// `pin.add` without depending on a real IPFS importer.
fn put_sample_blob(blob_store: &BlobStore, payload: &[u8]) -> String {
    let (hash, _size) = blob_store.put_bytes_sync(payload).expect("put blob");
    hash.as_hex().to_string()
}

#[tokio::test]
async fn ipc_ping_returns_recent_ts() {
    let (_dir, handle, client) = boot_with_handle().await;
    let ts = client.ping().await.expect("ping");
    // Anything earlier than 2025-01-01 should be flagged — guards
    // against accidentally returning 0 or a stale clock.
    assert!(
        ts > 1_735_689_600_000,
        "ping returned suspicious ts: {ts}"
    );
    drop(handle);
}

#[tokio::test]
async fn ipc_version_returns_semver() {
    let (_dir, handle, client) = boot_with_handle().await;
    let v = client.version().await.expect("version");
    assert!(!v.is_empty(), "version should not be empty");
    assert!(
        v.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false),
        "version should start with a digit, got `{v}`"
    );
    drop(handle);
}

#[tokio::test]
async fn ipc_uptime_increases() {
    let (_dir, handle, client) = boot_with_handle().await;
    let a = client.uptime_secs().await.expect("uptime a");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let b = client.uptime_secs().await.expect("uptime b");
    assert!(
        b > a,
        "uptime must increase after sleeping 1.1s (a={a}, b={b})"
    );
    drop(handle);
}

#[tokio::test]
async fn ipc_cid_exists_handles_present_and_absent() {
    let (svc, dir) = boot_service().await;
    let blob_store = Arc::new(BlobStore::new(dir.path()).expect("blob store"));
    let cid = put_sample_blob(&blob_store, b"hello world");
    let cfg = GatewayIpcConfig {
        socket_path: dir.path().join("gateway.ipc.sock"),
        notification_capacity: 32,
    };
    let handle = svc.start(cfg.clone()).await.expect("start");
    let client = GatewayIpcClient::connect(cfg.socket_path);

    // wait for listener to be live
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while client.ping().await.is_err() {
        if std::time::Instant::now() >= deadline {
            panic!("server never came online");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(client.cid_exists(&cid).await.expect("exists"));
    assert!(!client.cid_exists("0000000000000000000000000000000000000000000000000000000000000000").await.expect("missing"));

    let (size, chunks, is_dir) = client.cid_meta(&cid).await.expect("meta");
    assert!(size > 0, "size should be > 0 for a stored blob, got {size}");
    assert_eq!(chunks, 1, "single-chunk blob has chunk_count=1, got {chunks}");
    assert!(!is_dir);

    drop(handle);
}

#[tokio::test]
async fn ipc_pin_lifecycle() {
    let (svc, dir) = boot_service().await;
    let blob_store = Arc::new(BlobStore::new(dir.path()).expect("blob store"));
    let cid = put_sample_blob(&blob_store, b"pin me");
    let cfg = GatewayIpcConfig {
        socket_path: dir.path().join("gateway.ipc.sock"),
        notification_capacity: 32,
    };
    let handle = svc.start(cfg.clone()).await.expect("start");
    let client = GatewayIpcClient::connect(cfg.socket_path);

    // wait for ready
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while client.ping().await.is_err() {
        if std::time::Instant::now() >= deadline {
            panic!("server never came online");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Initially no pins.
    let initial = client.pin_list().await.expect("pin_list");
    let arr = initial.as_array().expect("pin_list should be an array");
    assert!(arr.is_empty(), "expected empty pin list, got {arr:?}");

    // Add and re-list.
    client.pin_add(&cid, false).await.expect("pin add");
    let after_add = client.pin_list().await.expect("pin_list");
    let arr = after_add.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected exactly 1 pin after add");
    let entry = &arr[0];
    assert_eq!(entry["cid"].as_str(), Some(cid.as_str()));
    assert_eq!(entry["status"].as_str(), Some("pinned"));

    // Remove.
    client.pin_remove(&cid).await.expect("pin remove");
    let after_rm = client.pin_list().await.expect("pin_list");
    assert!(
        after_rm.as_array().expect("array").is_empty(),
        "pin should be gone after remove"
    );

    drop(handle);
}

#[tokio::test]
async fn ipc_pin_add_rejects_unknown_cid() {
    let (svc, dir) = boot_service().await;
    let cfg = GatewayIpcConfig {
        socket_path: dir.path().join("gateway.ipc.sock"),
        notification_capacity: 32,
    };
    let handle = svc.start(cfg.clone()).await.expect("start");
    let client = GatewayIpcClient::connect(cfg.socket_path);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while client.ping().await.is_err() {
        if std::time::Instant::now() >= deadline {
            panic!("server never came online");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A perfectly valid hex hash that is not in the blob store.
    let bogus = "abcdef0000000000000000000000000000000000000000000000000000000000";
    let err = client
        .pin_add(bogus, false)
        .await
        .expect_err("pin add should fail for unknown CID");
    let msg = err.to_string();
    assert!(
        msg.contains("not found") || msg.contains("content not found"),
        "expected not-found error, got: {msg}"
    );

    drop(handle);
}

#[tokio::test]
async fn ipc_unknown_method_returns_server_error() {
    let (_dir, handle, client) = boot_with_handle().await;
    let err = client
        .raw_call("definitely.not.a.method", serde_json::json!({}))
        .await
        .expect_err("unknown method should error");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown method"),
        "expected unknown-method error, got: {msg}"
    );
    drop(handle);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ipc_shutdown_broadcast_observable_via_stream() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (svc, dir) = boot_service().await;
    let cfg = GatewayIpcConfig {
        socket_path: dir.path().join("shutdown.sock"),
        notification_capacity: 32,
    };
    let handle = svc.start(cfg.clone()).await.expect("start");
    let path = handle.socket_path().to_path_buf();

    // Open a single long-lived socket and drive a priming request
    // through it. After the priming request is read, the server's
    // per-connection forwarder task is guaranteed subscribed.
    let sock = tokio::net::UnixStream::connect(&path).await.expect("connect");
    let (read_half, mut writer) = sock.into_split();
    writer
        .write_all(
            br#"{"jsonrpc":"2.0","method":"gateway.ping","params":{},"id":1}\n"#,
        )
        .await
        .expect("write priming");
    writer.flush().await.expect("flush");
    let mut br = BufReader::new(read_half);
    let mut line = String::new();
    br.read_line(&mut line)
        .await
        .expect("read priming response");
    assert!(line.contains("\"result\""), "got: {line}");

    // Now fire the broadcast. The forwarder on our connection
    // receives it and writes to the socket — we read until we see a
    // notification frame (no `id`).
    let notifier = handle.notifier();
    let sent = notifier.send("gateway.shutdown", serde_json::json!({}));
    assert!(sent >= 1, "expected at least one subscriber, got {sent}");

    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(2), br.read_line(&mut line))
        .await
        .expect("timed out waiting for notification")
        .expect("read failed");
    assert!(n > 0, "got empty line");
    let v: serde_json::Value = serde_json::from_str(&line).expect("parse");
    assert_eq!(v["method"], "gateway.shutdown");
    assert!(v.get("id").is_none(), "notification must have no `id`");

    drop(handle);
    drop(svc);
}

#[tokio::test]
async fn ipc_handle_drop_removes_socket_file() {
    let (svc, dir) = boot_service().await;
    let socket_path = dir.path().join("drop.sock");
    let cfg = GatewayIpcConfig {
        socket_path: socket_path.clone(),
        notification_capacity: 32,
    };
    let handle = svc.start(cfg).await.expect("start");
    assert!(socket_path.exists(), "socket file should exist after start");

    drop(handle);

    // Give the listener task a moment to observe shutdown and clean
    // up the socket file.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while socket_path.exists() {
        if std::time::Instant::now() >= deadline {
            panic!(
                "socket file {} still present after drop",
                socket_path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn ipc_serializes_concurrent_requests() {
    let (_dir, handle, client) = boot_with_handle().await;

    // 32 concurrent ping requests — verifies the writer mutex
    // serialises correctly and no frame is interleaved.
    let mut joins = Vec::new();
    for _ in 0..32 {
        let c = client.clone();
        joins.push(tokio::spawn(async move { c.ping().await }));
    }
    for j in joins {
        j.await.expect("join").expect("ping");
    }

    drop(handle);
}

#[tokio::test]
async fn ipc_records_repo_and_bandwidth_objects() {
    let (svc, dir) = boot_service().await;
    let blob_store = Arc::new(BlobStore::new(dir.path()).expect("blob store"));
    put_sample_blob(&blob_store, b"a");
    put_sample_blob(&blob_store, b"bb");
    put_sample_blob(&blob_store, b"ccc");

    let cfg = GatewayIpcConfig {
        socket_path: dir.path().join("gateway.ipc.sock"),
        notification_capacity: 32,
    };
    let handle = svc.start(cfg.clone()).await.expect("start");
    let client = GatewayIpcClient::connect(cfg.socket_path);

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while client.ping().await.is_err() {
        if std::time::Instant::now() >= deadline {
            panic!("server never came online");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Repo stats — NumObjects must reflect the 3 inserted blobs.
    let v = client
        .raw_call("gateway.stats.repo", serde_json::json!({}))
        .await
        .expect("repo stats");
    let n = v["NumObjects"].as_u64().unwrap_or(0);
    assert!(
        n >= 3,
        "expected NumObjects >= 3, got {n}; raw={v}"
    );

    // Bandwidth — keys present even when zero (parity with Kubo).
    let v = client
        .raw_call("gateway.stats.bandwidth", serde_json::json!({}))
        .await
        .expect("bandwidth stats");
    assert!(v["TotalIn"].is_number(), "TotalIn missing: {v}");
    assert!(v["TotalOut"].is_number(), "TotalOut missing: {v}");
    assert!(v["RateIn"].is_number(), "RateIn missing: {v}");
    assert!(v["RateOut"].is_number(), "RateOut missing: {v}");

    // DHT — at least the name key.
    let v = client
        .raw_call("gateway.stats.dht", serde_json::json!({}))
        .await
        .expect("dht stats");
    assert!(v["Name"].is_string(), "Name missing: {v}");

    drop(handle);
}

#[tokio::test]
async fn ipc_supports_rebind_after_drop() {
    // Bind, drop, bind again on the same path. Verifies the server's
    // Drop impl removes the stale socket file (otherwise the second
    // `bind` would fail with EADDRINUSE).
    let (svc, dir) = boot_service().await;
    let cfg = GatewayIpcConfig {
        socket_path: dir.path().join("rebind.sock"),
        notification_capacity: 8,
    };
    let h1 = svc.start(cfg.clone()).await.expect("first bind");
    assert!(h1.socket_path().exists());
    drop(h1);

    // Brief settle.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let h2 = svc.start(cfg.clone()).await.expect("rebind");
    assert!(h2.socket_path().exists());
    drop(h2);
}