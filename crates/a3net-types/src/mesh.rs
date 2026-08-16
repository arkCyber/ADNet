//! Mesh network identity, membership, and roster.
//!
//! A [`MeshNetworkId`] identifies a single mesh network (the
//! "room" in rayfish / Tailscale terminology). It is the **room
//! id** peers gossip about and use to discover each other. On
//! closed networks the room id is *not* an admission credential —
//! it only enables discovery; admission is handled by the
//! [`MeshCoordinator`](crate::coordinator) sibling crate via
//! signed [`MeshMember`] records and one-time invite codes.
//!
//! The mesh identity is derived from a 32-byte Ed25519 public
//! key (the same construction as iroh's `EndpointId` and as
//! `a3net_types::NodeId`). Keeping the byte length and shape
//! identical means existing wire-level filters, ticket parsers,
//! and gossip overlays can carry mesh records unchanged.
//!
//! ## Membership model
//!
//! - [`MeshMember`] — a single admission. Carries the member's
//!   transport identity (`NodeId`), a [`VirtualIp`](crate::virtual_ip)
//!   pair, a hostname, and a signed roster signature.
//! - [`MeshMembership`] — the signed roster for a single
//!   network. The coordinator signs every addition / removal
//!   and publishes the new roster on the gossip topic; nodes
//!   verify each update with the coordinator's public key
//!   before applying it locally.
//! - [`InviteCode`] — a single-use, expiring code minted by a
//!   coordinator. `mesh-invite://<network>:<code>` is the
//!   canonical transfer form (suitable for QR / email).

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};
use crate::node::NodeId;
use crate::virtual_ip::{VirtualIp, VirtualIpv4};

/// Size, in bytes, of a [`MeshNetworkId`]. Same as a [`NodeId`].
pub const MESH_NETWORK_ID_BYTES: usize = 32;
/// Hex length of a [`MeshNetworkId`].
pub const MESH_NETWORK_ID_HEX_LEN: usize = MESH_NETWORK_ID_BYTES * 2;
/// Canonical URL scheme for mesh invite codes.
pub const MESH_INVITE_SCHEME: &str = "a3net-invite://";
/// Canonical URL scheme for network identifiers.
pub const MESH_NETWORK_SCHEME: &str = "a3net-mesh://";

/// Stable identifier for a single mesh network.
///
/// Conceptually identical to a "room id" or a Tailscale
/// "tailnet". Derived from the coordinator's Ed25519 public key
/// on creation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MeshNetworkId(String);

impl MeshNetworkId {
    pub const HEX_LEN: usize = MESH_NETWORK_ID_HEX_LEN;

    /// Construct from 32 raw bytes (typically a public key).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != MESH_NETWORK_ID_BYTES {
            return Err(AdnetError::Validation(format!(
                "mesh network id: expected {MESH_NETWORK_ID_BYTES} bytes, got {}",
                bytes.len()
            )));
        }
        Ok(Self(hex::encode(bytes)))
    }

    /// Construct from a 64-char hex string.
    pub fn from_hex(s: &str) -> Result<Self> {
        if s.len() != Self::HEX_LEN {
            return Err(AdnetError::Validation(format!(
                "mesh network id: expected {} hex chars, got {}",
                Self::HEX_LEN,
                s.len()
            )));
        }
        if !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AdnetError::Validation(format!(
                "mesh network id: non-hex character in {s:?}"
            )));
        }
        Ok(Self(s.to_ascii_lowercase()))
    }

    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// Inner hex string (alias of [`Self::as_hex`] for callers
    /// that prefer a string-like name).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        hex::decode(&self.0).expect("valid hex")
    }

    /// Short identifier (first 12 hex chars) for human display.
    pub fn short(&self) -> &str {
        &self.0[..12.min(self.0.len())]
    }

    /// Render the canonical URL form (`a3net-mesh://<hex>`).
    pub fn encode_url(&self) -> String {
        format!("{MESH_NETWORK_SCHEME}{}", self.0)
    }

    /// Parse the canonical URL form.
    pub fn parse_url(raw: &str) -> Result<Self> {
        let rest = raw
            .strip_prefix(MESH_NETWORK_SCHEME)
            .ok_or_else(|| AdnetError::Validation(format!("not a mesh URL: {raw:?}")))?;
        Self::from_hex(rest)
    }
}

impl fmt::Display for MeshNetworkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Network policy: who can join.
///
/// Rayfish / Tailscale both default to **closed** networks with
/// three admission paths (invite, reusable key, live approval)
/// and an opt-in **open** mode for public networks. The enum
/// here mirrors that split so a future policy like "approved +
/// key" can be added without breaking the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshPolicy {
    /// Anyone with the network id can join directly.
    Open,
    /// Coordinator approves every join via invite or live
    /// approval (default). Closed-by-default matches the
    /// "private by default" posture of both rayfish and
    /// Tailscale. Operators that want a public mesh must
    /// opt in explicitly.
    #[default]
    Closed,
}

impl fmt::Display for MeshPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "open",
            Self::Closed => "closed",
        })
    }
}

/// Network topology: full-mesh or hub-and-spoke.
///
/// Full mesh (the default) means every member connects to every
/// other member directly, mirroring rayfish's default mesh
/// behaviour. Hub-and-spoke funnels all inter-member traffic
/// through a designated hub, which trades latency for cheap
/// connectivity on resource-constrained nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MeshTopology {
    /// Every member connects to every other member (default).
    #[default]
    Full,
    /// All inter-member traffic is routed through `hub`.
    HubSpoke,
}

impl fmt::Display for MeshTopology {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full => "full",
            Self::HubSpoke => "hub_spoke",
        })
    }
}

/// A single admitted mesh member.
///
/// The record is what the coordinator publishes on the gossip
/// topic whenever membership changes; receiving nodes verify
/// the [`signature`](Self::signature) against the
/// [`MeshNetworkId`]'s public key (the coordinator) before
/// applying the update locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshMember {
    /// Member's transport identity (32-byte NodeId).
    pub node_id: NodeId,
    /// Human-friendly hostname inside the network. Unique
    /// within a single mesh.
    pub hostname: String,
    /// Member's virtual IP (derived deterministically from
    /// `node_id`, repeated here for wire-format convenience).
    pub virtual_ip: VirtualIp,
    /// True for the coordinator(s) of this network. The first
    /// member of every network is a coordinator by construction.
    #[serde(default)]
    pub is_coordinator: bool,
    /// When the member was admitted.
    pub admitted_at: DateTime<Utc>,
}

impl MeshMember {
    /// Build a coordinator member record for a freshly-created
    /// network. Used by `mesh create` paths.
    pub fn new_coordinator(node_id: NodeId, hostname: impl Into<String>) -> Self {
        Self {
            virtual_ip: VirtualIp::from_node_id(&node_id),
            node_id,
            hostname: hostname.into(),
            is_coordinator: true,
            admitted_at: Utc::now(),
        }
    }

    /// Build a regular (non-coordinator) member record.
    pub fn new_member(node_id: NodeId, hostname: impl Into<String>) -> Self {
        Self {
            virtual_ip: VirtualIp::from_node_id(&node_id),
            node_id,
            hostname: hostname.into(),
            is_coordinator: false,
            admitted_at: Utc::now(),
        }
    }

    /// Convenience accessor for the member's IPv4.
    pub fn ipv4(&self) -> VirtualIpv4 {
        self.virtual_ip.ipv4
    }
}

/// Signed roster for a single mesh network.
///
/// The roster is a `(network_id, version, members[], signature)`
/// tuple. `signature` is over the canonical serialisation of
/// `(network_id, version, members[])` using the coordinator's
/// Ed25519 key. Receiving nodes verify the signature before
/// applying any membership change; a node that does not have
/// the coordinator's public key can still extract the
/// membership list (the signature is the only field that
/// requires verification — `members[]` is plain JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshMembership {
    /// The network this roster belongs to.
    pub network_id: MeshNetworkId,
    /// Monotonically-increasing version number. Every accepted
    /// roster update must have `version > current_version`; a
    /// stale or equal version is silently dropped.
    pub version: u64,
    /// The full member list as of this version.
    pub members: Vec<MeshMember>,
    /// Ed25519 signature over the canonical form of
    /// `(network_id, version, members)`, hex-encoded.
    pub signature: String,
    /// When this roster version was published.
    pub published_at: DateTime<Utc>,
}

impl MeshMembership {
    /// Build a new unsigned roster. `signature` is left empty;
    /// the caller is expected to sign and fill it in before
    /// publishing.
    pub fn new_unsigned(network_id: MeshNetworkId, members: Vec<MeshMember>) -> Self {
        Self {
            network_id,
            version: 1,
            members,
            signature: String::new(),
            published_at: Utc::now(),
        }
    }

    /// Bump the version (used internally when adding/removing).
    pub fn bumped(&mut self) {
        self.version = self.version.saturating_add(1);
        self.published_at = Utc::now();
    }

    /// Canonical bytes for signature computation / verification:
    /// `version || network_id.canonical() || members_len ||
    /// joined(members.canonical())`. The signature field is
    /// always excluded from the preimage.
    pub fn signing_preimage(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 64 + 8 + self.members.len() * 96);
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(self.network_id.as_str().as_bytes());
        out.push(b'|');
        out.extend_from_slice(&(self.members.len() as u32).to_be_bytes());
        out.push(b'|');
        for m in &self.members {
            out.extend_from_slice(m.node_id.as_bytes().as_slice());
            out.push(b'|');
            out.extend_from_slice(m.hostname.as_bytes());
            out.push(b'|');
            out.push(if m.is_coordinator { 1 } else { 0 });
            out.push(b'|');
        }
        out
    }

    /// Hex-encoded BLAKE3 hash of the signing preimage.
    /// Useful for deterministic cache keys when the signature
    /// field is empty.
    pub fn content_hash_hex(&self) -> String {
        let digest = blake3::hash(&self.signing_preimage());
        hex::encode(digest.as_bytes())
    }

    /// Apply a hex-encoded signature in place. The caller is
    /// expected to have computed the signature externally
    /// (e.g. via [`crate::mesh::MeshRosterSigner`]). Passing
    /// `""` clears the field.
    pub fn set_signature_hex(&mut self, hex_sig: impl Into<String>) {
        self.signature = hex_sig.into();
    }

    /// Look up a member by their `NodeId`.
    pub fn member(&self, node_id: &NodeId) -> Option<&MeshMember> {
        self.members.iter().find(|m| &m.node_id == node_id)
    }

    /// Count of admitted members.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the roster is empty.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

/// Ed25519 signer trait for [`MeshMembership`]. Decoupling the
/// signature scheme behind a trait keeps `a3net-types` crypto-free
/// while letting the `a3net-mesh-coordinator` crate supply a
/// concrete `ed25519-dalek` implementation.
pub trait MeshRosterSigner: Send + Sync {
    /// Sign the canonical preimage of `roster`. Returns the
    /// 64-byte signature (the caller hex-encodes for storage).
    fn sign_roster(&self, preimage: &[u8]) -> Vec<u8>;
    /// Return the public key bytes (32) of the signer. Receiving
    /// nodes compare this against their registered coordinator
    /// pubkey before accepting any roster.
    fn public_key_bytes(&self) -> [u8; 32];
}

/// Ed25519 verifier trait for [`MeshMembership`].
pub trait MeshRosterVerifier: Send + Sync {
    /// Verify a signature against `preimage`. Returns `true` on
    /// success. Implementations MUST return `false` (never panic)
    /// on malformed inputs — rosters arrive over gossip where
    /// noise is the norm.
    fn verify_roster(&self, preimage: &[u8], signature: &[u8]) -> bool;
}

/// Helper: verify a roster's stored signature against a known
/// coordinator pubkey. Returns `false` for empty signatures,
/// non-hex, or wrong-length bodies. Internally decodes the hex
/// field, calls [`MeshRosterVerifier::verify_roster`], and
/// returns the boolean.
pub fn verify_roster_signature<V: MeshRosterVerifier>(
    verifier: &V,
    roster: &MeshMembership,
) -> bool {
    let preimage = roster.signing_preimage();
    // The signature is stored hex-encoded; both empty and short
    // / odd-length hex strings decode to `None`, so the early
    // `false` covers the missing-signature path.
    let sig_bytes = match hex::decode(roster.signature.as_bytes()) {
        Ok(b) => b,
        Err(_) => return false,
    };
    if sig_bytes.len() != 64 {
        return false;
    }
    verifier.verify_roster(&preimage, &sig_bytes)
}

/// One-time invite code for a closed mesh network.
///
/// `mesh-invite://<network_id_hex>:<code>` is the wire format.
/// The code itself is short (16 hex chars, 8 bytes of entropy)
/// and is single-use; the coordinator burns it on accept. See
/// [`crate::coordinator`] for the live state machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InviteCode {
    pub network_id: MeshNetworkId,
    pub code: String,
    /// When this code expires. After this point, accept
    /// attempts must fail.
    pub expires_at: DateTime<Utc>,
    /// How many times this code has been redeemed. Codes are
    /// minted single-use by default; the coordinator flips
    /// `redeemed = true` on first use.
    #[serde(default)]
    pub redeemed: bool,
}

/// Partial invite-code reference returned by
/// [`InviteCode::parse_url`] when the URL has not been
/// resolved against coordinator state.
///
/// Use this when the recipient only needs the network +
/// code to look up the full [`InviteCode`] (which
/// carries the expiry and the redeemed flag) from the
/// coordinator's pending-invite table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InviteCodeRef {
    pub network_id: MeshNetworkId,
    pub code: String,
}

impl InviteCodeRef {
    pub fn new(network_id: MeshNetworkId, code: impl Into<String>) -> Self {
        Self {
            network_id,
            code: code.into(),
        }
    }
}

/// The canonical URL grammar is:
/// `a3net-invite://<network_id_hex>:<code>:<unix_expires_at>`
///
/// The unix timestamp is a small integer; the expiry is
/// informational (the coordinator's state is authoritative)
/// but it lets a recipient parse and immediately decide
/// whether to attempt redemption.
pub const MESH_INVITE_URL_VERSION: u8 = 2;

impl InviteCode {
    pub const CODE_LEN: usize = 16;

    /// Construct a new invite code with a random secret.
    pub fn new(network_id: MeshNetworkId, ttl: Duration) -> Self {
        let mut bytes = [0u8; 8];
        rand::Rng::fill(&mut rand::thread_rng(), &mut bytes[..]);
        Self {
            network_id,
            code: hex::encode(bytes),
            expires_at: Utc::now() + chrono::Duration::from_std(ttl).unwrap_or_default(),
            redeemed: false,
        }
    }

    /// Render the canonical URL form including the
    /// unix-expiry timestamp. Callers that need to ship
    /// the URL through a QR / email round-trip should use
    /// this form so the recipient can parse the expiry
    /// back out via [`InviteCode::parse_url`].
    pub fn encode_url(&self) -> String {
        format!(
            "{MESH_INVITE_SCHEME}{}:{}:{}",
            self.network_id.as_hex(),
            self.code,
            self.expires_at.timestamp()
        )
    }

    /// Parse the canonical URL form. The URL **must**
    /// include the unix-expiry timestamp appended after a
    /// `:` (see [`InviteCode::encode_url`]). The returned
    /// `expires_at` is taken from the URL; `redeemed` is
    /// always `false` because the URL alone cannot convey
    /// that flag — the caller must check the coordinator's
    /// pending-invite table for the authoritative state.
    pub fn parse_url(raw: &str) -> Result<Self> {
        let rest = raw
            .strip_prefix(MESH_INVITE_SCHEME)
            .ok_or_else(|| AdnetError::Validation(format!("not an invite URL: {raw:?}")))?;
        let mut parts = rest.splitn(3, ':');
        let net_hex = parts.next().ok_or_else(|| {
            AdnetError::Validation(format!("missing network id in invite URL: {raw:?}"))
        })?;
        let code = parts.next().ok_or_else(|| {
            AdnetError::Validation(format!("missing code in invite URL: {raw:?}"))
        })?;
        let expires_str = parts
            .next()
            .ok_or_else(|| {
                AdnetError::Validation(format!(
                    "invite URL is missing the unix-expiry timestamp (use InviteCode::encode_url, version {MESH_INVITE_URL_VERSION}): {raw:?}"
                ))
            })?;
        let network_id = MeshNetworkId::from_hex(net_hex)?;
        let expires_at_secs: i64 = expires_str.parse().map_err(|_| {
            AdnetError::Validation(format!(
                "invalid unix timestamp in invite URL: {expires_str:?}"
            ))
        })?;
        let expires_at = DateTime::<Utc>::from_timestamp(expires_at_secs, 0).ok_or_else(|| {
            AdnetError::Validation(format!("unix timestamp out of range: {expires_at_secs}"))
        })?;
        Ok(Self {
            network_id,
            code: code.to_string(),
            expires_at,
            redeemed: false,
        })
    }

    /// Parse the URL and return only the network + code
    /// pair. Use this when the caller will resolve the
    /// full [`InviteCode`] (with its expiry and redeemed
    /// flag) from the coordinator anyway.
    pub fn parse_url_ref(raw: &str) -> Result<InviteCodeRef> {
        let code = Self::parse_url(raw)?;
        Ok(InviteCodeRef {
            network_id: code.network_id,
            code: code.code,
        })
    }

    /// Whether the code has expired relative to `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Whether this code can still be redeemed.
    pub fn is_redeemable(&self, now: DateTime<Utc>) -> bool {
        !self.redeemed && !self.is_expired(now)
    }
}

impl fmt::Display for InviteCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode_url())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_network_id_from_bytes_and_hex_roundtrip() {
        let bytes = [0xabu8; 32];
        let id = MeshNetworkId::from_bytes(&bytes).unwrap();
        assert_eq!(id.as_bytes(), bytes.to_vec());
        let hex = id.as_hex().to_string();
        let back = MeshNetworkId::from_hex(&hex).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn mesh_network_id_rejects_wrong_length() {
        assert!(MeshNetworkId::from_bytes(&[0u8; 31]).is_err());
        assert!(MeshNetworkId::from_hex(&"a".repeat(63)).is_err());
        assert!(MeshNetworkId::from_hex(&"a".repeat(65)).is_err());
        assert!(
            MeshNetworkId::from_hex("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz")
                .is_err()
        );
    }

    #[test]
    fn mesh_network_id_short_and_url() {
        let bytes = [0u8; 32];
        let id = MeshNetworkId::from_bytes(&bytes).unwrap();
        assert_eq!(id.short().len(), 12);
        let url = id.encode_url();
        assert!(url.starts_with(MESH_NETWORK_SCHEME));
        let back = MeshNetworkId::parse_url(&url).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn mesh_network_id_parse_url_rejects_other_scheme() {
        assert!(MeshNetworkId::parse_url("a3net-peer://abcdef").is_err());
        assert!(MeshNetworkId::parse_url("https://abcdef").is_err());
    }

    #[test]
    fn mesh_policy_default_is_closed() {
        assert_eq!(MeshPolicy::default(), MeshPolicy::Closed);
    }

    #[test]
    fn mesh_topology_default_is_full() {
        assert_eq!(MeshTopology::default(), MeshTopology::Full);
    }

    #[test]
    fn mesh_member_coordinator_flag() {
        let id = NodeId::random();
        let coord = MeshMember::new_coordinator(id.clone(), "alice");
        assert!(coord.is_coordinator);
        let member = MeshMember::new_member(id, "bob");
        assert!(!member.is_coordinator);
    }

    #[test]
    fn mesh_membership_bump_increments_version() {
        let nid = MeshNetworkId::from_bytes(&[1u8; 32]).unwrap();
        let mut roster = MeshMembership::new_unsigned(nid.clone(), vec![]);
        assert_eq!(roster.version, 1);
        roster.bumped();
        assert_eq!(roster.version, 2);
        roster.bumped();
        assert_eq!(roster.version, 3);
    }

    #[test]
    fn mesh_membership_lookup_by_node_id() {
        let nid = MeshNetworkId::from_bytes(&[1u8; 32]).unwrap();
        let id_a = NodeId::random();
        let id_b = NodeId::random();
        let members = vec![
            MeshMember::new_member(id_a.clone(), "alice"),
            MeshMember::new_member(id_b.clone(), "bob"),
        ];
        let roster = MeshMembership::new_unsigned(nid, members);
        assert_eq!(roster.member(&id_a).unwrap().hostname, "alice");
        assert_eq!(roster.member(&id_b).unwrap().hostname, "bob");
        assert!(roster.member(&NodeId::random()).is_none());
    }

    #[test]
    fn invite_code_default_ttl_and_redemption() {
        let nid = MeshNetworkId::from_bytes(&[0u8; 32]).unwrap();
        let c = InviteCode::new(nid.clone(), Duration::from_secs(60));
        assert!(c.is_redeemable(Utc::now()));
        assert!(!c.is_redeemable(Utc::now() + Duration::from_secs(120)));
        let url = c.encode_url();
        let back = InviteCode::parse_url(&url).unwrap();
        assert_eq!(back.network_id, nid);
        assert_eq!(back.code, c.code);
        // The expiry now round-trips through the URL at
        // second-level precision. Allow a 1-second drift
        // because `encode_url` truncates to seconds.
        let drift = (back.expires_at - c.expires_at).num_seconds().abs();
        assert!(
            drift <= 1,
            "expiry round-trip drift should be <= 1s, got {drift}s"
        );
    }

    /// Regression: older URLs without a unix-expiry
    /// timestamp must be rejected at parse time instead of
    /// silently fabricating `expires_at = now`.
    #[test]
    fn invite_code_legacy_url_is_rejected() {
        // Hand-built v1 URL (no timestamp suffix).
        let nid = MeshNetworkId::from_bytes(&[0u8; 32]).unwrap();
        let legacy = format!("a3net-invite://{}:abcd1234", nid.as_hex());
        let err = InviteCode::parse_url(&legacy).unwrap_err();
        assert!(err.to_string().contains("unix-expiry"));
    }

    #[test]
    fn invite_code_parse_url_rejects_wrong_shape() {
        assert!(InviteCode::parse_url("a3net-invite://").is_err());
        assert!(InviteCode::parse_url("a3net-invite://nocolon").is_err());
        assert!(InviteCode::parse_url("https://x:1").is_err());
    }

    #[test]
    fn invite_code_parse_url_ref_returns_pair() {
        let nid = MeshNetworkId::from_bytes(&[1u8; 32]).unwrap();
        let c = InviteCode::new(nid.clone(), Duration::from_secs(60));
        let url = c.encode_url();
        let r = InviteCode::parse_url_ref(&url).unwrap();
        assert_eq!(r.network_id, nid);
        assert_eq!(r.code, c.code);
    }

    #[test]
    fn mesh_membership_serde_roundtrip() {
        let nid = MeshNetworkId::from_bytes(&[2u8; 32]).unwrap();
        let id = NodeId::random();
        let roster = MeshMembership::new_unsigned(nid, vec![MeshMember::new_member(id, "alice")]);
        let s = serde_json::to_string(&roster).unwrap();
        let back: MeshMembership = serde_json::from_str(&s).unwrap();
        assert_eq!(back.network_id, roster.network_id);
        assert_eq!(back.members.len(), 1);
    }

    // ====================================================================
    // Signing preimage / verify_roster_signature tests (issue #2 fix:
    // MeshMembership::signature previously left empty and downstream
    // consumers could not authenticate the roster).
    // ====================================================================

    fn sample_roster() -> MeshMembership {
        let nid = MeshNetworkId::from_bytes(&[7u8; 32]).unwrap();
        let coord = MeshMember::new_coordinator(NodeId::random(), "alice");
        let bob = MeshMember::new_member(NodeId::random(), "bob");
        MeshMembership::new_unsigned(nid, vec![coord, bob])
    }

    #[test]
    fn signing_preimage_changes_with_version() {
        let mut a = sample_roster();
        let b = sample_roster();
        assert_ne!(a.signing_preimage(), b.signing_preimage());
        a.bumped();
        assert_ne!(a.signing_preimage(), b.signing_preimage());
    }

    #[test]
    fn signing_preimage_is_excludes_signature_field() {
        // Mutating the stored `signature` hex must never affect the
        // preimage; otherwise an attacker could replay the same signed
        // preimage under a different signature and pass verification.
        let mut r = sample_roster();
        let pre = r.signing_preimage();
        r.set_signature_hex("deadbeef");
        assert_eq!(r.signing_preimage(), pre);
    }

    #[test]
    fn content_hash_hex_is_deterministic_64_chars() {
        let r = sample_roster();
        let h1 = r.content_hash_hex();
        let h2 = r.content_hash_hex();
        assert_eq!(h1, h2);
        // 32-byte BLAKE3 digest hex-encoded.
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Trivial in-process signer used by the verify tests below.
    struct MockSigner {
        sig: Vec<u8>,
        key: [u8; 32],
    }

    impl MeshRosterSigner for MockSigner {
        fn sign_roster(&self, _preimage: &[u8]) -> Vec<u8> {
            self.sig.clone()
        }
        fn public_key_bytes(&self) -> [u8; 32] {
            self.key
        }
    }

    struct MockVerifier {
        expect_key: [u8; 32],
        /// When true, always return `false`. Used to test rejection.
        always_fail: bool,
    }

    impl MeshRosterVerifier for MockVerifier {
        fn verify_roster(&self, _preimage: &[u8], signature: &[u8]) -> bool {
            // Pre-built fixed 64-byte accepted body.
            const OK: [u8; 64] = [
                b'g', b'o', b'o', b'd', b'-', b's', b'i', b'g', // 8
                b'-', b'6', b'4', b'-', b'b', b'y', b't', b'e', // 16
                b's', b'-', b'p', b'a', b'd', b'd', b'i', b'n', // 24
                b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n', // 32
                b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n', // 40
                b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n', // 48
                b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n', // 56
                b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n', // 64
            ];
            !self.always_fail && signature == OK.as_slice()
        }
    }

    #[test]
    fn verify_roster_signature_rejects_empty() {
        let mut r = sample_roster();
        r.set_signature_hex("");
        let v = MockVerifier {
            expect_key: [0u8; 32],
            always_fail: false,
        };
        assert!(!verify_roster_signature(&v, &r));
    }

    #[test]
    fn verify_roster_signature_rejects_wrong_length() {
        let mut r = sample_roster();
        r.set_signature_hex("aabb"); // 2 bytes after hex decode
        let v = MockVerifier {
            expect_key: [0u8; 32],
            always_fail: false,
        };
        assert!(!verify_roster_signature(&v, &r));
    }

    #[test]
    fn verify_roster_signature_rejects_non_hex() {
        let mut r = sample_roster();
        // Odd-length hex -> decoder fails.
        r.set_signature_hex("abc");
        let v = MockVerifier {
            expect_key: [0u8; 32],
            always_fail: false,
        };
        assert!(!verify_roster_signature(&v, &r));
    }

    #[test]
    fn verify_roster_signature_accepts_valid_64_byte_body() {
        let mut r = sample_roster();
        let ok = [
            b'g', b'o', b'o', b'd', b'-', b's', b'i', b'g', b'-', b'6', b'4', b'-', b'b', b'y',
            b't', b'e', b's', b'-', b'p', b'a', b'd', b'd', b'i', b'n', b'g', b'-', b'p', b'a',
            b'd', b'd', b'i', b'n', b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n', b'g', b'-',
            b'p', b'a', b'd', b'd', b'i', b'n', b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n',
            b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n',
        ];
        r.set_signature_hex(hex::encode(ok));
        let v = MockVerifier {
            expect_key: [0u8; 32],
            always_fail: false,
        };
        assert!(verify_roster_signature(&v, &r));
    }

    #[test]
    fn verify_roster_signature_propagates_verifier_rejection() {
        let mut r = sample_roster();
        let ok = [
            b'g', b'o', b'o', b'd', b'-', b's', b'i', b'g', b'-', b'6', b'4', b'-', b'b', b'y',
            b't', b'e', b's', b'-', b'p', b'a', b'd', b'd', b'i', b'n', b'g', b'-', b'p', b'a',
            b'd', b'd', b'i', b'n', b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n', b'g', b'-',
            b'p', b'a', b'd', b'd', b'i', b'n', b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n',
            b'g', b'-', b'p', b'a', b'd', b'd', b'i', b'n',
        ];
        r.set_signature_hex(hex::encode(ok));
        let v = MockVerifier {
            expect_key: [0u8; 32],
            always_fail: true,
        };
        assert!(!verify_roster_signature(&v, &r));
    }

    #[test]
    fn mock_signer_returns_configured_signature() {
        let s = MockSigner {
            sig: vec![1u8; 64],
            key: [9u8; 32],
        };
        let sig = s.sign_roster(b"any preimage");
        assert_eq!(sig, vec![1u8; 64]);
        assert_eq!(s.public_key_bytes(), [9u8; 32]);
    }
}
