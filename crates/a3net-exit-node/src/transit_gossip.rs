//! Gossip-fed [`TransitTopology`] implementation.
//!
//! Where [`StaticTopology`] is a hand-rolled test
//! fixture, [`GossipFederatedTopology`] is what the
//! production code path uses. It wraps a
//! [`StaticTopology`] and applies incoming
//! [`PeeringGrant`](a3net_mesh_coordinator::PeeringGrant)
//! envelopes from the `a3net-transit/v1` gossip topic.
//!
//! ## What it does
//!
//! 1. Validates the grant envelope (parses JSON, checks
//!    the signature field is non-empty, checks `valid_until`).
//! 2. Translates each grant into one or more
//!    [`TransitHop`] entries:
//!    - `Bidirectional` ⇒ both `(target→source, cost)` and
//!      `(source→target, cost)`.
//!    - `SourceToTarget` ⇒ only `(source→target, cost)`.
//! 3. Stores the hops in the wrapped `StaticTopology` so
//!    the [`TransitRouter`] can query them.
//!
//! ## What it does NOT do (yet)
//!
//! - **Signature verification.** v1 treats a non-empty
//!   `signature` field as a syntactic marker; the
//!   actual cryptographic verification is the
//!   responsibility of the gossip layer (or a separate
//!   `a3net-identity` integration). See RFC §10.
//! - **Revocation-by-expiry.** Expired grants are pruned
//!   lazily by [`prune_expired`]; callers should run
//!   it on a timer.
//! - **Cost aggregation across multiple grants.** If
//!   two grants offer a path to the same target with
//!   different costs, the lowest cost wins (already
//!   what `TransitRouter::decide` does).

use std::sync::Arc;

use a3net_mesh_coordinator::{
    CoordinatorPubkeyRegistry, PeeringGrant, PeeringGrantId, PeeringGrantVerifier,
    PeeringRevocation,
};
use a3net_types::{MeshNetworkId, NodeId};
use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::transit::{StaticTopology, TransitHop, TransitTopology};

/// Gossip-fed transit topology.
///
/// Holds a `StaticTopology` underneath (so `TransitRouter`
/// can keep treating it as `impl TransitTopology`) and a
/// registry of `PeeringGrantId`s we have applied, so we
/// can apply revocations cleanly.
#[derive(Debug, Clone)]
pub struct GossipFederatedTopology {
    inner: Arc<GossipInner>,
}

#[derive(Debug)]
struct GossipInner {
    inner: StaticTopology,
    /// Active grants keyed by `PeeringGrantId`. We keep
    /// this so a revocation knows which `(via_network,
    /// next_hop)` entries to remove.
    applied: RwLock<Vec<AppliedGrant>>,
    /// Identity of the local node. A grant whose
    /// `grantor` is our own node can be accepted
    /// unconditionally; grants from other nodes are
    /// also accepted in v1 (no PKI yet).
    local_node: NodeId,
    /// Identity of the local network. Used to filter
    /// out grants that don't apply to us (RFC §5.5).
    local_network: MeshNetworkId,
}

/// What we applied to the inner `StaticTopology` for a
/// given grant. We keep this so `observe_revocation` can
/// reverse the change.
#[derive(Debug, Clone)]
struct AppliedGrant {
    grant_id: PeeringGrantId,
    /// When this grant expires. The expiry is captured here
    /// at accept-time so `prune_expired_at` can walk the list
    /// without re-fetching the underlying
    /// [`PeeringGrant`](crate::peering::PeeringGrant).
    valid_until: DateTime<Utc>,
    /// The hops we added. Each entry is
    /// `(target_network, hop_to_install)`. On revocation
    /// we remove the entry whose `target_network`
    /// matches and whose `next_hop` matches the only
    /// hop we installed for that direction.
    hops: Vec<(MeshNetworkId, TransitHop)>,
}

impl GossipFederatedTopology {
    /// Construct a topology tied to a specific local node
    /// and network.
    pub fn new(local_node: NodeId, local_network: MeshNetworkId) -> Self {
        Self {
            inner: Arc::new(GossipInner {
                inner: StaticTopology::new(),
                applied: RwLock::new(Vec::new()),
                local_node,
                local_network,
            }),
        }
    }

    /// The local node id.
    pub fn local_node(&self) -> &NodeId {
        &self.inner.local_node
    }

    /// The local network id.
    pub fn local_network(&self) -> &MeshNetworkId {
        &self.inner.local_network
    }

    /// Apply a [`PeeringGrant`]. Returns `Ok(())` if the
    /// grant was applied (or replaced an existing grant
    /// with the same id), `Err(reason)` otherwise.
    ///
    /// **No cryptographic verification.** This is the
    /// cheap, syntax-only path. Production callers should
    /// use [`observe_grant_verified`] instead.
    pub fn observe_grant(&self, grant: PeeringGrant) -> Result<(), GossipApplyError> {
        self.observe_grant_at(grant, Utc::now())
    }

    /// Time-parameterised variant of [`observe_grant`]
    /// for tests that need to exercise expiry.
    pub fn observe_grant_at(
        &self,
        grant: PeeringGrant,
        now: DateTime<Utc>,
    ) -> Result<(), GossipApplyError> {
        // 1. Cheap syntactic checks.
        if grant.source == grant.target {
            return Err(GossipApplyError::SelfLoop);
        }
        if grant.signature.is_empty() {
            return Err(GossipApplyError::Unsigned);
        }
        if grant.is_expired(now) {
            return Err(GossipApplyError::Expired {
                grant_id: grant.grant_id.to_string(),
                valid_until: grant.valid_until,
            });
        }
        self.apply_grant(grant)
    }

    /// Apply a [`PeeringGrant`] with full cryptographic
    /// verification. The verifier checks the Ed25519
    /// signature against the coordinator pubkey returned
    /// by `registry` for `grant.source`. Use this path
    /// for production gossip ingest.
    pub fn observe_grant_verified<R: CoordinatorPubkeyRegistry>(
        &self,
        grant: PeeringGrant,
        registry: &R,
    ) -> Result<(), GossipApplyError> {
        PeeringGrantVerifier::new()
            .verify(&grant, registry, Utc::now())
            .map_err(|e| match e {
                a3net_mesh_coordinator::CoordinatorError::PeeringSelfLoop => {
                    GossipApplyError::SelfLoop
                }
                a3net_mesh_coordinator::CoordinatorError::PeeringSignatureInvalid(s) => {
                    GossipApplyError::SignatureInvalid(s)
                }
                a3net_mesh_coordinator::CoordinatorError::PeeringUnknownCoordinator(s) => {
                    GossipApplyError::UnknownCoordinator(s)
                }
                a3net_mesh_coordinator::CoordinatorError::PeeringExpired {
                    grant_id,
                    valid_until,
                } => GossipApplyError::Expired {
                    grant_id,
                    valid_until,
                },
                other => GossipApplyError::SignatureInvalid(format!("{other}")),
            })?;
        self.apply_grant(grant)
    }

    /// Internal: apply (no verification). Caller is
    /// responsible for any pre-checks.
    fn apply_grant(&self, grant: PeeringGrant) -> Result<(), GossipApplyError> {
        // 2. Revoke any prior grant with the same id
        //    (re-publish semantics).
        if self.inner.applied.read().iter().any(|a| a.grant_id == grant.grant_id) {
            self.observe_revocation(&PeeringRevocation {
                grant_id: grant.grant_id.clone(),
                source: grant.source.clone(),
                signature: String::new(),
            });
        }

        // 3. Translate the grant into hops.
        //
        //    For a `Bidirectional` grant between `source`
        //    and `target`, we install:
        //
        //      (target → source, next_hop, cost)
        //      (source → target, next_hop, cost)
        //
        //    `next_hop` is the grantor's coordinator
        //    node (a member of `source`); we use it as a
        //    placeholder for "the peering partner you
        //    should talk to first". The actual end-to-end
        //    iroh connection establishes the real path.
        //
        //    `via_network` is the mesh on which the
        //    `next_hop` lives — i.e. `source` for both
        //    directions in v1 (we only support one-hop
        //    transits).
        let next_hop = grant.grantor.clone();
        let mut hops = Vec::new();

        let forward = TransitHop {
            via_network: grant.source.clone(),
            next_hop: next_hop.clone(),
            cost: grant.cost,
        };
        let backward = TransitHop {
            via_network: grant.target.clone(),
            next_hop: next_hop.clone(),
            cost: grant.cost,
        };

        // source → target (i.e. we want a route to `target`).
        hops.push((grant.target.clone(), forward.clone()));
        // target → source (mirror direction).
        if grant.direction.allows(false) {
            hops.push((grant.source.clone(), backward));
        }

        // 4. Apply.
        //
        //    We rebuild the entire path table for the
        //    two affected networks. This is O(N) per
        //    apply, but N is bounded by the number of
        //    grants (≪ 1000 in practice). A
        //    write-then-rewrite pattern would be
        //    faster but harder to reason about; keep it
        //    simple.
        {
            let mut applied = self.inner.applied.write();
            let mut paths = read_paths(&self.inner.inner);
            for (target, hop) in &hops {
                let entry = paths.entry(target.clone()).or_default();
                // If this exact hop already exists for
                // this target, skip — keeps the topology
                // small.
                if !entry.iter().any(|h| h.next_hop == hop.next_hop && h.via_network == hop.via_network) {
                    entry.push(hop.clone());
                }
            }
            self.inner.inner.set_paths(paths);
            applied.push(AppliedGrant {
                grant_id: grant.grant_id.clone(),
                valid_until: grant.valid_until,
                hops,
            });
        }

        Ok(())
    }

    /// Apply a [`PeeringRevocation`]. Idempotent.
    pub fn observe_revocation(&self, revocation: &PeeringRevocation) {
        let mut applied = self.inner.applied.write();
        let mut paths = read_paths(&self.inner.inner);
        let mut new_applied = Vec::with_capacity(applied.len());
        for entry in applied.drain(..) {
            if entry.grant_id == revocation.grant_id {
                for (target, hop) in &entry.hops {
                    if let Some(list) = paths.get_mut(target) {
                        list.retain(|h| !(h.next_hop == hop.next_hop && h.via_network == hop.via_network));
                    }
                }
            } else {
                new_applied.push(entry);
            }
        }
        *applied = new_applied;
        // Drop empty buckets so `hops_to` returns empty
        // rather than an empty list under a key.
        paths.retain(|_, v| !v.is_empty());
        self.inner.inner.set_paths(paths);
    }

    /// Drop every grant whose `valid_until` is in the
    /// past. Returns the number of grants pruned.
    pub fn prune_expired(&self) -> usize {
        self.prune_expired_at(Utc::now())
    }

    /// Time-parameterised variant of [`prune_expired`].
    ///
    /// Walks the in-process list of applied grants, removes
    /// any whose `valid_until <= now`, and rebuilds the
    /// path table for the affected entries. Returns the
    /// number of grants pruned.
    pub fn prune_expired_at(&self, now: DateTime<Utc>) -> usize {
        let mut applied = self.inner.applied.write();
        let before = applied.len();
        let mut pruned_paths: std::collections::HashSet<MeshNetworkId> =
            std::collections::HashSet::new();
        applied.retain(|entry| {
            if entry.valid_until <= now {
                // Remember every target so we can drop the
                // empty hop lists below.
                for (target, _hop) in &entry.hops {
                    pruned_paths.insert(target.clone());
                }
                false
            } else {
                true
            }
        });
        let pruned = before.saturating_sub(applied.len());
        if pruned > 0 {
            // Rebuild the path table so the pruned entries
            // no longer contribute.
            let mut new_paths: std::collections::HashMap<MeshNetworkId, Vec<TransitHop>> =
                std::collections::HashMap::new();
            for entry in applied.iter() {
                for (target, hop) in &entry.hops {
                    new_paths
                        .entry(target.clone())
                        .or_insert_with(Vec::new)
                        .push(hop.clone());
                }
            }
            new_paths.retain(|k, v| !v.is_empty() && !pruned_paths.contains(k));
            self.inner.inner.set_paths(new_paths);
        }
        pruned
    }

    /// Drop all state. Used by tests and by the
    /// `ray transit reset` operator command (future).
    pub fn reset(&self) {
        self.inner.inner.set_paths(Default::default());
        self.inner.applied.write().clear();
    }

    /// Number of distinct grants currently applied.
    pub fn applied_count(&self) -> usize {
        self.inner.applied.read().len()
    }
}

impl TransitTopology for GossipFederatedTopology {
    fn hops_to(&self, target: &MeshNetworkId) -> Vec<TransitHop> {
        self.inner.inner.hops_to(target)
    }

    fn peered_networks(&self) -> Vec<MeshNetworkId> {
        self.inner.inner.peered_networks()
    }
}

fn read_paths(topology: &StaticTopology) -> std::collections::HashMap<MeshNetworkId, Vec<TransitHop>> {
    // We don't have a direct accessor on StaticTopology;
    // rebuild the path table from the topology state.
    // Since `StaticTopology` owns its state privately, we
    // can't read paths wholesale. Instead, we enumerate
    // every known network by collecting from `peered`
    // plus probing via the union of `via_network` fields.
    //
    // For v1 we work around this by having GossipInner
    // own a *parallel* path table alongside
    // `StaticTopology`. This keeps the read API stable.
    //
    // The code below reads from a private field on
    // GossipInner via `paths_table()` — see below.
    topology.paths_table()
}

/// Errors that can occur when applying a peering grant.
#[derive(Debug, thiserror::Error)]
pub enum GossipApplyError {
    #[error("peering grant source and target must differ")]
    SelfLoop,

    #[error("peering grant has empty signature")]
    Unsigned,

    #[error("peering grant {grant_id} expired at {valid_until}")]
    Expired {
        grant_id: String,
        valid_until: DateTime<Utc>,
    },

    #[error("peering grant signature is invalid: {0}")]
    SignatureInvalid(String),

    #[error("peering grant coordinator pubkey is unknown for mesh {0}")]
    UnknownCoordinator(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_mesh_coordinator::Peerings;
    use a3net_mesh_coordinator::InMemoryPeerings;
    use a3net_mesh_coordinator::PeeringDirection;
    use std::time::Duration;

    use crate::transit::{TransitCapability, TransitConfig, TransitDecision, TransitRouter};

    fn nid(seed: u8) -> NodeId {
        NodeId::from_bytes(&[seed; 32]).unwrap()
    }

    fn net(seed: u8) -> MeshNetworkId {
        MeshNetworkId::from_bytes(&[seed; 32]).unwrap()
    }

    fn grant(source: u8, target: u8, grantor: u8) -> PeeringGrant {
        PeeringGrant::new_unsigned(
            net(source),
            net(target),
            nid(grantor),
            Duration::from_secs(60),
        )
        .unwrap()
        .with_signature("deadbeef")
    }

    #[test]
    fn new_topology_is_empty() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        assert_eq!(t.applied_count(), 0);
        assert!(t.hops_to(&net(42)).is_empty());
        assert!(t.peered_networks().is_empty());
    }

    #[test]
    fn apply_unidirectional_grant_installs_forward_hop() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = grant(1, 2, 3); // source=1 (local), target=2, grantor=3
        t.observe_grant(g).unwrap();
        // We are mesh 1; mesh 2 is the target. The
        // forward hop (mesh 1 → mesh 2) should be
        // installed.
        let hops_to_2 = t.hops_to(&net(2));
        assert_eq!(hops_to_2.len(), 1);
        assert_eq!(hops_to_2[0].via_network, net(1));
        assert_eq!(hops_to_2[0].next_hop, nid(3));
        assert_eq!(hops_to_2[0].cost, 1);
        // The reverse (mesh 2 → mesh 1) should also be
        // installed because the default direction is
        // Bidirectional.
        let hops_to_1 = t.hops_to(&net(1));
        assert_eq!(hops_to_1.len(), 1);
        assert_eq!(hops_to_1[0].via_network, net(2));
    }

    #[test]
    fn apply_source_to_target_grant_skips_reverse() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let mut g = grant(1, 2, 3);
        g.direction = PeeringDirection::SourceToTarget;
        t.observe_grant(g).unwrap();
        // Forward (1 → 2) is installed.
        assert_eq!(t.hops_to(&net(2)).len(), 1);
        // Reverse (2 → 1) is NOT installed because the
        // direction is SourceToTarget.
        assert!(t.hops_to(&net(1)).is_empty());
    }

    #[test]
    fn apply_unsigned_grant_errors() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        // No `.with_signature(...)` — signature is empty.
        let err = t.observe_grant(g).unwrap_err();
        assert!(matches!(err, GossipApplyError::Unsigned));
    }

    #[test]
    fn apply_self_loop_grant_errors() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        // Build by hand to bypass new_unsigned's check.
        let mut g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        g.source = net(2);
        g.target = net(2);
        g = g.with_signature("xx");
        let err = t.observe_grant(g).unwrap_err();
        assert!(matches!(err, GossipApplyError::SelfLoop));
    }

    #[test]
    fn apply_expired_grant_errors() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let mut g = grant(1, 2, 3);
        g.valid_until = Utc::now() - chrono::Duration::seconds(1);
        g = g.with_signature("xx");
        let err = t.observe_grant(g).unwrap_err();
        assert!(matches!(err, GossipApplyError::Expired { .. }));
    }

    #[test]
    fn revoke_removes_installed_hops() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = grant(1, 2, 3);
        let id = g.grant_id.clone();
        t.observe_grant(g).unwrap();
        assert_eq!(t.applied_count(), 1);
        t.observe_revocation(&PeeringRevocation {
            grant_id: id,
            source: net(1),
            signature: String::new(),
        });
        assert_eq!(t.applied_count(), 0);
        assert!(t.hops_to(&net(2)).is_empty());
    }

    #[test]
    fn revoke_unknown_grant_is_noop() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        t.observe_revocation(&PeeringRevocation {
            grant_id: PeeringGrantId::new(),
            source: net(1),
            signature: String::new(),
        });
        assert_eq!(t.applied_count(), 0);
    }

    #[test]
    fn reapplying_same_grant_replaces_previous() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = grant(1, 2, 3);
        let id = g.grant_id.clone();
        t.observe_grant(g.clone()).unwrap();
        let g2 = g.with_signature("ff");
        t.observe_grant(g2).unwrap();
        assert_eq!(t.applied_count(), 1);
        let hops = t.hops_to(&net(2));
        // Same `(next_hop, via_network)` pair — must
        // not double up.
        assert_eq!(hops.len(), 1);
        // (no-op assertion on id; kept for clarity)
        let _ = id;
    }

    #[test]
    fn two_grants_same_target_different_cost_picks_lowest() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g1 = grant(1, 2, 3);
        // Override cost on a fresh grant.
        let mut g2 = grant(1, 2, 4);
        g2.cost = 3;
        g2 = g2.with_signature("yy");
        t.observe_grant(g1).unwrap();
        t.observe_grant(g2).unwrap();
        let hops = t.hops_to(&net(2));
        assert_eq!(hops.len(), 2);

        // Now ask the transit router to decide. It
        // should pick the cost=1 hop.
        let router = TransitRouter::new(
            TransitConfig::permissive(net(1)),
            t.clone(),
        );
        let d = router.decide(&nid(99), &net(99), &net(2));
        match d {
            TransitDecision::Forward { cost, next_hop, .. } => {
                assert_eq!(cost, 1);
                assert_eq!(next_hop, nid(3));
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn full_integration_create_grant_publish_observe_decide() {
        // Simulate the end-to-end RFC §5 path:
        //   1. coordinator stores a grant
        //   2. publisher (we) reads it back
        //   3. observes it on a fresh topology
        //   4. transit router picks the new path
        let peerings: Box<dyn Peerings> = Box::new(InMemoryPeerings::new());
        let g = grant(1, 2, 3);
        let id = g.grant_id.clone();
        peerings.issue(g.clone()).unwrap();
        peerings.attach_signature(&id, "deadbeef".into()).unwrap();

        // Topology learns about the grant by observing
        // the *signed* envelope. In production this
        // would come from the gossip topic; here the
        // publisher reads from the coordinator store
        // and feeds it directly.
        let topo = GossipFederatedTopology::new(nid(7), net(1));
        let from_store = peerings.get(&id).unwrap();
        topo.observe_grant(from_store).unwrap();

        let router = TransitRouter::new(
            TransitConfig::permissive(net(1)),
            topo.clone(),
        );
        let d = router.decide(&nid(99), &net(99), &net(2));
        match d {
            TransitDecision::Forward {
                via_network,
                next_hop,
                cost,
            } => {
                assert_eq!(via_network, net(1));
                assert_eq!(next_hop, nid(3));
                assert_eq!(cost, 1);
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn reset_clears_everything() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        t.observe_grant(grant(1, 2, 3)).unwrap();
        t.observe_grant(grant(1, 4, 5)).unwrap();
        assert_eq!(t.applied_count(), 2);
        t.reset();
        assert_eq!(t.applied_count(), 0);
        assert!(t.hops_to(&net(2)).is_empty());
        assert!(t.hops_to(&net(4)).is_empty());
    }

    #[test]
    fn prune_expired_keeps_fresh_grants_until_their_valid_until() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        t.observe_grant(grant(1, 2, 3)).unwrap();
        // Fresh grants (60-second default TTL) are never
        // pruned by `prune_expired` (it uses `now` as the
        // cutoff, and the grant outlives `now`).
        let pruned = t.prune_expired();
        assert_eq!(pruned, 0);
    }

    #[test]
    fn observed_grant_visible_via_peered_networks() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        t.observe_grant(grant(1, 2, 3)).unwrap();
        let peered = t.peered_networks();
        // Cost=1 grants are considered "peered".
        assert!(peered.contains(&net(2)));
    }

    #[test]
    fn cost_override_above_one_not_counted_as_peered() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let mut g = grant(1, 2, 3);
        g.cost = 5;
        g = g.with_signature("zz");
        t.observe_grant(g).unwrap();
        let peered = t.peered_networks();
        // The hop is installed but cost>1 means it's
        // not "peered" (the StaticTopology definition
        // only counts cost==1 hops).
        assert!(!peered.contains(&net(2)));
    }

    #[test]
    fn strict_capability_rejects_after_grant_applied() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        t.observe_grant(grant(1, 2, 3)).unwrap();
        let allowed = nid(42);
        let router = TransitRouter::new(
            TransitConfig {
                local_network: net(1),
                capability: TransitCapability::Strict {
                    allowlist: vec![allowed.clone()],
                },
            },
            t.clone(),
        );
        // Allowed source → Forward.
        let d = router.decide(&allowed, &net(99), &net(2));
        assert!(matches!(d, TransitDecision::Forward { .. }));
        // Disallowed source → Drop.
        let d = router.decide(&nid(99), &net(99), &net(2));
        assert!(matches!(d, TransitDecision::Drop { .. }));
    }

    // ─────────────────────── observe_grant_verified ───────────────────────

    use a3net_mesh_coordinator::{
        PeeringGrantSigner, StaticPubkeyRegistry,
    };

    #[test]
    fn verified_path_accepts_signed_grant() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(net(1), pk);

        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        let signed = signer.sign(g).unwrap();
        t.observe_grant_verified(signed, &reg).unwrap();
        assert_eq!(t.applied_count(), 1);
    }

    #[test]
    fn verified_path_rejects_unsigned_grant() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(net(1), pk);

        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = grant(1, 2, 3); // no real signature — just "deadbeef"
        let err = t.observe_grant_verified(g, &reg).unwrap_err();
        assert!(matches!(err, GossipApplyError::SignatureInvalid(_)));
    }

    #[test]
    fn verified_path_rejects_wrong_pubkey() {
        let signer_a = PeeringGrantSigner::generate();
        let signer_b = PeeringGrantSigner::generate();
        let mut reg = StaticPubkeyRegistry::new();
        // Registry has A's pubkey; grant is signed by B.
        reg.register(net(1), signer_a.public_key());

        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        let signed = signer_b.sign(g).unwrap();
        let err = t.observe_grant_verified(signed, &reg).unwrap_err();
        assert!(matches!(err, GossipApplyError::SignatureInvalid(_)));
    }

    #[test]
    fn verified_path_rejects_unknown_coordinator() {
        let signer = PeeringGrantSigner::generate();
        let reg = StaticPubkeyRegistry::new(); // empty

        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        let signed = signer.sign(g).unwrap();
        let err = t.observe_grant_verified(signed, &reg).unwrap_err();
        assert!(matches!(err, GossipApplyError::UnknownCoordinator(_)));
    }

    #[test]
    fn verified_path_rejects_tampered_target() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(net(1), pk);

        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        let mut signed = signer.sign(g).unwrap();
        // Tamper after signing.
        signed.target = net(99);
        let err = t.observe_grant_verified(signed, &reg).unwrap_err();
        assert!(matches!(err, GossipApplyError::SignatureInvalid(_)));
    }

    #[test]
    fn verified_path_rejects_expired() {
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(net(1), pk);

        let t = GossipFederatedTopology::new(nid(7), net(1));
        let g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        let mut signed = signer.sign(g).unwrap();
        signed.valid_until = chrono::Utc::now() - chrono::Duration::seconds(1);
        let err = t.observe_grant_verified(signed, &reg).unwrap_err();
        assert!(matches!(err, GossipApplyError::Expired { .. }));
    }

    #[test]
    fn verified_path_accepts_signed_grant_after_full_flow() {
        // End-to-end: coordinator issues grant → signer
        // signs → topology ingests via verified path →
        // router picks the new path.
        use a3net_mesh_coordinator::Peerings;
        let signer = PeeringGrantSigner::generate();
        let pk = signer.public_key();
        let mut reg = StaticPubkeyRegistry::new();
        reg.register(net(1), pk);

        let peerings: Box<dyn Peerings> = Box::new(InMemoryPeerings::new());
        let g = PeeringGrant::new_unsigned(
            net(1),
            net(2),
            nid(3),
            Duration::from_secs(60),
        )
        .unwrap();
        let id = g.grant_id.clone();
        let stored = peerings.issue(g).unwrap();
        let signed = signer.sign(stored).unwrap();
        peerings.attach_signature(&id, signed.signature.clone()).unwrap();

        let t = GossipFederatedTopology::new(nid(7), net(1));
        let from_store = peerings.get(&id).unwrap();
        t.observe_grant_verified(from_store, &reg).unwrap();

        let router = TransitRouter::new(
            TransitConfig::permissive(net(1)),
            t.clone(),
        );
        let d = router.decide(&nid(99), &net(99), &net(2));
        match d {
            TransitDecision::Forward {
                via_network,
                next_hop,
                cost,
            } => {
                assert_eq!(via_network, net(1));
                assert_eq!(next_hop, nid(3));
                assert_eq!(cost, 1);
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    /// Build a grant with an explicit `valid_until` for the
    /// pruning tests.
    fn grant_expiring_at(
        source: u8,
        target: u8,
        grantor: u8,
        valid_until: DateTime<Utc>,
    ) -> PeeringGrant {
        let mut g = grant(source, target, grantor);
        g.valid_until = valid_until;
        g
    }

    /// Regression for the v1 `prune_expired_at` no-op: expired
    /// grants must now drop from the applied list and the path
    /// table must no longer route through the pruned network.
    #[test]
    fn prune_expired_at_drops_expired_entries_and_paths() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        // The verifier refuses to accept grants whose
        // `valid_until <= Utc::now()`, so we ingest everything
        // with a future expiry, then drive pruning forward in
        // time.
        let t0 = Utc::now();
        let obs_t0 = t0 + chrono::Duration::seconds(3600);
        t.observe_grant(grant_expiring_at(1, 2, 3, obs_t0)).unwrap();
        t.observe_grant(grant_expiring_at(1, 4, 5, obs_t0)).unwrap();
        t.observe_grant(grant_expiring_at(1, 6, 7, obs_t0)).unwrap();

        assert_eq!(t.applied_count(), 3);

        // Prune one second past the expiry window — everything
        // expires together.
        let pruned = t.prune_expired_at(obs_t0 + chrono::Duration::seconds(1));
        assert_eq!(pruned, 3, "all three grants should be pruned");
        assert_eq!(t.applied_count(), 0);
        // The path table is empty.
        assert!(t.peered_networks().is_empty());
    }

    /// Pruning on an empty topology must be a no-op (`0`) and
    /// must not error.
    #[test]
    fn prune_expired_at_on_empty_topology_returns_zero() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        assert_eq!(t.prune_expired_at(Utc::now()), 0);
    }

    /// Walking past every grant's `valid_until` removes them
    /// all in a single call.
    #[test]
    fn prune_expired_at_at_long_future_drops_all() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        // Ingest all grants with a uniform future expiry so
        // `observe_grant` doesn't reject them outright.
        let obs_t0 = Utc::now() + chrono::Duration::seconds(60);
        for (i, target) in [2u8, 3u8].iter().enumerate() {
            // Avoid granting the same network as the local
            // network (which would be a self-loop).
            let mut g = grant_expiring_at(1, *target, 9, obs_t0);
            g.direction = PeeringDirection::SourceToTarget;
            // Use target's 0 as a stand-in for unique source
            // per iteration to dodge the self-loop rejection
            // when target == local.
            if *target == 1 {
                let _ = i; // silence unused warning
                continue;
            }
            t.observe_grant(g).unwrap();
        }
        let applied = t.applied_count();
        assert_eq!(applied, 2);
        let pruned = t.prune_expired_at(obs_t0 + chrono::Duration::seconds(1));
        assert_eq!(pruned, 2);
        assert_eq!(t.applied_count(), 0);
    }

    /// Boundary condition: a grant whose `valid_until` is one
    /// nanosecond in the future is not yet pruned.
    #[test]
    fn prune_expired_at_keeps_grants_at_or_after_valid_until() {
        let t = GossipFederatedTopology::new(nid(7), net(1));
        let obs_t0 = Utc::now() + chrono::Duration::seconds(3600);
        t.observe_grant(grant_expiring_at(1, 2, 3, obs_t0)).unwrap();
        // One second earlier — not yet expired.
        assert_eq!(t.prune_expired_at(obs_t0 - chrono::Duration::seconds(1)), 0);
        // One second past `valid_until` — pruned.
        assert_eq!(t.prune_expired_at(obs_t0 + chrono::Duration::seconds(1)), 1);
    }
}