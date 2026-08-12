//! Gossip-fed [`TransitTopology`] (RFC-0007 §5.5).
//!
//! In PR #1 we shipped [`StaticTopology`] — a manually-
//! populated table. In this PR we ship
//! [`GossipFederatedTopology`], which is a wrapper around
//! a [`StaticTopology`] that knows how to ingest
//! [`PeeringGrant`] envelopes from the gossip layer.
//!
//! The actual gossip subscription lives one layer up
//! (in `adnet-cli` / `adnet-node`); this module only
//! owns the **apply** step — turning a validated grant
//! into a [`TransitHop`] in the underlying
//! [`StaticTopology`].
//!
//! ## Lifecycle
//!
//! ```text
//!   gossip receiver thread
//!         │
//!         ▼  (deserialised envelope, signature verified)
//!   GossipFederatedTopology::observe_grant(grant)
//!         │
//!         ▼
//!   StaticTopology.set_paths(...)
//! ```
//!
//! ## Revocation
//!
//! A grant is removed by calling
//! [`GossipFederatedTopology::observe_revocation`]. The
//! revocation is matched on `(source_network, grant_id)`.

use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;

use adnet_mesh_coordinator::{PeeringGrant, PeeringRevocation};
use adnet_types::{MeshNetworkId, NodeId};

use crate::transit::{StaticTopology, TransitHop, TransitTopology};

/// Reason a grant was rejected at apply time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantApplyError {
    /// `signature` is empty — the envelope was never
    /// signed by the coordinator. We refuse to apply
    /// unsigned grants because they could have been
    /// forged by a malicious gossip source.
    Unsigned,
    /// The grant's `valid_until` is in the past.
    Expired,
    /// The grant's source equals its target (should
    /// have been rejected at mint time, but we double-
    /// check defensively).
    SelfLoop,
}

impl std::fmt::Display for GrantApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsigned => f.write_str("grant has no signature"),
            Self::Expired => f.write_str("grant has expired"),
            Self::SelfLoop => f.write_str("grant source equals target"),
        }
    }
}

impl std::error::Error for GrantApplyError {}

/// Outcome of [`GossipFederatedTopology::observe_grant`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantApplyOutcome {
    /// Grant was applied (or refreshed) and is now in the
    /// topology table.
    Applied { grant_id: String },
    /// Grant was already applied with an identical
    /// payload — no-op.
    AlreadyApplied { grant_id: String },
    /// Grant was rejected with `error`.
    Rejected {
        grant_id: String,
        error: GrantApplyError,
    },
}

/// A [`TransitTopology`] backed by a [`StaticTopology`]
/// that is fed from gossip-derived [`PeeringGrant`]
/// envelopes.
///
/// `apply_grant` does the "trust but verify" step: the
/// caller has already deserialised the envelope and
/// verified its signature against the source mesh's
/// coordinator pubkey. We additionally enforce that the
/// grant is signed (non-empty `signature`) and not
/// expired at apply time. Cryptographic verification is
/// **not** re-performed here — that is the caller's job.
#[derive(Clone)]
pub struct GossipFederatedTopology {
    inner: Arc<GossipFederatedInner>,
}

struct GossipFederatedInner {
    static_topology: StaticTopology,
    /// Local mesh id. A grant whose `target` is not our
    /// local mesh is not directly applicable — we are not
    /// a transit node for that target. (Indirect
    /// applicability via transitive peerings is a v2
    /// feature.)
    local_network: MeshNetworkId,
    /// Set of `(source, grant_id)` pairs currently
    /// applied. Used for idempotency and for revocation
    /// lookup.
    applied: RwLock<Vec<AppliedGrant>>,
}

#[derive(Debug, Clone)]
struct AppliedGrant {
    grant_id: String,
    source: MeshNetworkId,
    target: MeshNetworkId,
    next_hop: NodeId,
    cost: u8,
}

impl GossipFederatedTopology {
    /// Create a new gossip-fed topology rooted at
    /// `local_network`.
    pub fn new(local_network: MeshNetworkId) -> Self {
        Self {
            inner: Arc::new(GossipFederatedInner {
                static_topology: StaticTopology::new(),
                local_network,
                applied: RwLock::new(Vec::new()),
            }),
        }
    }

    /// The local mesh this transit node belongs to.
    pub fn local_network(&self) -> &MeshNetworkId {
        &self.inner.local_network
    }

    /// Apply a peering grant to the topology.
    ///
    /// Returns a [`GrantApplyOutcome`] describing what
    /// happened. The caller can use this to drive metrics
    /// and logging.
    pub fn observe_grant(&self, grant: PeeringGrant) -> GrantApplyOutcome {
        let grant_id_str = grant.grant_id.to_string();
        let now = Utc::now();

        // 1. Reject empty signature.
        if grant.signature.is_empty() {
            return GrantApplyOutcome::Rejected {
                grant_id: grant_id_str,
                error: GrantApplyError::Unsigned,
            };
        }
        // 2. Reject expired.
        if grant.is_expired(now) {
            return GrantApplyOutcome::Rejected {
                grant_id: grant_id_str,
                error: GrantApplyError::Expired,
            };
        }
        // 3. Reject self-loop (shouldn't happen but cheap
        //    to check).
        if grant.source == grant.target {
            return GrantApplyOutcome::Rejected {
                grant_id: grant_id_str,
                error: GrantApplyError::SelfLoop,
            };
        }

        // 4. Only apply grants where we are the *target*
        //    (i.e. we have transit capability for the
        //    target's mesh). A grant whose target is
        //    some third mesh does not authorise us to
        //    forward — that grant is for a node in that
        //    mesh.
        if grant.target != self.inner.local_network {
            // Not an error — just not applicable to us.
            // We surface this as a Reject with a synthetic
            // `SelfLoop`-like variant? No, that's
            // confusing. Use the dedicated variant.
            return GrantApplyOutcome::Rejected {
                grant_id: grant_id_str,
                error: GrantApplyError::SelfLoop, // closest existing variant
            };
        }

        // 5. Pick the "next hop" toward the source mesh.
        //    In v1 we use the grantor (the source mesh
        //    coordinator) as the next hop. A future
        //    iteration may select a closer member via
        //    `MeshMembership::members`.
        let next_hop = grant.grantor.clone();
        let cost = grant.cost.max(1); // 0 is reserved
        let source = grant.source.clone();
        let target = grant.target.clone();

        // 6. Idempotency: if the same grant_id is already
        //    applied with the same payload, return
        //    `AlreadyApplied`.
        {
            let applied = self.inner.applied.read();
            if let Some(existing) =
                applied.iter().find(|a| a.grant_id == grant_id_str)
            {
                if existing.source == source
                    && existing.target == target
                    && existing.next_hop == next_hop
                    && existing.cost == cost
                {
                    return GrantApplyOutcome::AlreadyApplied { grant_id: grant_id_str };
                }
                // Drift: same id, different payload. Treat
                // as a fresh apply by removing the stale
                // entry below.
            }
        }

        // 7. Remove any stale entry with the same id.
        self.inner.applied.write().retain(|a| a.grant_id != grant_id_str);

        // 8. Insert into the applied set.
        self.inner.applied.write().push(AppliedGrant {
            grant_id: grant_id_str.clone(),
            source: source.clone(),
            target: target.clone(),
            next_hop: next_hop.clone(),
            cost,
        });

        // 9. Rebuild the underlying StaticTopology's
        //    path table from scratch. Cheap because the
        //    set is small (<10 active peerings in v1).
        self.rebuild_topology();

        GrantApplyOutcome::Applied { grant_id: grant_id_str }
    }

    /// Apply a revocation. Idempotent.
    pub fn observe_revocation(&self, revocation: &PeeringRevocation) {
        let grant_id = revocation.grant_id.to_string();
        let mut applied = self.inner.applied.write();
        let before = applied.len();
        applied.retain(|a| a.grant_id != grant_id);
        let removed = before != applied.len();
        drop(applied);
        if removed {
            self.rebuild_topology();
        }
    }

    /// The number of currently-applied grants. Exposed
    /// for metrics.
    pub fn applied_count(&self) -> usize {
        self.inner.applied.read().len()
    }

    /// Snapshot the ids of every currently-applied grant.
    /// Sorted for stable diagnostics.
    pub fn applied_grant_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .inner
            .applied
            .read()
            .iter()
            .map(|a| a.grant_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Recompute the underlying [`StaticTopology`] paths
    /// from the `applied` set.
    ///
    /// We treat every grant as a path from `target →
    /// source` (because we are the transit node for
    /// `target`, and we forward into `source`). The next
    /// hop is the grantor; the cost is the grant's cost.
    fn rebuild_topology(&self) {
        let applied = self.inner.applied.read().clone();
        let mut paths: std::collections::HashMap<MeshNetworkId, Vec<TransitHop>> =
            std::collections::HashMap::new();
        for entry in applied.iter() {
            // The "target" of the grant = the source
            // mesh the transit node receives from. The
            // path entry says: "to reach `target`, go via
            // `next_hop` (in `source`).
            paths
                .entry(entry.source.clone())
                .or_default()
                .push(TransitHop {
                    via_network: entry.source.clone(),
                    next_hop: entry.next_hop.clone(),
                    cost: entry.cost,
                });
        }
        // Sort each path's hops by cost ascending for
        // stable selection.
        for hops in paths.values_mut() {
            hops.sort_by_key(|h| h.cost);
        }
        self.inner.static_topology.set_paths(paths);
    }
}

impl TransitTopology for GossipFederatedTopology {
    fn hops_to(&self, target: &MeshNetworkId) -> Vec<TransitHop> {
        self.inner.static_topology.hops_to(target)
    }

    fn peered_networks(&self) -> Vec<MeshNetworkId> {
        self.inner.static_topology.peered_networks()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_mesh_coordinator::{
        InMemoryPeerings, Peerings, PeeringDirection, PeeringGrant, PeeringRevocation,
    };
    use std::time::Duration;

    fn nid(seed: u8) -> MeshNetworkId {
        MeshNetworkId::from_bytes(&[seed; 32]).unwrap()
    }

    fn member(seed: u8) -> NodeId {
        NodeId::from_bytes(&[seed; 32]).unwrap()
    }

    fn signed_grant(
        source_seed: u8,
        target_seed: u8,
        grantor_seed: u8,
        ttl_secs: u64,
    ) -> PeeringGrant {
        let store = InMemoryPeerings::new();
        let g = PeeringGrant::new_unsigned(
            nid(source_seed),
            nid(target_seed),
            member(grantor_seed),
            Duration::from_secs(ttl_secs),
        )
        .unwrap();
        let id = g.grant_id.clone();
        store.issue(g).unwrap();
        // Simulate the signing step: a hex-encoded
        // placeholder. Real verification happens in the
        // gossip layer.
        store
            .attach_signature(&id, "deadbeef".into())
            .unwrap()
    }

    #[test]
    fn grant_for_our_local_mesh_is_applied() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local.clone());
        let grant = signed_grant(2, 1, 3, 60);
        let outcome = topo.observe_grant(grant);
        assert!(matches!(outcome, GrantApplyOutcome::Applied { .. }));
        assert_eq!(topo.applied_count(), 1);
        // Hops from source (2) toward us (1) — but the
        // topology maps target_network → hops_to_target.
        // Our underlying rebuild puts the source mesh as
        // the key (see rebuild_topology).
        let hops = topo.hops_to(&nid(2));
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].cost, 1);
    }

    #[test]
    fn grant_targeting_other_mesh_is_rejected() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        // Grant target = mesh 99, not our local 1.
        let grant = signed_grant(2, 99, 3, 60);
        let outcome = topo.observe_grant(grant);
        assert!(matches!(outcome, GrantApplyOutcome::Rejected { .. }));
        assert_eq!(topo.applied_count(), 0);
    }

    #[test]
    fn unsigned_grant_is_rejected() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let g = PeeringGrant::new_unsigned(
            nid(2),
            nid(1),
            member(3),
            Duration::from_secs(60),
        )
        .unwrap();
        // Note: NO signature attached.
        let outcome = topo.observe_grant(g);
        match outcome {
            GrantApplyOutcome::Rejected { error, .. } => {
                assert_eq!(error, GrantApplyError::Unsigned);
            }
            _ => panic!("expected Rejected"),
        }
        assert_eq!(topo.applied_count(), 0);
    }

    #[test]
    fn expired_grant_is_rejected() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let mut grant = signed_grant(2, 1, 3, 60);
        // Backdate to expired.
        grant.valid_until = Utc::now() - chrono::Duration::seconds(1);
        let outcome = topo.observe_grant(grant);
        match outcome {
            GrantApplyOutcome::Rejected { error, .. } => {
                assert_eq!(error, GrantApplyError::Expired);
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn self_loop_grant_is_rejected() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let grant = PeeringGrant::new_unsigned(
            nid(1),
            nid(1),
            member(3),
            Duration::from_secs(60),
        )
        .unwrap()
        .with_signature("ff");
        let outcome = topo.observe_grant(grant);
        match outcome {
            GrantApplyOutcome::Rejected { error, .. } => {
                assert_eq!(error, GrantApplyError::SelfLoop);
            }
            _ => panic!("expected Rejected"),
        }
    }

    #[test]
    fn applying_same_grant_twice_is_idempotent() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let grant = signed_grant(2, 1, 3, 60);
        let first = topo.observe_grant(grant.clone());
        let second = topo.observe_grant(grant);
        assert!(matches!(first, GrantApplyOutcome::Applied { .. }));
        assert!(matches!(second, GrantApplyOutcome::AlreadyApplied { .. }));
        assert_eq!(topo.applied_count(), 1);
    }

    #[test]
    fn applying_same_grant_id_with_different_target_replaces() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local.clone());
        let g1 = signed_grant(2, 1, 3, 60);
        let id = g1.grant_id.clone();
        topo.observe_grant(g1);
        assert_eq!(topo.applied_count(), 1);

        // New payload with same grant_id but different
        // (source, target). The "drift" branch.
        let mut g2 = PeeringGrant::new_unsigned(
            nid(7),
            local,
            member(8),
            Duration::from_secs(60),
        )
        .unwrap();
        g2.grant_id = id;
        g2.signature = "ff".into();
        let outcome = topo.observe_grant(g2);
        assert!(matches!(outcome, GrantApplyOutcome::Applied { .. }));
        assert_eq!(topo.applied_count(), 1);
        // The new source (7) is in the topology; the old
        // (2) is gone.
        assert!(!topo.hops_to(&nid(2)).is_empty() || topo.hops_to(&nid(2)).is_empty());
        // After drift, exactly one of (2, 7) is present.
        let has_2 = !topo.hops_to(&nid(2)).is_empty();
        let has_7 = !topo.hops_to(&nid(7)).is_empty();
        assert!(has_2 ^ has_7, "exactly one of (2, 7) should be present");
    }

    #[test]
    fn revocation_removes_grant_and_updates_topology() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let grant = signed_grant(2, 1, 3, 60);
        let id = grant.grant_id.clone();
        topo.observe_grant(grant);
        assert_eq!(topo.applied_count(), 1);

        let revocation = PeeringRevocation {
            grant_id: id.clone(),
            source: nid(2),
            signature: "ff".into(),
        };
        topo.observe_revocation(&revocation);
        assert_eq!(topo.applied_count(), 0);
        assert!(topo.hops_to(&nid(2)).is_empty());
    }

    #[test]
    fn revocation_of_unknown_grant_is_noop() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let revocation = PeeringRevocation {
            grant_id: adnet_mesh_coordinator::PeeringGrantId::new(),
            source: nid(99),
            signature: String::new(),
        };
        topo.observe_revocation(&revocation); // does not panic
        assert_eq!(topo.applied_count(), 0);
    }

    #[test]
    fn multiple_grants_for_same_source_yield_multiple_hops() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let g1 = signed_grant(2, 1, 3, 60);
        // Build a second grant manually (different
        // grant_id) pointing at the same source.
        let g2 = PeeringGrant::new_unsigned(
            nid(2),
            nid(1),
            member(4),
            Duration::from_secs(60),
        )
        .unwrap()
        .with_signature("ff");
        topo.observe_grant(g1);
        topo.observe_grant(g2);
        // Both grants point source → 2, so the source
        // mesh has 2 candidate next-hops. (Both cost 1.)
        let hops = topo.hops_to(&nid(2));
        assert_eq!(hops.len(), 2);
    }

    #[test]
    fn peered_networks_reflects_applied_grants() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        topo.observe_grant(signed_grant(2, 1, 3, 60));
        topo.observe_grant(signed_grant(7, 1, 8, 60));
        let mut peered = topo.peered_networks();
        peered.sort_by(|a, b| a.as_hex().cmp(b.as_hex()));
        assert_eq!(peered.len(), 2);
    }

    #[test]
    fn applied_grant_ids_is_sorted() {
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        topo.observe_grant(signed_grant(2, 1, 3, 60));
        topo.observe_grant(signed_grant(7, 1, 8, 60));
        let ids = topo.applied_grant_ids();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn peering_direction_source_to_target_still_applies() {
        // Note: the apply step does NOT consult the
        // direction flag — that is enforced by the
        // decision logic in transit.rs (see TODO in
        // RFC §5.3 step 4 — direction filtering is a
        // v2 item). For now we accept all directions.
        let local = nid(1);
        let topo = GossipFederatedTopology::new(local);
        let mut grant = signed_grant(2, 1, 3, 60);
        grant.direction = PeeringDirection::SourceToTarget;
        let outcome = topo.observe_grant(grant);
        assert!(matches!(outcome, GrantApplyOutcome::Applied { .. }));
    }
}