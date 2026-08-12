//! Local DNS server — binds `:53` and forwards mesh queries to the resolver.
//!
//! This module provides [`DnsServer`], a standalone async DNS server
//! that binds UDP/TCP port 53 and answers `.ray` / `.adnet` queries
//! locally. All other queries are forwarded to the configured upstream
//! servers.

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::config::ResolverConfig;
use crate::forwarder::TunDnsForwarder;
use crate::Resolver;

/// Default port for the local DNS server.
pub const DEFAULT_DNS_PORT: u16 = 53;

/// Maximum DNS message size (RFC 6891 §6.2.5).
pub const MAX_DNS_SIZE: usize = 4096;

/// Handle to a running [`DnsServer`].
#[derive(Debug)]
pub struct DnsServerHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    port: u16,
}

impl DnsServerHandle {
    /// The port the server is bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Signal the server to shut down.
    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// A local DNS server that answers `.ray` / `.adnet` queries
/// and forwards everything else upstream.
#[derive(Clone)]
pub struct DnsServer {
    resolver: Resolver,
    config: ResolverConfig,
}

impl DnsServer {
    /// Build a new DNS server with the given resolver and config.
    pub fn new(resolver: Resolver, config: ResolverConfig) -> Self {
        Self { resolver, config }
    }

    /// Start the DNS server bound to the mesh's local address.
    ///
    /// Uses `100.64.0.1:53` by default (the mesh gateway address).
    /// Use [`serve_on`](Self::serve_on) to bind a custom address.
    pub async fn serve(self) -> std::result::Result<DnsServerHandle, ServeError> {
        self.serve_on(SocketAddr::V4(SocketAddrV4::new(
            std::net::Ipv4Addr::new(100, 64, 0, 1),
            DEFAULT_DNS_PORT,
        )))
        .await
    }

    /// Start the DNS server bound to `bind`.
    ///
    /// Returns a [`DnsServerHandle`] on success. The server runs
    /// in the background until the handle's [`shutdown`](DnsServerHandle::shutdown)
    /// is called.
    pub async fn serve_on(
        self,
        bind: SocketAddr,
    ) -> std::result::Result<DnsServerHandle, ServeError> {
        let resolver = self.resolver.clone();
        let config = self.config.clone();

        let udp = UdpSocket::bind(bind)
            .await
            .map_err(ServeError::Bind)?;
        udp.set_broadcast(true).ok();
        let tcp = TcpListener::bind(bind)
            .await
            .map_err(ServeError::Bind)?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let forwarder = Arc::new(TunDnsForwarder::new(resolver, config));

        let port = bind.port();
        let handle = DnsServerHandle {
            shutdown_tx: Some(shutdown_tx),
            port,
        };

        // UDP loop.
        let fwd_udp = forwarder.clone();
        tokio::spawn(async move {
            Self::run_udp(udp, fwd_udp, shutdown_rx).await;
        });

        // TCP acceptor loop.
        let fwd_tcp = forwarder;
        tokio::spawn(async move {
            Self::run_tcp(tcp, fwd_tcp).await;
        });

        Ok(handle)
    }

    async fn run_udp(
        socket: UdpSocket,
        forwarder: Arc<TunDnsForwarder>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        let mut buf = vec![0u8; MAX_DNS_SIZE];
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!("DNS server: UDP loop shutting down");
                    return;
                }
                res = socket.recv_from(&mut buf) => {
                    let (n, peer) = match res {
                        Ok(r) => r,
                        Err(e) => {
                            warn!(error = %e, "DNS UDP recv error");
                            continue;
                        }
                    };

                    let query = buf[..n].to_vec();
                    let response = Self::handle_udp_query(&query, &forwarder).await;

                    if let Some(resp) = response {
                        if let Err(e) = socket.send_to(&resp, peer).await {
                            warn!(error = %e, peer = %peer, "DNS UDP send error");
                        }
                    }
                }
            }
        }
    }

    /// Handle one UDP DNS query and return a response, if any.
    async fn handle_udp_query(query: &[u8], forwarder: &Arc<TunDnsForwarder>) -> Option<Vec<u8>> {
        if query.len() < 12 {
            debug!("UDP query too short");
            return None;
        }

        // Extract the QNAME from the DNS question (after the 12-byte header).
        let qname = Self::extract_qname(query.get(12..)?)?;
        let qtype = Self::extract_qtype(query.get(12..)?)?;

        // Check if this is a mesh TLD.
        let labels: Vec<&str> = qname.rsplit('.').collect();
        let tld = labels.first().copied().unwrap_or("");
        if !forwarder.config.is_mesh_tld(tld) {
            // Forward to upstream.
            return Self::forward_to_upstream(query, &forwarder.config).await;
        }

        debug!(qname = %qname, qtype = qtype, "DNS: mesh query");

        // Resolve via the resolver.
        let vip = forwarder.resolver.resolve_str(&qname, None).ok();

        let response = match vip {
            Some(vip) => forwarder.build_dns_response_for_type(query, &qname, vip, qtype),
            None => forwarder.build_nxdomain_response(query, &qname),
        };

        Some(response)
    }

    async fn run_tcp(
        listener: TcpListener,
        forwarder: Arc<TunDnsForwarder>,
    ) {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let fwd = forwarder.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_tcp(stream, &fwd).await {
                            warn!(error = %e, peer = %peer, "DNS TCP handler error");
                        }
                    });
                }
                Err(e) => {
                    warn!(error = %e, "DNS TCP accept error");
                }
            }
        }
    }

    async fn handle_tcp(
        mut stream: TcpStream,
        forwarder: &Arc<TunDnsForwarder>,
    ) -> Result<(), ServeError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Read 2-byte length prefix, then the DNS message.
        let mut len_buf = [0u8; 2];
        stream.read_exact(&mut len_buf).await.map_err(ServeError::Io)?;
        let msg_len = u16::from_be_bytes(len_buf) as usize;
        if msg_len > MAX_DNS_SIZE {
            return Err(ServeError::MessageTooLarge(msg_len));
        }

        let mut msg = vec![0u8; msg_len];
        stream.read_exact(&mut msg).await.map_err(ServeError::Io)?;

        let response = Self::handle_udp_query(&msg, forwarder)
            .await
            .unwrap_or_else(|| Self::empty_nxdomain(&msg));

        // Write 2-byte length prefix + response.
        let mut out = Vec::with_capacity(2 + response.len());
        out.extend_from_slice(&(response.len() as u16).to_be_bytes());
        out.extend_from_slice(&response);

        stream.write_all(&out).await.map_err(ServeError::Io)?;
        stream.flush().await.map_err(ServeError::Io)?;

        Ok(())
    }

    async fn forward_to_upstream(query: &[u8], config: &ResolverConfig) -> Option<Vec<u8>> {
        if config.upstreams.is_empty() {
            return None;
        }

        let upstream = config.upstreams.first()?;
        let addr: SocketAddr = upstream.parse().ok()?;

        let socket = match UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(_) => return None,
        };

        let timeout = tokio::time::sleep(std::time::Duration::from_secs(3));

        tokio::pin!(timeout);

        tokio::select! {
            res = socket.send_to(query, addr) => {
                if res.is_err() {
                    return None;
                }
            }
            _ = &mut timeout => {
                return None;
            }
        }

        let mut buf = vec![0u8; MAX_DNS_SIZE];

        tokio::select! {
            res = socket.recv_from(&mut buf) => {
                match res {
                    Ok((n, _)) => Some(buf[..n].to_vec()),
                    Err(_) => None,
                }
            }
            _ = &mut timeout => None,
        }
    }

    fn extract_qname(slice: &[u8]) -> Option<String> {
        let mut labels = Vec::new();
        let mut i = 0;
        loop {
            if i >= slice.len() {
                return None;
            }
            let len = slice[i] as usize;
            i += 1;
            if len == 0 {
                break;
            }
            if i + len > slice.len() {
                return None;
            }
            let label = std::str::from_utf8(&slice[i..i + len]).ok()?.to_lowercase();
            labels.push(label);
            i += len;
        }
        if labels.is_empty() {
            return None;
        }
        Some(labels.join("."))
    }

    fn extract_qtype(slice: &[u8]) -> Option<u16> {
        // Skip QNAME.
        let mut i = 0;
        loop {
            if i >= slice.len() {
                return None;
            }
            let len = slice[i] as usize;
            i += 1;
            if len == 0 {
                break;
            }
            i += len;
        }
        if i + 4 > slice.len() {
            return None;
        }
        Some(u16::from_be_bytes([slice[i], slice[i + 1]]))
    }

    fn empty_nxdomain(query: &[u8]) -> Vec<u8> {
        let id = if query.len() >= 2 {
            u16::from_be_bytes([query[0], query[1]])
        } else {
            0
        };
        let mut out = Vec::with_capacity(12);
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&0x8183u16.to_be_bytes()); // NXDOMAIN
        out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
        out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
        // Echo question.
        let qname = Self::extract_qname(query.get(12..).unwrap_or(&[])).unwrap_or_default();
        for label in qname.split('.') {
            if label.is_empty() {
                continue;
            }
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
        // Append QTYPE + QCLASS from the query.
        if let Some(rest) = query.get(12..) {
            let mut i = 0;
            // Skip QNAME in the echoed question.
            loop {
                if i >= rest.len() {
                    break;
                }
                let len = rest[i] as usize;
                i += 1;
                if len == 0 {
                    i += 1; // skip the null terminator
                    break;
                }
                i += len;
            }
            if i + 4 <= rest.len() {
                out.extend_from_slice(&rest[i..i + 4]);
            }
        }
        out
    }
}

/// DNS server startup errors.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("bind: {0}")]
    Bind(std::io::Error),

    #[error("io: {0}")]
    Io(std::io::Error),

    #[error("upstream: {0}")]
    Upstream(String),

    #[error("message too large: {0} bytes")]
    MessageTooLarge(usize),
}

impl From<std::io::Error> for ServeError {
    fn from(e: std::io::Error) -> Self {
        ServeError::Io(e)
    }
}

impl From<crate::forwarder::ForwarderError> for ServeError {
    fn from(e: crate::forwarder::ForwarderError) -> Self {
        ServeError::Upstream(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_qname() {
        let dns = b"\x05alice\x06gaming\x03ray\x00\x00\x01\x00\x01";
        let name = DnsServer::extract_qname(dns).unwrap();
        assert_eq!(name, "alice.gaming.ray");
    }

    #[test]
    fn extract_qtype() {
        let dns = b"\x05alice\x03ray\x00\x00\x01\x00\x01";
        assert_eq!(DnsServer::extract_qtype(dns), Some(1));

        let dns_aaaa = b"\x05alice\x03ray\x00\x00\x1c\x00\x01";
        assert_eq!(DnsServer::extract_qtype(dns_aaaa), Some(28));
    }

    #[test]
    fn empty_nxdomain() {
        let q = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00";
        let resp = DnsServer::empty_nxdomain(q);
        assert!(!resp.is_empty());
        // ID should be echoed.
        assert_eq!(resp[0], 0x12);
        assert_eq!(resp[1], 0x34);
        // RCODE = 3 (NXDOMAIN) in byte 3.
        assert_eq!(resp[3] & 0x0f, 3);
    }
}
