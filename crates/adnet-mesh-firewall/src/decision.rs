//! Firewall decision — what to do with a packet and why.

use serde::{Deserialize, Serialize};

use crate::rule::Action;

/// Verdict produced by [`crate::FirewallEngine::decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

impl From<Action> for Decision {
    fn from(a: Action) -> Self {
        match a {
            Action::Allow => Self::Allow,
            Action::Deny => Self::Deny,
        }
    }
}

/// Why a packet was decided the way it was.
///
/// Surfaced via [`crate::FirewallEngine::decide`] so callers
/// can log decisions without re-deriving the cause.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionReason {
    /// Matched an explicit allow rule. `AllowRule` wraps the
    /// matched `RuleId` in a struct so the JSON form is
    /// `{"kind":"allow_rule","rule_id":3}` (struct-variant
    /// shape is required by `#[serde(tag = ...)]`).
    AllowRule {
        rule_id: crate::rule::RuleId,
    },
    /// Matched an explicit deny rule.
    DenyRule {
        rule_id: crate::rule::RuleId,
    },
    /// No rule matched and the **default** policy allowed it.
    DefaultAllow,
    /// No rule matched and the **default** policy denied it.
    DefaultDeny,
    /// Inbound packet matching an existing conntrack entry
    /// opened by the local node.
    ConntrackAllow,
    /// Conntrack entry is full (capacity reached).
    ConntrackFull,
}

impl DecisionReason {
    /// Convenience constructor — preserves the call-site
    /// shape used by the engine (`DecisionReason::AllowRule(rule_id)`).
    pub fn allow_rule(id: crate::rule::RuleId) -> Self {
        Self::AllowRule { rule_id: id }
    }
    pub fn deny_rule(id: crate::rule::RuleId) -> Self {
        Self::DenyRule { rule_id: id }
    }
    pub fn rule_id(&self) -> Option<crate::rule::RuleId> {
        match self {
            Self::AllowRule { rule_id } | Self::DenyRule { rule_id } => Some(*rule_id),
            _ => None,
        }
    }
}

impl std::fmt::Display for DecisionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AllowRule { rule_id } => write!(f, "allow-rule {rule_id}"),
            Self::DenyRule { rule_id } => write!(f, "deny-rule {rule_id}"),
            Self::DefaultAllow => f.write_str("default-allow"),
            Self::DefaultDeny => f.write_str("default-deny"),
            Self::ConntrackAllow => f.write_str("conntrack-allow"),
            Self::ConntrackFull => f.write_str("conntrack-full"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Direction, PeerSpec, PortSpec, ProtoSpec, Rule};

    #[test]
    fn decision_from_action() {
        assert_eq!(Decision::from(Action::Allow), Decision::Allow);
        assert_eq!(Decision::from(Action::Deny), Decision::Deny);
    }

    #[test]
    fn decision_is_allowed() {
        assert!(Decision::Allow.is_allowed());
        assert!(!Decision::Deny.is_allowed());
    }

    #[test]
    fn decision_reason_display_includes_id() {
        let r = DecisionReason::allow_rule(crate::rule::RuleId::from_index(3));
        assert_eq!(r.to_string(), "allow-rule rule#3");
    }

    #[test]
    fn decision_reason_serde_roundtrip() {
        let r = DecisionReason::deny_rule(crate::rule::RuleId::from_index(7));
        let s = serde_json::to_string(&r).unwrap();
        let back: DecisionReason = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn decision_reason_conntrack_full_serde() {
        let r = DecisionReason::ConntrackFull;
        let s = serde_json::to_string(&r).unwrap();
        let back: DecisionReason = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn ensure_full_rule_construction_smoke() {
        // Sanity: a Rule constructed via the helper still
        // satisfies the matches contract.
        use std::net::IpAddr;
        let id = adnet_types::NodeId::random();
        let r = Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(22))
            .with_peer(id.clone());
        assert_eq!(r.peer, PeerSpec(Some(id.clone())));
        let ip: IpAddr = "100.64.0.5".parse().unwrap();
        assert!(r.matches(Direction::In, 6, 22, &id, ip));
    }
}
