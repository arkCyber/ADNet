//! Firewall rule model.
//!
//! A [`Rule`] is a 4-tuple:
//!
//! - `direction` — does the rule apply to inbound or outbound
//!   traffic?
//! - `action` — allow or deny.
//! - `proto` / `port` — what layer-4 filter to apply.
//! - `peer` — optional [`NodeId`](a3net_types::NodeId) scope. A
//!   rule with no peer filter applies to every peer.
//!
//! The rule model deliberately mirrors the rayfish `ray
//! firewall add in allow -p tcp --port 22 --peer alice` CLI so
//! operators with rayfish muscle memory find the same shape.

use std::net::IpAddr;

use a3net_types::NodeId;
use serde::{Deserialize, Serialize};

/// Maximum number of rules in a [`RuleSet`]. Picked to match
/// the typical per-mesh firewall rule count and to keep the
/// linear scan bounded.
pub const MAX_RULES: usize = 1024;

/// Maximum allowed port number.
pub const MAX_PORT: u16 = 65535;

/// Direction of a packet flow relative to the local node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Peer → local node.
    In,
    /// Local node → peer.
    Out,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::In => "in",
            Self::Out => "out",
        })
    }
}

/// Layer-4 protocol filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtoSpec {
    /// IPv4 / IPv6 ICMP (protocol 1 / 58).
    Icmp,
    /// TCP (protocol 6).
    Tcp,
    /// UDP (protocol 17).
    Udp,
    /// Any protocol. Use sparingly — a single-port rule is
    /// usually more useful.
    Any,
}

impl ProtoSpec {
    /// Whether a packet's protocol matches this spec.
    pub fn matches(self, proto: u8) -> bool {
        match self {
            Self::Any => true,
            Self::Icmp => proto == 1 || proto == 58,
            Self::Tcp => proto == 6,
            Self::Udp => proto == 17,
        }
    }
}

impl std::fmt::Display for ProtoSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Any => "any",
            Self::Icmp => "icmp",
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        })
    }
}

/// Layer-4 port filter.
///
/// `Any` means "all ports". `Single(n)` matches a single
/// destination port. (Source-port filtering is rare and
/// deliberately not modelled.)
///
/// **Constructing `Range(lo, hi)` directly bypasses the
/// `lo ≤ hi` invariant.** Use [`PortSpec::range`] for
/// validation or call [`PortSpec::is_valid`] afterwards.
/// A malformed range silently never matches anything in
/// [`PortSpec::matches`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSpec {
    Any,
    Single(u16),
    /// Inclusive range. Direct construction is unchecked;
    /// use [`PortSpec::range`] for the validated form.
    Range(u16, u16),
}

impl PortSpec {
    /// Validated constructor for [`PortSpec::Range`].
    /// Returns `Err` if `lo > hi`.
    pub fn range(lo: u16, hi: u16) -> Result<Self, PortRangeError> {
        if lo > hi {
            return Err(PortRangeError { lo, hi });
        }
        Ok(Self::Range(lo, hi))
    }

    /// Whether the variant is internally well-formed.
    /// A `Range(lo, hi)` with `lo > hi` is invalid and
    /// `matches()` will never return `true` for it.
    pub fn is_valid(self) -> bool {
        match self {
            Self::Range(lo, hi) => lo <= hi,
            _ => true,
        }
    }

    pub fn matches(self, port: u16) -> bool {
        match self {
            Self::Any => true,
            Self::Single(p) => p == port,
            Self::Range(lo, hi) if lo <= hi => port >= lo && port <= hi,
            // Invalid range — never matches.
            Self::Range(_, _) => false,
        }
    }

    pub fn is_any(self) -> bool {
        matches!(self, Self::Any)
    }
}

/// Error returned by [`PortSpec::range`] when the caller
/// provides `lo > hi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRangeError {
    pub lo: u16,
    pub hi: u16,
}

impl std::fmt::Display for PortRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid port range: lo ({}) must be <= hi ({})",
            self.lo, self.hi
        )
    }
}

impl std::error::Error for PortRangeError {}

impl std::fmt::Display for PortSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => f.write_str("any"),
            Self::Single(p) => write!(f, "{p}"),
            Self::Range(lo, hi) => write!(f, "{lo}..{hi}"),
        }
    }
}

/// Action taken when a rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Allow,
    Deny,
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        })
    }
}

/// Optional peer scope. `None` means "any peer".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerSpec(pub Option<NodeId>);

impl PeerSpec {
    pub fn any() -> Self {
        Self(None)
    }

    pub fn is_node(&self, node: &NodeId) -> bool {
        match &self.0 {
            None => true,
            Some(p) => p == node,
        }
    }
}

impl std::fmt::Display for PeerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            None => f.write_str("any"),
            Some(n) => write!(f, "{}", n.short()),
        }
    }
}

/// Stable identifier for a rule. The first 64 bits are the
/// rule's monotonic index inside its [`RuleSet`]; the rest is
/// reserved for future use (e.g. cluster-wide deduplication).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleId(u64);

impl RuleId {
    pub const fn from_index(i: usize) -> Self {
        Self(i as u64)
    }
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl std::fmt::Display for RuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rule#{}", self.0)
    }
}

/// A single firewall rule.
///
/// Rules are evaluated in declaration order; the first match
/// wins. A rule with no peer (`PeerSpec::any()`) applies to
/// every peer in that direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub direction: Direction,
    pub action: Action,
    pub proto: ProtoSpec,
    pub port: PortSpec,
    pub peer: PeerSpec,
}

impl Rule {
    /// Construct a rule that allows the given protocol/port
    /// from any peer in the given direction.
    pub fn allow(direction: Direction, proto: ProtoSpec, port: PortSpec) -> Self {
        Self {
            id: RuleId::from_index(0), // overridden by RuleSet::push
            direction,
            action: Action::Allow,
            proto,
            port,
            peer: PeerSpec::any(),
        }
    }

    /// Restrict the rule to a single peer.
    pub fn with_peer(mut self, peer: NodeId) -> Self {
        self.peer = PeerSpec(Some(peer));
        self
    }

    /// Whether this rule applies to the given packet tuple.
    pub fn matches(
        &self,
        direction: Direction,
        proto: u8,
        port: u16,
        peer: &NodeId,
        // Source address is reserved for future use; kept
        // here so the call site is consistent with the conntrack
        // signature.
        _src: IpAddr,
    ) -> bool {
        self.direction == direction
            && self.proto.matches(proto)
            && self.port.matches(port)
            && self.peer.is_node(peer)
    }
}

/// An ordered set of firewall rules.
///
/// Insertion order = evaluation order. Index 0 is the first
/// rule checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    rules: Vec<Rule>,
}

impl RuleSet {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Push a rule. Reassigns its `id` to the new index.
    /// Returns `false` if [`MAX_RULES`] would be exceeded.
    pub fn push(&mut self, mut rule: Rule) -> bool {
        if self.rules.len() >= MAX_RULES {
            return false;
        }
        rule.id = RuleId::from_index(self.rules.len());
        self.rules.push(rule);
        true
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Borrow the rule slice for inspection.
    pub fn as_slice(&self) -> &[Rule] {
        &self.rules
    }

    /// Iterate in declaration order.
    pub fn iter(&self) -> std::slice::Iter<'_, Rule> {
        self.rules.iter()
    }

    /// Remove all rules.
    pub fn clear(&mut self) {
        self.rules.clear();
    }

    /// Remove the rule with the given id. Returns `true` if a
    /// rule was removed.
    pub fn remove(&mut self, id: RuleId) -> bool {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != id);
        let removed = self.rules.len() != before;
        if removed {
            // Reassign ids so they stay dense.
            for (i, r) in self.rules.iter_mut().enumerate() {
                r.id = RuleId::from_index(i);
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> NodeId {
        NodeId::random()
    }

    #[test]
    fn proto_spec_matches_icmp_both_versions() {
        assert!(ProtoSpec::Icmp.matches(1));
        assert!(ProtoSpec::Icmp.matches(58));
        assert!(!ProtoSpec::Icmp.matches(6));
    }

    #[test]
    fn proto_spec_matches_tcp_udp_only() {
        assert!(ProtoSpec::Tcp.matches(6));
        assert!(!ProtoSpec::Tcp.matches(17));
        assert!(ProtoSpec::Udp.matches(17));
        assert!(!ProtoSpec::Udp.matches(6));
    }

    #[test]
    fn proto_spec_any_matches_everything() {
        for p in [0u8, 1, 6, 17, 58, 132, 255] {
            assert!(ProtoSpec::Any.matches(p), "proto {p} should match Any");
        }
    }

    #[test]
    fn port_spec_single_and_range() {
        assert!(PortSpec::Single(22).matches(22));
        assert!(!PortSpec::Single(22).matches(80));
        assert!(PortSpec::Range(8000, 8100).matches(8050));
        assert!(!PortSpec::Range(8000, 8100).matches(7999));
        assert!(!PortSpec::Range(8000, 8100).matches(8101));
        assert!(PortSpec::Any.matches(0));
        assert!(PortSpec::Any.matches(MAX_PORT));
    }

    /// Regression: an inverted range (lo > hi) must NOT
    /// silently match. Earlier versions happily evaluated
    /// `port >= lo && port <= hi` which is always false but
    /// also ambiguous when reading the rule.
    #[test]
    fn port_spec_inverted_range_never_matches() {
        let bad = PortSpec::Range(9000, 8000);
        assert!(!bad.is_valid());
        assert!(!bad.matches(7999));
        assert!(!bad.matches(8000));
        assert!(!bad.matches(8500));
        assert!(!bad.matches(9000));
        assert!(!bad.matches(9001));
    }

    #[test]
    fn port_spec_range_validated_constructor() {
        let ok = PortSpec::range(8000, 8100).unwrap();
        assert!(ok.is_valid());
        assert_eq!(ok, PortSpec::Range(8000, 8100));

        let err = PortSpec::range(9000, 8000).unwrap_err();
        assert_eq!(err.lo, 9000);
        assert_eq!(err.hi, 8000);
        assert_eq!(
            err.to_string(),
            "invalid port range: lo (9000) must be <= hi (8000)"
        );
    }

    #[test]
    fn peer_spec_any_matches_any_node() {
        let p = PeerSpec::any();
        assert!(p.is_node(&node()));
        assert!(p.is_node(&node()));
    }

    #[test]
    fn peer_spec_specific_only_matches_self() {
        let target = node();
        let p = PeerSpec(Some(target.clone()));
        assert!(p.is_node(&target));
        assert!(!p.is_node(&node()));
    }

    #[test]
    fn rule_matches_considers_every_field() {
        let me = node();
        let rule = Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(22));
        let ip: IpAddr = "100.64.0.5".parse().unwrap();
        assert!(rule.matches(Direction::In, 6, 22, &me, ip));
        assert!(!rule.matches(Direction::Out, 6, 22, &me, ip));
        assert!(!rule.matches(Direction::In, 6, 80, &me, ip));
        assert!(!rule.matches(Direction::In, 17, 22, &me, ip));
    }

    #[test]
    fn rule_with_peer_scopes_correctly() {
        let me = node();
        let other = node();
        let rule = Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(22))
            .with_peer(me.clone());
        let ip: IpAddr = "100.64.0.5".parse().unwrap();
        assert!(rule.matches(Direction::In, 6, 22, &me, ip));
        assert!(!rule.matches(Direction::In, 6, 22, &other, ip));
    }

    #[test]
    fn ruleset_push_assigns_dense_ids() {
        let mut rs = RuleSet::new();
        assert!(rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Any)));
        assert!(rs.push(Rule::allow(Direction::Out, ProtoSpec::Udp, PortSpec::Any)));
        assert_eq!(rs.as_slice()[0].id, RuleId::from_index(0));
        assert_eq!(rs.as_slice()[1].id, RuleId::from_index(1));
    }

    #[test]
    fn ruleset_remove_reassigns_ids() {
        let mut rs = RuleSet::new();
        rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Any));
        rs.push(Rule::allow(Direction::In, ProtoSpec::Udp, PortSpec::Any));
        rs.push(Rule::allow(Direction::Out, ProtoSpec::Tcp, PortSpec::Any));
        let removed = rs.remove(RuleId::from_index(1));
        assert!(removed);
        assert_eq!(rs.len(), 2);
        // IDs are re-assigned to stay dense.
        assert_eq!(rs.as_slice()[0].id, RuleId::from_index(0));
        assert_eq!(rs.as_slice()[1].id, RuleId::from_index(1));
    }

    #[test]
    fn ruleset_respects_max_rules() {
        let mut rs = RuleSet::new();
        for _ in 0..MAX_RULES {
            assert!(rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Any)));
        }
        // One more should fail.
        assert!(!rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Any)));
    }

    #[test]
    fn ruleset_clear_empties() {
        let mut rs = RuleSet::new();
        rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Any));
        rs.clear();
        assert!(rs.is_empty());
    }
}
