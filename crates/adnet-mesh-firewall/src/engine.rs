//! Firewall engine — wires rules + conntrack + default
//! policy into a single `decide()` entry point used by the
//! mesh stack's packet path.

use std::net::IpAddr;
use std::sync::Arc;

use adnet_observability::prelude::{Counter, Gauge};
use adnet_types::NodeId;

use crate::conntrack::{ConnProto, ConnTracker, ConnTrackerConfig, ConnTrackerError};
use crate::decision::{Decision, DecisionReason};
use crate::rule::{Action, Direction, RuleSet};

/// Default policy used when no rule matches.
///
/// Mirrors the rayfish / Tailscale default: deny unsolicited
/// inbound TCP/UDP, allow inbound ICMP, allow all outbound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultPolicy {
    pub inbound_tcp_udp: Action,
    pub inbound_icmp: Action,
    pub outbound: Action,
}

impl Default for DefaultPolicy {
    fn default() -> Self {
        Self {
            inbound_tcp_udp: Action::Deny,
            inbound_icmp: Action::Allow,
            outbound: Action::Allow,
        }
    }
}

/// Top-level firewall configuration.
#[derive(Debug, Clone)]
pub struct FirewallConfig {
    pub rules: RuleSet,
    pub default_policy: DefaultPolicy,
    pub conntrack: ConnTrackerConfig,
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            rules: RuleSet::new(),
            default_policy: DefaultPolicy::default(),
            conntrack: ConnTrackerConfig::default(),
        }
    }
}

/// Counters exposed to [`adnet_observability`].
#[derive(Debug)]
pub struct FirewallStats {
    pub decisions_total: Counter,
    pub decisions_allowed: Counter,
    pub decisions_denied: Counter,
    pub conntrack_open: Counter,
    pub conntrack_full: Counter,
    pub rule_evaluations: Counter,
    pub active_entries: Gauge,
}

impl Default for FirewallStats {
    fn default() -> Self {
        Self {
            decisions_total: Counter::new(
                "adnet_firewall_decisions_total",
                "Total number of firewall decisions",
            ),
            decisions_allowed: Counter::new(
                "adnet_firewall_decisions_allowed",
                "Decisions that allowed a packet",
            ),
            decisions_denied: Counter::new(
                "adnet_firewall_decisions_denied",
                "Decisions that denied a packet",
            ),
            conntrack_open: Counter::new(
                "adnet_firewall_conntrack_open",
                "Conntrack entries opened",
            ),
            conntrack_full: Counter::new(
                "adnet_firewall_conntrack_full",
                "Conntrack open attempts rejected (table full)",
            ),
            rule_evaluations: Counter::new(
                "adnet_firewall_rule_evaluations",
                "Rule evaluations performed",
            ),
            active_entries: Gauge::new(
                "adnet_firewall_active_conntrack",
                "Live conntrack entry count",
            ),
        }
    }
}

/// Inputs to a single firewall decision.
///
/// `proto` is the raw IP protocol byte (e.g. `6` for TCP,
/// `17` for UDP, `1` for ICMPv4, `58` for ICMPv6).
/// `port` is the **peer-side** port for inbound packets
/// (the port the peer used as their source) and the
/// **destination** port for outbound packets.
#[derive(Debug, Clone, Copy)]
pub struct Packet<'a> {
    pub direction: Direction,
    pub proto: u8,
    pub port: u16,
    pub peer: &'a NodeId,
    pub src_ip: IpAddr,
}

/// Thread-safe firewall engine.
///
/// Cheap to clone (the inner state is `Arc`-shared). Cloning
/// an engine gives every caller a view onto the same rules
/// + conntrack.
#[derive(Clone)]
pub struct FirewallEngine {
    inner: Arc<FirewallEngineInner>,
}

struct FirewallEngineInner {
    rules: parking_lot::RwLock<RuleSet>,
    default_policy: DefaultPolicy,
    conntrack: ConnTracker,
    stats: Arc<FirewallStats>,
}

impl FirewallEngine {
    /// Wrap a config + stats into a fresh engine. `stats`
    /// can be shared across many engines (e.g. the same
    /// process-wide metrics handle).
    pub fn new(config: FirewallConfig, stats: Arc<FirewallStats>) -> Self {
        let conntrack = ConnTracker::new(config.conntrack.clone());
        Self {
            inner: Arc::new(FirewallEngineInner {
                rules: parking_lot::RwLock::new(config.rules),
                default_policy: config.default_policy,
                conntrack,
                stats,
            }),
        }
    }
    /// Replace the rule set. Used by `ray firewall replace`
    /// style commands and by the declarative YAML loader.
    pub fn replace_rules(&self, rules: RuleSet) {
        let mut guard = self.inner.rules.write();
        *guard = rules;
    }

    /// Number of rules currently in the engine.
    pub fn rule_count(&self) -> usize {
        self.inner.rules.read().len()
    }

    /// Snapshot of conntrack entries for the status command.
    pub fn conntrack_snapshot(&self) -> Vec<(crate::conntrack::ConnKey, Direction, std::time::Instant)> {
        self.inner.conntrack.snapshot()
    }

    /// Open an outbound conntrack entry. Public so the
    /// mesh stack can pre-register long-lived flows (e.g.
    /// the embedded SSH server's outbound keep-alives)
    /// without waiting for the first packet.
    pub fn open_outbound(
        &self,
        proto: ConnProto,
        peer: NodeId,
        peer_port: u16,
        peer_ip: IpAddr,
        local_port: u16,
    ) -> Result<(), ConnTrackerError> {
        let sock = std::net::SocketAddr::new(peer_ip, peer_port);
        self.inner.stats.conntrack_open.inc();
        self.inner
            .conntrack
            .open_outbound(proto, peer, peer_port, sock, local_port)
    }

    /// Decide a packet. The packet is described by `pkt`;
    /// the engine returns the verdict and the reason for
    /// logging.
    pub fn decide(&self, pkt: Packet<'_>) -> (Decision, DecisionReason) {
        self.inner.stats.decisions_total.inc();
        self.inner.stats.rule_evaluations.inc();

        // 1. Conntrack path for inbound packets: if the
        //    packet matches an existing outbound flow we
        //    opened, allow it without scanning rules.
        if pkt.direction == Direction::In
            && let Some(probe) = inbound_to_connprobe(pkt)
            && self.inner.conntrack.lookup_inbound(probe)
        {
            self.inner.stats.decisions_allowed.inc();
            self.inner
                .stats
                .active_entries
                .set(self.inner.conntrack.len() as i64);
            return (Decision::Allow, DecisionReason::ConntrackAllow);
        }

        // 2. Explicit rule match (first-wins).
        let rules = self.inner.rules.read();
        for rule in rules.iter() {
            if rule.matches(pkt.direction, pkt.proto, pkt.port, pkt.peer, pkt.src_ip) {
                let decision = Decision::from(rule.action);
                let reason = match rule.action {
                    Action::Allow => DecisionReason::allow_rule(rule.id),
                    Action::Deny => DecisionReason::deny_rule(rule.id),
                };
                match decision {
                    Decision::Allow => self.inner.stats.decisions_allowed.inc(),
                    Decision::Deny => self.inner.stats.decisions_denied.inc(),
                }
                return (decision, reason);
            }
        }

        // 3. Default policy.
        let action = self.default_action(pkt.direction, pkt.proto);
        let decision = Decision::from(action);
        let reason = match action {
            Action::Allow => DecisionReason::DefaultAllow,
            Action::Deny => DecisionReason::DefaultDeny,
        };
        match decision {
            Decision::Allow => self.inner.stats.decisions_allowed.inc(),
            Decision::Deny => self.inner.stats.decisions_denied.inc(),
        }
        (decision, reason)
    }

    fn default_action(&self, direction: Direction, proto: u8) -> Action {
        match direction {
            Direction::In => match proto {
                1 | 58 => self.inner.default_policy.inbound_icmp,
                _ => self.inner.default_policy.inbound_tcp_udp,
            },
            Direction::Out => self.inner.default_policy.outbound,
        }
    }
}

fn inbound_to_connprobe(pkt: Packet<'_>) -> Option<crate::conntrack::InboundProbe<'_>> {
    let proto = match pkt.proto {
        6 => ConnProto::Tcp,
        17 => ConnProto::Udp,
        _ => return None,
    };
    Some(crate::conntrack::InboundProbe {
        proto,
        peer: pkt.peer,
        peer_port: pkt.port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{PortSpec, ProtoSpec, Rule};
    use std::net::Ipv4Addr;
    use std::sync::Arc;

    fn ip_v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn default_policy_inbound_tcp_denied() {
        let eng = FirewallEngine::new(
            FirewallConfig::default(),
            Arc::new(FirewallStats::default()),
        );
        let me = NodeId::random();
        let ip = ip_v4(100, 64, 0, 5);
        let (d, r) = eng.decide(Packet {
            direction: Direction::In,
            proto: 6,
            port: 9000,
            peer: &me,
            src_ip: ip,
        });
        assert_eq!(d, Decision::Deny);
        assert_eq!(r, DecisionReason::DefaultDeny);
    }

    #[test]
    fn default_policy_inbound_icmp_allowed() {
        let eng = FirewallEngine::new(
            FirewallConfig::default(),
            Arc::new(FirewallStats::default()),
        );
        let me = NodeId::random();
        let ip = ip_v4(100, 64, 0, 5);
        let (d, r) = eng.decide(Packet {
            direction: Direction::In,
            proto: 1,
            port: 0,
            peer: &me,
            src_ip: ip,
        });
        assert_eq!(d, Decision::Allow);
        assert_eq!(r, DecisionReason::DefaultAllow);
    }

    #[test]
    fn default_policy_outbound_allowed() {
        let eng = FirewallEngine::new(
            FirewallConfig::default(),
            Arc::new(FirewallStats::default()),
        );
        let me = NodeId::random();
        let ip = ip_v4(100, 64, 0, 5);
        let (d, _) = eng.decide(Packet {
            direction: Direction::Out,
            proto: 6,
            port: 443,
            peer: &me,
            src_ip: ip,
        });
        assert_eq!(d, Decision::Allow);
    }

    #[test]
    fn explicit_allow_rule_overrides_default() {
        let mut cfg = FirewallConfig::default();
        cfg.rules.push(Rule::allow(
            Direction::In,
            ProtoSpec::Tcp,
            PortSpec::Single(22),
        ));
        let eng = FirewallEngine::new(cfg, Arc::new(FirewallStats::default()));
        let me = NodeId::random();
        let ip = ip_v4(100, 64, 0, 5);
        let (d, r) = eng.decide(Packet {
            direction: Direction::In,
            proto: 6,
            port: 22,
            peer: &me,
            src_ip: ip,
        });
        assert_eq!(d, Decision::Allow);
        assert!(matches!(r, DecisionReason::AllowRule { rule_id: _ }));
    }

    #[test]
    fn explicit_deny_rule_overrides_default_allow() {
        let mut cfg = FirewallConfig::default();
        cfg.rules.push(Rule {
            id: crate::rule::RuleId::from_index(0),
            direction: Direction::Out,
            action: Action::Deny,
            proto: ProtoSpec::Tcp,
            port: PortSpec::Single(25),
            peer: crate::rule::PeerSpec::any(),
        });
        let eng = FirewallEngine::new(cfg, Arc::new(FirewallStats::default()));
        let me = NodeId::random();
        let ip = ip_v4(100, 64, 0, 5);
        let (d, _) = eng.decide(Packet {
            direction: Direction::Out,
            proto: 6,
            port: 25,
            peer: &me,
            src_ip: ip,
        });
        assert_eq!(d, Decision::Deny);
    }

    #[test]
    fn conntrack_allows_return_traffic() {
        let eng = FirewallEngine::new(
            FirewallConfig::default(),
            Arc::new(FirewallStats::default()),
        );
        let me = NodeId::random();
        let ip = ip_v4(100, 64, 0, 5);
        // local_port 0 means "any" — inbound lookups use 0 too
        eng.open_outbound(ConnProto::Tcp, me.clone(), 80, ip, 0)
            .unwrap();
        // Inbound packet that matches the conntrack key.
        let (d, r) = eng.decide(Packet {
            direction: Direction::In,
            proto: 6,
            port: 80,
            peer: &me,
            src_ip: ip,
        });
        assert_eq!(d, Decision::Allow);
        assert_eq!(r, DecisionReason::ConntrackAllow);
    }

    #[test]
    fn replace_rules_swaps_state() {
        let mut cfg = FirewallConfig::default();
        cfg.rules.push(Rule::allow(
            Direction::In,
            ProtoSpec::Tcp,
            PortSpec::Single(22),
        ));
        let eng = FirewallEngine::new(cfg, Arc::new(FirewallStats::default()));
        assert_eq!(eng.rule_count(), 1);
        eng.replace_rules(RuleSet::new());
        assert_eq!(eng.rule_count(), 0);
    }

    #[test]
    fn stats_counters_increment() {
        let stats = Arc::new(FirewallStats::default());
        let eng = FirewallEngine::new(FirewallConfig::default(), stats.clone());
        let me = NodeId::random();
        let ip = ip_v4(100, 64, 0, 5);
        eng.decide(Packet {
            direction: Direction::In,
            proto: 6,
            port: 22,
            peer: &me,
            src_ip: ip,
        });
        // Default policy denied the inbound TCP, so denied counter
        // advanced.
        assert!(stats.decisions_denied.get() >= 1);
    }
}
