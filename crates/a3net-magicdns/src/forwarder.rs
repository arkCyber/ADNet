//! DNS packet forwarder for the TUN interface.
//!
//! Intercepts DNS queries that arrive as UDP/53 packets on
//! the TUN device and resolves `.ray` / `.a3net` names
//! locally. All other packets are passed through unchanged.
//!
//! ## Architecture
//!
//! ```text
//!  kernel → TUN device
//!             │
//!             ▼ packet
//!     ┌──────────────────────┐
//!     │   TunDnsForwarder    │  (this module)
//!     │                      │
//!     │  Is this UDP/53 to   │──── YES ──► parse name
//!     │  mesh TLD?          │                │
//!     │                      │         Is it .ray/.a3net?
//!     │                      │                │       │
//!     │                      │         NO ◄───┘       │ YES
//!     │                      │         │              ▼
//!     │                      │         │    resolve via Resolver
//!     │                      │         │              │
//!     │                      │         │         inject response
//!     │                      │         │         into TUN
//!     │                      │         │              │
//!     └──────────────────────┘         └───────► forward upstream
//!                                              (if upstreams configured)
//! ```
//!
//! ## Interaction with the mesh packet loop
//!
//! The forwarder is a **layer** in the packet processing
//! pipeline, not a standalone service. The typical usage is:
//!
//! ```ignore
//! let forwarder = TunDnsForwarder::new(resolver.clone(), config.clone());
//!
//! loop {
//!     let pkt = tun.recv().await?;
//!     let parsed = parse_packet(&pkt)?;
//!
//!     // Short-circuit: let the forwarder handle DNS.
//!     if let Some(out) = forwarder.maybe_handle(&pkt, &parsed)? {
//!         tun.send(out).await?;
//!         continue;
//!     }
//!
//!     // Not a DNS packet — pass to the rest of the
//!     // firewall / router stack.
//!     handle_packet(pkt, parsed).await?;
//! }
//! ```
//!
//! `maybe_handle` is synchronous and lock-free — it returns
//! `None` for non-mesh-DNS packets in O(1) time.

use std::net::Ipv4Addr;

use a3net_types::VirtualIp;
use a3net_tun::packet::{IpProtocol, IpVersion, ParsedPacket};
use tracing::debug;

use crate::config::ResolverConfig;
use crate::error::MagicResult;
use crate::Resolver;

/// The standard DNS port.
const DNS_PORT: u16 = 53;

/// Magic DNS forwarder that intercepts `.ray` / `.a3net` DNS
/// queries from the TUN interface.
///
/// Construct with [`TunDnsForwarder::new`] and call
/// [`maybe_handle`](Self::maybe_handle) on each received
/// packet. The method returns:
///
/// - `Ok(Some(response_bytes))` — the packet was a mesh DNS
///   query and a response was generated. The caller **must**
///   write the response back to the TUN.
/// - `Ok(None)` — the packet was not a mesh DNS query and
///   should be processed normally by the caller.
/// - `Err` — the packet looked like a DNS query but was
///   malformed. The packet is dropped; nothing is written.
///
/// The forwarder does **not** own the [`Resolver`] — the
/// caller manages the resolver's lifetime and `apply_roster`
/// calls. A single forwarder can be shared across multiple
/// tasks via `Clone` (it's `Arc`-backed internally).
#[derive(Clone)]
pub struct TunDnsForwarder {
    pub(crate) resolver: Resolver,
    pub(crate) config: ResolverConfig,
}

impl TunDnsForwarder {
    /// Build a new forwarder backed by the given resolver.
    pub fn new(resolver: Resolver, config: ResolverConfig) -> Self {
        Self { resolver, config }
    }

    /// Attempt to handle a packet as a mesh DNS query.
    ///
    /// Returns `Ok(Some(response_bytes))` if the packet was a
    /// DNS query for a mesh TLD and a response was generated.
    /// Returns `Ok(None)` if the packet is not a mesh DNS
    /// query. Returns `Err` on malformed input (log and drop).
    pub fn maybe_handle(
        &self,
        raw: &[u8],
        parsed: &ParsedPacket,
    ) -> ForwarderResult<Option<Vec<u8>>> {
        // Only handle IPv4 UDP/53 packets.
        if !Self::is_dns_packet(parsed) {
            return Ok(None);
        }

        // DNS queries have dst port 53. Check the UDP dst port
        // from the raw bytes (IP header_len offset + 2).
        let dst_port = Self::read_udp_port(raw, parsed.header_len + 2);
        if dst_port != DNS_PORT {
            return Ok(None);
        }

        // UDP src port is at header_len + 0.
        let src_port = Self::read_udp_port(raw, parsed.header_len);

        // Extract DNS payload (after IP + UDP headers).
        let dns_start = parsed.header_len + 8;
        if dns_start >= raw.len() {
            return Ok(None);
        }
        let dns_payload = &raw[dns_start..];

        // Parse the query name.
        let qname = match Self::extract_qname(dns_payload) {
            Some(n) => n,
            None => {
                debug!(src = ?Self::fmt_ip(&parsed.src), "DNS: failed to extract qname");
                return Ok(None);
            }
        };

        // Check if it's a mesh TLD.
        if !self.check_mesh_tld(&qname) {
            debug!(qname = %qname, "DNS: not a mesh TLD, passthrough");
            return Ok(None);
        }

        debug!(qname = %qname, src = ?Self::fmt_ip(&parsed.src), "DNS: mesh query intercepted");

        // Attempt resolution.
        let vip = self.resolve_mesh_query(&qname);

        // Extract the QTYPE from the question section.
        let qtype = Self::extract_qtype(dns_payload).unwrap_or(1);

        let answer = match vip {
            Ok(vip) => self.build_answer(dns_payload, &qname, vip, qtype),
            Err(_) => self.build_nxdomain(dns_payload, &qname),
        };

        // Build the IPv4/UDP response packet.
        let out = self.wrap_in_ip(answer, parsed, src_port)?;
        Ok(Some(out))
    }

    /// Returns `true` if `parsed` is an IPv4 UDP packet.
    fn is_dns_packet(parsed: &ParsedPacket) -> bool {
        parsed.version == IpVersion::V4 && parsed.protocol == IpProtocol::Udp
    }

    /// Read a 16-bit port from `raw` at the given offset.
    fn read_udp_port(raw: &[u8], offset: usize) -> u16 {
        if offset + 2 > raw.len() {
            return 0;
        }
        u16::from_be_bytes([raw[offset], raw[offset + 1]])
    }

    /// Extract the queried name (QNAME) from a DNS question
    /// section. Returns `None` if the payload is too short.
    fn extract_qname(payload: &[u8]) -> Option<String> {
        let mut labels = Vec::new();
        let mut i = 0;

        loop {
            if i >= payload.len() {
                return None;
            }
            let len = payload[i] as usize;
            i += 1;

            if len == 0 {
                break;
            }

            if i + len > payload.len() {
                return None;
            }

            let label = std::str::from_utf8(&payload[i..i + len]).ok()?;
            labels.push(label.to_lowercase());
            i += len;
        }

        if labels.is_empty() {
            return None;
        }
        Some(labels.join("."))
    }

    /// Extract the QTYPE from a DNS question section.
    /// QTYPE starts after the QNAME null terminator.
    fn extract_qtype(payload: &[u8]) -> Option<u16> {
        let mut i = 0;
        loop {
            if i >= payload.len() {
                return None;
            }
            let len = payload[i] as usize;
            i += 1;
            if len == 0 {
                break;
            }
            i += len;
        }
        if i + 4 > payload.len() {
            return None;
        }
        Some(u16::from_be_bytes([payload[i], payload[i + 1]]))
    }

    /// Check if a qname ends with a mesh TLD.
    fn check_mesh_tld(&self, qname: &str) -> bool {
        let labels: Vec<&str> = qname.rsplit('.').collect();
        let tld = labels.first().copied().unwrap_or("");
        self.config.is_mesh_tld(tld)
    }

    /// Attempt to resolve a mesh query via the resolver.
    fn resolve_mesh_query(&self, qname: &str) -> MagicResult<VirtualIp> {
        self.resolver.resolve_str(qname, None)
    }

    /// Build a DNS response (A or AAAA) for a resolved virtual IP.
    fn build_answer(
        &self,
        question: &[u8],
        qname: &str,
        vip: VirtualIp,
        qtype: u16,
    ) -> Vec<u8> {
        let mut response = Self::build_response_header(question, false);

        // Question section.
        Self::write_qname(&mut response, qname);
        response.extend_from_slice(&qtype.to_be_bytes());
        response.extend_from_slice(&1u16.to_be_bytes()); // CLASS IN

        // Answer section.
        Self::write_qname(&mut response, qname);
        response.extend_from_slice(&qtype.to_be_bytes());
        response.extend_from_slice(&1u16.to_be_bytes()); // CLASS IN
        response.extend_from_slice(&self.config.dns_ttl_secs.to_be_bytes());

        let mut ancount: u16 = 1;

        match qtype {
            1 => {
                // A record — 4 bytes IPv4
                response.extend_from_slice(&4u16.to_be_bytes());
                response.extend_from_slice(&vip.ipv4.as_std().octets());
            }
            28 => {
                // AAAA record — 16 bytes IPv6
                response.extend_from_slice(&16u16.to_be_bytes());
                response.extend_from_slice(&vip.ipv6.as_std().octets());
            }
            _ => {
                // No answer for other types.
                ancount = 0;
            }
        }

        // Update ANCOUNT in header (bytes 6-7).
        if response.len() >= 8 {
            response[6..8].copy_from_slice(&ancount.to_be_bytes());
        }
        response
    }

    fn build_nxdomain(&self, question: &[u8], qname: &str) -> Vec<u8> {
        let mut response = Self::build_response_header(question, true);
        Self::write_qname(&mut response, qname);
        response.extend_from_slice(&1u16.to_be_bytes()); // QTYPE
        response.extend_from_slice(&1u16.to_be_bytes()); // QCLASS
        response
    }

    fn build_response_header(question: &[u8], nxdomain: bool) -> Vec<u8> {
        let mut h = Vec::with_capacity(12);
        // Transaction ID: reuse the question's ID.
        let id = if question.len() >= 2 {
            u16::from_be_bytes([question[0], question[1]])
        } else {
            0
        };
        h.extend_from_slice(&id.to_be_bytes());

        // Flags: QR=1, AA=1 (authoritative), RD=1, RA=1
        // RCODE=0 (NOERROR) or 3 (NXDOMAIN)
        let flags = if nxdomain { 0x8183u16 } else { 0x8184u16 };
        h.extend_from_slice(&flags.to_be_bytes());

        h.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT = 1
        h.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT (filled by caller)
        h.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        h.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
        h
    }

    fn write_qname(out: &mut Vec<u8>, name: &str) {
        for label in name.split('.') {
            if label.is_empty() {
                continue;
            }
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
    }

    /// Wrap a DNS payload in an IPv4/UDP header addressed from
    /// the local mesh IP to the original sender at `src_port`.
    fn wrap_in_ip(
        &self,
        dns_response: Vec<u8>,
        original: &ParsedPacket,
        original_src_port: u16,
    ) -> ForwarderResult<Vec<u8>> {
        let dns_len = dns_response.len();
        let udp_len = 8 + dns_len;
        let total = 20 + udp_len;
        if total > 65535 {
            return Err(ForwarderError::PacketTooLarge(total));
        }

        let src_ip = self.local_ipv4();
        let dst_ip = Ipv4Addr::from(original.src_v4());

        let mut pkt = vec![0u8; total];

        // IPv4 header.
        pkt[0] = 0x45;
        pkt[2] = ((total >> 8) & 0xff) as u8;
        pkt[3] = (total & 0xff) as u8;
        pkt[8] = 64; // TTL
        pkt[9] = 17; // UDP
        pkt[12..16].copy_from_slice(&src_ip.octets());
        pkt[16..20].copy_from_slice(&dst_ip.octets());

        // IPv4 header checksum.
        let checksum = Self::ip_checksum(&pkt[..20]);
        pkt[10] = ((checksum >> 8) & 0xff) as u8;
        pkt[11] = (checksum & 0xff) as u8;

        // UDP header: src=53, dst=original_src_port, len, checksum=0.
        let ip_end = 20;
        pkt[ip_end + 0] = ((DNS_PORT >> 8) & 0xff) as u8;
        pkt[ip_end + 1] = (DNS_PORT & 0xff) as u8;
        pkt[ip_end + 2] = ((original_src_port >> 8) & 0xff) as u8;
        pkt[ip_end + 3] = (original_src_port & 0xff) as u8;
        pkt[ip_end + 4] = ((udp_len >> 8) & 0xff) as u8;
        pkt[ip_end + 5] = (udp_len & 0xff) as u8;
        // Bytes 26-27: UDP checksum (0 = disabled for IPv4).

        // DNS response payload.
        pkt[ip_end + 8..].copy_from_slice(&dns_response);

        Ok(pkt)
    }

    /// Build a DNS A/AAAA answer for a resolved virtual IP and
    /// a specific QTYPE.
    pub fn build_dns_response_for_type(
        &self,
        question: &[u8],
        qname: &str,
        vip: VirtualIp,
        qtype: u16,
    ) -> Vec<u8> {
        self.build_answer(question, qname, vip, qtype)
    }

    /// Build an NXDOMAIN response for a failed resolution.
    pub fn build_nxdomain_response(
        &self,
        question: &[u8],
        qname: &str,
    ) -> Vec<u8> {
        self.build_nxdomain(question, qname)
    }

    /// Returns `true` if the given qname ends with a mesh TLD.
    pub fn is_mesh_query(&self, qname: &str) -> bool {
        let labels: Vec<&str> = qname.rsplit('.').collect();
        let tld = labels.first().copied().unwrap_or("");
        self.config.is_mesh_tld(tld)
    }

    fn local_ipv4(&self) -> Ipv4Addr {
        self.config
            .local_ipv4
            .unwrap_or_else(|| Ipv4Addr::new(100, 64, 0, 1))
    }

    /// Compute the RFC 1071 IPv4 header checksum.
    fn ip_checksum(header: &[u8]) -> u16 {
        let mut sum: u32 = 0;
        for i in (0..header.len()).step_by(2) {
            let word = if i + 1 < header.len() {
                u16::from_be_bytes([header[i], header[i + 1]])
            } else {
                u16::from_be_bytes([header[i], 0])
            };
            sum += u32::from(word);
        }
        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        !sum as u16
    }

    fn fmt_ip(src: &[u8; 16]) -> String {
        let v4 = Ipv4Addr::from(*&src[..4].try_into().unwrap_or([0u8; 4]));
        format!("{}", v4)
    }
}

/// Result alias for forwarder operations.
pub type ForwarderResult<T> = std::result::Result<T, ForwarderError>;

#[derive(Debug, thiserror::Error)]
pub enum ForwarderError {
    #[error("packet too large: {0} bytes")]
    PacketTooLarge(usize),

    #[error("invalid IP packet: {0}")]
    InvalidPacket(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_tun::packet::parse_packet;

    fn cfg() -> ResolverConfig {
        ResolverConfig::default()
    }

    /// Build a minimal DNS A query packet (IPv4 + UDP + DNS wire).
    fn make_dns_query_packet(qname: &str, qtype: u16) -> Vec<u8> {
        let qname_bytes: Vec<u8> = qname
            .split('.')
            .flat_map(|label| {
                std::iter::once(label.len() as u8)
                    .chain(label.as_bytes().iter().copied())
            })
            .chain(std::iter::once(0))
            .collect();

        let dns_len = 12 + qname_bytes.len() + 4; // header + qname + qtype/qclass
        let udp_len = 8 + dns_len;
        let total_len = 20 + udp_len;

        let mut ip = vec![0u8; total_len];
        ip[0] = 0x45;
        ip[2] = ((total_len >> 8) & 0xff) as u8;
        ip[3] = (total_len & 0xff) as u8;
        ip[8] = 64;
        ip[9] = 17; // UDP
        ip[12..16].copy_from_slice(&[100, 64, 0, 10]); // src
        ip[16..20].copy_from_slice(&[100, 64, 0, 1]); // dst

        // UDP header at offset 20.
        ip[20] = ((54321 >> 8) & 0xff) as u8; // src port
        ip[21] = (54321 & 0xff) as u8;
        ip[22] = ((DNS_PORT >> 8) & 0xff) as u8; // dst port = 53
        ip[23] = (DNS_PORT & 0xff) as u8;
        ip[24] = ((udp_len >> 8) & 0xff) as u8;
        ip[25] = (udp_len & 0xff) as u8;

        // DNS header at offset 28.
        let dns_start = 28;
        ip[dns_start + 0] = 0x12; // ID
        ip[dns_start + 1] = 0x34;
        ip[dns_start + 2] = 0x01; // flags: RD
        ip[dns_start + 3] = 0x00;
        ip[dns_start + 4] = 0x00; // QDCOUNT high
        ip[dns_start + 5] = 0x01; // QDCOUNT = 1
        // ANCOUNT, NSCOUNT, ARCOUNT = 0 (already 0)
        ip[dns_start + 12..dns_start + 12 + qname_bytes.len()]
            .copy_from_slice(&qname_bytes);
        let qname_end = dns_start + 12 + qname_bytes.len();
        ip[qname_end] = ((qtype >> 8) & 0xff) as u8;
        ip[qname_end + 1] = (qtype & 0xff) as u8;
        ip[qname_end + 2] = 0x00; // QCLASS IN
        ip[qname_end + 3] = 0x01;

        ip
    }

    #[test]
    fn is_dns_packet() {
        let pkt = make_dns_query_packet("alice.gaming.ray", 1);
        let parsed = parse_packet(&pkt).unwrap();
        assert!(TunDnsForwarder::is_dns_packet(&parsed));
    }

    #[test]
    fn non_udp_packet_returns_none() {
        let resolver = Resolver::new(ResolverConfig::default());
        let fwd = TunDnsForwarder::new(resolver, cfg());

        let mut pkt = vec![0u8; 20];
        pkt[0] = 0x45;
        pkt[2] = 0x00;
        pkt[3] = 20;
        pkt[9] = 6; // TCP
        pkt[12..16].copy_from_slice(&[100, 64, 0, 10]);
        pkt[16..20].copy_from_slice(&[100, 64, 0, 1]);

        let parsed = parse_packet(&pkt).unwrap();
        assert!(fwd.maybe_handle(&pkt, &parsed).unwrap().is_none());
    }

    #[test]
    fn non_port53_packet_returns_none() {
        let resolver = Resolver::new(ResolverConfig::default());
        let fwd = TunDnsForwarder::new(resolver, cfg());

        let pkt = make_dns_query_packet("alice.gaming.ray", 1);
        // Change dst port to 5353.
        let mut pkt = pkt.clone();
        pkt[22] = 0x14; // 5353 >> 8
        pkt[23] = 0xE9; // 5353 & 0xff

        let parsed = parse_packet(&pkt).unwrap();
        assert!(fwd.maybe_handle(&pkt, &parsed).unwrap().is_none());
    }

    #[test]
    fn extract_qname() {
        let dns = b"\x05alice\x06gaming\x03ray\x00\x00\x01\x00\x01";
        let name = TunDnsForwarder::extract_qname(dns).unwrap();
        assert_eq!(name, "alice.gaming.ray");
    }

    #[test]
    fn extract_qtype() {
        let dns = b"\x05alice\x03ray\x00\x00\x01\x00\x01"; // A
        assert_eq!(TunDnsForwarder::extract_qtype(dns), Some(1));

        let dns_aaaa = b"\x05alice\x03ray\x00\x00\x1c\x00\x01"; // AAAA
        assert_eq!(TunDnsForwarder::extract_qtype(dns_aaaa), Some(28));
    }

    #[test]
    fn ip_checksum_zeros() {
        let h = [0u8; 20];
        let sum = TunDnsForwarder::ip_checksum(&h);
        // All-zero header: sum of all zero words = 0, ~0 = 0xffff
        assert_eq!(sum, 0xffff);
    }

    #[test]
    fn forwarder_is_clone() {
        let resolver = Resolver::new(ResolverConfig::default());
        let fwd = TunDnsForwarder::new(resolver, cfg());
        let _ = fwd.clone();
    }

    #[test]
    fn is_mesh_query() {
        let resolver1 = Resolver::new(ResolverConfig::default());
        let fwd = TunDnsForwarder::new(resolver1, cfg());
        assert!(fwd.is_mesh_query("alice.ray"));
        assert!(fwd.is_mesh_query("gaming.ray")); // two-label form
        assert!(fwd.is_mesh_query("bob.gaming.a3net")); // a3net is built-in

        let resolver2 = Resolver::new(ResolverConfig::default());
        let fwd2 = TunDnsForwarder::new(resolver2, cfg().with_extra_tld("mesh"));
        assert!(fwd2.is_mesh_query("web-1.mesh"));
        assert!(!fwd.is_mesh_query("web-1.mesh")); // not in default config
    }

    #[test]
    fn read_udp_port() {
        let raw = [0x13, 0x88, 0x00, 0x35]; // big-endian: 0x1388=5000, 0x0035=53
        assert_eq!(TunDnsForwarder::read_udp_port(&raw, 0), 5000);
        assert_eq!(TunDnsForwarder::read_udp_port(&raw, 2), 53);
    }
}
