//! Comprehensive tests for the `server` module.
//!
//! Tests Server, probe_local_ssh, and related functions.

#![cfg(feature = "iroh")]

use a3net_ssh::builder::IrohSshBuilder;
use a3net_ssh::server::{probe_local_ssh, Server};
use std::time::Duration;
use tokio::net::TcpListener;

// ============================================================================
// probe_local_ssh tests
// ============================================================================

#[tokio::test]
async fn probe_local_ssh_timeout_on_unbound_port() {
    // Bind a port and immediately drop it
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    // Small delay to ensure port is released
    tokio::time::sleep(Duration::from_millis(10)).await;

    let result = probe_local_ssh(port).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn probe_local_ssh_timeout_on_high_port() {
    // Try a very high port number that's unlikely to be in use
    let result = probe_local_ssh(65535).await;

    // May timeout or get connection refused - both are errors
    assert!(result.is_err());
}

#[tokio::test]
async fn probe_local_ssh_handles_invalid_port() {
    // Port 0 is invalid
    let result = probe_local_ssh(0).await;
    // Should error
    assert!(result.is_err());
}

// ============================================================================
// Server construction tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn server_start_creates_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    // Start a fake SSH server
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Spawn a task that accepts and immediately closes connections
    tokio::spawn(async move {
        loop {
            if let Ok((_socket, _)) = listener.accept().await {
                // Just accept and drop
            }
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await;
    assert!(server.is_ok());
}

// ============================================================================
// Server methods tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn server_endpoint_id() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key.clone())
        .build()
        .await
        .expect("build should succeed");

    let expected_id = ssh.endpoint().id();

    let server = Server::start(ssh).await.expect("server should start");

    assert_eq!(server.endpoint_id(), expected_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn server_node_id() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    let node_id = server.node_id();
    // NodeId should be 32 bytes
    assert_eq!(node_id.as_bytes().len(), 32);
}

#[tokio::test(flavor = "multi_thread")]
async fn server_ssh_port() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    assert_eq!(server.ssh_port(), port);
}

// ============================================================================
// Server debug trait tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn server_debug() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    let debug_str = format!("{:?}", server);
    assert!(debug_str.contains("Server") || debug_str.contains("endpoint_id"));
}

// ============================================================================
// Server shutdown tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn server_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    // Shutdown should not panic
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_double_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    // Multiple shutdowns should not panic
    server.shutdown().await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_shutdown_drops_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    server.shutdown().await;

    // Drop tempdir while server is shutting down
    drop(tmp);
}

// ============================================================================
// Server clone tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
async fn server_cloneable() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    // Server should be cloneable
    let _server_clone = server.clone();
    assert!(true);
}

#[tokio::test(flavor = "multi_thread")]
async fn server_multiple_clones() {
    let tmp = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let _ = listener.accept().await;
        }
    });

    let ssh = IrohSshBuilder::new(tmp.path())
        .accept_incoming(true)
        .accept_port(port)
        .secret_key(key)
        .build()
        .await
        .expect("build should succeed");

    let server = Server::start(ssh).await.expect("server should start");

    // Multiple clones should work
    let _clone1 = server.clone();
    let _clone2 = server.clone();
    let _clone3 = server.clone();

    assert!(true);
}
