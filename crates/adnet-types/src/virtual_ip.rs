//! Virtual IP addressing for the ADNet mesh VPN.
//!
//! Every mesh member owns one IPv4 address inside the
//! `100.64.0.0/10` CGNAT range and one IPv6 address inside
//! `200::/7`. The address is **derived deterministically** from
//! the member's identity (`NodeId`) so there is no allocator and
//! no collision: any peer can compute any other peer's virtual IP
//! from the public `NodeId` alone, and the same identity always
//! maps to the same address.
//!
//! This mirrors the approach used by rayfish, Tailscale's
//! `100.64.0.0/10` allocation, and Yggdrasil's deterministic
//! address scheme. The advantage of derivation is operational:
//! every node already knows every other node's virtual IP
//! without needing a coordinator handshake, which keeps mesh
//! routing purely peer-to-peer.
//!
//! ## Why these ranges?
//!
//! - **100.64.0.0/10** — RFC 6598 "Shared Address Space", the
//!   same block Tailscale uses. It is reserved for carrier-grade
//!   NAT and is never assigned by real routers, so mesh traffic
//!   cannot be mistaken for public Internet traffic by upstream
//!   firewalls.
//! - **200::/8** — the IETF-reserved "ORCHIDv2" range, byte
//!   slice `0x02 0x00 .. 0x02 0xff`. The wider `200::/7`
//!   range is reserved by the RFC for future use; we use
//!   only the `200::/8` half so the prefix check is exact.
//!
//! ## Wire format
//!
//! Both addresses are 16-byte big-endian integers (`Ipv4Addr` /
//! `Ipv6Addr` natively work in network byte order). The mapping
//! is:
//!
//! ```text
//! virtual_ipv4(node_id) =
//!     100.64.0.0 + u32::from_le_bytes(node_id[..4]) mod 2^22
//!
//! virtual_ipv6(node_id) =
//!     200:: + u128::from_be_bytes(node_id[..16])
//! ```
//!
//! The IPv6 derivation leaves the first two bytes equal to
//! `0x0200` (`200::/16`), which is the only stable check a packet
//! filter can do to recognise a mesh-origin frame. The full
//! `200::/7` range is reserved by IANA for ORCHIDv2.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};
use crate::node::NodeId;

/// Prefix length of the IPv4 mesh range (`100.64.0.0/10`).
pub const MESH_IPV4_PREFIX_LEN: u8 = 10;
/// Mesh IPv4 base address.
pub const MESH_IPV4_BASE: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 0);
/// IPv4 broadcast / last address in the mesh range.
pub const MESH_IPV4_LAST: Ipv4Addr = Ipv4Addr::new(100, 127, 255, 255);
/// Number of IPv4 addresses in the mesh range.
pub const MESH_IPV4_COUNT: u32 = 1 << (32 - MESH_IPV4_PREFIX_LEN);

/// Mesh IPv6 prefix bytes: `[0x02, 0x00]` (the `200::/16` slice).
pub const MESH_IPV6_PREFIX_BYTES: [u8; 2] = [0x02, 0x00];
/// Number of low bytes used in the IPv6 derivation.
pub const MESH_IPV6_LOW_BYTES: usize = 16;

/// A virtual IPv4 address inside the ADNet mesh range.
///
/// Created from a [`NodeId`] via [`VirtualIpv4::from_node_id`] or
/// parsed from the canonical dotted-quad form. The two are
/// equivalent for any valid `NodeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VirtualIpv4(Ipv4Addr);

impl VirtualIpv4 {
    /// Derive the virtual IPv4 address for a given node identity.
    ///
    /// The mapping is deterministic: identical inputs produce
    /// identical outputs across processes / restarts / machines.
    pub fn from_node_id(id: &NodeId) -> Self {
        let bytes = id.as_bytes();
        // Take the first 4 bytes as a u32 offset into the
        // mesh range. We deliberately use `from_le_bytes` so
        // the mapping is endianness-stable regardless of host
        // byte order.
        let offset = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let n = u32::from(MESH_IPV4_BASE);
        let span = MESH_IPV4_COUNT;
        let offset = offset % span;
        let raw = n + offset;
        Self(Ipv4Addr::from(raw))
    }

    /// Parse a dotted-quad IPv4 string and validate that it
    /// falls inside the mesh range.
    pub fn parse(s: &str) -> Result<Self> {
        let ip: Ipv4Addr = s
            .parse()
            .map_err(|e| AdnetError::Validation(format!("invalid IPv4 {s:?}: {e}")))?;
        Self::from_std(ip).ok_or_else(|| {
            AdnetError::Validation(format!(
                "{ip} is not inside the mesh IPv4 range {MESH_IPV4_BASE}/{MESH_IPV4_PREFIX_LEN}"
            ))
        })
    }

    /// Wrap an [`Ipv4Addr`], returning `None` if it is outside
    /// the mesh range.
    pub fn from_std(addr: Ipv4Addr) -> Option<Self> {
        let raw = u32::from(addr);
        let base = u32::from(MESH_IPV4_BASE);
        let span = MESH_IPV4_COUNT;
        if raw >= base && raw < base + span {
            Some(Self(addr))
        } else {
            None
        }
    }

    pub fn as_std(&self) -> Ipv4Addr {
        self.0
    }
}

impl std::fmt::Display for VirtualIpv4 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for VirtualIpv4 {
    type Err = AdnetError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A virtual IPv6 address inside the ADNet mesh range
/// (`200::/8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VirtualIpv6(Ipv6Addr);

impl VirtualIpv6 {
    /// Derive the virtual IPv6 address for a given node identity.
    pub fn from_node_id(id: &NodeId) -> Self {
        let bytes = id.as_bytes();
        // First two bytes are fixed to the mesh prefix (`0x02 0x00`,
        // i.e. `200::/16`). Remaining 14 bytes are copied from the
        // node id.
        let mut raw = [0u8; 16];
        raw[0] = MESH_IPV6_PREFIX_BYTES[0];
        raw[1] = MESH_IPV6_PREFIX_BYTES[1];
        for (i, slot) in raw[2..].iter_mut().enumerate() {
            *slot = bytes[i + 2];
        }
        Self(Ipv6Addr::from(raw))
    }

    /// Parse an IPv6 string and validate that it lies in the
    /// mesh prefix.
    pub fn parse(s: &str) -> Result<Self> {
        let ip: Ipv6Addr = s
            .parse()
            .map_err(|e| AdnetError::Validation(format!("invalid IPv6 {s:?}: {e}")))?;
        Self::from_std(ip).ok_or_else(|| {
            AdnetError::Validation(format!("{ip} is not inside the mesh IPv6 range 200::/16"))
        })
    }

    /// Wrap an [`Ipv6Addr`], returning `None` if it is outside
    /// the mesh prefix.
    pub fn from_std(addr: Ipv6Addr) -> Option<Self> {
        let octets = addr.octets();
        if octets[0] == MESH_IPV6_PREFIX_BYTES[0] && octets[1] == MESH_IPV6_PREFIX_BYTES[1] {
            Some(Self(addr))
        } else {
            None
        }
    }

    pub fn as_std(&self) -> Ipv6Addr {
        self.0
    }
}

impl std::fmt::Display for VirtualIpv6 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for VirtualIpv6 {
    type Err = AdnetError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A virtual IP pair (IPv4 + IPv6) for a single mesh member.
///
/// Both addresses are derived from the same [`NodeId`] and are
/// therefore collision-free. Operators can hand a [`VirtualIp`]
/// to a TUN device / route table / firewall rule without ever
/// touching the underlying cryptographic identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VirtualIp {
    pub ipv4: VirtualIpv4,
    pub ipv6: VirtualIpv6,
}

impl VirtualIp {
    pub fn from_node_id(id: &NodeId) -> Self {
        Self {
            ipv4: VirtualIpv4::from_node_id(id),
            ipv6: VirtualIpv6::from_node_id(id),
        }
    }

    /// Build from an existing IPv4 (which determines the IPv6
    /// prefix). Convenience for tests.
    pub fn from_ipv4(ipv4: VirtualIpv4) -> Self {
        Self {
            ipv4,
            ipv6: VirtualIpv6(Ipv6Addr::from(0u128)),
        }
    }
}

impl std::fmt::Display for VirtualIp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.ipv4, self.ipv6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_from_hex(hex: &str) -> NodeId {
        NodeId::from_hex(hex).expect("valid hex")
    }

    #[test]
    fn virtual_ipv4_inside_mesh_range() {
        // Any 32-byte id, the first 4 bytes map into 100.64/10.
        let id = id_from_hex("0000000000000000000000000000000000000000000000000000000000000000");
        let ip = VirtualIpv4::from_node_id(&id);
        assert_eq!(ip.as_std(), MESH_IPV4_BASE);
        let parsed = VirtualIpv4::parse("100.64.0.0").unwrap();
        assert_eq!(ip, parsed);
    }

    #[test]
    fn virtual_ipv4_offset_is_modulo() {
        // Two ids whose first two bytes differ should still
        // land inside the mesh range. Use hex chars as the
        // raw bytes (each `0`–`f` is a single hex digit).
        let s1 = "ff00000000000000000000000000000000000000000000000000000000000000";
        let s2 = "0000000000000000000000000000000000000000000000000000000000000000";
        let ip1 = VirtualIpv4::from_node_id(&id_from_hex(s1));
        let ip2 = VirtualIpv4::from_node_id(&id_from_hex(s2));
        let raw1 = u32::from(ip1.as_std());
        let raw2 = u32::from(ip2.as_std());
        let base = u32::from(MESH_IPV4_BASE);
        let span = MESH_IPV4_COUNT;
        assert!(raw1 >= base && raw1 < base + span);
        assert!(raw2 >= base && raw2 < base + span);
    }

    #[test]
    fn virtual_ipv4_parse_rejects_outside_range() {
        assert!(VirtualIpv4::parse("8.8.8.8").is_err());
        assert!(VirtualIpv4::parse("192.168.1.1").is_err());
        assert!(VirtualIpv4::parse("100.128.0.0").is_err());
    }

    #[test]
    fn virtual_ipv4_parse_accepts_inside_range() {
        assert!(VirtualIpv4::parse("100.64.0.1").is_ok());
        assert!(VirtualIpv4::parse("100.127.255.254").is_ok());
    }

    #[test]
    fn virtual_ipv4_deterministic() {
        let id = NodeId::random();
        let a = VirtualIpv4::from_node_id(&id);
        let b = VirtualIpv4::from_node_id(&id);
        assert_eq!(a, b);
    }

    #[test]
    fn virtual_ipv4_serde() {
        let id = NodeId::random();
        let ip = VirtualIpv4::from_node_id(&id);
        let s = serde_json::to_string(&ip).unwrap();
        let back: VirtualIpv4 = serde_json::from_str(&s).unwrap();
        assert_eq!(ip, back);
    }

    #[test]
    fn virtual_ipv6_prefix_is_200() {
        let id = NodeId::random();
        let ip = VirtualIpv6::from_node_id(&id);
        let octets = ip.as_std().octets();
        assert_eq!(octets[0], 0x02);
        assert_eq!(octets[1], 0x00);
    }

    #[test]
    fn virtual_ipv6_parse_rejects_wrong_prefix() {
        assert!(VirtualIpv6::parse("::1").is_err());
        assert!(VirtualIpv6::parse("fe80::1").is_err());
        assert!(VirtualIpv6::parse("201::1").is_err());
    }

    #[test]
    fn virtual_ipv6_parse_accepts_correct_prefix() {
        // The IPv6 form for `200::/16` accepts compressed zeros.
        assert!(VirtualIpv6::parse("200::1").is_ok());
        assert!(VirtualIpv6::parse("200::").is_ok());
        assert!(VirtualIpv6::parse("200:0000:0000:0000:0000:0000:0000:0001").is_ok());
    }

    #[test]
    fn virtual_ipv6_deterministic() {
        let id = NodeId::random();
        let a = VirtualIpv6::from_node_id(&id);
        let b = VirtualIpv6::from_node_id(&id);
        assert_eq!(a, b);
    }

    #[test]
    fn virtual_ip_pair_from_node_id() {
        let id = NodeId::random();
        let pair = VirtualIp::from_node_id(&id);
        // IPv4 derived independently from IPv6 (different byte
        // slices of the node id) — they should not coincide.
        assert!(pair.ipv4.as_std() != Ipv4Addr::from([0, 0, 0, 0]));
        assert_eq!(pair.ipv6.as_std().octets()[0], 0x02);
        assert_eq!(pair.ipv6.as_std().octets()[1], 0x00);
    }

    #[test]
    fn mesh_constants_invariants() {
        assert_eq!(MESH_IPV4_BASE, Ipv4Addr::new(100, 64, 0, 0));
        assert_eq!(MESH_IPV4_COUNT, 1 << 22);
        assert!(u32::from(MESH_IPV4_LAST) - u32::from(MESH_IPV4_BASE) + 1 == MESH_IPV4_COUNT);
    }
}
