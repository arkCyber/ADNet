//! End-to-end integration tests for the embedded DERP server.
//!
//! These tests pin three contracts that operators depend on:
//!
//! 1. **Wire compatibility**: the embedded [`DerpServer`] uses
//!    the same `iroh_relay::server::Server` instance that
//!    `iroh::test_utils::run_relay_server` does, so its
//!    wire-format is identical. The unit tests in
//!    `crates/adnet-relay/src/derp/` cover construction; this
//!    test file focuses on the operator-facing surface
//!    ([`DerpConfig`]).
//!
//! 2. **Config-shape regression**: [`DerpConfig`] persists and
//!    re-reads cleanly via JSON. Snake/camel-case wiring is
//!    verified by round-trip.
//!
//! 3. **test_utils sanity**: confirms that the iroh upstream
//!    test helper still produces a well-formed HTTPS relay URL
//!    on loopback, which is a precondition for the upstream
//!    iroh client stack to actually connect to it.
//!
//! All tests are feature-gated on `derp`.

#![cfg(feature = "derp")]

/// **A4 — config-shape regression**: [`DerpConfig`] persists and
/// re-reads cleanly across JSON serialisation. Catches
/// accidental renames of fields or `#[serde(rename_all)]`
/// regressions early.
#[test]
fn derp_config_persists_round_trip() {
    use adnet_relay::derp::{AccessConfig, DerpConfig, DerpManualCert, DerpTlsConfig};
    let cfg = DerpConfig {
        http_bind_addr: "127.0.0.1:8765".parse().unwrap(),
        tls: Some(DerpTlsConfig {
            https_bind_addr: Some("127.0.0.1:4443".parse().unwrap()),
            manual: Some(DerpManualCert {
                cert_path: "/tmp/cert.pem".into(),
                key_path: "/tmp/key.pem".into(),
            }),
            lets_encrypt: None,
        }),
        quic: None,
        access: AccessConfig::Everyone,
        rate_limits: None,
        metrics_bind_addr: None,
        key_cache_capacity: None,
    };
    let json = serde_json::to_string(&cfg).expect("serialise");
    assert!(json.contains("httpBindAddr"), "camelCase failed: {json}");
    let back: DerpConfig = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, cfg);
}

/// **`DerpServer` is constructible, signal-able, and shuts
/// down cleanly** — exercises the public lifecycle contract:
///
/// 1. `spawn` returns a running server bound to a kernel-
///    assigned port.
/// 2. `handle.request_shutdown()` is idempotent (calling it
///    multiple times is a no-op).
/// 3. `server.shutdown().await` consumes the server, awaits the
///    background task, and returns `Ok(())`.
///
/// This regression-tests the audit-fix that replaced a
/// swallow-the-result `shutdown()` with a proper join handle,
/// so a future regression that drops the join would surface as
/// a compile error rather than a silent detach.
#[tokio::test]
async fn derp_server_lifecycle_smoke() {
    use adnet_relay::derp::{AccessConfig, DerpConfig, DerpServer};

    let cfg = DerpConfig {
        http_bind_addr: "127.0.0.1:0".parse().unwrap(),
        tls: None,
        quic: None,
        access: AccessConfig::Everyone,
        rate_limits: None,
        metrics_bind_addr: None,
        key_cache_capacity: None,
    };
    let server = DerpServer::spawn(cfg).await.expect("spawn");
    let handle = server.handle();
    // Idempotent: two calls in a row must not panic.
    handle.request_shutdown();
    handle.request_shutdown();
    // Consuming shutdown joins the background task and returns
    // the upstream result.
    server.shutdown().await.expect("graceful shutdown");
}

/// Sanity check: `iroh::test_utils::run_relay_server` returns a
/// well-formed HTTPS `RelayUrl` on loopback. If this fails,
/// the rest of the workspace's integration tests that rely on
/// it will also fail — so we pin it here explicitly to keep
/// the regression visible.
#[tokio::test]
async fn run_relay_server_returns_well_formed_https_url() {
    use iroh::test_utils::run_relay_server;
    let (_map, relay_url, _server) = run_relay_server()
        .await
        .expect("test_utils: relay server should spawn");
    let s = relay_url.to_string();
    assert!(
        s.starts_with("https://"),
        "test_utils relay is HTTPS; got {s}"
    );
    assert!(
        s.contains("127.0.0.1"),
        "test_utils relay binds loopback; got {s}"
    );
}
