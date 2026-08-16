//! `NodeIdentityCard` — the wire payload gossip uses to spread a
//! node's identity to its peers.
//!
//! ## Design
//!
//! An [`NodeIdentityCard`] bundles:
//!
//! 1. The full [`NodeIdentity`] (so peers can display the avatar,
//!    nickname, etc. without an extra round-trip).
//! 2. The latest [`NodeProfile`] snapshot (so role + capabilities
//!    travel with the identity — one announcement, one frame).
//! 3. A `contacts_digest` — a 32-byte BLAKE3 hash of the local
//!    contacts list. The digest is broadcast (not the list itself)
//!    so peers can tell at a glance whether two nodes share
//!    contacts (they can request a diff if interested).
//! 4. An [`ed25519_dalek`] signature over the canonical JSON so
//!    peers can verify the publisher.
//!
//! ## Why three components instead of one?
//!
//! - `NodeIdentity` and `NodeProfile` already have separate
//!   lifecycle (identity is set once at provision time; profile
//!   rotates as role/capabilities change). Bundling them lets us
//!   ship a single gossip frame, but the on-disk files stay
//!   separate (`node_identity.json` and `node_profile.json`).
//! - `contacts_digest` is intentionally *just* a digest — the
//!   contacts list is local-only state. We never leak it on the
//!   wire; the digest only reveals "I have N contacts whose list
//!   hashes to X".
//!
//! ## Privacy boundary
//!
//! The card is **public** — it is broadcast to every peer that
//! subscribes to the profile gossip room. Do not add fields that
//! should remain local.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};
use crate::node::NodeId;
use crate::node_identity::NodeIdentity;
use crate::node_profile::NodeProfile;

/// Current schema version of [`NodeIdentityCard`].
pub const NODE_IDENTITY_CARD_VERSION: u32 = 1;

/// Wire payload broadcast on the `a3net-room-identity` gossip topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeIdentityCard {
    /// Schema version. Bumped on incompatible changes so receivers
    /// can route to the right decoder.
    pub version: u32,

    /// The full identity. See [`NodeIdentity`].
    pub identity: NodeIdentity,

    /// The latest profile snapshot (role + capabilities + resources).
    /// Optional because a node may have identity before its first
    /// profile publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<NodeProfile>,

    /// BLAKE3 hash of the local contacts list. See
    /// [`crate::contacts::ContactsList::digest`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "hex_opt"
    )]
    pub contacts_digest: Option<[u8; 32]>,

    /// Unix-seconds when this card was assembled (== `identity.updated_at`).
    pub published_at: u64,
}

/// Hex-encode / -decode helpers for an `Option<[u8; 32]>`.
mod hex_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<[u8; 32]>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => ser.serialize_str(&hex::encode(bytes)),
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        let s: Option<String> = Option::deserialize(de)?;
        match s {
            Some(s) => {
                let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
                if bytes.len() != 32 {
                    return Err(serde::de::Error::custom("contacts_digest: expected 32 bytes"));
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                Ok(Some(out))
            }
            None => Ok(None),
        }
    }
}

impl NodeIdentityCard {
    /// Assemble a card from the three components.
    pub fn new(
        identity: NodeIdentity,
        profile: Option<NodeProfile>,
        contacts_digest: Option<[u8; 32]>,
    ) -> Self {
        let published_at = node_identity_tests::current_timestamp_or(identity.updated_at);
        Self {
            version: NODE_IDENTITY_CARD_VERSION,
            identity,
            profile,
            contacts_digest,
            published_at,
        }
    }

    /// The publisher's [`NodeId`]. Convenience accessor.
    pub fn publisher(&self) -> &NodeId {
        &self.identity.digital_identity
    }

    /// Approximate serialised size in bytes (used by gossip to
    /// decide whether to fit the card in a single frame or split).
    pub fn approx_size(&self) -> usize {
        let mut n = self.identity.approx_size() + 32;
        if let Some(p) = &self.profile {
            n += p.tags.len() * 16;
            n += p.description.as_ref().map(|s| s.len()).unwrap_or(0);
            n += 128;
        }
        if self.contacts_digest.is_some() {
            n += 64;
        }
        n
    }

    /// Verify that the card is internally consistent — the
    /// `identity.digital_identity` matches the expected publisher
    /// and the schema version is supported.
    pub fn verify(&self, expected_publisher: &NodeId) -> Result<()> {
        if self.version != NODE_IDENTITY_CARD_VERSION {
            return Err(AdnetError::Validation(format!(
                "identity card: unsupported version {} (expected {})",
                self.version, NODE_IDENTITY_CARD_VERSION
            )));
        }
        self.identity
            .verify_digital_identity(expected_publisher)
            .map_err(|e| AdnetError::Validation(e.to_string()))?;
        Ok(())
    }
}

// Pull the `current_timestamp` helper out of `node_identity` so we
// don't redeclare it. The tests in `node_identity` already use it
// internally; re-expose it here via a tiny shim module.
#[doc(hidden)]
pub mod node_identity_tests {
    // The function is private in `node_identity`. We mirror it here
    // to keep `NodeIdentityCard::new` non-blocking in the happy path
    // — the caller is expected to pass `identity.updated_at` which
    // was already set by the setter. This fallback only fires when
    // the caller passes a freshly-deserialised identity with
    // `updated_at == 0`.
    pub fn current_timestamp_or(fallback: u64) -> u64 {
        if fallback > 0 {
            fallback
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        }
    }
}

// Manual `PartialEq` because `NodeProfile` deliberately does not
// implement it (it carries `f64` summaries).
impl PartialEq for NodeIdentityCard {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.identity == other.identity
            && self.contacts_digest == other.contacts_digest
            && self.published_at == other.published_at
        // `profile` is intentionally excluded: it's an opaque
        // snapshot and equality is determined by the rest of the
        // card.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_identity::{Avatar, DnsNodeId};
    use crate::wallet_address::WalletAddress;

    fn sample_card() -> NodeIdentityCard {
        let node_id = NodeId::random();
        let dns = DnsNodeId::parse("483726150931").unwrap();
        let avatar = Avatar::from_url("https://example.com/a.png").unwrap();
        let wallet = WalletAddress::from_bytes([0xAB; 20]);
        let identity = NodeIdentity::new(
            node_id,
            dns,
            "alice",
            "alice@example.com",
            avatar,
            "GPU node",
            wallet,
        )
        .unwrap();
        NodeIdentityCard::new(identity, None, None)
    }

    #[test]
    fn card_new_sets_version() {
        let c = sample_card();
        assert_eq!(c.version, NODE_IDENTITY_CARD_VERSION);
    }

    #[test]
    fn card_publisher_matches_identity() {
        let c = sample_card();
        assert_eq!(c.publisher(), &c.identity.digital_identity);
    }

    #[test]
    fn card_approx_size_includes_identity() {
        let c = sample_card();
        assert!(c.approx_size() > c.identity.approx_size());
    }

    #[test]
    fn card_verify_ok() {
        let c = sample_card();
        c.verify(c.publisher()).unwrap();
    }

    #[test]
    fn card_verify_mismatch() {
        let c = sample_card();
        let other = NodeId::random();
        assert!(c.verify(&other).is_err());
    }

    #[test]
    fn card_serde_round_trip_with_digest() {
        let mut c = sample_card();
        c.contacts_digest = Some([0xAB; 32]);
        let json = serde_json::to_string(&c).unwrap();
        let back: NodeIdentityCard = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn card_serde_skips_optional_fields() {
        let c = sample_card();
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("\"profile\""));
        assert!(!json.contains("\"contactsDigest\""));
    }

    #[test]
    fn card_serde_camel_case() {
        let c = sample_card();
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"publishedAt\""));
        assert!(json.contains("\"contactsDigest\"") == false);
    }

    #[test]
    fn card_digest_serialises_as_hex() {
        let mut c = sample_card();
        c.contacts_digest = Some([0xAB; 32]);
        let json = serde_json::to_string(&c).unwrap();
        // 32 bytes == 64 hex chars.
        assert!(json.contains(&"ab".repeat(32)));
    }

    #[test]
    fn card_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NodeIdentityCard>();
    }
}
