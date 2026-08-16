//! End-to-end integration test for the SSH tunnel.
//!
//! Boots two iroh endpoints on the same machine (loopback), wires
//! the server-side tunnel onto one and the client-side
//! `connect()` onto the other, and verifies that bytes flowing
//! over a QUIC stream land on the local TCP socket the server
//! has listening. We use a fake `sshd` (a `tokio::net::TcpListener`
//! that echos back whatever it receives) instead of a real sshd
//! because:
//
//! - The crate's job is to forward bytes, not to speak the SSH
//!   wire protocol.
//! - The fake sshd makes the test hermetic — no
//!   `~/.ssh/known_hosts`, no dh params, no `sshd` binary on the
//!   test machine.
//!
//! The test is gated on the `iroh` feature so the default
//! (no-iroh) build stays green.

#![cfg(feature = "iroh")]

use std::sync::Arc;

use a3net_ssh::builder::IrohSshBuilder;
use a3net_ssh::client;
use a3net_ssh::server::Server;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Spin up a fake sshd that echoes back whatever it reads.
/// Returns the bound listener so the test can read its own
/// echo.
async fn fake_sshd() -> std::io::Result<(u16, Arc<Mutex<Vec<u8>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received);
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let received = Arc::clone(&received_clone);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            received.lock().await.extend_from_slice(&buf[..n]);
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });
    Ok((port, received))
}

/// Walk the full pipeline: dial server endpoint, open a
/// bi-stream, send a payload, and verify the fake sshd echoes
/// it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_tunnel_round_trip() {
    // 1. Start a fake sshd on a kernel-allocated port.
    let (sshd_port, sshd_received) = fake_sshd().await.expect("fake sshd");

    // 2. Build the server-side tunnel. We keep a clone of
    //    `IrohSsh` so we can grab the server endpoint's full
    //    `EndpointAddr` after `Server::start` consumes the
    //    original.
    let server_dir = tempfile::tempdir().expect("server tempdir");
    let server_ssh = IrohSshBuilder::new(server_dir.path())
        .accept_incoming(true)
        .accept_port(sshd_port)
        .build()
        .await
        .expect("server builder");
    let server_for_addr = server_ssh.clone();
    let server_handle = Server::start(server_ssh).await.expect("server start");

    // 3. Build the client-side endpoint. Pass the server's
    //    endpoint id directly (no relay / DERP involved) so the
    //    test is hermetic.
    let client_dir = tempfile::tempdir().expect("client tempdir");
    let client_ssh = IrohSshBuilder::new(client_dir.path())
        .accept_incoming(false)
        .accept_port(22)
        .secret_key(iroh::SecretKey::generate())
        .build()
        .await
        .expect("client builder");

    // 4. Connect to the server and shuttle a payload. We use
    //    `connect_with_addr` with the server's full
    //    `EndpointAddr` so the test is hermetic — no relay /
    //    DERP / discovery layer required.
    let server_addr = server_for_addr.endpoint().addr();
    let (mut client_send, mut client_recv) =
        client::connect_with_addr(client_ssh.endpoint(), server_addr)
            .await
            .expect("client connect");

    let payload = b"hello a3net-ssh tunnel";

    // Run the read on a separate task so the test orchestrates
    // write → finish → read on three separate futures; that
    // mirrors how a real `ssh` client would use the tunnel
    // (where the read happens on a different task than the
    // write) and avoids a fake-sshd-dependent ordering
    // problem in the half-close path.
    let read_handle = tokio::spawn(async move {
        client_recv.read_to_end(64 * 1024).await
    });

    client_send.write_all(payload).await.expect("client send");
    client_send.finish().ok();

    // 5. Read the echo back via the client stream.
    let echoed = read_handle
        .await
        .expect("join read task")
        .expect("client recv");
    assert_eq!(echoed, payload, "fake sshd must echo the payload");

    // 6. The fake sshd must have received the same bytes.
    let received = sshd_received.lock().await.clone();
    assert_eq!(received, payload, "fake sshd must observe the payload");

    // 7. Tear down. `server_dir` and `client_dir` are dropped
    //    when this scope ends, removing the temporary
    //    directories and the persistent identity files they
    //    contained.
    server_handle.shutdown().await;
    // Hold the tempdirs alive until the server is fully
    // torn down so the iroh endpoint can read its identity
    // file during shutdown.
    drop(server_dir);
    drop(client_dir);
}
