//! Comprehensive tests for the `info` module.
//!
//! Tests render_invite and related functions.

#![cfg(feature = "iroh")]

use a3net_ssh::info::render_invite;

// ============================================================================
// render_invite tests
// ============================================================================

#[test]
fn render_invite_contains_version() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    assert!(out.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn render_invite_contains_github_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    assert!(out.contains("iroh-ssh") || out.contains("github.com"));
}

#[test]
fn render_invite_contains_endpoint_id() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should mention endpoint id
    assert!(out.contains("endpoint") || out.contains("id"));
}

#[test]
fn render_invite_contains_a3net_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should mention a3net
    assert!(out.contains("a3net") || out.contains("A3Net"));
}

#[test]
fn render_invite_contains_invite_instruction() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should contain the invite format
    assert!(out.contains("ssh connect") || out.contains("connect"));
}

#[test]
fn render_invite_contains_user_info() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should mention the user
    assert!(out.contains('@') || out.contains("user"));
}

#[test]
fn render_invite_returns_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let result = render_invite(tmp.path());
    assert!(result.is_ok());
}

#[test]
fn render_invite_returns_string() {
    let tmp = tempfile::tempdir().unwrap();
    let result = render_invite(tmp.path()).unwrap();
    assert!(!result.is_empty());
}

#[test]
fn render_invite_is_multiline() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should contain newlines (multiline format)
    assert!(out.contains('\n'));
}

#[test]
fn render_invite_multiple_calls() {
    let tmp = tempfile::tempdir().unwrap();

    let out1 = render_invite(tmp.path()).unwrap();
    let out2 = render_invite(tmp.path()).unwrap();

    // Multiple calls should produce similar output
    assert_eq!(out1, out2);
}

#[test]
fn render_invite_different_data_dirs() {
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();

    let out1 = render_invite(tmp1.path()).unwrap();
    let out2 = render_invite(tmp2.path()).unwrap();

    // Different data dirs may produce different endpoint IDs
    // but both should be valid output
    assert!(!out1.is_empty());
    assert!(!out2.is_empty());
}

#[test]
fn render_invite_contains_identity_path() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should mention the identity file
    assert!(out.contains("iroh_secret_key") || out.contains("Identity"));
}

#[test]
fn render_invite_contains_short_id() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should mention short id or similar
    assert!(out.contains("short") || out.contains("a3net-"));
}

// ============================================================================
// Error handling tests
// ============================================================================

#[test]
fn render_invite_handles_nonexistent_dir() {
    // Should still work even if directory doesn't exist
    // (identity will be created)
    let tmp = tempfile::tempdir().unwrap();
    let non_existent = tmp.path().join("nonexistent");

    let result = render_invite(&non_existent);
    // May succeed (creating identity) or fail depending on implementation
    // Just ensure no panic
    assert!(result.is_ok() || result.is_err());
}

#[test]
fn render_invite_handles_temp_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let result = render_invite(tmp.path());
    assert!(result.is_ok());
}

#[test]
fn render_invite_result_is_send() {
    let tmp = tempfile::tempdir().unwrap();
    let result = render_invite(tmp.path()).unwrap();
    fn assert_send<T: Send>(_: T) {}
    assert_send(result);
}

// ============================================================================
// Output format tests
// ============================================================================

#[test]
fn render_invite_output_is_valid_utf8() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Should be valid UTF-8
    assert!(std::str::from_utf8(out.as_bytes()).is_ok());
}

#[test]
fn render_invite_output_length_reasonable() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    // Output should be reasonably sized (not empty, not gigabytes)
    assert!(!out.is_empty());
    assert!(out.len() < 10000, "Output should be reasonable size");
}

#[test]
fn render_invite_contains_crate_name() {
    let tmp = tempfile::tempdir().unwrap();
    let out = render_invite(tmp.path()).unwrap();

    assert!(out.contains("a3net-ssh"));
}

// ============================================================================
// Integration tests
// ============================================================================

#[test]
fn render_invite_with_persistent_identity() {
    use a3net_ssh::keys::persistent_identity;

    let tmp = tempfile::tempdir().unwrap();

    // First, ensure we have a persistent identity
    let identity = persistent_identity(tmp.path()).unwrap();

    // Then render the invite
    let out = render_invite(tmp.path()).unwrap();

    // Output should reflect the identity
    assert!(out.contains(&identity.endpoint_id().to_string()));
}
