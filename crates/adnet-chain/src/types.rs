//! Chain-agnostic types shared by config, node, and future backends.
//!
//! These are placeholders: enough shape to design the surrounding
//! framework (config, role, status) without committing to a specific
//! chain's wire format yet.

use serde::{Deserialize, Serialize};

/// Which public chain(s) a node can speak to. Intentionally coarse-grained;
/// real chain clients (e.g. an Ethereum execution-layer client, a
/// Substrate-based client) will be added as variants or become a
/// string-keyed registry once the concrete integration is designed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainKind {
    /// No chain backend selected yet; framework is inert.
    None,
    /// Placeholder for a future EVM-compatible chain client.
    Evm,
    /// Placeholder for a future Substrate-based chain client.
    Substrate,
    /// Escape hatch for experimentation before a variant is added.
    Custom(String),
}

impl Default for ChainKind {
    fn default() -> Self {
        ChainKind::None
    }
}

impl std::fmt::Display for ChainKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainKind::None => write!(f, "none"),
            ChainKind::Evm => write!(f, "evm"),
            ChainKind::Substrate => write!(f, "substrate"),
            ChainKind::Custom(name) => write!(f, "{name}"),
        }
    }
}

/// The role a NAS node plays on the chain it participates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainRole {
    /// Node only reads chain state (light client / RPC follower).
    Observer,
    /// Node relays and validates transactions but does not produce blocks.
    FullNode,
    /// Node participates in consensus (validator / miner / authority).
    Validator,
}

impl Default for ChainRole {
    fn default() -> Self {
        ChainRole::Observer
    }
}

/// Snapshot of the chain node's lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainStatus {
    Stopped,
    Starting,
    Syncing,
    Synced,
    Error,
}
