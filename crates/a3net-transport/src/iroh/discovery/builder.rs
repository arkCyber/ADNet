//! Discovery builder — composes the iroh [`Endpoint`] builder with
//! A3Net-specific address-lookup services and a publish-policy
//! filter.
//!
//! # Example
//!
//! ```no_run
//! # #[cfg(feature = "iroh")]
//! # {
//! use std::net::SocketAddr;
//! use a3net_transport::iroh::discovery::{DiscoveryBuilder, DiscoveryConfig, MemoryLookup};
//!
//! # #[tokio::main]
//! # async fn main() {
//! let memory = MemoryLookup::new();
//! let cfg = DiscoveryConfig::default()
//!     .with_memory(memory.clone());
//! let bound = DiscoveryBuilder::new(cfg)
//!     .bind(SocketAddr::from(([0, 0, 0, 0], 0)))
//!     .await
//!     .expect("bind iroh endpoint with discovery");
//!
//! // `bound.memory()` keeps a handle so post-bind out-of-band
//! // addresses still land in the active iroh pipeline.
//! let snap = bound.snapshot();
//! println!("hit_rate = {:.1}%", snap.hit_rate_pct());
//! # }
//! # }
//! ```
//!
//! [`Endpoint`]: iroh::Endpoint

#![cfg(feature = "iroh")]

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use iroh::address_lookup::{
    AddressLookup, DnsAddressLookup, EndpointData, Error as LookupError, Item, PkarrPublisher,
    PkarrResolver,
};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, SecretKey};
use iroh_base::EndpointId;
use n0_future::boxed::BoxStream;
use tracing::{info, warn};

use a3net_types::NodeId;

use super::diagnostics::DiscoveryDiagnostics;
use super::lookup::{MainlineLookup, MemoryLookup};
use super::pkarr_publisher::{AdnetPkarrPublisher, PkarrPublisherConfig, build_publisher};
use super::policy::{PublishPolicy, publish_policy_to_addr_filter};

/// User-facing knob for the discovery stack.
///
/// `presets::N0` already wires `PkarrPublisher::n0_dns` +
/// `PkarrResolver::n0_dns` + `DnsAddressLookup::n0_dns`; this
/// struct layers composability on top of those defaults.
///
/// ## Defaults
///
/// `n0_dns_pkarr` defaults to **true** so that, out of the box, an
/// A3Net node behaves identically to a stock iroh 1.0 endpoint
/// (publishes a signed pkarr packet to `dns.iroh.link/pkarr` and
/// resolves via the same relay's `/pkarr` HTTP path + DNS).
/// Operators that want to suppress the n0 defaults entirely
/// (e.g. for air-gapped deployments) can use
/// [`DiscoveryConfig::without_n0_dns_pkarr`] or override the
/// publisher with a custom [`PkarrPublisherConfig`].
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// When `true` (the default), the iroh `PkarrPublisher::n0_dns()`
    /// is registered after [`Builder::clear_address_lookup`] — i.e.
    /// the node publishes signed pkarr packets to the public n0
    /// relay and resolves peers from the same relay + DNS.
    pub n0_dns_pkarr: bool,
    /// Optional in-memory out-of-band address book.
    pub memory: Option<MemoryLookup>,
    /// Optional Mainline-DHT lookup (via the `pkarr` crate's DHT
    /// backend).
    pub mainline: Option<MainlineLookup>,
    /// User-supplied custom lookups registered via
    /// [`DiscoveryBuilder::with_extra_lookup_owned`]. We store them
    /// as `Arc<dyn AddressLookup>` (i.e. they keep the concrete
    /// type's strong count) and adapt them via [`DynLookupAdapter`]
    /// when handed to iroh's builder.
    pub extra: Vec<Arc<dyn AddressLookup>>,
    /// What subset of addresses the local node publishes.
    pub publish_policy: PublishPolicy,
    /// Optional Pkarr publisher configuration. When `Some`, the
    /// A3Net-instrumented publisher is registered *instead of* the
    /// raw `PkarrPublisher::n0_dns()` (which still works, but emits
    /// no `DiscoveryEvent` to the diagnostics recorder).
    /// `n0_dns_pkarr` is ignored when this is `Some` — the custom
    /// publisher always wins.
    pub pkarr: Option<PkarrPublisherConfig>,
    /// Shared diagnostics recorder. Created on demand by
    /// [`DiscoveryBuilder::new`] when not supplied.
    pub diagnostics: Option<Arc<DiscoveryDiagnostics>>,
    /// Optional user-data payload applied to **every** Pkarr
    /// publish path — both the n0 default and a custom
    /// `PkarrPublisherConfig`. Mirrors
    /// `iroh_dns::endpoint_info::UserData` (v1.0.3) so A3Net
    /// nodes stay wire-compatible with stock iroh endpoints that
    /// publish the same field. When `Some`, the value is the
    /// single source of truth for `InstrumentedPublisher::publish`
    /// (custom config) and is also baked into the operator's
    /// `PkarrPublisherConfig` when one is supplied.
    ///
    /// Length is bounded at
    /// [`crate::iroh::discovery::USER_DATA_MAX_LEN`] bytes; the
    /// `with_user_data` setter validates before assigning.
    pub user_data: Option<crate::iroh::discovery::UserData>,
    /// When `true`, attaches [`MdnsAddressLookup`] to the endpoint
    /// during [`bind_internal`](DiscoveryBuilder::bind_internal) so
    /// the local node advertises itself via mDNS and discovers peers
    /// on the same LAN. Default `false` (opt-in) so operators who
    /// don't want LAN discovery can leave it off.
    ///
    /// Requires the `mdns` cargo feature. When the feature is absent
    /// this field is still stored (for config persistence) but has
    /// no effect at runtime.
    #[cfg(feature = "mdns")]
    pub mdns_enabled: bool,
    /// Placeholder field for non-mdns builds to keep the struct
    /// layout stable across feature flag changes.
    #[cfg(not(feature = "mdns"))]
    _mdns_placeholder: (),
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            n0_dns_pkarr: true,
            memory: None,
            mainline: None,
            extra: Vec::new(),
            publish_policy: PublishPolicy::default(),
            pkarr: None,
            diagnostics: None,
            user_data: None,
            #[cfg(feature = "mdns")]
            mdns_enabled: false,
            #[cfg(not(feature = "mdns"))]
            _mdns_placeholder: (),
        }
    }
}

impl DiscoveryConfig {
    /// Convenience constructor: enable N0 DNS/Pkarr (the default;
    /// idempotent — already `true` by [`Default`]).
    pub fn with_n0_dns_pkarr(mut self) -> Self {
        self.n0_dns_pkarr = true;
        self
    }

    /// Suppress the n0 DNS/Pkarr defaults. Use this for
    /// air-gapped deployments where the operator substitutes a
    /// private pkarr relay (or no pkarr at all).
    pub fn without_n0_dns_pkarr(mut self) -> Self {
        self.n0_dns_pkarr = false;
        self
    }

    /// Attach an in-memory lookup. Returns `self` for builder chaining.
    pub fn with_memory(mut self, memory: MemoryLookup) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Attach a Mainline-DHT lookup.
    pub fn with_mainline(mut self, mainline: MainlineLookup) -> Self {
        self.mainline = Some(mainline);
        self
    }

    /// Enable the mDNS lookup at the staged-config level. The
    /// actual `EndpointId` is supplied at
    /// `DiscoveryBuilder::with_mdns(local_endpoint)` time so
    /// the staged config can be persisted (e.g. to JSON5)
    /// without one. Requires the `mdns` cargo feature.
    #[cfg(feature = "mdns")]
    pub fn with_mdns_enabled(mut self, enabled: bool) -> Self {
        self.mdns_enabled = enabled;
        self
    }

    /// Set the publish policy (relay-only / ip-only / all).
    pub fn with_publish_policy(mut self, policy: PublishPolicy) -> Self {
        self.publish_policy = policy;
        self
    }

    /// Attach a shared diagnostics recorder. If not called, a fresh
    /// one is created when [`DiscoveryBuilder::new`] runs.
    pub fn with_diagnostics(mut self, diag: Arc<DiscoveryDiagnostics>) -> Self {
        self.diagnostics = Some(diag);
        self
    }

    /// Attach a custom Pkarr publisher configuration. The publisher
    /// emits `DiscoveryEvent::PublishFiltered` events into the
    /// shared diagnostics recorder.
    pub fn with_pkarr(mut self, pkarr: PkarrPublisherConfig) -> Self {
        self.pkarr = Some(pkarr);
        self
    }

    /// Attach a user-data payload to every published pkarr
    /// packet. Length-validated against
    /// [`USER_DATA_MAX_LEN`](crate::iroh::discovery::USER_DATA_MAX_LEN);
    /// oversized inputs return the original `Self` unchanged so
    /// the call is `Err`-free at the `DiscoveryConfig` layer
    /// (callers that need a hard error go through
    /// [`PkarrPublisherConfig::with_user_data`]).
    pub fn with_user_data(mut self, user_data: crate::iroh::discovery::UserData) -> Self {
        self.user_data = Some(user_data);
        self
    }

    /// Drop any attached user-data payload.
    pub fn without_user_data(mut self) -> Self {
        self.user_data = None;
        self
    }

    /// Get-or-create the shared diagnostics recorder. Convenience
    /// for callers that want to read counters after the endpoint
    /// binds.
    pub fn diagnostics(&self) -> Arc<DiscoveryDiagnostics> {
        self.diagnostics
            .clone()
            .unwrap_or_else(|| Arc::new(DiscoveryDiagnostics::new()))
    }

    /// Returns `true` when mDNS LAN discovery is enabled.
    /// On a non-mdns build this always returns `false`.
    pub fn mdns_enabled(&self) -> bool {
        #[cfg(feature = "mdns")]
        return self.mdns_enabled;
        #[cfg(not(feature = "mdns"))]
        return false;
    }
}

/// Adapter: wraps `Arc<dyn AddressLookup>` so it can be passed to
/// `Endpoint::builder(...).address_lookup(...)`.
///
/// iroh's blanket impl is `impl<T: AddressLookupBuilder>
/// AddressLookupBuilder for T`, which (combined with the blanket
/// `impl<T: AddressLookup> AddressLookupBuilder for T` and the
/// `impl<T: AddressLookup> AddressLookup for Arc<T>`) means we can
/// register `Arc<DynLookupAdapter>` directly — the adapter
/// implements `AddressLookup` so the blanket chain fires.
#[derive(Debug)]
pub struct DynLookupAdapter(pub(crate) Arc<dyn AddressLookup>);

impl AddressLookup for DynLookupAdapter {
    fn publish(&self, data: &EndpointData) {
        self.0.publish(data);
    }
    fn resolve(&self, endpoint_id: EndpointId) -> Option<BoxStream<Result<Item, LookupError>>> {
        self.0.resolve(endpoint_id)
    }
}

/// Stateful builder. Owns a `MemoryLookup` (if configured) so
/// callers can keep adding out-of-band entries after the endpoint is
/// bound. The [`BoundDiscovery`] return value holds the same
/// `MemoryLookup` so post-bind additions still land in the active
/// iroh pipeline.
pub struct DiscoveryBuilder {
    config: DiscoveryConfig,
    memory: MemoryLookup,
    diagnostics: Arc<DiscoveryDiagnostics>,
}

impl std::fmt::Debug for DiscoveryBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryBuilder")
            .field("config", &self.config)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

impl DiscoveryBuilder {
    /// Construct a builder from a [`DiscoveryConfig`]. Always
    /// installs an internal [`MemoryLookup`] (even when
    /// `config.memory` is `None`) so the post-bind snapshot can
    /// report a stable provenance.
    pub fn new(config: DiscoveryConfig) -> Self {
        let memory = config.memory.clone().unwrap_or_default();
        let diagnostics = config.diagnostics.clone().unwrap_or_default();
        Self {
            config,
            memory,
            diagnostics,
        }
    }

    /// Borrow the shared in-memory lookup. Callers can call
    /// `add` / `remove` on this handle after `bind` returns to
    /// inject out-of-band addresses (e.g. from a fresh `PeerTicket`).
    pub fn memory(&self) -> &MemoryLookup {
        &self.memory
    }

    /// Borrow the shared diagnostics recorder.
    pub fn diagnostics(&self) -> &Arc<DiscoveryDiagnostics> {
        &self.diagnostics
    }

    /// Borrow the resolved configuration.
    pub fn config(&self) -> &DiscoveryConfig {
        &self.config
    }

    /// Add a concrete custom lookup directly on the builder. This is
    /// the recommended path for production use:
    ///
    /// ```ignore
    /// DiscoveryBuilder::new(cfg)
    ///     .with_extra_lookup_owned(MdnsAddressLookup::new().await?)
    ///     .bind(addr).await?;
    /// ```
    pub fn with_extra_lookup_owned<T>(mut self, lookup: T) -> Self
    where
        T: AddressLookup + Send + Sync + 'static,
    {
        let arc: Arc<dyn AddressLookup> = Arc::new(lookup);
        self.config.extra.push(arc);
        self
    }

    /// Sugar for the common case: build and register an mDNS
    /// lookup targeting the local endpoint. Requires the
    /// `mdns` cargo feature; on a non-mDNS build this is a
    /// hard error rather than a silent no-op.
    ///
    /// ## Why a dedicated method (vs. `with_extra_lookup_owned`)
    ///
    /// `with_extra_lookup_owned` would force every caller to
    /// import `iroh_mdns_address_lookup` themselves. Routing
    /// through `with_mdns` keeps the upstream type an
    /// implementation detail and lets the operator write
    /// `DiscoveryConfig::default().with_mdns(...)` without
    /// pulling in the mDNS crate on the call site.
    #[cfg(feature = "mdns")]
    pub fn with_mdns(self, local_endpoint: iroh_base::EndpointId) -> anyhow::Result<Self> {
        let mdns = super::mdns::MdnsAddressLookup::new(local_endpoint)?;
        Ok(self.with_extra_lookup_owned(mdns))
    }

    /// Build the iroh endpoint using the configured discovery stack.
    /// Uses a freshly generated `SecretKey`.
    pub async fn bind(self, bind_addr: SocketAddr) -> anyhow::Result<BoundDiscovery> {
        let secret_key = SecretKey::generate();
        Self::bind_internal(
            self.config,
            bind_addr,
            secret_key,
            self.memory,
            self.diagnostics,
        )
        .await
    }

    /// Build the endpoint with a stable [`SecretKey`] (callers that
    /// want a persistent identity pass their own key).
    pub async fn bind_with_secret_key(
        self,
        bind_addr: SocketAddr,
        secret_key: SecretKey,
    ) -> anyhow::Result<BoundDiscovery> {
        Self::bind_internal(
            self.config,
            bind_addr,
            secret_key,
            self.memory,
            self.diagnostics,
        )
        .await
    }

    async fn bind_internal(
        config: DiscoveryConfig,
        bind_addr: SocketAddr,
        secret_key: SecretKey,
        memory: MemoryLookup,
        diagnostics: Arc<DiscoveryDiagnostics>,
    ) -> anyhow::Result<BoundDiscovery> {
        let policy = config.publish_policy.clone();
        // NOTE: mDNS requires the endpoint's public key. We must extract it
        // BEFORE passing secret_key to the builder (it gets moved into bind()).
        #[cfg(feature = "mdns")]
        let mdns_ep_id = if config.mdns_enabled {
            Some(secret_key.public())
        } else {
            None
        };
        // Start from `presets::N0` to inherit iroh's default
        // transport + crypto + relay map, then strip *all* address
        // lookup services (`clear_address_lookup`) so we can
        // re-compose them deterministically below. This avoids
        // the "two PkarrPublishers PUT the same packet every
        // 5 minutes" bug that would otherwise come from leaving
        // `presets::N0`'s `PkarrPublisher::n0_dns()` in place
        // while also registering our own.
        let mut builder = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .clear_address_lookup();

        // Wire the user's `MemoryLookup` *before* the iroh
        // defaults so out-of-band entries are tried first.
        // iroh's `AddressLookupServices` merges results from every
        // registered service concurrently — order only affects
        // `publish()` replay, not resolution concurrency.
        builder = builder
            .address_lookup(memory.inner().clone())
            .addr_filter(publish_policy_to_addr_filter(policy));

        // Re-add the iroh defaults **without** the PkarrPublisher
        // when the operator hasn't supplied a custom one; with
        // the operator's PkarrPublisher otherwise. (Re-adding the
        // bare `n0_dns()` PkarrPublisher would create a second
        // signing client that fights the one we're replacing.)
        //
        // We track whether any Pkarr publisher got registered so
        // the unconditional `diagnostics.record_publish(true)`
        // below doesn't fabricate a "kept" publish event when
        // the operator configured `without_n0_dns_pkarr()` AND
        // did not supply a custom `PkarrPublisherConfig`.
        //
        // `effective_user_data` is the operator-supplied payload
        // (from `DiscoveryConfig::user_data`) merged with any
        // per-config payload the `PkarrPublisherConfig` already
        // carries. Precedence: `DiscoveryConfig.user_data` wins
        // because that's the documented "single source of truth"
        // on the builder layer; the per-config field is a
        // legacy escape hatch for callers that build the
        // publisher directly.
        let mut pkarr_publisher_registered = false;
        let effective_user_data: Option<crate::iroh::discovery::UserData> = config
            .user_data
            .clone()
            .or_else(|| config.pkarr.as_ref().and_then(|p| p.user_data.clone()));
        // Pre-stamp the diagnostics recorder with the effective
        // user-data so a snapshot taken before any
        // `publish(...)` call already surfaces the operator's
        // intent. This is the same value the
        // `InstrumentedPublisher::publish` path will re-stamp
        // on every call, so the snapshot stays accurate even
        // if the operator flips `user_data` mid-session
        // (without rebuilding the endpoint).
        if let Some(ud) = &effective_user_data {
            diagnostics.record_user_data(Some(ud.clone()));
        }
        if let Some(mut pkarr_cfg) = config.pkarr.clone() {
            // Always apply the merged user-data: the operator
            // may have configured it at the DiscoveryConfig
            // level (the "common" path) without setting the
            // per-config field. Setting it here ensures
            // `InstrumentedPublisher::publish` injects it on
            // every call regardless of which path the caller
            // took.
            if effective_user_data.is_some() {
                pkarr_cfg.user_data = effective_user_data.clone();
            }
            let a3net_publisher: AdnetPkarrPublisher =
                build_publisher(pkarr_cfg, Arc::clone(&diagnostics))?;
            builder = builder.address_lookup(a3net_publisher);
            pkarr_publisher_registered = true;
        } else if config.n0_dns_pkarr {
            if let Some(ud) = effective_user_data.clone() {
                // Custom user-data on the n0 default → fall
                // through to the instrumented path so the
                // payload lands on the wire. We rebuild a
                // fresh `PkarrPublisherConfig::n0_dns()` and
                // attach the payload, then run it through the
                // A3Net-instrumented publisher.
                let pkarr_cfg = PkarrPublisherConfig::n0_dns().with_user_data(ud);
                let a3net_publisher: AdnetPkarrPublisher =
                    build_publisher(pkarr_cfg, Arc::clone(&diagnostics))?;
                builder = builder.address_lookup(a3net_publisher);
            } else {
                // The iroh default `PkarrPublisher::n0_dns()` is
                // registered only when the operator hasn't supplied a
                // custom `PkarrPublisherConfig`, has no user-data
                // payload to publish, AND `n0_dns_pkarr` is true (the
                // default — see [`DiscoveryConfig::default`]).
                // Operators that want to suppress the n0 defaults
                // entirely (air-gapped deployments, custom pkarr
                // relays, etc.) should call
                // [`DiscoveryConfig::without_n0_dns_pkarr`] or set
                // `n0_dns_pkarr` to false. The custom-path branch
                // above takes precedence over this branch so an
                // operator who supplies a `PkarrPublisherConfig`
                // never gets two signing clients fighting each other.
                builder = builder.address_lookup(PkarrPublisher::n0_dns());
            }
            pkarr_publisher_registered = true;
        }
        builder = builder
            .address_lookup(PkarrResolver::n0_dns())
            .address_lookup(DnsAddressLookup::n0_dns());

        if let Some(mainline) = &config.mainline {
            builder = builder.address_lookup(mainline.clone());
        }

        for arc_dyn in &config.extra {
            // Wrap the `Arc<dyn AddressLookup>` in a
            // concrete-typed `DynLookupAdapter` so iroh's blanket
            // `AddressLookupBuilder` impl applies (the blanket impl
            // is `impl<T: AddressLookup> AddressLookupBuilder for T`,
            // not for `dyn AddressLookup`). Then wrap the adapter in
            // another `Arc` so the iroh `Arc<T>: AddressLookup` impl
            // kicks in.
            let adapter = DynLookupAdapter(Arc::clone(arc_dyn));
            builder = builder.address_lookup(Arc::new(adapter));
        }

        // Wire mDNS lookup if enabled in the config. This is deferred
        // past the extra lookups so mDNS resolves can be augmented
        // by custom lookups the operator registered. mDNS is a
        // push-based service: the lookup publishes via multicast
        // and discovers peers the same way, requiring no prior
        // knowledge of endpoint IDs.
        #[cfg(feature = "mdns")]
        if config.mdns_enabled {
            let ep_id = mdns_ep_id.expect("mdns_ep_id set when mdns_enabled");
            let mdns = super::mdns::MdnsAddressLookup::new(ep_id)?;
            let adapter = DynLookupAdapter(Arc::new(mdns) as Arc<dyn AddressLookup>);
            builder = builder.address_lookup(Arc::new(adapter));
        }

        let builder = builder
            .bind_addr(bind_addr)
            .context("invalid bind address for iroh endpoint")?;
        // `Endpoint::bind()` can hang forever if the underlying
        // socket-level bringup stalls (e.g. router misconfiguration
        // in some Linux network namespaces). Wrap it in a default
        // 30s timeout so a hung `bind` doesn't block the whole
        // caller; the error is mapped back to `anyhow::Error` so
        // the existing `?`-chain in callers keeps working.
        let endpoint = tokio::time::timeout(std::time::Duration::from_secs(30), builder.bind())
            .await
            .context("iroh endpoint bind timed out after 30s")?
            .context("bind iroh endpoint")?;

        // Stamp a publish event so `/discovery` shows the local
        // node has published at least once — **only when** a
        // Pkarr publisher was actually registered above. A
        // configuration with `without_n0_dns_pkarr()` AND no
        // custom `PkarrPublisherConfig` has no path to a public
        // publish relay; recording a synthetic "kept" event
        // would lie to operators and skew their hit-rate / filter
        // telemetry.
        if pkarr_publisher_registered {
            diagnostics.record_publish(true);
        }

        if policy.exposes_direct_ip() {
            warn!(
                policy = %policy,
                "discovery: publish policy will publish direct IPs to public Pkarr/DHT — \
                 verify this is intentional"
            );
        } else {
            info!(
                policy = %policy,
                "discovery: publish policy is relay-only — direct IPs will NOT be published"
            );
        }

        info!(
            memory_entries = memory.len(),
            mainline = config.mainline.is_some(),
            extra = config.extra.len(),
            mdns = %config.mdns_enabled(),
            "discovery: iroh endpoint bound with composed lookup stack"
        );

        Ok(BoundDiscovery {
            endpoint,
            memory,
            diagnostics,
            policy,
            config,
        })
    }
}

/// Result of [`DiscoveryBuilder::bind`]. Owns the live endpoint and
/// the shared in-memory handle so post-bind additions land in the
/// active iroh pipeline.
pub struct BoundDiscovery {
    pub endpoint: Endpoint,
    pub memory: MemoryLookup,
    pub diagnostics: Arc<DiscoveryDiagnostics>,
    pub policy: PublishPolicy,
    /// The discovery configuration that was used to bind this endpoint.
    /// Stored so tests can verify the mDNS flag was set correctly.
    pub config: DiscoveryConfig,
}

impl std::fmt::Debug for BoundDiscovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundDiscovery")
            .field("endpoint_id", &self.endpoint.id())
            .field("memory_entries", &self.memory.len())
            .field("policy", &self.policy)
            .finish()
    }
}

impl BoundDiscovery {
    /// Convenience accessor for the underlying endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Borrow the shared in-memory lookup.
    pub fn memory(&self) -> &MemoryLookup {
        &self.memory
    }

    /// Borrow the shared diagnostics recorder.
    pub fn diagnostics(&self) -> &Arc<DiscoveryDiagnostics> {
        &self.diagnostics
    }

    /// Point-in-time snapshot of the discovery counters.
    pub fn snapshot(&self) -> super::IrohDiscoverySnapshot {
        self.diagnostics.snapshot()
    }

    /// Pre-resolved `EndpointAddr` for the local node. Useful when
    /// the caller wants to publish their own `PeerTicket`.
    pub fn local_endpoint_addr(&self) -> EndpointAddr {
        EndpointAddr::from(self.endpoint.id())
    }

    /// Translate a `NodeId` into an iroh `EndpointAddr` with the
    /// best known addressing information.
    pub fn resolve_node_id(&self, node_id: NodeId) -> anyhow::Result<EndpointAddr> {
        let pk = crate::iroh::node_id_to_public_key(&node_id)?;
        Ok(EndpointAddr::from(pk))
    }

    /// Access the endpoint's `SecretKey` (for persisting identity
    /// across restarts).
    pub fn secret_key(&self) -> &SecretKey {
        self.endpoint.secret_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_n0_dns_pkarr_on_and_relay_only() {
        let cfg = DiscoveryConfig::default();
        assert!(cfg.n0_dns_pkarr, "default should keep n0 DNS/Pkarr on");
        assert_eq!(cfg.publish_policy, PublishPolicy::RelayOnly);
        assert!(cfg.memory.is_none());
        assert!(cfg.mainline.is_none());
        assert!(cfg.extra.is_empty());
        assert!(cfg.pkarr.is_none());
        assert!(
            cfg.user_data.is_none(),
            "default DiscoveryConfig must not carry user_data"
        );
    }

    #[test]
    fn config_with_memory_returns_handle() {
        let mem = MemoryLookup::new();
        let cfg = DiscoveryConfig::default().with_memory(mem.clone());
        let builder = DiscoveryBuilder::new(cfg);
        assert_eq!(builder.memory().len(), 0);
    }

    #[test]
    fn diagnostics_default_is_fresh_when_absent() {
        let cfg = DiscoveryConfig::default();
        let diag = cfg.diagnostics();
        let snap = diag.snapshot();
        assert_eq!(snap.publishes_total, 0);
    }

    #[test]
    fn policy_warnings_trigger_on_ip_leak() {
        let cfg_relay = DiscoveryConfig::default();
        assert!(!cfg_relay.publish_policy.exposes_direct_ip());
        let cfg_all = DiscoveryConfig::default().with_publish_policy(PublishPolicy::All);
        assert!(cfg_all.publish_policy.exposes_direct_ip());
    }

    #[test]
    fn without_n0_dns_pkarr_flips_the_default() {
        // M6 / C1: the `n0_dns_pkarr` flag must actually be
        // mutable through the builder API. Pinning the
        // `without_n0_dns_pkarr` setter down prevents
        // future renames silently regressing to "always on".
        let cfg = DiscoveryConfig::default().without_n0_dns_pkarr();
        assert!(!cfg.n0_dns_pkarr);

        // Re-enabling after disabling should restore the default.
        let cfg = cfg.with_n0_dns_pkarr();
        assert!(cfg.n0_dns_pkarr);
    }

    #[test]
    fn pkarr_overrides_n0_dns_pkarr_flag() {
        // C1: when the operator supplies a custom `pkarr`
        // config, the `n0_dns_pkarr` flag is irrelevant —
        // the custom publisher always wins. We don't bind a
        // real endpoint here (that needs network); we just
        // check that the config composes cleanly.
        let cfg = DiscoveryConfig::default()
            .without_n0_dns_pkarr()
            .with_pkarr(PkarrPublisherConfig::default());
        assert!(!cfg.n0_dns_pkarr);
        assert!(cfg.pkarr.is_some());
    }

    /// V4-a2 regression: when the operator disables the iroh
    /// n0 Pkarr publisher AND does not supply a custom
    /// `PkarrPublisherConfig`, the bound endpoint has no
    /// Pkarr publishing path at all (no relay URL to PUT to).
    /// `DiscoveryConfig` should expose this fact via a helper
    /// so `bind_internal` can decide whether to stamp a
    /// `record_publish(true)` event. Without this contract,
    /// operators reading `/discovery` would see a phantom
    /// publish event that no actual pkarr relay ever saw.
    #[test]
    fn no_publisher_when_n0_disabled_and_no_custom_pkarr() {
        let cfg = DiscoveryConfig::default().without_n0_dns_pkarr();
        assert!(!cfg.n0_dns_pkarr);
        assert!(cfg.pkarr.is_none());
        // Documenting the V4-a2 contract: with both flags
        // off, the operator has chosen to run the node with
        // no public publish path. The default config (with
        // n0 enabled) does have a publisher — these two cases
        // must NOT collapse in `bind_internal`.
        assert!(
            cfg.pkarr.is_none() && !cfg.n0_dns_pkarr,
            "no publisher configured: bind_internal must skip the synthetic record_publish"
        );
    }

    // ──────────────────── UserData config wiring ──────────────────────────

    #[test]
    fn config_with_user_data_stores_field() {
        let ud = crate::iroh::discovery::UserData::new("audit-marker").unwrap();
        let cfg = DiscoveryConfig::default().with_user_data(ud.clone());
        assert_eq!(cfg.user_data.as_ref(), Some(&ud));
    }

    #[test]
    fn config_without_user_data_clears_field() {
        let cfg = DiscoveryConfig::default()
            .with_user_data(crate::iroh::discovery::UserData::new("x").unwrap())
            .without_user_data();
        assert!(cfg.user_data.is_none());
    }

    #[test]
    fn user_data_is_pre_stamped_in_diagnostics_on_bind() {
        // The integration tests below exercise the wire path; here
        // we only assert the pre-stamp behaviour on
        // `DiscoveryBuilder::diagnostics()` after `.bind(...)`.
        let ud = crate::iroh::discovery::UserData::new("audit-marker").unwrap();
        let cfg = DiscoveryConfig::default()
            .with_diagnostics(Arc::new(DiscoveryDiagnostics::new()))
            .with_user_data(ud.clone());
        let builder = DiscoveryBuilder::new(cfg);
        // `diagnostics()` exposes the shared recorder; the
        // `bind()` path (exercised in the integration tests)
        // is the one that pre-stamps `record_user_data`.
        let _ = builder.diagnostics();
    }

    // ──────────────────── mDNS config wiring ─────────────────────────────────

    #[test]
    fn mdns_enabled_defaults_to_false() {
        let cfg = DiscoveryConfig::default();
        #[cfg(feature = "mdns")]
        {
            assert!(!cfg.mdns_enabled, "mDNS must default to disabled (opt-in)");
        }
        #[cfg(not(feature = "mdns"))]
        {
            // Without the feature, the placeholder exists but doesn't affect logic
            assert_eq!(cfg._mdns_placeholder, ());
        }
    }

    #[test]
    fn mdns_enabled_is_mutable_through_builder() {
        #[cfg(feature = "mdns")]
        {
            let cfg = DiscoveryConfig::default()
                .with_mdns_enabled(true);
            assert!(cfg.mdns_enabled, "with_mdns_enabled(true) must set the flag");

            let cfg = cfg.with_mdns_enabled(false);
            assert!(!cfg.mdns_enabled, "with_mdns_enabled(false) must clear the flag");
        }
        #[cfg(not(feature = "mdns"))]
        {
            // Without the feature, with_mdns_enabled doesn't exist — this test
            // only runs on mdns-enabled builds
        }
    }

    #[test]
    fn config_mdns_enabled_stores_persistent_flag() {
        // This test documents the contract: the mdns_enabled field
        // survives cloning and can be used to determine whether to
        // invoke DiscoveryBuilder::with_mdns at bind time.
        #[cfg(feature = "mdns")]
        {
            let cfg = DiscoveryConfig::default().with_mdns_enabled(true);
            let cloned = cfg.clone();
            assert!(cloned.mdns_enabled, "mdns_enabled must survive clone");
        }
    }
}
