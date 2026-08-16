//! Smoke tests for the webrtc config and error paths that don't require
//! the `webrtc` feature.

#[test]
fn error_is_fatal_classification_stable() {
    let e = crate::WebRtcError::PeerClosed;
    assert!(e.is_fatal());
}

#[test]
fn config_default_has_stun_servers() {
    let cfg = crate::WebRtcConfig::default();
    assert!(!cfg.stun.is_empty());
}
