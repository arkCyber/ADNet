//! Gossip topic identifiers (iroh-gossip parity).
//!
//! A topic is a 32-byte opaque identifier. A3Net derives topics from a string
//! label (`"a3net-room-{room_id}"`) so that human-readable names map
//! deterministically into the gossip overlay.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Opaque 32-byte gossip topic id, hex-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Topic(String);

impl Topic {
    pub const HEX_LEN: usize = 64;

    /// Derive a topic from a string label via BLAKE3.
    pub fn from_label(label: &str) -> Self {
        Self(blake3::hash(label.as_bytes()).to_hex().to_string())
    }

    /// Build from raw 32 bytes.
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(hex::encode(bytes))
    }

    /// Parse a hex-encoded topic id.
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() == Self::HEX_LEN && s.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(Self(s.to_ascii_lowercase()))
        } else {
            None
        }
    }

    pub fn as_hex(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Canonical topic naming convention. Mirrors the Exodus `exodus-cdn-{room}`
/// pattern so external observers can recognise the protocol family.
pub fn topic_name(scope: &str, room_id: &str) -> String {
    format!("a3net-{scope}-{room_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_is_deterministic_for_label() {
        let a = Topic::from_label("a3net-room-lobby");
        let b = Topic::from_label("a3net-room-lobby");
        assert_eq!(a, b);
        assert_eq!(a.as_hex().len(), Topic::HEX_LEN);
    }

    #[test]
    fn topic_name_helper() {
        let n = topic_name("room", "lobby");
        assert_eq!(n, "a3net-room-lobby");
        let t = Topic::from_label(&n);
        // round-trip
        assert_eq!(Topic::from_hex(t.as_hex()).unwrap(), t);
    }
}
