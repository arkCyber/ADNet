//! Web3 blockchain-node control plane for [`crate::Node`].
//!
//! Feature-gated behind `chain` (= `dep:a3net-chain`). Mirrors the shape
//! of [`crate::nas`]: this module is a thin adapter between `Node` and
//! the `a3net-chain` scaffold. `a3net-chain` itself does not implement a
//! concrete blockchain client yet, so [`start_chain`] currently succeeds
//! only for a disabled config (a no-op) and otherwise surfaces
//! `a3net_chain::ChainError::Unimplemented`.

use a3net_chain::{ChainNode, ChainNodeConfig, ChainNodeHandle};

/// Re-exported so callers of `a3net-node` don't need a direct dependency
/// on `a3net-chain` just to name the handle type.
pub type ChainNodeSeam = ChainNodeHandle;

/// Start the (currently scaffolded) chain-node role for this NAS server.
///
/// Returns `Ok(None)` when `config.enabled` is `false`. Returns
/// `Err(..)` when enabled, since no backend is implemented yet.
pub async fn start_chain(
    config: ChainNodeConfig,
) -> Result<Option<ChainNodeSeam>, a3net_chain::ChainError> {
    ChainNode::new(config).start().await
}
