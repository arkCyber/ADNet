//! `NodeAddr` — iroh-style routable address: direct endpoint + optional
//! relay URL. This is the ADNet equivalent of `iroh_base::NodeAddr`.
//!
//! Also defines [`NodeId`] (iroh `EndpointId` parity: 32 random bytes
//! rendered as 64 hex characters) — kept in this module so the addressing
//! primitives live next to each other.

use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};

/// Size, in bytes, of a [`NodeId`] digest.
pub const NODE_ID_BYTES: usize = 32;
/// Hex-string length of a [`NodeId`].
pub const NODE_ID_HEX_LEN: usize = NODE_ID_BYTES * 2;

/// Unique node identifier. 32 random bytes shown as 64 hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl Default for NodeId {
    fn default() -> Self {
        Self::random()
    }
}

impl NodeId {
    pub const HEX_LEN: usize = NODE_ID_HEX_LEN;

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != NODE_ID_BYTES {
            return Err(AdnetError::InvalidNodeId(format!(
                "expected {NODE_ID_BYTES} bytes, got {}",
                bytes.len()
            )));
        }
        Ok(Self(hex::encode(bytes)))
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        if s.len() != Self::HEX_LEN {
            return Err(AdnetError::InvalidNodeId(format!(
                "expected {} hex chars, got {}",
                Self::HEX_LEN,
                s.len()
            )));
        }
        if !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AdnetError::InvalidNodeId(s.to_string()));
        }
        Ok(Self(s.to_ascii_lowercase()))
    }

    pub fn random() -> Self {
        let mut bytes = [0u8; NODE_ID_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(hex::encode(bytes))
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        hex::decode(&self.0).expect("valid hex")
    }

    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// Short identifier (`first 12 hex chars`) for human display.
    pub fn short(&self) -> &str {
        &self.0[..12.min(self.0.len())]
    }

    /// XOR-distance to another [`NodeId`]. Defined over the
    /// 32-byte decoded representation (decoding failure is
    /// treated as a self-distance so the routing table stays
    /// total). Used by Kademlia routing tables and contact
    /// ordering.
    pub fn xor_distance(&self, other: &NodeId) -> Vec<u8> {
        let a = hex::decode(&self.0).unwrap_or_default();
        let b = hex::decode(&other.0).unwrap_or_default();
        let len = a.len().max(b.len()).max(NODE_ID_BYTES);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let ai = a.get(i).copied().unwrap_or(0);
            let bi = b.get(i).copied().unwrap_or(0);
            out.push(ai ^ bi);
        }
        out
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for NodeId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s).map_err(|e| e.to_string())
    }
}

impl TryFrom<&str> for NodeId {
    type Error = String;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::from_hex(s).map_err(|e| e.to_string())
    }
}

/// Find the next occurrence of ` direct=` or ` relay=` (with the leading
/// space) in the buffer; returns the offset relative to the start.
fn find_next_marker(buf: &str) -> usize {
    let bytes = buf.as_bytes();
    let mut best: Option<usize> = None;
    for marker in [" direct=", " relay="] {
        if let Some(idx) = find_subslice(bytes, marker.as_bytes()) {
            best = Some(best.map_or(idx, |b| b.min(idx)));
        }
    }
    best.unwrap_or(bytes.len())
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A routable network endpoint (`host:port`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Endpoint(String);

impl Endpoint {
    /// Build an endpoint from host + port.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self(format!("{}:{}", host.into(), port))
    }

    pub fn host(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }

    pub fn port(&self) -> Option<u16> {
        self.0.rsplit(':').next()?.parse().ok()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A relay URL like `https://relay.example.com` (iroh `RelayUrl` parity).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelayUrl(String);

impl RelayUrl {
    pub fn new(url: impl Into<String>) -> Self {
        Self(url.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RelayUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Composite routable address: a node + its direct endpoint + optional relay.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeAddr {
    pub node_id: NodeId,
    pub direct: Option<Endpoint>,
    pub relay: Option<RelayUrl>,
}

impl NodeAddr {
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            direct: None,
            relay: None,
        }
    }

    pub fn with_direct(mut self, direct: Endpoint) -> Self {
        self.direct = Some(direct);
        self
    }

    pub fn with_relay(mut self, relay: RelayUrl) -> Self {
        self.relay = Some(relay);
        self
    }

    /// Render as a human-readable multi-line string for diagnostics.
    pub fn display(&self) -> String {
        let mut s = self.node_id.to_string();
        if let Some(d) = &self.direct {
            s.push_str(&format!(" direct={d}"));
        }
        if let Some(r) = &self.relay {
            s.push_str(&format!(" relay={r}"));
        }
        s
    }

    /// Parse an iroh-style `NodeAddr` from its display form:
    /// `<node_id> direct=<host:port> relay=<url>`.
    ///
    /// Tokens are delimited by the `direct=` and `relay=` keywords (which are
    /// guaranteed not to appear inside a host or URL), so the embedded `:` in
    /// `host:port` and `/` in relay URLs do not need escaping.
    pub fn parse(raw: &str) -> Result<Self> {
        let mut addr = NodeAddr::new(NodeId::random());
        // Walk the string, splitting on ` direct=` and ` relay=` markers.
        // The very first token is the node id; whatever follows until the
        // next marker is the value of that field.
        let bytes = raw.as_bytes();
        // Find first whitespace boundary after the node id.
        let id_end = bytes
            .iter()
            .position(|b| b.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let node_hex = &raw[..id_end];
        addr.node_id = crate::node::NodeId::from_hex(node_hex)?;
        let mut cursor = id_end;
        while cursor < bytes.len() {
            // Skip whitespace.
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                break;
            }
            // Identify the marker.
            if raw[cursor..].starts_with("direct=") {
                cursor += "direct=".len();
                let value_end = find_next_marker(&raw[cursor..]);
                let value = &raw[cursor..cursor + value_end];
                let (h, p) = value
                    .rsplit_once(':')
                    .ok_or_else(|| AdnetError::InvalidTicket(value.to_string()))?;
                let port: u16 = p
                    .parse()
                    .map_err(|_| AdnetError::InvalidTicket(value.to_string()))?;
                addr.direct = Some(Endpoint::new(h, port));
                cursor += value_end;
            } else if raw[cursor..].starts_with("relay=") {
                cursor += "relay=".len();
                let value_end = find_next_marker(&raw[cursor..]);
                let value = &raw[cursor..cursor + value_end];
                addr.relay = Some(RelayUrl::new(value));
                cursor += value_end;
            } else {
                return Err(AdnetError::InvalidTicket(raw.to_string()));
            }
        }
        Ok(addr)
    }
}

impl std::fmt::Display for NodeAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_roundtrip_and_short() {
        let id = crate::node::NodeId::random();
        let hex = id.as_hex().to_string();
        assert_eq!(hex.len(), crate::node::NodeId::HEX_LEN);
        let back = crate::node::NodeId::from_hex(&hex).unwrap();
        assert_eq!(id, back);
        assert_eq!(id.short().len(), 12);
    }

    #[test]
    fn invalid_node_id_rejected() {
        assert!(crate::node::NodeId::from_hex("nope").is_err());
        assert!(crate::node::NodeId::from_hex(&"a".repeat(crate::node::NodeId::HEX_LEN)).is_ok());
    }

    #[test]
    fn endpoint_host_port() {
        let e = Endpoint::new("127.0.0.1", 7878);
        assert_eq!(e.host(), "127.0.0.1");
        assert_eq!(e.port(), Some(7878));
    }

    #[test]
    fn node_addr_parse_roundtrip() {
        let raw = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff direct=10.0.0.1:9000 relay=https://relay.example.com";
        let addr = NodeAddr::parse(raw).unwrap();
        assert_eq!(addr.direct.as_ref().unwrap().port(), Some(9000));
        assert_eq!(
            addr.relay.as_ref().unwrap().as_str(),
            "https://relay.example.com"
        );
        assert_eq!(addr.display(), raw);
    }

    #[test]
    fn node_addr_with_helpers() {
        let id = crate::node::NodeId::random();
        let addr = NodeAddr::new(id.clone())
            .with_direct(Endpoint::new("127.0.0.1", 1000))
            .with_relay(RelayUrl::new("https://relay.example.com"));
        assert_eq!(addr.node_id, id);
        assert!(addr.direct.is_some());
        assert!(addr.relay.is_some());
    }
}
