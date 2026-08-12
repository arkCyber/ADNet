//! Gateway role — the side that offers Internet
//! connectivity to the mesh.
//!
//! A gateway is a mesh member that:
//!
//! 1. Has the host OS configured to forward IP packets
//!    (`sysctl net.ipv4.ip_forward=1` on Linux, the
//!    `utun` interface in forwarding mode on macOS).
//! 2. Has NAT configured to masquerade mesh-originated
//!    traffic (`iptables -t nat -A POSTROUTING ...`).
//! 3. Has called `Gateway::allow()` to advertise itself
//!    as an exit-node candidate.
//!
//! The gateway advertises its availability on the network
//! gossip topic so clients can pick it. The advert is a
//! [`GatewayAdvert`] record; receiving clients add it to
//! their routing tables.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use adnet_types::NodeId;

use crate::error::ExitResult;

/// Gateway advertisement — published on the network
/// gossip topic so clients can find available gateways.
///
/// `node_id` is the gateway's stable transport identity
/// (the local node's own `NodeId`, supplied at
/// construction time so it is preserved across `allow`
/// cycles). `endpoint` is the gateway's mesh-side
/// endpoint (host + port) that packets should be
/// forwarded to. The actual IP forwarding happens on
/// the gateway's host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayAdvert {
    pub node_id: NodeId,
    pub endpoint: String,
    /// When the gateway last confirmed it was healthy
    /// (e.g. `ray exit-node check`). Clients treat
    /// expiring adverts as "no longer available".
    pub advertised_at: DateTime<Utc>,
}

/// Gateway-side state.
#[derive(Clone)]
pub struct Gateway {
    inner: Arc<GatewayInner>,
    /// The local node's identity. Stored at construction
    /// time so every advert carries the *same* identity
    /// — peers can pin a stable gateway even if the
    /// gateway toggles `allow` / `revoke` repeatedly.
    node_id: NodeId,
}

struct GatewayInner {
    state: RwLock<GatewayState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayState {
    /// Default: the local node is not advertising itself
    /// as a gateway.
    #[default]
    NotOffering,
    /// `ray exit-node allow` was called and the local
    /// node is advertising itself.
    Offering,
    /// The local node was previously a gateway but is
    /// now disabled (e.g. NAT failed to start). The
    /// router must drop non-mesh traffic.
    Disabled,
}

impl Gateway {
    /// Build a gateway with a fixed local `NodeId`.
    ///
    /// Pass the local node's transport identity here. It
    /// is preserved across every [`Gateway::allow`] call
    /// so the resulting [`GatewayAdvert::node_id`] is
    /// stable and clients can pin it.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            inner: Arc::new(GatewayInner {
                state: RwLock::new(GatewayState::default()),
            }),
            node_id,
        }
    }

    /// The gateway's local identity (the value passed to
    /// [`Gateway::new`]).
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Mark the local node as a gateway. Returns the
    /// fresh advert the caller should publish.
    ///
    /// `endpoint` is the host:port string other peers
    /// should forward packets to. The default (empty
    /// string) is accepted for tests and offline
    /// simulations but is **not** a usable endpoint on
    /// a real mesh — production callers must pass a
    /// reachable address.
    pub fn allow(&self, endpoint: impl Into<String>) -> GatewayAdvert {
        let mut state = self.inner.state.write();
        *state = GatewayState::Offering;
        GatewayAdvert {
            node_id: self.node_id.clone(),
            endpoint: endpoint.into(),
            advertised_at: Utc::now(),
        }
    }

    /// Stop advertising. Returns the previous advert so
    /// the caller can publish a "withdrawn" message.
    pub fn revoke(&self) -> ExitResult<()> {
        let mut state = self.inner.state.write();
        match *state {
            GatewayState::NotOffering => {
                // Revoking while not offering is a no-op
                // but not an error — rayfish accepts the
                // command idempotently.
                Ok(())
            }
            _ => {
                *state = GatewayState::NotOffering;
                Ok(())
            }
        }
    }

    /// Mark the gateway as disabled (e.g. NAT failed).
    pub fn disable(&self) {
        let mut state = self.inner.state.write();
        *state = GatewayState::Disabled;
    }

    /// Current gateway state.
    pub fn state(&self) -> GatewayState {
        *self.inner.state.read()
    }

    /// Whether the local node is currently a usable
    /// gateway.
    pub fn is_offering(&self) -> bool {
        matches!(*self.inner.state.read(), GatewayState::Offering)
    }
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new(NodeId::random())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node_id() -> NodeId {
        // A deterministic NodeId so we can assert against
        // it in `allow_uses_construction_node_id`.
        NodeId::from_bytes(&[0xabu8; 32]).unwrap()
    }

    #[test]
    fn default_state_is_not_offering() {
        let g = Gateway::new(test_node_id());
        assert_eq!(g.state(), GatewayState::NotOffering);
        assert!(!g.is_offering());
    }

    #[test]
    fn allow_then_revoke_lifecycle() {
        let g = Gateway::new(test_node_id());
        let _advert = g.allow("100.64.0.1:9999");
        assert!(g.is_offering());
        g.revoke().unwrap();
        assert!(!g.is_offering());
    }

    #[test]
    fn revoke_when_not_offering_is_noop() {
        let g = Gateway::new(test_node_id());
        // Should not error.
        g.revoke().unwrap();
        assert!(!g.is_offering());
    }

    #[test]
    fn disable_marks_unhealthy() {
        let g = Gateway::new(test_node_id());
        g.allow("100.64.0.1:9999");
        assert!(g.is_offering());
        g.disable();
        assert!(!g.is_offering());
        assert_eq!(g.state(), GatewayState::Disabled);
    }

    /// Regression: `Gateway::allow()` MUST return an
    /// advert whose `node_id` matches the local identity
    /// the gateway was constructed with. Earlier
    /// versions generated a fresh `NodeId::random()` on
    /// every call, which broke pinning.
    #[test]
    fn allow_uses_construction_node_id() {
        let id = test_node_id();
        let g = Gateway::new(id.clone());
        let advert = g.allow("100.64.0.1:9999");
        assert_eq!(advert.node_id, id);
        assert_eq!(advert.endpoint, "100.64.0.1:9999");
        // `allow` again — same identity, fresh timestamp.
        let again = g.allow("100.64.0.1:9999");
        assert_eq!(again.node_id, id);
        assert!(again.advertised_at >= advert.advertised_at);
    }

    #[test]
    fn node_id_accessor_returns_construction_value() {
        let id = test_node_id();
        let g = Gateway::new(id.clone());
        assert_eq!(g.node_id(), &id);
    }

    #[test]
    fn default_impl_uses_random_node_id() {
        // `Default` is allowed to pick a random id — but
        // two defaults must not collide.
        let a = Gateway::default();
        let b = Gateway::default();
        assert_ne!(a.node_id(), b.node_id());
    }

    #[test]
    fn gateway_advert_serializes() {
        let advert = GatewayAdvert {
            node_id: NodeId::random(),
            endpoint: "100.64.0.1:9999".into(),
            advertised_at: Utc::now(),
        };
        let s = serde_json::to_string(&advert).unwrap();
        let back: GatewayAdvert = serde_json::from_str(&s).unwrap();
        assert_eq!(back, advert);
    }
}