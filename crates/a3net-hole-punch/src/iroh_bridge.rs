//! `#![cfg(feature = "iroh")]` bridge to the iroh 1.0 ecosystem.
//!
//! This module is the only place in `a3net-hole-punch` that
//! imports `iroh::address_lookup` and `iroh::Endpoint`. It exposes
//! replacement resolvers for the built-in strategies (Ticket,
//! mDNS, Mainline DHT, Pkarr/DNS) that wrap the existing
//! `a3net-transport::iroh::discovery` implementations.
//!
//! Operators that want the real resolver for a built-in strategy
//! should construct one of the structs below and add it as a
//! `HolePunchStrategy::Custom(...)`.
//!
//! [`HolePunchStrategy::Custom`]: crate::strategy::HolePunchStrategy::Custom

#![cfg(feature = "iroh")]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use iroh::address_lookup::AddressLookup;
use iroh_base::EndpointId;
use n0_future::StreamExt;
use tokio::sync::Notify;
use tracing::{debug, warn};

use a3net_types::NodeId;

use crate::error::{HolePunchError, HolePunchResult};
use crate::strategy::{
    DirectAddress, HolePunchResolver, ResolvedEndpoint, ResolverCapabilities,
};

/// Internal helper: convert a `iroh_base::EndpointAddr` into our
/// `ResolvedEndpoint`. The two types are intentionally similar
/// (iroh's `EndpointAddr` is `EndpointId + Vec<TransportAddr>`).
/// This helper consults the iroh accessor methods (`ip_addrs`,
/// `relay_urls`) which are the canonical way to walk the
/// addressing info.
fn addr_to_resolved(addr: &iroh_base::EndpointAddr) -> ResolvedEndpoint {
    let mut out = ResolvedEndpoint::empty(
        NodeId::from_bytes(addr.id.as_bytes()).unwrap_or_else(|_| {
            // Fallback: `NodeId::from_bytes` is the only path
            // that can fail (wrong length). 32-byte EndpointId
            // is guaranteed by `EndpointId::from_bytes` itself,
            // so the only failure path is a real bug; we
            // substitute an all-zero NodeId so the caller has
            // something to log.
            NodeId::from_bytes(&[0u8; 32]).expect("32 bytes is valid")
        }),
    );
    for relay_url in addr.relay_urls() {
        out.relay_urls.push(relay_url.to_string());
    }
    for socket in addr.ip_addrs() {
        out.direct_addresses.push(DirectAddress::new(
            socket.ip().to_string(),
            socket.port(),
        ));
    }
    out
}

/// Bridge: a custom resolver that delegates to any
/// `Arc<dyn AddressLookup>` (e.g. the iroh in-memory lookup that
/// `a3net-transport::iroh::discovery::MemoryLookup` wraps).
///
/// The resolver forwards `resolve(...)` to the underlying lookup
/// and converts the resulting `iroh_base::EndpointAddr` into our
/// `ResolvedEndpoint`. The conversion is **lossy on Custom
/// transports** — see `addr_to_resolved` for the policy.
pub struct AddressesBridge {
    label: &'static str,
    capabilities: ResolverCapabilities,
    lookup: Arc<dyn AddressLookup>,
}

impl std::fmt::Debug for AddressesBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddressesBridge")
            .field("label", &self.label)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl AddressesBridge {
    /// Build a bridge that forwards to the supplied address
    /// lookup. The `label` is the provenance string used in
    /// diagnostics (typically reused from the upstream lookup).
    pub fn new(
        label: &'static str,
        capabilities: ResolverCapabilities,
        lookup: Arc<dyn AddressLookup>,
    ) -> Self {
        Self {
            label,
            capabilities,
            lookup,
        }
    }
}

#[async_trait]
impl HolePunchResolver for AddressesBridge {
    async fn resolve(
        &self,
        target: NodeId,
        budget: Duration,
        cancel: Arc<Notify>,
    ) -> HolePunchResult<ResolvedEndpoint> {
        let endpoint_id = endpoint_id_from_node_id(&target).map_err(|e| {
            HolePunchError::InvalidNodeId(format!(
                "cannot map NodeId to EndpointId: {e}"
            ))
        })?;
        let stream = match self.lookup.resolve(endpoint_id) {
            Some(s) => s,
            None => {
                debug!(
                    label = self.label,
                    "AddressLookup::resolve returned None; treating as empty"
                );
                return Ok(ResolvedEndpoint::empty(target));
            }
        };

        // Race the resolution stream against the cancel
        // channel and the per-call budget.
        let mut stream = stream;
        let collected = tokio::select! {
            biased;
            _ = cancel.notified() => {
                return Err(HolePunchError::Cancelled);
            }
            _ = tokio::time::sleep(budget) => {
                debug!(label = self.label, "AddressLookup::resolve budget elapsed");
                return Ok(ResolvedEndpoint::empty(target));
            }
            collected = async {
                let mut acc: Vec<iroh_base::EndpointAddr> = Vec::new();
                while let Some(slot) = stream.next().await {
                    // iroh 1.0's `AddressLookup::resolve`
                    // returns a `Stream<Item =
                    // Result<Item, Error>>`, so we have to
                    // unwrap one layer before we can read
                    // the embedded `Item`.
                    match slot {
                        Ok(item) => {
                            let info = item.endpoint_info().clone();
                            let addr = info.into_endpoint_addr();
                            acc.push(addr);
                        }
                        Err(e) => {
                            debug!(label = self.label, "AddressLookup::resolve error: {e}");
                        }
                    }
                }
                acc
            } => {
                collected
            }
        };

        // Pick the first non-empty endpoint. The iroh
        // aggregator merges results from every registered
        // lookup, so the first item is typically the most
        // specific (e.g. memory > pkarr > dns).
        for addr in collected {
            let resolved = addr_to_resolved(&addr);
            if resolved.has_any_address() {
                return Ok(resolved);
            }
        }
        Ok(ResolvedEndpoint::empty(target))
    }

    fn label(&self) -> &'static str {
        self.label
    }

    fn capabilities(&self) -> ResolverCapabilities {
        self.capabilities
    }
}

/// Convert a `NodeId` into an `iroh_base::EndpointId`. Centralised
/// so the planner keeps its own conversion logic outside of this
/// module.
fn endpoint_id_from_node_id(node_id: &NodeId) -> Result<EndpointId, String> {
    let bytes: [u8; 32] = node_id
        .as_bytes()
        .as_slice()
        .try_into()
        .map_err(|_| format!("NodeId must be exactly 32 bytes"))?;
    EndpointId::from_bytes(&bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::address_lookup::memory::MemoryLookup as IrohMemoryLookup;
    use iroh::address_lookup::EndpointData;
    use iroh::SecretKey;

    fn nid() -> NodeId {
        NodeId::from_bytes(&[0u8; 32]).expect("32 bytes is valid")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn addr_to_resolved_extracts_relay_and_direct() {
        let key = SecretKey::generate();
        let ep = key.public();
        let relay_url: iroh_base::RelayUrl =
            "https://relay.iroh.link".parse().unwrap();
        let addr = iroh_base::EndpointAddr::from(ep)
            .with_relay_url(relay_url)
            .with_ip_addr("127.0.0.1:9000".parse().unwrap());
        let r = addr_to_resolved(&addr);
        assert_eq!(r.relay_urls.len(), 1);
        assert_eq!(r.direct_addresses.len(), 1);
        assert_eq!(r.direct_addresses[0].host, "127.0.0.1");
        assert_eq!(r.direct_addresses[0].port, 9000);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn addresses_bridge_returns_empty_on_unknown_target() {
        let lookup = IrohMemoryLookup::with_provenance("test");
        let bridge = AddressesBridge::new(
            "test",
            ResolverCapabilities::all(),
            Arc::new(lookup),
        );
        let resolved = bridge
            .resolve(nid(), Duration::from_millis(50), Arc::new(Notify::new()))
            .await
            .unwrap();
        assert!(!resolved.has_any_address());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn addresses_bridge_returns_hit_when_target_known() {
        let lookup = IrohMemoryLookup::with_provenance("test");
        let key = SecretKey::generate();
        let ep = key.public();
        // Inject an endpoint into the lookup with a real
        // relay URL.
        let relay_url: iroh_base::RelayUrl =
            "https://relay.iroh.link".parse().unwrap();
        let addr = iroh_base::EndpointAddr::from(ep).with_relay_url(relay_url);
        let info = iroh::address_lookup::EndpointInfo::from_parts(
            ep,
            iroh::address_lookup::EndpointData::from(addr),
        );
        lookup.add_endpoint_info(info);
        let target = NodeId::from_bytes(ep.as_bytes()).unwrap();
        let bridge = AddressesBridge::new(
            "test",
            ResolverCapabilities::all(),
            Arc::new(lookup),
        );
        let resolved = bridge
            .resolve(target, Duration::from_millis(500), Arc::new(Notify::new()))
            .await
            .unwrap();
        assert!(
            resolved.has_any_address(),
            "expected the bridge to surface the relay URL, got {resolved:?}"
        );
        assert_eq!(resolved.relay_urls.len(), 1);
    }}
