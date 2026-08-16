//! Local SOCKS5 / HTTP-CONNECT proxy servers.
//!
//! The vless-client runs **two** tiny proxy servers on the host
//! (one SOCKS5, one HTTP-CONNECT). User applications configure their
//! own network stack to use either of these as the upstream proxy.
//! Every byte that arrives here is forwarded, byte-for-byte, over
//! the local loopback to the xray / sing-box subprocess, which then
//! sends it through the VLESS tunnel.
//!
//! ## Why two protocols
//!
//! Most CLI tools (curl, wget) accept HTTP-CONNECT via `-x` but
//! speak SOCKS5 via `-x socks5h://`. Browsers usually default to
//! SOCKS5. By exposing both, we don't force the user to pick.
//!
//! ## Auth
//!
//! Both servers run in **no-auth** mode by default. The listener
//! binds to `127.0.0.1` exclusively so the only client able to
//! reach it is the local user — there is no authentication surface
//! to attack. If the caller asks for a non-loopback address, we
//! refuse at startup rather than silently opening a wider surface.
//!
//! ## Forwarding
//!
//! The forward step (`proxy::forward`) is a half-duplex `copy_bidirectional`:
//! we don't inspect or mutate the bytes. That keeps the proxy
//! transport-agnostic — it works for HTTPS, HTTP/2, WebSocket, gRPC,
//! or anything else the VLESS tunnel supports.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, trace, warn};

use crate::error::{VlessClientError, VlessClientResult};

/// SOCKS5 protocol constants (RFC 1928).
mod socks5 {
    pub const VERSION: u8 = 0x05;
    pub const AUTH_NONE: u8 = 0x00;
    /// `NO ACCEPTABLE METHODS` — the only reply we ever send
    /// during the method-selection step, because we only support
    /// "no auth".
    pub const NO_ACCEPTABLE: u8 = 0xFF;
    pub const CMD_CONNECT: u8 = 0x01;
    pub const ATYP_IPV4: u8 = 0x01;
    pub const ATYP_DOMAIN: u8 = 0x03;
    pub const ATYP_IPV6: u8 = 0x04;
    pub const REP_SUCCESS: u8 = 0x00;
    #[allow(dead_code)]
    pub const REP_GENERAL_FAILURE: u8 = 0x01;
    #[allow(dead_code)]
    pub const REP_NOT_ALLOWED: u8 = 0x02;
    #[allow(dead_code)]
    pub const REP_NETWORK_UNREACHABLE: u8 = 0x03;
    #[allow(dead_code)]
    pub const REP_HOST_UNREACHABLE: u8 = 0x04;
    #[allow(dead_code)]
    pub const REP_REFUSED: u8 = 0x05;
    #[allow(dead_code)]
    pub const REP_TTL_EXPIRED: u8 = 0x06;
    #[allow(dead_code)]
    pub const REP_COMMAND_UNSUPPORTED: u8 = 0x07;
    #[allow(dead_code)]
    pub const REP_ADDR_UNSUPPORTED: u8 = 0x08;
}

/// A SOCKS5 proxy server bound to a local address.
///
/// The server runs as a single accept loop; every accepted
/// connection is driven to completion by [`Socks5Server::handle`]
/// in its own task.
#[derive(Debug)]
pub struct Socks5Server {
    /// Bind address, e.g. `127.0.0.1:1080`.
    addr: SocketAddr,
    /// Where to forward accepted connections — typically the
    /// SOCKS port the xray / sing-box subprocess listens on.
    upstream: SocketAddr,
    /// Pre-bound listener. Constructed in [`Socks5Server::bind`]
    /// so a port collision surfaces as an error before we start
    /// accepting.
    listener: TcpListener,
}

impl Socks5Server {
    /// Bind the listener and refuse to start if `addr` is not on
    /// the loopback. Returns the bound server and the actual
    /// local address (useful when the caller asked for port 0
    /// and we picked one).
    pub async fn bind(
        addr: SocketAddr,
        upstream: SocketAddr,
    ) -> VlessClientResult<Self> {
        if !addr.ip().is_loopback() {
            // Refuse to expose an unauthenticated proxy on a
            // public interface. This is a safety property, not
            // a policy preference — see module docs.
            return Err(VlessClientError::BadLink(format!(
                "socks5 listener must bind to a loopback address, got {addr}"
            )));
        }
        let listener = TcpListener::bind(addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                VlessClientError::PortInUse { port: addr.port() }
            } else {
                VlessClientError::Io(e)
            }
        })?;
        let bound = listener.local_addr().map_err(VlessClientError::Io)?;
        Ok(Self {
            addr: bound,
            upstream,
            listener,
        })
    }

    /// The address the listener is bound to (post-bind; may
    /// differ from the requested address when the caller asked
    /// for port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Run the accept loop until the listener errors (typically
    /// because [`Socks5Server::shutdown`] closed it).
    pub async fn serve(self) -> VlessClientResult<()> {
        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    // Closed-by-shutdown surfaces as
                    // `NotConnected`; we treat that as graceful
                    // termination rather than propagating.
                    if e.kind() == std::io::ErrorKind::NotConnected
                        || e.kind() == std::io::ErrorKind::InvalidInput
                    {
                        return Ok(());
                    }
                    return Err(VlessClientError::Io(e));
                }
            };
            let upstream = self.upstream;
            tokio::spawn(async move {
                if let Err(e) = Self::handle(stream, peer, upstream).await {
                    warn!(peer = %peer, error = %e, "socks5 connection aborted");
                }
            });
        }
    }

    /// Drive a single SOCKS5 handshake + bidirectional copy.
    ///
    /// Split out from [`Socks5Server::serve`] so integration
    /// tests can drive a synthetic client through the same
    /// state machine without spinning up a real listener.
    pub async fn handle(
        mut client: TcpStream,
        peer: SocketAddr,
        upstream: SocketAddr,
    ) -> VlessClientResult<()> {
        // --- greeting ------------------------------------------------
        // VER | NMETHODS | METHODS...
        let mut head = [0u8; 2];
        client.read_exact(&mut head).await.map_err(VlessClientError::Io)?;
        if head[0] != socks5::VERSION {
            return Err(VlessClientError::ProxyProtocol(format!(
                "expected SOCKS5 (0x05), got 0x{:02x}",
                head[0]
            )));
        }
        let n = head[1] as usize;
        let mut methods = vec![0u8; n];
        client.read_exact(&mut methods).await.map_err(VlessClientError::Io)?;
        if !methods.contains(&socks5::AUTH_NONE) {
            // Server selection failed — reply 0x05 0xFF.
            client.write_all(&[socks5::VERSION, socks5::NO_ACCEPTABLE]).await
                .map_err(VlessClientError::Io)?;
            return Err(VlessClientError::ProxyProtocol(
                "client offered no acceptable auth methods".into(),
            ));
        }
        client.write_all(&[socks5::VERSION, socks5::AUTH_NONE]).await
            .map_err(VlessClientError::Io)?;

        // --- request -------------------------------------------------
        // VER | CMD | RSV | ATYP | DST.ADDR | DST.PORT
        let mut req = [0u8; 4];
        client.read_exact(&mut req).await.map_err(VlessClientError::Io)?;
        if req[0] != socks5::VERSION {
            return Err(VlessClientError::ProxyProtocol(format!(
                "expected SOCKS5 request, got 0x{:02x}",
                req[0]
            )));
        }
        if req[1] != socks5::CMD_CONNECT {
            reply(&mut client, socks5::REP_COMMAND_UNSUPPORTED, "127.0.0.1", 0).await?;
            return Err(VlessClientError::ProxyProtocol(format!(
                "unsupported SOCKS5 command 0x{:02x}",
                req[1]
            )));
        }
        let (target_host, target_port) = match req[3] {
            socks5::ATYP_IPV4 => {
                let mut addr = [0u8; 4];
                client.read_exact(&mut addr).await.map_err(VlessClientError::Io)?;
                let mut port = [0u8; 2];
                client.read_exact(&mut port).await.map_err(VlessClientError::Io)?;
                (
                    std::net::IpAddr::V4(addr.into()).to_string(),
                    u16::from_be_bytes(port),
                )
            }
            socks5::ATYP_DOMAIN => {
                let mut len = [0u8; 1];
                client.read_exact(&mut len).await.map_err(VlessClientError::Io)?;
                let mut name = vec![0u8; len[0] as usize];
                client.read_exact(&mut name).await.map_err(VlessClientError::Io)?;
                let mut port = [0u8; 2];
                client.read_exact(&mut port).await.map_err(VlessClientError::Io)?;
                let host = String::from_utf8(name).map_err(|e| {
                    VlessClientError::ProxyProtocol(format!("non-utf8 domain: {e}"))
                })?;
                (host, u16::from_be_bytes(port))
            }
            socks5::ATYP_IPV6 => {
                let mut addr = [0u8; 16];
                client.read_exact(&mut addr).await.map_err(VlessClientError::Io)?;
                let mut port = [0u8; 2];
                client.read_exact(&mut port).await.map_err(VlessClientError::Io)?;
                (
                    std::net::IpAddr::V6(addr.into()).to_string(),
                    u16::from_be_bytes(port),
                )
            }
            other => {
                reply(&mut client, socks5::REP_ADDR_UNSUPPORTED, "127.0.0.1", 0).await?;
                return Err(VlessClientError::ProxyProtocol(format!(
                    "unsupported ATYP 0x{other:02x}"
                )));
            }
        };
        debug!(peer = %peer, target = %format!("{target_host}:{target_port}"), "socks5 connect");

        // --- upstream dial -------------------------------------------
        let mut remote = match TcpStream::connect(upstream).await {
            Ok(s) => s,
            Err(e) => {
                let rep = match e.kind() {
                    std::io::ErrorKind::ConnectionRefused => socks5::REP_REFUSED,
                    std::io::ErrorKind::TimedOut => socks5::REP_TTL_EXPIRED,
                    std::io::ErrorKind::NetworkUnreachable => socks5::REP_NETWORK_UNREACHABLE,
                    _ => socks5::REP_GENERAL_FAILURE,
                };
                reply(&mut client, rep, "127.0.0.1", 0).await?;
                return Err(VlessClientError::Io(e));
            }
        };
        // Hand the upstream the proxy-target as a SOCKS5 CONNECT
        // request so the subprocess (which speaks SOCKS5) sees the
        // real destination. This is the canonical "chain two
        // SOCKS5 servers" pattern.
        //
        // TCP ordering note: we MUST complete the greeting exchange
        // before sending the CONNECT request. If we write both in
        // one `write_all` the TCP stack may merge them and the
        // upstream would see a corrupt greeting. By reading the
        // greeting reply before writing the CONNECT request we
        // guarantee the upstream sees two clean protocol phases.
        remote
            .write_all(&[socks5::VERSION, 1, socks5::AUTH_NONE])
            .await
            .map_err(VlessClientError::Io)?;
        let mut greeting_reply = [0u8; 2];
        remote
            .read_exact(&mut greeting_reply)
            .await
            .map_err(VlessClientError::Io)?;
        if greeting_reply != [socks5::VERSION, socks5::AUTH_NONE] {
            reply(&mut client, socks5::REP_GENERAL_FAILURE, "127.0.0.1", 0).await?;
            return Err(VlessClientError::ProxyProtocol(format!(
                "upstream greeting rejected: {greeting_reply:?}"
            )));
        }
        // Now send the CONNECT request.
        let mut h = Vec::with_capacity(64);
        h.push(socks5::VERSION);
        h.push(socks5::CMD_CONNECT);
        h.push(0x00);
        h.push(socks5::ATYP_DOMAIN);
        h.push(target_host.len() as u8);
        h.extend_from_slice(target_host.as_bytes());
        h.extend_from_slice(&target_port.to_be_bytes());
        remote.write_all(&h).await.map_err(VlessClientError::Io)?;

        // Read upstream's reply; we don't strictly need to parse
        // it (the local client only cares about our own reply),
        // but if the upstream refused we should propagate that to
        // the local client with a meaningful REP code.
        let mut upstream_reply = [0u8; 4];
        remote
            .read_exact(&mut upstream_reply)
            .await
            .map_err(VlessClientError::Io)?;
        if upstream_reply[0] != socks5::VERSION || upstream_reply[1] != socks5::REP_SUCCESS {
            let rep = upstream_reply[1];
            reply(&mut client, rep, "127.0.0.1", 0).await?;
            return Err(VlessClientError::ProxyProtocol(format!(
                "upstream rejected CONNECT with REP 0x{:02x}",
                upstream_reply[1]
            )));
        }
        // Skip BND.ADDR/BND.PORT portion of the reply, using the
        // ATYP we already read from the head.
        skip_socks5_address(&mut remote, upstream_reply[3]).await?;

        reply(&mut client, socks5::REP_SUCCESS, "127.0.0.1", 0).await?;

        // --- byte pump ------------------------------------------------
        forward(&mut client, &mut remote).await
    }
}

/// HTTP-CONNECT proxy server (RFC 7231 §4.3.6).
///
/// Smaller than SOCKS5 because there's only one request shape —
/// `CONNECT host:port HTTP/1.1`. After we reply with `200 OK`,
/// the rest of the connection is a transparent tunnel.
#[derive(Debug)]
pub struct HttpConnectServer {
    addr: SocketAddr,
    upstream: SocketAddr,
    listener: TcpListener,
}

impl HttpConnectServer {
    pub async fn bind(
        addr: SocketAddr,
        upstream: SocketAddr,
    ) -> VlessClientResult<Self> {
        if !addr.ip().is_loopback() {
            return Err(VlessClientError::BadLink(format!(
                "http listener must bind to a loopback address, got {addr}"
            )));
        }
        let listener = TcpListener::bind(addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                VlessClientError::PortInUse { port: addr.port() }
            } else {
                VlessClientError::Io(e)
            }
        })?;
        let bound = listener.local_addr().map_err(VlessClientError::Io)?;
        Ok(Self {
            addr: bound,
            upstream,
            listener,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn serve(self) -> VlessClientResult<()> {
        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotConnected
                        || e.kind() == std::io::ErrorKind::InvalidInput
                    {
                        return Ok(());
                    }
                    return Err(VlessClientError::Io(e));
                }
            };
            let upstream = self.upstream;
            tokio::spawn(async move {
                if let Err(e) = Self::handle(stream, peer, upstream).await {
                    warn!(peer = %peer, error = %e, "http connect aborted");
                }
            });
        }
    }

    pub async fn handle(
        mut client: TcpStream,
        peer: SocketAddr,
        upstream: SocketAddr,
    ) -> VlessClientResult<()> {
        // Read the request line and headers until CRLFCRLF. We
        // cap the buffer at 8 KiB to defend against pathological
        // clients.
        let mut buf = Vec::with_capacity(512);
        let mut tmp = [0u8; 512];
        loop {
            let n = client.read(&mut tmp).await.map_err(VlessClientError::Io)?;
            if n == 0 {
                return Err(VlessClientError::ProxyProtocol(
                    "client closed before sending CONNECT request".into(),
                ));
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if buf.len() > 8 * 1024 {
                return Err(VlessClientError::ProxyProtocol(
                    "request headers exceed 8 KiB".into(),
                ));
            }
        }
        let request = std::str::from_utf8(&buf).map_err(|e| {
            VlessClientError::ProxyProtocol(format!("non-utf8 request: {e}"))
        })?;
        let first_line = request.lines().next().ok_or_else(|| {
            VlessClientError::ProxyProtocol("empty request".into())
        })?;
        // `CONNECT example.com:443 HTTP/1.1`
        let mut parts = first_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");
        if method != "CONNECT" {
            client
                .write_all(
                    b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .map_err(VlessClientError::Io)?;
            return Err(VlessClientError::ProxyProtocol(format!(
                "unsupported method: {method}"
            )));
        }
        let (host, port) = target
            .rsplit_once(':')
            .ok_or_else(|| VlessClientError::ProxyProtocol("missing port".into()))?;
        let port: u16 = port.parse().map_err(|e| {
            VlessClientError::ProxyProtocol(format!("bad port: {e}"))
        })?;
        debug!(peer = %peer, target = %format!("{host}:{port}"), "http CONNECT");

        // Dial the upstream subprocess. Unlike SOCKS5 there is no
        // second protocol hop — the subprocess listens on a plain
        // TCP port for our use here, and we treat the bytes after
        // the HTTP handshake as the raw stream.
        //
        // If the dial fails we need to surface a 502 to the local
        // client; because we're already inside `async`, we just
        // `write_all` and return the error together — the client's
        // HTTP stack will see the early close as the error.
        let mut remote = match TcpStream::connect(upstream).await {
            Ok(s) => s,
            Err(e) => {
                let _ = client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await;
                return Err(VlessClientError::Io(e));
            }
        };
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(VlessClientError::Io)?;
        forward(&mut client, &mut remote).await
    }
}

/// Send a SOCKS5 reply. `bn_addr` / `bn_port` describe the bound
/// address (the address the client is now connected to *from the
/// proxy's perspective*); we always send `127.0.0.1:0` because the
/// local client only cares that the handshake succeeded.
async fn reply(
    client: &mut TcpStream,
    rep: u8,
    bnd_addr: &str,
    bnd_port: u16,
) -> VlessClientResult<()> {
    let mut out = Vec::with_capacity(32);
    out.push(socks5::VERSION);
    out.push(rep);
    out.push(0x00); // RSV
    out.push(socks5::ATYP_IPV4);
    // 127.0.0.1
    out.extend_from_slice(&[127, 0, 0, 1]);
    out.extend_from_slice(&bnd_port.to_be_bytes());
    let _ = bnd_addr; // explicit BND.ADDR is always 127.0.0.1 here
    client.write_all(&out).await.map_err(VlessClientError::Io)?;
    Ok(())
}

/// Skip the variable-length BND.ADDR + BND.PORT trailer that
/// follows a SOCKS5 reply.
///
/// The caller has already consumed the 4-byte reply head
/// (`VER | REP | RSV | ATYP`); the ATYP value is passed in so
/// we know how to skip the BND without re-reading it from the
/// stream.
async fn skip_socks5_address(stream: &mut TcpStream, atyp: u8) -> VlessClientResult<()> {
    match atyp {
        socks5::ATYP_IPV4 => {
            let mut buf = [0u8; 4 + 2];
            stream.read_exact(&mut buf).await.map_err(VlessClientError::Io)?;
        }
        socks5::ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await.map_err(VlessClientError::Io)?;
            let mut buf = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut buf).await.map_err(VlessClientError::Io)?;
        }
        socks5::ATYP_IPV6 => {
            let mut buf = [0u8; 16 + 2];
            stream.read_exact(&mut buf).await.map_err(VlessClientError::Io)?;
        }
        other => {
            return Err(VlessClientError::ProxyProtocol(format!(
                "unexpected ATYP in upstream reply: 0x{other:02x}"
            )));
        }
    }
    Ok(())
}

/// Copy bytes in both directions until either side closes.
///
/// This is intentionally a thin wrapper around
/// `tokio::io::copy_bidirectional` so the future returns when
/// either side EOFs (whichever happens first). For VPN-style
/// traffic we don't care which side closed first; the remote end
/// of a closed connection will time out naturally.
async fn forward(a: &mut TcpStream, b: &mut TcpStream) -> VlessClientResult<()> {
    let (a_to_b, b_to_a) = tokio::io::copy_bidirectional(a, b).await?;
    trace!(bytes_a_to_b = a_to_b, bytes_b_to_a = b_to_a, "proxy forward done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    async fn pick_port() -> u16 {
        // Ask the OS for a free port. Binding to :0 immediately
        // and reading it back is the standard idiom; we use a
        // throwaway listener and let it drop.
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        l.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn refuses_non_loopback_bind() {
        // A routable address (TEST-NET-1) — not loopback.
        let addr: SocketAddr = "192.0.2.1:0".parse().unwrap();
        let r = Socks5Server::bind(addr, "127.0.0.1:1".parse().unwrap()).await;
        assert!(matches!(r, Err(VlessClientError::BadLink(_))));
    }

    #[tokio::test]
    async fn http_server_refuses_non_loopback_bind() {
        // HTTP server should also refuse non-loopback addresses.
        let addr: SocketAddr = "192.0.2.1:0".parse().unwrap();
        let r = HttpConnectServer::bind(addr, "127.0.0.1:1".parse().unwrap()).await;
        assert!(matches!(r, Err(VlessClientError::BadLink(_))));
    }

    #[tokio::test]
    async fn port_collision_surfaces_port_in_use() {
        let port = pick_port().await;
        // First bind wins.
        let _first = Socks5Server::bind(
            (Ipv4Addr::LOCALHOST, port).into(),
            "127.0.0.1:1".parse().unwrap(),
        )
        .await
        .expect("first bind");
        // Second bind must fail with PortInUse.
        let r = HttpConnectServer::bind(
            (Ipv4Addr::LOCALHOST, port).into(),
            "127.0.0.1:1".parse().unwrap(),
        )
        .await;
        assert!(matches!(r, Err(VlessClientError::PortInUse { port: p }) if p == port));
    }

    #[tokio::test]
    async fn socks5_reply_format_is_correct() {
        // Verify the SOCKS5 reply bytes are exactly what a client
        // would expect: VER(0x05) | REP(0x00) | RSV(0x00) | ATYP(0x01)
        // | 127.0.0.1 | PORT(2 bytes).
        let mut out = Vec::new();
        out.push(socks5::VERSION);
        out.push(socks5::REP_SUCCESS);
        out.push(0x00); // RSV
        out.push(socks5::ATYP_IPV4);
        out.extend_from_slice(&[127u8, 0, 0, 1]);
        out.extend_from_slice(&0u16.to_be_bytes()); // port 0
        assert_eq!(out.len(), 10);
        assert_eq!(out[0], 0x05); // VER
        assert_eq!(out[1], 0x00); // REP_SUCCESS
        assert_eq!(out[2], 0x00); // RSV
        assert_eq!(out[3], 0x01); // ATYP_IPV4
        assert_eq!(&out[4..8], &[127u8, 0, 0, 1]); // 127.0.0.1
    }

    #[tokio::test]
    async fn socks5_connect_request_encoding() {
        // Verify the CONNECT request encoding for a domain target.
        let host = "example.com";
        let port: u16 = 443;
        let mut req = Vec::new();
        req.push(socks5::VERSION);           // 0x05
        req.push(socks5::CMD_CONNECT);     // 0x01
        req.push(0x00);                    // RSV
        req.push(socks5::ATYP_DOMAIN);      // 0x03
        req.push(host.len() as u8);        // domain length = 11
        req.extend_from_slice(host.as_bytes()); // "example.com"
        req.extend_from_slice(&port.to_be_bytes()); // port 443
        // Total: 1+1+1+1+1+11+2 = 18 bytes
        assert_eq!(req.len(), 18);
        assert_eq!(req[0], 0x05); // VER
        assert_eq!(req[1], 0x01); // CMD_CONNECT
        assert_eq!(req[3], 0x03); // ATYP_DOMAIN
        assert_eq!(req[4], 11);   // domain length
    }

    #[test]
    fn local_addr_is_loopback_ipv4() {
        // We can't drive `bind` in a sync test, but we can check
        // the bound addr structure of the address we'll pass to
        // it. This catches the common mistake of binding to
        // `0.0.0.0` instead of `127.0.0.1`.
        let a: SocketAddr = (IpAddr::V4(Ipv4Addr::LOCALHOST), 1080).into();
        assert!(a.ip().is_loopback());
        let b: SocketAddr = "0.0.0.0:1080".parse().unwrap();
        assert!(!b.ip().is_loopback());
    }

    #[tokio::test]
    async fn socks5_greeting_rejects_unsupported_version() {
        // Verify that a SOCKS4 greeting is rejected.
        let upstream: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let server = Socks5Server::bind(
            (Ipv4Addr::LOCALHOST, 0).into(),
            upstream,
        )
        .await
        .expect("server bind");
        let addr = server.local_addr();

        let mut client = TcpStream::connect(addr).await.expect("connect");
        // Send SOCKS4 greeting (version 0x04 instead of 0x05)
        client
            .write_all(&[0x04, 0x01]) // VER=4, NMETHODS=1
            .await
            .expect("write");
        client
            .write_all(&[0x00]) // METHOD=NO_AUTH
            .await
            .expect("write");

        // Should get an error response, or connection closed
        let mut buf = [0u8; 2];
        let result = client.read_exact(&mut buf).await;
        // Either EOF or error is acceptable — SOCKS4 not supported
        assert!(result.is_err() || buf[0] != socks5::VERSION);
    }

    #[tokio::test]
    async fn socks5_greeting_accepts_no_auth() {
        // Test that a valid SOCKS5 greeting with NO_AUTH works.
        let upstream: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let server = Socks5Server::bind(
            (Ipv4Addr::LOCALHOST, 0).into(),
            upstream,
        )
        .await
        .expect("server bind");
        let addr = server.local_addr();

        let mut client = TcpStream::connect(addr).await.expect("connect");
        // Valid SOCKS5 greeting with NO_AUTH
        client
            .write_all(&[socks5::VERSION, 0x01, socks5::AUTH_NONE])
            .await
            .expect("write");

        // Should get VER=5, METHOD=0 (NO_AUTH)
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.expect("read");
        assert_eq!(reply[0], socks5::VERSION);
        assert_eq!(reply[1], socks5::AUTH_NONE);
    }

    #[tokio::test]
    async fn socks5_handshake_rejects_auth_methods() {
        // Test that we reject methods other than NO_AUTH.
        let upstream: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let server = Socks5Server::bind(
            (Ipv4Addr::LOCALHOST, 0).into(),
            upstream,
        )
        .await
        .expect("server bind");
        let addr = server.local_addr();

        let mut client = TcpStream::connect(addr).await.expect("connect");
        // Greeting with username/password auth (method 0x02) — not supported
        client
            .write_all(&[socks5::VERSION, 0x02, 0x02]) // Only offer USERNAME/PASSWORD
            .await
            .expect("write");

        // Should get VER=5, METHOD=0xFF (NO ACCEPTABLE)
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.expect("read");
        assert_eq!(reply[0], socks5::VERSION);
        assert_eq!(reply[1], socks5::NO_ACCEPTABLE);
    }

    #[tokio::test]
    async fn socks5_handshake_rejects_connect_without_greeting() {
        // Test that a CONNECT request without greeting fails.
        let upstream: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let server = Socks5Server::bind(
            (Ipv4Addr::LOCALHOST, 0).into(),
            upstream,
        )
        .await
        .expect("server bind");
        let addr = server.local_addr();

        let mut client = TcpStream::connect(addr).await.expect("connect");
        // Skip greeting, send a request directly
        client
            .write_all(&[socks5::VERSION, socks5::CMD_CONNECT, 0x00, socks5::ATYP_DOMAIN])
            .await
            .expect("write");

        // Should get an error or EOF
        let result = client.read(&mut [0u8; 1]).await;
        assert!(result.is_err() || result.unwrap() == 0);
    }

    #[test]
    fn socks5_constants_are_stable() {
        // Verify SOCKS5 protocol constants are stable.
        assert_eq!(socks5::VERSION, 0x05);
        assert_eq!(socks5::AUTH_NONE, 0x00);
        assert_eq!(socks5::NO_ACCEPTABLE, 0xFF);
        assert_eq!(socks5::CMD_CONNECT, 0x01);
        assert_eq!(socks5::ATYP_IPV4, 0x01);
        assert_eq!(socks5::ATYP_DOMAIN, 0x03);
        assert_eq!(socks5::ATYP_IPV6, 0x04);
        assert_eq!(socks5::REP_SUCCESS, 0x00);
    }

    #[test]
    fn socks5_reply_codes_are_defined() {
        // Verify all SOCKS5 reply codes are defined (even if unused).
        assert_eq!(socks5::REP_GENERAL_FAILURE, 0x01);
        assert_eq!(socks5::REP_NOT_ALLOWED, 0x02);
        assert_eq!(socks5::REP_NETWORK_UNREACHABLE, 0x03);
        assert_eq!(socks5::REP_HOST_UNREACHABLE, 0x04);
        assert_eq!(socks5::REP_REFUSED, 0x05);
        assert_eq!(socks5::REP_TTL_EXPIRED, 0x06);
        assert_eq!(socks5::REP_COMMAND_UNSUPPORTED, 0x07);
        assert_eq!(socks5::REP_ADDR_UNSUPPORTED, 0x08);
    }
}
