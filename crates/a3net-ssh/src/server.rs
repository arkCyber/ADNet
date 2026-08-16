//! Long-running SSH-tunnel server.
//!
//! This is the A3Net equivalent of iroh-ssh's `server_mode`. It:
//!
//! 1. Constructs an `iroh::protocol::Router` that listens on the
//!    `a3net/ssh-tunnel/1` ALPN.
//! 2. For each incoming QUIC stream, opens a `TcpStream` to
//!    `127.0.0.1:<ssh_port>` and bidirectionally copies bytes
//!    between the two until either side hangs up.
//! 3. Exposes a [`Server`] handle the REPL can `await` on (or
//!    cancel) and an `endpoint_id()` accessor for the invitation
//!    string.
//!
//! The actual byte-copying is the `SshTunnelHandler` — a small
//! `ProtocolHandler` impl, mirroring `IrohFrameHandler`'s shape
//! so the pattern stays consistent across the workspace.

#[cfg(feature = "iroh")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "iroh")]
use a3net_transport::iroh::public_key_to_node_id;
#[cfg(feature = "iroh")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
#[cfg(feature = "iroh")]
use tokio::sync::watch;

#[cfg(feature = "iroh")]
use crate::builder::{IrohSsh, SSH_TUNNEL_ALPN};
use crate::error::{SshError, SshResult};
#[cfg(feature = "iroh")]
use crate::metrics;

#[cfg(feature = "iroh")]
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
#[cfg(feature = "iroh")]
use iroh::{
    endpoint::{Connection, RecvStream, SendStream},
};

/// Default TCP probe timeout for `probe_local_ssh`. Matches
/// iroh-ssh's `Duration::from_secs(10)`.
const SSH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Probe the local SSH daemon. Returns `Ok(())` if the TCP
/// port accepts a connection within [`SSH_PROBE_TIMEOUT`],
/// otherwise [`SshError::NoSshServer`].
pub async fn probe_local_ssh(port: u16) -> SshResult<()> {
    let addr = format!("127.0.0.1:{port}");
    match tokio::time::timeout(SSH_PROBE_TIMEOUT, TcpStream::connect(&addr)).await {
        Ok(Ok(_stream)) => Ok(()),
        Ok(Err(e)) => Err(SshError::Other(format!(
            "TCP connect to {addr} failed: {e}"
        ))),
        Err(_) => Err(SshError::NoSshServer { port }),
    }
}

/// Handle to the running SSH-tunnel server.
///
/// The handle owns a shutdown channel; dropping (or calling
/// [`Server::shutdown`](Server::shutdown)) on it causes the
/// router task to exit cleanly within a few hundred milliseconds.
#[cfg(feature = "iroh")]
#[derive(Clone)]
pub struct Server {
    endpoint_id: iroh::EndpointId,
    ssh_port: u16,
    router: Arc<Router>,
    shutdown_tx: watch::Sender<bool>,
}

#[cfg(feature = "iroh")]
impl Server {
    /// Construct and start the SSH-tunnel server. Returns once
    /// the iroh `Router` has been spawned.
    pub async fn start(ssh: IrohSsh) -> SshResult<Self> {
        let endpoint_id = ssh.endpoint().id();
        let ssh_port = ssh.ssh_port();

        // The shutdown channel is owned by the handler. We
        // don't keep a `Receiver` here on purpose — the router
        // shutdown is driven by `Server::shutdown()` calling
        // `router.shutdown().await`. Per-connection tasks
        // subscribe to the channel inside `accept` and abort
        // when the sender fires `true`.
        //
        // The dropped receiver is intentional: `watch::Sender`
        // outlives its receivers, so dropping the initial one
        // doesn't affect the per-connection `subscribe()` calls
        // that happen later in `SshTunnelHandler::accept`.
        let (shutdown_tx, _drop_rx) = watch::channel(false);
        let handler = SshTunnelHandler {
            ssh_port,
            shutdown: shutdown_tx.clone(),
        };

        let router = Router::builder(ssh.endpoint().clone())
            .accept(SSH_TUNNEL_ALPN, handler)
            .spawn();

        Ok(Self {
            endpoint_id,
            ssh_port,
            router: Arc::new(router),
            shutdown_tx,
        })
    }

    /// iroh endpoint id a peer would dial.
    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.endpoint_id
    }

    /// A3Net `NodeId` form of [`Self::endpoint_id`], suitable for
    /// printing next to other A3Net identities.
    pub fn node_id(&self) -> a3net_types::NodeId {
        public_key_to_node_id(&self.endpoint_id)
    }

    /// TCP port the local SSH daemon is being proxied to.
    pub fn ssh_port(&self) -> u16 {
        self.ssh_port
    }

    /// Signal per-connection tasks to abort (via the watch
    /// channel) and wait for the router to shut down.
    ///
    /// # Behaviour
    ///
    /// 1. `shutdown_tx.send(true)` flips the watch channel.
    ///    Each in-flight `proxy_one_connection` task observes
    ///    the flip inside its `tokio::select!` and unwinds —
    ///    its QUIC streams drop, the TCP socket closes, the
    ///    `Connection` is released.
    ///
    /// 2. `router.shutdown().await` then drops the underlying
    ///    `iroh::Endpoint`, which is what actually prevents
    ///    *new* connections from being accepted. The router
    ///    also disposes of the ALPN handler we registered.
    ///
    /// # Race window
    ///
    /// A connection that arrives **between** step 1 and the
    /// `Endpoint`'s actual close (step 2 finishes) will still be
    /// accepted by iroh. Its handler will spawn
    /// `proxy_one_connection`, which will fail on its first I/O
    /// call (the endpoint is closing) and exit via the error
    /// arm. This is benign — the connection just fails — but
    /// the watch-channel flip alone does *not* guarantee that
    /// no new tasks are spawned.
    ///
    /// Safe to call multiple times; subsequent calls are no-ops
    /// once the router has already exited (the inner `Arc`
    /// keeps the router alive past `shutdown`).
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        self.router.shutdown().await.ok();
    }
}

#[cfg(feature = "iroh")]
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("endpoint_id", &self.endpoint_id)
            .field("ssh_port", &self.ssh_port)
            .finish_non_exhaustive()
    }
}

/// Per-connection SSH-tunnel handler. One instance per iroh
/// `Router`; the router clones it for each incoming stream via
/// `ProtocolHandler::accept`.
#[cfg(feature = "iroh")]
#[derive(Clone, Debug)]
struct SshTunnelHandler {
    ssh_port: u16,
    shutdown: watch::Sender<bool>,
}

#[cfg(feature = "iroh")]
impl ProtocolHandler for SshTunnelHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // iroh's `ProtocolHandler::accept` is awaited by the
        // router, so we must NOT spawn-and-return. Doing so
        // would let the router hand the connection off and
        // close it before our proxy has finished shuttling
        // bytes. iroh-ssh uses the same inline-await pattern.
        //
        // We bump the accepted-counter up front so an operator
        // can correlate a slow connection with the metric even
        // if it later fails inside `proxy_one_connection`.
        metrics::TUNNEL_CONNECTIONS_ACCEPTED.inc();
        let ssh_port = self.ssh_port;
        let shutdown_rx = self.shutdown.subscribe();
        match proxy_one_connection(connection, ssh_port, shutdown_rx).await {
            Ok(()) => {}
            Err(e) => {
                tracing::debug!("a3net-ssh: tunnel connection ended: {e}");
                metrics::TUNNEL_CONNECTIONS_FAILED.inc();
            }
        }
        Ok(())
    }
}

/// Bidirectionally copy bytes between a freshly-accepted QUIC
/// stream pair and a fresh TCP connection to the local SSH
/// daemon.
///
/// `connection.closed()` is awaited after the proxy drains so
/// the QUIC connection doesn't drop while the client is still
/// in the middle of reading the response. This mirrors the
/// pattern in iroh's own `ProtocolHandler` docs.
#[cfg(feature = "iroh")]
async fn proxy_one_connection(
    connection: Connection,
    ssh_port: u16,
    mut shutdown_rx: watch::Receiver<bool>,
) -> SshResult<()> {
    // Server side: the *client* opens the bi-stream; we accept
    // it here. iroh-ssh does the same — see
    // https://github.com/rustonbsd/iroh-ssh/blob/main/src/ssh.rs
    let (quic_send, quic_recv) = connection
        .accept_bi()
        .await
        .map_err(|e| SshError::Tunnel(format!("accept_bi: {e}")))?;
    // Honor the shutdown signal by accepting and proxying in
    // parallel. Whichever finishes first aborts the other.
    let proxy = proxy_bidirectional(ssh_port, quic_send, quic_recv);
    let shutdown = async {
        let _ = shutdown_rx.changed().await;
    };
    tokio::select! {
        r = proxy => {
            r?;
        }
        _ = shutdown => {
            // Operator-initiated shutdown. The QUIC streams
            // will be dropped as this future unwinds, which
            // closes them cleanly.
        }
    }
    // Drain the connection before letting `accept` return.
    // Without this, the `Connection` is dropped, which closes
    // the QUIC connection with code 0 — and the client may
    // observe that as `ConnectionLost(ApplicationClosed(0))`
    // before observing the EOF on `RecvStream::read_to_end`.
    // iroh's own `ProtocolHandler` example uses the same
    // pattern (`send.finish()?; connection.closed().await;`).
    // We don't surface `closed()`'s error here because a
    // graceful close is the expected outcome — the only
    // alternative is `ConnectionLost`, which the *peer* has
    // already logged.
    let _ = connection.closed().await;
    Ok(())
}

/// Bidirectional proxy between a QUIC stream pair and a TCP
/// stream. Returns once both directions have finished
/// (half-close propagation). The caller is responsible for
/// waiting on `Connection::closed` afterwards.
#[cfg(feature = "iroh")]
async fn proxy_bidirectional(
    ssh_port: u16,
    mut quic_send: SendStream,
    mut quic_recv: RecvStream,
) -> SshResult<()> {
    let tcp = TcpStream::connect(("127.0.0.1", ssh_port))
        .await
        .map_err(|e| SshError::Other(format!("connect local sshd: {e}")))?;
    // `OwnedWriteHalf` / `OwnedReadHalf` are `'static` so we can
    // move them into spawned tasks without lifetime gymnastics.
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    // quic -> tcp: take ownership of `quic_recv` and `tcp_write`
    // so the spawned task owns them outright. When the client
    // closes the QUIC stream (we read `Ok(None)`) we shut down
    // our write side of the TCP socket so the local sshd sees
    // EOF and can finish responding. Without this half-close
    // the bidirectional proxy deadlocks: the client is waiting
    // for the server's echo, but the server is waiting for the
    // client to send more, but the client already half-closed.
    let quic_to_tcp = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            // iroh's `RecvStream::read` returns
            // `Result<Option<usize>, ReadError>` — `None` means
            // the stream was reset by the peer.
            match quic_recv.read(&mut buf).await {
                Ok(Some(0)) | Err(_) => break,
                Ok(Some(n)) => {
                    if tcp_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
            }
        }
        // Half-close: signal the local sshd that no more data
        // is coming. The sshd is now expected to finish its
        // response and close its side.
        tcp_write.shutdown().await.ok();
    });

    // tcp -> quic: same idea, move ownership in. When the
    // local sshd closes the TCP socket (read returns `Ok(0)`)
    // we finish the QUIC send stream so the client sees EOF
    // on its receive side.
    let tcp_to_quic = tokio::spawn(async move {
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
        quic_send.finish().ok();
    });

    // Wait for both directions to drain. The half-closes
    // above ensure the survivor exits naturally once the
    // other side has signalled EOF.
    let _ = tokio::join!(quic_to_tcp, tcp_to_quic);
    Ok(())
}
