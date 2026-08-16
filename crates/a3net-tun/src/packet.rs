//! Minimal IP packet parser/builder.
//!
//! The mesh stack needs to peek at the IPv4/IPv6 header of
//! every packet that crosses the TUN device so that:
//!
//! - The userspace firewall can match on source / destination
//!   without re-implementing iptables.
//! - The exit-node can route by destination address.
//! - The MTU enforcement can decide whether to fragment or
//!   drop.
//!
//! We deliberately implement only what we need: the version
//! field, the source/destination addresses, and the
//! `next header` / `protocol` byte. Everything else is
//! passed through opaquely.
//!
//! The parser is `no_std`-friendly apart from the `Vec<u8>`
//! payload. It does **not** allocate for header parsing —
//! the only allocation is the original packet buffer.

use crate::error::{TunError, TunResult};

/// Minimum size of an IPv4 header (no options).
pub const IPV4_HEADER_MIN: usize = 20;
/// Size of an IPv6 header (no extension headers).
pub const IPV6_HEADER_MIN: usize = 40;

/// IP version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpVersion {
    V4,
    V6,
}

impl IpVersion {
    pub fn header_min(self) -> usize {
        match self {
            Self::V4 => IPV4_HEADER_MIN,
            Self::V6 => IPV6_HEADER_MIN,
        }
    }
}

/// IP next-header / protocol number.
///
/// We expose only the values the firewall / routing layers
/// care about. Everything else is reported as
/// [`IpProtocol::Other`] so a future ICMPv6 / SCTP / etc.
/// rule can plug in without breaking the parse path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpProtocol {
    Icmp,
    Tcp,
    Udp,
    Icmpv6,
    Other(u8),
}

impl IpProtocol {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Icmp => 1,
            Self::Tcp => 6,
            Self::Udp => 17,
            Self::Icmpv6 => 58,
            Self::Other(n) => n,
        }
    }
}

/// Parsed view of an IP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPacket {
    pub version: IpVersion,
    pub protocol: IpProtocol,
    /// Source IP, in network byte order.
    pub src: [u8; 16],
    /// Destination IP, in network byte order.
    pub dst: [u8; 16],
    /// Header length (bytes). For IPv4 this can exceed
    /// [`IPV4_HEADER_MIN`] when options are present.
    pub header_len: usize,
    /// Total packet length (header + payload).
    pub total_len: usize,
}

impl ParsedPacket {
    /// Source IPv4, valid only when `version == V4`.
    pub fn src_v4(&self) -> [u8; 4] {
        self.src[..4].try_into().expect("v4 src has 4 bytes")
    }

    /// Destination IPv4, valid only when `version == V4`.
    pub fn dst_v4(&self) -> [u8; 4] {
        self.dst[..4].try_into().expect("v4 dst has 4 bytes")
    }

    /// Source IPv6, valid only when `version == V6`.
    pub fn src_v6(&self) -> [u8; 16] {
        self.src
    }

    /// Destination IPv6, valid only when `version == V6`.
    pub fn dst_v6(&self) -> [u8; 16] {
        self.dst
    }

    /// Payload slice (everything after the IP header).
    pub fn payload<'a>(&self, raw: &'a [u8]) -> &'a [u8] {
        &raw[self.header_len..self.total_len.min(raw.len())]
    }
}

/// Parse an IP packet (IPv4 or IPv6) into a [`ParsedPacket`].
///
/// Returns [`TunError::InvalidPacket`] for any malformed
/// frame. The packet buffer is **not** mutated; the caller
/// can pass the original bytes through the firewall and
/// only build a new buffer on a forward.
pub fn parse_packet(raw: &[u8]) -> TunResult<ParsedPacket> {
    if raw.is_empty() {
        return Err(TunError::InvalidPacket("empty buffer".into()));
    }
    let version_byte = raw[0] >> 4;
    match version_byte {
        4 => parse_v4(raw),
        6 => parse_v6(raw),
        v => Err(TunError::InvalidPacket(format!(
            "unsupported IP version {v}"
        ))),
    }
}

fn parse_v4(raw: &[u8]) -> TunResult<ParsedPacket> {
    if raw.len() < IPV4_HEADER_MIN {
        return Err(TunError::InvalidPacket(format!(
            "IPv4 header truncated: {} bytes",
            raw.len()
        )));
    }
    let ihl_words = (raw[0] & 0x0f) as usize;
    let header_len = ihl_words * 4;
    if header_len < IPV4_HEADER_MIN || header_len > raw.len() {
        return Err(TunError::InvalidPacket(format!(
            "IPv4 IHL {ihl_words} (header_len={header_len}) out of range"
        )));
    }
    let total_len = u16::from_be_bytes([raw[2], raw[3]]) as usize;
    if total_len < header_len || total_len > raw.len() {
        return Err(TunError::InvalidPacket(format!(
            "IPv4 total_len {total_len} inconsistent with buffer {}",
            raw.len()
        )));
    }
    let protocol = match raw[9] {
        1 => IpProtocol::Icmp,
        6 => IpProtocol::Tcp,
        17 => IpProtocol::Udp,
        n => IpProtocol::Other(n),
    };
    let mut src = [0u8; 16];
    let mut dst = [0u8; 16];
    src[..4].copy_from_slice(&raw[12..16]);
    dst[..4].copy_from_slice(&raw[16..20]);
    Ok(ParsedPacket {
        version: IpVersion::V4,
        protocol,
        src,
        dst,
        header_len,
        total_len,
    })
}

fn parse_v6(raw: &[u8]) -> TunResult<ParsedPacket> {
    if raw.len() < IPV6_HEADER_MIN {
        return Err(TunError::InvalidPacket(format!(
            "IPv6 header truncated: {} bytes",
            raw.len()
        )));
    }
    let payload_len = u16::from_be_bytes([raw[4], raw[5]]) as usize;
    let total_len = IPV6_HEADER_MIN + payload_len;
    if total_len > raw.len() {
        return Err(TunError::InvalidPacket(format!(
            "IPv6 payload_len {payload_len} (total {total_len}) > buffer {}",
            raw.len()
        )));
    }
    let protocol = match raw[6] {
        6 => IpProtocol::Tcp,
        17 => IpProtocol::Udp,
        58 => IpProtocol::Icmpv6,
        n => IpProtocol::Other(n),
    };
    let mut src = [0u8; 16];
    let mut dst = [0u8; 16];
    src.copy_from_slice(&raw[8..24]);
    dst.copy_from_slice(&raw[24..40]);
    Ok(ParsedPacket {
        version: IpVersion::V6,
        protocol,
        src,
        dst,
        header_len: IPV6_HEADER_MIN,
        total_len,
    })
}

/// Re-emit the bytes of a parsed packet (used by the firewall
/// when it modifies only the header bytes and wants to write
/// the rest through unchanged). This is a passthrough that
/// verifies the parsed view is consistent with the raw bytes.
pub fn packet_to_bytes(raw: &[u8]) -> TunResult<Vec<u8>> {
    let parsed = parse_packet(raw)?;
    if parsed.total_len > raw.len() {
        return Err(TunError::InvalidPacket(format!(
            "parsed total_len {} exceeds buffer {}",
            parsed.total_len,
            raw.len()
        )));
    }
    Ok(raw[..parsed.total_len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built IPv4 header: `ver=4, ihl=5, total_len=44,
    /// proto=TCP, src=100.64.0.5, dst=100.64.0.7`. Payload
    /// is 24 zero bytes — enough to assert header parse but
    /// not a valid TCP frame.
    fn fake_ipv4_tcp_packet() -> Vec<u8> {
        let mut p = vec![0u8; 44];
        p[0] = 0x45; // version 4, IHL 5
        p[2] = (44u16 >> 8) as u8;
        p[3] = (44u16 & 0xff) as u8;
        p[9] = 6; // TCP
        p[12..16].copy_from_slice(&[100, 64, 0, 5]);
        p[16..20].copy_from_slice(&[100, 64, 0, 7]);
        p
    }

    /// Hand-built IPv6 header: `ver=6, payload_len=24,
    /// next=TCP, src=200::5, dst=200::7`. Payload 24 zero
    /// bytes.
    fn fake_ipv6_tcp_packet() -> Vec<u8> {
        let mut p = vec![0u8; 40 + 24];
        p[0] = 0x60; // version 6
        p[4] = (24u16 >> 8) as u8;
        p[5] = (24u16 & 0xff) as u8;
        p[6] = 6; // TCP
        // src = 200::5 (0x02 0x00 ... 0x05)
        p[8] = 0x02;
        p[9] = 0x00;
        p[23] = 0x05;
        // dst = 200::7
        p[24] = 0x02;
        p[25] = 0x00;
        p[39] = 0x07;
        p
    }

    #[test]
    fn parse_empty_buffer_errors() {
        assert!(parse_packet(&[]).is_err());
    }

    #[test]
    fn parse_unknown_version_errors() {
        let p = vec![0x70u8; 20];
        assert!(parse_packet(&p).is_err());
    }

    #[test]
    fn parse_ipv4_tcp() {
        let pkt = fake_ipv4_tcp_packet();
        let p = parse_packet(&pkt).unwrap();
        assert_eq!(p.version, IpVersion::V4);
        assert_eq!(p.protocol, IpProtocol::Tcp);
        assert_eq!(p.src_v4(), [100, 64, 0, 5]);
        assert_eq!(p.dst_v4(), [100, 64, 0, 7]);
        assert_eq!(p.header_len, 20);
        assert_eq!(p.total_len, 44);
    }

    #[test]
    fn parse_ipv4_truncated_header_errors() {
        let pkt = vec![0x45u8, 10];
        assert!(parse_packet(&pkt).is_err());
    }

    #[test]
    fn parse_ipv4_total_len_too_large_errors() {
        let mut pkt = vec![0u8; 20];
        pkt[0] = 0x45;
        pkt[2] = 0xff;
        pkt[3] = 0xff;
        assert!(parse_packet(&pkt).is_err());
    }

    #[test]
    fn parse_ipv4_with_options() {
        // IHL=6 → 24-byte header. total_len=24+8 = 32.
        let mut pkt = vec![0u8; 32];
        pkt[0] = 0x46;
        pkt[2] = 0x00;
        pkt[3] = 32;
        pkt[9] = 17; // UDP
        pkt[12..16].copy_from_slice(&[100, 64, 0, 1]);
        pkt[16..20].copy_from_slice(&[100, 64, 0, 2]);
        let p = parse_packet(&pkt).unwrap();
        assert_eq!(p.header_len, 24);
        assert_eq!(p.protocol, IpProtocol::Udp);
    }

    #[test]
    fn parse_ipv4_other_protocol() {
        let mut pkt = vec![0u8; 20];
        pkt[0] = 0x45;
        pkt[2] = 0x00;
        pkt[3] = 20;
        pkt[9] = 132; // SCTP
        let p = parse_packet(&pkt).unwrap();
        assert_eq!(p.protocol, IpProtocol::Other(132));
    }

    #[test]
    fn parse_ipv6_tcp() {
        let pkt = fake_ipv6_tcp_packet();
        let p = parse_packet(&pkt).unwrap();
        assert_eq!(p.version, IpVersion::V6);
        assert_eq!(p.protocol, IpProtocol::Tcp);
        assert_eq!(p.header_len, 40);
        assert_eq!(p.total_len, 64);
        assert_eq!(p.src_v6()[0], 0x02);
        assert_eq!(p.dst_v6()[0], 0x02);
    }

    #[test]
    fn parse_ipv6_truncated_header_errors() {
        let pkt = vec![0x60u8; 10];
        assert!(parse_packet(&pkt).is_err());
    }

    #[test]
    fn parse_ipv6_payload_too_large_errors() {
        let mut pkt = vec![0u8; 40];
        pkt[0] = 0x60;
        pkt[5] = 0xff; // payload_len > buffer
        assert!(parse_packet(&pkt).is_err());
    }

    #[test]
    fn parsed_payload_returns_correct_slice() {
        let pkt = fake_ipv4_tcp_packet();
        let parsed = parse_packet(&pkt).unwrap();
        let payload = parsed.payload(&pkt);
        assert_eq!(payload.len(), 24);
    }

    #[test]
    fn packet_to_bytes_roundtrips() {
        let pkt = fake_ipv4_tcp_packet();
        let bytes = packet_to_bytes(&pkt).unwrap();
        assert_eq!(bytes, pkt);
    }

    #[test]
    fn packet_to_bytes_rejects_truncated() {
        let pkt = vec![0x45u8; 20];
        let err = packet_to_bytes(&pkt);
        // total_len=0 but header_len=20 → invalid
        assert!(err.is_err());
    }
}
