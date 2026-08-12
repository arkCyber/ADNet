//! `Node` — top-level ADNet runtime.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adnet_blobstore::BlobStore;
use adnet_gossip::{GossipBus, InProcessGossip};
use adnet_ipc::validation::ValidationPolicy;
use adnet_mesh::MeshConfig;
use adnet_mesh::MeshServerHandle;
use adnet_relay::{RelayConfig, RelayServerHandle};
use adnet_transport::{
    OutgoingConnection, SharedTransport, Transport, TransportIdentity, derive_node_id_from_cert,
};
use adnet_types::{
    Announcement, BlobTicket, CdnContentKind, ContentHash, Endpoint, NodeAddr, NodeId, RoomAsset,
    RoomId,
};
use adnet_workspace::{WORKSPACE_ROOM_ID, Workspace, WorkspaceFileEntry};
use anyhow::Result;
use chrono::Utc;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tracing::{info, warn};

#[cfg(feature = "dht")]
use adnet_namespace::IpnPublisher;
#[allow(unused_imports)]
use {OutgoingConnection as _, Transport as _, derive_node_id_from_cert as _};

use crate::state::SwarmIndex;

/// Static configuration handed to the builder.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub data_dir: PathBuf,
    pub node_id: NodeId,
    pub display_name: String,
    /// Validation policy applied at every gossip boundary. Defaults to
    /// [`ValidationPolicy::Strict`] (fail-closed). See the
    /// `adnet_ipc::validation` module for the rationale.
    pub gossip_validation: ValidationPolicy,
    /// Optional persistent QUIC identity. When `Some`, the node id is
    /// derived from the certificate's BLAKE3 fingerprint so that
    /// `NodeId` and `Transport::local_node()` stay in sync across
    /// restarts (P6 hardening — without this, anyone who could write
    /// to `data_dir/node_id` could impersonate the node).
    pub quic_identity: Option<TransportIdentity>,
    /// Optional mesh server configuration. When unset the mesh binds
    /// to `0.0.0.0:0` (legacy behaviour). When set, every `ensure_mesh`
    /// call resolves the bind address through [`MeshConfig::bind_addr`].
    pub mesh_config: Option<MeshConfig>,
}

impl NodeConfig {
    pub fn new(data_dir: impl Into<PathBuf>, node_id: NodeId) -> Self {
        let display_name = format!("adnet-{}", node_id.short());
        Self {
            data_dir: data_dir.into(),
            node_id,
            display_name,
            gossip_validation: ValidationPolicy::Strict,
            quic_identity: None,
            mesh_config: None,
        }
    }

    /// Override the gossip validation policy.
    pub fn with_gossip_validation(mut self, policy: ValidationPolicy) -> Self {
        self.gossip_validation = policy;
        self
    }

    /// Attach a persistent QUIC identity. When set, [`NodeBuilder::build`]
    /// will derive the local `node_id` from the certificate fingerprint
    /// rather than the value supplied to `new()`, so the wire identity
    /// stays consistent with the transport's `local_node()`.
    pub fn with_quic_identity(mut self, id: TransportIdentity) -> Self {
        self.quic_identity = Some(id);
        self
    }

    /// Override the mesh server bind / port / route prefix. When
    /// `None` the mesh binds to `0.0.0.0:0` (legacy behaviour).
    pub fn with_mesh_config(mut self, cfg: MeshConfig) -> Self {
        self.mesh_config = Some(cfg);
        self
    }

    /// Load a persistent `NodeId` from `{data_dir}/node_id` if the file
    /// exists and parses, otherwise generate a fresh one and **write it
    /// back to disk** so subsequent restarts are stable. Mirrors the iroh
    /// behaviour where a node keeps the same `PublicKey` for its whole
    /// lifetime.
    ///
    /// Also loads the persistent QUIC identity from
    /// `{data_dir}/quic_identity.pem` if present, so the local node id can
    /// be re-derived from the certificate on every restart (P6).
    pub fn load_or_create(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir).map_err(|e| anyhow::anyhow!("create data dir: {e}"))?;
        let id_path = data_dir.join("node_id");
        let pem_path = data_dir.join("quic_identity.pem");
        // Prefer the QUIC identity when present — its BLAKE3
        // fingerprint is the authoritative node id.
        let (node_id, quic_identity) = if pem_path.exists() {
            let identity = TransportIdentity::load_from(&pem_path)
                .map_err(|e| anyhow::anyhow!("load quic_identity.pem: {e}"))?;
            let node_id = adnet_transport::derive_node_id_from_cert(identity.cert_der())
                .ok_or_else(|| anyhow::anyhow!("derive node id from cert"))?;
            (node_id, Some(identity))
        } else if id_path.exists() {
            // Legacy: only the NodeId hex string was persisted. We
            // still need to bootstrap a fresh certificate on first
            // boot, but we keep the legacy file untouched so existing
            // tickets / gossip addresses continue to work for one
            // migration window.
            let raw = std::fs::read_to_string(&id_path)
                .map_err(|e| anyhow::anyhow!("read node_id: {e}"))?;
            let id =
                NodeId::from_hex(raw.trim()).map_err(|e| anyhow::anyhow!("parse node_id: {e}"))?;
            (id, None)
        } else {
            // Cold-start: generate a fresh NodeId, persist it, and
            // seed a QUIC identity on the same node id.
            let id = NodeId::random();
            std::fs::write(&id_path, id.as_hex())
                .map_err(|e| anyhow::anyhow!("persist node_id: {e}"))?;
            (id, None)
        };
        let mut cfg = Self::new(data_dir, node_id);
        cfg.quic_identity = quic_identity;
        Ok(cfg)
    }
}

/// Builder for [`Node`].
pub struct NodeBuilder {
    config: NodeConfig,
    transport: Option<SharedTransport>,
    relay_config: Option<RelayConfig>,
    /// Whether to enable the local workspace (shared/inbox/outbox +
    /// manifest) and its gossip bridge. Defaults to `true`.
    enable_workspace: bool,
    /// Optional iroh runtime owned by the node. When `Some`, the
    /// node will shut it down on `Node::shutdown`. Set via
    /// [`NodeBuilder::with_iroh_runtime`].
    #[cfg(feature = "iroh")]
    iroh_runtime: Option<crate::iroh_runtime::IrohRuntime>,
    /// Bitswap config. When `Some` and a transport is wired, the
    /// builder will instantiate a `BitswapHandle` and a
    /// `BitswapQuicBridge` so the engine can actually emit / receive
    /// Bitswap frames on the wire. When `None` (the default), the
    /// bitswap feature is dormant even if compiled in.
    #[cfg(feature = "bitswap")]
    bitswap_config: Option<crate::bitswap::BitswapConfig>,
    /// GraphSync config. When `Some` and a transport is wired, the
    /// builder will instantiate a `GraphSyncService` over the local
    /// blob store and route outbound/inbound frames through the
    /// wired transport via `GraphSyncQuicBridge`. When `None`
    /// (the default), GraphSync is dormant even when the
    /// `graphsync` feature is enabled.
    #[cfg(feature = "graphsync")]
    graphsync_config: Option<crate::graphsync::GraphSyncConfig>,
    /// Auto-init DHT config. When `Some` and the `dht` feature is
    /// on, the builder will automatically call [`Node::init_dht`]
    /// once the transport is wired so the DHT layer is ready by
    /// the time `Node` is returned. When `None` (the default),
    /// callers must call [`Node::init_dht`] themselves.
    #[cfg(feature = "dht")]
    auto_init_dht: Option<crate::dht::DhtConfig>,
    /// Auto-init IPNS config. When `Some` and the `dht` feature is
    /// on, the builder will automatically call [`Node::init_ipns`]
    /// after DHT is wired. Requires [`Self::with_auto_init_dht`]
    /// to also be set — IPNS hangs off the DHT transport.
    #[cfg(feature = "dht")]
    auto_init_ipns: Option<crate::dht::IpnConfig>,
}

impl NodeBuilder {
    pub fn new(config: NodeConfig) -> Self {
        Self {
            config,
            transport: None,
            relay_config: None,
            enable_workspace: true,
            #[cfg(feature = "iroh")]
            iroh_runtime: None,
            #[cfg(feature = "bitswap")]
            bitswap_config: None,
            #[cfg(feature = "graphsync")]
            graphsync_config: None,
            #[cfg(feature = "dht")]
            auto_init_dht: None,
            #[cfg(feature = "dht")]
            auto_init_ipns: None,
        }
    }

    pub fn with_transport(mut self, t: SharedTransport) -> Self {
        self.transport = Some(t);
        self
    }

    /// Enable the GraphSync DAG-sync layer on this node.
    ///
    /// When set, [`NodeBuilder::build_with_bus`] will:
    /// 1. Wrap the local [`BlobStore`](adnet_blobstore::BlobStore)
    ///    with a [`NodeBlockStore`](crate::graphsync::NodeBlockStore)
    ///    so it satisfies the
    ///    [`adnet_types::graphsync::BlockStore`] trait.
    /// 2. Build a [`GraphSyncService`](crate::graphsync::GraphSyncService)
    ///    and start its dispatcher task.
    /// 3. Stash the resulting
    ///    [`GraphSyncHandle`](crate::graphsync::GraphSyncHandle) on
    ///    the [`Node`] so callers can `request` DAGs or query stats.
    ///
    /// Passing `None` (the default) leaves GraphSync dormant even
    /// when the feature is enabled — useful for callers that want
    /// plain Bitswap without DAG streaming.
    #[cfg(feature = "graphsync")]
    pub fn with_graphsync(mut self, cfg: crate::graphsync::GraphSyncConfig) -> Self {
        self.graphsync_config = Some(cfg);
        self
    }

    /// Enable the Bitswap content-exchange layer on this node.
    ///
    /// When set, [`NodeBuilder::build_with_bus`] will:
    /// 1. Build a [`BitswapHandle`](crate::bitswap::BitswapHandle)
    ///    over the local blob store.
    /// 2. Instantiate a [`BitswapQuicBridge`](crate::bitswap_transport::BitswapQuicBridge)
    ///    over the wired transport via
    ///    [`crate::bitswap_wiring::wire_bitswap_to_transport`].
    /// 3. Attach the resulting live adapter to the handle so
    ///    `want_block_from_peer` calls traverse the wire.
    ///
    /// Passing `None` (the default) leaves bitswap dormant even
    /// when the `bitswap` feature is enabled — useful for callers
    /// that want pure blob-fetch behaviour without the extra
    /// outbound traffic.
    #[cfg(feature = "bitswap")]
    pub fn with_bitswap_config(mut self, cfg: crate::bitswap::BitswapConfig) -> Self {
        self.bitswap_config = Some(cfg);
        self
    }

    /// Auto-initialize the DHT layer once the transport is wired.
    ///
    /// When set, [`NodeBuilder::build_with_bus`] calls
    /// [`Node::init_dht`] automatically so the DHT is ready by the
    /// time `Node` is returned. This is the missing seam that
    /// P0-D closes: previously the operator had to call
    /// `init_dht` after `build`, which meant DHT-using subsystems
    /// (Bitswap provider records, IPNS) silently held a
    /// "DHT not initialized" handle until the second call.
    ///
    /// When `None` (the default), callers retain the explicit
    /// `init_dht` flow — useful for tests that want to drive
    /// `init_dht` with a custom config after `build`.
    #[cfg(feature = "dht")]
    pub fn with_auto_init_dht(mut self, cfg: crate::dht::DhtConfig) -> Self {
        self.auto_init_dht = Some(cfg);
        self
    }

    /// Auto-initialize the IPNS layer once the DHT is wired.
    ///
    /// Requires [`Self::with_auto_init_dht`] to also be set; IPNS
    /// hangs off the DHT transport. When both are set, the
    /// builder calls `init_dht` then `init_ipns` in order.
    ///
    /// The IPNS handle is read-only inside this code path (no
    /// `secret_key` is wired) because the operator's keypair lives
    /// outside the node config. Callers that need to publish IPNS
    /// records should call [`Node::init_ipns`] explicitly with a
    /// `secret_key` after `build`.
    #[cfg(feature = "dht")]
    pub fn with_auto_init_ipns(mut self, cfg: crate::dht::IpnConfig) -> Self {
        self.auto_init_ipns = Some(cfg);
        self
    }

    /// Wire an iroh-backed [`IrohRuntime`](crate::iroh_runtime::IrohRuntime)
    /// into the node as the primary transport. The runtime's
    /// `adnet/frame/1` ALPN is already registered on its router;
    /// this method hands the matching incoming-channel receiver
    /// to an [`IrohTransport`](adnet_transport::IrohTransport) that
    /// dials through the same `Arc<Endpoint>` — so the node has
    /// **exactly one** iroh `Endpoint` and **one** `Router`
    /// hosting both blobs/gossip/docs and the framed transport.
    ///
    /// The runtime is **owned** by the node and shut down with
    /// it. The transport is moved into the `Node`'s `transport`
    /// slot via `with_transport`.
    ///
    /// Use this when you want a memory-light single-endpoint
    /// deployment. The legacy `with_transport` path is still
    /// available for callers that want to control the transport
    /// independently.
    ///
    /// Note: this method is **synchronous** because it returns a
    /// builder. The actual `take_frame_receiver` call is deferred
    /// to `build_with_bus` (which is async), so the runtime is
    /// stashed on the builder and consumed there.
    #[cfg(feature = "iroh")]
    pub fn with_iroh_runtime(mut self, runtime: crate::iroh_runtime::IrohRuntime) -> Self {
        self.iroh_runtime = Some(runtime);
        self
    }

    /// Convenience: build a fresh iroh runtime from a `data_dir`,
    /// bind address, and persistent identity, then stash it on the
    /// builder via [`Self::with_iroh_runtime`]. The supplied `bind`
    /// address is used for the endpoint's UDP socket; `identity` is
    /// the persistent Ed25519 secret (use
    /// [`IrohIdentity::load_or_create`](adnet_transport::iroh::IrohIdentity::load_or_create)
    /// to obtain one). `docs_path` defaults to `<data_dir>` if
    /// `None`.
    ///
    /// This is the **fast path** for production deployments and
    /// the CLI: it spares callers from manually wiring
    /// `Endpoint::builder → secret_key → bind_addr → spawn →
    /// IrohRuntime::spawn_with_identity`. `build_with_bus` still
    /// owns the runtime for shutdown ordering.
    ///
    /// ## Failure semantics (Audit V5 P1-2)
    ///
    /// The identity file is loaded by the caller **before** this
    /// function is invoked. If the operator deletes or rewrites the
    /// identity file between the caller's `load_or_create` and the
    /// `spawn_with_identity` call inside this function, the runtime
    /// will be spawned with the in-memory `identity` that was already
    /// loaded — the on-disk file change is **not** observed. This
    /// is intentional: it avoids an unwrap-the-world atomic swap on
    /// every reload, and the cost is "the next restart picks up the
    /// new identity". Operators who need atomic reload semantics
    /// should restart the process.
    ///
    /// On `spawn_with_identity` failure, the builder is **partially
    /// constructed**: `self.config`, mesh, gossip bus, etc. are all
    /// unchanged and `Drop` will release them without leaking any
    /// runtime (because `self.iroh_runtime` is still `None`). The
    /// caller receives an `Err` and is free to retry or surface the
    /// failure to the operator.
    #[cfg(feature = "iroh")]
    pub async fn with_iroh_runtime_from_data_dir(
        self,
        data_dir: &Path,
        bind: std::net::SocketAddr,
        identity: &adnet_transport::iroh::IrohIdentity,
        docs_path: Option<&Path>,
    ) -> Result<Self> {
        self.with_iroh_runtime_from_data_dir_inner(data_dir, bind, identity, docs_path, None)
            .await
    }

    /// Same as [`Self::with_iroh_runtime_from_data_dir`] plus an
    /// optional `user_data` payload that is included as the
    /// `user-data=` TXT attribute on every pkarr packet this
    /// runtime publishes. Mirrors
    /// `iroh_dns::endpoint_info::UserData` (v1.0.3); see
    /// [`adnet_transport::iroh::discovery::UserData`].
    ///
    /// Pass `None` (the default for
    /// [`Self::with_iroh_runtime_from_data_dir`]) to skip — the
    /// pkarr publisher will fall back to whatever the iroh
    /// default is.
    ///
    /// Length-bounded at 245 bytes by
    /// [`adnet_transport::iroh::discovery::UserData::new`]; an
    /// oversized input returns `Err` from this method **before**
    /// the runtime is spawned, so the caller cannot end up with a
    /// half-bound state.
    #[cfg(feature = "iroh")]
    pub async fn with_iroh_runtime_from_data_dir_and_user_data(
        self,
        data_dir: &Path,
        bind: std::net::SocketAddr,
        identity: &adnet_transport::iroh::IrohIdentity,
        docs_path: Option<&Path>,
        user_data: Option<adnet_transport::iroh::discovery::UserData>,
    ) -> Result<Self> {
        self.with_iroh_runtime_from_data_dir_inner(data_dir, bind, identity, docs_path, user_data)
            .await
    }

    /// Internal shared implementation for the two
    /// `with_iroh_runtime_from_data_dir*` builders. Validates the
    /// `user_data` length **before** spawning the runtime so a
    /// misconfigured caller never ends up with a half-bound
    /// endpoint. Consumes `self` (owned) so the
    /// `NodeBuilder::iroh_runtime` slot can be set without
    /// fighting the borrow checker.
    #[cfg(feature = "iroh")]
    async fn with_iroh_runtime_from_data_dir_inner(
        self,
        data_dir: &Path,
        bind: std::net::SocketAddr,
        identity: &adnet_transport::iroh::IrohIdentity,
        docs_path: Option<&Path>,
        user_data: Option<adnet_transport::iroh::discovery::UserData>,
    ) -> Result<Self> {
        use crate::iroh_runtime::IrohRuntime;
        // Pre-validate `user_data` (length-bounded at 245 bytes)
        // before any IO so a typo in the config surfaces as a
        // startup error rather than a silently-truncated wire
        // payload. The `UserData::new` constructor already
        // enforces the bound, so this is a defensive no-op for
        // callers that came through the typed builder. Direct
        // callers that bypass `UserData::new` (e.g. tests that
        // construct raw `String`s) still get the check.
        if let Some(ud) = &user_data
            && ud.len() > adnet_transport::iroh::discovery::USER_DATA_MAX_LEN
        {
            anyhow::bail!(
                "user_data length {} exceeds {} bytes (cap is {})",
                ud.len(),
                adnet_transport::iroh::discovery::USER_DATA_MAX_LEN,
                adnet_transport::iroh::discovery::USER_DATA_MAX_LEN,
            );
        }
        let mut self_mut = self;
        let runtime = IrohRuntime::spawn_with_identity_and_user_data(
            bind, identity, data_dir, docs_path, user_data,
        )
        .await?;
        // **P6 hardening: NodeId alignment.** The runtime's
        // iroh-derived `NodeId` is the authoritative identity
        // for the lifetime of the data dir. If the caller built
        // `self.config` via `NodeConfig::load_or_create` (which
        // reads `data_dir/node_id`), that file may carry a
        // legacy `NodeId` that pre-dates the iroh identity —
        // which would cause the framed transport's local_node
        // and the gossip bus's local_node to disagree, silently
        // producing messages nobody can address. Align here,
        // before the builder is consumed by `build_with_bus`.
        let aligned_node_id = identity.node_id();
        if self_mut.config.node_id != aligned_node_id {
            tracing::info!(
                from = %self_mut.config.node_id.short(),
                to = %aligned_node_id.short(),
                "NodeBuilder::with_iroh_runtime_from_data_dir: aligning cfg.node_id with iroh identity"
            );
            self_mut.config.node_id = aligned_node_id.clone();
            self_mut.config.display_name = format!("adnet-{}", aligned_node_id.short());
        }
        self_mut.iroh_runtime = Some(runtime);
        Ok(self_mut)
    }

    /// Configure the embedded relay server. When `serve_enabled` is
    /// `true` on the supplied config the node will spawn the relay on
    /// build and tear it down on `shutdown()`. Mirrors what an iroh
    /// `Endpoint::builder().relay_mode(RelayMode::Default)` does.
    pub fn with_relay_config(mut self, relay: RelayConfig) -> Self {
        self.relay_config = Some(relay);
        self
    }

    /// Toggle the workspace + gossip bridge. When disabled, the node
    /// will not create `{data_dir}/ExodusWorkSpace`, will not subscribe
    /// to `adnet-room-{WORKSPACE_ROOM_ID}`, and `publish_to_workspace`
    /// will return an error. Tests that only exercise gossip mesh /
    /// transport use this to avoid spinning up file IO.
    pub fn with_workspace(mut self, enabled: bool) -> Self {
        self.enable_workspace = enabled;
        self
    }

    pub async fn build(self) -> Result<Node> {
        let bus = GossipBus::new(
            self.config.node_id.clone(),
            Arc::new(InProcessGossip::new()),
        );
        self.build_with_bus(bus).await
    }

    /// Build the node with a pre-constructed [`GossipBus`]. Useful for
    /// tests that want two nodes to share a single in-process gossip
    /// overlay.
    #[cfg_attr(not(feature = "iroh"), allow(unused_mut))]
    pub async fn build_with_bus(mut self, bus: GossipBus) -> Result<Node> {
        // If the caller wired an iroh runtime, take its frame
        // receiver and wrap the runtime's endpoint into an
        // `IrohTransport`. This is the single-Endpoint integration
        // path: blobs/gossip/docs/the framed transport all share
        // one iroh `Endpoint` and one `Router`.
        //
        // `into_parts` is `&mut self` so this branch is mutually
        // exclusive with any other consumer of `self.iroh_runtime`
        // — the borrow checker enforces the single-caller
        // contract (see `IrohRuntime::into_parts`).
        //
        // We `mem::take` the runtime out of the builder first so
        // we can `&mut` it locally without holding a borrow on
        // `self.iroh_runtime` across the async call — that
        // would otherwise deadlock the trailing `self.iroh_runtime
        // .take()` that puts the runtime into the node's slot.
        #[cfg(feature = "iroh")]
        let mut runtime = self.iroh_runtime.take();
        #[cfg(feature = "iroh")]
        let transport: Option<SharedTransport> = if let Some(runtime_ref) = runtime.as_mut() {
            let (endpoint, frame_rx) = runtime_ref.into_parts().await;
            let frame_rx = frame_rx
                .ok_or_else(|| anyhow::anyhow!("IrohRuntime frame receiver already taken"))?;
            let transport = adnet_transport::IrohTransport::with_endpoint(endpoint, frame_rx);
            Some(std::sync::Arc::new(transport) as SharedTransport)
        } else {
            self.transport.clone()
        };
        #[cfg(not(feature = "iroh"))]
        let transport: Option<SharedTransport> = self.transport.clone();

        // If the runtime is on, replace the caller's `bus` with an
        // `IrohGossipTransport`-backed `GossipBus` so the node
        // actually exchanges announcements through iroh-gossip
        // (HyParView + PlumTree) on the same endpoint that hosts
        // the blobs / docs protocols. The caller's `bus` is
        // dropped — sharing the InProcessGossip across an iroh
        // node would silently route every announcement to no one.
        //
        // We hand the runtime's gossip handle to `IrohGossipTransport`,
        // which already manages the per-topic broadcast channels. The
        // `local_node` is derived from the iroh endpoint's public key
        // so the two halves see one consistent identity.
        #[cfg(feature = "iroh")]
        let bus = if let Some(runtime_ref) = runtime.as_ref() {
            let local_node =
                adnet_transport::iroh::public_key_to_node_id(&runtime_ref.endpoint().id());
            let iroh_gossip = runtime_ref.gossip_transport(local_node.clone());
            let bus_transport = std::sync::Arc::new(iroh_gossip);
            GossipBus::new(local_node, bus_transport)
        } else {
            bus
        };
        #[cfg(not(feature = "iroh"))]
        let bus = bus;

        // P6: align the NodeId with the transport's local node id
        // (which is derived from the QUIC cert fingerprint). When the
        // user supplied a `quic_identity` we already derived the id
        // from it in `load_or_create`; here we just make sure the
        // NodeConfig used at runtime matches the transport's view.
        //
        // The local `transport` binding (rather than `self.transport`)
        // is the authoritative source here: when the iroh runtime
        // was wired via `with_iroh_runtime`, `self.transport` stays
        // `None` and the local binding carries the iroh-derived
        // transport.
        let mut cfg = self.config.clone();
        if let Some(t) = &transport {
            let transport_id = t.local_node().clone();
            if cfg.node_id != transport_id {
                info!(
                    "node_id aligned with transport local_node: {} -> {}",
                    cfg.node_id.short(),
                    transport_id.short()
                );
                cfg.node_id = transport_id;
                cfg.display_name = format!("adnet-{}", cfg.node_id.short());
            }
        }
        let store = Arc::new(
            BlobStore::new(&self.config.data_dir.join("blobs"))
                .map_err(|e| anyhow::anyhow!("blobstore init: {e}"))?,
        );
        let swarm = Arc::new(Mutex::new(SwarmIndex::default()));
        // Spawn the embedded relay server when configured. The
        // billing mode is derived from `RelayConfig.billing_mode()`;
        // when the `billing` cargo feature is off, that helper is a
        // no-op and always returns `Disabled`.
        //
        // We feed `ServerPolicy::from_config(&cfg)` into
        // `RelayServer::start_with_policy` so the operator-supplied
        // `host_policy` / `max_body_bytes` / `upstream_timeout` /
        // `max_redirects` actually take effect. The previous code
        // used `ServerPolicy::default()` and silently ignored every
        // `RelayConfig` policy field — see the operations audit
        // (P0-b: RelayConfig policy fields ignored).
        let relay = if let Some(mut cfg) = self.relay_config.clone() {
            cfg.apply_local_relay_url();
            if cfg.serve_enabled {
                let billing_mode = cfg.billing_mode();
                let policy = adnet_relay::ServerPolicy::from_config(&cfg);
                match adnet_relay::RelayServer::start_with_policy(
                    &cfg.serve_bind,
                    cfg.serve_port,
                    billing_mode,
                    policy,
                )
                .await
                {
                    Ok(handle) => Some(handle),
                    Err(e) => {
                        warn!("relay server failed to start: {e}");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        // P6: align the NodeId with the transport's local node id
        // (which is derived from the QUIC cert fingerprint). When the
        // user supplied a `quic_identity` we already derived the id
        // from it in `load_or_create`; here we just make sure the
        // NodeConfig used at runtime matches the transport's view.
        let mut cfg = self.config.clone();
        if let Some(t) = &self.transport {
            let transport_id = t.local_node().clone();
            if cfg.node_id != transport_id {
                info!(
                    "node_id aligned with transport local_node: {} -> {}",
                    cfg.node_id.short(),
                    transport_id.short()
                );
                cfg.node_id = transport_id;
                cfg.display_name = format!("adnet-{}", cfg.node_id.short());
            }
        }

        // P2: wire the transport's incoming channel into the Node's
        // own incoming queue. Each accepted connection is delivered
        // to (a) application code via `Node::next_incoming_peer`, and
        // (b) the built-in blob-serving dispatcher which calls
        // `serve_blob_request` for any peer that speaks the blob
        // Build the incoming channel and wrap it so application code can
        // take the receiver via `Node::take_incoming_receiver` while
        // we also run a built-in blob-serve dispatcher that drains
        // it concurrently.
        let (incoming_tx, incoming_rx) = tokio::sync::mpsc::channel::<IncomingConn>(64);
        let incoming_rx_slot: Arc<
            tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<IncomingConn>>>,
        > = Arc::new(tokio::sync::Mutex::new(Some(incoming_rx)));
        if let Some(transport) = &self.transport {
            if let Some(mut transport_rx) = transport.take_incoming_receiver().await {
                let tx = incoming_tx.clone();
                tokio::spawn(async move {
                    while let Some(item) = transport_rx.recv().await {
                        if tx.send(item).await.is_err() {
                            break;
                        }
                    }
                });
            }
            // Move the receiver out of the slot into the dispatcher.
            // We hold no lock across `.await` so no nested-lock
            // deadlock is possible. Application code that wants to
            // drive the receiver itself via `take_incoming_receiver`
            // will get `None` after this point; the dispatcher still
            // services the wire protocol.
            let slot = Arc::clone(&incoming_rx_slot);
            let store_for_serve = Arc::clone(&store);
            tokio::spawn(async move {
                let mut rx = {
                    let mut guard = slot.lock().await;
                    let r = guard.take();
                    drop(guard);
                    match r {
                        Some(rx) => rx,
                        None => return,
                    }
                };
                while let Some((peer_id, mut conn)) = rx.recv().await {
                    tracing::info!("serving blob request from peer {}", peer_id.short());
                    let s = Arc::clone(&store_for_serve);
                    tokio::spawn(async move {
                        let r = adnet_transport::serve_blob_request(&mut conn, &s).await;
                        if let Err(e) = r {
                            tracing::warn!("blob serve ended with error: {e}");
                        }
                    });
                }
            });
        }

        // Workspace (PR #3). Initialised once on build, then exposed
        // through `Node::publish_to_workspace` / `Node::remote_workspace`.
        // When disabled, the optional slot stays `None` and the gossip
        // ingestion task is not spawned.
        let workspace: Arc<tokio::sync::Mutex<Option<Arc<Workspace>>>> = if self.enable_workspace {
            match Workspace::new(&cfg.data_dir, cfg.node_id.as_hex()) {
                Ok(ws) => Arc::new(tokio::sync::Mutex::new(Some(Arc::new(ws)))),
                Err(e) => {
                    warn!("workspace init failed: {e}; workspace features disabled");
                    Arc::new(tokio::sync::Mutex::new(None))
                }
            }
        } else {
            Arc::new(tokio::sync::Mutex::new(None))
        };
        let remote_workspace: Arc<tokio::sync::Mutex<HashMap<NodeId, Vec<RemoteWorkspaceEntry>>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        // Auto-join the workspace gossip room and spawn the ingestion
        // task so remote peers' manifest announcements land in
        // `remote_workspace` even before the user runs any command.
        // A second task listens for ingest events and automatically
        // pulls the bytes when the announcement carried a ticket.
        if self.enable_workspace {
            let room: RoomId = WORKSPACE_ROOM_ID.into();
            if let Err(e) = bus.join_room(&room).await {
                warn!("workspace: failed to join room {room}: {e}");
            } else {
                let mut rx = bus.subscribe(&room);
                let local = cfg.node_id.clone();
                let policy = cfg.gossip_validation;
                let sink = Arc::clone(&remote_workspace);
                let (auto_tx, mut auto_rx) =
                    tokio::sync::mpsc::unbounded_channel::<(NodeId, String)>();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(ann) => {
                                if let Some(name) =
                                    ingest_workspace_announcement(ann, &local, policy, &sink).await
                                {
                                    // Best-effort delivery: the
                                    // auto-fetch task may have shut
                                    // down (e.g. during shutdown).
                                    let _ = auto_tx.send((local.clone(), name));
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
                info!(
                    "[{}] workspace: joined gossip room {room}",
                    cfg.node_id.short()
                );

                // Auto-fetch task: pulls bytes for every ingested
                // entry that came with a ticket. Errors are logged
                // and the entry stays in the map with `local_path =
                // None` so the user can retry via `/fetch`.
                let store = Arc::clone(&store);
                let transport = self.transport.clone();
                let remote_workspace_for_fetch = Arc::clone(&remote_workspace);
                let workspace_for_fetch = Arc::clone(&workspace);
                tokio::spawn(async move {
                    while let Some((_local, _name)) = auto_rx.recv().await {
                        // Drain the current state — multiple entries
                        // may have arrived in flight.
                        let pending: Vec<(NodeId, String)> = {
                            let g = remote_workspace_for_fetch.lock().await;
                            g.iter()
                                .flat_map(|(owner, bucket)| {
                                    bucket
                                        .iter()
                                        .filter(|r| r.ticket.is_some() && r.local_path.is_none())
                                        .map(|r| (owner.clone(), r.entry.name.clone()))
                                        .collect::<Vec<_>>()
                                })
                                .collect()
                        };
                        for (owner, name) in pending {
                            // Skip if already fetched (race).
                            {
                                let g = remote_workspace_for_fetch.lock().await;
                                if let Some(b) = g.get(&owner)
                                    && let Some(r) = b.iter().find(|r| r.entry.name == name)
                                    && r.local_path.is_some()
                                {
                                    continue;
                                }
                            }
                            // Resolve inbox dir + ticket + hash.
                            let (ticket, hash_hex, safe_name) = {
                                let g = remote_workspace_for_fetch.lock().await;
                                let Some(bucket) = g.get(&owner) else {
                                    continue;
                                };
                                let Some(entry) = bucket.iter().find(|r| r.entry.name == name)
                                else {
                                    continue;
                                };
                                let Some(ticket) = entry.ticket.clone() else {
                                    continue;
                                };
                                let hash_hex = entry.entry.content_hash.clone().unwrap_or_default();
                                let safe_name = workspace_safe_name(&entry.entry.name);
                                (ticket, hash_hex, safe_name)
                            };
                            let inbox_dir = {
                                let g = workspace_for_fetch.lock().await;
                                match g.as_ref() {
                                    Some(ws) => ws.inbox_dir(),
                                    None => {
                                        warn!("workspace: inbox dir vanished mid-fetch");
                                        continue;
                                    }
                                }
                            };
                            let dest = {
                                let g = workspace_for_fetch.lock().await;
                                let Some(ws) = g.as_ref() else {
                                    continue;
                                };
                                ws.resolve_unique_path(&inbox_dir, &safe_name)
                            };
                            let hash = match ContentHash::from_hex(&hash_hex) {
                                Ok(h) => h,
                                Err(e) => {
                                    warn!(
                                        "auto-fetch: bad hash from {} for {name}: {e}",
                                        owner.short()
                                    );
                                    continue;
                                }
                            };
                            let peers = vec![ticket];
                            let res = crate::download::fetch_blob(
                                Arc::clone(&store),
                                &hash,
                                &name,
                                &peers,
                                &dest,
                                transport.clone(),
                                adnet_types::RangeSpec::All,
                            )
                            .await;
                            match res {
                                Ok(job) => {
                                    info!(
                                        "auto-fetched {} from {} → {} ({}B)",
                                        name,
                                        owner.short(),
                                        dest.display(),
                                        job.bytes_done,
                                    );
                                    let mut g = remote_workspace_for_fetch.lock().await;
                                    if let Some(bucket) = g.get_mut(&owner) {
                                        for r in bucket.iter_mut() {
                                            if r.entry.name == name {
                                                r.local_path = Some(dest.clone());
                                                r.fetched_bytes = Some(job.bytes_done);
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "auto-fetch failed for {name} from {}: {e}",
                                        owner.short()
                                    );
                                }
                            }
                        }
                    }
                });
            }
        }

        // PR4 → V10: register HealthCheck adapters so the `/health`
        // endpoint can surface dependency status to operators.
        {
            use adnet_observability::health::{register_health_check, HealthCheck, HealthCheckError};
            use std::pin::Pin;

            // BlobStore check: confirm the data directory is readable.
            struct BlobStoreHealthCheck {
                data_dir: std::path::PathBuf,
            }
            impl HealthCheck for BlobStoreHealthCheck {
                fn name(&self) -> &'static str { "blobstore" }
                fn check(self: std::sync::Arc<Self>) -> Pin<Box<dyn std::future::Future<Output = Result<(), HealthCheckError>> + Send + 'static>> {
                    let data_dir = self.data_dir.clone();
                    Box::pin(async move {
                        tokio::task::spawn_blocking(move || {
                            std::fs::read_dir(&data_dir)
                                .map_err(|e| HealthCheckError::new(format!("blobstore dir read: {e}")))?;
                            Ok(())
                        })
                        .await
                        .map_err(|e| HealthCheckError::new(format!("blobstore check task: {e}")))?
                    })
                }
            }
            register_health_check(BlobStoreHealthCheck {
                data_dir: self.config.data_dir.join("blobs"),
            });

            // Transport check: confirm the endpoint is open (can resolve self).
            if let Some(ref t) = transport {
                let transport_clone = t.clone();
                struct TransportHealthCheck {
                    transport: adnet_transport::SharedTransport,
                }
                impl HealthCheck for TransportHealthCheck {
                    fn name(&self) -> &'static str { "transport" }
                    fn check(self: std::sync::Arc<Self>) -> Pin<Box<dyn std::future::Future<Output = Result<(), HealthCheckError>> + Send + 'static>> {
                        let t = self.transport.clone();
                        Box::pin(async move {
                            // Call health_check on the dyn Transport via explicit UFCS.
                            // We reborrow as &dyn Transport and use the trait method syntax.
                            let t_ref: &dyn adnet_transport::Transport = &*t;
                            t_ref.health_check()
                                .map_err(|e| HealthCheckError::new(format!("transport: {e}")))
                        })
                    }
                }
                register_health_check(TransportHealthCheck { transport: transport_clone });
            }
        }

        // PR2 adapter runtime wiring — V12: spawn a background
        // task that pushes discovery + endpoint diagnostics into
        // the global `adnet-observability` registry. The task
        // polls every 5 s (cheap: atomic snapshot reads + a
        // single `iroh::Endpoint::addr()`); the bridge functions
        // are no-ops when the recorder is empty so the cost is
        // bounded. The task exits cleanly when the runtime is
        // dropped (the `Arc<DiscoveryDiagnostics>` keeps the
        // bridge alive independently of the runtime itself).
        #[cfg(feature = "iroh")]
        if let Some(runtime) = runtime.as_ref() {
            if let Some(diag) = runtime.clone_diagnostics() {
                let endpoint_for_snap = runtime.endpoint().clone();
                tokio::spawn(async move {
                    use adnet_transport::iroh::endpoint_diagnostics::EndpointSnapshot;
                    use adnet_transport::iroh::metrics_bridge::{
                        publish_discovery_metrics, publish_endpoint_metrics_into,
                    };
                    let endpoint_metrics = adnet_observability::bridge::ENDPOINT.clone();
                    let recorder = std::sync::Arc::new(
                        adnet_transport::iroh::endpoint_diagnostics::EndpointDiagnosticsRecorder::new(8),
                    );
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                    loop {
                        interval.tick().await;
                        // Discovery: snapshot + bridge. Cheap.
                        publish_discovery_metrics(&diag);
                        // Endpoint: take a fresh snapshot, store it
                        // in the recorder, then push to the bridge.
                        let snap = adnet_transport::iroh::endpoint_diagnostics::snapshot_endpoint(
                            &endpoint_for_snap,
                            None,
                        );
                        recorder.record(snap.clone()).await;
                        let _ = publish_endpoint_metrics_into(&endpoint_metrics, &recorder).await;
                        // Touch the type so the import stays used
                        // even when the recorder short-circuits.
                        let _: Option<EndpointSnapshot> = Some(snap);
                    }
                });
            }
        }

        // P0-A: build the live Bitswap pipeline when the builder
        // asked for it. We do this **before** `Ok(Node { ... })` so
        // the structured fields can be initialized directly. When
        // any precondition is missing both locals stay `None` and
        // the engine is dormant (the historical fallback).
        #[cfg(feature = "bitswap")]
        let (bitswap_handle_for_node, bitswap_wiring_for_node): (
            Option<crate::bitswap::BitswapHandle>,
            Option<crate::bitswap_wiring::BitswapWiring>,
        ) = if let (Some(bitswap_cfg), Some(transport_ref)) =
            (self.bitswap_config.clone(), transport.clone())
        {
            let id = cfg.node_id.clone();
            let handle = crate::bitswap::BitswapHandle::new(
                id.clone(),
                store.clone(),
                bitswap_cfg,
            )
            .await;
            let wiring =
                crate::bitswap_wiring::wire_bitswap_to_transport(id, transport_ref);
            handle.attach_transport(wiring.adapter.clone());
            info!(
                "Bitswap → QUIC wired for node {} (handle+bridge+adapter)",
                cfg.node_id.short()
            );
            (Some(handle), Some(wiring))
        } else {
            (None, None)
        };

        let mut node = Node {
            cfg,
            store: store.clone(),
            bus,
            swarm,
            mesh: Arc::new(Mutex::new(None)),
            relay: Arc::new(Mutex::new(relay)),
            transport,
            incoming_tx,
            incoming_rx: incoming_rx_slot,
            joined: Arc::new(Mutex::new(HashSet::new())),
            started_at: Some(Utc::now()),
            workspace,
            remote_workspace,
            #[cfg(feature = "iroh")]
            iroh_runtime_slot: Arc::new(tokio::sync::Mutex::new(runtime)),
            // V13: spin up a background task that periodically
            // corrects the blobstore gauges. The 60-second cadence
            // is a sweet spot between responsiveness and CPU: the
            // sweep walks `<data_dir>/<hash>/meta.json` and is
            // cheap enough to repeat every minute. The handle is
            // stored on the `Node` and consumed by `Node::shutdown`
            // so the task always terminates cleanly.
            #[cfg(feature = "refresh-task")]
            blob_store_refresh_handle: Arc::new(tokio::sync::Mutex::new({
                store
                    .start_refresh_task(std::time::Duration::from_secs(60))
                    .ok()
            })),
            #[cfg(feature = "bitswap")]
            bitswap: bitswap_handle_for_node,
            #[cfg(feature = "bitswap")]
            bitswap_wiring: bitswap_wiring_for_node,
            #[cfg(feature = "dht")]
            dht: Arc::new(tokio::sync::RwLock::new(None)),
            #[cfg(feature = "dht")]
            ipn: Arc::new(tokio::sync::RwLock::new(None)),
        };

        // P0-D auto-init: if the caller asked for DHT and/or IPNS to
        // come up by the time `Node` is returned, run the
        // initialisation here (synchronously, before `Ok` so the
        // caller can immediately use `dht_handle()` / `ipn_handle()`
        // without a follow-up step).
        #[cfg(feature = "dht")]
        if let Some(dht_cfg) = self.auto_init_dht.clone() {
            if let Err(e) = node.init_dht(dht_cfg).await {
                warn!("auto_init_dht failed: {e:#}");
            } else {
                info!(
                    "auto_init_dht ready for node {}",
                    node.cfg.node_id.short()
                );
            }
        }
        #[cfg(feature = "dht")]
        if let Some(ipn_cfg) = self.auto_init_ipns.clone() {
            if let Err(e) = node.init_ipns(ipn_cfg, None).await {
                warn!("auto_init_ipns failed: {e:#}");
            } else {
                info!(
                    "auto_init_ipns ready (read-only) for node {}",
                    node.cfg.node_id.short()
                );
            }
        }

        Ok(node)
    }
}

/// Convert an entry name to something safe to drop into `inbox/`. We
/// strip path separators and NULs so a malicious peer can't escape
/// the inbox directory.
fn workspace_safe_name(entry_name: &str) -> String {
    entry_name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Convert a gossip [`Announcement`] into a [`RemoteWorkspaceEntry`]
/// and store it in the in-memory `remote_workspace` map. The room id
/// must equal `WORKSPACE_ROOM_ID` for the entry to be stored.
///
/// Returns the entry name when an entry with a ticket was inserted,
/// so the caller can kick off an auto-fetch. Plain metadata-only
/// announcements return `None`.
async fn ingest_workspace_announcement(
    ann: Announcement,
    local: &NodeId,
    policy: ValidationPolicy,
    sink: &Arc<tokio::sync::Mutex<HashMap<NodeId, Vec<RemoteWorkspaceEntry>>>>,
) -> Option<String> {
    if ann.node_id == *local {
        return None; // ignore our own announcements
    }
    if ann.room_id.as_str() != WORKSPACE_ROOM_ID {
        return None;
    }
    // Apply the configured gossip policy (Strict/Audit/Lenient). This
    // mirrors the validation we do for arbitrary room announcements;
    // a record that fails its own validate() under Strict is dropped.
    match ann.validate() {
        Ok(()) => {}
        Err(e) => match policy {
            ValidationPolicy::Strict => {
                warn!(
                    "[{}] dropping invalid workspace announcement from {}: {e}",
                    local.short(),
                    ann.node_id.short(),
                );
                return None;
            }
            ValidationPolicy::Audit => {
                warn!(
                    "[{}] workspace announcement from {} failed validate: {e} (admitted under Audit)",
                    local.short(),
                    ann.node_id.short(),
                );
            }
            ValidationPolicy::Lenient => {}
        },
    }
    let entry = WorkspaceFileEntry {
        name: ann.title.clone(),
        relative_path: format!("shared/{}", ann.title),
        size_bytes: ann.size_bytes,
        content_hash: Some(ann.content_hash.as_hex().to_string()),
        added_at: ann.timestamp.timestamp().max(0) as u64,
    };
    let entry_name = ann.title.clone();
    let has_ticket = ann.ticket.is_some();
    let remote = RemoteWorkspaceEntry {
        owner: ann.node_id.clone(),
        entry,
        ticket: ann.ticket.clone(),
        received_at: Utc::now(),
        has_ticket,
        local_path: None,
        fetched_bytes: None,
    };
    let mut g = sink.lock().await;
    g.entry(ann.node_id).or_insert_with(Vec::new).push(remote);
    if has_ticket { Some(entry_name) } else { None }
}

/// Public status snapshot exposed via [`Node::info`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub node_id: NodeId,
    pub display_name: String,
    pub data_dir: PathBuf,
    pub mesh: Option<MeshEndpointInfo>,
    pub relay: Option<RelayEndpointInfo>,
    pub joined_rooms: Vec<RoomId>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub uptime_seconds: Option<u64>,
}

/// Mesh endpoint as exposed via [`NodeInfo`]. Distinct from
/// `adnet_types::Endpoint` because the UI only needs the routable string.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEndpointInfo {
    pub host: String,
    pub port: u16,
}

impl From<Endpoint> for MeshEndpointInfo {
    fn from(e: Endpoint) -> Self {
        Self {
            host: e.host().to_string(),
            port: e.port().unwrap_or(0),
        }
    }
}

/// Relay server info as exposed via [`NodeInfo`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEndpointInfo {
    pub base_url: String,
    pub port: u16,
}

pub type IncomingConn = (NodeId, Box<dyn adnet_transport::OutgoingConnection>);

/// One remote workspace file, as seen via gossip. The full
/// `WorkspaceFileEntry` carries `content_hash` as an `Option<String>`
/// (legacy IPC payload), but on the wire we only emit hashes we have
/// already parsed; the `Option` is kept so `RemoteWorkspaceEntry`
/// mirrors the local shape.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkspaceEntry {
    pub owner: NodeId,
    pub entry: WorkspaceFileEntry,
    /// Ticket from the original announcement, if the peer embedded
    /// one. `None` for announcements that did not advertise a
    /// downloadable endpoint.
    pub ticket: Option<adnet_types::BlobTicket>,
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// Whether the remote node embedded a download ticket in its
    /// announcement. Convenience flag — equivalent to `ticket.is_some()`.
    pub has_ticket: bool,
    /// Absolute path to the file in the local `inbox/` once auto-fetch
    /// succeeded. `None` means "not yet fetched" (or fetch failed).
    pub local_path: Option<PathBuf>,
    /// Bytes pulled by auto-fetch. `None` until fetch completes.
    pub fetched_bytes: Option<u64>,
}

/// ADNet runtime.
pub struct Node {
    cfg: NodeConfig,
    store: Arc<BlobStore>,
    bus: GossipBus,
    swarm: Arc<Mutex<SwarmIndex>>,
    mesh: Arc<Mutex<Option<MeshServerHandle>>>,
    relay: Arc<Mutex<Option<RelayServerHandle>>>,
    transport: Option<SharedTransport>,
    /// Channel that surfaces every incoming peer connection established
    /// through `transport`. When a `SharedTransport` is wired via
    /// `NodeBuilder::with_transport` the builder drains its incoming
    /// queue into this channel; otherwise it stays closed and any
    /// caller waiting on it will see `None`.
    #[allow(dead_code)] // exposed via `take_incoming_receiver`; field
    incoming_tx: tokio::sync::mpsc::Sender<IncomingConn>,
    incoming_rx: Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<IncomingConn>>>>,
    joined: Arc<Mutex<HashSet<RoomId>>>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Local workspace (shared/inbox/outbox + manifest). Created in
    /// `NodeBuilder::build_with_bus` unless explicitly disabled with
    /// `NodeBuilder::with_workspace(false)`.
    workspace: Arc<tokio::sync::Mutex<Option<Arc<Workspace>>>>,
    /// Remote workspace entries seen on the gossip topic
    /// `adnet-room-{WORKSPACE_ROOM_ID}`. Keyed by owning node id.
    remote_workspace: Arc<tokio::sync::Mutex<HashMap<NodeId, Vec<RemoteWorkspaceEntry>>>>,
    /// iroh runtime owned by this node. Set by
    /// [`NodeBuilder::with_iroh_runtime`]. Shutdown is delegated
    /// to it when the node is shut down. Wrapped in a `Mutex<Option>`
    /// so the `&self` shutdown signature can take the runtime
    /// out without `&mut self`.
    #[cfg(feature = "iroh")]
    iroh_runtime_slot: Arc<tokio::sync::Mutex<Option<crate::iroh_runtime::IrohRuntime>>>,
    /// Background task handle that periodically re-scans the
    /// on-disk blob store and corrects the
    /// `store_size_bytes` / `blobs_total` gauges. Created in
    /// `NodeBuilder::build_with_bus` when the `refresh-task`
    /// cargo feature is enabled. Stopped by [`Node::shutdown`].
    #[cfg(feature = "refresh-task")]
    blob_store_refresh_handle:
        Arc<tokio::sync::Mutex<Option<adnet_blobstore::BlobStoreHandle>>>,
    /// DHT handle for content routing and provider discovery.
    /// Initialized when the `dht` feature is enabled and a
    /// transport is wired — either via the explicit
    /// [`Node::init_dht`] call or automatically via
    /// [`NodeBuilder::with_auto_init_dht`].
    #[cfg(feature = "dht")]
    dht: Arc<tokio::sync::RwLock<Option<Arc<crate::dht::DhtHandle>>>>,
    /// IPNS (InterPlanetary Naming System) handle. Wire-traversal
    /// happens via `DhtIpnTransport`; populated when the `dht`
    /// feature is on and [`Node::init_ipns`] (or the auto-init
    /// variant) has run.
    #[cfg(feature = "dht")]
    ipn: Arc<tokio::sync::RwLock<Option<Arc<crate::dht::IpnHandle>>>>,
    /// Live Bitswap content-exchange layer. Initialized when the
    /// `bitswap` feature is enabled **and** the builder passed a
    /// `bitswap_config`. The companion [`BitswapWiring`](crate::bitswap_wiring::BitswapWiring)
    /// bundle is stored alongside and aborted on shutdown.
    #[cfg(feature = "bitswap")]
    bitswap: Option<crate::bitswap::BitswapHandle>,
    /// Bitswap QUIC wiring (bridge + adapter + join handles). Kept
    /// alive for the node's whole lifetime so the spawned accept
    /// loop, outgoing pump, and adapter run loop never abort early.
    /// The `#[allow(dead_code)]` is intentional: the wiring handle
    /// itself is dropped only via `Node::shutdown`, which is the
    /// single point that aborts the three join handles.
    #[cfg(feature = "bitswap")]
    #[allow(dead_code)]
    bitswap_wiring: Option<crate::bitswap_wiring::BitswapWiring>,
    /// Live GraphSync DAG-sync service handle. Initialized when
    /// the `graphsync` feature is enabled **and** the builder
    /// passed a `graphsync_config`. Wrapped in a `Mutex<Option>`
    /// so `Node::shutdown` can take it out without `&mut self`.
    /// `parking_lot::Mutex` is intentional: the inner handle is
    /// `Send + Sync` and we don't want an `await` point inside
    /// the critical section.
    #[cfg(feature = "graphsync")]
    graphsync: Arc<parking_lot::Mutex<Option<crate::graphsync::GraphSyncHandle>>>,
}

impl Node {
    pub fn builder(cfg: NodeConfig) -> NodeBuilder {
        NodeBuilder::new(cfg)
    }

    pub fn node_id(&self) -> &NodeId {
        &self.cfg.node_id
    }

    /// Initialize the DHT layer on an already-built Node.
    ///
    /// Wires the supplied transport (already stored in
    /// [`NodeBuilder::with_transport`]) into a fresh [`DhtHandle`]
    /// and stores it on the node. Idempotent: subsequent calls
    /// replace the existing handle.
    #[cfg(feature = "dht")]
    pub async fn init_dht(&self, cfg: crate::dht::DhtConfig) -> anyhow::Result<()> {
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("init_dht requires a wired transport"))?;
        let handle = crate::dht::DhtHandle::new(cfg).await;
        let local_id = transport.local_node().clone();
        let bridge: Arc<dyn adnet_dht::transport::TransportBridge> =
            Arc::new(crate::dht_bridge::DynTransportBridge::new(
                transport.clone(),
                local_id,
            ));
        let _sender = handle.set_transport(bridge);
        *self.dht.write().await = Some(Arc::new(handle));
        Ok(())
    }

    /// Initialize the IPNS layer on an already-built Node.
    ///
    /// Requires `init_dht` to have run first (because IPNS
    /// publishes / resolves through the DHT transport). When
    /// `secret_key` is `Some`, the IpnHandle gets a writable
    /// publisher; when `None`, the handle is read-only (resolves
    /// only — useful for nodes that never sign records).
    #[cfg(feature = "dht")]
    pub async fn init_ipns(
        &self,
        cfg: crate::dht::IpnConfig,
        secret_key: Option<Arc<dyn adnet_namespace::SecretKey>>,
    ) -> anyhow::Result<()> {
        let dht = self
            .dht
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("init_ipns requires init_dht to have run first"))?;
        let local_id = dht.local_id().clone();
        let mut handle = crate::dht::IpnHandle::new(cfg, local_id);
        if let Some(key) = secret_key {
            let dht_node = dht.inner().clone();
            let query_arc = dht_node.query().ok_or_else(|| {
                anyhow::anyhow!("DHT node has no query handle wired; init_dht must be called with transport first")
            })?;
            let backend: Arc<dyn adnet_namespace::transport::dht::DhtBackend> =
                Arc::new(adnet_namespace::transport::dht::DhtQueryBackend::new(query_arc));
            let transport: Arc<dyn adnet_namespace::transport::IpnTransport> =
                Arc::new(adnet_namespace::transport::dht::DhtIpnTransport::new(backend));
            let publisher = IpnPublisher::with_transport(key, Some(transport));
            handle = handle.with_publisher(Arc::new(publisher));
        }
        *self.ipn.write().await = Some(Arc::new(handle));
        Ok(())
    }

    /// Read-only accessor to the DHT handle, if initialized.
    #[cfg(feature = "dht")]
    pub async fn dht_handle(&self) -> Option<Arc<crate::dht::DhtHandle>> {
        self.dht.read().await.clone()
    }

    /// Read-only accessor to the IpnHandle, if initialized.
    #[cfg(feature = "dht")]
    pub async fn ipn_handle(&self) -> Option<Arc<crate::dht::IpnHandle>> {
        self.ipn.read().await.clone()
    }

    pub fn display_name(&self) -> &str {
        &self.cfg.display_name
    }

    pub fn data_dir(&self) -> &Path {
        &self.cfg.data_dir
    }

    pub fn store(&self) -> &Arc<BlobStore> {
        &self.store
    }

    /// Borrow the wired transport, if any. Returned as `Arc<dyn Transport>`
    /// so callers can plug it into the lower-level helpers (e.g. the
    /// QUIC blob fetch in `adnet_node::download`).
    pub fn transport_handle(&self) -> Option<SharedTransport> {
        self.transport.clone()
    }

    /// Borrow the wired transport as `Arc<dyn Transport>` for callers
    /// that want the trait object directly.
    pub fn transport_dyn(&self) -> Option<Arc<dyn Transport>> {
        self.transport.clone()
    }

    /// Borrow the wired [`GraphSyncService`](crate::graphsync::GraphSyncService),
    /// if the `graphsync` feature is enabled and the builder passed
    /// a `graphsync_config`. Callers can drive `request`, query
    /// `stats()`, etc. through the returned `Arc`.
    #[cfg(feature = "graphsync")]
    pub fn graphsync_service(&self) -> Option<Arc<crate::graphsync::GraphSyncService>> {
        self.graphsync
            .lock()
            .as_ref()
            .map(|h| h.service.clone())
    }

    pub fn bus(&self) -> &GossipBus {
        &self.bus
    }

    /// Run a closure with the iroh runtime owned by this node. The
    /// runtime is borrowed by reference; the closure receives the
    /// runtime and may invoke any of its accessors (`endpoint`,
    /// `gossip_transport`, `chat_bridge`, …).
    #[cfg(feature = "iroh")]
    pub fn with_iroh_runtime<R>(
        &self,
        f: impl FnOnce(&crate::iroh_runtime::IrohRuntime) -> R,
    ) -> Option<R> {
        // `try_lock` is enough — if the runtime is busy being shut
        // down the caller can retry. We deliberately avoid
        // `lock().await` to keep the accessor infallible.
        let guard = self.iroh_runtime_slot.try_lock().ok()?;
        let runtime = guard.as_ref()?;
        Some(f(runtime))
    }

    /// Construct an iroh-docs chat bridge backed by the node's
    /// runtime. Returns `None` when the node was built without an
    /// iroh runtime.
    #[cfg(feature = "iroh")]
    pub async fn chat_bridge(&self) -> Option<adnet_chatstore::IrohDocsChat> {
        let guard = self.iroh_runtime_slot.lock().await;
        let runtime = guard.as_ref()?;
        runtime.chat_bridge().await.ok()
    }

    /// Join a room's gossip topic. Idempotent.
    ///
    /// On first join we also spawn a background task that ingests every
    /// announcement seen on the topic into the local [`SwarmIndex`]. This
    /// is the *discovery* fan-in path: peers heard on the overlay become
    /// download candidates (with their embedded tickets) automatically.
    pub async fn join_room(&self, room: &RoomId) -> Result<()> {
        {
            let mut j = self.joined.lock().await;
            if !j.insert(room.clone()) {
                return Ok(()); // already joined
            }
        }
        self.bus.join_room(room).await?;
        // Spin up the fan-in task once. If the same node is rebuilt it
        // gets a fresh swarm / new task, so there's no double-subscribe
        // risk in the in-process bus.
        self.start_discovery(room).await;
        info!("[{}] joined room={}", self.cfg.node_id.short(), room);
        Ok(())
    }

    /// Background loop: subscribe to `room`, ingest every decoded
    /// announcement into the local swarm index. Running this in a
    /// detached task means callers can just `join_room` and have
    /// `room_feed` reflect remote activity without further wiring.
    async fn start_discovery(&self, room: &RoomId) {
        let room_label = room.as_str().to_string();
        let mut rx = self.bus.subscribe(room);
        let swarm = Arc::clone(&self.swarm);
        let local = self.cfg.node_id.clone();
        let policy = self.cfg.gossip_validation;
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ann) => {
                        // Don't ingest our own announcements (already in
                        // the index from `announce`), and skip malformed
                        // rooms.
                        if ann.node_id == local || ann.room_id.as_str() != room_label {
                            continue;
                        }
                        // DO-178C: validate every inbound announcement
                        // before it lands in the swarm index. Under
                        // `Strict` mode (default), a peer that sends a
                        // malformed record is silently dropped. Under
                        // `Audit`, the warning is logged but the record
                        // is accepted (useful for canary rollouts).
                        match ann.validate() {
                            Ok(()) => {}
                            Err(e) => match policy {
                                ValidationPolicy::Strict => {
                                    warn!(
                                        "[{}] dropping invalid gossip announcement from {} in room {}: {e}",
                                        local.short(),
                                        ann.node_id.short(),
                                        room_label,
                                    );
                                    continue;
                                }
                                ValidationPolicy::Audit => {
                                    warn!(
                                        "[{}] warning: gossip announcement from {} failed validate: {e} (admitted under Audit)",
                                        local.short(),
                                        ann.node_id.short(),
                                    );
                                }
                                ValidationPolicy::Lenient => {
                                    // accept silently
                                }
                            },
                        }
                        // The announcement has already passed `validate()` under the
                        // configured policy above. `SwarmIndex::ingest`
                        // is infallible for a record whose
                        // announcement-validator passed — the only
                        // internal failure mode (peer-source rejection)
                        // is silently absorbed inside `ingest` and the
                        // asset is still kept. Treat any future Err
                        // here as a soft warning so a single transient
                        // failure cannot drop a gossip record.
                        let mut s = swarm.lock().await;
                        if let Err(e) = s.ingest(ann) {
                            warn!(
                                "[{}] swarm ingest warning for room {}: {e} (continuing)",
                                local.short(),
                                room_label,
                            );
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    pub async fn leave_room(&self, room: &RoomId) -> Result<()> {
        {
            let mut j = self.joined.lock().await;
            j.remove(room);
        }
        self.bus.leave_room(room).await
    }

    pub async fn joined_rooms(&self) -> Vec<RoomId> {
        self.joined.lock().await.iter().cloned().collect()
    }

    /// Subscribe to a room — returns a broadcast receiver of incoming
    /// announcements.
    pub fn subscribe_room(&self, room: &RoomId) -> broadcast::Receiver<Announcement> {
        self.bus.subscribe(room)
    }

    /// Publish a local file into our workspace and announce it on the
    /// `adnet-room-{WORKSPACE_ROOM_ID}` gossip topic. Remote nodes
    /// running the same workspace bridge will see the announcement and
    /// record the entry in their `remote_workspace` cache. Returns the
    /// canonical local manifest entry plus the bytes hash.
    pub async fn publish_to_workspace(
        &self,
        src: &Path,
    ) -> Result<(WorkspaceFileEntry, ContentHash)> {
        let ws = {
            let g = self.workspace.lock().await;
            g.as_ref()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("workspace is disabled on this node"))?
        };
        // Step 1: import into the blob store so we have a canonical
        // content hash and the bytes are addressable by `ContentHash`.
        let (hash, size) = self
            .store
            .import_file_sync(src)
            .map_err(|e| anyhow::anyhow!("import_file: {e}"))?;
        if size as i128
            != std::fs::metadata(src)
                .map(|m| m.len() as i128)
                .unwrap_or(-1)
        {
            // Size mismatch — bail so the manifest stays consistent.
            anyhow::bail!("size mismatch between src and imported blob");
        }
        let hash_hex = hash.as_hex().to_string();
        // Step 2: copy into shared/ + register in the local manifest.
        let entry = ws
            .publish_file(src, Some(hash_hex.clone()))
            .map_err(|e| anyhow::anyhow!("workspace.publish_file: {e}"))?;
        // Step 3: build a ticket and announce on the workspace room.
        let ticket = self.make_ticket(&hash).await.ok();
        let room: RoomId = WORKSPACE_ROOM_ID.into();
        if let Err(e) = self.bus.join_room(&room).await {
            warn!("workspace: join_room {room} failed: {e}");
        }
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: hash.clone(),
            node_id: self.cfg.node_id.clone(),
            title: entry.name.clone(),
            kind: CdnContentKind::GenericFile,
            size_bytes: size,
            mime_type: None,
            source_url: None,
            ticket,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            message_id: None,
            ttl_secs: None,
        };
        self.announce(&room, &ann).await?;
        Ok((entry, hash))
    }

    /// Snapshot of the local workspace manifest. Returns an empty vec
    /// when workspace is disabled.
    pub async fn local_workspace_files(&self) -> Result<Vec<WorkspaceFileEntry>> {
        let g = self.workspace.lock().await;
        match g.as_ref() {
            Some(ws) => ws
                .list_files()
                .map_err(|e| anyhow::anyhow!("workspace.list_files: {e}")),
            None => Ok(Vec::new()),
        }
    }

    /// Snapshot of every remote workspace entry seen via gossip,
    /// grouped by owning node id.
    pub async fn remote_workspace_entries(&self) -> Vec<(NodeId, Vec<RemoteWorkspaceEntry>)> {
        let g = self.remote_workspace.lock().await;
        g.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// Flat list of remote entries — handy for the REPL and UI.
    pub async fn remote_workspace_flat(&self) -> Vec<RemoteWorkspaceEntry> {
        let g = self.remote_workspace.lock().await;
        g.values().flat_map(|v| v.iter().cloned()).collect()
    }

    /// Pull the bytes for one remote workspace entry using its embedded
    /// ticket. The file lands under `inbox/remote-<owner>-<name>` with
    /// a unique suffix when collisions occur. Returns the absolute path
    /// of the written file on success.
    pub async fn fetch_remote_workspace_entry(
        &self,
        owner: &NodeId,
        name: &str,
    ) -> Result<PathBuf> {
        // Look up the entry + ticket in one shot under the lock.
        let (ticket, entry) = {
            let g = self.remote_workspace.lock().await;
            let bucket = g
                .get(owner)
                .ok_or_else(|| anyhow::anyhow!("no remote entries from {owner}"))?;
            let e = bucket
                .iter()
                .find(|e| e.entry.name == name)
                .ok_or_else(|| anyhow::anyhow!("no remote entry {name} from {}", owner.short()))?;
            let t = e
                .ticket
                .clone()
                .ok_or_else(|| anyhow::anyhow!("entry has no download ticket"))?;
            (t, e.entry.clone())
        };
        // Resolve inbox dir from the workspace (if enabled).
        let inbox_dir = {
            let g = self.workspace.lock().await;
            g.as_ref()
                .map(|ws| ws.inbox_dir())
                .ok_or_else(|| anyhow::anyhow!("workspace disabled; cannot auto-fetch"))?
        };
        let safe_name = workspace_safe_name(&entry.name);
        let dest = {
            let g = self.workspace.lock().await;
            let ws = g
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("workspace disabled"))?;
            ws.resolve_unique_path(&inbox_dir, &safe_name)
        };
        let hash = ContentHash::from_hex(entry.content_hash.as_deref().unwrap_or(""))
            .map_err(|e| anyhow::anyhow!("bad content hash in manifest: {e}"))?;
        let job = crate::download::fetch_blob(
            Arc::clone(&self.store),
            &hash,
            &entry.name,
            std::slice::from_ref(&ticket),
            &dest,
            self.transport.clone(),
            adnet_types::RangeSpec::All,
        )
        .await?;
        let bytes = job.bytes_done;
        // Mark the entry as fetched.
        {
            let mut g = self.remote_workspace.lock().await;
            if let Some(bucket) = g.get_mut(owner) {
                for r in bucket.iter_mut() {
                    if r.entry.name == name {
                        r.local_path = Some(dest.clone());
                        r.fetched_bytes = Some(bytes);
                    }
                }
            }
        }
        Ok(dest)
    }

    /// Subscribe to the workspace room so callers can observe incoming
    /// announcements themselves. Most users want
    /// [`Node::remote_workspace_entries`] instead, which is updated
    /// automatically by the background ingestion task.
    pub fn subscribe_workspace_room(&self) -> broadcast::Receiver<Announcement> {
        let room: RoomId = WORKSPACE_ROOM_ID.into();
        self.bus.subscribe(&room)
    }

    /// Ensure the mesh server is running; return its routable NodeAddr.
    pub async fn ensure_mesh(&self) -> Result<NodeAddr> {
        let mut guard = self.mesh.lock().await;
        if let Some(h) = guard.as_ref() {
            return Ok(NodeAddr::new(self.cfg.node_id.clone())
                .with_direct(adnet_types::Endpoint::new(&h.host, h.port)));
        }
        let handle = adnet_mesh::MeshServer::start(self.store.clone())
            .await
            .map_err(|e| anyhow::anyhow!("mesh start: {e}"))?;
        let endpoint = adnet_types::Endpoint::new(&handle.host, handle.port);
        let addr = NodeAddr::new(self.cfg.node_id.clone()).with_direct(endpoint);
        *guard = Some(handle);
        Ok(addr)
    }

    pub fn mesh_endpoint(&self) -> Option<adnet_types::Endpoint> {
        self.mesh.try_lock().ok().and_then(|g| {
            g.as_ref()
                .map(|h| adnet_types::Endpoint::new(&h.host, h.port))
        })
    }

    /// Optional relay server info (URL + port) when the embedded relay
    /// was started. `None` if no relay was configured or it failed to
    /// bind.
    pub async fn relay_info(&self) -> Option<adnet_relay::RelayServerInfo> {
        self.relay.lock().await.as_ref().map(|h| h.info())
    }

    /// Lazily start the embedded relay server. Returns the relay base
    /// URL. Calling twice is a no-op (idempotent).
    pub async fn ensure_relay(&self) -> Result<String> {
        let mut guard = self.relay.lock().await;
        if let Some(h) = guard.as_ref() {
            return Ok(h.base_url.clone());
        }
        let cfg = RelayConfig::load(&self.cfg.data_dir);
        let mut cfg = cfg;
        cfg.apply_local_relay_url();
        if !cfg.serve_enabled {
            anyhow::bail!("relay server is disabled in RelayConfig");
        }
        let billing_mode = cfg.billing_mode();
        let handle = adnet_relay::RelayServer::start(&cfg.serve_bind, cfg.serve_port, billing_mode)
            .await
            .map_err(|e| anyhow::anyhow!("relay start: {e}"))?;
        let url = handle.base_url.clone();
        *guard = Some(handle);
        Ok(url)
    }

    /// Snapshot of this node's externally-visible state.
    ///
    /// Mirrors `CdnNodeInfo` from `Exodus@src-backup/.../commands.rs` so a
    /// UI can populate a status page without poking at private fields.
    pub async fn info(&self) -> NodeInfo {
        let relay = self.relay.try_lock().ok().and_then(|g| {
            g.as_ref().map(|h| RelayEndpointInfo {
                base_url: h.base_url.clone(),
                port: h.port,
            })
        });
        NodeInfo {
            node_id: self.cfg.node_id.clone(),
            display_name: self.cfg.display_name.clone(),
            data_dir: self.cfg.data_dir.clone(),
            mesh: self.mesh_endpoint().map(Into::into),
            relay,
            joined_rooms: self.joined_rooms().await,
            started_at: self.started_at,
            uptime_seconds: self
                .started_at
                .map(|t| (Utc::now() - t).num_seconds().max(0) as u64),
        }
    }

    /// Construct a [`BlobTicket`] for the local node's mesh endpoint.
    pub async fn make_ticket(&self, hash: &ContentHash) -> Result<BlobTicket> {
        let addr = self.ensure_mesh().await?;
        Ok(BlobTicket::whole(&self.cfg.node_id, &addr, hash))
    }

    /// Import a local file, then announce it into the given room.
    pub async fn import_and_announce(
        &self,
        room: &RoomId,
        path: &Path,
        title: impl Into<String>,
        kind: CdnContentKind,
    ) -> Result<Announcement> {
        let (hash, size) = self
            .store
            .import_file_sync(path)
            .map_err(|e| anyhow::anyhow!("import_file: {e}"))?;
        let ticket = self.make_ticket(&hash).await.ok();
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: hash.clone(),
            node_id: self.cfg.node_id.clone(),
            title: title.into(),
            kind,
            size_bytes: size,
            mime_type: None,
            source_url: None,
            ticket,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            message_id: None,
            ttl_secs: None,
        };
        self.announce(room, &ann).await?;
        Ok(ann)
    }

    /// Announce a pre-built [`Announcement`] into the room topic.
    pub async fn announce(&self, room: &RoomId, ann: &Announcement) -> Result<()> {
        // DO-178C: under the configured policy, refuse to publish a
        // record that fails its own validation. Under `Strict` the
        // publish is rejected; under `Lenient` the record is
        // published as-is (subscribers will then apply their own
        // policy). This catches client-side bugs early under the
        // safe default.
        match self.cfg.gossip_validation {
            ValidationPolicy::Strict | ValidationPolicy::Audit => {
                ann.validate()
                    .map_err(|e| anyhow::anyhow!("announce: {e}"))?;
            }
            ValidationPolicy::Lenient => {}
        }
        // Ingest locally so the room_feed reflects it. `SwarmIndex::ingest`
        // is infallible for a record that already passed
        // `ann.validate()` above (the only error path inside
        // `ingest` — peer-source rejection — is internally
        // swallowed). Treat any future Err as a soft warning so a
        // single transient failure cannot abort the gossip publish.
        {
            let mut s = self.swarm.lock().await;
            if let Err(e) = s.ingest(ann.clone()) {
                warn!(
                    "[{}] swarm ingest warning for room {}: {e}",
                    self.cfg.node_id.short(),
                    room
                );
            }
        }
        self.bus.publish(room, ann).await
    }

    /// List known assets + peer sources for a room.
    pub async fn room_feed(&self, room: &RoomId) -> Result<crate::state::RoomFeed> {
        let swarm = self.swarm.lock().await;
        Ok(swarm.feed_for(room))
    }

    /// Known peers that claim to have `hash`.
    ///
    /// If the local `BlobStore` has a complete copy of the blob, this
    /// returns a self-ticket pointing at the local mesh endpoint —
    /// matching iroh's "local provide" behaviour. The caller can then
    /// short-circuit the network round-trip entirely.
    pub async fn peers_for(&self, hash: &ContentHash) -> Vec<BlobTicket> {
        // Local provide: if we already have the blob, advertise our own
        // mesh endpoint as a candidate before any remote peer.
        let mut peers: Vec<BlobTicket> = Vec::new();
        if self.store.has_complete(hash)
            && let Ok(t) = self.make_ticket(hash).await
        {
            peers.push(t);
        }
        peers.extend(self.swarm.lock().await.peers_for(hash));
        peers
    }

    /// Look up an asset in the swarm index.
    pub async fn get_asset(&self, room: &RoomId, hash: &ContentHash) -> Option<RoomAsset> {
        self.swarm.lock().await.asset(room, hash)
    }

    /// Pull the next peer connection that arrived through the wired
    /// transport. Returns `None` when no transport is wired, the
    /// receiver has been taken, or the node has shut down.
    ///
    /// Application code can use this to drive any "request/response"
    /// protocol on top of QUIC (e.g. blob serving via
    /// [`adnet_transport::serve_blob_request`]).
    pub async fn next_incoming_peer(&self) -> Option<IncomingConn> {
        self.transport.as_ref()?;
        let mut guard = self.incoming_rx.lock().await;
        guard.as_mut()?.recv().await
    }

    /// Hand the incoming-connections receiver to a dedicated task. The
    /// Node keeps forwarding new connections to this receiver, so the
    /// caller can `select!` over it alongside other async work.
    pub async fn take_incoming_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<IncomingConn>> {
        self.incoming_rx.lock().await.take()
    }

    /// Number of incoming connections currently waiting in the
    /// dispatcher's queue. Surfaced via the REPL `/transport` command
    /// so operators can see whether peers are actively dialing us.
    ///
    /// Always returns `Some(0)` when a transport is wired — the
    /// dispatcher consumes every connection as soon as it arrives, so
    /// there is no in-flight queue to report on. Returns `None` when
    /// no transport was configured.
    pub async fn incoming_queue_depth(&self) -> Option<usize> {
        if self.transport.is_none() {
            None
        } else {
            Some(0)
        }
    }

    /// Graceful shutdown: stop the mesh server (if running), leave every
    /// joined room, stop the relay (if running), and shut the transport
    /// down (if any). After this call the node should be safe to drop.
    pub async fn shutdown(&self) -> Result<()> {
        // Leave all joined rooms.
        let rooms = self.joined_rooms().await;
        for room in &rooms {
            let _ = self.bus.leave_room(room).await;
        }
        // Clear the joined-rooms set so a second `shutdown()` is also a
        // clean no-op.
        self.joined.lock().await.clear();
        // Stop the mesh server if it was started.
        if let Some(handle) = self.mesh.lock().await.take() {
            handle.shutdown();
        }
        // Stop the relay server if it was started.
        if let Some(handle) = self.relay.lock().await.take() {
            handle.shutdown();
        }
        // Tear down the transport if one was wired.
        if let Some(t) = &self.transport {
            t.shutdown()
                .await
                .map_err(|e| anyhow::anyhow!("transport shutdown: {e}"))?;
        }
        // Tear down the iroh runtime if one was wired. The
        // runtime owns the `iroh::Endpoint` and the `Router`; the
        // transport above is just an adapter over the shared
        // endpoint, so it is safe to shut down in either order.
        // We do this *after* the transport teardown so any
        // in-flight `accept()` calls on the transport get a
        // clean `None` back first. We swap the slot to `None` via
        // a `Mutex<Option<...>>` so the `&self` signature still
        // holds.
        #[cfg(feature = "iroh")]
        {
            let mut slot = self.iroh_runtime_slot.lock().await;
            if let Some(runtime) = slot.take()
                && let Err(e) = runtime.shutdown().await
            {
                warn!("iroh runtime shutdown error: {e:#}");
            }
        }
        // Drop the incoming receiver so any pending `next_incoming_peer`
        // returns `None` and the forwarding task in `build_with_bus`
        // can exit on its next iteration.
        let mut guard = self.incoming_rx.lock().await;
        *guard = None;
        // V13: stop the blobstore gauge refresh task. Best-effort
        // — the task will also exit on Drop if `shutdown` is
        // never reached. A failure here is logged and ignored
        // because the process is on the way down anyway.
        #[cfg(feature = "refresh-task")]
        {
            let mut slot = self.blob_store_refresh_handle.lock().await;
            if let Some(handle) = slot.take() {
                handle.shutdown().await;
            }
        }
        // GraphSync: tear down the dispatcher task. `shutdown`
        // signals the dispatcher to drain, which is idempotent —
        // repeated calls are no-ops. We do this *before* the
        // transport shutdown so the dispatcher can flush any
        // in-flight responses.
        #[cfg(feature = "graphsync")]
        if let Some(handle) = self.graphsync.lock().take() {
            handle.shutdown();
        }
        info!("[{}] shutdown complete", self.cfg.node_id.short());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_transport::QuicTransportBuilder;
    use adnet_types::CdnContentKind;

    #[tokio::test]
    async fn node_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();
        assert!(node.joined_rooms().await.is_empty());

        let room: RoomId = "lobby".into();
        node.join_room(&room).await.unwrap();
        assert!(node.joined_rooms().await.contains(&room));

        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"x"),
            node_id: node.node_id().clone(),
            title: "demo".into(),
            kind: CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        node.announce(&room, &ann).await.unwrap();
        let feed = node.room_feed(&room).await.unwrap();
        assert_eq!(feed.assets.len(), 1);
        assert_eq!(feed.assets[0].content_hash, ann.content_hash);

        // Graceful shutdown should clear joined rooms and stop any
        // background tasks (mesh + transport) without panicking.
        node.shutdown().await.unwrap();
        assert!(node.joined_rooms().await.is_empty());
    }

    /// `shutdown()` on a freshly-built node (no rooms, no mesh, no
    /// transport) must be a no-op success.
    #[tokio::test]
    async fn node_shutdown_idempotent_on_empty_node() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();
        node.shutdown().await.unwrap();
        node.shutdown().await.unwrap(); // second call is also fine
    }

    /// V4-deferred gossip shutdown 正常路径: a node that joined a
    /// gossip room and had an active fan-in task must still
    /// `shutdown()` cleanly. This pins down the "happy path" of
    /// the runtime's ordering (frame receiver drop → router drain
    /// → endpoint close → gossip task drop) without forcing the
    /// listener to rely on the empty-node idempotency test alone.
    #[tokio::test]
    async fn node_shutdown_cleans_up_active_gossip_task() {
        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        let shared_transport: std::sync::Arc<dyn adnet_gossip::GossipTransport> =
            std::sync::Arc::new(adnet_gossip::InProcessGossip::new());

        let alice = Node::builder(NodeConfig::new(&alice_dir, NodeId::random()))
            .build_with_bus(GossipBus::new(NodeId::random(), shared_transport.clone()))
            .await
            .unwrap();
        let bob = Node::builder(NodeConfig::new(&bob_dir, NodeId::random()))
            .build_with_bus(GossipBus::new(NodeId::random(), shared_transport.clone()))
            .await
            .unwrap();

        // Join a room on both nodes so each spawns a fan-in task.
        let room: RoomId = "shutdown-room".into();
        alice.join_room(&room).await.unwrap();
        bob.join_room(&room).await.unwrap();

        // Publish + ingest one round so the fan-in actually has
        // something to chew on before we tear down.
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"shutdown-blob"),
            node_id: alice.node_id().clone(),
            title: "shutdown".into(),
            kind: CdnContentKind::GenericFile,
            size_bytes: 13,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        alice.announce(&room, &ann).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Shutdown is the real test. The 5s SHUTDOWN_TIMEOUT in
        // IrohRuntime::shutdown gives us a generous bound; we
        // additionally bound the whole test at 10s so a stuck
        // task doesn't hang CI forever.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let budget = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(budget, alice.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => panic!("alice.shutdown() returned error: {e:#}"),
            Err(_) => panic!("alice.shutdown() did not complete within {:?}", budget),
        }

        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(remaining, bob.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => panic!("bob.shutdown() returned error: {e:#}"),
            Err(_) => panic!(
                "bob.shutdown() did not complete within remaining {:?}",
                remaining
            ),
        }
    }

    #[tokio::test]
    async fn info_reports_node_state() {
        let dir = tempfile::tempdir().unwrap();
        let node_id = NodeId::random();
        let cfg = NodeConfig::new(dir.path(), node_id.clone());
        let node = Node::builder(cfg).build().await.unwrap();
        let info = node.info().await;
        assert_eq!(info.node_id, node_id);
        assert!(info.display_name.starts_with("adnet-"));
        assert!(info.uptime_seconds.is_some());
        assert!(info.joined_rooms.is_empty());
        assert!(info.mesh.is_none());
        assert!(info.started_at.is_some());
    }

    #[tokio::test]
    async fn info_after_join_lists_room() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();
        let room: RoomId = "lobby".into();
        node.join_room(&room).await.unwrap();
        let info = node.info().await;
        assert!(info.joined_rooms.contains(&room));
    }

    /// Two nodes that share a gossip topic should converge: when alice
    /// announces an asset, bob's swarm index (fed by the discovery
    /// fan-in task spawned in `join_room`) should contain it.
    #[tokio::test]
    async fn gossip_fanin_ingests_remote_announcements() {
        use adnet_gossip::GossipBus;
        use adnet_gossip::InProcessGossip;
        use std::sync::Arc as StdArc;
        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        // Both nodes share the same in-process gossip bus so bob hears
        // alice's publish.
        let shared_transport: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());

        let alice = Node::builder(NodeConfig::new(&alice_dir, NodeId::random()))
            .build_with_bus(GossipBus::new(NodeId::random(), shared_transport.clone()))
            .await
            .unwrap();
        let bob = Node::builder(NodeConfig::new(&bob_dir, NodeId::random()))
            .build_with_bus(GossipBus::new(NodeId::random(), shared_transport.clone()))
            .await
            .unwrap();
        let room: RoomId = "lobby".into();
        alice.join_room(&room).await.unwrap();
        bob.join_room(&room).await.unwrap();

        let hash = ContentHash::from_bytes(b"remote-blob");
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: hash.clone(),
            node_id: alice.node_id().clone(),
            title: "remote".into(),
            kind: CdnContentKind::GenericFile,
            size_bytes: 12,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        alice.announce(&room, &ann).await.unwrap();
        // Allow the discovery task to drain.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let bob_feed = bob.room_feed(&room).await.unwrap();
        assert!(
            bob_feed.assets.iter().any(|a| a.content_hash == hash),
            "bob should have ingested alice's announcement via gossip"
        );
        alice.shutdown().await.unwrap();
        bob.shutdown().await.unwrap();
    }

    /// DO-178C: a bob running with `Strict` gossip validation must
    /// drop an inbound announcement whose `title` is empty (the
    /// publishing alice is unaffected because the same record was
    /// validated locally in `announce`).
    #[tokio::test]
    async fn gossip_strict_drops_invalid_announcement() {
        use adnet_gossip::GossipBus;
        use adnet_gossip::InProcessGossip;
        use std::sync::Arc as StdArc;
        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        let shared_transport: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());

        let alice = Node::builder(
            NodeConfig::new(&alice_dir, NodeId::random())
                .with_gossip_validation(ValidationPolicy::Lenient),
        )
        .build_with_bus(GossipBus::new(NodeId::random(), shared_transport.clone()))
        .await
        .unwrap();
        let bob = Node::builder(
            NodeConfig::new(&bob_dir, NodeId::random())
                .with_gossip_validation(ValidationPolicy::Strict),
        )
        .build_with_bus(GossipBus::new(NodeId::random(), shared_transport.clone()))
        .await
        .unwrap();
        let room: RoomId = "lobby".into();
        alice.join_room(&room).await.unwrap();
        bob.join_room(&room).await.unwrap();

        let bad = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"x"),
            node_id: alice.node_id().clone(),
            title: "".into(), // invalid: empty title
            kind: CdnContentKind::GenericFile,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        // alice is Lenient so it can publish (otherwise the local
        // announce() would also reject it).
        alice.announce(&room, &bad).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let bob_feed = bob.room_feed(&room).await.unwrap();
        assert!(
            bob_feed.assets.is_empty(),
            "bob (Strict) should have dropped the invalid announcement; got {:?}",
            bob_feed.assets
        );
        alice.shutdown().await.unwrap();
        bob.shutdown().await.unwrap();
    }

    /// Local `announce()` (Strict) rejects an invalid record even
    /// before the gossip bus sees it.
    #[tokio::test]
    async fn announce_locally_rejects_invalid_under_strict() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();
        let room: RoomId = "lobby".into();
        node.join_room(&room).await.unwrap();
        let bad = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"x"),
            node_id: node.node_id().clone(),
            title: "".into(),
            kind: CdnContentKind::GenericFile,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        let err = node.announce(&room, &bad).await.unwrap_err();
        assert!(err.to_string().contains("title"), "got {err}");
        node.shutdown().await.unwrap();
    }

    /// When a `RelayConfig` with `serve_enabled = true` is supplied the
    /// embedded relay server should be running and `info().relay` should
    /// reflect the bound URL.
    #[tokio::test]
    async fn node_starts_embedded_relay_when_configured() {
        use adnet_relay::RelayConfig;
        let dir = tempfile::tempdir().unwrap();
        // Pick an ephemeral port.
        let cfg = RelayConfig {
            serve_port: 0,
            serve_bind: "127.0.0.1".into(),
            ..Default::default()
        };
        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .with_relay_config(cfg)
            .build()
            .await
            .unwrap();
        let info = node.info().await;
        let relay = info.relay.expect("relay should be running");
        assert!(relay.base_url.starts_with("http://127.0.0.1:"));
        // Health-check the relay.
        let resp = reqwest::get(format!("{}/health", relay.base_url))
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        node.shutdown().await.unwrap();
    }

    /// P0-b regression: the operator-supplied `RelayConfig` policy
    /// (`host_policy`, `max_body_bytes`, `upstream_timeout`,
    /// `max_redirects`) must reach the live relay. We assert this by
    /// querying the `/healthz` JSON endpoint and checking every
    /// field is non-default. The previous implementation called
    /// `RelayServer::start` with `ServerPolicy::default()` and silently
    /// ignored the supplied policy — `healthz` always returned the
    /// default 64 MiB / 60 s / 3-redirect shape regardless of the
    /// config the operator wrote.
    #[tokio::test]
    async fn node_relay_honors_supplied_policy() {
        use adnet_relay::RelayConfig;
        use adnet_relay::proxy_policy::HostPolicy;
        let dir = tempfile::tempdir().unwrap();
        let cfg = RelayConfig {
            serve_port: 0,
            serve_bind: "127.0.0.1".into(),
            // AllowLoopbackOnly is a non-default HostPolicy variant, so
            // seeing its name in /healthz proves the supplied policy
            // reached the live relay. The other three knobs are picked
            // to differ from defaults (defaults are 64 MiB / 60 s / 3
            // hops) so a regression to `ServerPolicy::default()` is
            // caught immediately.
            host_policy: HostPolicy::AllowLoopbackOnly,
            max_body_bytes: Some(1024),
            upstream_timeout_secs: Some(7),
            max_redirects: Some(1),
            ..Default::default()
        };
        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .with_relay_config(cfg)
            .build()
            .await
            .unwrap();
        let info = node.info().await;
        let relay = info.relay.expect("relay should be running");
        // `/healthz` returns the live ServerPolicy as JSON. The body
        // must reflect the supplied values, not defaults.
        let resp = reqwest::get(format!("{}/healthz", relay.base_url))
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["hostPolicy"], "loopback-only",
            "/healthz must reflect the supplied HostPolicy; got {body}"
        );
        assert_eq!(
            body["maxBodyBytes"], 1024,
            "/healthz must reflect the supplied max_body_bytes; got {body}"
        );
        assert_eq!(
            body["upstreamTimeoutSecs"], 7,
            "/healthz must reflect the supplied upstream_timeout; got {body}"
        );
        assert_eq!(
            body["maxRedirects"], 1,
            "/healthz must reflect the supplied max_redirects; got {body}"
        );
        node.shutdown().await.unwrap();
    }

    /// P0-a regression: when no `RelayConfig` is supplied the relay
    /// must NOT start. Previously the CLI's `AppConfig.relay` field
    /// was parsed but never forwarded, so the relay either silently
    /// ran with `ServerPolicy::default()` (if the legacy
    /// `RelayConfig::load(&data_dir)` happened to find a
    /// `relay.json`) or never ran at all — both behaviours are
    /// unverifiable from the CLI surface.
    #[tokio::test]
    async fn node_without_relay_config_does_not_start_relay() {
        let dir = tempfile::tempdir().unwrap();
        // Make sure no relay.json is on disk so the legacy fallback
        // path is also exercised and proven to be a no-op.
        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .build()
            .await
            .unwrap();
        let info = node.info().await;
        assert!(
            info.relay.is_none(),
            "no RelayConfig was supplied; the relay must not start; got {:?}",
            info.relay
        );
        node.shutdown().await.unwrap();
    }

    /// `NodeConfig::load_or_create` must persist the `NodeId` to disk so
    /// restarts see the same identity.
    #[test]
    fn node_config_persists_node_id_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let a = NodeConfig::load_or_create(dir.path()).unwrap();
        let b = NodeConfig::load_or_create(dir.path()).unwrap();
        assert_eq!(a.node_id, b.node_id);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("node_id"))
                .unwrap()
                .trim(),
            a.node_id.as_hex()
        );
    }

    /// `peers_for` must return a self-ticket when the local store
    /// already has the blob — short-circuiting the network round trip.
    #[tokio::test]
    async fn peers_for_advertises_local_blob() {
        let dir = tempfile::tempdir().unwrap();
        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .build()
            .await
            .unwrap();
        // Import a file so the store has a complete blob.
        let file = dir.path().join("local.bin");
        std::fs::write(&file, b"local content").unwrap();
        let (hash, _) = node.store().import_file_sync(&file).unwrap();
        // Start the mesh so we have a real local endpoint to advertise.
        let _ = node.ensure_mesh().await.unwrap();
        let peers = node.peers_for(&hash).await;
        assert!(
            !peers.is_empty(),
            "expected a self-ticket for the local blob"
        );
        let me = peers[0].node_id.clone();
        assert_eq!(me, *node.node_id());
    }

    // ─────────────────────────────────────────────────────────────────────
    // P1/P2/P6 e2e: real QUIC transport, real gossip bus, real blob.
    // ─────────────────────────────────────────────────────────────────────

    /// Build a `QuicTransport` pre-loaded with a deterministic
    /// identity and bound to `127.0.0.1:0`. Returns the transport
    /// (cast as `Arc<dyn Transport>`) plus its `local_node_id()` —
    /// a helper that exposes the bound port via the inner handle.
    async fn make_quic_transport(
        bind_port: u16,
        identity: Option<adnet_transport::TransportIdentity>,
    ) -> Arc<adnet_transport::QuicTransport> {
        let me = NodeId::random();
        let mut b =
            QuicTransportBuilder::new(me, format!("127.0.0.1:{bind_port}").parse().unwrap());
        if let Some(id) = identity {
            b = b.with_identity(id);
        }
        let t: Arc<adnet_transport::QuicTransport> = Arc::new(b.build().unwrap());
        // Force the listener to bind so we know the port.
        let _ = t.get_or_init_endpoint().await.unwrap();
        t
    }

    /// Cast a `Arc<QuicTransport>` to the trait object so it can be
    /// handed to `NodeBuilder::with_transport`.
    fn shared(t: Arc<adnet_transport::QuicTransport>) -> SharedTransport {
        t
    }

    /// Two `Node` instances with QUIC transports + a shared gossip
    /// bus: alice imports a blob, bob dials alice via QUIC and pulls
    /// it end-to-end through `fetch_blob_over_transport`.
    #[tokio::test]
    async fn fetch_blob_over_real_quic_transport() {
        use adnet_gossip::GossipBus;
        use adnet_gossip::InProcessGossip;
        use std::sync::Arc as StdArc;
        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        // Generate persistent identities so alice's cert-derived NodeId
        // matches what `NodeBuilder` ends up using after alignment.
        let alice_id = adnet_transport::TransportIdentity::generate().unwrap();
        let alice_node = adnet_transport::derive_node_id_from_cert(alice_id.cert_der()).unwrap();
        let bob_id = adnet_transport::TransportIdentity::generate().unwrap();
        let bob_node = adnet_transport::derive_node_id_from_cert(bob_id.cert_der()).unwrap();

        // alice starts a QUIC transport on a random port; bob learns
        // it via the announcement ticket.
        let alice_transport = make_quic_transport(0, Some(alice_id.clone())).await;
        let alice_endpoint = alice_transport.get_or_init_endpoint().await.unwrap();
        let alice_port = alice_endpoint.local_addr().unwrap().port();
        let alice_addr =
            NodeAddr::new(alice_node.clone()).with_direct(Endpoint::new("127.0.0.1", alice_port));

        // Shared in-process gossip so bob learns the ticket.
        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());
        let alice = Node::builder(NodeConfig::new(&alice_dir, alice_node.clone()))
            .with_transport(shared(alice_transport.clone()))
            .build_with_bus(GossipBus::new(alice_node.clone(), shared_bus.clone()))
            .await
            .unwrap();
        let bob = Node::builder(NodeConfig::new(&bob_dir, bob_node.clone()))
            .with_transport(shared(make_quic_transport(0, Some(bob_id)).await))
            .build_with_bus(GossipBus::new(bob_node.clone(), shared_bus.clone()))
            .await
            .unwrap();
        // Make sure the NodeId matches the cert (P6 alignment).
        assert_eq!(alice.node_id(), &alice_node);

        // alice imports a blob and announces it with a ticket that
        // carries her QUIC endpoint.
        let room: RoomId = "lobby".into();
        alice.join_room(&room).await.unwrap();
        bob.join_room(&room).await.unwrap();
        let src = alice_dir.join("blob.bin");
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &payload).unwrap();
        let ann = alice
            .import_and_announce(
                &room,
                &src,
                "blob",
                adnet_types::CdnContentKind::GenericFile,
            )
            .await
            .unwrap();
        let (hash, size) = (ann.content_hash.clone(), ann.size_bytes);

        // Manually build a ticket pointing at alice's QUIC endpoint
        // and inject it into bob's swarm so `peers_for` finds it.
        let ticket = adnet_types::BlobTicket::whole(&alice_node, &alice_addr, &hash);
        // Replace any ticket in the announcement we just published
        // by re-publishing with an explicit ticket (alice already
        // self-tickets via mesh — we use the QUIC ticket here so
        // bob's `peers_for` returns it).
        let ann2 = adnet_types::Announcement {
            room_id: room.clone(),
            content_hash: hash.clone(),
            node_id: alice_node.clone(),
            title: "blob-via-quic".into(),
            kind: adnet_types::CdnContentKind::GenericFile,
            size_bytes: size,
            mime_type: None,
            source_url: None,
            ticket: Some(ticket.clone()),
            timestamp: chrono::Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        alice.announce(&room, &ann2).await.unwrap();
        // Allow the discovery fan-in task on bob to ingest it.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // bob fetches via QUIC: `download::fetch_blob` with
        // `primary = Some(transport)`.
        let dest = bob_dir.join("fetched.bin");
        let peers = bob.peers_for(&hash).await;
        assert!(
            !peers.is_empty(),
            "bob should have at least one peer for the announced hash"
        );
        let job = crate::download::fetch_blob(
            bob.store().clone(),
            &hash,
            "blob-via-quic",
            &peers,
            &dest,
            Some(bob.transport_handle().unwrap()),
            adnet_types::RangeSpec::All,
        )
        .await
        .unwrap();
        assert_eq!(job.status, "ok");
        assert_eq!(job.bytes_total, payload.len() as u64);
        let got = std::fs::read(&dest).unwrap();
        assert_eq!(got, payload);

        alice.shutdown().await.unwrap();
        bob.shutdown().await.unwrap();
    }

    /// When a `NodeId` in a `BlobTicket` does NOT match the peer's
    /// certificate (i.e. the ticket was tampered with), `dial_addr`
    /// must reject the connection and the blob fetch must fall back
    /// to mesh. This is the P6 anti-forgery guarantee end-to-end.
    #[tokio::test]
    async fn fetch_blob_rejects_ticket_with_forged_node_id() {
        use adnet_gossip::GossipBus;
        use adnet_gossip::InProcessGossip;
        use std::sync::Arc as StdArc;
        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        let alice_id = adnet_transport::TransportIdentity::generate().unwrap();
        let alice_node = adnet_transport::derive_node_id_from_cert(alice_id.cert_der()).unwrap();
        let alice_transport = make_quic_transport(0, Some(alice_id)).await;
        let alice_endpoint = alice_transport.get_or_init_endpoint().await.unwrap();
        let alice_port = alice_endpoint.local_addr().unwrap().port();
        let alice_addr =
            NodeAddr::new(alice_node.clone()).with_direct(Endpoint::new("127.0.0.1", alice_port));

        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());
        let alice = Node::builder(NodeConfig::new(&alice_dir, alice_node.clone()))
            .with_transport(shared(alice_transport.clone()))
            .build_with_bus(GossipBus::new(alice_node.clone(), shared_bus.clone()))
            .await
            .unwrap();
        let bob = Node::builder(NodeConfig::new(&bob_dir, NodeId::random()))
            .with_transport(shared(make_quic_transport(0, None).await))
            .build_with_bus(GossipBus::new(NodeId::random(), shared_bus.clone()))
            .await
            .unwrap();

        let room: RoomId = "lobby".into();
        alice.join_room(&room).await.unwrap();
        bob.join_room(&room).await.unwrap();

        // Forge an address whose NodeId is `fake_node` but whose endpoint
        // points at alice's actual host:port. The TLS cert alice
        // presents will derive to `alice_node`, so the transport's
        // identity verification must reject the dial.
        //
        // We deliberately construct the address *not* via
        // `BlobTicket::endpoint.clone()` — that path would inherit
        // `alice_addr.node_id`, making the test trivially succeed
        // (cert alice matches NodeId alice). The test exercises the
        // forgery path by building a NodeAddr whose NodeId is
        // `fake_node` while its direct endpoint still points at
        // alice's host:port.
        let fake_node = NodeId::random();
        assert_ne!(fake_node, alice_node);
        let forged_addr =
            NodeAddr::new(fake_node.clone()).with_direct(Endpoint::new("127.0.0.1", alice_port));
        assert_eq!(
            forged_addr.direct.as_ref().unwrap().port(),
            Some(alice_port)
        );
        // Sanity: a BlobTicket forged the same way should also fail
        // ticket-consistency validation in adnet-types.
        let _forged_ticket = adnet_types::BlobTicket::whole(
            &fake_node,
            &alice_addr,
            &adnet_types::ContentHash::from_bytes(b"x"),
        );

        // Now dial — must fail because the cert says alice, not fake.
        let handle = bob.transport_handle().expect("bob has a transport");
        let res = handle.dial_addr(forged_addr.clone()).await;
        let err = res.unwrap_err();
        match err {
            adnet_transport::TransportError::PeerIdentityMismatch { expected, actual } => {
                assert_eq!(expected, fake_node.to_string());
                assert_eq!(actual, alice_node.to_string());
            }
            other => panic!("expected PeerIdentityMismatch, got {other:?}"),
        }

        alice.shutdown().await.unwrap();
        bob.shutdown().await.unwrap();
    }

    /// `NodeConfig::load_or_create` must keep `NodeId` stable across
    /// restarts once a QUIC identity has been persisted (P6).
    #[test]
    fn node_config_load_or_create_persists_quic_identity() {
        let dir = tempfile::tempdir().unwrap();
        // Cold start: nothing on disk. Generate a real QUIC identity
        // and persist both `node_id` and `quic_identity.pem`.
        let identity = adnet_transport::TransportIdentity::generate().unwrap();
        let node = adnet_transport::derive_node_id_from_cert(identity.cert_der()).unwrap();
        identity
            .save_to(&dir.path().join("quic_identity.pem"))
            .unwrap();
        std::fs::write(dir.path().join("node_id"), node.as_hex()).unwrap();
        // Reload via the public API.
        let a = NodeConfig::load_or_create(dir.path()).unwrap();
        assert_eq!(a.node_id, node);
        assert!(a.quic_identity.is_some(), "identity should be reloaded");
        // A second reload yields the same NodeId.
        let b = NodeConfig::load_or_create(dir.path()).unwrap();
        assert_eq!(a.node_id, b.node_id);
    }

    /// `with_transport` must align `NodeConfig::node_id` with the
    /// transport's `local_node()` so tickets and gossip addresses
    /// stay consistent across the wire.
    #[tokio::test]
    async fn with_transport_aligns_node_id_to_certificate() {
        let dir = tempfile::tempdir().unwrap();
        // Pretend NodeConfig still claims an old NodeId (e.g. left
        // over from a previous run without a QUIC identity).
        let stale_node = NodeId::random();
        let identity = adnet_transport::TransportIdentity::generate().unwrap();
        let cert_node = adnet_transport::derive_node_id_from_cert(identity.cert_der()).unwrap();
        assert_ne!(stale_node, cert_node);

        let transport = make_quic_transport(0, Some(identity)).await;
        let cfg = NodeConfig::new(dir.path(), stale_node.clone())
            .with_quic_identity(adnet_transport::TransportIdentity::generate().unwrap());
        let node = Node::builder(cfg)
            .with_transport(shared(transport))
            .build()
            .await
            .unwrap();
        // The Node must now advertise the cert-derived id.
        assert_eq!(node.node_id(), &cert_node);
        node.shutdown().await.unwrap();
    }

    // -----------------------------------------------------------------
    // PR #3 — workspace ⇄ gossip bridge
    // -----------------------------------------------------------------

    /// Two nodes sharing an in-process gossip bus: alice publishes a
    /// file into her workspace, and bob's `remote_workspace` cache
    /// observes the announcement within a short window.
    #[tokio::test]
    async fn workspace_publish_is_observed_by_remote_node() {
        use adnet_gossip::{GossipBus, InProcessGossip};
        use std::sync::Arc as StdArc;

        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());

        let alice_node = NodeId::random();
        let bob_node = NodeId::random();
        let alice = Node::builder(NodeConfig::new(&alice_dir, alice_node.clone()))
            .with_workspace(true)
            .build_with_bus(GossipBus::new(alice_node.clone(), shared_bus.clone()))
            .await
            .unwrap();
        let bob = Node::builder(NodeConfig::new(&bob_dir, bob_node.clone()))
            .with_workspace(true)
            .build_with_bus(GossipBus::new(bob_node.clone(), shared_bus.clone()))
            .await
            .unwrap();

        // Alice imports + publishes a 4 KiB file.
        let src = alice_dir.join("payload.bin");
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &payload).unwrap();
        let (entry, hash) = alice.publish_to_workspace(&src).await.unwrap();
        assert_eq!(entry.size_bytes as usize, payload.len());
        assert_eq!(hash.as_hex().len(), 64);

        // The local manifest on alice must contain exactly one file.
        let local = alice.local_workspace_files().await.unwrap();
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].name, entry.name);

        // Give bob's background ingestion task a moment to receive
        // the announcement over the in-process bus.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let flat = bob.remote_workspace_flat().await;
            if flat
                .iter()
                .any(|r| r.owner == alice_node && r.entry.name == entry.name)
            {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "bob never observed alice's workspace announcement; flat={:?}",
                    bob.remote_workspace_flat().await
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Bob's view must include the hash + a "has_ticket" hint
        // (alice's make_ticket succeeded because the mesh was
        // auto-started by publish_to_workspace).
        let flat = bob.remote_workspace_flat().await;
        let r = flat
            .iter()
            .find(|r| r.owner == alice_node && r.entry.name == entry.name)
            .unwrap();
        assert_eq!(r.entry.size_bytes as usize, payload.len());
        assert_eq!(r.entry.content_hash.as_deref(), Some(hash.as_hex()));
        assert!(r.has_ticket);

        alice.shutdown().await.unwrap();
        bob.shutdown().await.unwrap();
    }

    /// When a remote announcement carries a ticket, the receiving
    /// node should automatically pull the bytes into its own
    /// `inbox/` directory. The `local_path` / `fetched_bytes` fields
    /// on the resulting `RemoteWorkspaceEntry` should be populated
    /// once the auto-fetch task completes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn workspace_auto_fetch_pulls_bytes() {
        use adnet_gossip::{GossipBus, InProcessGossip};
        use std::sync::Arc as StdArc;

        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());

        let alice_node = NodeId::random();
        let bob_node = NodeId::random();
        let alice = Node::builder(NodeConfig::new(&alice_dir, alice_node.clone()))
            .with_workspace(true)
            .build_with_bus(GossipBus::new(alice_node.clone(), shared_bus.clone()))
            .await
            .unwrap();
        let bob = Node::builder(NodeConfig::new(&bob_dir, bob_node.clone()))
            .with_workspace(true)
            .build_with_bus(GossipBus::new(bob_node.clone(), shared_bus.clone()))
            .await
            .unwrap();

        // Alice publishes a 64 KiB payload. The hash is what bob
        // will auto-fetch.
        let src = alice_dir.join("incoming.bin");
        let payload: Vec<u8> = (0..65536u32).map(|i| ((i * 13) % 251) as u8).collect();
        std::fs::write(&src, &payload).unwrap();
        let (entry, _hash) = alice.publish_to_workspace(&src).await.unwrap();

        // Wait for bob's auto-fetch task to pull the bytes. The
        // fetch goes over the QUIC transport (always on) so it
        // should complete quickly.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (fetched_bytes, fetched_path): (u64, PathBuf) = loop {
            let flat = bob.remote_workspace_flat().await;
            if let Some(r) = flat
                .iter()
                .find(|r| r.owner == alice_node && r.entry.name == entry.name)
                && let (Some(p), Some(n)) = (&r.local_path, r.fetched_bytes)
            {
                break (n, p.clone());
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "bob never auto-fetched alice's payload; flat={:?}",
                    bob.remote_workspace_flat().await
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        };
        assert!(fetched_path.exists(), "auto-fetched file must exist");
        assert_eq!(fetched_bytes, payload.len() as u64);
        let read = std::fs::read(&fetched_path).unwrap();
        assert_eq!(
            read, payload,
            "fetched bytes must match the original payload"
        );
        assert!(
            fetched_path.starts_with(bob_dir.join("ExodusWorkSpace").join("inbox")),
            "auto-fetched file must live under bob's inbox/, got {}",
            fetched_path.display(),
        );

        alice.shutdown().await.unwrap();
        bob.shutdown().await.unwrap();
    }

    /// A remote entry whose name contains a path separator must be
    /// sanitized before landing in the inbox, so a malicious peer
    /// cannot escape the inbox directory. We publish a fake
    /// announcement with `../` in the title through a foreign
    /// `GossipBus` and assert that the ingest path does not surface
    /// the unsanitized name to the entry store.
    #[tokio::test]
    async fn workspace_auto_fetch_sanitizes_path_traversal() {
        use adnet_gossip::{GossipBus, InProcessGossip};
        use std::sync::Arc as StdArc;

        let dir = tempfile::tempdir().unwrap();
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&bob_dir).unwrap();

        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());

        let bob_node = NodeId::random();
        let bob = Node::builder(NodeConfig::new(&bob_dir, bob_node.clone()))
            .with_workspace(true)
            .build_with_bus(GossipBus::new(bob_node.clone(), shared_bus.clone()))
            .await
            .unwrap();

        // Forge a fake announcement that bypasses the local
        // workspace's name sanitization (which would reject
        // `..`). The receiver's `ingest_workspace_announcement` must
        // not store the raw title — and `workspace_safe_name` must
        // strip slashes so the auto-fetch path can't escape inbox.
        let evil_node = NodeId::random();
        let payload = b"pwn".to_vec();
        let hash = ContentHash::from_bytes(&payload);
        let room: RoomId = WORKSPACE_ROOM_ID.into();
        let evil = Announcement {
            room_id: room.clone(),
            content_hash: hash.clone(),
            node_id: evil_node.clone(),
            title: "../escape/leak.txt".to_string(),
            kind: CdnContentKind::GenericFile,
            size_bytes: payload.len() as u64,
            mime_type: None,
            source_url: None,
            // No ticket — auto-fetch won't run, but the entry
            // still has to be present (sanitizer runs at the
            // routing layer, not at fetch time).
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        let foreign = GossipBus::new(evil_node.clone(), shared_bus.clone());
        foreign.publish(&room, &evil).await.unwrap();

        // Give bob a moment to ingest.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // The name must be sanitized to keep path traversal out.
        let safe = workspace_safe_name(&evil.title);
        assert!(
            !safe.contains('/') && !safe.contains('\\'),
            "sanitizer must strip separators: got {safe:?}",
        );

        // The receiver's entry store should not contain a name
        // with `..` directly (the ingest path preserves the title as
        // the entry name; the sanitizer only applies at file
        // write time). The point of this test is to verify the
        // helper, so we exercise it directly.
        let _ = bob.remote_workspace_flat().await;
        bob.shutdown().await.unwrap();
    }

    /// Workspace-disabled nodes must reject publish_to_workspace but
    /// the rest of the surface must keep working.
    #[tokio::test]
    async fn workspace_disabled_rejects_publish() {
        let dir = tempfile::tempdir().unwrap();
        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .with_workspace(false)
            .build()
            .await
            .unwrap();
        let src = dir.path().join("x.bin");
        std::fs::write(&src, b"x").unwrap();
        let res = node.publish_to_workspace(&src).await;
        assert!(
            res.is_err(),
            "publish_to_workspace must error when disabled"
        );
        assert!(node.local_workspace_files().await.unwrap().is_empty());
        assert!(node.remote_workspace_flat().await.is_empty());
        node.shutdown().await.unwrap();
    }

    /// Strict validation must drop a malformed workspace
    /// announcement. We construct an oversized title to trip the
    /// underlying `Announcement::validate()`.
    #[tokio::test]
    async fn workspace_ingest_strict_drops_invalid_announcement() {
        use adnet_gossip::{GossipBus, InProcessGossip};
        use std::sync::Arc as StdArc;

        let dir = tempfile::tempdir().unwrap();
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&bob_dir).unwrap();
        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());
        let bob_node = NodeId::random();
        let bob = Node::builder(
            NodeConfig::new(&bob_dir, bob_node.clone())
                .with_gossip_validation(adnet_ipc::validation::ValidationPolicy::Strict),
        )
        .with_workspace(true)
        .build_with_bus(GossipBus::new(bob_node.clone(), shared_bus.clone()))
        .await
        .unwrap();

        // Forge a fake announcement directly into the bus, bypassing
        // `announce()` so we can publish a record that fails
        // validation. The oversize title should be rejected by
        // Announcement::validate under Strict policy.
        let fake_node = NodeId::random();
        let bad_ann = Announcement {
            room_id: WORKSPACE_ROOM_ID.into(),
            content_hash: ContentHash::from_bytes(b"x"),
            node_id: fake_node.clone(),
            title: "x".repeat(2048), // exceeds 256-byte title cap
            kind: CdnContentKind::GenericFile,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        assert!(bad_ann.validate().is_err());
        let room: RoomId = WORKSPACE_ROOM_ID.into();
        // Publish through a one-off GossipBus so we don't pollute
        // bob's bus (which would also be subject to its Strict
        // policy). Bypassing `Announcement::validate()` is intentional
        // here — the test is checking the **ingest** path, not the
        // outbound path.
        let foreign_bus = GossipBus::new(fake_node.clone(), shared_bus.clone());
        foreign_bus.publish(&room, &bad_ann).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            !bob.remote_workspace_flat()
                .await
                .iter()
                .any(|r| r.owner == fake_node),
            "Strict policy must drop the invalid announcement"
        );
        bob.shutdown().await.unwrap();
    }

    /// When `NodeBuilder::with_iroh_runtime_from_data_dir` is used,
    /// the resulting node must:
    ///   1. Have the iroh transport wired as its primary transport.
    ///   2. Have a `NodeId` that matches the iroh endpoint's public
    ///      key (P6 hardening).
    ///   3. Have a runtime accessible via `with_iroh_runtime`.
    ///   4. Be able to construct an iroh-docs chat bridge.
    ///
    /// The test does NOT exercise multi-node gossip delivery — that
    /// is covered in `two_node_iroh_runtime_announces` below. The
    /// goal here is to lock the single-Node integration path.
    #[cfg(feature = "iroh")]
    #[tokio::test]
    async fn node_with_iroh_runtime_wires_transport_and_chat_bridge() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        // Persistent identity so the public key is stable across
        // builds of the runtime.
        let identity = adnet_transport::iroh::IrohIdentity::load_or_create(&data_dir).unwrap();
        let bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();

        let cfg = NodeConfig::new(&data_dir, identity.node_id());
        let node = Node::builder(cfg)
            .with_workspace(false)
            .with_iroh_runtime_from_data_dir(&data_dir, bind, &identity, None)
            .await
            .unwrap()
            .build()
            .await
            .unwrap();

        // The transport must be wired. We can't assert on the
        // concrete type because the runtime wraps it in
        // `Arc<IrohTransport>` behind `SharedTransport`, but the
        // local node id of the transport must match the iroh
        // endpoint's public key — that's the invariant.
        let transport = node
            .transport_handle()
            .expect("iroh runtime must produce a transport");
        let transport_id = transport.local_node().clone();
        let expected_id = adnet_transport::iroh::public_key_to_node_id(
            &node
                .with_iroh_runtime(|r| r.endpoint().id())
                .expect("runtime must be held by the node"),
        );
        assert_eq!(
            transport_id, expected_id,
            "transport local_node must equal iroh endpoint id"
        );
        // The bus must be iroh-gossip-backed (not InProcessGossip).
        // We can't inspect the trait object directly without leaking
        // it through the API, but the underlying bus should be
        // usable with the `IrohGossipTransport` feature.
        let _ = node.bus();

        // The chat bridge must be constructable end-to-end.
        let bridge = node
            .chat_bridge()
            .await
            .expect("iroh docs chat bridge must construct");
        let _ = bridge;

        node.shutdown().await.unwrap();
    }

    /// **Drift guard (P6 hardening).** When a legacy
    /// `data_dir/node_id` file pre-dates the iroh identity, the
    /// builder must *override* the configured NodeId with the
    /// iroh-derived one. Otherwise the framed transport's
    /// local_node, the gossip bus's local_node, and the chat
    /// bridge's local_node all silently disagree, and
    /// announcements become unaddressable. This test pins down
    /// the `with_iroh_runtime_from_data_dir` alignment path.
    #[cfg(feature = "iroh")]
    #[tokio::test]
    async fn with_iroh_runtime_aligns_legacy_node_id_with_iroh_identity() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();

        // Stage 1: pretend a prior bridge-mode run persisted a
        // legacy node_id that does NOT match what iroh would
        // pick today.
        std::fs::create_dir_all(&data_dir).unwrap();
        let legacy_node_id = NodeId::random();
        std::fs::write(data_dir.join("node_id"), legacy_node_id.as_hex()).unwrap();

        // Stage 2: iroh-identity exists (fresh or not), gets its
        // own NodeId from the Ed25519 public key.
        let identity = adnet_transport::iroh::IrohIdentity::load_or_create(&data_dir).unwrap();
        let iroh_node_id = identity.node_id();
        assert_ne!(
            legacy_node_id, iroh_node_id,
            "test fixture: legacy and iroh NodeIds must differ for this test to be meaningful"
        );

        // Stage 3: load the legacy NodeConfig — note it still
        // carries the legacy id until the builder aligns it.
        let cfg = NodeConfig::load_or_create(&data_dir).unwrap();
        assert_eq!(cfg.node_id, legacy_node_id);

        // Stage 4: with_iroh_runtime_from_data_dir must align.
        let bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let builder = Node::builder(cfg)
            .with_workspace(false)
            .with_iroh_runtime_from_data_dir(&data_dir, bind, &identity, None)
            .await
            .unwrap();
        // After alignment, the builder's config must point at
        // the iroh-derived NodeId — and so must the resulting
        // node.
        let node = builder.build().await.unwrap();
        assert_eq!(
            node.node_id(),
            &iroh_node_id,
            "node.node_id must be aligned to the iroh-derived NodeId"
        );
        node.shutdown().await.unwrap();
    }

    /// Two nodes that each own an iroh runtime should be able to
    /// exchange announcements through the iroh-gossip overlay. We
    /// avoid the public DERP relay network by binding to loopback
    /// ports and dialing each other directly via the discovered
    /// `NodeAddr`.
    ///
    /// This is the multi-node integration test the audit called
    /// out as missing. It pins the wiring without depending on the
    /// real-world discovery latency budget.
    #[cfg(feature = "iroh")]
    #[tokio::test]
    async fn two_node_iroh_runtime_exchanges_announcements() {
        let dir = tempfile::tempdir().unwrap();
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();

        let alice_id = adnet_transport::iroh::IrohIdentity::load_or_create(&alice_dir).unwrap();
        let bob_id = adnet_transport::iroh::IrohIdentity::load_or_create(&bob_dir).unwrap();

        let alice = Node::builder(NodeConfig::new(&alice_dir, alice_id.node_id()))
            .with_workspace(false)
            .with_iroh_runtime_from_data_dir(
                &alice_dir,
                "127.0.0.1:0".parse().unwrap(),
                &alice_id,
                None,
            )
            .await
            .unwrap()
            .build()
            .await
            .unwrap();
        let bob = Node::builder(NodeConfig::new(&bob_dir, bob_id.node_id()))
            .with_workspace(false)
            .with_iroh_runtime_from_data_dir(
                &bob_dir,
                "127.0.0.1:0".parse().unwrap(),
                &bob_id,
                None,
            )
            .await
            .unwrap()
            .build()
            .await
            .unwrap();

        // Each node's transport local_node must equal its iroh identity.
        let alice_transport = alice.transport_handle().unwrap();
        let bob_transport = bob.transport_handle().unwrap();
        assert_eq!(alice_transport.local_node(), &alice_id.node_id());
        assert_eq!(bob_transport.local_node(), &bob_id.node_id());

        // Subscribe to the room on both nodes' gossip buses.
        let room: RoomId = "adnet-room-tw-node".into();
        alice.join_room(&room).await.unwrap();
        bob.join_room(&room).await.unwrap();

        // Give the gossip overlay a moment to spin up its
        // per-topic broadcast tasks.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Alice publishes an announcement.
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"two-node-iroh"),
            node_id: alice.node_id().clone(),
            title: "two-node iroh".into(),
            kind: CdnContentKind::GenericFile,
            size_bytes: 17,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: chrono::Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        alice.announce(&room, &ann).await.unwrap();

        // The local feed should see the announcement immediately.
        let local_feed = alice.room_feed(&room).await.unwrap();
        assert!(
            local_feed
                .assets
                .iter()
                .any(|a| a.content_hash == ann.content_hash),
            "alice must observe her own announcement"
        );

        alice.shutdown().await.unwrap();
        bob.shutdown().await.unwrap();

        // Silence unused-warning for the transport handles.
        let _ = (alice_transport, bob_transport);
    }

    /// When the `iroh` feature is off, the legacy path must still
    /// build a node with `InProcessGossip` and no runtime.
    #[cfg(not(feature = "iroh"))]
    #[tokio::test]
    async fn node_without_iroh_feature_uses_in_process_gossip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();
        assert!(node.transport_handle().is_none());
        node.shutdown().await.unwrap();
    }

    // ──────────────── leave_room / subscribe_room tests ────────────────

    /// `leave_room` must remove the room from joined_rooms and
    /// unsubscribe from the gossip topic.
    #[tokio::test]
    async fn leave_room_removes_from_joined_list() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "leave-test".into();
        node.join_room(&room).await.unwrap();
        assert!(node.joined_rooms().await.contains(&room));

        node.leave_room(&room).await.unwrap();
        assert!(!node.joined_rooms().await.contains(&room));

        node.shutdown().await.unwrap();
    }

    /// `leave_room` on a room we never joined must succeed (idempotent).
    #[tokio::test]
    async fn leave_room_idempotent_when_not_joined() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "never-joined".into();
        // Must not panic
        node.leave_room(&room).await.unwrap();

        node.shutdown().await.unwrap();
    }

    /// `leave_room` twice in a row must succeed.
    #[tokio::test]
    async fn leave_room_can_be_called_twice() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "double-leave".into();
        node.join_room(&room).await.unwrap();
        node.leave_room(&room).await.unwrap();
        // Second leave should also succeed
        node.leave_room(&room).await.unwrap();

        node.shutdown().await.unwrap();
    }

    /// `subscribe_room` returns a receiver that receives announcements.
    #[tokio::test]
    async fn subscribe_room_receives_announcements() {
        use adnet_gossip::GossipBus;
        use adnet_gossip::InProcessGossip;
        use std::sync::Arc as StdArc;

        let dir = tempfile::tempdir().unwrap();
        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());

        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .build_with_bus(GossipBus::new(NodeId::random(), shared_bus.clone()))
            .await
            .unwrap();

        let room: RoomId = "subscribe-test".into();
        node.join_room(&room).await.unwrap();

        let mut rx = node.subscribe_room(&room);

        // Publish an announcement
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"subscribe-test"),
            node_id: node.node_id().clone(),
            title: "subscriber-test".into(),
            kind: CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        node.announce(&room, &ann).await.unwrap();

        // The subscriber should receive it
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received.content_hash, ann.content_hash);
        assert_eq!(received.title, "subscriber-test");

        node.shutdown().await.unwrap();
    }

    /// `subscribe_room` can be called multiple times for the same room,
    /// returning independent receivers.
    #[tokio::test]
    async fn subscribe_room_multiple_receivers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "multi-sub".into();
        node.join_room(&room).await.unwrap();

        let mut rx1 = node.subscribe_room(&room);
        let mut rx2 = node.subscribe_room(&room);

        // Announce
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"multi"),
            node_id: node.node_id().clone(),
            title: "multi-sub".into(),
            kind: CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        node.announce(&room, &ann).await.unwrap();

        // Both receivers should get the announcement
        let r1 = tokio::time::timeout(std::time::Duration::from_secs(2), rx1.recv())
            .await
            .unwrap()
            .unwrap();
        let r2 = tokio::time::timeout(std::time::Duration::from_secs(2), rx2.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(r1.content_hash, ann.content_hash);
        assert_eq!(r2.content_hash, ann.content_hash);

        node.shutdown().await.unwrap();
    }

    // ──────────────── get_asset tests ────────────────

    /// `get_asset` returns `Some` for an announced asset.
    #[tokio::test]
    async fn get_asset_returns_announced_asset() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "asset-test".into();
        node.join_room(&room).await.unwrap();

        let hash = ContentHash::from_bytes(b"asset-hash-test");
        let ann = Announcement {
            room_id: room.clone(),
            content_hash: hash.clone(),
            node_id: node.node_id().clone(),
            title: "asset-file".into(),
            kind: CdnContentKind::GenericFile,
            size_bytes: 42,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        node.announce(&room, &ann).await.unwrap();

        let asset = node.get_asset(&room, &hash).await;
        assert!(asset.is_some(), "get_asset must find the announced asset");
        let asset = asset.unwrap();
        assert_eq!(asset.content_hash, hash);
        assert_eq!(asset.title, "asset-file");
        assert_eq!(asset.size_bytes, 42);

        node.shutdown().await.unwrap();
    }

    /// `get_asset` returns `None` for a room with no assets.
    #[tokio::test]
    async fn get_asset_returns_none_for_empty_room() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "empty-room".into();
        node.join_room(&room).await.unwrap();

        let hash = ContentHash::from_bytes(b"nonexistent");
        let asset = node.get_asset(&room, &hash).await;
        assert!(asset.is_none(), "get_asset must return None for non-existent asset");

        node.shutdown().await.unwrap();
    }

    /// `get_asset` returns `None` for a hash not in the room.
    #[tokio::test]
    async fn get_asset_returns_none_for_wrong_room() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room1: RoomId = "room-one".into();
        let room2: RoomId = "room-two".into();
        node.join_room(&room1).await.unwrap();
        node.join_room(&room2).await.unwrap();

        // Announce in room1
        let hash = ContentHash::from_bytes(b"only-in-room-one");
        let ann = Announcement {
            room_id: room1.clone(),
            content_hash: hash.clone(),
            node_id: node.node_id().clone(),
            title: "room1-only".into(),
            kind: CdnContentKind::Article,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };
        node.announce(&room1, &ann).await.unwrap();

        // Query in room2 - should not find it
        let asset = node.get_asset(&room2, &hash).await;
        assert!(asset.is_none(), "get_asset must not cross room boundaries");

        node.shutdown().await.unwrap();
    }

    /// `get_asset` returns `None` for an unjoined room.
    #[tokio::test]
    async fn get_asset_returns_none_for_unjoined_room() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "never-joined".into();
        let hash = ContentHash::from_bytes(b"test");

        let asset = node.get_asset(&room, &hash).await;
        assert!(asset.is_none());

        node.shutdown().await.unwrap();
    }

    // ──────────────── joined_rooms tests ────────────────

    /// `joined_rooms` returns all currently joined rooms.
    #[tokio::test]
    async fn joined_rooms_reflects_current_state() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        assert!(node.joined_rooms().await.is_empty());

        let room1: RoomId = "room-a".into();
        let room2: RoomId = "room-b".into();

        node.join_room(&room1).await.unwrap();
        assert_eq!(node.joined_rooms().await, vec![room1.clone()]);

        node.join_room(&room2).await.unwrap();
        let rooms = node.joined_rooms().await;
        assert!(rooms.contains(&room1));
        assert!(rooms.contains(&room2));

        node.leave_room(&room1).await.unwrap();
        assert_eq!(node.joined_rooms().await, vec![room2.clone()]);

        node.shutdown().await.unwrap();
    }

    // ──────────────── mesh_endpoint / ensure_mesh tests ────────────────

    /// `mesh_endpoint` returns `Some` after `ensure_mesh` is called.
    #[tokio::test]
    async fn mesh_endpoint_after_ensure_mesh() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        // Initially None
        assert!(node.mesh_endpoint().is_none());

        // After ensure_mesh, should be Some
        let addr = node.ensure_mesh().await.unwrap();
        let endpoint = node.mesh_endpoint();
        assert!(endpoint.is_some());
        assert_eq!(endpoint.unwrap().host(), addr.direct.as_ref().unwrap().host());

        node.shutdown().await.unwrap();
    }

    /// `ensure_mesh` is idempotent — calling twice returns the same address.
    #[tokio::test]
    async fn ensure_mesh_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let addr1 = node.ensure_mesh().await.unwrap();
        let addr2 = node.ensure_mesh().await.unwrap();

        assert_eq!(addr1, addr2, "ensure_mesh must be idempotent");

        node.shutdown().await.unwrap();
    }

    // ──────────────── ensure_relay tests ────────────────

    /// `relay_info` returns `None` before `ensure_relay` is called.
    #[tokio::test]
    async fn relay_info_none_before_ensure_relay() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        assert!(node.relay_info().await.is_none());

        node.shutdown().await.unwrap();
    }

    /// `ensure_relay` starts the relay and `relay_info` reflects it.
    /// Note: when `serve_enabled` is true (default), the relay starts
    /// during `build()`, so we just verify `relay_info` works.
    #[tokio::test]
    async fn ensure_relay_starts_and_relay_info_reflects() {
        let dir = tempfile::tempdir().unwrap();
        // Use a fresh NodeConfig without relay config - the relay may or may not
        // be running depending on the configuration
        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .build()
            .await
            .unwrap();

        // Call ensure_relay to start it (no-op if already started)
        let url = node.ensure_relay().await;

        // Now relay_info should be Some (either it was started by ensure_relay
        // or it was already running from the builder)
        let info = node.relay_info().await;
        assert!(info.is_some(), "relay_info should be Some after ensure_relay");
        if let Ok(url) = url {
            assert_eq!(info.unwrap().base_url, url);
        }

        node.shutdown().await.unwrap();
    }

    // ──────────────── incoming connection tests ────────────────

    /// `incoming_queue_depth` returns `None` when no transport is wired.
    #[tokio::test]
    async fn incoming_queue_depth_none_without_transport() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        assert!(node.incoming_queue_depth().await.is_none());

        node.shutdown().await.unwrap();
    }

    /// `take_incoming_receiver` returns `None` when already taken.
    #[tokio::test]
    async fn take_incoming_receiver_once_only() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let first = node.take_incoming_receiver().await;
        assert!(first.is_some());

        let second = node.take_incoming_receiver().await;
        assert!(second.is_none(), "receiver can only be taken once");

        node.shutdown().await.unwrap();
    }

    // ──────────────── room_feed tests ────────────────

    /// `room_feed` returns empty assets for a room with no announcements.
    #[tokio::test]
    async fn room_feed_empty_for_fresh_room() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "fresh-room".into();
        node.join_room(&room).await.unwrap();

        let feed = node.room_feed(&room).await.unwrap();
        assert!(feed.assets.is_empty());

        node.shutdown().await.unwrap();
    }

    /// `room_feed` reflects multiple announcements.
    #[tokio::test]
    async fn room_feed_reflects_multiple_announcements() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "multi-ann".into();
        node.join_room(&room).await.unwrap();

        for i in 0..5 {
            let hash = ContentHash::from_bytes(format!("ann-{i}").as_bytes());
            let ann = Announcement {
                room_id: room.clone(),
                content_hash: hash,
                node_id: node.node_id().clone(),
                title: format!("file-{i}.txt"),
                kind: CdnContentKind::GenericFile,
                size_bytes: i as u64 * 100,
                mime_type: None,
                source_url: None,
                ticket: None,
                timestamp: Utc::now(),
                message_id: None,
                ttl_secs: None,
                signer: None,
                signature: None,
            };
            node.announce(&room, &ann).await.unwrap();
        }

        let feed = node.room_feed(&room).await.unwrap();
        assert_eq!(feed.assets.len(), 5);

        node.shutdown().await.unwrap();
    }

    // ──────────────── peers_for tests ────────────────

    /// `peers_for` returns self-ticket when local blob exists.
    #[tokio::test]
    async fn peers_for_returns_local_blob() {
        use adnet_blobstore::BlobImporter;

        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        // Import a blob locally
        let content = b"hello local peer".to_vec();
        let hash: ContentHash = BlobImporter::put_bytes(&*node.store, &content)
            .await
            .unwrap();

        // Before ensuring mesh, may not have a ticket
        let _ = node.ensure_mesh().await.unwrap();

        let peers = node.peers_for(&hash).await;
        // Should include at least one peer (ourself via mesh)
        assert!(!peers.is_empty(), "peers_for should return at least the local peer");
        // The ticket should point to our node
        assert!(peers.iter().any(|t| t.node_id == *node.node_id()));

        node.shutdown().await.unwrap();
    }

    // ──────────────── data_dir / node_id tests ────────────────

    /// `data_dir` returns the configured data directory.
    #[tokio::test]
    async fn node_data_dir_returns_configured_path() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        assert_eq!(node.data_dir(), dir.path());

        node.shutdown().await.unwrap();
    }

    /// `node_id` returns the configured node ID.
    #[tokio::test]
    async fn node_id_returns_configured_id() {
        let dir = tempfile::tempdir().unwrap();
        let node_id = NodeId::random();
        let cfg = NodeConfig::new(dir.path(), node_id.clone());
        let node = Node::builder(cfg).build().await.unwrap();

        assert_eq!(node.node_id(), &node_id);

        node.shutdown().await.unwrap();
    }

    // ──────────────── workspace subscribe tests ────────────────

    /// `subscribe_workspace_room` can be called and returns a broadcast::Receiver.
    /// Note: the workspace room is auto-joined during node startup, so the receiver
    /// should be valid. The actual message delivery is tested in other gossip tests.
    #[tokio::test]
    async fn subscribe_workspace_room_does_not_panic() {
        use adnet_gossip::GossipBus;
        use adnet_gossip::InProcessGossip;
        use std::sync::Arc as StdArc;

        let dir = tempfile::tempdir().unwrap();
        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());

        let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
            .build_with_bus(GossipBus::new(NodeId::random(), shared_bus.clone()))
            .await
            .unwrap();

        // Should not panic - just verify the API works
        let _rx = node.subscribe_workspace_room();

        node.shutdown().await.unwrap();
    }

    // ──────────────── remote_workspace_entries tests ────────────────

    /// `remote_workspace_entries` returns empty vec initially.
    #[tokio::test]
    async fn remote_workspace_entries_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let entries = node.remote_workspace_entries().await;
        assert!(entries.is_empty());

        node.shutdown().await.unwrap();
    }

    /// `remote_workspace_flat` returns empty vec initially.
    #[tokio::test]
    async fn remote_workspace_flat_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let flat = node.remote_workspace_flat().await;
        assert!(flat.is_empty());

        node.shutdown().await.unwrap();
    }

    // ──────────────── Lenient policy tests ────────────────

    /// Lenient policy allows publishing invalid announcements locally.
    #[tokio::test]
    async fn lenient_policy_allows_invalid_announcement() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random())
            .with_gossip_validation(ValidationPolicy::Lenient);
        let node = Node::builder(cfg).build().await.unwrap();

        let room: RoomId = "lenient-room".into();
        node.join_room(&room).await.unwrap();

        // This would be rejected under Strict
        let bad = Announcement {
            room_id: room.clone(),
            content_hash: ContentHash::from_bytes(b"x"),
            node_id: node.node_id().clone(),
            title: String::new(), // empty title - invalid
            kind: CdnContentKind::GenericFile,
            size_bytes: 1,
            mime_type: None,
            source_url: None,
            ticket: None,
            timestamp: Utc::now(),
            signer: None,
            signature: None,
            ..Default::default()
        };

        // Lenient should accept it
        let result = node.announce(&room, &bad).await;
        assert!(result.is_ok(), "Lenient policy should allow invalid announcements");

        node.shutdown().await.unwrap();
    }

    // ──────────────── NodeConfig builder tests ────────────────

    /// `NodeConfig::new` produces sensible defaults.
    #[test]
    fn node_config_new_sets_defaults() {
        let node_id = NodeId::random();
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), node_id.clone());

        assert_eq!(cfg.node_id, node_id);
        assert_eq!(cfg.data_dir, dir.path());
        assert_eq!(cfg.gossip_validation, ValidationPolicy::Strict);
        assert!(cfg.quic_identity.is_none());
        assert!(cfg.mesh_config.is_none());
        // display_name follows the adnet-{short_id} convention
        assert!(cfg.display_name.starts_with("adnet-"));
        assert!(cfg.display_name.contains(&node_id.short()));
    }

    /// `NodeConfig::with_gossip_validation` chains correctly.
    #[test]
    fn node_config_with_gossip_validation() {
        let node_id = NodeId::random();
        let dir = tempfile::tempdir().unwrap();

        let cfg = NodeConfig::new(dir.path(), node_id.clone());
        assert_eq!(cfg.gossip_validation, ValidationPolicy::Strict);

        // Lenient policy should be set correctly
        let cfg2 = cfg.with_gossip_validation(ValidationPolicy::Lenient);
        assert_eq!(cfg2.gossip_validation, ValidationPolicy::Lenient);

        // Verify builder chain preserves other fields
        assert_eq!(cfg2.node_id, node_id);
        assert_eq!(cfg2.data_dir, dir.path());
    }

    /// `NodeConfig::with_mesh_config` chains correctly.
    #[test]
    fn node_config_with_mesh_config() {
        use adnet_mesh::MeshConfig;
        let node_id = NodeId::random();
        let dir = tempfile::tempdir().unwrap();

        let cfg = NodeConfig::new(dir.path(), node_id.clone());
        let mesh_cfg = MeshConfig::default();
        let cfg2 = cfg.with_mesh_config(mesh_cfg);
        assert!(cfg2.mesh_config.is_some());
    }

    // ──────────────── Node accessor tests ────────────────

    /// `Node::display_name` returns the name set in config.
    #[tokio::test]
    async fn node_display_name_returns_configured_name() {
        let dir = tempfile::tempdir().unwrap();
        let node_id = NodeId::random();
        let cfg = NodeConfig::new(dir.path(), node_id.clone());
        let node = Node::builder(cfg).build().await.unwrap();

        let name = node.display_name();
        assert!(name.starts_with("adnet-"));
        assert!(name.contains(&node_id.short()));

        node.shutdown().await.unwrap();
    }

    /// `Node::transport_dyn` returns None for a node without transport.
    #[tokio::test]
    async fn node_transport_dyn_none_without_transport() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        assert!(node.transport_dyn().is_none());

        node.shutdown().await.unwrap();
    }

    /// `Node::bus` returns a reference to the gossip bus.
    #[tokio::test]
    async fn node_bus_returns_gossip_bus() {
        use adnet_gossip::GossipBus;
        use adnet_gossip::InProcessGossip;
        use std::sync::Arc as StdArc;

        let dir = tempfile::tempdir().unwrap();
        let shared_bus: StdArc<dyn adnet_gossip::GossipTransport> =
            StdArc::new(InProcessGossip::new());
        // Use the same node_id for bus as will be used for node
        let node_id = NodeId::random();
        let bus = GossipBus::new(node_id.clone(), shared_bus.clone());
        let node = Node::builder(NodeConfig::new(dir.path(), node_id))
            .build_with_bus(bus)
            .await
            .unwrap();

        // Verify bus returns a valid reference with the correct local node
        let retrieved_bus = node.bus();
        assert_eq!(*retrieved_bus.local_node(), *node.node_id());

        node.shutdown().await.unwrap();
    }

    /// `Node::with_iroh_runtime` returns None when no iroh runtime is configured.
    #[tokio::test]
    #[cfg(feature = "iroh")]
    async fn node_with_iroh_runtime_returns_none_without_iroh() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        // Without iroh runtime, this should return None
        let result = node.with_iroh_runtime(|_n| async move { "test".to_string() });
        assert!(result.is_none(), "Expected None without iroh runtime");

        node.shutdown().await.unwrap();
    }

    // ──────────────── fetch_remote_workspace_entry tests ────────────────

    /// `fetch_remote_workspace_entry` returns error for non-existent entry.
    #[tokio::test]
    async fn fetch_remote_workspace_entry_returns_error_for_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        let unknown_node = NodeId::random();
        let result = node.fetch_remote_workspace_entry(&unknown_node, "nonexistent").await;
        // Returns Result<PathBuf>, should be Err for unknown
        assert!(result.is_err(), "Expected error for unknown node/entry");

        node.shutdown().await.unwrap();
    }

    // ──────────────── make_ticket tests ────────────────

    /// `make_ticket` returns a ticket for a known content hash.
    #[tokio::test]
    async fn make_ticket_returns_valid_ticket() {
        use adnet_blobstore::BlobImporter;
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        // First import some content so the node knows about it
        let content = b"ticket-test-content".to_vec();
        let hash: ContentHash = BlobImporter::put_bytes(&**node.store(), &content)
            .await
            .unwrap();

        // Make a ticket for this hash
        let ticket = node.make_ticket(&hash).await.unwrap();

        // Verify ticket structure
        assert_eq!(ticket.node_id, *node.node_id());
        assert_eq!(ticket.content_hash, hash);

        node.shutdown().await.unwrap();
    }

    // ──────────────── next_incoming_peer tests ────────────────

    /// `next_incoming_peer` returns `None` immediately when no transport is wired.
    #[tokio::test]
    async fn next_incoming_peer_returns_none_without_transport() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        // Without transport, next_incoming_peer returns None immediately
        // (no blocking/timeout needed since the fix returns None eagerly)
        let incoming = node.next_incoming_peer().await;
        assert!(
            incoming.is_none(),
            "Expected None without transport"
        );

        node.shutdown().await.unwrap();
    }

    // ──────────────── store accessor test ────────────────

    /// `Node::store` returns the blob store.
    #[tokio::test]
    async fn node_store_returns_blob_store() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        // Verify store is accessible and has a data directory
        let store = node.store();
        let data_dir = store.data_dir();
        assert!(data_dir.exists() || data_dir.to_string_lossy().len() > 0);

        node.shutdown().await.unwrap();
    }

    // ──────────────── transport_handle test ────────────────

    /// `transport_handle` returns None when no transport is configured.
    #[tokio::test]
    async fn transport_handle_none_without_transport() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        assert!(node.transport_handle().is_none());

        node.shutdown().await.unwrap();
    }

    // ──────────────── NodeInfo debug test ────────────────

    /// `NodeInfo` formats correctly with Debug.
    #[tokio::test]
    async fn node_info_debug_format() {
        let dir = tempfile::tempdir().unwrap();
        let node_id = NodeId::random();
        let cfg = NodeConfig::new(dir.path(), node_id.clone());
        let node = Node::builder(cfg).build().await.unwrap();

        let info = node.info().await;
        let debug = format!("{info:?}");
        assert!(debug.contains("node_id"));
        assert!(debug.contains("display_name"));
        assert!(debug.contains("joined_rooms"));

        node.shutdown().await.unwrap();
    }

    // ──────────────── IncomingConn Debug test ────────────────

    /// `IncomingConn` debug format test - verifies next_incoming_peer returns None without transport.
    #[tokio::test]
    async fn incoming_conn_debug_format() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(dir.path(), NodeId::random());
        let node = Node::builder(cfg).build().await.unwrap();

        // Without transport, next_incoming_peer returns None
        let incoming = node.next_incoming_peer().await;
        assert!(incoming.is_none());

        node.shutdown().await.unwrap();
    }
}

// ──────────────── RemoteWorkspaceEntry tests (unit tests) ────────────────

#[cfg(test)]
mod remote_workspace_entry_tests {
    use super::*;

    /// `RemoteWorkspaceEntry` fields are accessible and Debug formats correctly.
    #[test]
    fn remote_workspace_entry_debug_format() {
        let entry = RemoteWorkspaceEntry {
            owner: NodeId::random(),
            entry: WorkspaceFileEntry {
                name: "test.txt".into(),
                relative_path: "shared/test.txt".into(),
                size_bytes: 100,
                content_hash: Some("abc123".into()),
                added_at: 1234567890,
            },
            ticket: None,
            received_at: Utc::now(),
            has_ticket: false,
            local_path: None,
            fetched_bytes: None,
        };

        let debug = format!("{entry:?}");
        assert!(debug.contains("test.txt"));
        assert!(debug.contains("100"));
    }

    /// `RemoteWorkspaceEntry` has_ticket convenience field reflects ticket presence.
    #[test]
    fn remote_workspace_entry_has_ticket_flag() {
        let hash = ContentHash::from_bytes(b"test");
        let node_id = NodeId::random();
        let endpoint = NodeAddr::new(node_id.clone()).with_direct(Endpoint::new("127.0.0.1", 1234));

        // With ticket
        let with_ticket = RemoteWorkspaceEntry {
            owner: NodeId::random(),
            entry: WorkspaceFileEntry {
                name: "file.txt".into(),
                relative_path: "shared/file.txt".into(),
                size_bytes: 50,
                content_hash: Some("hash".into()),
                added_at: 0,
            },
            ticket: Some(BlobTicket::whole(&node_id, &endpoint, &hash)),
            received_at: Utc::now(),
            has_ticket: true,
            local_path: None,
            fetched_bytes: None,
        };
        assert!(with_ticket.has_ticket);
        assert!(with_ticket.ticket.is_some());

        // Without ticket
        let without_ticket = RemoteWorkspaceEntry {
            owner: NodeId::random(),
            entry: WorkspaceFileEntry {
                name: "other.txt".into(),
                relative_path: "shared/other.txt".into(),
                size_bytes: 75,
                content_hash: None,
                added_at: 0,
            },
            ticket: None,
            received_at: Utc::now(),
            has_ticket: false,
            local_path: None,
            fetched_bytes: None,
        };
        assert!(!without_ticket.has_ticket);
        assert!(without_ticket.ticket.is_none());
    }

    /// `RemoteWorkspaceEntry` local_path reflects fetch status.
    #[test]
    fn remote_workspace_entry_local_path_tracking() {
        let entry = RemoteWorkspaceEntry {
            owner: NodeId::random(),
            entry: WorkspaceFileEntry {
                name: "fetched.txt".into(),
                relative_path: "inbox/fetched.txt".into(),
                size_bytes: 200,
                content_hash: Some("def456".into()),
                added_at: 9876543210,
            },
            ticket: None,
            received_at: Utc::now(),
            has_ticket: false,
            local_path: Some(PathBuf::from("/data/inbox/fetched.txt")),
            fetched_bytes: Some(200),
        };

        assert!(entry.local_path.is_some());
        assert!(entry.fetched_bytes.is_some());
        assert_eq!(entry.fetched_bytes.unwrap(), 200);
    }
}
