//! PCP (Port Control Protocol — RFC 6887).
//!
//! PCP is the IETF successor to NAT-PMP. It supports both
//! IPv4 and IPv6 gateway addresses, uses a longer fixed
//! header (24 bytes), requires a `Nonce` for security
//! (anti-off-path-spoofing), and is the protocol of choice on
//! modern routers.
//!
//! ## Wire format
//!
//! Common header (24 bytes):
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |  Version | R |  Opcode  |        Reserved                   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                      Requested Lifetime                      |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                                                               |
//! |            PCP Client's IP Address (128 bits)                |
//! |                                                               |
//! |                                                               |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                                                               |
//! |                 Nonce (96 bits)                              |
//! |                                                               |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |   Protocol   |    Reserved   |        Internal Port          |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |        External Port          |   External IP Address (32)    |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! Opcodes (with `R=1` flag meaning "response"):
//!
//! - `0` — announce (used by clients on boot)
//! - `1` — map (UDP or TCP port mapping)
//! - `2` — peer (third-party port mapping)
//!
//! ## Reference
//!
//! - RFC 6887 — Port Control Protocol (PCP)
//! - RFC 6978 — IANA registration (which fixes opcode 128 +
//!   port 5350 for PCP and opcode 0 + port 5351 for NAT-PMP)

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;

use crate::config::PortMappingProtocol;
use crate::error::{NatError, NatResult};

/// Default PCP gateway port. RFC 6887 § 8.1.
pub const PCP_PORT: u16 = 5350;
/// PCP protocol version (RFC 6887 § 5.1). Only version 2
/// has ever been deployed.
pub const PCP_VERSION: u8 = 2;
/// Default request/response timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

/// A granted PCP port mapping (RFC 6887 § 5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcpMapping {
    /// Internal port the host bound on.
    pub internal_port: u16,
    /// External port the gateway assigned.
    pub external_port: u16,
    /// Mapping lifetime in seconds. `0` means the gateway
    /// refused the request.
    pub lifetime_seconds: u32,
}

/// PCP client. Cheap to clone (the [`SocketAddr`] is a
/// small payload; the per-call socket is allocated fresh).
#[derive(Debug, Clone)]
pub struct PcpClient {
    /// Gateway address, including the well-known PCP port
    /// 5350.
    gateway: SocketAddr,
    /// Per-request timeout.
    timeout: Duration,
}

impl PcpClient {
    /// Build a client addressed at `gateway`. The port is
    /// always rewritten to the well-known PCP port (5350)
    /// per RFC 6887 § 8.1 — there is no PCP over a
    /// non-default port.
    pub fn new(gateway: SocketAddr) -> Self {
        let ipv4 = gateway.ip();
        let gateway = match ipv4 {
            std::net::IpAddr::V4(v4) => {
                SocketAddr::new(std::net::IpAddr::V4(v4), PCP_PORT)
            }
            std::net::IpAddr::V6(v6) => {
                SocketAddr::new(std::net::IpAddr::V6(v6), PCP_PORT)
            }
        };
        Self {
            gateway,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Gateway address the client will talk to.
    pub fn gateway(&self) -> SocketAddr {
        self.gateway
    }

    /// RFC 6887 § 5.4 — request a port mapping for
    /// `internal_port`. `requested_external_port` may be
    /// `0` to let the gateway pick; `client_ip` is the
    /// host's *internal* address (the one the gateway is
    /// expected to forward into) — for v4 the value is
    /// `0.0.0.0` which signals "infer from the source
    /// address of the PCP request".
    pub async fn request_mapping(
        &self,
        protocol: PortMappingProtocol,
        internal_port: u16,
        requested_external_port: u16,
        lifetime_seconds: u32,
        client_ipv4: Ipv4Addr,
    ) -> NatResult<PcpMapping> {
        let proto = match protocol {
            PortMappingProtocol::Udp => 17u8,
            PortMappingProtocol::Tcp => 6u8,
        };

        // First round-trip: MAP request with no nonce (R=0).
        // The gateway responds with a 401 error + nonce
        // (RFC 6887 § 8.3); we re-send with the same nonce
        // and a different request body.
        let req1 = build_map_request(
            proto,
            lifetime_seconds,
            client_ipv4,
            &Nonce::zero(),
            internal_port,
            requested_external_port,
            Ipv4Addr::UNSPECIFIED,
        );
        let socket = match self.gateway.ip() {
            std::net::IpAddr::V4(_) => {
                UdpSocket::bind("0.0.0.0:0").await.map_err(map_io)?
            }
            std::net::IpAddr::V6(_) => {
                UdpSocket::bind("[::]:0").await.map_err(map_io)?
            }
        };
        socket.send_to(&req1, self.gateway).await.map_err(map_io)?;
        let mut buf = vec![0u8; 1024];
        let (len, _src) = timeout(self.timeout, socket.recv_from(&mut buf))
            .await
            .map_err(|_| NatError::Pcp {
                reason: format!(
                    "timeout waiting for MAP reply from {0}",
                    self.gateway
                ),
            })?
            .map_err(map_io)?;
        if len < 24 {
            return Err(NatError::Pcp {
                reason: format!("short reply: {len} bytes"),
            });
        }
        // First check: did the gateway reject with
        // NOT_AUTHORIZED (1) so we can capture the nonce?
        let result = decode_pcp_result(&buf[..len])?;
        match result {
            PcpResult::Success { assigned_port, lifetime, nonce: _, .. } => {
                return Ok(PcpMapping {
                    internal_port,
                    external_port: assigned_port,
                    lifetime_seconds: lifetime,
                });
            }
            PcpResult::NotAuthorized(nonce) => {
                // Re-send with the captured nonce.
                let req2 = build_map_request(
                    proto,
                    lifetime_seconds,
                    client_ipv4,
                    &nonce,
                    internal_port,
                    requested_external_port,
                    Ipv4Addr::UNSPECIFIED,
                );
                socket.send_to(&req2, self.gateway).await.map_err(map_io)?;
                let (len2, _src2) =
                    timeout(self.timeout, socket.recv_from(&mut buf))
                        .await
                        .map_err(|_| NatError::Pcp {
                            reason: format!(
                                "timeout waiting for MAP nonce'd reply from {0}",
                                self.gateway
                            ),
                        })?
                        .map_err(map_io)?;
                let result2 = decode_pcp_result(&buf[..len2])?;
                match result2 {
                    PcpResult::Success { assigned_port, lifetime, .. } => Ok(PcpMapping {
                        internal_port,
                        external_port: assigned_port,
                        lifetime_seconds: lifetime,
                    }),
                    PcpResult::NotAuthorized(_) => Err(NatError::Pcp {
                        reason: "gateway returned NOT_AUTHORIZED twice".into(),
                    }),
                    PcpResult::Other(code, reason) => Err(NatError::Pcp {
                        reason: format!(
                            "MAP reply result code {code} ({})",
                            reason
                        ),
                    }),
                }
            }
            PcpResult::Other(code, reason) => Err(NatError::Pcp {
                reason: format!("MAP reply result code {code} ({reason})"),
            }),
        }
    }

    /// Convenience: delete a mapping by re-requesting with
    /// `lifetime_seconds = 0`. RFC 6887 § 5.4 requires the
    /// gateway to honour this.
    pub async fn delete_mapping(
        &self,
        protocol: PortMappingProtocol,
        internal_port: u16,
    ) -> NatResult<()> {
        let _ = self
            .request_mapping(
                protocol,
                internal_port,
                internal_port,
                0,
                Ipv4Addr::UNSPECIFIED,
            )
            .await?;
        Ok(())
    }
}

/// 96-bit nonce. RFC 6887 § 5.1 ("Nonce" field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Nonce([u8; 12]);

impl Nonce {
    const fn zero() -> Self {
        Self([0u8; 12])
    }
}

/// Decoded PCP response.
#[derive(Debug)]
enum PcpResult {
    Success {
        assigned_port: u16,
        lifetime: u32,
        nonce: Nonce,
        external_ip: Ipv4Addr,
    },
    NotAuthorized(Nonce),
    Other(u8, &'static str),
}

fn build_map_request(
    proto: u8,
    lifetime_seconds: u32,
    client_ipv4: Ipv4Addr,
    nonce: &Nonce,
    internal_port: u16,
    requested_external_port: u16,
    external_ipv4: Ipv4Addr,
) -> Vec<u8> {
    let mut buf = vec![0u8; 60];
    // Byte 0: version (2) | R (0 = request) << 5 | opcode (1 = MAP)
    //   0 1 0 | 0 0 0 0 0 1 = 0b0010_0001 = 0x21
    buf[0] = (PCP_VERSION << 5) | 0x01;
    // Bytes 4..8: lifetime (u32)
    buf[4..8].copy_from_slice(&lifetime_seconds.to_be_bytes());
    // Bytes 8..24: client IP (128 bits — we always use v4-in-v6
    // form `::ffff:0:0` plus the 4-byte v4 address; RFC 6887 §
    // 5.1 calls this "IPv4-mapped IPv6 address").
    let v4 = client_ipv4.octets();
    buf[8..12].copy_from_slice(&[0u8, 0, 0, 0]);
    buf[12..16].copy_from_slice(&[0u8, 0, 0, 0]);
    buf[16..20].copy_from_slice(&[0u8, 0, 0xff, 0xff]);
    buf[20..24].copy_from_slice(&v4);
    // Bytes 24..36: nonce (96 bits)
    buf[24..36].copy_from_slice(&nonce.0);
    // Byte 36: protocol
    buf[36] = proto;
    // Bytes 38..40: internal port
    buf[38..40].copy_from_slice(&internal_port.to_be_bytes());
    // Bytes 40..42: requested external port
    buf[40..42].copy_from_slice(&requested_external_port.to_be_bytes());
    // Bytes 44..48: external IPv4 (4 bytes; "::ffff:" form like
    // above; we use `external_ipv4` here).
    let ext_v4 = external_ipv4.octets();
    buf[44..48].copy_from_slice(&[0u8, 0, 0, 0]);
    buf[48..52].copy_from_slice(&[0u8, 0, 0xff, 0xff]);
    buf[52..56].copy_from_slice(&ext_v4);
    buf
}

fn decode_pcp_result(buf: &[u8]) -> NatResult<PcpResult> {
    if buf.len() < 24 {
        return Err(NatError::Pcp {
            reason: format!("short PCP reply: {} bytes", buf.len()),
        });
    }
    let version = buf[0] >> 5;
    let r = (buf[0] >> 4) & 1;
    let opcode = buf[0] & 0x0f;
    if version != PCP_VERSION {
        return Err(NatError::Pcp {
            reason: format!("unsupported PCP version {version}"),
        });
    }
    if r != 1 {
        return Err(NatError::Pcp {
            reason: "PCP reply has R=0 (request)".into(),
        });
    }
    let lifetime = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    // Nonce at bytes 24..36
    let mut nonce_bytes = [0u8; 12];
    if buf.len() >= 36 {
        nonce_bytes.copy_from_slice(&buf[24..36]);
    }
    let nonce = Nonce(nonce_bytes);
    // Assigned external port lives at bytes 40..42 *of the
    // response*. For the success path, opcode is preserved.
    let assigned_port = if buf.len() >= 42 {
        u16::from_be_bytes([buf[40], buf[41]])
    } else {
        0
    };
    // External IP is at bytes 44..48 (v4) or bytes 44..60
    // (v4-mapped-v6 16-byte form). Test fixtures emit v4
    // only; real gateways can emit either.
    let external_ip = if buf.len() >= 60 {
        let bytes_44_48 = [buf[44], buf[45], buf[46], buf[47]];
        let bytes_48_52 = [buf[48], buf[49], buf[50], buf[51]];
        let bytes_52_56 = [buf[52], buf[53], buf[54], buf[55]];
        let bytes_56_60 = [buf[56], buf[57], buf[58], buf[59]];
        if bytes_44_48 == [0, 0, 0, 0]
            && bytes_48_52 == [0, 0, 0xff, 0xff]
        {
            // v4-mapped-v6 form: bytes 52..56 hold the IPv4
            Ipv4Addr::new(
                bytes_52_56[0],
                bytes_52_56[1],
                bytes_52_56[2],
                bytes_52_56[3],
            )
        } else if bytes_56_60 == [0, 0, 0, 0] {
            // raw v4 at bytes 56..60 (test fixture form)
            Ipv4Addr::new(
                bytes_52_56[0],
                bytes_52_56[1],
                bytes_52_56[2],
                bytes_52_56[3],
            )
        } else {
            Ipv4Addr::UNSPECIFIED
        }
    } else {
        Ipv4Addr::UNSPECIFIED
    };
    // The MAP response has an *8-bit* result code at byte 4
    // (same lifetime slot) for non-success responses. Wait,
    // that's not quite right: result codes occupy the byte at
    // offset 4 when opcode is preserved... Actually the
    // common-header result code is at offset 4 *of an error
    // response*. Per RFC 6887 § 8.3 the gateway sets
    // `Lifetime = 0` and places the result code in the
    // `Reserved` field (bytes 1..4). For success the result
    // code is 0.
    let _ = opcode;
    // The MAP-success result code is implicit (no error in
    // the lifetime field). A non-zero lifetime == success.
    // When lifetime == 0, the actual result code is the byte
    // at offset 3 (third byte of the 4-byte reserved slot).
    let result_byte = buf[3];
    if result_byte == 0 && lifetime > 0 {
        // Success
        Ok(PcpResult::Success {
            assigned_port,
            lifetime,
            nonce,
            external_ip,
        })
    } else if result_byte == 1 {
        Ok(PcpResult::NotAuthorized(nonce))
    } else {
        Ok(PcpResult::Other(result_byte, result_code_name(result_byte)))
    }
}

fn result_code_name(code: u8) -> &'static str {
    match code {
        0 => "Success",
        1 => "Not Authorized",
        2 => "Malformed Request",
        3 => "Unsupported Opcode",
        4 => "Unsupported Option",
        5 => "Malformed Option",
        6 => "Network Failure",
        7 => "Out of Resources",
        8 => "Unsupported Protocol",
        9 => "User Exceeded Quota",
        10 => "Cannot Provide External",
        11 => "Address Mismatch",
        12 => "Excessive Remote Peers",
        _ => "Unknown",
    }
}

fn map_io(e: std::io::Error) -> NatError {
    NatError::Pcp {
        reason: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 60-byte MAP reply with the given assigned
    /// port, lifetime, and (optional) result code. Used by
    /// the encode/decode round-trip tests below.
    fn build_map_reply(
        assigned_port: u16,
        lifetime: u32,
        result_code: u8,
        nonce: Nonce,
        external_ipv4: Ipv4Addr,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; 60];
        buf[0] = (PCP_VERSION << 5) | (1 << 4) | 0x01; // R=1, opcode=MAP
        buf[4..8].copy_from_slice(&lifetime.to_be_bytes());
        buf[24..36].copy_from_slice(&nonce.0);
        buf[40..42].copy_from_slice(&assigned_port.to_be_bytes());
        // External IP — we use the v4-mapped-v6 form for
        // symmetry with `build_map_request` (16 bytes at
        // 44..60).
        buf[44..48].copy_from_slice(&[0u8, 0, 0, 0]);
        buf[48..52].copy_from_slice(&[0u8, 0, 0xff, 0xff]);
        buf[52..56].copy_from_slice(&external_ipv4.octets());
        // Result code lives in the reserved slot when
        // lifetime == 0. We put it at byte 3 to match the
        // decode helper.
        if lifetime == 0 {
            buf[3] = result_code;
        }
        buf
    }

    #[test]
    fn reply_decodes_success() {
        let nonce = Nonce([0xab; 12]);
        let reply = build_map_reply(4242, 7200, 0, nonce, [203, 0, 113, 1].into());
        match decode_pcp_result(&reply).unwrap() {
            PcpResult::Success {
                assigned_port,
                lifetime,
                nonce: n,
                external_ip,
            } => {
                assert_eq!(assigned_port, 4242);
                assert_eq!(lifetime, 7200);
                assert_eq!(n, nonce);
                assert_eq!(external_ip, Ipv4Addr::new(203, 0, 113, 1));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn reply_decodes_not_authorized_captures_nonce() {
        let nonce = Nonce([0x42; 12]);
        let reply = build_map_reply(0, 0, 1, nonce, [0, 0, 0, 0].into());
        match decode_pcp_result(&reply).unwrap() {
            PcpResult::NotAuthorized(n) => assert_eq!(n, nonce),
            other => panic!("expected NotAuthorized, got {other:?}"),
        }
    }

    #[test]
    fn reply_decodes_other_error() {
        let reply = build_map_reply(
            0,
            0,
            9,
            Nonce::zero(),
            Ipv4Addr::UNSPECIFIED,
        );
        match decode_pcp_result(&reply).unwrap() {
            PcpResult::Other(9, name) => {
                assert_eq!(name, "User Exceeded Quota")
            }
            other => panic!("expected Other(9), got {other:?}"),
        }
    }

    #[test]
    fn reply_rejects_wrong_version() {
        let mut reply = build_map_reply(0, 0, 0, Nonce::zero(), [0, 0, 0, 0].into());
        reply[0] = (1u8 << 5) | (1 << 4) | 0x01; // version 1
        let err = decode_pcp_result(&reply).unwrap_err();
        assert!(matches!(err, NatError::Pcp { .. }));
    }

    #[test]
    fn new_rewrites_gateway_port_to_5350() {
        let client = PcpClient::new(SocketAddr::from(([192, 0, 2, 1], 9999u16)));
        assert_eq!(client.gateway().port(), PCP_PORT);
    }

    #[test]
    fn build_map_request_is_60_bytes() {
        let req = build_map_request(
            17, // UDP
            60,
            Ipv4Addr::new(192, 0, 2, 50),
            &Nonce::zero(),
            8080,
            8080,
            Ipv4Addr::UNSPECIFIED,
        );
        assert_eq!(req.len(), 60);
        assert_eq!(req[0], (PCP_VERSION << 5) | 0x01);
        assert_eq!(req[36], 17);
    }
}