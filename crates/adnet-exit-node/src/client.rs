//! Client role — the side that *uses* a gateway to reach
//! the public Internet.
//!
//! A client picks exactly one gateway at a time (`ray
//! exit-node use <peer>`). The gateway choice is sticky
//! until the operator changes it (`ray exit-node use
//! other-peer`) or unsets it (`ray exit-node unset`).
//!
//! The client state is what the routing layer reads to
//! decide whether to forward non-mesh traffic via the
//! gateway or drop it.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use adnet_types::NodeId;

use crate::error::{ExitError, ExitResult};

/// Client configuration.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// If true, the client refuses to fall back to a
    /// direct Internet connection when no gateway is set.
    /// This is the secure-by-default posture.
    pub require_explicit_gateway: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            require_explicit_gateway: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientState {
    /// No gateway is configured. Outbound non-mesh
    /// traffic is dropped (or refused if
    /// `require_explicit_gateway` is true).
    Unconfigured,
    /// A gateway is currently in use.
    Using { gateway: NodeId, since: DateTime<Utc> },
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    config: ClientConfig,
    state: RwLock<ClientState>,
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                config,
                state: RwLock::new(ClientState::Unconfigured),
            }),
        }
    }

    /// Set the active gateway. Returns the previous
    /// state so the caller can decide whether to log a
    /// change-of-gateway event.
    pub fn use_gateway(&self, gateway: NodeId) -> ExitResult<ClientState> {
        let mut state = self.inner.state.write();
        let prev = state.clone();
        *state = ClientState::Using {
            gateway,
            since: Utc::now(),
        };
        Ok(prev)
    }

    /// Unset the active gateway. Idempotent: calling
    /// `unset` on an already-unconfigured client is a
    /// no-op.
    pub fn unset(&self) -> ExitResult<()> {
        let mut state = self.inner.state.write();
        *state = ClientState::Unconfigured;
        Ok(())
    }

    /// Active gateway, if any.
    pub fn state(&self) -> ClientState {
        self.inner.state.read().clone()
    }

    /// Active gateway id, or an error if the client is
    /// unconfigured.
    pub fn active_gateway(&self) -> ExitResult<NodeId> {
        match self.inner.state.read().clone() {
            ClientState::Unconfigured => Err(ExitError::NoActiveGateway),
            ClientState::Using { gateway, .. } => Ok(gateway),
        }
    }

    /// Configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new(ClientConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_unconfigured() {
        let c = Client::default();
        assert_eq!(c.state(), ClientState::Unconfigured);
        assert!(c.active_gateway().is_err());
    }

    #[test]
    fn use_then_unset() {
        let c = Client::default();
        let gw = NodeId::random();
        let prev = c.use_gateway(gw.clone()).unwrap();
        assert_eq!(prev, ClientState::Unconfigured);
        assert_eq!(c.active_gateway().unwrap(), gw);
        c.unset().unwrap();
        assert_eq!(c.state(), ClientState::Unconfigured);
    }

    #[test]
    fn use_replaces_previous_gateway() {
        let c = Client::default();
        let gw1 = NodeId::random();
        let gw2 = NodeId::random();
        c.use_gateway(gw1.clone()).unwrap();
        c.use_gateway(gw2.clone()).unwrap();
        assert_eq!(c.active_gateway().unwrap(), gw2);
        assert_ne!(c.active_gateway().unwrap(), gw1);
    }

    #[test]
    fn unset_is_idempotent() {
        let c = Client::default();
        c.unset().unwrap();
        c.unset().unwrap();
        assert_eq!(c.state(), ClientState::Unconfigured);
    }

    #[test]
    fn active_gateway_unconfigured_errors() {
        let c = Client::default();
        let err = c.active_gateway().unwrap_err();
        assert!(matches!(err, ExitError::NoActiveGateway));
    }

    #[test]
    fn client_state_serializes() {
        let s = ClientState::Using {
            gateway: NodeId::random(),
            since: Utc::now(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ClientState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn client_config_default_is_strict() {
        let cfg = ClientConfig::default();
        assert!(cfg.require_explicit_gateway);
    }
}
