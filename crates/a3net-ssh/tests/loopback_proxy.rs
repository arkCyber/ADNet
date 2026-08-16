//! Loopback round-trip test for the QUIC ↔ TCP bidirectional
//! proxy contract.
//!
//! We don't traverse the iroh QUIC stack here — that's
//! `integration_tunnel.rs`'s job. This test exists because the
//! production proxy has *three* interleaved code paths
//! (QUIC-recv-EOF → TCP-write-shutdown, TCP-read-EOF →
//! QUIC-send-finish, and Connection::closed().await) and each
//! has its own race window. Driving them through iroh means a
//! flake is hard to root-cause; a loopback version makes the
//! failure mode obvious.
//!
//! We use `tokio::io::duplex` to construct an in-memory pipe
//! and split it into a `ReadHalf` / `WriteHalf` pair — the
//! same shape iroh's `RecvStream` / `SendStream` pair has at
//! the byte level (both implement `AsyncRead` +
//! `AsyncWrite` + a `shutdown` / `finish` operation). The TCP
//! side connects to a local echo listener that closes its
//! socket when the client half-closes its write side.

#![cfg(feature = "iroh")]

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Spawn a TCP echo server on a kernel-allocated port. The
/// echo server records every byte it reads into `received` and
/// closes its socket when the client half-closes its write
/// side. Returns the bound port so the test can dial it.
async fn echo_server(received: Arc<Mutex<Vec<u8>>>) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            let received = Arc::clone(&received);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => return,
                        Ok(n) => {
                            received.lock().await.extend_from_slice(&buf[..n]);
                            if sock.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            });
        }
    });
    Ok(port)
}

/// Mirror of the production `proxy_bidirectional` contract.
/// Takes a QUIC-side (in-memory) `ReadHalf` + `WriteHalf` and
/// a TCP-side `OwnedReadHalf` + `OwnedWriteHalf` and shuttles
/// bytes between them until either side EOFs.
///
/// - `quic_recv` (QUIC read) → `tcp_write` (TCP write):
///   when the QUIC side returns `Ok(0)`, half-close the TCP
///   write side so the local echo server sees EOF.
/// - `tcp_read` (TCP read) → `quic_send` (QUIC write):
///   when the TCP side returns `Ok(0)`, half-close the QUIC
///   write side so the remote sees EOF.
async fn run_proxy_half(
    mut quic_recv: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    mut tcp_write: tokio::net::tcp::OwnedWriteHalf,
    mut tcp_read: tokio::net::tcp::OwnedReadHalf,
    mut quic_send: tokio::io::WriteHalf<tokio::io::DuplexStream>,
) {
    let q_to_t = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match quic_recv.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tcp_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        tcp_write.shutdown().await.ok();
    });

    let t_to_q = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if quic_send.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        quic_send.shutdown().await.ok();
    });

    let _ = tokio::join!(q_to_t, t_to_q);
}

/// Half-close propagation in the QUIC → TCP direction: the
/// QUIC side writes a payload, half-closes, and the echo
/// server must observe the payload and reply. The proxy must
/// then unwind within a small timeout — otherwise the
/// half-close contract is broken and we deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_proxy_quic_to_tcp_half_close() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let port = echo_server(Arc::clone(&received)).await.expect("echo server");

    // QUIC-side pipe. `duplex()` returns one DuplexStream;
    // `split()` gives us (ReadHalf, WriteHalf). The two
    // sides of the duplex are interchangeable — both halves
    // can read from and write to each other.
    let (client_pipe, server_pipe) = duplex(64 * 1024);
    let (mut client_read, mut client_write) = tokio::io::split(client_pipe);
    let (server_read, server_write) = tokio::io::split(server_pipe);

    // Local "sshd" TCP socket.
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect echo");
    let (tcp_read, tcp_write) = tcp.into_split();

    // Spawn the proxy contract. The "server" side of the
    // duplex faces the local sshd.
    let proxy_task = tokio::spawn(run_proxy_half(
        server_read,
        tcp_write,
        tcp_read,
        server_write,
    ));

    // Client side of the duplex: write payload, half-close.
    let payload = b"loopback round-trip payload";
    client_write.write_all(payload).await.expect("client write");
    client_write.shutdown().await.expect("client half-close");

    // Read until EOF. The echo server closes when we
    // half-close, and the proxy propagates that into
    // `client_read` as `Ok(0)`.
    let mut echoed = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match client_read.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => echoed.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    assert_eq!(echoed, payload, "echo server must round-trip the payload");

    // The echo server must have observed the same bytes.
    let observed = received.lock().await.clone();
    assert_eq!(observed, payload, "echo server must observe the payload");

    // The proxy must unwind within a reasonable timeout —
    // if it deadlocked (because one direction never saw
    // EOF), this would hang the test until the test runner
    // kills it.
    timeout(Duration::from_secs(5), proxy_task)
        .await
        .expect("proxy task must not deadlock")
        .expect("proxy task must not panic");
}

/// Half-close propagation in the TCP → QUIC direction: the
/// QUIC side writes a payload, the echo server replies and
/// closes its socket (because the client half-closed), and
/// the QUIC side must observe `Ok(0)` on its read half. This
/// is the mirror of `loopback_proxy_quic_to_tcp_half_close`
/// — written separately so a regression in either direction
/// points at the right line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_proxy_tcp_close_propagates_to_quic() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let port = echo_server(Arc::clone(&received)).await.expect("echo server");

    let (client_pipe, server_pipe) = duplex(64 * 1024);
    let (mut client_read, mut client_write) = tokio::io::split(client_pipe);
    let (server_read, server_write) = tokio::io::split(server_pipe);

    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let (tcp_read, tcp_write) = tcp.into_split();

    let proxy_task = tokio::spawn(run_proxy_half(
        server_read,
        tcp_write,
        tcp_read,
        server_write,
    ));

    // Don't write anything to the echo server; just close
    // our write side. The proxy's `quic_recv.read` returns
    // `Ok(0)`, the proxy half-closes the TCP write side,
    // the echo server sees EOF and closes its socket, the
    // proxy's `tcp_read.read` returns `Ok(0)`, the proxy
    // half-closes the QUIC send side, and `client_read`
    // returns `Ok(0)`.
    client_write.shutdown().await.expect("client half-close");

    let mut echoed = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match client_read.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => echoed.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    assert!(
        echoed.is_empty(),
        "no payload was sent; nothing should echo: got {echoed:?}"
    );

    timeout(Duration::from_secs(5), proxy_task)
        .await
        .expect("proxy task must not deadlock")
        .expect("proxy task must not panic");
}
