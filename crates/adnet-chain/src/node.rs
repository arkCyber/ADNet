//! The chain-node runtime scaffold.
//!
//! [`ChainNode`] is intentionally inert today: `start()` validates the
//! config and, if `enabled`, returns a [`ChainNodeHandle`] whose status
//! is always [`ChainSyncStatus::Stopped`]-adjacent placeholders. Wiring
//! up a real chain client (sync loop, RPC, consensus) is future work;
//! this module exists so `adnet-node` and callers have a stable seam to
//! build against.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::ChainNodeConfig;
use crate::error::{ChainError, ChainResult};
use crate::types::ChainStatus;

/// Entry point for the blockchain-node scaffold.
///
/// Holds configuration only; no background task is spawned until a real
/// backend is implemented.
#[derive(Debug, Clone)]
pub struct ChainNode {
    config: ChainNodeConfig,
}

impl ChainNode {
    pub fn new(config: ChainNodeConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ChainNodeConfig {
        &self.config
    }

    /// Validate the config and "start" the chain node.
    ///
    /// If `config.enabled` is `false`, this returns `Ok(None)`: the NAS
    /// server continues to behave exactly as it does without this crate.
    /// If `enabled` is `true`, this currently returns
    /// [`ChainError::Unimplemented`] because no concrete chain backend
    /// exists yet; the [`ChainNodeHandle`] type and [`ChainStatus`]
    /// plumbing are already in place for when one is added.
    pub async fn start(self) -> ChainResult<Option<ChainNodeHandle>> {
        if !self.config.enabled {
            tracing::debug!("adnet-chain: disabled, skipping start");
            return Ok(None);
        }
        tracing::info!(
            kind = %self.config.kind,
            role = ?self.config.role,
            "adnet-chain: enabled but no backend is implemented yet"
        );
        Err(ChainError::Unimplemented(
            "chain backend not implemented; this crate is a framework only",
        ))
    }
}

/// Handle to a running chain node.
///
/// Reserved for future use once [`ChainNode::start`] can actually spawn
/// a backend. The handle already exposes the shape callers will need:
/// a status snapshot and a graceful shutdown.
#[derive(Debug)]
pub struct ChainNodeHandle {
    stopped: Arc<AtomicBool>,
}

impl ChainNodeHandle {
    pub fn status(&self) -> ChainStatus {
        if self.stopped.load(Ordering::SeqCst) {
            ChainStatus::Stopped
        } else {
            ChainStatus::Starting
        }
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChainKind, ChainRole};

    #[tokio::test]
    async fn disabled_config_is_a_noop() {
        let node = ChainNode::new(ChainNodeConfig::default());
        let handle = node.start().await.unwrap();
        assert!(handle.is_none());
    }

    #[tokio::test]
    async fn enabled_config_is_unimplemented_for_now() {
        let node = ChainNode::new(ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Observer));
        let err = node.start().await.unwrap_err();
        assert!(matches!(err, ChainError::Unimplemented(_)));
    }
}
