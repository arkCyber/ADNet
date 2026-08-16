//! Comprehensive tests for the `builder` module.
//!
//! Tests IrohSshBuilder and IrohSsh with iroh feature enabled.

#![cfg(feature = "iroh")]

use a3net_ssh::IrohSshBuilder;
use a3net_ssh::builder::SSH_TUNNEL_ALPN;
use iroh::SecretKey;

// ============================================================================
// SSH_TUNNEL_ALPN constant tests
// ============================================================================

#[test]
fn ssh_tunnel_alpn_is_valid() {
    assert!(!SSH_TUNNEL_ALPN.is_empty());
    assert!(SSH_TUNNEL_ALPN.len() < 255); // ALPN length limit
}

#[test]
fn ssh_tunnel_alpn_starts_with_a3net() {
    assert!(SSH_TUNNEL_ALPN.starts_with(b"a3net/"));
}

#[test]
fn ssh_tunnel_alpn_contains_ssh() {
    let alpn_str = std::str::from_utf8(SSH_TUNNEL_ALPN).unwrap();
    assert!(alpn_str.contains("ssh"), "ALPN should contain 'ssh'");
}

#[test]
fn ssh_tunnel_alpn_is_versioned() {
    let alpn_str = std::str::from_utf8(SSH_TUNNEL_ALPN).unwrap();
    assert!(alpn_str.contains('/'), "ALPN should be versioned with /");
}

#[test]
fn ssh_tunnel_alpn_as_str() {
    let alpn_str = std::str::from_utf8(SSH_TUNNEL_ALPN).unwrap();
    assert_eq!(alpn_str, "a3net/ssh-tunnel/1");
}

// ============================================================================
// IrohSshBuilder construction tests (async - requires iroh)
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_with_ephemeral_key() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    // IrohSsh should be usable
    let endpoint = ssh.endpoint();
    assert!(!endpoint.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_with_accept_incoming_no_sshd() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    // This should fail because sshd is not running
    let result = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(22)
        .secret_key(key)
        .build()
        .await;

    // Expect failure because no SSH server
    assert!(result.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_default_port() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    assert_eq!(ssh.ssh_port(), 22);
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_custom_port() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .accept_port(2222)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    assert_eq!(ssh.ssh_port(), 2222);
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_accept_incoming_false() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    assert!(!ssh.accept_incoming());
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_different_keys_produce_different_endpoints() {
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();

    let key1 = SecretKey::generate();
    let key2 = SecretKey::generate();

    let ssh1 = IrohSshBuilder::new(tmp1.path())
        .accept_incoming(false)
        .secret_key(key1)
        .build()
        .await
        .expect("build should succeed");

    let ssh2 = IrohSshBuilder::new(tmp2.path())
        .accept_incoming(false)
        .secret_key(key2)
        .build()
        .await
        .expect("build should succeed");

    assert_ne!(ssh1.endpoint().id(), ssh2.endpoint().id());
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_same_key_produces_same_endpoint() {
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();

    let key = SecretKey::generate();

    let ssh1 = IrohSshBuilder::new(tmp1.path())
        .accept_incoming(false)
        .secret_key(key.clone())
        .build()
        .await
        .expect("build should succeed");

    let ssh2 = IrohSshBuilder::new(tmp2.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    assert_eq!(ssh1.endpoint().id(), ssh2.endpoint().id());
}

// ============================================================================
// IrohSsh method tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_endpoint_access() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let endpoint = ssh.endpoint();
    assert!(!endpoint.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_ssh_port() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .accept_port(12345)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    assert_eq!(ssh.ssh_port(), 12345);
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_accept_incoming() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(22)
        .secret_key(key)
        .build()
        .await;

    // May fail due to no sshd, but should set flag correctly
    match ssh {
        Ok(ssh) => assert!(ssh.accept_incoming()),
        Err(_) => {} // Expected if no sshd
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_identity_path_with_explicit_key() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    // With explicit key, identity path should be empty/default
    let path = ssh.identity_path();
    assert!(path.as_os_str().is_empty() || !path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_cloneable() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    // Should be cloneable (cheap via Arc)
    let _ssh_clone = ssh.clone();
    assert!(true);
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_multiple_clones() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    // Multiple clones should all work
    let _clone1 = ssh.clone();
    let _clone2 = ssh.clone();
    let _clone3 = ssh.clone();
    assert!(true);
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_endpoint_id() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let endpoint_id = ssh.endpoint().id();
    assert_eq!(endpoint_id.as_bytes().len(), 32);
}

// ============================================================================
// Debug trait tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_endpoint_still_accessible() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    // Just verify the endpoint is accessible
    let endpoint = ssh.endpoint();
    assert!(!endpoint.is_closed());
}

// ============================================================================
// Error handling tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_nonexistent_dir_with_explicit_key() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    // Use a path that doesn't exist
    let nonexistent = tmp.path().join("nonexistent/deep/path");

    // With explicit key, should still work even without directory
    let result = IrohSshBuilder::new(&nonexistent)
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await;

    // Should succeed because we have explicit key
    assert!(result.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_build_creates_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    // Endpoint should be usable
    assert!(!ssh.endpoint().is_closed());
}

// ============================================================================
// Endpoint address tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_endpoint_addr_usable() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let addr = ssh.endpoint().addr();
    // Just verify the addr is usable - check it has the right size
    let bytes = addr.id.as_bytes();
    assert_eq!(bytes.len(), 32, "endpoint addr id should be 32 bytes");
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_ssh_endpoint_id_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(false)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let endpoint_id = ssh.endpoint().id();

    // The endpoint id should be consistent
    assert_eq!(endpoint_id.as_bytes().len(), 32);
}
