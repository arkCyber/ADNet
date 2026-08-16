//! Smoke tests for the webtransport config and connect-token paths that
//! don't require the `webtransport` feature.

#[test]
fn config_defaults_are_sane() {
    let cfg = crate::WebTransportConfig::default();
    assert_eq!(cfg.token_ttl_seconds, 60);
    assert!(cfg.ephemeral_cert);
}
