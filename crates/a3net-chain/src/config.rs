//! Configuration for the chain-node scaffold.
//!
//! This models "does this NAS server also want to be a blockchain node,
//! and if so how" without wiring up any real chain client yet.

use serde::{Deserialize, Serialize};

use crate::types::{ChainKind, ChainRole};

/// Configuration for [`crate::ChainNode`].
///
/// `enabled = false` (the default) means the NAS node behaves exactly as
/// it does today; nothing in this crate runs unless an operator opts in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainNodeConfig {
    /// Master switch. When `false`, [`crate::ChainNode::start`] is a no-op.
    pub enabled: bool,
    /// Which chain family to join. Ignored while `enabled` is `false`.
    #[serde(default)]
    pub kind: ChainKind,
    /// How deeply this node participates (archive-only vs. validator).
    #[serde(default)]
    pub role: ChainRole,
    /// Directory used for chain data (headers/blocks/state), rooted under
    /// the node's data dir. Reserved for the future backend; the scaffold
    /// does not read or write here yet.
    #[serde(default)]
    pub data_subdir: String,
    /// RPC/P2P listen address for the chain client, once implemented.
    #[serde(default)]
    pub bind: Option<std::net::SocketAddr>,
}

impl Default for ChainNodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: ChainKind::default(),
            role: ChainRole::default(),
            data_subdir: "chain".to_string(),
            bind: None,
        }
    }
}

impl ChainNodeConfig {
    /// Convenience constructor: opt in with a chain kind and role, leaving
    /// the rest at defaults.
    pub fn enabled(kind: ChainKind, role: ChainRole) -> Self {
        Self {
            enabled: true,
            kind,
            role,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let cfg = ChainNodeConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.kind, ChainKind::None);
    }

    #[test]
    fn enabled_constructor_sets_fields() {
        let cfg = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::FullNode);
        assert!(cfg.enabled);
        assert_eq!(cfg.kind, ChainKind::Evm);
        assert_eq!(cfg.role, ChainRole::FullNode);
    }

    #[test]
    fn roundtrips_through_json() {
        let cfg = ChainNodeConfig::enabled(ChainKind::Substrate, ChainRole::Validator);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ChainNodeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, ChainKind::Substrate);
        assert_eq!(back.role, ChainRole::Validator);
    }
}
