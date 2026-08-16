//! Smoke tests for the public-API surface of `a3net-ssh`.
//!
//! These tests intentionally avoid binding an iroh endpoint —
//! they exist so the default (no-`iroh`) build path stays
//! exercised by CI even when no relay / DERP is reachable.

use a3net_ssh::error::{SshError, SshResult};

#[test]
fn error_display_includes_context() {
    let err = SshError::NoSshServer { port: 22 };
    let msg = err.to_string();
    assert!(msg.contains("22"), "error message must mention the port: {msg}");
    assert!(msg.contains("SSH server"), "error message must mention SSH");
}

#[test]
fn result_alias_compiles() {
    // Just make sure the alias resolves and the round-trip
    // `Result<T, SshError>` shape is what callers expect.
    fn returns_ok() -> SshResult<u32> {
        Ok(7)
    }
    assert_eq!(returns_ok().unwrap(), 7);
}

#[cfg(not(feature = "iroh"))]
#[test]
fn feature_missing_error_when_iroh_disabled() {
    // `render_invite` exists even without iroh; it just prints a
    // degraded banner so docs / minimal builds still compile.
    let dir = tempfile::tempdir().unwrap();
    let out = a3net_ssh::info::render_invite(dir.path()).unwrap();
    assert!(
        out.contains("built without `iroh` feature"),
        "non-iroh invite should explain the feature gap: {out}"
    );
}

#[test]
fn invalid_invite_error_carries_input() {
    let err = SshError::InvalidInvite {
        input: "aliceendpoint".into(),
        source: "missing `@` separator".into(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("aliceendpoint"),
        "InvalidInvite display must echo the original input: {msg}"
    );
}

#[test]
fn feature_missing_error_display_is_stable() {
    // The exact wording is consumed by docs / ops tooling — guard
    // against accidental rewording by pinning the substring.
    let msg = SshError::FeatureMissing.to_string();
    assert!(
        msg.contains("--features iroh"),
        "FeatureMissing error must mention how to opt in: {msg}"
    );
}

#[test]
fn identity_error_carries_path() {
    // Source is a plain string (Box<dyn Error>); we only check the
    // path ends up in the rendered message.
    let path = "/tmp/iroh_secret_key".to_string();
    let err = SshError::Identity {
        path: path.clone(),
        source: "permission denied".into(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains(&path),
        "Identity error must mention the path: {msg}"
    );
    assert!(
        msg.contains("permission denied"),
        "Identity error must include the source: {msg}"
    );
}

#[test]
fn spawn_ssh_error_includes_binary_name() {
    let err = SshError::SpawnSsh {
        binary: "/usr/bin/ssh".into(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("/usr/bin/ssh"),
        "SpawnSsh display must mention the binary path: {msg}"
    );
}

#[test]
fn tunnel_error_passthrough() {
    let err = SshError::Tunnel("quic stream reset".into());
    let msg = err.to_string();
    assert!(
        msg.contains("quic stream reset"),
        "Tunnel error must wrap the inner message: {msg}"
    );
}

#[test]
fn resolve_data_dir_uses_cli_value_when_set() {
    let resolved = a3net_ssh::keys::resolve_data_dir(Some("/tmp/ssh-test"));
    assert_eq!(resolved.to_str().unwrap(), "/tmp/ssh-test");
}

#[test]
fn resolve_data_dir_falls_back_to_default_when_none() {
    let resolved = a3net_ssh::keys::resolve_data_dir(None);
    assert_eq!(
        resolved.to_str().unwrap(),
        "./.a3net-data",
        "default data dir must match a3net-cli's default"
    );
}

#[test]
fn resolve_data_dir_falls_back_to_default_when_empty() {
    let resolved = a3net_ssh::keys::resolve_data_dir(Some(""));
    assert_eq!(resolved.to_str().unwrap(), "./.a3net-data");
}

#[test]
fn ssh_tunnel_alpn_is_namespace_scoped() {
    // Bumping this string is a wire-format break; lock it in.
    assert_eq!(
        a3net_ssh::builder::SSH_TUNNEL_ALPN,
        b"a3net/ssh-tunnel/1",
        "ALPN must stay namespaced under a3net/ and version-pinned"
    );
}

#[cfg(not(feature = "iroh"))]
#[tokio::test]
async fn stub_builder_errors_when_iroh_disabled() {
    // The no-feature path must error cleanly, never panic.
    let dir = tempfile::tempdir().unwrap();
    let res = a3net_ssh::IrohSshBuilder::new(dir.path())
        .accept_incoming(true)
        .accept_port(22)
        .build()
        .await;
    assert!(matches!(res, Err(SshError::FeatureMissing)));
}

#[tokio::test]
async fn probe_local_ssh_fails_on_unbound_port() {
    // Bind a TCP listener to port 0 to get a kernel-allocated
    // port, then immediately drop it. The port is now very
    // likely free (modulo a fast rebinding, which we tolerate
    // by accepting both `NoSshServer` and `Other` outcomes).
    // This avoids the pattern of hard-coding 19999 — which was
    // a CI-flake risk and bound us to a specific environment.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let res = a3net_ssh::server::probe_local_ssh(port).await;
    match res {
        Err(SshError::NoSshServer { port: p }) => assert_eq!(p, port),
        Err(SshError::Other(_)) => {
            // Kernel returned connection-refused fast — also
            // acceptable. The point is "no sshd there".
        }
        Ok(()) => panic!(
            "probe_local_ssh on an unbound port must not succeed"
        ),
        Err(other) => panic!("unexpected error: {other}"),
    }
}

#[test]
fn metrics_init_does_not_panic() {
    // Forcing the lazy registration is the smoke test: if the
    // counter constructor or the registry wiring breaks, this
    // panics. The fact that we can read the values back via
    // `inc()` / `value()` is also implicit.
    a3net_ssh::metrics::init();
    a3net_ssh::metrics::TUNNEL_CONNECTIONS_ACCEPTED.inc();
    a3net_ssh::metrics::TUNNEL_CONNECTIONS_FAILED.inc();
    a3net_ssh::metrics::CLIENT_BRIDGES_STARTED.inc();
    a3net_ssh::metrics::CLIENT_BRIDGES_COMPLETED.inc();
}

#[test]
fn metrics_are_shared_across_calls() {
    // Two reads of the same static must hit the same `Arc<Counter>`,
    // so increments from one caller must be visible to the next.
    // This is the contract we rely on for the Prometheus exporter.
    // We don't expose a `value()` accessor on `Counter`, so we
    // assert on the Prometheus text output instead.
    use a3net_observability::metrics::Metric;
    let before = a3net_ssh::metrics::CLIENT_BRIDGES_STARTED.render_prometheus();
    a3net_ssh::metrics::CLIENT_BRIDGES_STARTED.inc();
    let after = a3net_ssh::metrics::CLIENT_BRIDGES_STARTED.render_prometheus();
    assert_ne!(
        before, after,
        "render_prometheus output must change after an inc"
    );
}

#[test]
fn in_flight_gauge_is_balanced() {
    // Smoke test for gap §5: the gauge must accept both inc()
    // and dec() without panicking, and the net effect of an
    // inc/dec pair must be observable. We don't assert on the
    // exact final value because the metric is process-global
    // and other tests may have touched it.
    a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.inc();
    a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.inc();
    a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.dec();
    let mid = a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.get();
    a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.dec();
    let after = a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.get();
    assert_eq!(
        mid - after,
        1,
        "dec() must decrement the gauge by exactly 1 each time"
    );
}
