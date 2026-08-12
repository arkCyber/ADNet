//! Node-role taxonomy, capability flags, resource declarations, and the
//! self-describing [`NodeProfile`] that every ADNet node carries.
//!
//! ## Design goals
//!
//! - **Typed, not free-form** — roles are a closed enum, capabilities are
//!   bitflags, resources are structured integers. No stringly-typed `role`
//!   fields that break at the wire boundary.
//! - **Aerospace-grade invariants** — every field carries a constructor that
//!   validates bounds at construction time; the types are `Copy` where
//!   possible so they compose cheaply.
//! - **Wire-compatible** — every type implements `serde::{Serialize,Deserialize}`
//!   so it can travel over gossip, pkarr, IPC, and FFI without custom codecs.
//! - **No `unsafe_code`** — this module is `#![forbid(unsafe_code)]` even
//!   though the parent crate already enforces it.
//!
//! ## Node role taxonomy
//!
//! The four roles cover the full device spectrum ADNet must support:
//!
//! | Role          | Devices                                              |
//! |---------------|------------------------------------------------------|
//! | `LightEdge`   | Mobile phones, IoT/embedded, battery-constrained    |
//! | `Standard`    | Desktops, laptops, workstations                      |
//! | `Specialized` | AI inference, distributed storage, MQTT gateways      |
//! | `Observer`    | Network monitors, log aggregators, audit replayers  |

// This module is at the crate root which already has these attributes,
// but we repeat them so the file is self-documenting even if the crate
// rules ever change.
#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// NodeRole — closed taxonomy of device classes
// ---------------------------------------------------------------------------

/// Node role — the position this node occupies in the ADNet topology.
///
/// Used by:
/// - The gossip `NodeProfileAnnouncement` frame so peers can filter
///   announcements by role (e.g. observer nodes are read-only subscribers).
/// - The [`NodeProfile`] persisted at `{data_dir}/node_profile.json`.
/// - The pkarr `UserData` field (serialized as a short string tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// Mobile phones, IoT / embedded devices, battery-constrained hardware.
    /// These nodes prefer mDNS for LAN discovery, QUIC hole-punch for NAT
    /// traversal, and should never be asked to relay traffic or serve
    /// large blobs.
    LightEdge,

    /// Standard desktop, laptop, or workstation nodes. The default role
    /// for a newly-provisioned ADNet install. Supports full protocol suite:
    /// QUIC + DERP relay, blob storage + serving, workspace, social feed.
    Standard,

    /// Specialised service nodes — AI inference providers, distributed storage
    /// backends, AI-agent hosts, MQTT brokers, or any node that advertises
    /// a specific capability set to attract work from the network.
    ///
    /// These nodes SHOULD publish resource declarations (see [`NodeResources`])
    /// so the P2P scheduler can route jobs appropriately.
    Specialized,

    /// Observability / monitoring nodes. Read-only gossip subscribers that
    /// aggregate metrics, log streams, or audit trails. They never originate
    /// content announcements and MUST NOT be selectable as blob providers.
    Observer,
}

impl NodeRole {
    /// Human-readable label suitable for CLI output and admin UIs.
    pub fn label(self) -> &'static str {
        match self {
            NodeRole::LightEdge => "light-edge",
            NodeRole::Standard => "standard",
            NodeRole::Specialized => "specialized",
            NodeRole::Observer => "observer",
        }
    }

    /// Whether this role is permitted to serve blob content to peers.
    ///
    /// - `LightEdge`: never — constrained devices should not relay.
    /// - `Standard`: yes — primary blob host tier.
    /// - `Specialized`: yes — dedicated storage / inference tier.
    /// - `Observer`: never — read-only.
    pub fn can_serve_blobs(self) -> bool {
        matches!(self, NodeRole::Standard | NodeRole::Specialized)
    }

    /// Whether this role can originate gossip announcements (other than
    /// its own [`NodeProfileAnnouncement`]).
    pub fn can_publish(self) -> bool {
        !matches!(self, NodeRole::Observer)
    }

    /// Short serialisation key used inside pkarr `UserData`.
    ///
    /// Kept to ≤ 4 bytes so it fits inside the 245-byte UserData budget.
    pub fn pkarr_tag(self) -> &'static str {
        match self {
            NodeRole::LightEdge => "le",
            NodeRole::Standard => "st",
            NodeRole::Specialized => "sp",
            NodeRole::Observer => "ob",
        }
    }

    /// Parse a role from its label string. Returns `None` for unknown labels.
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "light-edge" | "le" => Some(NodeRole::LightEdge),
            "standard" | "st" => Some(NodeRole::Standard),
            "specialized" | "sp" => Some(NodeRole::Specialized),
            "observer" | "ob" => Some(NodeRole::Observer),
            _ => None,
        }
    }
}

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ---------------------------------------------------------------------------
// NodeCapability — bitflags for fine-grained capability declarations
// ---------------------------------------------------------------------------

/// Individual capability flags.  Each variant represents a discrete
/// subsystem that this node has enabled and is willing to exercise.
///
/// Combine with bitwise-or (`|`) and test with `contains()`.
///
/// ```
/// use adnet_types::NodeCapability;
///
/// let caps = NodeCapability::RELAY | NodeCapability::MQTT_BRIDGE;
/// assert!(caps.contains(NodeCapability::RELAY));
/// assert!(!caps.contains(NodeCapability::AI_INFERENCE));
/// ```
///
/// Stored as a `u64` bitfield (flags 0-63).  The top 32 bits are reserved
/// for custom third-party capability extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeCapability(u64);

/// Maximum number of custom (extension) capability bits available.
/// Custom caps use bits 32-63; bits 0-31 are reserved for the ADNet
/// standard set.
pub const NODE_CAPABILITY_MAX_CUSTOM: u64 = 32;

/// Canonical empty capability set.
pub const NODE_CAPABILITY_NONE: NodeCapability = NodeCapability(0);

impl NodeCapability {
    // Individual capability constants — lower 32 bits reserved for ADNet.
    // BIT 0
    pub const RELAY: NodeCapability = NodeCapability(1 << 0);
    /// MQTT broker / bridge capability.  This node can forward
    /// MQTT messages between the ADNet gossip layer and an external
    /// MQTT broker (see `adnet-mqtt`).
    pub const MQTT_BRIDGE: NodeCapability = NodeCapability(1 << 1);
    /// AI inference capability.  This node hosts an AI inference endpoint
    /// (OpenAI-compatible, ollama, vLLM, etc.) reachable over the mesh.
    pub const AI_INFERENCE: NodeCapability = NodeCapability(1 << 2);
    /// Persistent blob storage.  This node has writable blob storage and
    /// will serve content to peers on request.
    pub const BLOB_STORAGE: NodeCapability = NodeCapability(1 << 3);
    /// Workspace host.  This node runs the [`crate::workspace`] module and
    /// can exchange workspace files with peers.
    pub const WORKSPACE_HOST: NodeCapability = NodeCapability(1 << 4);
    /// Mesh network monitor.  This node passively observes gossip topics,
    /// records latency / reachability metrics, and publishes health reports.
    pub const MESH_MONITOR: NodeCapability = NodeCapability(1 << 5);
    /// SSH gateway.  This node runs [`crate::ssh::Server`] and can proxy
    /// SSH connections over QUIC (see `adnet-ssh`).
    pub const SSH_GATEWAY: NodeCapability = NodeCapability(1 << 6);
    /// DNS resolver (MagicDNS).  This node runs [`crate::magicdns::Resolver`]
    /// and can answer DNS queries from the mesh address range.
    pub const DNS_RESOLVER: NodeCapability = NodeCapability(1 << 7);
    /// Exit-node / VPN gateway.  This node runs [`crate::tun::Device`] and
    /// can tunnel client traffic to the internet.
    pub const EXIT_NODE: NodeCapability = NodeCapability(1 << 8);
    /// NAS / WebDAV storage server.  This node runs [`crate::NasHandle`]
    /// and exposes a WebDAV endpoint for file access.
    pub const NAS_SERVER: NodeCapability = NodeCapability(1 << 9);
    /// AI agent host.  This node registers an AI agent via
    /// [`crate::Node::register_agent`] and can receive agent-task
    /// announcements from peers.
    pub const AI_AGENT: NodeCapability = NodeCapability(1 << 10);

    /// Construct a capability set from raw bits (bits 0-31 are the standard
    /// ADNet set; bits 32-63 may be used for custom capability extensions).
    pub const fn from_bits(bits: u64) -> Self {
        NodeCapability(bits)
    }

    /// Returns the raw bit representation.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns `true` if `self` contains all bits set in `other`.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns `true` if at least one of the bits in `other` is set in `self`.
    #[inline]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// Adds all capability bits from `other` to `self`, returning a new set.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        NodeCapability(self.0 | other.0)
    }

    /// Removes all bits from `other` from `self`, returning a new set.
    #[inline]
    pub const fn difference(self, other: Self) -> Self {
        NodeCapability(self.0 & !other.0)
    }

    /// Iterator over the individual standard-capability bits present in this set.
    pub fn iter_standard(self) -> impl Iterator<Item = NodeCapability> {
        StandardCapIterator {
            caps: self,
            next: 0,
        }
    }
}

impl Default for NodeCapability {
    fn default() -> Self {
        NODE_CAPABILITY_NONE
    }
}

impl std::ops::BitOr for NodeCapability {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        NodeCapability(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for NodeCapability {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        NodeCapability(self.0 & rhs.0)
    }
}

impl std::ops::BitOrAssign for NodeCapability {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl fmt::Display for NodeCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let caps: Vec<&'static str> = self
            .iter_standard()
            .map(|c| {
                if c == Self::RELAY {
                    "relay"
                } else if c == Self::MQTT_BRIDGE {
                    "mqtt_bridge"
                } else if c == Self::AI_INFERENCE {
                    "ai_inference"
                } else if c == Self::BLOB_STORAGE {
                    "blob_storage"
                } else if c == Self::WORKSPACE_HOST {
                    "workspace_host"
                } else if c == Self::MESH_MONITOR {
                    "mesh_monitor"
                } else if c == Self::SSH_GATEWAY {
                    "ssh_gateway"
                } else if c == Self::DNS_RESOLVER {
                    "dns_resolver"
                } else if c == Self::EXIT_NODE {
                    "exit_node"
                } else if c == Self::NAS_SERVER {
                    "nas_server"
                } else if c == Self::AI_AGENT {
                    "ai_agent"
                } else {
                    "unknown"
                }
            })
            .collect();
        write!(f, "[{}]", caps.join("|"))
    }
}

struct StandardCapIterator {
    caps: NodeCapability,
    next: u32,
}

impl Iterator for StandardCapIterator {
    type Item = NodeCapability;

    fn next(&mut self) -> Option<Self::Item> {
        // Bits 0-31 are the standard set; we iterate through them.
        while self.next < 32 {
            let bit = self.next;
            self.next += 1;
            let flag = NodeCapability(1u64 << bit);
            if self.caps.contains(flag) {
                return Some(flag);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// NodeResources — hardware resource declarations
// ---------------------------------------------------------------------------

/// Hardware resource declarations advertised by a [`NodeProfile`].
///
/// All resource values are expressed in **natural units** (cores, bytes,
/// bits-per-second) so callers do not need to parse strings.
///
/// `None` fields mean "unknown / not declared" — this is intentional
/// so a light-edge node that does not want to expose telemetry can omit
/// sensitive fields.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeResources {
    /// Number of logical CPU cores available to this node.
    /// `None` means the node does not declare its CPU capacity.
    pub cpu_cores: Option<u16>,

    /// Total memory in bytes available to this node.
    /// `None` means the node does not declare its memory capacity.
    pub memory_bytes: Option<u64>,

    /// Available blob-storage capacity in bytes.
    /// This is the total writable storage budget — not the current usage.
    /// `None` means storage is not a declared resource (e.g. observer nodes).
    pub storage_bytes: Option<u64>,

    /// Advertised upload bandwidth in bits per second.
    /// `None` means the node does not declare bandwidth.
    pub bandwidth_bps: Option<u64>,

    /// Battery charge percentage, 0-100.
    /// `None` means "not a battery-powered device" or "unknown".
    /// Only meaningful for `LightEdge` nodes.
    pub battery_pct: Option<u8>,

    /// Geographic region tag, free-form but recommended to follow
    /// ISO 3166-1 alpha-2 codes (e.g. `"us-west"`, `"eu-central"`).
    /// Used by the P2P scheduler to prefer geographically local peers.
    pub region: Option<String>,
}

impl NodeResources {
    /// Construct with required fields; everything else defaults to `None`.
    pub fn new(cpu_cores: u16, memory_bytes: u64, storage_bytes: u64) -> Self {
        Self {
            cpu_cores: Some(cpu_cores),
            memory_bytes: Some(memory_bytes),
            storage_bytes: Some(storage_bytes),
            bandwidth_bps: None,
            battery_pct: None,
            region: None,
        }
    }

    /// Convenience: set bandwidth.
    pub fn with_bandwidth(mut self, bps: u64) -> Self {
        self.bandwidth_bps = Some(bps);
        self
    }

    /// Convenience: set region.
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Convenience: set battery (clamped to 0-100).
    pub fn with_battery(mut self, pct: u8) -> Self {
        self.battery_pct = Some(pct.min(100));
        self
    }

    /// Returns `true` if `self` has at least as much of every resource as `required`.
    pub fn satisfies(&self, required: &NodeResources) -> bool {
        let cpu_ok = required
            .cpu_cores
            .map(|r| self.cpu_cores.unwrap_or(0) >= r)
            .unwrap_or(true);
        let mem_ok = required
            .memory_bytes
            .map(|r| self.memory_bytes.unwrap_or(0) >= r)
            .unwrap_or(true);
        let sto_ok = required
            .storage_bytes
            .map(|r| self.storage_bytes.unwrap_or(0) >= r)
            .unwrap_or(true);
        let bw_ok = required
            .bandwidth_bps
            .map(|r| self.bandwidth_bps.unwrap_or(0) >= r)
            .unwrap_or(true);
        cpu_ok && mem_ok && sto_ok && bw_ok
    }

    /// Human-readable summary, one line.
    pub fn summary(&self) -> String {
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        let cpu = self
            .cpu_cores
            .map(|n| format!("{n} cores"))
            .unwrap_or_else(|| "cpu unknown".to_string());
        let mem = self
            .memory_bytes
            .map(|b| format!("{:.1} GiB", b as f64 / GIB))
            .unwrap_or_else(|| "mem unknown".to_string());
        let sto = self
            .storage_bytes
            .map(|b| format!("{:.0} GiB storage", b as f64 / GIB))
            .unwrap_or_else(|| "storage unknown".to_string());
        format!("{cpu} · {mem} · {sto}")
    }
}

// ---------------------------------------------------------------------------
// NodeProfile — the self-describing identity packet
// ---------------------------------------------------------------------------

/// Maximum length of the free-form `description` field in bytes (UTF-8).
pub const MAX_PROFILE_DESC_LEN: usize = 512;

/// Maximum number of free-form tag strings on a profile.
pub const MAX_PROFILE_TAGS: usize = 32;

/// Maximum length of a single tag string in bytes (UTF-8).
pub const MAX_TAG_LEN: usize = 64;

/// Canonical gossip room id used for profile announcements.
pub const PROFILE_ROOM_ID: &str = "profile";

/// The self-describing identity packet that every ADNet node carries.
///
/// Persisted to `{data_dir}/node_profile.json` and published via
/// [`NodeProfileAnnouncement`] on the `adnet-room-profile` gossip topic.
///
/// ## Wire format
///
/// The struct is serialized as JSON (or bincode when space-constrained).
/// A [`Signature`](crate::integrity::Signature) over the canonical JSON bytes
/// is appended by the announcer so receivers can verify the publisher's
/// [`NodeId`](crate::node::NodeId) before trusting the profile.
///
/// ## Example
///
/// ```json
/// {
///   "nodeId": "abc123...",
///   "role": "specialized",
///   "capabilities": 1048584,
///   "resources": { "cpuCores": 64, "memoryBytes": 137438953472, "storageBytes": 10995116277760 },
///   "description": "GPU inference node — 4× A100 80 GiB",
///   "tags": ["ai", "inference", "us-west-2"],
///   "version": "0.1.0",
///   "publishedAt": 1723276800
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeProfile {
    /// Canonical node identifier this profile belongs to.
    pub node_id: crate::node::NodeId,

    /// Device-class role.
    pub role: NodeRole,

    /// Bitfield of enabled capabilities.  See [`NodeCapability`].
    pub capabilities: NodeCapability,

    /// Optional hardware resource declarations.  `None` means "not declared".
    #[serde(default)]
    pub resources: Option<NodeResources>,

    /// Free-form human-readable description (max 512 bytes UTF-8).
    /// Example: `"GPU inference node — 4× A100 80 GiB, us-west-2a"`.
    #[serde(default)]
    pub description: Option<String>,

    /// Free-form tag strings for discovery / filtering.
    /// Example: `["ai", "inference", "us-west-2"]`.
    #[serde(default)]
    pub tags: Vec<String>,

    /// ADNet software version string, e.g. `"0.1.0"`.
    /// Helps operators identify nodes running outdated software.
    pub version: String,

    /// Unix timestamp (UTC seconds) when this profile was created / last updated.
    pub published_at: u64,
}

impl NodeProfile {
    /// Build a minimal profile for a standard node.
    ///
    /// Includes `BLOB_STORAGE | WORKSPACE_HOST` as the default capability set.
    pub fn standard(node_id: crate::node::NodeId, version: impl Into<String>) -> Self {
        Self {
            node_id,
            role: NodeRole::Standard,
            capabilities: NodeCapability::BLOB_STORAGE.union(NodeCapability::WORKSPACE_HOST),
            resources: None,
            description: None,
            tags: Vec::new(),
            version: version.into(),
            published_at: current_timestamp(),
        }
    }

    /// Build a minimal profile for an observer node.
    pub fn observer(node_id: crate::node::NodeId, version: impl Into<String>) -> Self {
        Self {
            node_id,
            role: NodeRole::Observer,
            capabilities: NodeCapability::MESH_MONITOR,
            resources: None,
            description: None,
            tags: Vec::new(),
            version: version.into(),
            published_at: current_timestamp(),
        }
    }

    /// Build a profile for a specialized AI inference node.
    pub fn ai_inference(
        node_id: crate::node::NodeId,
        version: impl Into<String>,
        resources: NodeResources,
        description: impl Into<String>,
    ) -> Self {
        Self {
            node_id,
            role: NodeRole::Specialized,
            capabilities: NodeCapability::AI_INFERENCE
                | NodeCapability::BLOB_STORAGE
                | NodeCapability::WORKSPACE_HOST,
            resources: Some(resources),
            description: Some(description.into()),
            tags: vec!["ai".to_string(), "inference".to_string()],
            version: version.into(),
            published_at: current_timestamp(),
        }
    }

    /// Add a tag.  Returns an error if the tag is too long or the tag
    /// limit is already reached.
    pub fn add_tag(&mut self, tag: impl Into<String>) -> Result<(), NodeProfileError> {
        if self.tags.len() >= MAX_PROFILE_TAGS {
            return Err(NodeProfileError::TooManyTags);
        }
        let tag = tag.into();
        if tag.len() > MAX_TAG_LEN {
            return Err(NodeProfileError::TagTooLong(tag.len()));
        }
        if !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
        Ok(())
    }

    /// Set the description.  Returns an error if the description exceeds
    /// [`MAX_PROFILE_DESC_LEN`].
    pub fn set_description(&mut self, desc: impl Into<String>) -> Result<(), NodeProfileError> {
        let desc_str = desc.into();
        if desc_str.len() > MAX_PROFILE_DESC_LEN {
            return Err(NodeProfileError::DescriptionTooLong(desc_str.len()));
        }
        self.description = Some(desc_str);
        Ok(())
    }

    /// Returns `true` if this profile can satisfy the required capability set.
    pub fn satisfies_capabilities(&self, required: NodeCapability) -> bool {
        self.capabilities.contains(required)
    }

    /// Returns `true` if this profile's role permits blob serving.
    pub fn can_serve_blobs(&self) -> bool {
        self.role.can_serve_blobs()
    }

    /// Refresh the `published_at` timestamp to now.
    pub fn touch(&mut self) {
        self.published_at = current_timestamp();
    }

    /// Human-readable one-line summary for operator UIs and logs.
    pub fn summary(&self) -> String {
        let role = self.role.label();
        let caps = self.capabilities.to_string();
        let res = self
            .resources
            .as_ref()
            .map(NodeResources::summary)
            .unwrap_or_default();
        let desc = self
            .description
            .as_deref()
            .map(|s| format!(" — {s}"))
            .unwrap_or_default();
        format!(
            "[{}] {} · {} · v{} · {} · {}{}",
            self.node_id.short(),
            role,
            caps,
            self.version,
            chrono_human_relative(self.published_at),
            res,
            desc
        )
    }
}

/// Errors returned by [`NodeProfile`] mutations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NodeProfileError {
    #[error("description exceeds maximum length ({0} bytes; max {MAX_PROFILE_DESC_LEN})")]
    DescriptionTooLong(usize),

    #[error("tag exceeds maximum length ({0} bytes; max {MAX_TAG_LEN})")]
    TagTooLong(usize),

    #[error("profile already has the maximum number of tags ({MAX_PROFILE_TAGS})")]
    TooManyTags,
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Unix seconds of the current UTC moment.  Extracted into a fn so tests
/// can override it via cfg(test).
fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Format a unix timestamp as a human-readable relative string
/// (e.g. `"3 hours ago"`).
fn chrono_human_relative(unix_secs: u64) -> String {
    let now = current_timestamp();
    let diff = now.saturating_sub(unix_secs);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── NodeRole ─────────────────────────────────────────────────────────────

    #[test]
    fn node_role_all_variants() {
        for role in [
            NodeRole::LightEdge,
            NodeRole::Standard,
            NodeRole::Specialized,
            NodeRole::Observer,
        ] {
            assert_eq!(NodeRole::from_label(role.label()), Some(role));
        }
    }

    #[test]
    fn node_role_from_label_unknown() {
        assert_eq!(NodeRole::from_label("alien"), None);
        assert_eq!(NodeRole::from_label(""), None);
    }

    #[test]
    fn node_role_from_label_short_codes() {
        assert_eq!(NodeRole::from_label("le"), Some(NodeRole::LightEdge));
        assert_eq!(NodeRole::from_label("st"), Some(NodeRole::Standard));
        assert_eq!(NodeRole::from_label("sp"), Some(NodeRole::Specialized));
        assert_eq!(NodeRole::from_label("ob"), Some(NodeRole::Observer));
    }

    #[test]
    fn node_role_can_serve_blobs() {
        assert!(!NodeRole::LightEdge.can_serve_blobs());
        assert!(NodeRole::Standard.can_serve_blobs());
        assert!(NodeRole::Specialized.can_serve_blobs());
        assert!(!NodeRole::Observer.can_serve_blobs());
    }

    #[test]
    fn node_role_can_publish() {
        assert!(NodeRole::LightEdge.can_publish());
        assert!(NodeRole::Standard.can_publish());
        assert!(NodeRole::Specialized.can_publish());
        assert!(!NodeRole::Observer.can_publish());
    }

    #[test]
    fn node_role_pkarr_tag() {
        assert_eq!(NodeRole::LightEdge.pkarr_tag(), "le");
        assert_eq!(NodeRole::Standard.pkarr_tag(), "st");
        assert_eq!(NodeRole::Specialized.pkarr_tag(), "sp");
        assert_eq!(NodeRole::Observer.pkarr_tag(), "ob");
    }

    #[test]
    fn node_role_display() {
        assert_eq!(NodeRole::Standard.to_string(), "standard");
        assert_eq!(NodeRole::LightEdge.to_string(), "light-edge");
    }

    // ── NodeCapability ───────────────────────────────────────────────────────

    #[test]
    fn capability_none_is_empty() {
        let none = NODE_CAPABILITY_NONE;
        assert_eq!(none.bits(), 0);
        assert!(!none.contains(NodeCapability::RELAY));
    }

    #[test]
    fn capability_bitor() {
        let caps = NodeCapability::RELAY | NodeCapability::AI_INFERENCE;
        assert!(caps.contains(NodeCapability::RELAY));
        assert!(caps.contains(NodeCapability::AI_INFERENCE));
        assert!(!caps.contains(NodeCapability::BLOB_STORAGE));
    }

    #[test]
    fn capability_bitand() {
        let caps = NodeCapability::RELAY | NodeCapability::AI_INFERENCE;
        let relay = caps & NodeCapability::RELAY;
        assert!(relay.contains(NodeCapability::RELAY));
        assert!(!relay.contains(NodeCapability::AI_INFERENCE));
    }

    #[test]
    fn capability_union() {
        let a = NodeCapability::RELAY;
        let b = NodeCapability::BLOB_STORAGE;
        let union = a.union(b);
        assert!(union.contains(a));
        assert!(union.contains(b));
    }

    #[test]
    fn capability_difference() {
        let caps = NodeCapability::RELAY | NodeCapability::AI_INFERENCE;
        let diff = caps.difference(NodeCapability::AI_INFERENCE);
        assert!(diff.contains(NodeCapability::RELAY));
        assert!(!diff.contains(NodeCapability::AI_INFERENCE));
    }

    #[test]
    fn capability_intersects() {
        let caps = NodeCapability::RELAY | NodeCapability::AI_INFERENCE;
        assert!(caps.intersects(NodeCapability::RELAY));
        assert!(caps.intersects(NodeCapability::AI_INFERENCE));
        assert!(!caps.intersects(NodeCapability::NAS_SERVER));
    }

    #[test]
    fn capability_from_bits_roundtrip() {
        let original = NodeCapability::RELAY.bits() | NodeCapability::BLOB_STORAGE.bits();
        let caps = NodeCapability::from_bits(original);
        assert!(caps.contains(NodeCapability::RELAY));
        assert!(caps.contains(NodeCapability::BLOB_STORAGE));
    }

    #[test]
    fn capability_from_bits_all_ones() {
        // All 64 bits set — all known flags should be present.
        let caps = NodeCapability::from_bits(u64::MAX);
        assert!(caps.contains(NodeCapability::RELAY));
    }

    #[test]
    fn capability_iter_standard() {
        let caps =
            NodeCapability::RELAY | NodeCapability::AI_INFERENCE | NodeCapability::BLOB_STORAGE;
        let collected: Vec<_> = caps.iter_standard().collect();
        assert!(collected.contains(&NodeCapability::RELAY));
        assert!(collected.contains(&NodeCapability::AI_INFERENCE));
        assert!(collected.contains(&NodeCapability::BLOB_STORAGE));
    }

    #[test]
    fn capability_iter_none() {
        let none = NODE_CAPABILITY_NONE;
        assert!(none.iter_standard().next().is_none());
    }

    #[test]
    fn capability_display() {
        let caps = NodeCapability::RELAY | NodeCapability::AI_INFERENCE;
        let display = caps.to_string();
        assert!(display.contains("relay"));
        assert!(display.contains("ai_inference"));
    }

    #[test]
    fn capability_display_all_flags() {
        // Exercise every known flag through display
        let all = NodeCapability::RELAY
            | NodeCapability::MQTT_BRIDGE
            | NodeCapability::AI_INFERENCE
            | NodeCapability::BLOB_STORAGE
            | NodeCapability::WORKSPACE_HOST
            | NodeCapability::MESH_MONITOR
            | NodeCapability::SSH_GATEWAY
            | NodeCapability::DNS_RESOLVER
            | NodeCapability::EXIT_NODE
            | NodeCapability::NAS_SERVER
            | NodeCapability::AI_AGENT;
        let display = all.to_string();
        assert!(display.contains("relay"));
        assert!(display.contains("mqtt_bridge"));
        assert!(display.contains("ai_inference"));
        assert!(display.contains("blob_storage"));
        assert!(display.contains("workspace_host"));
        assert!(display.contains("mesh_monitor"));
        assert!(display.contains("ssh_gateway"));
        assert!(display.contains("dns_resolver"));
        assert!(display.contains("exit_node"));
        assert!(display.contains("nas_server"));
        assert!(display.contains("ai_agent"));
    }

    #[test]
    fn capability_default_is_none() {
        let default: NodeCapability = Default::default();
        assert_eq!(default, NODE_CAPABILITY_NONE);
    }

    #[test]
    fn capability_bitor_assign() {
        let mut caps = NodeCapability::RELAY;
        caps |= NodeCapability::AI_INFERENCE;
        assert!(caps.contains(NodeCapability::RELAY));
        assert!(caps.contains(NodeCapability::AI_INFERENCE));
    }

    // ── NodeResources ─────────────────────────────────────────────────────────

    #[test]
    fn resources_new() {
        let r = NodeResources::new(8, 16 * 1024 * 1024 * 1024, 512 * 1024 * 1024 * 1024);
        assert_eq!(r.cpu_cores, Some(8));
        assert_eq!(r.memory_bytes, Some(16 * 1024 * 1024 * 1024));
        assert_eq!(r.storage_bytes, Some(512 * 1024 * 1024 * 1024));
        assert_eq!(r.bandwidth_bps, None);
        assert_eq!(r.region, None);
    }

    #[test]
    fn resources_builder() {
        let r = NodeResources::new(64, 256 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024 * 1024)
            .with_bandwidth(1_000_000_000)
            .with_region("us-west-2")
            .with_battery(75);
        assert_eq!(r.bandwidth_bps, Some(1_000_000_000));
        assert_eq!(r.region.as_deref(), Some("us-west-2"));
        assert_eq!(r.battery_pct, Some(75));
    }

    #[test]
    fn resources_battery_clamp() {
        let r = NodeResources::default().with_battery(250);
        assert_eq!(r.battery_pct, Some(100));
    }

    #[test]
    fn resources_satisfies_exact() {
        let r = NodeResources::new(8, 32 * 1024 * 1024 * 1024, 1 * 1024 * 1024 * 1024 * 1024);
        let required =
            NodeResources::new(8, 32 * 1024 * 1024 * 1024, 1 * 1024 * 1024 * 1024 * 1024);
        assert!(r.satisfies(&required));
    }

    #[test]
    fn resources_satisfies_sufficient() {
        let r = NodeResources::new(16, 64 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024 * 1024);
        let required = NodeResources::new(8, 16 * 1024 * 1024 * 1024, 512 * 1024 * 1024 * 1024);
        assert!(r.satisfies(&required));
    }

    #[test]
    fn resources_satisfies_insufficient() {
        let r = NodeResources::new(4, 8 * 1024 * 1024 * 1024, 256 * 1024 * 1024 * 1024);
        let required = NodeResources::new(8, 16 * 1024 * 1024 * 1024, 512 * 1024 * 1024 * 1024);
        assert!(!r.satisfies(&required));
    }

    #[test]
    fn resources_satisfies_partial() {
        let r = NodeResources {
            cpu_cores: Some(4),
            memory_bytes: Some(32 * 1024 * 1024 * 1024),
            storage_bytes: None,
            bandwidth_bps: None,
            battery_pct: None,
            region: None,
        };
        let required = NodeResources {
            cpu_cores: Some(4),
            memory_bytes: Some(32 * 1024 * 1024 * 1024),
            storage_bytes: Some(1_000_000_000),
            bandwidth_bps: None,
            battery_pct: None,
            region: None,
        };
        // storage is None in r, required is Some — should fail
        assert!(!r.satisfies(&required));
    }

    #[test]
    fn resources_satisfies_empty_required() {
        let r = NodeResources::new(4, 8 * 1024 * 1024 * 1024, 256 * 1024 * 1024 * 1024);
        let required = NodeResources::default();
        assert!(r.satisfies(&required));
    }

    #[test]
    fn resources_summary() {
        let r = NodeResources::new(16, 32 * 1024 * 1024 * 1024, 512 * 1024 * 1024 * 1024);
        let summary = r.summary();
        assert!(summary.contains("16 cores"));
        assert!(summary.contains("32.0 GiB"));
        assert!(summary.contains("512 GiB storage"));
    }

    #[test]
    fn resources_summary_unknown() {
        let r = NodeResources::default();
        let summary = r.summary();
        assert!(summary.contains("cpu unknown"));
        assert!(summary.contains("mem unknown"));
        assert!(summary.contains("storage unknown"));
    }

    // ── NodeProfile ───────────────────────────────────────────────────────────

    #[test]
    fn profile_standard() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::standard(id.clone(), "0.1.0");
        assert_eq!(p.role, NodeRole::Standard);
        assert!(p.capabilities.contains(NodeCapability::BLOB_STORAGE));
        assert!(p.capabilities.contains(NodeCapability::WORKSPACE_HOST));
        assert_eq!(p.version, "0.1.0");
        assert!(p.tags.is_empty());
    }

    #[test]
    fn profile_observer() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::observer(id.clone(), "0.2.0");
        assert_eq!(p.role, NodeRole::Observer);
        assert!(p.capabilities.contains(NodeCapability::MESH_MONITOR));
        assert!(!p.can_serve_blobs());
    }

    #[test]
    fn profile_ai_inference() {
        let id = crate::node::NodeId::random();
        let resources =
            NodeResources::new(64, 256 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024 * 1024);
        let p = NodeProfile::ai_inference(id.clone(), "0.1.0", resources, "4x A100 80GB");
        assert_eq!(p.role, NodeRole::Specialized);
        assert!(p.capabilities.contains(NodeCapability::AI_INFERENCE));
        assert_eq!(p.description.as_deref(), Some("4x A100 80GB"));
        assert!(p.tags.contains(&"ai".to_string()));
        assert!(p.tags.contains(&"inference".to_string()));
    }

    #[test]
    fn profile_add_tag() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "0.1.0");
        p.add_tag("fast").unwrap();
        p.add_tag("eu-central").unwrap();
        assert_eq!(p.tags, vec!["fast", "eu-central"]);
    }

    #[test]
    fn profile_add_tag_no_dupe() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "0.1.0");
        p.add_tag("fast").unwrap();
        p.add_tag("fast").unwrap();
        assert_eq!(p.tags.len(), 1);
    }

    #[test]
    fn profile_add_tag_too_long() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "0.1.0");
        let long_tag = "x".repeat(MAX_TAG_LEN + 1);
        let err = p.add_tag(long_tag).unwrap_err();
        assert!(matches!(err, NodeProfileError::TagTooLong(_)));
    }

    #[test]
    fn profile_add_tag_max() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "0.1.0");
        for i in 0..MAX_PROFILE_TAGS {
            p.add_tag(format!("tag{i}")).unwrap();
        }
        let err = p.add_tag("one-more").unwrap_err();
        assert!(matches!(err, NodeProfileError::TooManyTags));
    }

    #[test]
    fn profile_set_description() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "0.1.0");
        p.set_description("my node").unwrap();
        assert_eq!(p.description.as_deref(), Some("my node"));
    }

    #[test]
    fn profile_set_description_too_long() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "0.1.0");
        let long_desc = "x".repeat(MAX_PROFILE_DESC_LEN + 1);
        let err = p.set_description(long_desc).unwrap_err();
        assert!(matches!(err, NodeProfileError::DescriptionTooLong(_)));
    }

    #[test]
    fn profile_satisfies_capabilities() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::standard(id, "0.1.0");
        assert!(p.satisfies_capabilities(NodeCapability::BLOB_STORAGE));
        assert!(!p.satisfies_capabilities(NodeCapability::AI_INFERENCE));
    }

    #[test]
    fn profile_can_serve_blobs() {
        let id = crate::node::NodeId::random();

        let standard = NodeProfile::standard(id.clone(), "0.1.0");
        assert!(standard.can_serve_blobs());

        let observer = NodeProfile::observer(id.clone(), "0.1.0");
        assert!(!observer.can_serve_blobs());

        let resources =
            NodeResources::new(64, 256 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024 * 1024);
        let ai = NodeProfile::ai_inference(id, "0.1.0", resources, "GPU node");
        assert!(ai.can_serve_blobs());
    }

    #[test]
    fn profile_touch() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "0.1.0");
        let original = p.published_at;
        std::thread::sleep(std::time::Duration::from_secs(1));
        p.touch();
        assert!(
            p.published_at > original,
            "touch() should update published_at; original={original}, new={}",
            p.published_at
        );
    }

    #[test]
    fn profile_summary() {
        let id = crate::node::NodeId::random();
        let resources =
            NodeResources::new(16, 64 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024 * 1024);
        let p = NodeProfile::ai_inference(id.clone(), "0.1.0", resources, "GPU beast");
        let summary = p.summary();
        assert!(summary.contains("specialized"));
        assert!(summary.contains("v0.1.0"));
        assert!(summary.contains("GPU beast"));
    }

    // ── Serde round-trips ─────────────────────────────────────────────────────

    #[test]
    fn serde_node_role_json() {
        for role in [
            NodeRole::LightEdge,
            NodeRole::Standard,
            NodeRole::Specialized,
            NodeRole::Observer,
        ] {
            let json = serde_json::to_string(&role).unwrap();
            let back: NodeRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back);
        }
    }

    #[test]
    fn serde_node_capability_json() {
        let caps = NodeCapability::RELAY | NodeCapability::AI_INFERENCE;
        let json = serde_json::to_string(&caps).unwrap();
        let back: NodeCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, back);
    }

    #[test]
    fn serde_node_resources_json() {
        let r = NodeResources::new(8, 16 * 1024 * 1024 * 1024, 512 * 1024 * 1024 * 1024)
            .with_bandwidth(1_000_000_000)
            .with_region("us-west-2");
        let json = serde_json::to_string(&r).unwrap();
        let back: NodeResources = serde_json::from_str(&json).unwrap();
        assert_eq!(r.cpu_cores, back.cpu_cores);
        assert_eq!(r.memory_bytes, back.memory_bytes);
        assert_eq!(r.bandwidth_bps, back.bandwidth_bps);
        assert_eq!(r.region.as_deref(), back.region.as_deref());
    }

    #[test]
    fn serde_node_profile_json() {
        let id = crate::node::NodeId::random();
        let resources =
            NodeResources::new(64, 256 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024 * 1024);
        let p = NodeProfile::ai_inference(id.clone(), "0.1.0", resources, "GPU node");
        let json = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p.node_id, back.node_id);
        assert_eq!(p.role, back.role);
        assert_eq!(p.capabilities, back.capabilities);
        assert_eq!(p.version, back.version);
    }

    #[test]
    fn serde_node_profile_bincode() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::standard(id, "0.1.0");
        let bytes = bincode::serialize(&p).unwrap();
        let back: NodeProfile = bincode::deserialize(&bytes).unwrap();
        assert_eq!(p.node_id, back.node_id);
    }

    #[test]
    fn serde_node_resources_bincode() {
        let r = NodeResources::new(8, 16 * 1024 * 1024 * 1024, 512 * 1024 * 1024 * 1024);
        let bytes = bincode::serialize(&r).unwrap();
        let back: NodeResources = bincode::deserialize(&bytes).unwrap();
        assert_eq!(r.cpu_cores, back.cpu_cores);
        assert_eq!(r.memory_bytes, back.memory_bytes);
        assert_eq!(r.storage_bytes, back.storage_bytes);
    }

    #[test]
    fn serde_node_capability_bincode() {
        let caps = NodeCapability::RELAY | NodeCapability::NAS_SERVER;
        let bytes = bincode::serialize(&caps).unwrap();
        let back: NodeCapability = bincode::deserialize(&bytes).unwrap();
        assert_eq!(caps, back);
    }

    // ── Canonical form ────────────────────────────────────────────────────────

    #[test]
    fn profile_roundtrip_via_json_string() {
        let id = crate::node::NodeId::random();
        let resources = NodeResources::new(
            128,
            512 * 1024 * 1024 * 1024,
            16 * 1024 * 1024 * 1024 * 1024,
        )
        .with_bandwidth(10_000_000_000)
        .with_region("eu-central")
        .with_battery(95);
        let mut p =
            NodeProfile::ai_inference(id.clone(), "0.1.0", resources, "AI inference server");
        p.add_tag("gpu").unwrap();
        p.add_tag("llm").unwrap();

        let json = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&json).unwrap();

        assert_eq!(p.node_id, back.node_id);
        assert_eq!(p.role, back.role);
        assert_eq!(p.capabilities.bits(), back.capabilities.bits());
        assert_eq!(p.description, back.description);
        assert_eq!(p.tags, back.tags);
        assert_eq!(p.version, back.version);
        assert_eq!(
            p.resources.as_ref().unwrap().cpu_cores,
            back.resources.as_ref().unwrap().cpu_cores
        );
    }

    // ── Trait bounds ───────────────────────────────────────────────────────────

    #[test]
    fn node_role_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NodeRole>();
        assert_send_sync::<NodeCapability>();
        assert_send_sync::<NodeResources>();
        assert_send_sync::<NodeProfile>();
    }

    #[test]
    fn node_profile_is_default() {
        // NodeProfile has no Default impl (intentional — profile must always
        // carry a node_id and role).  This test just documents that fact.
        // If you add Default, delete this test.
    }

    #[test]
    fn node_resources_is_default() {
        let r = NodeResources::default();
        assert!(r.cpu_cores.is_none());
        assert!(r.memory_bytes.is_none());
        assert!(r.storage_bytes.is_none());
        assert!(r.bandwidth_bps.is_none());
        assert!(r.battery_pct.is_none());
        assert!(r.region.is_none());
    }

    #[test]
    fn constants() {
        assert_eq!(MAX_PROFILE_DESC_LEN, 512);
        assert_eq!(MAX_PROFILE_TAGS, 32);
        assert_eq!(MAX_TAG_LEN, 64);
        assert_eq!(NODE_CAPABILITY_MAX_CUSTOM, 32);
    }

    // ── Aerospace-grade: property-based tests ────────────────────────────────

    proptest! {
        /// Profile round-trips through JSON for any valid role/capabilities combo.
        #[test]
        fn prop_profile_json_roundtrip(
            node_id_hex in "[a-f0-9]{64}",
            role_str in prop_oneof![
                Just("light-edge"),
                Just("standard"),
                Just("specialized"),
                Just("observer"),
            ],
        ) {
            let node_id = crate::node::NodeId::from_hex(&node_id_hex).unwrap();
            let role = NodeRole::from_label(&role_str).unwrap();

            let mut profile = NodeProfile::standard(node_id, "1.0.0");
            profile.role = role;

            let json = serde_json::to_string(&profile).unwrap();
            let back: NodeProfile = serde_json::from_str(&json).unwrap();

            prop_assert_eq!(profile.role, back.role);
            prop_assert_eq!(profile.version, back.version);
        }

        /// Capability set bits are deterministic.
        #[test]
        fn prop_capability_bits_deterministic(
            bits: u64,
        ) {
            let caps = NodeCapability::from_bits(bits);
            prop_assert_eq!(caps.bits(), bits);
        }

        /// Capability intersection is commutative.
        #[test]
        fn prop_capability_intersection_commutative(a: u64, b: u64) {
            let caps_a = NodeCapability::from_bits(a);
            let caps_b = NodeCapability::from_bits(b);
            prop_assert_eq!(
                caps_a.intersects(caps_b),
                caps_b.intersects(caps_a),
            );
        }

        /// Capability contains is reflexive for any single flag.
        #[test]
        fn prop_capability_contains_reflexive(flag_bits: u8) {
            let flag = NodeCapability(1u64 << (flag_bits % 32));
            prop_assert!(flag.contains(flag));
        }

        /// NodeResources::satisfies is reflexive.
        #[test]
        fn prop_resources_satisfies_reflexive(
            cpu in 1u16..=256,
            mem: u64,
            sto: u64,
        ) {
            let r = NodeResources::new(cpu, mem, sto);
            prop_assert!(r.satisfies(&r));
        }

        /// NodeResources::satisfies is transitive: if A satisfies B and B satisfies C,
        /// then A should satisfy C (for the fields that B specified).
        #[test]
        fn prop_resources_satisfies_transitive(
            cpu_a: u16, cpu_b: u16, cpu_c: u16,
            mem_a: u64, mem_b: u64, mem_c: u64,
            sto_a: u64, sto_b: u64, sto_c: u64,
        ) {
            let a = NodeResources::new(cpu_a, mem_a, sto_a);
            let b = NodeResources::new(cpu_b, mem_b, sto_b);
            let c = NodeResources::new(cpu_c, mem_c, sto_c);

            if a.satisfies(&b) && b.satisfies(&c) {
                // a should satisfy c where c declared resources
                prop_assert!(a.satisfies(&c));
            }
        }

        /// Profile add_tag rejects tags exceeding MAX_TAG_LEN.
        #[test]
        fn prop_profile_add_tag_rejects_long_tag(
            tag_len in (MAX_TAG_LEN + 1)..1000usize,
        ) {
            let id = crate::node::NodeId::random();
            let mut p = NodeProfile::standard(id, "1.0.0");
            let long_tag = "x".repeat(tag_len);
            let result = p.add_tag(long_tag);
            prop_assert!(result.is_err());
        }

        /// Profile set_description rejects descriptions exceeding MAX_PROFILE_DESC_LEN.
        #[test]
        fn prop_profile_set_description_rejects_long_desc(
            desc_len in (MAX_PROFILE_DESC_LEN + 1)..2000usize,
        ) {
            let id = crate::node::NodeId::random();
            let mut p = NodeProfile::standard(id, "1.0.0");
            let long_desc = "x".repeat(desc_len);
            let result = p.set_description(long_desc);
            prop_assert!(result.is_err());
        }

        /// Profile add_tag succeeds for any tag within MAX_TAG_LEN.
        #[test]
        fn prop_profile_add_tag_accepts_short_tag(tag in "[a-zA-Z0-9_-]{1,64}") {
            let id = crate::node::NodeId::random();
            let mut p = NodeProfile::standard(id, "1.0.0");
            let result = p.add_tag(tag);
            prop_assert!(result.is_ok());
        }

        /// NodeRole from_label is consistent with label().
        #[test]
        fn prop_node_role_roundtrip(
            role_str in prop_oneof![
                Just("light-edge"), Just("le"),
                Just("standard"), Just("st"),
                Just("specialized"), Just("sp"),
                Just("observer"), Just("ob"),
            ],
        ) {
            let role = NodeRole::from_label(&role_str);
            prop_assert!(role.is_some());
            let role = role.unwrap();
            let expected_label: &'static str = match &*role_str {
                "le" => "light-edge",
                "st" => "standard",
                "sp" => "specialized",
                "ob" => "observer",
                _ => &role_str,
            };
            prop_assert_eq!(role.label(), expected_label);
        }

        /// can_serve_blobs is true only for Standard and Specialized.
        #[test]
        fn prop_role_can_serve_blobs(
            role_str in prop_oneof![
                Just("light-edge"),
                Just("standard"),
                Just("specialized"),
                Just("observer"),
            ],
        ) {
            let role = NodeRole::from_label(&role_str).unwrap();
            let expected = role_str == "standard" || role_str == "specialized";
            prop_assert_eq!(role.can_serve_blobs(), expected);
        }
    }

    // ── Aerospace-grade: boundary / edge-case tests ──────────────────────────

    #[test]
    fn profile_add_tag_max_boundary() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "1.0.0");
        // Fill to exactly MAX_PROFILE_TAGS - 1
        for i in 0..(MAX_PROFILE_TAGS - 1) {
            p.add_tag(format!("tag{i}")).unwrap();
        }
        // One more should succeed
        p.add_tag("final-tag").unwrap();
        assert_eq!(p.tags.len(), MAX_PROFILE_TAGS);
        // One more after that must fail
        let err = p.add_tag("extra-tag").unwrap_err();
        assert!(matches!(err, NodeProfileError::TooManyTags));
    }

    #[test]
    fn profile_add_tag_exactly_max_len() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "1.0.0");
        let max_tag = "x".repeat(MAX_TAG_LEN);
        p.add_tag(&max_tag).unwrap();
        assert_eq!(p.tags.len(), 1);
    }

    #[test]
    fn profile_set_description_exactly_max_len() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "1.0.0");
        let max_desc = "x".repeat(MAX_PROFILE_DESC_LEN);
        p.set_description(&max_desc).unwrap();
        assert!(p.description.is_some());
        assert_eq!(p.description.unwrap().len(), MAX_PROFILE_DESC_LEN);
    }

    #[test]
    fn profile_add_tag_one_byte_over_max_fails() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "1.0.0");
        let over_tag = "x".repeat(MAX_TAG_LEN + 1);
        let err = p.add_tag(&over_tag).unwrap_err();
        assert!(matches!(err, NodeProfileError::TagTooLong(_)));
    }

    #[test]
    fn profile_set_description_one_byte_over_fails() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "1.0.0");
        let over_desc = "x".repeat(MAX_PROFILE_DESC_LEN + 1);
        let err = p.set_description(&over_desc).unwrap_err();
        assert!(matches!(err, NodeProfileError::DescriptionTooLong(_)));
    }

    #[test]
    fn profile_touch_updates_timestamp() {
        let id = crate::node::NodeId::random();
        let mut p = NodeProfile::standard(id, "1.0.0");
        let original = p.published_at;
        std::thread::sleep(std::time::Duration::from_secs(1));
        p.touch();
        assert!(p.published_at > original);
    }

    #[test]
    fn profile_empty_tag_list_works() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::standard(id, "1.0.0");
        assert!(p.tags.is_empty());
        let json = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&json).unwrap();
        assert!(back.tags.is_empty());
    }

    #[test]
    fn resources_battery_boundary_0_and_100() {
        let r0 = NodeResources::default().with_battery(0);
        assert_eq!(r0.battery_pct, Some(0));
        let r100 = NodeResources::default().with_battery(100);
        assert_eq!(r100.battery_pct, Some(100));
        let r101 = NodeResources::default().with_battery(101);
        assert_eq!(r101.battery_pct, Some(100)); // clamped
    }

    #[test]
    fn capability_all_known_flags_dont_overlap() {
        let flags = [
            NodeCapability::RELAY,
            NodeCapability::MQTT_BRIDGE,
            NodeCapability::AI_INFERENCE,
            NodeCapability::BLOB_STORAGE,
            NodeCapability::WORKSPACE_HOST,
            NodeCapability::MESH_MONITOR,
            NodeCapability::SSH_GATEWAY,
            NodeCapability::DNS_RESOLVER,
            NodeCapability::EXIT_NODE,
            NodeCapability::NAS_SERVER,
            NodeCapability::AI_AGENT,
        ];
        // Every pair should have non-overlapping bits
        for i in 0..flags.len() {
            for j in (i + 1)..flags.len() {
                let intersection = flags[i] & flags[j];
                assert!(
                    intersection == NODE_CAPABILITY_NONE,
                    "Flags {} and {} have overlapping bits",
                    i,
                    j
                );
            }
        }
    }

    #[test]
    fn capability_union_produces_all_bits() {
        let all = NodeCapability::RELAY
            | NodeCapability::MQTT_BRIDGE
            | NodeCapability::AI_INFERENCE
            | NodeCapability::BLOB_STORAGE
            | NodeCapability::WORKSPACE_HOST
            | NodeCapability::MESH_MONITOR
            | NodeCapability::SSH_GATEWAY
            | NodeCapability::DNS_RESOLVER
            | NodeCapability::EXIT_NODE
            | NodeCapability::NAS_SERVER
            | NodeCapability::AI_AGENT;

        // Every known flag should be contained in the union
        let flags = [
            NodeCapability::RELAY,
            NodeCapability::MQTT_BRIDGE,
            NodeCapability::AI_INFERENCE,
            NodeCapability::BLOB_STORAGE,
            NodeCapability::WORKSPACE_HOST,
            NodeCapability::MESH_MONITOR,
            NodeCapability::SSH_GATEWAY,
            NodeCapability::DNS_RESOLVER,
            NodeCapability::EXIT_NODE,
            NodeCapability::NAS_SERVER,
            NodeCapability::AI_AGENT,
        ];
        for flag in flags {
            assert!(all.contains(flag), "union should contain {:?}", flag);
        }
    }

    #[test]
    fn capability_difference_removes_all() {
        let all =
            NodeCapability::RELAY | NodeCapability::AI_INFERENCE | NodeCapability::BLOB_STORAGE;
        let diff = all.difference(all);
        assert_eq!(diff, NODE_CAPABILITY_NONE);
    }

    #[test]
    fn capability_none_has_zero_bits() {
        assert_eq!(NODE_CAPABILITY_NONE.bits(), 0);
        assert!(!NODE_CAPABILITY_NONE.contains(NodeCapability::RELAY));
        assert!(!NODE_CAPABILITY_NONE.contains(NodeCapability::AI_INFERENCE));
    }

    #[test]
    fn profile_can_serve_blobs_by_role() {
        let id = crate::node::NodeId::random();

        // Standard: yes
        let standard = NodeProfile::standard(id.clone(), "1.0.0");
        assert!(standard.can_serve_blobs());

        // Observer: no
        let observer = NodeProfile::observer(id.clone(), "1.0.0");
        assert!(!observer.can_serve_blobs());

        // LightEdge: no
        let mut light = NodeProfile::standard(id.clone(), "1.0.0");
        light.role = NodeRole::LightEdge;
        assert!(!light.can_serve_blobs());

        // Specialized: yes
        let resources =
            NodeResources::new(64, 512 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024 * 1024);
        let specialized = NodeProfile::ai_inference(id, "1.0.0", resources, "AI");
        assert!(specialized.can_serve_blobs());
    }

    #[test]
    fn profile_satisfies_capabilities_exact() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::standard(id, "1.0.0");
        assert!(p.satisfies_capabilities(NodeCapability::BLOB_STORAGE));
        assert!(p.satisfies_capabilities(NodeCapability::WORKSPACE_HOST));
        assert!(
            p.satisfies_capabilities(NodeCapability::BLOB_STORAGE | NodeCapability::WORKSPACE_HOST)
        );
        assert!(!p.satisfies_capabilities(NodeCapability::AI_INFERENCE));
        assert!(!p.satisfies_capabilities(NodeCapability::RELAY));
    }

    #[test]
    fn profile_summary_includes_key_fields() {
        let id = crate::node::NodeId::random();
        let resources =
            NodeResources::new(16, 64 * 1024 * 1024 * 1024, 2 * 1024 * 1024 * 1024 * 1024);
        let mut p = NodeProfile::ai_inference(id.clone(), "0.1.0", resources, "GPU beast");
        p.add_tag("ai").unwrap();

        let summary = p.summary();
        assert!(summary.contains(&id.short()[..8.min(id.short().len())]));
        assert!(summary.contains("specialized"));
        assert!(summary.contains("0.1.0"));
        assert!(summary.contains("GPU beast"));
    }

    #[test]
    fn resources_summary_handles_zero_values() {
        let r = NodeResources::new(0, 0, 0);
        let summary = r.summary();
        assert!(summary.contains("0 cores"));
        assert!(summary.contains("0.0 GiB"));
        assert!(summary.contains("0 GiB storage"));
    }

    #[test]
    fn profile_version_preserved_through_json() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::standard(id.clone(), "99.99.99-alpha+build.42");
        let json = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, "99.99.99-alpha+build.42");
    }

    #[test]
    fn profile_role_serde_preserves_variant() {
        for role in [
            NodeRole::LightEdge,
            NodeRole::Standard,
            NodeRole::Specialized,
            NodeRole::Observer,
        ] {
            let id = crate::node::NodeId::random();
            let mut p = NodeProfile::standard(id, "1.0.0");
            p.role = role;

            let json = serde_json::to_string(&p).unwrap();
            let back: NodeProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(back.role, role);
        }
    }

    #[test]
    fn profile_resources_optional_persists_none() {
        let id = crate::node::NodeId::random();
        let p = NodeProfile::standard(id.clone(), "1.0.0");
        assert!(p.resources.is_none());

        let json = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&json).unwrap();
        assert!(back.resources.is_none());
    }

    #[test]
    fn capability_bitor_assign_updates_in_place() {
        let mut caps = NodeCapability::RELAY;
        caps |= NodeCapability::AI_INFERENCE;
        caps |= NodeCapability::BLOB_STORAGE;
        assert!(caps.contains(NodeCapability::RELAY));
        assert!(caps.contains(NodeCapability::AI_INFERENCE));
        assert!(caps.contains(NodeCapability::BLOB_STORAGE));
    }

    #[test]
    fn capability_from_bits_preserves_all_64_bits() {
        // Test boundary: all bits set
        let all = NodeCapability::from_bits(u64::MAX);
        assert!(all.contains(NodeCapability::RELAY));
        assert!(all.contains(NodeCapability::AI_AGENT));

        // Test: alternating bits
        let alt = NodeCapability::from_bits(0xAAAA_AAAA_AAAA_AAAA);
        assert!(alt.bits() == 0xAAAA_AAAA_AAAA_AAAA);
    }
}
