//! Declarative firewall loader.
//!
//! Operators can ship a YAML (or JSON) spec and apply it
//! via `a3net mesh firewall apply --spec deploy.yaml`. The
//! format mirrors rayfish's `ray apply deploy.yaml` rule
//! shape so a familiar spec drops in.
//!
//! ```yaml
//! networks:
//!   minecraft:
//!     "*":
//!       allows:
//!         "*": "tcp:25565"
//!   infra:
//!     "*":
//!       allows:
//!         admins: "tcp:22"
//! ```
//!
//! Each subject (the network member hostname, or `*` for
//! every host) maps to a set of `allows:` and `denies:`
//! lists. The peer side of the rule can be a hostname (or
//! `*` for everyone). The `proto:port` value is a
//! comma-separated list of tokens.
//!
//! Parsing is intentionally minimal — we don't pull in a
//! full YAML dependency. JSON5 / JSON parsing works through
//! `serde_json` directly (YAML → JSON via the operator's
//! converter is the recommended workflow for now; a
//! future iteration can add `serde_yaml`).

use a3net_types::NodeId;
use serde::{Deserialize, Serialize};

use crate::engine::FirewallEngine;
use crate::rule::{Action, Direction, PeerSpec, PortSpec, ProtoSpec, Rule, RuleSet};

/// Top-level declarative spec.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirewallSpec {
    #[serde(default)]
    pub networks: std::collections::BTreeMap<String, NetworkSpec>,
}

/// Per-network firewall rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSpec {
    /// `peer -> proto:port, proto:port` allow list.
    #[serde(default)]
    pub allows: std::collections::BTreeMap<String, String>,
    /// Same shape as `allows:` but the rules deny instead.
    #[serde(default)]
    pub denies: std::collections::BTreeMap<String, String>,
}

impl FirewallSpec {
    /// Parse a JSON spec into a typed [`FirewallSpec`].
    pub fn parse_json(raw: &str) -> Result<Self, DeclarativeError> {
        serde_json::from_str(raw).map_err(|e| DeclarativeError::Parse(e.to_string()))
    }

    /// Apply this spec to an engine. Existing rules are
    /// replaced wholesale — a spec is the source of truth.
    pub fn apply(
        &self,
        engine: &FirewallEngine,
        peer_resolver: &dyn PeerResolver,
    ) -> Result<ApplyReport, DeclarativeError> {
        let mut ruleset = RuleSet::new();
        let mut report = ApplyReport::default();
        for (network, spec) in &self.networks {
            for (peer_label, raw) in &spec.allows {
                let tokens = parse_token_list(raw).map_err(|e| {
                    DeclarativeError::InvalidRule(network.clone(), e)
                })?;
                for token in tokens {
                    let (proto, port) = parse_token(&token)?;
                    let peer_id = peer_resolver
                        .resolve(peer_label)
                        .ok_or_else(|| DeclarativeError::UnknownPeer(peer_label.clone()))?;
                    let mut rule = Rule::allow(Direction::In, proto, port);
                    rule.peer = PeerSpec(Some(peer_id));
                    if !ruleset.push(rule) {
                        return Err(DeclarativeError::TooManyRules);
                    }
                    report.allows += 1;                }
            }
            for (peer_label, raw) in &spec.denies {
                let tokens = parse_token_list(raw).map_err(|e| {
                    DeclarativeError::InvalidRule(network.clone(), e)
                })?;
                for token in tokens {
                    let (proto, port) = parse_token(&token)?;
                    let peer_id = peer_resolver
                        .resolve(peer_label)
                        .ok_or_else(|| DeclarativeError::UnknownPeer(peer_label.clone()))?;
                    let rule = Rule {
                        id: crate::rule::RuleId::from_index(0),
                        direction: Direction::In,
                        action: Action::Deny,
                        proto,
                        port,
                        peer: PeerSpec(Some(peer_id)),
                    };
                    if !ruleset.push(rule) {
                        return Err(DeclarativeError::TooManyRules);
                    }
                    report.denies += 1;
                }
            }
        }
        engine.replace_rules(ruleset);
        Ok(report)
    }
}

/// Summary of an [`FirewallSpec::apply`] run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub allows: usize,
    pub denies: usize,
}

/// Resolves a peer label (hostname / `*` / alias) into a
/// concrete [`NodeId`].
///
/// Implementations live in the higher-level mesh CLI / daemon
/// crates — `a3net-mesh-firewall` only needs the abstraction
/// so declarative specs can be parsed without coupling to
/// the contact-directory or membership-lookup logic.
pub trait PeerResolver {
    fn resolve(&self, label: &str) -> Option<NodeId>;
}

/// A no-op resolver for tests: resolves `*` to a wildcard
/// [`NodeId`] (all-zeros) so the spec can be parsed but not
/// matched against a real peer. Real callers wire this to
/// the membership table.
pub struct StaticPeerResolver(pub std::collections::HashMap<String, NodeId>);

impl PeerResolver for StaticPeerResolver {
    fn resolve(&self, label: &str) -> Option<NodeId> {
        if label == "*" {
            Some(NodeId::from_hex(
                "0000000000000000000000000000000000000000000000000000000000000000",
            ).ok()?)
        } else {
            self.0.get(label).cloned()
        }
    }
}

/// Declarative loader errors.
#[derive(Debug, thiserror::Error)]
pub enum DeclarativeError {
    #[error("spec parse: {0}")]
    Parse(String),

    #[error("invalid rule for network {0}: {1}")]
    InvalidRule(String, String),

    #[error("unknown peer: {0}")]
    UnknownPeer(String),

    #[error("rule set exceeded MAX_RULES")]
    TooManyRules,
}

fn parse_token_list(raw: &str) -> Result<Vec<String>, String> {
    Ok(raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

fn parse_token(token: &str) -> Result<(ProtoSpec, PortSpec), DeclarativeError> {
    let (proto_str, port_str) = token
        .split_once(':')
        .ok_or_else(|| DeclarativeError::InvalidRule("?".into(), format!("missing ':' in {token:?}")))?;
    let proto = match proto_str.trim() {
        "tcp" => ProtoSpec::Tcp,
        "udp" => ProtoSpec::Udp,
        "icmp" => ProtoSpec::Icmp,
        "any" => ProtoSpec::Any,
        other => {
            return Err(DeclarativeError::InvalidRule(
                "?".into(),
                format!("unknown protocol {other:?}"),
            ))
        }
    };
    let port = match port_str.trim() {
        "any" => PortSpec::Any,
        single => {
            let p: u16 = single.parse().map_err(|_| {
                DeclarativeError::InvalidRule(
                    "?".into(),
                    format!("invalid port {single:?}"),
                )
            })?;
            PortSpec::Single(p)
        }
    };
    Ok((proto, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::FirewallConfig;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn resolver_with(node: NodeId) -> StaticPeerResolver {
        let mut m = HashMap::new();
        m.insert("alice".to_string(), node);
        StaticPeerResolver(m)
    }

    #[test]
    fn parse_token_recognises_each_proto() {
        assert_eq!(parse_token("tcp:22").unwrap().0, ProtoSpec::Tcp);
        assert_eq!(parse_token("udp:53").unwrap().0, ProtoSpec::Udp);
        assert_eq!(parse_token("icmp:any").unwrap().0, ProtoSpec::Icmp);
        assert_eq!(parse_token("any:80").unwrap().0, ProtoSpec::Any);
    }

    #[test]
    fn parse_token_rejects_unknown_proto() {
        assert!(parse_token("sctp:9000").is_err());
    }

    #[test]
    fn parse_token_rejects_missing_colon() {
        assert!(parse_token("tcp22").is_err());
    }

    #[test]
    fn parse_token_rejects_invalid_port() {
        assert!(parse_token("tcp:notaport").is_err());
    }

    #[test]
    fn firewall_spec_parse_json_roundtrip() {
        let raw = r#"{"networks":{"foo":{"allows":{"alice":"tcp:22"}}}}"#;
        let spec = FirewallSpec::parse_json(raw).unwrap();
        assert_eq!(spec.networks.len(), 1);
        assert_eq!(
            spec.networks["foo"].allows.get("alice").unwrap(),
            "tcp:22"
        );
    }

    #[test]
    fn firewall_spec_apply_inserts_rules() {
        let alice = NodeId::random();
        let resolver = resolver_with(alice.clone());
        let spec = FirewallSpec {
            networks: std::collections::BTreeMap::from([(
                "infra".to_string(),
                NetworkSpec {
                    allows: std::collections::BTreeMap::from([(
                        "alice".to_string(),
                        "tcp:22".to_string(),
                    )]),
                    denies: std::collections::BTreeMap::new(),
                },
            )]),
        };
        let engine = FirewallEngine::new(
            FirewallConfig::default(),
            Arc::new(crate::engine::FirewallStats::default()),
        );
        let report = spec.apply(&engine, &resolver).unwrap();
        assert_eq!(report.allows, 1);
        assert_eq!(report.denies, 0);
        assert_eq!(engine.rule_count(), 1);
    }

    #[test]
    fn firewall_spec_apply_handles_multiple_tokens() {
        let peer = NodeId::random();
        // Use a fixed hostname so the resolver lookup is
        // deterministic (short() would be a hex prefix that
        // has no semantic meaning in a YAML spec).
        let resolver = StaticPeerResolver(HashMap::from([(
            "gamer".to_string(),
            peer.clone(),
        )]));
        let spec = FirewallSpec {
            networks: std::collections::BTreeMap::from([(
                "gaming".to_string(),
                NetworkSpec {
                    allows: std::collections::BTreeMap::from([(
                        "gamer".to_string(),
                        "tcp:9000,tcp:8123".to_string(),
                    )]),
                    denies: std::collections::BTreeMap::new(),
                },
            )]),
        };
        let engine = FirewallEngine::new(
            FirewallConfig::default(),
            Arc::new(crate::engine::FirewallStats::default()),
        );
        let report = spec.apply(&engine, &resolver).unwrap();
        assert_eq!(report.allows, 2);
        assert_eq!(engine.rule_count(), 2);
    }

    #[test]
    fn firewall_spec_apply_unknown_peer_errors() {
        let spec = FirewallSpec {
            networks: std::collections::BTreeMap::from([(
                "x".to_string(),
                NetworkSpec {
                    allows: std::collections::BTreeMap::from([(
                        "ghost".to_string(),
                        "tcp:22".to_string(),
                    )]),
                    denies: std::collections::BTreeMap::new(),
                },
            )]),
        };
        let resolver = StaticPeerResolver(HashMap::new());
        let engine = FirewallEngine::new(
            FirewallConfig::default(),
            Arc::new(crate::engine::FirewallStats::default()),
        );
        let err = spec.apply(&engine, &resolver).unwrap_err();
        assert!(matches!(err, DeclarativeError::UnknownPeer(_)));
    }

    #[test]
    fn static_resolver_wildcard_returns_some() {
        let resolver = StaticPeerResolver(HashMap::new());
        let resolved = resolver.resolve("*").unwrap();
        // The wildcard sentinel is all-zeros.
        assert_eq!(
            resolved.as_hex(),
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
        assert!(resolver.resolve("nobody").is_none());
    }
}
