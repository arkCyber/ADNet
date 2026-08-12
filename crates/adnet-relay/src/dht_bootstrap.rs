//! DHT bootstrap endpoint.
//!
//! A minimal TCP server that ADNet DHT nodes can connect to as a
//! `bootstrap` peer. The bootstrap node itself does **not** store
//! the routing table — it merely hands out a small set of
//! well-known peers so the newcomer can `FIND_NODE` into the rest
//! of the network.
//!
//! ## Wire protocol (intentionally tiny)
//!
//! All messages are length-prefixed CBOR (4-byte big-endian length
//! prefix, payload bytes). The bootstrap server speaks two message
//! types today:
//!
//!   * `HelloRequest { magic: [u8; 4], version: u16 }` →
//!     `HelloResponse { version: u16, peers: Vec<PeerInfo> }`
//!   * `Ping { nonce: [u8; 8] }` →
//!     `Pong { nonce: [u8; 8] }`
//!
//! The whole point of the bootstrap server is **low surface area**:
//! no DHT storage, no joins, no DHT lookups — those happen between
//! the client and the peers it learned about. A compromised
//! bootstrap can only lie about its peer list; it cannot tamper
//! with records a client subsequently fetches.

use std::net::SocketAddr;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MAGIC: &[u8; 4] = b"ADDB";
const PROTO_VERSION: u16 = 1;

/// A peer the bootstrap is willing to share.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub node_id: String,
    pub addr: String,
}

/// Mutable configuration for the bootstrap — peers can be added
/// at runtime by the operator or by an upstream discovery source.
#[derive(Debug, Default, Clone)]
pub struct BootstrapConfig {
    inner: Arc<RwLock<BootstrapInner>>,
}

#[derive(Debug, Default)]
struct BootstrapInner {
    peers: Vec<PeerInfo>,
}

impl BootstrapConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_static_peers<I, P>(self, peers: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PeerInfo>,
    {
        let s = self;
        s.inner.write().peers.extend(peers.into_iter().map(Into::into));
        s
    }

    pub fn add_peer(&self, peer: PeerInfo) {
        self.inner.write().peers.push(peer);
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        self.inner.read().peers.clone()
    }

    pub fn peer_count(&self) -> usize {
        self.inner.read().peers.len()
    }
}

impl From<(String, String)> for PeerInfo {
    fn from((node_id, addr): (String, String)) -> Self {
        Self { node_id, addr }
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum ClientMessage {
    Hello {
        version: u16,
    },
    Ping {
        nonce: [u8; 8],
    },
}

#[derive(Debug, Serialize, Deserialize)]
enum ServerMessage {
    Hello {
        version: u16,
        peers: Vec<PeerInfo>,
    },
    Pong {
        nonce: [u8; 8],
    },
    Error {
        reason: String,
    },
}

/// Start a bootstrap server bound to `bind`. Returns once the
/// listener has been bound and the accept loop is running; drop the
/// handle to stop.
pub async fn serve(bind: SocketAddr, cfg: BootstrapConfig) -> std::io::Result<BootstrapHandle> {
    let listener = TcpListener::bind(bind).await?;
    let local = listener.local_addr().ok();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let cfg = Arc::new(cfg);
    tokio::spawn(async move {
        run(listener, cfg.clone(), rx).await;
    });
    Ok(BootstrapHandle {
        local,
        shutdown: Some(tx),
    })
}

async fn run(
    listener: TcpListener,
    cfg: Arc<BootstrapConfig>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            res = listener.accept() => {
                let (stream, peer) = match res {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, cfg).await {
                        tracing::debug!(peer = %peer, error = %e, "bootstrap client error");
                    }
                });
            }
        }
    }
}

async fn handle(mut stream: tokio::net::TcpStream, cfg: Arc<BootstrapConfig>) -> std::io::Result<()> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > 16 * 1024 {
        return Ok(());
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await?;

    let msg: ClientMessage = match ciborium::de::from_reader(&payload[..]) {
        Ok(m) => m,
        Err(e) => {
            send_msg(&mut stream, &ServerMessage::Error { reason: e.to_string() }).await?;
            return Ok(());
        }
    };

    let reply = match msg {
        ClientMessage::Hello { version } => {
            if version != PROTO_VERSION {
                ServerMessage::Error {
                    reason: format!("unsupported version {version}"),
                }
            } else {
                ServerMessage::Hello {
                    version: PROTO_VERSION,
                    peers: cfg.peers(),
                }
            }
        }
        ClientMessage::Ping { nonce } => ServerMessage::Pong { nonce },
    };
    send_msg(&mut stream, &reply).await
}

async fn send_msg(stream: &mut tokio::net::TcpStream, msg: &ServerMessage) -> std::io::Result<()> {
    let mut body = Vec::new();
    ciborium::ser::into_writer(msg, &mut body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = body.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

pub struct BootstrapHandle {
    pub local: Option<SocketAddr>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BootstrapHandle {
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
    pub fn addr(&self) -> Option<SocketAddr> {
        self.local
    }
}

/// Helper that runs the bootstrap server with a peer list read from
/// the operator's config file. The file is a JSON array of
/// `PeerInfo`.
pub fn load_peers_from_file(
    path: &std::path::Path,
) -> Result<Vec<PeerInfo>, std::io::Error> {
    let bytes = std::fs::read(path)?;
    let peers: Vec<PeerInfo> = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};
    use tokio::io::AsyncReadExt as _;
    use tokio::io::AsyncWriteExt as _;

    fn bind() -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
    }

    async fn read_frame(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).await.unwrap();
        let n = u32::from_be_bytes(len) as usize;
        let mut buf = vec![0u8; n];
        stream.read_exact(&mut buf).await.unwrap();
        buf
    }

    async fn send_msg(stream: &mut tokio::net::TcpStream, msg: &ClientMessage) {
        let mut body = Vec::new();
        ciborium::ser::into_writer(msg, &mut body).unwrap();
        let len = body.len() as u32;
        stream.write_all(&len.to_be_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        stream.flush().await.unwrap();
    }

    #[tokio::test]
    async fn hello_returns_static_peers() {
        let cfg = BootstrapConfig::new()
            .with_static_peers(vec![("node-a".into(), "1.2.3.4:7777".into())]);
        let handle = serve(bind(), cfg).await.unwrap();
        let addr = handle.addr().unwrap();
        // Don't shut down until the client has finished.
        let cfg_clone = handle; // moved
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        send_msg(&mut stream, &ClientMessage::Hello { version: PROTO_VERSION }).await;
        let payload = read_frame(&mut stream).await;
        let reply: ServerMessage = ciborium::de::from_reader(&payload[..]).unwrap();
        match reply {
            ServerMessage::Hello { peers, .. } => {
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0].node_id, "node-a");
                assert_eq!(peers[0].addr, "1.2.3.4:7777");
            }
            other => panic!("unexpected: {other:?}"),
        }
        cfg_clone.shutdown();
    }

    #[tokio::test]
    async fn ping_returns_pong_with_same_nonce() {
        let cfg = BootstrapConfig::new();
        let handle = serve(bind(), cfg).await.unwrap();
        let mut stream = tokio::net::TcpStream::connect(handle.addr().unwrap()).await.unwrap();
        let nonce = [0xab; 8];
        send_msg(&mut stream, &ClientMessage::Ping { nonce }).await;
        let payload = read_frame(&mut stream).await;
        let reply: ServerMessage = ciborium::de::from_reader(&payload[..]).unwrap();
        match reply {
            ServerMessage::Pong { nonce: got } => assert_eq!(got, nonce),
            other => panic!("unexpected: {other:?}"),
        }
        handle.shutdown();
    }

    #[tokio::test]
    async fn version_mismatch_returns_error() {
        let cfg = BootstrapConfig::new();
        let handle = serve(bind(), cfg).await.unwrap();
        let mut stream = tokio::net::TcpStream::connect(handle.addr().unwrap()).await.unwrap();
        send_msg(&mut stream, &ClientMessage::Hello { version: 999 }).await;
        let payload = read_frame(&mut stream).await;
        let reply: ServerMessage = ciborium::de::from_reader(&payload[..]).unwrap();
        assert!(matches!(reply, ServerMessage::Error { .. }));
        handle.shutdown();
    }
}