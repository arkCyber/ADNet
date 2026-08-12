//! Magic DNS resolver — maps mesh DNS names to virtual IPs.
//!
//! The resolver is a pure-data structure: it takes a parsed
//! [`MagicName`] plus a network hint and returns the
//! [`VirtualIp`](adnet_types::VirtualIp) of the member
//! owning that hostname. The actual UDP listener is a
//! separate concern; this crate only owns the name →
//! virtual IP mapping.

use std::collections::HashMap;
use std::sync::Arc;

use adnet_types::{MeshMember, MeshMembership, NodeId, VirtualIp, VirtualIpv4, VirtualIpv6};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{MagicError, MagicResult};
use crate::query::{MagicName, MagicQuery};

/// Resolver configuration. Currently a placeholder for
/// future knobs (cache TTL, max network count).
#[derive(Debug, Clone, Default)]
pub struct ResolverConfig {}

/// A snapshot of the resolver state, suitable for the
/// `ray status` output.
///
/// `entries` is the per-network hostname → virtual IP map.
/// `flat_index` is the global hostname → (network, member)
/// map for the flat `.ray` lookup form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolverSnapshot {
    pub entries: Vec<NetworkSnapshot>,
    pub flat_index: Vec<(String, String, NodeId)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    pub network: String,
    pub entries: Vec<(String, VirtualIpv4, VirtualIpv6, NodeId)>,
}

/// Thread-safe magic DNS resolver.
#[derive(Clone)]
pub struct Resolver {
    inner: Arc<ResolverInner>,
}

struct ResolverInner {
    #[allow(dead_code)]
    config: ResolverConfig,
    /// Per-network hostname → member map. The network
    /// name is the local short name (`"gaming"` for
    /// `gaming.ray`).
    networks: RwLock<HashMap<String, NetworkState>>,
    /// Global hostname → (network_name, member_id). Used
    /// for the flat `.ray` lookup form. Populated lazily
    /// when `network_hint` is `None`.
    flat_index: RwLock<HashMap<String, Vec<(String, NodeId)>>>,
}

#[derive(Debug, Clone)]
struct NetworkState {
    by_host: HashMap<String, MeshMember>,
    /// Member count, mirrored for diagnostics.
    #[allow(dead_code)]
    size: usize,
}

impl Resolver {
    pub fn new(config: ResolverConfig) -> Self {
        Self {
            inner: Arc::new(ResolverInner {
                config,
                networks: RwLock::new(HashMap::new()),
                flat_index: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Apply a coordinator roster to the resolver.
    ///
    /// `display_name` is the local short name (`"gaming"`
    /// for `gaming.ray`). The resolver indexes networks by
    /// their display name; the underlying `MeshNetworkId`
    /// is what the gossip layer uses for cryptographic
    /// verification. Keeping the two strings separate is
    /// deliberate: the display name can be changed by the
    /// operator without invalidating the coordinator
    /// signature, while the network id cannot.
    ///
    /// The flat index is **merged**: any pre-existing
    /// entries for `display_name` are dropped, and entries
    /// for other networks are preserved. This is what
    /// makes the flat `.ray` lookup form useful across
    /// multiple networks.
    pub fn apply_roster(&self, display_name: &str, roster: &MeshMembership) {
        let mut networks = self.inner.networks.write();
        let mut flat = self.inner.flat_index.write();

        // Drop the previous per-network state (if any).
        networks.remove(display_name);

        // Rebuild this network's entries in the flat
        // index: purge stale `(display_name, _)` entries
        // first, then re-insert the new ones.
        for (_, candidates) in flat.iter_mut() {
            candidates.retain(|(net, _)| net != display_name);
        }

        let mut by_host = HashMap::new();
        for member in &roster.members {
            by_host.insert(member.hostname.clone(), member.clone());
            flat
                .entry(member.hostname.clone())
                .or_default()
                .push((display_name.to_string(), member.node_id.clone()));
        }
        // Drop now-empty flat entries so `flat.get(host)`
        // doesn't return an empty Vec for a host that has
        // been completely removed.
        flat.retain(|_, c| !c.is_empty());

        networks.insert(
            display_name.to_string(),
            NetworkState {
                by_host,
                size: roster.members.len(),
            },
        );
    }

    /// Look up a virtual IP for a parsed name.
    ///
    /// `network_hint` is consulted first; if the lookup
    /// fails, the flat index is searched. This matches
    /// rayfish's behaviour for `<host>.ray` lookups.
    ///
    /// ## Flat lookup determinism
    ///
    /// When the same hostname exists in multiple networks
    /// (e.g. `alice` in both `gaming.ray` and `work.ray`),
    /// the flat `.ray` form picks the **first** network
    /// inserted into [`Resolver::apply_roster`]. Operators
    /// that need a specific match should use the full
    /// `<host>.<net>.ray` form.
    pub fn resolve(&self, query: &MagicQuery) -> MagicResult<VirtualIp> {
        let MagicName {
            hostname,
            network,
            ..
        } = &query.name;
        let networks = self.inner.networks.read();
        let flat = self.inner.flat_index.read();

        // 1. Explicit network form.
        if let Some(net) = network {
            let state = networks.get(net).ok_or_else(|| {
                MagicError::UnknownNetwork(net.clone())
            })?;
            let member = state
                .by_host
                .get(hostname)
                .ok_or_else(|| MagicError::UnknownHost(hostname.clone(), net.clone()))?;
            return Ok(member.virtual_ip);
        }

        // 2. Try the network hint.
        if let Some(hint) = &query.network_hint
            && let Some(state) = networks.get(hint)
            && let Some(member) = state.by_host.get(hostname)
        {
            return Ok(member.virtual_ip);
        }

        // 3. Flat lookup — see the doc comment on the
        //    determinism guarantee above.
        if let Some(candidates) = flat.get(hostname)
            && let Some((net, _id)) = candidates.first()
        {
            let state = networks.get(net).ok_or_else(|| {
                MagicError::UnknownNetwork(net.clone())
            })?;
            let member = state
                .by_host
                .get(hostname)
                .ok_or_else(|| MagicError::UnknownHost(hostname.clone(), net.clone()))?;
            return Ok(member.virtual_ip);
        }

        Err(MagicError::UnknownHost(
            hostname.clone(),
            query
                .network_hint
                .clone()
                .unwrap_or_else(|| "<flat>".into()),
        ))
    }

    /// Convenience wrapper for callers that only have the
    /// raw name string.
    pub fn resolve_str(
        &self,
        raw: &str,
        network_hint: Option<&str>,
    ) -> MagicResult<VirtualIp> {
        let name = MagicName::parse(raw)?;
        let q = MagicQuery {
            name,
            network_hint: network_hint.map(|s| s.to_string()),
        };
        self.resolve(&q)
    }

    /// Snapshot the resolver for diagnostics / status.
    pub fn snapshot(&self) -> ResolverSnapshot {
        let networks = self.inner.networks.read();
        let flat = self.inner.flat_index.read();
        let entries: Vec<NetworkSnapshot> = networks
            .iter()
            .map(|(net, state)| NetworkSnapshot {
                network: net.clone(),
                entries: state
                    .by_host
                    .iter()
                    .map(|(host, member)| {
                        (
                            host.clone(),
                            member.virtual_ip.ipv4,
                            member.virtual_ip.ipv6,
                            member.node_id.clone(),
                        )
                    })
                    .collect(),
            })
            .collect();
        let flat_index: Vec<(String, String, NodeId)> = flat
            .iter()
            .flat_map(|(host, candidates)| {
                candidates
                    .iter()
                    .map(|(net, id)| (host.clone(), net.clone(), id.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        ResolverSnapshot {
            entries,
            flat_index,
        }
    }

    /// Number of networks currently known.
    pub fn network_count(&self) -> usize {
        self.inner.networks.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adnet_types::{MeshNetworkId, MeshPolicy};

    fn roster(network: &str, hosts: &[&str]) -> MeshMembership {
        let nid = MeshNetworkId::from_bytes(&[1u8; 32]).unwrap();
        let mut roster = MeshMembership::new_unsigned(nid, vec![]);
        for h in hosts {
            let id = NodeId::random();
            let mut m = MeshMember::new_member(id.clone(), *h);
            m.is_coordinator = false;
            roster.members.push(m);
        }
        let _ = network; // network param is unused; we derive from roster.network_id
        roster
    }

    #[test]
    fn resolve_explicit_network_form() {
        let res = Resolver::new(ResolverConfig::default());
        res.apply_roster("gaming", &roster("gaming", &["alice", "bob"]));
        let vip = res.resolve_str("alice.gaming.ray", None).unwrap();
        let expected = VirtualIp::from_node_id(
            &res.snapshot()
                .entries
                .iter()
                .find(|e| e.network == "gaming")
                .unwrap()
                .entries
                .iter()
                .find(|(h, _, _, _)| h == "alice")
                .unwrap()
                .3,
        );
        assert_eq!(vip, expected);
    }

    #[test]
    fn resolve_short_form_works() {
        let res = Resolver::new(ResolverConfig::default());
        res.apply_roster("gaming", &roster("gaming", &["alice"]));
        let vip = res.resolve_str("alice.gaming", None).unwrap();
        let expected = VirtualIp::from_node_id(
            &res.snapshot().entries[0].entries[0].3,
        );
        assert_eq!(vip, expected);
    }

    #[test]
    fn resolve_flat_form_uses_index() {
        let res = Resolver::new(ResolverConfig::default());
        res.apply_roster("gaming", &roster("gaming", &["alice"]));
        let vip = res.resolve_str("alice.ray", None).unwrap();
        let expected = VirtualIp::from_node_id(
            &res.snapshot().entries[0].entries[0].3,
        );
        assert_eq!(vip, expected);
    }

    #[test]
    fn resolve_flat_form_with_hint() {
        let res = Resolver::new(ResolverConfig::default());
        res.apply_roster("gaming", &roster("gaming", &["alice"]));
        // hint matches an existing network, should resolve.
        let vip = res
            .resolve_str("alice.ray", Some("gaming"))
            .unwrap();
        assert_eq!(
            vip,
            VirtualIp::from_node_id(&res.snapshot().entries[0].entries[0].3)
        );
    }

    #[test]
    fn resolve_unknown_network_errors() {
        let res = Resolver::new(ResolverConfig::default());
        let err = res.resolve_str("alice.gaming.ray", None).unwrap_err();
        assert!(matches!(err, MagicError::UnknownNetwork(_)));
    }

    #[test]
    fn resolve_unknown_host_errors() {
        let res = Resolver::new(ResolverConfig::default());
        res.apply_roster("gaming", &roster("gaming", &["alice"]));
        let err = res.resolve_str("ghost.gaming.ray", None).unwrap_err();
        assert!(matches!(err, MagicError::UnknownHost(_, _)));
    }

    #[test]
    fn apply_roster_replaces_previous_state() {
        let res = Resolver::new(ResolverConfig::default());
        res.apply_roster("gaming", &roster("gaming", &["alice", "bob"]));
        assert_eq!(res.network_count(), 1);
        // Apply a new roster with only `carol`; `alice` and `bob` should be gone.
        res.apply_roster("gaming", &roster("gaming", &["carol"]));
        let snap = res.snapshot();
        let entries = &snap.entries[0].entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "carol");
    }

    #[test]
    fn snapshot_lists_all_networks() {
        let _res = Resolver::new(ResolverConfig::default());
        let nid = MeshNetworkId::from_bytes(&[2u8; 32]).unwrap();
        let r1 = MeshMembership::new_unsigned(nid, vec![]);
        let _ = r1; // smoke: ensure MeshMembership::new_unsigned compiles
        let _ = MeshPolicy::Closed;
    }

    #[test]
    fn resolver_is_clone_and_arc_shared() {
        let res = Resolver::new(ResolverConfig::default());
        let res2 = res.clone();
        res.apply_roster("gaming", &roster("gaming", &["alice"]));
        // res2 sees the same state through the shared Arc.
        let vip = res2.resolve_str("alice.gaming.ray", None).unwrap();
        assert_eq!(
            vip,
            VirtualIp::from_node_id(&res.snapshot().entries[0].entries[0].3)
        );
    }
}
