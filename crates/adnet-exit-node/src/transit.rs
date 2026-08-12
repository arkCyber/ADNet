//! Transit-forwarding decision — the data-plane sibling of
//! `router.rs`.
//!
//! Whereas [`crate::router::Router`] answers *"where does
//! this packet go from here?"* (mesh / gateway / drop),
//! [`TransitRouter`] answers *"is this packet addressed to
//! a peer that I, as a transit node, am willing to forward
//! for, and if so via which next hop?"*.
//!
//! ## Scope (this commit)
//!
//! This file is a **pure decision module**. It does not
//! touch sockets, the TUN device, the firewall, or any
//! runtime state beyond what the caller passes in. The
//! whole thing is `Sync + Send`, allocation-free in the
//! hot path, and unit-testable without a mesh.
//!
//! Wiring it into the actual packet path is the subject of
//! RFC-0007 and PR #3 of the phased rollout — see
//! `docs/rfcs/0007-mesh-transit-forwarding.md`.
//!
//! ## Mental model
//!
//! ```text
//!   mesh A                 mesh B                 mesh C
//!   ──────                 ──────                 ──────
//!   node S ───packet─────▶ node T ───packet─────▶ node D
//!            (transit)             (target)
//! ```
//!
//! - `S` is the **source** — authenticated member of
//!   `mesh A`.
//! - `T` is the **transit node** (us, if we are `T`).
//! - `D` is the **target** — member of `mesh C`.
//!
//! We (`T`) decide whether the packet from `S` should be
//! forwarded into `mesh C` via one of our peering partners,
//! dropped, or treated as local (i.e. `T` is the actual
//! destination).
//!
//! ## Threat model (RFC-0007 §4)
//!
//! The decision is **default-allow for authenticated
//! sources** in our current posture. "Authenticated" means
//! the source's membership is verifiable against the
//! roster of the mesh they claim to be part of. This is the
//! "permissive" preset chosen for the first iteration;
//! switching to strict-default only requires flipping the
//! [`TransitCapability::default`] — every other code path
//! is identical.

use std::collections::HashMap;
use std::sync::Arc;

use adnet_types::{MeshNetworkId, NodeId};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// A candidate next-hop toward a target mesh.
///
/// `cost` is an opaque, comparable "route metric" — lower
/// is better. In the first iteration we use hop count
/// (1 = direct peering); later iterations may blend RTT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitHop {
    /// The mesh the next-hop node belongs to. Always our
    /// own mesh in the direct case; some other mesh in the
    /// transit-of-transit case (future).
    pub via_network: MeshNetworkId,
    /// The next-hop node's identity.
    pub next_hop: NodeId,
    /// Route metric, lower is preferred. Direct peering is
    /// always `1`.
    pub cost: u8,
}

/// A read-only view of the topology the transit router
/// uses to make its decision.
///
/// Implementations are expected to be cheap to clone
/// (`Arc` inside) and to refresh from the gossip topic
/// asynchronously. For tests we use an in-memory
/// [`StaticTopology`].
pub trait TransitTopology: Send + Sync {
    /// All known next-hops that can reach `target`. Return
    /// an empty slice if `target` is unknown.
    fn hops_to(&self, target: &MeshNetworkId) -> Vec<TransitHop>;

    /// The networks this node has a direct peering with.
    /// A packet destined for one of these networks may be
    /// forwarded in a single hop.
    fn peered_networks(&self) -> Vec<MeshNetworkId>;
}

/// In-memory topology used by tests and by the bootstrap
/// path before gossip has converged.
#[derive(Debug, Clone, Default)]
pub struct StaticTopology {
    inner: Arc<RwLock<TopologyState>>,
}

#[derive(Debug, Clone, Default)]
struct TopologyState {
    /// For each known target network, the ordered list of
    /// candidate next-hops. Ordered ascending by `cost`.
    paths: HashMap<MeshNetworkId, Vec<TransitHop>>,
    /// Networks we have direct peering with (subset of the
    /// keys of `paths`).
    peered: Vec<MeshNetworkId>,
}

impl StaticTopology {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the path table wholesale. Cheaper for tests
    /// than incremental mutation.
    pub fn set_paths(&self, paths: HashMap<MeshNetworkId, Vec<TransitHop>>) {
        let peered: Vec<MeshNetworkId> = paths
            .iter()
            .filter_map(|(net, hops)| {
                if hops.iter().any(|h| h.cost == 1) {
                    Some(net.clone())
                } else {
                    None
                }
            })
            .collect();
        self.inner.write().paths = paths;
        self.inner.write().peered = peered;
    }

    /// Read-only snapshot of the path table. Used by
    /// [`crate::transit_gossip::GossipFederatedTopology`]
    /// to apply grants incrementally without losing
    /// pre-existing entries from other sources.
    pub fn paths_table(&self) -> HashMap<MeshNetworkId, Vec<TransitHop>> {
        self.inner.read().paths.clone()
    }
}

impl TransitTopology for StaticTopology {
    fn hops_to(&self, target: &MeshNetworkId) -> Vec<TransitHop> {
        self.inner
            .read()
            .paths
            .get(target)
            .cloned()
            .unwrap_or_default()
    }

    fn peered_networks(&self) -> Vec<MeshNetworkId> {
        self.inner.read().peered.clone()
    }
}

/// What this node is willing to forward for.
///
/// The default is **permissive**: any authenticated source
/// may transit through us. Switching to strict (require an
/// explicit capability grant) is a one-line change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransitCapability {
    /// Forward for any authenticated source (default).
    #[default]
    Permissive,
    /// Forward only for sources listed in the grant set.
    /// The list is checked against the *source node's
    /// identity*; the source's network claim is verified
    /// separately via the roster.
    Strict { allowlist: Vec<NodeId> },
}

impl TransitCapability {
    /// Whether `source` is allowed to transit through us.
    pub fn permits(&self, source: &NodeId) -> bool {
        match self {
            Self::Permissive => true,
            Self::Strict { allowlist } => allowlist.iter().any(|n| n == source),
        }
    }
}

/// The decision returned by [`TransitRouter::decide`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransitDecision {
    /// The packet is addressed to us. No forwarding.
    Local,
    /// Forward the packet via the given next-hop.
    Forward {
        via_network: MeshNetworkId,
        next_hop: NodeId,
        cost: u8,
    },
    /// Refuse to forward, with a human-readable reason.
    Drop { reason: String },
}

impl TransitDecision {
    pub fn is_forward(&self) -> bool {
        matches!(self, Self::Forward { .. })
    }
}

/// Configuration for [`TransitRouter`].
#[derive(Debug, Clone)]
pub struct TransitConfig {
    /// The network we (the transit node) belong to.
    pub local_network: MeshNetworkId,
    /// What we are willing to forward for. Defaults to
    /// [`TransitCapability::Permissive`].
    pub capability: TransitCapability,
}

impl TransitConfig {
    pub fn permissive(local_network: MeshNetworkId) -> Self {
        Self {
            local_network,
            capability: TransitCapability::Permissive,
        }
    }
}

/// Stateless transit-decision engine.
///
/// Holds no state of its own beyond the config and a cheap
/// clone-able handle to the topology. Cheap to clone
/// (`Arc` inside); wrap it in a `Router` of your choice.
#[derive(Clone)]
pub struct TransitRouter<T: TransitTopology> {
    inner: Arc<TransitInner<T>>,
}

struct TransitInner<T: TransitTopology> {
    config: TransitConfig,
    topology: T,
}

impl<T: TransitTopology> TransitRouter<T> {
    pub fn new(config: TransitConfig, topology: T) -> Self {
        Self {
            inner: Arc::new(TransitInner { config, topology }),
        }
    }

    /// The network we belong to.
    pub fn local_network(&self) -> &MeshNetworkId {
        &self.inner.config.local_network
    }

    /// Decide what to do with a packet from `source`
    /// (claimed `source_network`) destined for
    /// `target_network`.
    ///
    /// Inputs are *untrusted* — the caller is responsible
    /// for proving the source's membership in
    /// `source_network` (e.g. via a signed envelope) before
    /// calling this. We only enforce our local policy and
    /// the topology constraint.
    pub fn decide(
        &self,
        source: &NodeId,
        source_network: &MeshNetworkId,
        target_network: &MeshNetworkId,
    ) -> TransitDecision {
        // 1. Local: the packet is for our own network and
        //    we are the actual destination (caller decides
        //    "is it for me" — we only check the network
        //    match here, because the caller may forward to
        //    a peer inside our mesh on our behalf).
        if target_network == &self.inner.config.local_network {
            return TransitDecision::Local;
        }

        // 2. Permission: do we forward for this source at
        //    all?
        if !self.inner.config.capability.permits(source) {
            return TransitDecision::Drop {
                reason: format!(
                    "source {} not in strict allowlist",
                    source.short()
                ),
            };
        }

        // 3. Same-mesh but cross-network claim: a member
        //    of our own mesh trying to reach another mesh
        //    via us is the canonical transit case. We do
        //    not require `source_network` to be `local`
        //    here — a peering partner's member may also
        //    transit if it has a valid grant (future).
        //    For the first iteration we accept any
        //    authenticated `source_network` as the
        //    "from" leg.
        let _ = source_network; // documented; not consulted
                                // in v1 decision.

        // 4. Topology: do we know a path to the target?
        let hops = self.inner.topology.hops_to(target_network);
        if hops.is_empty() {
            return TransitDecision::Drop {
                reason: format!(
                    "no known path to target network {}",
                    target_network.short()
                ),
            };
        }

        // 5. Pick the lowest-cost hop. `hops_to` is
        //    expected to return hops already sorted by
        //    `cost` ascending, but we re-sort defensively
        //    in case the topology implementation broke
        //    that invariant (it's only an 8-bit field,
        //    well under any sort threshold).
        let mut hops = hops;
        hops.sort_by_key(|h| h.cost);
        let best = hops.into_iter().next().expect("non-empty");

        // 6. Sanity: cost of 0 means the topology lied
        //    (zero is reserved for "no path"). Treat as
        //    a topology error and refuse.
        if best.cost == 0 {
            return TransitDecision::Drop {
                reason: "topology returned cost=0 (reserved)".into(),
            };
        }

        TransitDecision::Forward {
            via_network: best.via_network,
            next_hop: best.next_hop,
            cost: best.cost,
        }
    }

    /// Snapshot for the status command. Currently exposes
    /// only the local network id and the capability
    /// preset; expand in PR #2.
    pub fn snapshot(&self) -> TransitSnapshot {
        TransitSnapshot {
            local_network: self.inner.config.local_network.clone(),
            capability: self.inner.config.capability.clone(),
            peered_networks: self.inner.topology.peered_networks(),
        }
    }
}

/// Snapshot of the transit router's state for diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitSnapshot {
    pub local_network: MeshNetworkId,
    pub capability: TransitCapability,
    pub peered_networks: Vec<MeshNetworkId>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::NodeId;

    fn nid(seed: u8) -> NodeId {
        // Deterministic NodeId from a single seed byte.
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        NodeId::from_bytes(&bytes).unwrap()
    }

    fn net(seed: u8) -> MeshNetworkId {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        MeshNetworkId::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn local_target_is_local_decision() {
        let local = net(1);
        let topo = StaticTopology::new();
        let r = TransitRouter::new(TransitConfig::permissive(local.clone()), topo);
        let d = r.decide(&nid(2), &net(99), &local);
        assert_eq!(d, TransitDecision::Local);
    }

    #[test]
    fn unknown_target_drops_with_reason() {
        let local = net(1);
        let topo = StaticTopology::new();
        let r = TransitRouter::new(TransitConfig::permissive(local), topo);
        let d = r.decide(&nid(2), &net(99), &net(42));
        assert!(matches!(d, TransitDecision::Drop { .. }));
        let reason = match d {
            TransitDecision::Drop { reason } => reason,
            _ => unreachable!(),
        };
        assert!(reason.contains("no known path"));
    }

    #[test]
    fn strict_capability_rejects_unlisted_source() {
        let local = net(1);
        let topo = StaticTopology::new();
        let allowed = nid(7);
        let mut paths = HashMap::new();
        paths.insert(
            net(42),
            vec![TransitHop {
                via_network: local.clone(),
                next_hop: nid(5),
                cost: 1,
            }],
        );
        topo.set_paths(paths);

        let r = TransitRouter::new(
            TransitConfig {
                local_network: local,
                capability: TransitCapability::Strict {
                    allowlist: vec![allowed],
                },
            },
            topo,
        );
        // Unlisted source → Drop.
        let d = r.decide(&nid(99), &net(2), &net(42));
        assert!(matches!(d, TransitDecision::Drop { .. }));
    }

    #[test]
    fn strict_capability_accepts_listed_source() {
        let local = net(1);
        let topo = StaticTopology::new();
        let allowed = nid(7);
        let mut paths = HashMap::new();
        paths.insert(
            net(42),
            vec![TransitHop {
                via_network: local.clone(),
                next_hop: nid(5),
                cost: 1,
            }],
        );
        topo.set_paths(paths);

        let r = TransitRouter::new(
            TransitConfig {
                local_network: local.clone(),
                capability: TransitCapability::Strict {
                    allowlist: vec![allowed.clone()],
                },
            },
            topo,
        );
        let d = r.decide(&allowed, &net(2), &net(42));
        assert_eq!(
            d,
            TransitDecision::Forward {
                via_network: local,
                next_hop: nid(5),
                cost: 1,
            }
        );
    }

    #[test]
    fn permissive_capability_forwards_via_lowest_cost_hop() {
        let local = net(1);
        let topo = StaticTopology::new();
        let mut paths = HashMap::new();
        paths.insert(
            net(42),
            vec![
                TransitHop {
                    via_network: local.clone(),
                    next_hop: nid(5),
                    cost: 3,
                },
                TransitHop {
                    via_network: net(7),
                    next_hop: nid(6),
                    cost: 1,
                },
                TransitHop {
                    via_network: net(8),
                    next_hop: nid(9),
                    cost: 2,
                },
            ],
        );
        topo.set_paths(paths);

        let r = TransitRouter::new(TransitConfig::permissive(local.clone()), topo);
        let d = r.decide(&nid(2), &net(99), &net(42));
        // Cost=1 should win, even though it was inserted
        // second.
        assert_eq!(
            d,
            TransitDecision::Forward {
                via_network: net(7),
                next_hop: nid(6),
                cost: 1,
            }
        );
    }

    #[test]
    fn cost_zero_is_treated_as_topology_error() {
        let local = net(1);
        let topo = StaticTopology::new();
        let mut paths = HashMap::new();
        paths.insert(
            net(42),
            vec![TransitHop {
                via_network: local.clone(),
                next_hop: nid(5),
                cost: 0,
            }],
        );
        topo.set_paths(paths);

        let r = TransitRouter::new(TransitConfig::permissive(local), topo);
        let d = r.decide(&nid(2), &net(99), &net(42));
        assert!(matches!(d, TransitDecision::Drop { .. }));
        let reason = match d {
            TransitDecision::Drop { reason } => reason,
            _ => unreachable!(),
        };
        assert!(reason.contains("cost=0"));
    }

    #[test]
    fn snapshot_includes_peered_networks() {
        let local = net(1);
        let topo = StaticTopology::new();
        let mut paths = HashMap::new();
        paths.insert(
            net(42),
            vec![TransitHop {
                via_network: local.clone(),
                next_hop: nid(5),
                cost: 1,
            }],
        );
        // cost > 1 should NOT count as peered.
        paths.insert(
            net(43),
            vec![TransitHop {
                via_network: net(7),
                next_hop: nid(6),
                cost: 2,
            }],
        );
        topo.set_paths(paths);

        let r = TransitRouter::new(TransitConfig::permissive(local.clone()), topo);
        let snap = r.snapshot();
        assert_eq!(snap.local_network, local);
        assert_eq!(snap.peered_networks, vec![net(42)]);
        assert_eq!(snap.capability, TransitCapability::Permissive);
    }

    #[test]
    fn capability_default_is_permissive() {
        assert_eq!(TransitCapability::default(), TransitCapability::Permissive);
    }

    #[test]
    fn capability_permits_permissive_for_any_source() {
        let cap = TransitCapability::Permissive;
        assert!(cap.permits(&nid(1)));
        assert!(cap.permits(&nid(255)));
    }

    #[test]
    fn capability_permits_strict_only_listed() {
        let cap = TransitCapability::Strict {
            allowlist: vec![nid(7)],
        };
        assert!(cap.permits(&nid(7)));
        assert!(!cap.permits(&nid(8)));
    }

    #[test]
    fn decision_is_forward_predicate() {
        assert!(TransitDecision::Forward {
            via_network: net(1),
            next_hop: nid(2),
            cost: 1,
        }
        .is_forward());
        assert!(!TransitDecision::Local.is_forward());
        assert!(!TransitDecision::Drop {
            reason: "x".into()
        }
        .is_forward());
    }

    #[test]
    fn empty_topology_drops_every_non_local_target() {
        let local = net(1);
        let r = TransitRouter::new(TransitConfig::permissive(local), StaticTopology::new());
        for target_seed in 2..5u8 {
            let d = r.decide(&nid(10), &net(20), &net(target_seed));
            assert!(matches!(d, TransitDecision::Drop { .. }));
        }
    }

    #[test]
    fn local_network_short_helper_matches_node_short() {
        // Sanity: both NodeId and MeshNetworkId expose a
        // `short()` that returns the first 12 hex chars.
        // We rely on this in Drop reasons — make sure it
        // doesn't panic on a fresh id.
        let n = nid(0xAB);
        assert_eq!(n.short().len(), 12);
        let m = net(0xCD);
        assert_eq!(m.short().len(), 12);
    }

    #[test]
    fn static_topology_default_is_empty() {
        let topo = StaticTopology::new();
        assert!(topo.hops_to(&net(1)).is_empty());
        assert!(topo.peered_networks().is_empty());
    }

    #[test]
    fn static_topology_set_paths_then_query() {
        let topo = StaticTopology::new();
        let mut paths = HashMap::new();
        paths.insert(
            net(42),
            vec![TransitHop {
                via_network: net(1),
                next_hop: nid(5),
                cost: 1,
            }],
        );
        topo.set_paths(paths);
        let hops = topo.hops_to(&net(42));
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].cost, 1);
        assert!(topo.peered_networks().contains(&net(42)));
    }
}
