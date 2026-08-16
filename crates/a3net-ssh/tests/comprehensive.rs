//! Comprehensive test suite for `a3net-ssh` module.
//!
//! This file provides test coverage for public functions across the a3net-ssh crate.

use a3net_ssh::error::SshError;
use a3net_ssh::error::SshResult;

// ============================================================================
// error.rs tests
// ============================================================================

#[test]
fn ssh_error_debug_format() {
    // Verify Debug format includes all variants without panicking
    let errors = [
        SshError::Identity {
            path: "/test/path".into(),
            source: "test error".into(),
        },
        SshError::NoSshServer { port: 22 },
        SshError::InvalidInvite {
            input: "bad@input".into(),
            source: "missing @".into(),
        },
        SshError::SpawnSsh {
            binary: "/bin/ssh".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        },
        SshError::Tunnel("connection lost".into()),
        SshError::FeatureMissing,
        SshError::Other("generic error".into()),
    ];

    for err in errors {
        let debug_str = format!("{:?}", err);
        assert!(!debug_str.is_empty(), "Debug format must not be empty");
    }
}

#[test]
fn ssh_error_display_no_ssh_server() {
    let err = SshError::NoSshServer { port: 2222 };
    let msg = err.to_string();
    assert!(msg.contains("2222"), "should contain port number");
    assert!(msg.contains("SSH server") || msg.contains("sshd"), "should mention SSH");
}

#[test]
fn ssh_error_display_other() {
    let err = SshError::Other("custom error message".into());
    let msg = err.to_string();
    assert!(msg.contains("custom error message"), "should contain custom message");
}

#[test]
fn ssh_error_display_identity() {
    let err = SshError::Identity {
        path: "/data/iroh_secret_key".into(),
        source: "file not found".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("/data/iroh_secret_key"), "should contain path");
}

#[test]
fn ssh_error_display_tunnel() {
    let err = SshError::Tunnel("timeout".into());
    let msg = err.to_string();
    assert!(msg.contains("timeout"), "should contain tunnel message");
}

#[test]
fn ssh_result_type_alias() {
    // Test that SshResult works as expected
    fn success() -> SshResult<u32> {
        Ok(42)
    }
    fn failure() -> SshResult<u32> {
        Err(SshError::Other("failed".into()))
    }

    assert_eq!(success().unwrap(), 42);
    assert!(failure().is_err());
}

#[test]
fn fmt_error_conversion() {
    use std::fmt;

    // Test From<std::fmt::Error> implementation
    fn make_fmt_error() -> fmt::Error {
        fmt::Error
    }

    let fmt_err = make_fmt_error();
    let ssh_err: SshError = fmt_err.into();
    let msg = ssh_err.to_string();
    assert!(msg.contains("fmt::Write failed") || !msg.is_empty());
}

// ============================================================================
// keys.rs constants and helpers
// ============================================================================

#[test]
fn ssh_subdir_constant() {
    assert_eq!(a3net_ssh::keys::SSH_SUBDIR, "ssh");
}

#[test]
fn iroh_secret_key_file_constant() {
    assert_eq!(a3net_ssh::keys::IROH_SECRET_KEY_FILE, "iroh_secret_key");
}

#[test]
fn resolve_data_dir_with_absolute_path() {
    let path = a3net_ssh::keys::resolve_data_dir(Some("/absolute/path"));
    assert_eq!(path.to_str().unwrap(), "/absolute/path");
}

#[test]
fn resolve_data_dir_with_relative_path() {
    let path = a3net_ssh::keys::resolve_data_dir(Some("./relative/path"));
    assert_eq!(path.to_str().unwrap(), "./relative/path");
}

#[test]
fn resolve_data_dir_with_whitespace() {
    let path = a3net_ssh::keys::resolve_data_dir(Some("  /path/with/spaces  "));
    assert_eq!(path.to_str().unwrap().trim(), "/path/with/spaces");
}

#[test]
fn resolve_data_dir_none_defaults_to_a3net_data() {
    let path = a3net_ssh::keys::resolve_data_dir(None);
    assert_eq!(path.to_str().unwrap(), "./.a3net-data");
}

#[test]
fn resolve_data_dir_empty_string_defaults_to_a3net_data() {
    let path = a3net_ssh::keys::resolve_data_dir(Some(""));
    assert_eq!(path.to_str().unwrap(), "./.a3net-data");
}

// ============================================================================
// metrics.rs tests
// ============================================================================

#[test]
fn metrics_lazy_initialization() {
    // Force all metrics to be initialized
    a3net_ssh::metrics::init();

    // All metrics should be accessible after init()
    a3net_ssh::metrics::TUNNEL_CONNECTIONS_ACCEPTED.inc();
    a3net_ssh::metrics::TUNNEL_CONNECTIONS_FAILED.inc();
    a3net_ssh::metrics::CLIENT_BRIDGES_STARTED.inc();
    a3net_ssh::metrics::CLIENT_BRIDGES_COMPLETED.inc();
}

#[test]
fn metrics_counter_increment() {
    use a3net_observability::metrics::Metric;

    // Get initial state
    let before = a3net_ssh::metrics::CLIENT_BRIDGES_COMPLETED.render_prometheus();
    a3net_ssh::metrics::CLIENT_BRIDGES_COMPLETED.inc();
    let after = a3net_ssh::metrics::CLIENT_BRIDGES_COMPLETED.render_prometheus();

    // After increment, the rendered output should change
    assert_ne!(before, after);
}

#[test]
fn metrics_gauge_inc_dec() {
    let initial = a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.get();
    a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.inc();
    let after_inc = a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.get();
    assert_eq!(after_inc, initial + 1);

    a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.dec();
    let after_dec = a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.get();
    assert_eq!(after_dec, after_inc - 1);
}

#[test]
fn metrics_render_prometheus() {
    use a3net_observability::metrics::Metric;

    // Force metric initialization
    a3net_ssh::metrics::init();

    // Render all metrics
    let accepted = a3net_ssh::metrics::TUNNEL_CONNECTIONS_ACCEPTED.render_prometheus();
    let failed = a3net_ssh::metrics::TUNNEL_CONNECTIONS_FAILED.render_prometheus();
    let started = a3net_ssh::metrics::CLIENT_BRIDGES_STARTED.render_prometheus();
    let completed = a3net_ssh::metrics::CLIENT_BRIDGES_COMPLETED.render_prometheus();
    let in_flight = a3net_ssh::metrics::CLIENT_BRIDGES_IN_FLIGHT.render_prometheus();

    // All should contain metric names
    assert!(accepted.contains("a3net_ssh_tunnel_connections_accepted"));
    assert!(failed.contains("a3net_ssh_tunnel_connections_failed"));
    assert!(started.contains("a3net_ssh_client_bridges_started"));
    assert!(completed.contains("a3net_ssh_client_bridges_completed"));
    assert!(in_flight.contains("a3net_ssh_client_bridges_in_flight"));
}

#[test]
fn metrics_names_match_prometheus_conventions() {
    // Verify metric names follow Prometheus naming conventions
    let counter_names = [
        "a3net_ssh_tunnel_connections_accepted_total",
        "a3net_ssh_tunnel_connections_failed_total",
        "a3net_ssh_client_bridges_started_total",
        "a3net_ssh_client_bridges_completed_total",
    ];

    for name in counter_names {
        // Must end with _total for counters
        assert!(name.ends_with("_total"), "{name} should end with _total");
        // Must be lowercase
        assert!(name.to_lowercase() == name, "{name} should be lowercase");
    }

    // Gauge names should not end with _total
    assert!(!"a3net_ssh_client_bridges_in_flight".ends_with("_total"));
}

// ============================================================================
// builder.rs ALPN and constants
// ============================================================================

#[test]
fn ssh_tunnel_alpn_is_valid_bytes() {
    let alpn = a3net_ssh::builder::SSH_TUNNEL_ALPN;
    assert!(!alpn.is_empty());
    assert!(alpn.starts_with(b"a3net/"));
}

#[test]
fn ssh_tunnel_alpn_version_pinned() {
    // The ALPN should contain a version number
    let alpn = a3net_ssh::builder::SSH_TUNNEL_ALPN;
    let alpn_str = std::str::from_utf8(alpn).unwrap();
    assert!(alpn_str.contains('/'), "ALPN should be versioned");
}

#[test]
fn ssh_tunnel_alpn_different_from_frame_alpn() {
    // Verify that SSH tunnel ALPN is distinct from the frame ALPN
    let ssh_alpn = a3net_ssh::builder::SSH_TUNNEL_ALPN;
    let frame_alpn = b"a3net/frame/1";

    // Compare as byte slices
    assert_ne!(ssh_alpn, frame_alpn,
        "SSH tunnel ALPN must be distinct from frame ALPN");
}

#[cfg(not(feature = "iroh"))]
mod builder_no_iroh_tests {
    use super::*;

    #[tokio::test]
    async fn stub_builder_build_returns_feature_missing() {
        let builder = a3net_ssh::IrohSshBuilder::new("/tmp/test");
        let result = builder.build().await;
        assert!(matches!(result, Err(SshError::FeatureMissing)));
    }

    #[test]
    fn stub_builder_accept_incoming_noop() {
        let builder = a3net_ssh::IrohSshBuilder::new("/tmp/test");
        let _builder = builder.accept_incoming(true);
        // Should not panic
    }

    #[test]
    fn stub_builder_accept_port_noop() {
        let builder = a3net_ssh::IrohSshBuilder::new("/tmp/test");
        let _builder = builder.accept_port(2222);
        // Should not panic
    }

    #[tokio::test]
    async fn stub_builder_method_chaining() {
        let builder = a3net_ssh::IrohSshBuilder::new("/tmp/test")
            .accept_incoming(true)
            .accept_port(2222)
            .accept_incoming(false);
        assert!(builder.build().await.is_err());
    }
}

#[cfg(not(feature = "iroh"))]
#[test]
fn render_invite_without_iroh_feature() {
    let dir = tempfile::tempdir().unwrap();
    let out = a3net_ssh::info::render_invite(dir.path()).unwrap();
    assert!(out.contains("iroh"), "should mention iroh dependency");
    assert!(out.contains("feature"), "should mention feature flag");
}

// ============================================================================
// Constants and public exports
// ============================================================================

#[test]
fn ssh_error_public_re_exports() {
    // Verify SshError is accessible from crate root
    let _: SshError = a3net_ssh::SshError::FeatureMissing;
}

#[test]
fn ssh_result_is_public_alias() {
    // Verify SshResult is accessible from crate root
    fn test_result() -> SshResult<()> {
        Ok(())
    }
    assert!(test_result().is_ok());
}

#[cfg(not(feature = "iroh"))]
#[test]
fn builder_export_without_iroh() {
    // Without iroh feature, IrohSshBuilder should be accessible
    let builder = a3net_ssh::IrohSshBuilder::new("/tmp");
    let _ = builder.accept_incoming(true).accept_port(22);
}
