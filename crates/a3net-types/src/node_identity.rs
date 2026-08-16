//! Node identity card — every A3Net node carries exactly one.
//!
//! An [`NodeIdentity`] is the user-facing, *non-cryptographic* self-description
//! attached to a node. It co-exists with the existing cryptographic
//! [`crate::node::NodeId`] (32-byte ed25519 public key) and the
//! [`crate::wallet_address::WalletAddress`] (20-byte EVM-style address):
//!
//! | Concept               | Format            | Assigned by        | Purpose                       |
//! |-----------------------|-------------------|--------------------|-------------------------------|
//! | `NodeId` (digital id) | 64-hex (32 bytes) | keypair gen        | routing, signing, gossip key  |
//! | `dns_node_id`         | 12 digits         | DNS server         | human-friendly node number    |
//! | `wallet_address`      | `0x` + 40 hex     | wallet / chain     | on-chain payments             |
//! | `email`               | RFC 5322          | user               | contact / verification        |
//!
//! ## Invariants (enforced at construction)
//!
//! - `email` — non-empty, RFC-lite (single `@`, non-empty local + domain).
//! - `dns_node_id` — exactly 12 ASCII digits in `[0-9]`; semantically
//!   a `u64` so it fits comfortably inside a `u128` (12 decimal digits
//!   ≤ 10^12 − 1 < 2^40).
//! - `nickname` — 1..=64 bytes UTF-8, no control bytes (display name).
//! - `description` — 0..=`MAX_NODE_DESCRIPTION_LEN` (128) bytes UTF-8,
//!   no NULs.
//! - `avatar` — either an `https://` URL (max 512 bytes) or an inline
//!   data URI (`data:image/...;base64,...` up to 256 KiB). Anything
//!   else is rejected.
//! - `digital_identity` — must equal this node's [`crate::node::NodeId`]
//!   hex string; we keep it as a separate field for forward
//!   compatibility (later versions may use a wallet / pubkey variant).
//! - `wallet_address` — 20-byte EVM-style address (validated by
//!   [`crate::wallet_address::WalletAddress`]).
//!
//! ## Wire format
//!
//! Serialised as JSON for `node_identity.json` on disk and as the
//! payload of the gossip `NodeIdentityAnnouncement` frame.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};
use crate::node::NodeId;
use crate::wallet_address::WalletAddress;

/// Length of the DNS-assigned numeric node id, in decimal digits.
///
/// 12 digits gives a 10^12-id namespace (≈ 1 trillion) — large enough
/// for any plausible A3Net deployment, small enough to fit in a u64.
pub const DNS_NODE_ID_DIGITS: usize = 12;

/// Maximum numeric value of the DNS-assigned id (10^12 − 1).
pub const DNS_NODE_ID_MAX: u64 = 999_999_999_999;

/// Maximum length of the `description` field, in bytes (UTF-8).
pub const MAX_NODE_DESCRIPTION_LEN: usize = 128;

/// Maximum length of the `nickname` field, in bytes (UTF-8).
pub const MAX_NICKNAME_LEN: usize = 64;

/// Maximum length of an `https://` avatar URL, in bytes.
pub const MAX_AVATAR_URL_LEN: usize = 512;

/// Maximum length of a `data:` avatar URI, in bytes (base64 payload included).
pub const MAX_AVATAR_DATA_LEN: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// NodeFunction — feature / capability-role taxonomy
// ---------------------------------------------------------------------------

/// Maximum length of a [`NodeFunction::Custom`] tag, in bytes (UTF-8).
///
/// Custom functions live in the gossip card's extension slot; we cap the
/// size to keep the on-wire cost bounded and to discourage verbose tag
/// names like `"experimental-audio-streaming-relay-v2-rc1-with-debug"`.
pub const MAX_NODE_FUNCTION_CUSTOM_LEN: usize = 32;

/// Maximum number of functions a single node may declare.
///
/// A node is allowed to wear multiple hats (e.g. `AiAgent` + `MailRelay`
/// + `WorkspaceHost`), but we cap the list to keep the identity card
/// digest stable and the gossip frame budget predictable.
pub const MAX_NODE_FUNCTIONS: usize = 32;

/// Feature / role taxonomy for A3Net nodes.
///
/// A [`NodeFunction`] describes **what a node does on the network** — as
/// opposed to [`NodeKind`] (who operates it) or [`crate::node_profile::NodeRole`]
/// (what hardware it is). One node can advertise any number of
/// functions simultaneously; the set is non-exhaustive and
/// forward-compatible via the [`NodeFunction::Custom`] extension slot.
///
/// ## How it relates to `NodeKind` and `NodeCapability`
///
/// | Field           | Axis       | Cardinality | Status    |
/// |-----------------|------------|-------------|-----------|
/// | [`NodeRole`]    | device class   | 4 mutually exclusive | **stable** |
/// | [`NodeKind`]    | operator class | 4 mutually exclusive | **stable** |
/// | [`NodeFunction`]| feature / role | up to 32 per node    | **extensible** |
/// | [`NodeCapability`] | low-level service bits | u64 bitfield  | **stable** |
///
/// - `NodeRole` says "this is a `Specialized` box".
/// - `NodeKind` says "this is `AiCompute`" (the operator).
/// - `NodeFunction` says "this box exposes a `ModelCatalog` and a
///   `BlobStore` and additionally serves as a `MailRelay`".
/// - `NodeCapability` says "the lower-level byte-level services this
///   profile advertises" (a 64-bit bitfield; very stable).
///
/// `NodeFunction` is the user-facing, narratable version of what
/// `NodeCapability` encodes in binary. New functions are added by
/// extending this enum — old clients ignore unknown variants (they
/// see a [`NodeFunction::Custom`] tag and log it). New capabilities
/// require a protocol-version bump.
///
/// ## Wire format
///
/// Serialised as JSON, either as a `snake_case` string for known
/// variants or as `{"custom": "tag"}` for [`Custom`]. Defaults to an
/// empty list when the field is missing on the wire (backward-
/// compatible with v1 identity cards).
///
/// `[derive(Default)]` is intentionally **not** provided at the enum
/// level — the containing collection (`Vec<NodeFunction>`) is `default`
/// to `Vec::new()` so a missing field is simply "no declared functions".
///
/// [`Custom`]: NodeFunction::Custom
/// [`NodeRole`]: crate::node_profile::NodeRole
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeFunction {
    // ── AI ─────────────────────────────────────────────────────────────
    /// Hosts an AI agent loop (consumer of compute, producer of replies).
    /// Mirrors [`NodeKind::AiAgent`] but at the **function** level — a
    /// node may advertise `AiAgent` here even when its [`NodeKind`]
    /// is `Human` (a workstation running a local agent daemon).
    AiAgent,

    /// Provides AI inference compute (Ollama / vLLM / llama.cpp /
    /// lm-studio / OpenAI-compatible). Mirrors [`NodeKind::AiCompute`]
    /// but again at the function level — *any* node can run an
    /// inference daemon and advertise this function; the `NodeKind`
    /// just gives the canonical role.
    AiCompute,

    /// Maintains a live catalog of models known to the mesh, plus
    /// routing rules ("send embeddings to peer X, completions to Y").
    /// First-class so consumers (`RemoteAgentModel::discover`) can
    /// find a single stable endpoint instead of fan-out pinging every
    /// `AiCompute` peer.
    ModelCatalog,

    /// Embedding / vector index endpoint (text → vector, vector ↔ text).
    /// Distinct from `AiCompute` because not every model server
    /// exposes embeddings on the same socket.
    Embedding,

    // ── 存储 / 数据 ───────────────────────────────────────────────────
    /// Persists BLOB content and serves Bitswap / IPFS-style lookups.
    BlobStore,

    /// Hosts a collaborative workspace (real-time CRDT doc store).
    WorkspaceHost,

    /// Incremental / encrypted backup vault (target or source).
    BackupVault,

    /// User-facing NAS — exposes a flat file API to LAN/mesh peers.
    Nas,

    // ── 通信 / 转发 ────────────────────────────────────────────────────
    /// QUIC / TURN relay — forwards opaque traffic between peers that
    /// can't hole-punch each other.
    Relay,

    /// SMTP / IMAP bridge — relays mail between A3Net mail tool
    /// peers and an external mail server.
    MailRelay,

    /// MQTT broker / bridge — connects the gossip layer with an
    /// external pub/sub topic tree.
    MqttBridge,

    /// Pubsub broker — internally routable pub/sub over the gossip
    /// layer (no external MQTT dependency).
    PubsubBroker,

    /// Chat room / DM relay — long-lived chat fan-out node.
    ChatRelay,

    // ── 基础设施 ──────────────────────────────────────────────────────
    /// MagicDNS / pkarr resolver — answers `*.a3net` queries.
    DnsResolver,

    /// Publishes pkarr records to Mainline DHT on behalf of one or
    /// more nodes (e.g. for nodes behind a NAT that can't publish
    /// directly).
    PkarrPublisher,

    /// Mesh observability — health probes, latency atlas, loss reports.
    /// Distinct from `NetworkManager` (which is the *operator class*);
    /// any kind of node can opt into a monitoring role.
    MeshMonitor,

    /// Audit aggregator — collects signed event digests from peers
    /// and produces a per-room / per-node audit log.
    AuditAggregator,

    // ── 安全 / 权限 ────────────────────────────────────────────────────
    /// SSH gateway — tunnels SSH sessions over the mesh for outbound
    /// access to nodes without public IPs.
    SshGateway,

    /// Tor / exit-node / SOCKS proxy out of the mesh.
    ExitNode,

    // ── 扩展槽 ─────────────────────────────────────────────────────────
    /// Third-party / not-yet-standardised function. The string is a
    /// short, reverse-DNS-style tag such as `"acme.weather-feed"` or
    /// `"mycompany.stream-gateway"`. Callers MUST:
    ///
    /// 1. Keep the tag ≤ [`MAX_NODE_FUNCTION_CUSTOM_LEN`] bytes.
    /// 2. Use only the chars `[a-z0-9.\-_]` (snake-case / DNS labels).
    /// 3. Prefix with a stable namespace (`org.example.<feature>`).
    ///
    /// Validation happens in [`NodeFunction::Custom::validated`].
    /// Untrusted input should always go through that constructor.
    Custom(String),
}

impl NodeFunction {
    /// Snake-case label suitable for JSON serialisation **and** for
    /// CLI tables. Returns the same string as the underlying variant
    /// name (e.g. `"ai_agent"`, `"blob_store"`).
    pub fn label(&self) -> String {
        match self {
            NodeFunction::AiAgent => "ai_agent".to_string(),
            NodeFunction::AiCompute => "ai_compute".to_string(),
            NodeFunction::ModelCatalog => "model_catalog".to_string(),
            NodeFunction::Embedding => "embedding".to_string(),
            NodeFunction::BlobStore => "blob_store".to_string(),
            NodeFunction::WorkspaceHost => "workspace_host".to_string(),
            NodeFunction::BackupVault => "backup_vault".to_string(),
            NodeFunction::Nas => "nas".to_string(),
            NodeFunction::Relay => "relay".to_string(),
            NodeFunction::MailRelay => "mail_relay".to_string(),
            NodeFunction::MqttBridge => "mqtt_bridge".to_string(),
            NodeFunction::PubsubBroker => "pubsub_broker".to_string(),
            NodeFunction::ChatRelay => "chat_relay".to_string(),
            NodeFunction::DnsResolver => "dns_resolver".to_string(),
            NodeFunction::PkarrPublisher => "pkarr_publisher".to_string(),
            NodeFunction::MeshMonitor => "mesh_monitor".to_string(),
            NodeFunction::AuditAggregator => "audit_aggregator".to_string(),
            NodeFunction::SshGateway => "ssh_gateway".to_string(),
            NodeFunction::ExitNode => "exit_node".to_string(),
            NodeFunction::Custom(tag) => tag.clone(),
        }
    }

    /// Short pkarr `UserData` tag (≤ 4 bytes / UTF-8) for the **known**
    /// variants. `Custom(_)` returns `None` because third-party tags
    /// have no fixed short code.
    pub fn pkarr_tag(&self) -> Option<&'static str> {
        match self {
            NodeFunction::AiAgent => Some("agn"),
            NodeFunction::AiCompute => Some("cmp"),
            NodeFunction::ModelCatalog => Some("mcl"),
            NodeFunction::Embedding => Some("emb"),
            NodeFunction::BlobStore => Some("blb"),
            NodeFunction::WorkspaceHost => Some("wsp"),
            NodeFunction::BackupVault => Some("bkv"),
            NodeFunction::Nas => Some("nas"),
            NodeFunction::Relay => Some("rly"),
            NodeFunction::MailRelay => Some("mlr"),
            NodeFunction::MqttBridge => Some("mqt"),
            NodeFunction::PubsubBroker => Some("psb"),
            NodeFunction::ChatRelay => Some("chr"),
            NodeFunction::DnsResolver => Some("dns"),
            NodeFunction::PkarrPublisher => Some("pkr"),
            NodeFunction::MeshMonitor => Some("mmo"),
            NodeFunction::AuditAggregator => Some("aud"),
            NodeFunction::SshGateway => Some("ssh"),
            NodeFunction::ExitNode => Some("ext"),
            NodeFunction::Custom(_) => None,
        }
    }

    /// Parse a function from its label. Unknown / `Custom` tags round-
    /// trip back as [`NodeFunction::Custom`].
    pub fn from_label(s: &str) -> Option<Self> {
        Some(match s {
            "ai_agent" | "agent" => NodeFunction::AiAgent,
            "ai_compute" | "compute" | "ollama" | "inference" => NodeFunction::AiCompute,
            "model_catalog" | "catalog" => NodeFunction::ModelCatalog,
            "embedding" | "embeddings" | "vector" => NodeFunction::Embedding,
            "blob_store" | "blob" | "storage" => NodeFunction::BlobStore,
            "workspace_host" | "workspace" => NodeFunction::WorkspaceHost,
            "backup_vault" | "backup" => NodeFunction::BackupVault,
            "nas" | "files" => NodeFunction::Nas,
            "relay" => NodeFunction::Relay,
            "mail_relay" | "mail" | "smtp" => NodeFunction::MailRelay,
            "mqtt_bridge" | "mqtt" => NodeFunction::MqttBridge,
            "pubsub_broker" | "pubsub" => NodeFunction::PubsubBroker,
            "chat_relay" | "chat" => NodeFunction::ChatRelay,
            "dns_resolver" | "dns" => NodeFunction::DnsResolver,
            "pkarr_publisher" | "pkarr" => NodeFunction::PkarrPublisher,
            "mesh_monitor" | "monitor" => NodeFunction::MeshMonitor,
            "audit_aggregator" | "audit" => NodeFunction::AuditAggregator,
            "ssh_gateway" | "ssh" => NodeFunction::SshGateway,
            "exit_node" | "exit" | "socks" => NodeFunction::ExitNode,
            other => NodeFunction::Custom(other.to_string()),
        })
    }

    /// `true` for variants that are part of the A3Net standard set
    /// (i.e. non-custom). Custom tags return `false`.
    pub fn is_standard(&self) -> bool {
        !matches!(self, NodeFunction::Custom(_))
    }

    /// `true` for variants that are mutually exclusive with at least
    /// one other variant in the standard set. Currently every standard
    /// function can co-exist with every other, so this always returns
    /// `false` — but the helper exists so callers can `assert!(!f1.is_exclusive_with(&f2))`
    /// in policy code without future-proofing every callsite by hand.
    pub fn is_exclusive_with(&self, _other: &NodeFunction) -> bool {
        false
    }

    /// Coarse functional category for grouping in dashboards / CLI tables.
    pub fn category(&self) -> &'static str {
        match self {
            NodeFunction::AiAgent
            | NodeFunction::AiCompute
            | NodeFunction::ModelCatalog
            | NodeFunction::Embedding => "ai",
            NodeFunction::BlobStore
            | NodeFunction::WorkspaceHost
            | NodeFunction::BackupVault
            | NodeFunction::Nas => "storage",
            NodeFunction::Relay
            | NodeFunction::MailRelay
            | NodeFunction::MqttBridge
            | NodeFunction::PubsubBroker
            | NodeFunction::ChatRelay => "communication",
            NodeFunction::DnsResolver
            | NodeFunction::PkarrPublisher
            | NodeFunction::MeshMonitor
            | NodeFunction::AuditAggregator => "infrastructure",
            NodeFunction::SshGateway | NodeFunction::ExitNode => "security",
            NodeFunction::Custom(_) => "custom",
        }
    }
}

impl NodeFunction {
    /// Validate and construct a [`NodeFunction::Custom`]. Returns an
    /// [`AdnetError::Validation`] if the tag violates any rule.
    pub fn custom_validated(tag: impl Into<String>) -> Result<Self> {
        let tag = tag.into();
        if tag.is_empty() {
            return Err(AdnetError::Validation(
                "NodeFunction::Custom tag must be non-empty".into(),
            ));
        }
        if tag.len() > MAX_NODE_FUNCTION_CUSTOM_LEN {
            return Err(AdnetError::Validation(format!(
                "NodeFunction::Custom tag exceeds {MAX_NODE_FUNCTION_CUSTOM_LEN} bytes (got {})",
                tag.len()
            )));
        }
        if !tag.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        }) {
            return Err(AdnetError::Validation(format!(
                "NodeFunction::Custom tag must be lowercase [a-z0-9.\\-_] only (got {tag:?})"
            )));
        }
        if tag.starts_with('.') || tag.starts_with('-') {
            return Err(AdnetError::Validation(format!(
                "NodeFunction::Custom tag must not start with '.' or '-' (got {tag:?})"
            )));
        }
        Ok(NodeFunction::Custom(tag))
    }
}

// ---------------------------------------------------------------------------
// NodeKind — operator-class taxonomy
// ---------------------------------------------------------------------------

/// Operator-class taxonomy for A3Net nodes.
///
/// Every node on the network belongs to one of four mutually exclusive
/// categories:
///
/// - `AiAgent`        — the node is *driven by* an AI agent (Hermes-Rust,
///                      Eliza, an OpenAI-compatible endpoint, etc.). The
///                      agent can represent the node in conversations,
///                      sign announcements on its behalf, and reply to
///                      messages routed through [`crate::group_chat::DirectChat`].
/// - `AiCompute`      — the node *provides* AI inference compute to the
///                      mesh (Ollama, vLLM, llama.cpp, lm-studio, a remote
///                      OpenAI-compatible endpoint, etc.). It exposes a
///                      `/v1/chat/completions`-style HTTP API and serves
///                      other nodes' `ChatRequest`s. It does **not** itself
///                      host an agent loop — it is a **token factory**.
/// - `Human`          — the node is *operated by* a person who signs
///                      gossip and chat messages themselves. This is the
///                      default for any node without an explicit agent
///                      registration.
/// - `NetworkManager` — the node belongs to the network's operations
///                      tier (DNS server, pkarr publisher, relay, mesh
///                      monitor, audit aggregator). These nodes
///                      privilege routing / observability over content
///                      authoring.
///
/// ## Why a separate taxonomy from `NodeRole`?
///
/// `NodeRole` (in `node_profile.rs`) is a **device-class** taxonomy
/// ("is this a phone, a workstation, a storage backend?"). `NodeKind`
/// is **operator-class** ("who runs the show?"). The two are
/// orthogonal: a `Standard` `Human` workstation is the common case;
/// a `Specialized` `NetworkManager` is a relay; a `LightEdge`
/// `AiAgent` is a sensor with an on-device model; a `Specialized`
/// `AiCompute` is a GPU inference box serving Ollama.
///
/// ## `AiAgent` vs `AiCompute` — the consumer / producer split
///
/// Both touch AI, but they sit on opposite sides of the request flow:
///
/// | `NodeKind` | Direction | Loop | Advertises  | Auth          |
/// |------------|-----------|------|-------------|---------------|
/// | `AiAgent`  | consumer  | yes  | `AI_AGENT`  | user prompts  |
/// | `AiCompute`| producer  | no   | `AI_INFERENCE` (+ `AI_AGENT` if co-resident) | per-token billing |
///
/// An `AiCompute` node answers `/v1/chat/completions` requests from
/// anywhere on the mesh (subject to the per-peer `AgentAclMode` and
/// audit log). An `AiAgent` node *makes* those requests. A single
/// workstation can carry both kinds — the kind is a per-node
/// declaration, not a per-process tag.
///
/// ## Wire format
///
/// Serialised as a `snake_case` string alongside `NodeIdentity` and
/// bundled through the gossip `NodeIdentityCard`. Defaults to `Human`
/// on the wire when missing (backward-compatible with v1 identity
/// files that pre-date this field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// AI agent operates this node (consumer of AI compute).
    AiAgent,
    /// AI inference provider — exposes an OpenAI-compatible endpoint
    /// and serves chat-completion requests for the mesh (producer of
    /// AI tokens).
    AiCompute,
    /// A human operator runs this node. **Default** for all legacy
    /// identities that don't carry a `kind` field.
    #[default]
    Human,
    /// Operations / observability tier (DNS, relay, mesh monitor).
    NetworkManager,
}

impl NodeKind {
    /// Human-readable label, suitable for CLI tables and admin UIs.
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::AiAgent => "ai_agent",
            NodeKind::AiCompute => "ai_compute",
            NodeKind::Human => "human",
            NodeKind::NetworkManager => "network_manager",
        }
    }

    /// Short pkarr `UserData` tag. Kept ≤ 4 bytes to fit inside the
    /// 245-byte `UserData` budget.
    pub fn pkarr_tag(self) -> &'static str {
        match self {
            NodeKind::AiAgent => "aa",
            NodeKind::AiCompute => "ac",
            NodeKind::Human => "hu",
            NodeKind::NetworkManager => "nm",
        }
    }

    /// Parse a kind from its label or short tag. Returns `None` for
    /// unknown inputs.
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "ai_agent" | "aa" | "agent" => Some(NodeKind::AiAgent),
            "ai_compute" | "ac" | "compute" | "ollama" | "inference" => {
                Some(NodeKind::AiCompute)
            }
            "human" | "hu" | "person" => Some(NodeKind::Human),
            "network_manager" | "nm" | "manager" | "ops" => Some(NodeKind::NetworkManager),
            _ => None,
        }
    }

    /// `true` for kinds that can author signed chat / DM messages on
    /// their own behalf.
    pub fn can_sign_messages(self) -> bool {
        // All four kinds can sign; the difference is in *who* is
        // assumed to hold the key (a person vs an agent process vs
        // an inference server vs an ops daemon).
        true
    }

    /// `true` for kinds whose operator is expected to be online in
    /// near-real-time and respond to DMs / room messages. `Human`,
    /// `AiAgent`, and `AiCompute` all qualify (the last one because
    /// it can echo a synthetic reply when the upstream model
    /// responds). `NetworkManager` typically does not.
    pub fn is_realtime(self) -> bool {
        matches!(
            self,
            NodeKind::Human | NodeKind::AiAgent | NodeKind::AiCompute
        )
    }

    /// `true` for kinds that can **answer** an inbound
    /// `chat_completions`-style request from another node. Only
    /// `AiCompute` is a producer end-point.
    pub fn serves_inference(self) -> bool {
        matches!(self, NodeKind::AiCompute)
    }

    /// `true` for kinds that can **make** an outbound chat request
    /// to a remote `AiCompute` node. `AiAgent` is the canonical
    /// consumer; `Human` workstations (via CLI) and `AiCompute`
    /// (for model fallback / A-B testing) also qualify.
    pub fn consumes_inference(self) -> bool {
        matches!(
            self,
            NodeKind::AiAgent | NodeKind::Human | NodeKind::AiCompute
        )
    }

    /// Inverse of [`NodeKind::serves_inference`]: asks whether this
    /// node should be **visible** in the network's "inference
    /// providers" listing (and therefore routable by `agent.v1` /
    /// `model.catalog`).
    pub fn is_inference_provider(self) -> bool {
        self.serves_inference()
    }
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The DNS-assigned 12-digit numeric node id (e.g. `483726150931`).
///
/// `Display` and `serde` render it without separators; the inner `u64`
/// is the canonical value used by the allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DnsNodeId(u64);

impl DnsNodeId {
    /// Construct from the raw numeric value. Returns `Validation` if
    /// the value exceeds [`DNS_NODE_ID_MAX`].
    pub fn from_u64(v: u64) -> Result<Self> {
        if v > DNS_NODE_ID_MAX {
            return Err(AdnetError::Validation(format!(
                "dns_node_id: {v} exceeds max {DNS_NODE_ID_MAX}"
            )));
        }
        Ok(Self(v))
    }

    /// Parse the canonical 12-digit decimal string. Returns
    /// `Validation` if the input is the wrong length, contains
    /// non-digit bytes, or overflows.
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != DNS_NODE_ID_DIGITS {
            return Err(AdnetError::Validation(format!(
                "dns_node_id: expected {DNS_NODE_ID_DIGITS} digits, got {}",
                s.len()
            )));
        }
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AdnetError::Validation(format!(
                "dns_node_id: non-digit character in {s:?}"
            )));
        }
        let v: u64 = s.parse().map_err(|e| {
            AdnetError::Validation(format!("dns_node_id: parse error {e}"))
        })?;
        Self::from_u64(v)
    }

    /// Raw numeric value.
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Render as the canonical 12-digit decimal string with leading zeros.
    pub fn to_digits(self) -> String {
        format!("{:0>12}", self.0)
    }
}

impl std::fmt::Display for DnsNodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_digits())
    }
}

impl From<DnsNodeId> for String {
    fn from(v: DnsNodeId) -> String {
        v.to_digits()
    }
}

impl TryFrom<String> for DnsNodeId {
    type Error = AdnetError;
    fn try_from(s: String) -> Result<Self> {
        Self::parse(&s)
    }
}

impl TryFrom<&str> for DnsNodeId {
    type Error = AdnetError;
    fn try_from(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// Avatar representation. Either an HTTPS URL or an inline data URI.
///
/// Inline avatars are useful for nodes that want a stable identity
/// picture without depending on an external CDN (the URL form keeps
/// the on-disk payload tiny).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Avatar {
    /// HTTPS URL pointing at an image (jpg/png/webp/gif).
    Url {
        /// The URL, must start with `https://`.
        url: String,
    },
    /// Inline data URI: `data:image/<type>;base64,<payload>`.
    Data {
        /// MIME sub-type (e.g. `png`, `jpeg`).
        media_type: String,
        /// Base64-encoded payload.
        payload_b64: String,
    },
}

/// Default tags implied by [`NodeKind`] when no explicit
/// [`NodeFunction`] list is provided. Used as a back-compat fallback
/// for identity cards written before the `functions` field existed.
///
/// The mapping mirrors — but is intentionally independent of —
/// [`NodeKind`]. We don't want a hidden import cycle, and we want
/// each side of the schema to evolve separately.
pub fn default_functions_for_kind(kind: NodeKind) -> Vec<NodeFunction> {
    match kind {
        NodeKind::AiAgent => vec![NodeFunction::AiAgent],
        NodeKind::AiCompute => vec![NodeFunction::AiCompute],
        NodeKind::NetworkManager => vec![NodeFunction::Relay],
        // `Human` declares no function by default — humans wear many
        // hats and we'd rather have them opt-in than pin a wrong
        // guess (was `WORKSPACE_HOST` historically; intentionally
        // removed because even a fresh human node doesn't auto-host).
        NodeKind::Human => Vec::new(),
    }
}

impl Avatar {
    /// Construct an HTTPS URL avatar. Validates the URL prefix and length.
    pub fn from_url(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        if !url.starts_with("https://") {
            return Err(AdnetError::Validation(format!(
                "avatar: must start with https:// (got {url:?})"
            )));
        }
        if url.len() > MAX_AVATAR_URL_LEN {
            return Err(AdnetError::Validation(format!(
                "avatar: url exceeds {MAX_AVATAR_URL_LEN} bytes (got {})",
                url.len()
            )));
        }
        if url.as_bytes().contains(&0) {
            return Err(AdnetError::Validation(
                "avatar: url contains NUL".into(),
            ));
        }
        Ok(Avatar::Url { url })
    }

    /// Construct an inline data URI avatar. `media_type` should be a
    /// short sub-type like `png` or `jpeg`; the caller is responsible
    /// for base64-encoding `payload_b64`.
    pub fn from_data_uri(
        media_type: impl Into<String>,
        payload_b64: impl Into<String>,
    ) -> Result<Self> {
        let media_type = media_type.into();
        let payload_b64 = payload_b64.into();
        if media_type.is_empty()
            || media_type.len() > 32
            || !media_type
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(AdnetError::Validation(format!(
                "avatar: invalid media_type {media_type:?}"
            )));
        }
        // Build the full data URI to validate length.
        let full = format!("data:image/{media_type};base64,{payload_b64}");
        if full.len() > MAX_AVATAR_DATA_LEN {
            return Err(AdnetError::Validation(format!(
                "avatar: data URI exceeds {MAX_AVATAR_DATA_LEN} bytes (got {})",
                full.len()
            )));
        }
        if payload_b64.as_bytes().contains(&0) {
            return Err(AdnetError::Validation(
                "avatar: payload contains NUL".into(),
            ));
        }
        Ok(Avatar::Data {
            media_type,
            payload_b64,
        })
    }

    /// Approximate serialised size in bytes (used by callers that want
    /// to budget gossip frames without re-serialising).
    pub fn approx_size(&self) -> usize {
        match self {
            Avatar::Url { url } => url.len(),
            Avatar::Data {
                media_type,
                payload_b64,
            } => media_type.len() + payload_b64.len() + 32,
        }
    }
}

/// The full self-description every A3Net node carries.
///
/// Constructed via [`NodeIdentity::new`] which enforces every
/// invariant. After construction, fields are publicly readable
/// (`pub`) so the struct is wire-friendly, but **always mutate
/// through the typed setters** ([`set_email`], [`set_nickname`], …)
/// so invariants cannot be violated by callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeIdentity {
    /// Cryptographic node id (ed25519 public key). This is the
    /// authoritative routing key — the *digital identity*.
    pub digital_identity: NodeId,

    /// DNS-assigned 12-digit numeric id. Allocated by the
    /// `exodus-dns-server` and stable for the lifetime of the node.
    pub dns_node_id: DnsNodeId,

    /// User-chosen display name (1..=64 bytes UTF-8).
    pub nickname: String,

    /// RFC-lite email address (single `@`, non-empty parts, ≤ 254 bytes).
    pub email: String,

    /// Inline data URI or HTTPS URL — see [`Avatar`].
    pub avatar: Avatar,

    /// Free-form one-line self-description, 0..=128 bytes UTF-8.
    pub description: String,

    /// EVM-style 20-byte wallet address for on-chain payments.
    pub wallet_address: WalletAddress,

    /// Unix-seconds when this identity was first provisioned.
    pub created_at: u64,
    /// Unix-seconds when this identity was last mutated.
    pub updated_at: u64,

    /// Operator-class taxonomy — see [`NodeKind`]. Defaults to
    /// [`NodeKind::Human`] when deserialised from older identity files
    /// that pre-date this field (the `#[serde(default)]` keeps the
    /// gossip card backward-compatible).
    #[serde(default)]
    pub kind: NodeKind,

    /// Feature / role taxonomy — see [`NodeFunction`]. A node may
    /// declare **zero, one, or many** functions; the field is
    /// independent of `kind` (an operator-class taxonomy) and of
    /// `NodeRole` (a device-class taxonomy). Defaults to an empty
    /// list, which is forward-compatible with v1 identity cards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<NodeFunction>,
}

/// Errors returned by [`NodeIdentity`] mutators.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NodeIdentityError {
    #[error("email: {0}")]
    InvalidEmail(String),

    #[error("nickname: {0}")]
    InvalidNickname(String),

    #[error("description: {0}")]
    InvalidDescription(String),

    #[error("avatar: {0}")]
    InvalidAvatar(String),

    #[error("digital_identity must equal the node's own NodeId (got {0:?})")]
    DigitalIdentityMismatch(String),

    #[error("dns_node_id: {0}")]
    InvalidDnsNodeId(String),

    #[error("function: {0}")]
    InvalidFunction(String),

    #[error("too many functions: limit is {MAX_NODE_FUNCTIONS}, got {0}")]
    TooManyFunctions(usize),

    #[error("duplicate function: {0:?} is already declared on this node")]
    DuplicateFunction(String),

    #[error("function not found: {0:?}")]
    FunctionNotFound(String),
}

/// Convert a [`NodeIdentityError`] into the protocol-level
/// [`AdnetError::Validation`] variant. Used by [`NodeIdentity::new`]
/// which returns [`AdnetError`] for cross-crate consistency.
fn identity_err_to_a3net(e: NodeIdentityError) -> AdnetError {
    AdnetError::Validation(e.to_string())
}

impl NodeIdentity {
    /// Build a brand-new identity with the canonical invariants
    /// applied. `node_id` is recorded as both the routing key and the
    /// `digital_identity` field.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: NodeId,
        dns_node_id: DnsNodeId,
        nickname: impl Into<String>,
        email: impl Into<String>,
        avatar: Avatar,
        description: impl Into<String>,
        wallet_address: WalletAddress,
    ) -> Result<Self> {
        let now = current_timestamp();
        let mut me = Self {
            digital_identity: node_id.clone(),
            dns_node_id,
            nickname: String::new(),
            email: String::new(),
            avatar,
            description: String::new(),
            wallet_address,
            created_at: now,
            updated_at: now,
            kind: NodeKind::Human,
            functions: Vec::new(),
        };
        me.set_nickname(nickname).map_err(identity_err_to_a3net)?;
        me.set_email(email).map_err(identity_err_to_a3net)?;
        me.set_description(description).map_err(identity_err_to_a3net)?;
        // digital_identity starts as a copy of node_id by construction,
        // so the mismatch check below only fires if the caller mutates
        // it later via deserialised data.
        let _ = me.verify_digital_identity(&node_id);
        Ok(me)
    }

    /// Build a brand-new identity with an explicit [`NodeKind`].
    /// Convenience wrapper around [`NodeIdentity::new`] for callers
    /// that already know whether they are provisioning a human
    /// workstation, an AI agent, or an ops-tier node.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_kind(
        node_id: NodeId,
        dns_node_id: DnsNodeId,
        nickname: impl Into<String>,
        email: impl Into<String>,
        avatar: Avatar,
        description: impl Into<String>,
        wallet_address: WalletAddress,
        kind: NodeKind,
    ) -> Result<Self> {
        let mut me = Self::new(
            node_id,
            dns_node_id,
            nickname,
            email,
            avatar,
            description,
            wallet_address,
        )?;
        me.kind = kind;
        me.touch();
        Ok(me)
    }

    /// Replace the operator-class [`NodeKind`]. Bumps `updated_at`.
    pub fn set_kind(&mut self, kind: NodeKind) {
        self.kind = kind;
        self.touch();
    }

    /// Borrow the operator-class [`NodeKind`].
    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    // ── NodeFunction mutators ──────────────────────────────────────────

    /// Replace the entire [`NodeFunction`] list. Validates each entry
    /// (custom tags must be ≤ `MAX_NODE_FUNCTION_CUSTOM_LEN` bytes and
    /// snake-case) and enforces `MAX_NODE_FUNCTIONS`.
    pub fn set_functions(
        &mut self,
        functions: impl IntoIterator<Item = NodeFunction>,
    ) -> Result<(), NodeIdentityError> {
        let v: Vec<NodeFunction> = functions.into_iter().collect();
        if v.len() > MAX_NODE_FUNCTIONS {
            return Err(NodeIdentityError::TooManyFunctions(v.len()));
        }
        for f in &v {
            if let NodeFunction::Custom(tag) = f {
                NodeFunction::custom_validated(tag.clone())
                    .map_err(|e| NodeIdentityError::InvalidFunction(e.to_string()))?;
            }
        }
        self.functions = v;
        self.touch();
        Ok(())
    }

    /// Add a single function. Returns `Err(AlreadyDeclared)` if the
    /// function is already present (we dedup by `label()` so a
    /// `Custom("foo")` and a hypothetical future standard `Foo` would
    /// also collide on the same label).
    pub fn add_function(&mut self, f: NodeFunction) -> Result<(), NodeIdentityError> {
        if let NodeFunction::Custom(tag) = &f {
            NodeFunction::custom_validated(tag.clone())
                .map_err(|e| NodeIdentityError::InvalidFunction(e.to_string()))?;
        }
        if self.functions.iter().any(|existing| existing.label() == f.label()) {
            return Err(NodeIdentityError::DuplicateFunction(f.label()));
        }
        if self.functions.len() >= MAX_NODE_FUNCTIONS {
            return Err(NodeIdentityError::TooManyFunctions(self.functions.len() + 1));
        }
        self.functions.push(f);
        self.touch();
        Ok(())
    }

    /// Remove the first function whose `label()` matches.
    /// Returns `Err(NotFound)` if no entry matches.
    pub fn remove_function(&mut self, label: &str) -> Result<NodeFunction, NodeIdentityError> {
        let pos = self
            .functions
            .iter()
            .position(|f| f.label() == label)
            .ok_or_else(|| NodeIdentityError::FunctionNotFound(label.to_string()))?;
        let removed = self.functions.remove(pos);
        self.touch();
        Ok(removed)
    }

    /// Borrow the function list.
    pub fn functions(&self) -> &[NodeFunction] {
        &self.functions
    }

    /// `true` if the node declares a function whose label matches
    /// `label` (e.g. `id.has_function("ai_compute")`).
    pub fn has_function(&self, label: &str) -> bool {
        self.functions.iter().any(|f| f.label() == label)
    }

    /// `true` if the node declares any of the supplied functions.
    /// Useful for routing: "find peers that serve **either**
    /// `ai_compute` **or** `model_catalog`".
    pub fn has_any_function<'a>(&self, labels: impl IntoIterator<Item = &'a str>) -> bool {
        let set: std::collections::HashSet<&str> = labels.into_iter().collect();
        self.functions
            .iter()
            .any(|f| set.contains(f.label().as_str()))
    }

    /// Imply a default function list from the current `kind` and
    /// apply it. Used when bootstrapping a fresh identity card that
    /// predates explicit function declarations. See
    /// [`default_functions_for_kind`].
    pub fn apply_default_functions_for_kind(&mut self) {
        self.functions = default_functions_for_kind(self.kind);
        self.touch();
    }

    /// Replace the email, validating format.
    pub fn set_email(&mut self, email: impl Into<String>) -> Result<(), NodeIdentityError> {
        let email = email.into();
        validate_email(&email)
            .map_err(|e| NodeIdentityError::InvalidEmail(e.to_string()))?;
        self.email = email;
        self.touch();
        Ok(())
    }

    /// Replace the nickname, validating charset + length.
    pub fn set_nickname(
        &mut self,
        nickname: impl Into<String>,
    ) -> Result<(), NodeIdentityError> {
        let nickname = nickname.into();
        validate_nickname(&nickname)
            .map_err(|e| NodeIdentityError::InvalidNickname(e.to_string()))?;
        self.nickname = nickname;
        self.touch();
        Ok(())
    }

    /// Replace the description, validating length.
    pub fn set_description(
        &mut self,
        description: impl Into<String>,
    ) -> Result<(), NodeIdentityError> {
        let desc = description.into();
        validate_description(&desc)
            .map_err(|e| NodeIdentityError::InvalidDescription(e.to_string()))?;
        self.description = desc;
        self.touch();
        Ok(())
    }

    /// Replace the avatar.
    pub fn set_avatar(&mut self, avatar: Avatar) -> Result<(), NodeIdentityError> {
        // `Avatar::from_url` / `from_data_uri` already validated; the
        // constructor itself can't fail. We re-check size here so the
        // setter is a hard boundary.
        if avatar.approx_size() > MAX_AVATAR_DATA_LEN {
            return Err(NodeIdentityError::InvalidAvatar(format!(
                "exceeds {MAX_AVATAR_DATA_LEN} bytes"
            )));
        }
        self.avatar = avatar;
        self.touch();
        Ok(())
    }

    /// Replace the wallet address.
    pub fn set_wallet_address(
        &mut self,
        addr: WalletAddress,
    ) -> Result<(), NodeIdentityError> {
        // WalletAddress::from_bytes is infallible; the only
        // construction-time check is length which already passed.
        self.wallet_address = addr;
        self.touch();
        Ok(())
    }

    /// Validate that `digital_identity` equals `node_id`. Called by
    /// the IPC boundary before persisting a deserialised identity
    /// to disk.
    pub fn verify_digital_identity(&self, node_id: &NodeId) -> Result<(), NodeIdentityError> {
        if &self.digital_identity != node_id {
            return Err(NodeIdentityError::DigitalIdentityMismatch(
                self.digital_identity.to_string(),
            ));
        }
        Ok(())
    }

    /// Refresh `updated_at`. Called by every setter.
    pub fn touch(&mut self) {
        self.updated_at = current_timestamp();
    }

    /// Approximate serialised JSON size in bytes. Used to budget
    /// gossip frames without serialising.
    pub fn approx_size(&self) -> usize {
        // 64 (digital_identity) + 12 (dns_node_id) + nickname +
        // email + avatar + description + 42 (wallet hex) + 32 (timestamps)
        let mut n = 64 + 12 + 32;
        n += self.nickname.len();
        n += self.email.len();
        n += self.avatar.approx_size();
        n += self.description.len();
        n += 42; // wallet_address hex (0x + 40)
        n
    }

    /// One-line summary for operator UIs / CLI tables.
    pub fn summary(&self) -> String {
        format!(
            "[{}] {} <{}> · dns={} · kind={} · wallet={}",
            self.digital_identity.short(),
            self.nickname,
            self.email,
            self.dns_node_id,
            self.kind.label(),
            self.wallet_address,
        )
    }
}

// ---------------------------------------------------------------------------
// Field-level validators
// ---------------------------------------------------------------------------

/// RFC-lite email check: single `@`, non-empty local + domain parts,
/// no NUL / control bytes, ≤ 254 bytes total (RFC 5321 §4.5.3.1.3).
pub fn validate_email(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 254 {
        return Err(AdnetError::Validation(format!(
            "email: length must be 1..=254 (got {})",
            s.len()
        )));
    }
    if s.as_bytes().contains(&0) {
        return Err(AdnetError::Validation("email: contains NUL".into()));
    }
    let at_count = s.bytes().filter(|&b| b == b'@').count();
    if at_count != 1 {
        return Err(AdnetError::Validation(format!(
            "email: must contain exactly one '@' (got {at_count})"
        )));
    }
    let (local, domain) = s.split_once('@').unwrap();
    if local.is_empty() || local.len() > 64 {
        return Err(AdnetError::Validation(format!(
            "email: local part must be 1..=64 bytes (got {})",
            local.len()
        )));
    }
    if !local.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-')
    }) {
        return Err(AdnetError::Validation(
            "email: local part contains invalid characters".into(),
        ));
    }
    if domain.is_empty() || !domain.contains('.') {
        return Err(AdnetError::Validation(
            "email: domain must contain a dot".into(),
        ));
    }
    if !domain.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')) {
        return Err(AdnetError::Validation(
            "email: domain contains invalid characters".into(),
        ));
    }
    Ok(())
}

fn validate_nickname(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > MAX_NICKNAME_LEN {
        return Err(AdnetError::Validation(format!(
            "nickname: length must be 1..={MAX_NICKNAME_LEN} (got {})",
            s.len()
        )));
    }
    if s.as_bytes().contains(&0) {
        return Err(AdnetError::Validation("nickname: contains NUL".into()));
    }
    // Reject control bytes (incl. newlines) — nicknames render in
    // many contexts where a newline would break the layout.
    for &b in s.as_bytes() {
        if b < 0x20 || b == 0x7f {
            return Err(AdnetError::Validation(format!(
                "nickname: contains control byte 0x{b:02x}"
            )));
        }
    }
    Ok(())
}

fn validate_description(s: &str) -> Result<()> {
    if s.len() > MAX_NODE_DESCRIPTION_LEN {
        return Err(AdnetError::Validation(format!(
            "description: exceeds {MAX_NODE_DESCRIPTION_LEN} bytes (got {})",
            s.len()
        )));
    }
    if s.as_bytes().contains(&0) {
        return Err(AdnetError::Validation(
            "description: contains NUL".into(),
        ));
    }
    Ok(())
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> NodeIdentity {
        let node_id = NodeId::random();
        let dns = DnsNodeId::parse("483726150931").unwrap();
        let avatar = Avatar::from_url("https://avatars.example.com/u/1.png").unwrap();
        let wallet = WalletAddress::from_bytes([0xAB; 20]);
        NodeIdentity::new(
            node_id,
            dns,
            "alice",
            "alice@example.com",
            avatar,
            "GPU inference node, eu-central",
            wallet,
        )
        .unwrap()
    }

    // ── DnsNodeId ─────────────────────────────────────────────────────────

    #[test]
    fn dns_node_id_parse_12_digits() {
        let v = DnsNodeId::parse("483726150931").unwrap();
        assert_eq!(v.as_u64(), 483_726_150_931);
        assert_eq!(v.to_digits(), "483726150931");
        assert_eq!(v.to_string(), "483726150931");
    }

    #[test]
    fn dns_node_id_parse_zero_padded() {
        let v = DnsNodeId::parse("000000000001").unwrap();
        assert_eq!(v.as_u64(), 1);
        assert_eq!(v.to_digits(), "000000000001");
    }

    #[test]
    fn dns_node_id_parse_max_value() {
        let v = DnsNodeId::parse("999999999999").unwrap();
        assert_eq!(v.as_u64(), DNS_NODE_ID_MAX);
    }

    #[test]
    fn dns_node_id_parse_rejects_wrong_length() {
        assert!(DnsNodeId::parse("12345").is_err());
        assert!(DnsNodeId::parse("1234567890123").is_err());
        assert!(DnsNodeId::parse("").is_err());
    }

    #[test]
    fn dns_node_id_parse_rejects_non_digit() {
        assert!(DnsNodeId::parse("48372615093a").is_err());
        assert!(DnsNodeId::parse("483726150931 ").is_err());
        assert!(DnsNodeId::parse("483726150931-").is_err());
    }

    #[test]
    fn dns_node_id_from_u64_clamps_at_max() {
        let v = DnsNodeId::from_u64(DNS_NODE_ID_MAX).unwrap();
        assert_eq!(v.as_u64(), DNS_NODE_ID_MAX);
        let err = DnsNodeId::from_u64(DNS_NODE_ID_MAX + 1).unwrap_err();
        assert!(matches!(err, AdnetError::Validation(_)));
        let err = DnsNodeId::from_u64(u64::MAX).unwrap_err();
        assert!(matches!(err, AdnetError::Validation(_)));
    }

    #[test]
    fn dns_node_id_display_has_12_chars() {
        let v = DnsNodeId::from_u64(42).unwrap();
        assert_eq!(v.to_digits().len(), 12);
        assert_eq!(v.to_string().len(), 12);
    }

    #[test]
    fn dns_node_id_serde_round_trip() {
        let v = DnsNodeId::parse("483726150931").unwrap();
        let s: String = v.clone().into();
        let back = DnsNodeId::try_from(s).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn dns_node_id_json_round_trip() {
        let v = DnsNodeId::parse("000000000001").unwrap();
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"000000000001\"");
        let back: DnsNodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn dns_node_id_hash_eq() {
        use std::collections::HashSet;
        let a = DnsNodeId::parse("483726150931").unwrap();
        let b = DnsNodeId::parse("483726150931").unwrap();
        let c = DnsNodeId::parse("483726150932").unwrap();
        let mut s = HashSet::new();
        s.insert(a);
        assert!(s.contains(&b));
        assert!(!s.contains(&c));
    }

    // ── Avatar ────────────────────────────────────────────────────────────

    #[test]
    fn avatar_url_ok() {
        let a = Avatar::from_url("https://example.com/a.png").unwrap();
        match a {
            Avatar::Url { url } => assert_eq!(url, "https://example.com/a.png"),
            _ => panic!("expected Url"),
        }
    }

    #[test]
    fn avatar_url_rejects_http() {
        assert!(Avatar::from_url("http://example.com/a.png").is_err());
    }

    #[test]
    fn avatar_url_rejects_too_long() {
        let url = format!("https://example.com/{}", "x".repeat(MAX_AVATAR_URL_LEN));
        assert!(Avatar::from_url(url).is_err());
    }

    #[test]
    fn avatar_url_rejects_nul() {
        assert!(Avatar::from_url("https://example.com/\0evil").is_err());
    }

    #[test]
    fn avatar_data_ok() {
        let a =
            Avatar::from_data_uri("png", "iVBORw0KGgo=").unwrap();
        match a {
            Avatar::Data {
                media_type,
                payload_b64,
            } => {
                assert_eq!(media_type, "png");
                assert_eq!(payload_b64, "iVBORw0KGgo=");
            }
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn avatar_data_rejects_bad_media_type() {
        assert!(Avatar::from_data_uri("", "abc").is_err());
        assert!(Avatar::from_data_uri("png/../etc", "abc").is_err());
        assert!(Avatar::from_data_uri("x".repeat(64).as_str(), "abc").is_err());
    }

    #[test]
    fn avatar_data_rejects_oversize() {
        let big = "A".repeat(MAX_AVATAR_DATA_LEN);
        assert!(Avatar::from_data_uri("png", &big).is_err());
    }

    #[test]
    fn avatar_serde_round_trip_url() {
        let a = Avatar::from_url("https://example.com/a.png").unwrap();
        let json = serde_json::to_string(&a).unwrap();
        let back: Avatar = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn avatar_serde_round_trip_data() {
        let a = Avatar::from_data_uri("jpeg", "/9j/4AAQ").unwrap();
        let json = serde_json::to_string(&a).unwrap();
        let back: Avatar = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn avatar_approx_size_url() {
        let a = Avatar::from_url("https://example.com/x.png").unwrap();
        assert_eq!(a.approx_size(), "https://example.com/x.png".len());
    }

    // ── validate_email ────────────────────────────────────────────────────

    #[test]
    fn email_valid() {
        for e in [
            "alice@example.com",
            "a.b+c@sub.example.co",
            "user_1@x.io",
            "x@y.z",
        ] {
            assert!(validate_email(e).is_ok(), "should accept {e}");
        }
    }

    #[test]
    fn email_rejects_empty() {
        assert!(validate_email("").is_err());
    }

    #[test]
    fn email_rejects_no_at() {
        assert!(validate_email("alice.example.com").is_err());
    }

    #[test]
    fn email_rejects_multiple_at() {
        assert!(validate_email("a@b@c.com").is_err());
    }

    #[test]
    fn email_rejects_no_dot_in_domain() {
        assert!(validate_email("alice@localhost").is_err());
    }

    #[test]
    fn email_rejects_invalid_chars() {
        assert!(validate_email("ali ce@example.com").is_err());
        assert!(validate_email("alice@exa mple.com").is_err());
        assert!(validate_email("alice@exa<mple>.com").is_err());
    }

    #[test]
    fn email_rejects_nul() {
        assert!(validate_email("ali\0ce@example.com").is_err());
    }

    #[test]
    fn email_rejects_too_long() {
        let local = "a".repeat(64);
        let domain = "example.com";
        let e = format!("{local}@{domain}");
        // 64+1+11 = 76 bytes, well under 254 — should pass.
        assert!(validate_email(&e).is_ok());
        let e = format!("{}@{}", "a".repeat(64), "example.com");
        let e = format!("{e}{}", "x".repeat(200));
        assert!(validate_email(&e).is_err());
    }

    #[test]
    fn email_rejects_empty_local() {
        assert!(validate_email("@example.com").is_err());
    }

    // ── validate_nickname ─────────────────────────────────────────────────

    #[test]
    fn nickname_valid() {
        for n in ["alice", "张三", "agent-007", "x".repeat(MAX_NICKNAME_LEN).as_str()] {
            assert!(validate_nickname(n).is_ok(), "should accept {n:?}");
        }
    }

    #[test]
    fn nickname_rejects_empty() {
        assert!(validate_nickname("").is_err());
    }

    #[test]
    fn nickname_rejects_too_long() {
        assert!(validate_nickname(&"x".repeat(MAX_NICKNAME_LEN + 1)).is_err());
    }

    #[test]
    fn nickname_rejects_nul_and_control() {
        assert!(validate_nickname("ali\0ce").is_err());
        assert!(validate_nickname("ali\nce").is_err());
        assert!(validate_nickname("ali\tce").is_err());
    }

    #[test]
    fn nickname_accepts_exactly_max() {
        assert!(validate_nickname(&"x".repeat(MAX_NICKNAME_LEN)).is_ok());
    }

    // ── validate_description ──────────────────────────────────────────────

    #[test]
    fn description_empty_ok() {
        assert!(validate_description("").is_ok());
    }

    #[test]
    fn description_at_max_ok() {
        assert!(validate_description(&"x".repeat(MAX_NODE_DESCRIPTION_LEN)).is_ok());
    }

    #[test]
    fn description_over_max_fails() {
        assert!(validate_description(&"x".repeat(MAX_NODE_DESCRIPTION_LEN + 1)).is_err());
    }

    #[test]
    fn description_rejects_nul() {
        assert!(validate_description("hello\0world").is_err());
    }

    #[test]
    fn description_accepts_utf8() {
        assert!(validate_description("中文描述 — 128 chars max").is_ok());
    }

    // ── NodeIdentity ──────────────────────────────────────────────────────

    #[test]
    fn identity_new_validates_every_field() {
        let id = sample_identity();
        assert_eq!(id.nickname, "alice");
        assert_eq!(id.email, "alice@example.com");
        assert_eq!(id.description, "GPU inference node, eu-central");
        assert!(id.created_at > 0);
        assert!(id.updated_at >= id.created_at);
    }

    #[test]
    fn identity_new_rejects_bad_email() {
        let node_id = NodeId::random();
        let dns = DnsNodeId::parse("000000000001").unwrap();
        let avatar = Avatar::from_url("https://example.com/a.png").unwrap();
        let wallet = WalletAddress::from_bytes([0; 20]);
        let err = NodeIdentity::new(
            node_id,
            dns,
            "alice",
            "not-an-email",
            avatar,
            "",
            wallet,
        )
        .unwrap_err();
        assert!(matches!(err, AdnetError::Validation(_)));
    }

    #[test]
    fn identity_new_rejects_bad_nickname() {
        let node_id = NodeId::random();
        let dns = DnsNodeId::parse("000000000001").unwrap();
        let avatar = Avatar::from_url("https://example.com/a.png").unwrap();
        let wallet = WalletAddress::from_bytes([0; 20]);
        let err = NodeIdentity::new(
            node_id,
            dns,
            "",
            "alice@example.com",
            avatar,
            "",
            wallet,
        )
        .unwrap_err();
        assert!(matches!(err, AdnetError::Validation(_)));
    }

    #[test]
    fn identity_new_rejects_bad_description() {
        let node_id = NodeId::random();
        let dns = DnsNodeId::parse("000000000001").unwrap();
        let avatar = Avatar::from_url("https://example.com/a.png").unwrap();
        let wallet = WalletAddress::from_bytes([0; 20]);
        let err = NodeIdentity::new(
            node_id,
            dns,
            "alice",
            "alice@example.com",
            avatar,
            "x".repeat(MAX_NODE_DESCRIPTION_LEN + 1),
            wallet,
        )
        .unwrap_err();
        assert!(matches!(err, AdnetError::Validation(_)));
    }

    #[test]
    fn identity_setters_touch() {
        let mut id = sample_identity();
        let original = id.updated_at;
        std::thread::sleep(std::time::Duration::from_secs(1));
        id.set_nickname("alice-2").unwrap();
        assert!(id.updated_at > original);
        assert_eq!(id.nickname, "alice-2");
    }

    #[test]
    fn identity_set_email_invalid() {
        let mut id = sample_identity();
        let err = id.set_email("nope").unwrap_err();
        assert!(matches!(err, NodeIdentityError::InvalidEmail(_)));
    }

    #[test]
    fn identity_set_nickname_invalid() {
        let mut id = sample_identity();
        let err = id.set_nickname("").unwrap_err();
        assert!(matches!(err, NodeIdentityError::InvalidNickname(_)));
    }

    #[test]
    fn identity_set_description_invalid() {
        let mut id = sample_identity();
        let err = id.set_description("x".repeat(MAX_NODE_DESCRIPTION_LEN + 1)).unwrap_err();
        assert!(matches!(err, NodeIdentityError::InvalidDescription(_)));
    }

    #[test]
    fn identity_verify_digital_identity_ok() {
        let id = sample_identity();
        id.verify_digital_identity(&id.digital_identity).unwrap();
    }

    #[test]
    fn identity_verify_digital_identity_mismatch() {
        let mut id = sample_identity();
        let other = NodeId::random();
        id.digital_identity = other.clone();
        // We mutated digital_identity to match; should pass.
        id.verify_digital_identity(&other).unwrap();
        let err = id
            .verify_digital_identity(&NodeId::random())
            .unwrap_err();
        assert!(matches!(
            err,
            NodeIdentityError::DigitalIdentityMismatch(_)
        ));
    }

    #[test]
    fn identity_summary_contains_fields() {
        let id = sample_identity();
        let s = id.summary();
        assert!(s.contains("alice"));
        assert!(s.contains("alice@example.com"));
        assert!(s.contains("483726150931"));
    }

    #[test]
    fn identity_approx_size_includes_payload() {
        let id = sample_identity();
        let n = id.approx_size();
        assert!(n > 64 + 12 + id.nickname.len() + id.email.len() + id.description.len());
    }

    #[test]
    fn identity_serde_round_trip() {
        let id = sample_identity();
        let json = serde_json::to_string(&id).unwrap();
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(id.digital_identity, back.digital_identity);
        assert_eq!(id.dns_node_id, back.dns_node_id);
        assert_eq!(id.nickname, back.nickname);
        assert_eq!(id.email, back.email);
        assert_eq!(id.description, back.description);
        assert_eq!(id.wallet_address, back.wallet_address);
        assert_eq!(id.created_at, back.created_at);
    }

    #[test]
    fn identity_serde_camel_case_fields() {
        let id = sample_identity();
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.contains("\"digitalIdentity\""));
        assert!(json.contains("\"dnsNodeId\""));
        assert!(json.contains("\"walletAddress\""));
        assert!(json.contains("\"createdAt\""));
        assert!(json.contains("\"updatedAt\""));
    }

    #[test]
    fn identity_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NodeIdentity>();
        assert_send_sync::<DnsNodeId>();
        assert_send_sync::<Avatar>();
    }

    // ── NodeKind ───────────────────────────────────────────────────────────

    #[test]
    fn node_kind_default_is_human() {
        assert_eq!(NodeKind::default(), NodeKind::Human);
    }

    #[test]
    fn node_kind_label_and_from_label_round_trip() {
        for k in [
            NodeKind::AiAgent,
            NodeKind::AiCompute,
            NodeKind::Human,
            NodeKind::NetworkManager,
        ] {
            assert_eq!(NodeKind::from_label(k.label()), Some(k));
            assert_eq!(k.label(), format!("{k}"));
        }
    }

    #[test]
    fn node_kind_from_label_accepts_short_tags() {
        assert_eq!(NodeKind::from_label("aa"), Some(NodeKind::AiAgent));
        assert_eq!(NodeKind::from_label("ac"), Some(NodeKind::AiCompute));
        assert_eq!(NodeKind::from_label("hu"), Some(NodeKind::Human));
        assert_eq!(NodeKind::from_label("nm"), Some(NodeKind::NetworkManager));
        assert_eq!(NodeKind::from_label("agent"), Some(NodeKind::AiAgent));
        assert_eq!(NodeKind::from_label("compute"), Some(NodeKind::AiCompute));
        assert_eq!(NodeKind::from_label("ollama"), Some(NodeKind::AiCompute));
        assert_eq!(NodeKind::from_label("inference"), Some(NodeKind::AiCompute));
        assert_eq!(NodeKind::from_label("ops"), Some(NodeKind::NetworkManager));
    }

    #[test]
    fn node_kind_from_label_rejects_unknown() {
        assert_eq!(NodeKind::from_label(""), None);
        assert_eq!(NodeKind::from_label("robot"), None);
        assert_eq!(NodeKind::from_label("ADMIN"), None);
    }

    #[test]
    fn node_kind_pkarr_tag_is_at_most_2_bytes() {
        for k in [
            NodeKind::AiAgent,
            NodeKind::AiCompute,
            NodeKind::Human,
            NodeKind::NetworkManager,
        ] {
            assert!(k.pkarr_tag().len() <= 4, "{} too long", k.pkarr_tag());
        }
    }

    #[test]
    fn node_kind_serde_snake_case() {
        let json = serde_json::to_string(&NodeKind::AiAgent).unwrap();
        assert_eq!(json, "\"ai_agent\"");
        let json = serde_json::to_string(&NodeKind::AiCompute).unwrap();
        assert_eq!(json, "\"ai_compute\"");
        let json = serde_json::to_string(&NodeKind::NetworkManager).unwrap();
        assert_eq!(json, "\"network_manager\"");
        let back: NodeKind = serde_json::from_str("\"ai_agent\"").unwrap();
        assert_eq!(back, NodeKind::AiAgent);
        let back: NodeKind = serde_json::from_str("\"ai_compute\"").unwrap();
        assert_eq!(back, NodeKind::AiCompute);
    }

    #[test]
    fn node_kind_is_realtime() {
        assert!(NodeKind::Human.is_realtime());
        assert!(NodeKind::AiAgent.is_realtime());
        assert!(NodeKind::AiCompute.is_realtime());
        assert!(!NodeKind::NetworkManager.is_realtime());
    }

    #[test]
    fn node_kind_serves_inference() {
        // Only AiCompute produces tokens.
        assert!(NodeKind::AiCompute.serves_inference());
        assert!(!NodeKind::AiAgent.serves_inference());
        assert!(!NodeKind::Human.serves_inference());
        assert!(!NodeKind::NetworkManager.serves_inference());
        assert!(NodeKind::AiCompute.is_inference_provider());
    }

    #[test]
    fn node_kind_consumes_inference() {
        // AiAgent, Human, AiCompute can all *issue* chat requests.
        assert!(NodeKind::AiAgent.consumes_inference());
        assert!(NodeKind::Human.consumes_inference());
        assert!(NodeKind::AiCompute.consumes_inference()); // model fallback / A-B
        assert!(!NodeKind::NetworkManager.consumes_inference());
    }

    // ── NodeIdentity.kind ─────────────────────────────────────────────────

    #[test]
    fn identity_new_defaults_kind_to_human() {
        let id = sample_identity();
        assert_eq!(id.kind(), NodeKind::Human);
        assert_eq!(id.kind, NodeKind::Human);
    }

    #[test]
    fn identity_new_with_kind_sets_kind() {
        let node_id = NodeId::random();
        let dns = DnsNodeId::parse("000000000007").unwrap();
        let avatar = Avatar::from_url("https://example.com/a.png").unwrap();
        let wallet = WalletAddress::from_bytes([0xAA; 20]);
        let id = NodeIdentity::new_with_kind(
            node_id,
            dns,
            "agent-α",
            "agent@example.com",
            avatar,
            "AI agent node",
            wallet,
            NodeKind::AiAgent,
        )
        .unwrap();
        assert_eq!(id.kind(), NodeKind::AiAgent);
        assert_eq!(id.nickname, "agent-α");
    }

    #[test]
    fn identity_set_kind_touches() {
        let mut id = sample_identity();
        let original = id.updated_at;
        std::thread::sleep(std::time::Duration::from_secs(1));
        id.set_kind(NodeKind::AiAgent);
        assert_eq!(id.kind(), NodeKind::AiAgent);
        assert!(id.updated_at > original);
    }

    #[test]
    fn identity_summary_includes_kind() {
        let id = sample_identity();
        let s = id.summary();
        assert!(s.contains("kind=human"), "summary was {s:?}");
        let mut id2 = id.clone();
        id2.set_kind(NodeKind::NetworkManager);
        assert!(id2.summary().contains("kind=network_manager"));
    }

    #[test]
    fn identity_serde_backward_compat_missing_kind_defaults_to_human() {
        // Legacy v1 payloads didn't carry a `kind` field.
        let legacy = r#"{
            "digitalIdentity": "0000000000000000000000000000000000000000000000000000000000000000",
            "dnsNodeId": "000000000001",
            "nickname": "alice",
            "email": "alice@example.com",
            "avatar": { "kind": "url", "url": "https://example.com/a.png" },
            "description": "",
            "walletAddress": "0x0000000000000000000000000000000000000000",
            "createdAt": 1700000000,
            "updatedAt": 1700000000
        }"#;
        let id: NodeIdentity = serde_json::from_str(legacy).unwrap();
        assert_eq!(id.kind, NodeKind::Human);
    }

    #[test]
    fn identity_serde_round_trip_preserves_kind() {
        let mut id = sample_identity();
        id.set_kind(NodeKind::AiAgent);
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.contains("\"kind\":\"ai_agent\""));
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, NodeKind::AiAgent);
    }

    #[test]
    fn identity_card_propagates_kind() {
        // Verify the new field rides through NodeIdentityCard.
        let id = NodeIdentity::new_with_kind(
            NodeId::random(),
            DnsNodeId::parse("000000000009").unwrap(),
            "bot",
            "bot@example.com",
            Avatar::from_url("https://example.com/a.png").unwrap(),
            "ai agent",
            WalletAddress::from_bytes([0x11; 20]),
            NodeKind::AiAgent,
        )
        .unwrap();
        let card = crate::node_identity_card::NodeIdentityCard::new(
            id.clone(),
            None,
            None,
        );
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("\"kind\":\"ai_agent\""));
        let back: crate::node_identity_card::NodeIdentityCard =
            serde_json::from_str(&json).unwrap();
        assert_eq!(back.identity.kind, NodeKind::AiAgent);
    }

    // ── NodeFunction ─────────────────────────────────────────────────────────

    #[test]
    fn node_function_label_round_trip_all_standard_variants() {
        let variants = [
            NodeFunction::AiAgent,
            NodeFunction::AiCompute,
            NodeFunction::ModelCatalog,
            NodeFunction::Embedding,
            NodeFunction::BlobStore,
            NodeFunction::WorkspaceHost,
            NodeFunction::BackupVault,
            NodeFunction::Nas,
            NodeFunction::Relay,
            NodeFunction::MailRelay,
            NodeFunction::MqttBridge,
            NodeFunction::PubsubBroker,
            NodeFunction::ChatRelay,
            NodeFunction::DnsResolver,
            NodeFunction::PkarrPublisher,
            NodeFunction::MeshMonitor,
            NodeFunction::AuditAggregator,
            NodeFunction::SshGateway,
            NodeFunction::ExitNode,
        ];
        for v in &variants {
            let label = v.label();
            let back = NodeFunction::from_label(&label).unwrap();
            assert_eq!(&back, v, "round-trip mismatch for {label}");
        }
    }

    #[test]
    fn node_function_label_is_snake_case() {
        for v in [
            NodeFunction::AiAgent,
            NodeFunction::AiCompute,
            NodeFunction::ModelCatalog,
            NodeFunction::WorkspaceHost,
        ] {
            let label = v.label();
            assert!(!label.is_empty());
            assert!(
                label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "label must be snake_case, got {label:?}"
            );
        }
    }

    #[test]
    fn node_function_pkarr_tag_is_short_or_none() {
        for v in [
            NodeFunction::AiAgent,
            NodeFunction::AiCompute,
            NodeFunction::DnsResolver,
            NodeFunction::BackupVault,
        ] {
            let tag = v.pkarr_tag().expect("standard variant must have a pkarr tag");
            assert!(tag.len() <= 4, "tag too long for {v:?}: {tag}");
        }
        assert!(NodeFunction::Custom("acme.weather".into())
            .pkarr_tag()
            .is_none());
    }

    #[test]
    fn node_function_from_label_accepts_casual_aliases() {
        assert_eq!(
            NodeFunction::from_label("ollama"),
            Some(NodeFunction::AiCompute)
        );
        assert_eq!(
            NodeFunction::from_label("inference"),
            Some(NodeFunction::AiCompute)
        );
        assert_eq!(
            NodeFunction::from_label("smtp"),
            Some(NodeFunction::MailRelay)
        );
        assert_eq!(
            NodeFunction::from_label("socks"),
            Some(NodeFunction::ExitNode)
        );
        assert_eq!(
            NodeFunction::from_label("monitor"),
            Some(NodeFunction::MeshMonitor)
        );
    }

    #[test]
    fn node_function_from_label_unknown_falls_back_to_custom() {
        let f = NodeFunction::from_label("acme.weather-feed").unwrap();
        match f {
            NodeFunction::Custom(tag) => assert_eq!(tag, "acme.weather-feed"),
            _ => panic!("unknown label should land in Custom"),
        }
    }

    #[test]
    fn node_function_is_standard_for_known_variants_only() {
        assert!(NodeFunction::AiAgent.is_standard());
        assert!(NodeFunction::Relay.is_standard());
        assert!(NodeFunction::DnsResolver.is_standard());
        assert!(!NodeFunction::Custom("x".into()).is_standard());
    }

    #[test]
    fn node_function_category_groups_logically() {
        assert_eq!(NodeFunction::AiAgent.category(), "ai");
        assert_eq!(NodeFunction::AiCompute.category(), "ai");
        assert_eq!(NodeFunction::Embedding.category(), "ai");
        assert_eq!(NodeFunction::BlobStore.category(), "storage");
        assert_eq!(NodeFunction::WorkspaceHost.category(), "storage");
        assert_eq!(NodeFunction::Relay.category(), "communication");
        assert_eq!(NodeFunction::MailRelay.category(), "communication");
        assert_eq!(NodeFunction::ChatRelay.category(), "communication");
        assert_eq!(NodeFunction::DnsResolver.category(), "infrastructure");
        assert_eq!(NodeFunction::MeshMonitor.category(), "infrastructure");
        assert_eq!(NodeFunction::SshGateway.category(), "security");
        assert_eq!(NodeFunction::ExitNode.category(), "security");
        assert_eq!(NodeFunction::Custom("acme".into()).category(), "custom");
    }

    #[test]
    fn node_function_custom_validated_accepts_well_formed_tag() {
        for tag in [
            "acme.weather-feed",
            "mycompany.stream_gateway",
            "v2.streaming",
            "a",
            "a.b.c.d.e.f",
        ] {
            assert!(
                NodeFunction::custom_validated(tag).is_ok(),
                "should accept {tag:?}"
            );
        }
    }

    #[test]
    fn node_function_custom_validated_rejects_bad_chars() {
        for bad in [
            "",            // empty
            "UPPER",       // uppercase
            "MixedCase",
            "has space",
            "has/slash",
            "has+plus",
            "has:colon",
            ".leading-dot",
            "-leading-dash",
            &"x".repeat(MAX_NODE_FUNCTION_CUSTOM_LEN + 1),
        ] {
            assert!(
                NodeFunction::custom_validated(bad).is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn node_function_serde_round_trip_standard() {
        for v in [
            NodeFunction::AiAgent,
            NodeFunction::AiCompute,
            NodeFunction::Relay,
            NodeFunction::DnsResolver,
            NodeFunction::ChatRelay,
        ] {
            let json = serde_json::to_string(&v).unwrap();
            let back: NodeFunction = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v, "serde round-trip for {v:?}");
        }
    }

    #[test]
    fn node_function_serde_round_trip_custom() {
        let f = NodeFunction::Custom("acme.weather-feed".into());
        let json = serde_json::to_string(&f).unwrap();
        let back: NodeFunction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn node_function_serde_uses_snake_case_for_known() {
        assert_eq!(
            serde_json::to_string(&NodeFunction::AiAgent).unwrap(),
            "\"ai_agent\""
        );
        assert_eq!(
            serde_json::to_string(&NodeFunction::ModelCatalog).unwrap(),
            "\"model_catalog\""
        );
        assert_eq!(
            serde_json::to_string(&NodeFunction::WorkspaceHost).unwrap(),
            "\"workspace_host\""
        );
        assert_eq!(
            serde_json::to_string(&NodeFunction::AuditAggregator).unwrap(),
            "\"audit_aggregator\""
        );
    }

    // ── NodeIdentity::functions API ────────────────────────────────────────

    fn fresh_identity() -> NodeIdentity {
        let node_id = NodeId::random();
        let dns_id = DnsNodeId::from_u64(1).unwrap();
        let wallet = WalletAddress::from_bytes([0xAA; 20]);
        NodeIdentity::new(
            node_id,
            dns_id,
            "alice",
            "alice@example.com",
            Avatar::Url {
                url: "https://example.com/a.png".into(),
            },
            "",
            wallet,
        )
        .unwrap()
    }

    #[test]
    fn identity_functions_default_empty() {
        let id = fresh_identity();
        assert!(id.functions().is_empty());
    }

    #[test]
    fn identity_add_function_dedups_and_validates() {
        let mut id = fresh_identity();
        assert!(id.add_function(NodeFunction::Relay).is_ok());
        // duplicate label rejected even via different syntax.
        assert!(matches!(
            id.add_function(NodeFunction::Relay),
            Err(NodeIdentityError::DuplicateFunction(_))
        ));
        // bad custom tag rejected.
        assert!(matches!(
            id.add_function(NodeFunction::Custom("Has Space".into())),
            Err(NodeIdentityError::InvalidFunction(_))
        ));
        // well-formed custom tag accepted.
        assert!(id
            .add_function(NodeFunction::Custom("acme.stream".into()))
            .is_ok());
        assert_eq!(id.functions().len(), 2);
        assert!(id.has_function("relay"));
        assert!(id.has_function("acme.stream"));
        assert!(!id.has_function("ai_compute"));
    }

    #[test]
    fn identity_remove_function_touches_timestamp() {
        let mut id = fresh_identity();
        id.add_function(NodeFunction::BlobStore).unwrap();
        let before = id.updated_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let removed = id.remove_function("blob_store").unwrap();
        assert_eq!(removed, NodeFunction::BlobStore);
        assert!(id.updated_at >= before);
        assert!(matches!(
            id.remove_function("blob_store"),
            Err(NodeIdentityError::FunctionNotFound(_))
        ));
    }

    #[test]
    fn identity_set_functions_replaces_and_validates_all() {
        let mut id = fresh_identity();
        id.add_function(NodeFunction::Relay).unwrap();
        let new_list = vec![
            NodeFunction::AiCompute,
            NodeFunction::Embedding,
            NodeFunction::Custom("acme.fanout".into()),
        ];
        id.set_functions(new_list).unwrap();
        assert_eq!(id.functions().len(), 3);
        assert!(id.has_function("ai_compute"));
        assert!(!id.has_function("relay"));
        // Mixed bag with one bad entry → Err, no mutation.
        let bad = vec![
            NodeFunction::Relay,
            NodeFunction::Custom("HAS SPACE".into()),
        ];
        assert!(id.set_functions(bad).is_err());
        // Functions unchanged after failed call.
        assert_eq!(id.functions().len(), 3);
        assert!(id.has_function("ai_compute"));
    }

    #[test]
    fn identity_too_many_functions_rejected() {
        let mut id = fresh_identity();
        let many: Vec<NodeFunction> = (0..MAX_NODE_FUNCTIONS + 1)
            .map(|i| NodeFunction::Custom(format!("f{i}")))
            .collect();
        assert!(matches!(
            id.set_functions(many),
            Err(NodeIdentityError::TooManyFunctions(_))
        ));
        // Exactly MAX_NODE_FUNCTIONS is fine.
        let exactly_max: Vec<NodeFunction> = (0..MAX_NODE_FUNCTIONS)
            .map(|i| NodeFunction::Custom(format!("f{i}")))
            .collect();
        id.set_functions(exactly_max).unwrap();
        assert_eq!(id.functions().len(), MAX_NODE_FUNCTIONS);
        // Adding one more now overflows.
        assert!(matches!(
            id.add_function(NodeFunction::Relay),
            Err(NodeIdentityError::TooManyFunctions(_))
        ));
    }

    #[test]
    fn identity_has_any_function_matches_any_member() {
        let mut id = fresh_identity();
        id.add_function(NodeFunction::Relay).unwrap();
        id.add_function(NodeFunction::ChatRelay).unwrap();
        // OR-of-disjoint sets.
        assert!(id.has_any_function(["dns_resolver", "relay", "ai_compute"]));
        assert!(id.has_any_function(["chat_relay"]));
        // Empty set is a vacuous match — return false.
        assert!(!id.has_any_function(std::iter::empty::<&str>()));
        assert!(!id.has_any_function(["ai_compute", "dns_resolver"]));
    }

    #[test]
    fn identity_apply_default_functions_for_kind() {
        let mut id = fresh_identity();
        id.set_kind(NodeKind::AiAgent);
        id.apply_default_functions_for_kind();
        assert!(id.has_function("ai_agent"));

        id.set_kind(NodeKind::AiCompute);
        id.apply_default_functions_for_kind();
        assert!(id.has_function("ai_compute"));

        id.set_kind(NodeKind::NetworkManager);
        id.apply_default_functions_for_kind();
        assert!(id.has_function("relay"));

        id.set_kind(NodeKind::Human);
        id.apply_default_functions_for_kind();
        assert!(id.functions().is_empty());
    }

    #[test]
    fn identity_functions_serde_omitted_when_empty() {
        let id = fresh_identity();
        let json = serde_json::to_string(&id).unwrap();
        // skip_serializing_if = "Vec::is_empty" — the field should
        // not appear on the wire when no functions are declared.
        assert!(!json.contains("\"functions\""), "got: {json}");

        // And back: an old client with `functions` missing should
        // round-trip without panic.
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert!(back.functions().is_empty());
    }

    #[test]
    fn identity_functions_serde_round_trip_with_mixed_standard_and_custom() {
        let mut id = fresh_identity();
        id.set_functions([
            NodeFunction::AiAgent,
            NodeFunction::Relay,
            NodeFunction::Custom("acme.fanout".into()),
        ])
        .unwrap();
        let json = serde_json::to_string(&id).unwrap();
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.functions().len(), 3);
        assert!(back.has_function("ai_agent"));
        assert!(back.has_function("relay"));
        assert!(back.has_function("acme.fanout"));
    }

    #[test]
    fn identity_legacy_v1_card_without_functions_loads() {
        // A hand-written v1 (pre-functions) identity JSON.
        let v1 = r#"{
            "digitalIdentity": "0000000000000000000000000000000000000000000000000000000000000000",
            "dnsNodeId": "000000000001",
            "nickname": "alice",
            "email": "alice@example.com",
            "avatar": { "kind": "url", "url": "https://example.com/a.png" },
            "description": "",
            "walletAddress": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "createdAt": 0,
            "updatedAt": 0,
            "kind": "ai_compute"
        }"#;
        let id: NodeIdentity = serde_json::from_str(v1).unwrap();
        assert_eq!(id.kind, NodeKind::AiCompute);
        assert!(id.functions().is_empty(), "missing field → empty list");
    }

    #[test]
    fn default_functions_for_kind_returns_expected_lists() {
        assert_eq!(
            default_functions_for_kind(NodeKind::AiAgent),
            vec![NodeFunction::AiAgent]
        );
        assert_eq!(
            default_functions_for_kind(NodeKind::AiCompute),
            vec![NodeFunction::AiCompute]
        );
        assert_eq!(
            default_functions_for_kind(NodeKind::NetworkManager),
            vec![NodeFunction::Relay]
        );
        assert!(default_functions_for_kind(NodeKind::Human).is_empty());
    }
}
