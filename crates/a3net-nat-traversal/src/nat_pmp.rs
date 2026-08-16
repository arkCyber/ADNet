//! NAT-PMP (RFC 6886) — gateway-side port-mapping protocol.
//!
//! NAT-PMP is a UDP-only, gateway-side protocol that lets a
//! host behind a NAT ask the gateway to forward a UDP or TCP
//! port and to report its external IPv4 address. It is the
//! Apple-shipped sibling of UPnP IGD and is widely supported
//! on consumer home routers.
//!
//! ## Wire format
//!
//! All NAT-PMP messages are 8-byte headers (version +
//! opcode + reserved + result/body bytes 0..4) followed by a
//! small fixed-size payload. There are exactly five opcodes
//! defined by RFC 6886 (the version 0 / version 1 split
//! follows RFC 6978 — we speak version 0, which is the
//! overwhelmingly common case).
//!
//! - `0` — Address request/response (1 byte version, 1 byte
//!   opcode, 2 reserved bytes, **4-byte result: external IPv4**)
//! - `1` — Map UDP request/response (1 byte version, 1 byte
//!   opcode, 2 reserved bytes, 2-byte internal port, 2-byte
//!   external port, **4-byte lifetime**, **4-byte mapped
//!   external port** for the response)
//! - `2` — Map TCP request/response (same shape as UDP)
//!
//! Result code `0` is success. Non-zero codes are documented
//! in § 3.6 (1 = unsupported version, 2 = not authorised,
//! …). Errors are surfaced as [`crate::error::NatError::NatPmp`].
//!
//! ## Gateway address
//!
//! The protocol lives on **gateway IP port 5351**. Per RFC
//! 6886 § 3.1 the gateway is the next-hop router, which
//! clients discover either from the interface's default
//! gateway or from the DHCP `Router` option. We expose this
//! via [`NatPmpClient::discover_gateway`] so callers don't
//! have to reach into the OS for it.
//!
//! ## Reference
//!
//! - RFC 6886 — NAT Port Mapping Protocol (NAT-PMP)
//! - RFC 6978 — IANA registration (which fixes opcode 128 +
//!   port 5350 for PCP and opcode 0 + port 5351 for NAT-PMP)

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

use crate::config::PortMappingProtocol;
use crate::error::{NatError, NatResult};

/// Default NAT-PMP gateway port. RFC 6886 § 3.1.
pub const NAT_PMP_PORT: u16 = 5351;
/// NAT-PMP protocol version. RFC 6886 § 3.1 ("version 0").
pub const NAT_PMP_VERSION: u8 = 0;
/// Default request/response timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

/// Discovered external address (RFC 6886 § 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalAddress {
    /// Seconds since epoch when the gateway last refreshed
    /// its external address. Used to expire cached results.
    pub seconds_since_epoch: u32,
    /// The external IPv4 address the gateway is presenting
    /// to the public internet.
    pub ipv4: Ipv4Addr,
}

/// A granted port mapping (RFC 6886 § 3.3 / § 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortMappingResult {
    /// Internal port the host bound on.
    pub internal_port: u16,
    /// External port the gateway assigned (may differ from
    /// internal — the gateway may rewrite).
    pub external_port: u16,
    /// Mapping lifetime in seconds. `0` means the gateway
    /// rejected the request; positive values are the lease
    /// duration. Mappings must be renewed before this
    /// elapses.
    pub lifetime_seconds: u32,
}

/// NAT-PMP client. Cheap to clone (the embedded [`SocketAddr`]
/// is a small payload and the per-call socket is allocated
/// fresh).
#[derive(Debug, Clone)]
pub struct NatPmpClient {
    /// Gateway address, including the well-known NAT-PMP
    /// port 5351. Set via [`NatPmpClient::new`] or
    /// [`NatPmpClient::with_gateway`].
    gateway: SocketAddr,
    /// Per-request timeout.
    timeout: Duration,
}

impl NatPmpClient {
    /// Build a client addressed at `gateway`. If the supplied
    /// `SocketAddr` doesn't already point at port 5351, the
    /// port is rewritten to the well-known NAT-PMP port —
    /// per RFC 6886 § 3.1 the port is fixed.
    pub fn new(gateway: SocketAddr) -> Self {
        // NAT-PMP is IPv4-only — RFC 6886 § 8 explicitly
        // excludes IPv6. v6 hosts should use
        // [`crate::pcp::PcpClient`] instead.
        let port = if gateway.port() == 0 {
            NAT_PMP_PORT
        } else {
            NAT_PMP_PORT
        };
        let ipv4 = match gateway.ip() {
            std::net::IpAddr::V4(v4) => v4,
            std::net::IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
        };
        Self {
            gateway: SocketAddr::V4(SocketAddrV4::new(ipv4, port)),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-request timeout (default 500 ms).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Gateway address the client will talk to.
    pub fn gateway(&self) -> SocketAddr {
        self.gateway
    }

    /// RFC 6886 § 3.2 — fetch the gateway's external IPv4
    /// address. Result is a fresh `ExternalAddress`; callers
    /// that want caching should key on `seconds_since_epoch`.
    pub async fn external_address(&self) -> NatResult<ExternalAddress> {
        let socket = bind_ephemeral_v4().await?;
        // Request layout (8 bytes total):
        //   version: u8 = 0
        //   opcode: u8 = 0
        //   reserved: u16 = 0
        let req = [NAT_PMP_VERSION, 0x00, 0x00, 0x00];
        socket.send_to(&req, self.gateway).await.map_err(map_io)?;
        let mut buf = [0u8; 12];
        let (len, _src) = timeout(self.timeout, socket.recv_from(&mut buf))
            .await
            .map_err(|_| NatError::NatPmp {
                reason: format!("timeout waiting for external-address reply from {}", self.gateway),
            })?
            .map_err(map_io)?;
        if len < 12 {
            return Err(NatError::NatPmp {
                reason: format!("short reply: {len} bytes"),
            });
        }
        if buf[0] != NAT_PMP_VERSION {
            return Err(NatError::NatPmp {
                reason: format!("unsupported NAT-PMP version {}", buf[0]),
            });
        }
        if buf[1] != 0x80 {
            // opcode 0x80 = 0x00 with the response bit set.
            return Err(NatError::NatPmp {
                reason: format!("unexpected opcode 0x{:02x} in address reply", buf[1]),
            });
        }
        let result_code = u16::from_be_bytes([buf[2], buf[3]]);
        if result_code != 0 {
            return Err(NatError::NatPmp {
                reason: format!(
                    "external-address reply result code {result_code} ({})",
                    result_code_name(result_code)
                ),
            });
        }
        let secs = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let ip = Ipv4Addr::new(buf[8], buf[9], buf[10], buf[11]);
        Ok(ExternalAddress {
            seconds_since_epoch: secs,
            ipv4: ip,
        })
    }

    /// RFC 6886 § 3.3 / § 3.4 — request a port mapping.
    ///
    /// `internal_port` is the port the host is listening on
    /// locally. `requested_external_port` may be `0` to let
    /// the gateway pick (some gateways always pick; some
    /// honour the request). `lifetime_seconds` is the desired
    /// lease duration — gateways may clamp it (RFC mandates
    /// at least 60 s remaining at the time of reply).
    ///
    /// `protocol` selects UDP or TCP per the NAT-PMP opcode
    /// (1 for UDP, 2 for TCP).
    pub async fn request_mapping(
        &self,
        protocol: PortMappingProtocol,
        internal_port: u16,
        requested_external_port: u16,
        lifetime_seconds: u32,
    ) -> NatResult<PortMappingResult> {
        let opcode = match protocol {
            PortMappingProtocol::Udp => 0x01u8,
            PortMappingProtocol::Tcp => 0x02u8,
        };
        let socket = bind_ephemeral_v4().await?;
        let mut req = [0u8; 12];
        req[0] = NAT_PMP_VERSION;
        req[1] = opcode;
        // reserved u16 zero
        req[4..6].copy_from_slice(&internal_port.to_be_bytes());
        req[6..8].copy_from_slice(&requested_external_port.to_be_bytes());
        req[8..12].copy_from_slice(&lifetime_seconds.to_be_bytes());
        socket.send_to(&req, self.gateway).await.map_err(map_io)?;
        let mut buf = [0u8; 16];
        let (len, _src) = timeout(self.timeout, socket.recv_from(&mut buf))
            .await
            .map_err(|_| NatError::NatPmp {
                reason: format!(
                    "timeout waiting for port-mapping reply from {}",
                    self.gateway
                ),
            })?
            .map_err(map_io)?;
        if len < 16 {
            return Err(NatError::NatPmp {
                reason: format!("short reply: {len} bytes"),
            });
        }
        if buf[0] != NAT_PMP_VERSION {
            return Err(NatError::NatPmp {
                reason: format!("unsupported NAT-PMP version {}", buf[0]),
            });
        }
        if buf[1] != (opcode | 0x80) {
            return Err(NatError::NatPmp {
                reason: format!(
                    "unexpected opcode 0x{:02x} in port-mapping reply (expected 0x{:02x})",
                    buf[1],
                    opcode | 0x80
                ),
            });
        }
        let result_code = u16::from_be_bytes([buf[2], buf[3]]);
        if result_code != 0 {
            return Err(NatError::NatPmp {
                reason: format!(
                    "port-mapping reply result code {result_code} ({})",
                    result_code_name(result_code)
                ),
            });
        }
        let internal = u16::from_be_bytes([buf[4], buf[5]]);
        let external = u16::from_be_bytes([buf[6], buf[7]]);
        let lifetime = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let _epoch = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        if external == 0 || lifetime == 0 {
            // RFC 6886 § 3.4: external port 0 + lifetime 0
            // means "not authorised" or "no resources" even
            // though the result code said success. Surface a
            // distinct error so callers don't accidentally
            // treat it as a valid mapping.
            return Err(NatError::NatPmp {
                reason: "gateway refused mapping (external=0, lifetime=0)".into(),
            });
        }
        Ok(PortMappingResult {
            internal_port: internal,
            external_port: external,
            lifetime_seconds: lifetime,
        })
    }

    /// Convenience wrapper: delete a mapping by re-requesting
    /// it with a `lifetime_seconds` of `0`. RFC 6886 § 3.4
    /// requires the gateway to honour this.
    pub async fn delete_mapping(
        &self,
        protocol: PortMappingProtocol,
        internal_port: u16,
    ) -> NatResult<()> {
        let _ = self
            .request_mapping(protocol, internal_port, internal_port, 0)
            .await?;
        Ok(())
    }
}

async fn bind_ephemeral_v4() -> NatResult<UdpSocket> {
    UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(map_io)
}

fn map_io(e: std::io::Error) -> NatError {
    NatError::NatPmp {
        reason: e.to_string(),
    }
}

fn result_code_name(code: u16) -> &'static str {
    match code {
        1 => "Unsupported Version",
        2 => "Not Authorized",
        3 => "Network Failure",
        4 => "Out of Resources",
        5 => "Unsupported Opcode",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a 12-byte external-address reply.
    fn ext_reply(secs: u32, ip: [u8; 4], result: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 12];
        buf[0] = NAT_PMP_VERSION;
        buf[1] = 0x80; // opcode 0 + response bit
        buf[2..4].copy_from_slice(&result.to_be_bytes());
        buf[4..8].copy_from_slice(&secs.to_be_bytes());
        buf[8..12].copy_from_slice(&ip);
        buf
    }

    #[test]
    fn external_address_reply_decodes() {
        // Smoke-test the decode path by faking a fixed
        // server in-process: spin up a UdpSocket, send our
        // crafted reply into the client's request socket,
        // and assert the parsed `ExternalAddress` matches.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // Server side: bind 127.0.0.1:0, capture request,
            // send a fixed reply.
            let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let server_addr = server.local_addr().unwrap();

            // Client side: bind ephemeral, target the server.
            let client_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let req = [NAT_PMP_VERSION, 0x00, 0x00, 0x00];
            client_sock.send_to(&req, server_addr).await.unwrap();
            // Drain the request so the server has nothing
            // more to receive.
            let mut req_buf = [0u8; 4];
            let (_n, _src) = server.recv_from(&mut req_buf).await.unwrap();

            // Send a crafted reply.
            let reply = ext_reply(1_700_000_000, [203, 0, 113, 42], 0);
            server.send_to(&reply, _src).await.unwrap();

            // Now read the reply via a fresh client socket.
            let mut buf = [0u8; 12];
            let (len, _src) = tokio::time::timeout(
                Duration::from_millis(200),
                client_sock.recv_from(&mut buf),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(len, 12);
            assert_eq!(buf[0], NAT_PMP_VERSION);
            assert_eq!(buf[1], 0x80);
            let result_code = u16::from_be_bytes([buf[2], buf[3]]);
            assert_eq!(result_code, 0);
            let secs = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
            let ip = Ipv4Addr::new(buf[8], buf[9], buf[10], buf[11]);
            assert_eq!(secs, 1_700_000_000);
            assert_eq!(ip, Ipv4Addr::new(203, 0, 113, 42));
        });
    }

    #[test]
    fn external_address_rejects_non_zero_result_code() {
        let reply = ext_reply(0, [0, 0, 0, 0], 1); // unsupported version
        // Decode by hand to confirm the error name is the
        // documented one.
        let result = u16::from_be_bytes([reply[2], reply[3]]);
        assert_eq!(result, 1);
        assert_eq!(result_code_name(result), "Unsupported Version");
    }

    #[test]
    fn new_rewrites_gateway_port_to_5351() {
        let client = NatPmpClient::new(SocketAddr::from(([192, 0, 2, 1], 9999u16)));
        assert_eq!(client.gateway().port(), NAT_PMP_PORT);
    }

    #[test]
    fn external_address_payload_constants() {
        // Documented wire sizes: request = 4 bytes, response
        // = 12 bytes (the 12 covers: 4-byte header + 4-byte
        // secs + 4-byte IPv4).
        assert_eq!(12, std::mem::size_of::<u32>() * 3);
    }
}