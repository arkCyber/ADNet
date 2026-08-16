//! Routing decision — the core of the exit-node crate.
//!
//! The [`Router`] decides what to do with a packet based
//! on its destination:
//!
//! - **mesh-destination**: forward via the mesh transport.
//!   The mesh's virtual IP range (`100.64.0.0/10` IPv4,
//!   `200::/16` IPv6) is the canonical "this peer is on
//!   the mesh" signal.
//! - **non-mesh destination + active gateway**: forward
//!   via the chosen gateway. The gateway masquerades the
//!   traffic as it leaves the host's network interface.
//! - **non-mesh destination + no active gateway**: drop
//!   the packet. This is the secure-by-default posture
//!   for the A3Net mesh; outbound Internet only happens
//!   when the operator explicitly opts in.

use std::net::IpAddr;
use std::sync::Arc;

use a3net_types::node::NodeId;
use a3net_types::virtual_ip::{MESH_IPV4_BASE, MESH_IPV4_PREFIX_LEN, MESH_IPV6_PREFIX_BYTES, VirtualIp, VirtualIpv4, VirtualIpv6};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::error::ExitResult;
use crate::gateway::Gateway;

/// Routing decision for a single packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteAction {
    /// Forward to another mesh peer via the mesh transport.
    ForwardToMesh,
    /// Forward via the active gateway.
    ForwardViaGateway { gateway: NodeId },
    /// Drop the packet (no gateway + non-mesh destination,
    /// or some other refusal reason).
    Drop { reason: String },
}

impl RouteAction {
    pub fn is_forward(&self) -> bool {
        matches!(self, Self::ForwardToMesh | Self::ForwardViaGateway { .. })
    }
}

/// Router configuration.
///
/// Currently a placeholder; the field exists so that
/// configuration can be extended (e.g. enable per-flow
/// metrics, allow outbound Internet directly when no
/// gateway is configured, etc.) without a breaking change
/// to the public API.
#[derive(Debug, Clone, Default)]
pub struct RouterConfig {
    #[allow(dead_code)]
    _private: (),
}

/// Snapshot of the router's state for the status command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterSnapshot {
    pub client_state: crate::client::ClientState,
    pub gateway_state: crate::gateway::GatewayState,
}

/// Thread-safe routing engine.
#[derive(Clone)]
pub struct Router {
    inner: Arc<RouterInner>,
}

struct RouterInner {
    client: Client,
    gateway: Gateway,
    /// Reserved for future router-level knobs. Currently
    /// only `Default::default()` is stored; the
    /// `RwLock` is in place so we can swap it in
    /// place later without changing the public API.
    #[allow(dead_code)]
    config: RwLock<RouterConfig>,
}

impl Router {
    pub fn new(client: Client, gateway: Gateway, config: RouterConfig) -> Self {
        Self {
            inner: Arc::new(RouterInner {
                client,
                gateway,
                config: RwLock::new(config),
            }),
        }
    }

    /// Convenience constructor with default config.
    pub fn with_default(client: Client, gateway: Gateway) -> Self {
        Self::new(client, gateway, RouterConfig::default())
    }

    /// Decide the route for `destination`.
    pub fn route(&self, destination: IpAddr) -> RouteAction {
        if is_mesh_address(destination) {
            return RouteAction::ForwardToMesh;
        }
        // Non-mesh destination: ask the client whether it
        // has an active gateway.
        match self.inner.client.active_gateway() {
            Ok(gw) => RouteAction::ForwardViaGateway { gateway: gw },
            Err(_) => RouteAction::Drop {
                reason: "non-mesh destination with no active gateway".into(),
            },
        }
    }

    /// Apply a gateway advertisement from the gossip
    /// topic. Currently this is just a no-op on the
    /// router itself — the client tracks its choice
    /// separately — but is wired in so a future iteration
    /// can build a "best available" picker.
    pub fn observe_advert(&self, _advert: &crate::gateway::GatewayAdvert) {}

    /// Snapshot for the status command.
    pub fn snapshot(&self) -> RouterSnapshot {
        RouterSnapshot {
            client_state: self.inner.client.state(),
            gateway_state: self.inner.gateway.state(),
        }
    }

    /// Whether the local node is currently a usable
    /// gateway.
    pub fn is_gateway(&self) -> bool {
        self.inner.gateway.is_offering()
    }

    /// Convenience: route + return Err if the action
    /// is a Drop.
    pub fn require_route(&self, destination: IpAddr) -> ExitResult<RouteAction> {
        let action = self.route(destination);
        match action {
            RouteAction::Drop { .. } => Err(crate::error::ExitError::NoActiveGateway),
            _ => Ok(action),
        }
    }
}

/// Whether an IP address is in the A3Net mesh range.
///
/// We check both IPv4 (`100.64.0.0/10`) and IPv6
/// (`200::/16`).
pub fn is_mesh_address(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => VirtualIpv4::from_std(v4).is_some(),
        IpAddr::V6(v6) => VirtualIpv6::from_std(v6).is_some(),
    }
}

// Re-export so callers don't need to know the constants.
#[allow(dead_code)]
pub(crate) const _MESH_IPV4_PREFIX_LEN: u8 = MESH_IPV4_PREFIX_LEN;
#[allow(dead_code)]
pub(crate) const _MESH_IPV4_BASE: std::net::Ipv4Addr = MESH_IPV4_BASE;
#[allow(dead_code)]
pub(crate) const _MESH_IPV6_PREFIX_BYTES: [u8; 2] = MESH_IPV6_PREFIX_BYTES;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Client, ClientConfig};
    use crate::gateway::Gateway;
    use a3net_types::virtual_ip::{VirtualIp, VirtualIpv4, VirtualIpv6};
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn mesh_destination_routes_to_mesh() {
        let r = Router::with_default(Client::default(), Gateway::default());
        let node_id = NodeId::random();
        let vip = VirtualIp::from_node_id(&node_id);
        let action = r.route(vip.ipv4.as_std().into());
        assert_eq!(action, RouteAction::ForwardToMesh);
    }

    #[test]
    fn mesh_ipv6_destination_routes_to_mesh() {
        let r = Router::with_default(Client::default(), Gateway::default());
        let node_id = NodeId::random();
        let vip = VirtualIp::from_node_id(&node_id);
        let action = r.route(vip.ipv6.as_std().into());
        assert_eq!(action, RouteAction::ForwardToMesh);
    }

    #[test]
    fn non_mesh_destination_without_gateway_drops() {
        let r = Router::with_default(Client::default(), Gateway::default());
        let action = r.route("8.8.8.8".parse().unwrap());
        assert!(matches!(action, RouteAction::Drop { .. }));
    }

    #[test]
    fn non_mesh_destination_with_gateway_routes_via_gateway() {
        let client = Client::default();
        let gw = NodeId::random();
        client.use_gateway(gw.clone()).unwrap();
        let r = Router::with_default(client, Gateway::default());
        let action = r.route("8.8.8.8".parse().unwrap());
        assert_eq!(action, RouteAction::ForwardViaGateway { gateway: gw });
    }

    #[test]
    fn require_route_returns_err_on_drop() {
        let r = Router::with_default(Client::default(), Gateway::default());
        let err = r.require_route("1.1.1.1".parse().unwrap()).unwrap_err();
        // We return NoActiveGateway for any Drop. The
        // caller can pattern-match on the reason if
        // needed.
        assert!(matches!(err, crate::error::ExitError::NoActiveGateway));
    }

    #[test]
    fn snapshot_includes_client_and_gateway() {
        let client = Client::default();
        let gw = NodeId::random();
        client.use_gateway(gw).unwrap();
        let r = Router::with_default(client, Gateway::default());
        let snap = r.snapshot();
        assert!(matches!(
            snap.client_state,
            crate::client::ClientState::Using { .. }
        ));
        assert_eq!(snap.gateway_state, crate::gateway::GatewayState::NotOffering);
    }

    #[test]
    fn is_mesh_address_ipv4_boundary() {
        assert!(is_mesh_address("100.64.0.0".parse().unwrap()));
        assert!(is_mesh_address("100.64.0.1".parse().unwrap()));
        assert!(is_mesh_address("100.127.255.255".parse().unwrap()));
        assert!(!is_mesh_address("100.128.0.0".parse().unwrap()));
        assert!(!is_mesh_address("8.8.8.8".parse().unwrap()));
        assert!(!is_mesh_address("192.168.0.1".parse().unwrap()));
    }

    #[test]
    fn is_mesh_address_ipv6_boundary() {
        assert!(is_mesh_address("200::1".parse().unwrap()));
        assert!(is_mesh_address("200::".parse().unwrap()));
        assert!(!is_mesh_address("::1".parse().unwrap()));
        assert!(!is_mesh_address("201::1".parse().unwrap()));
        assert!(!is_mesh_address("fe80::1".parse().unwrap()));
    }

    #[test]
    fn action_is_forward_matches() {
        assert!(RouteAction::ForwardToMesh.is_forward());
        assert!(RouteAction::ForwardViaGateway {
            gateway: NodeId::random()
        }
        .is_forward());
        assert!(!RouteAction::Drop {
            reason: "test".into()
        }
        .is_forward());
    }

    #[test]
    fn virtual_ipv4_from_std_roundtrip() {
        let ip = Ipv4Addr::new(100, 64, 1, 1);
        assert!(VirtualIpv4::from_std(ip).is_some());
        let ip2 = Ipv4Addr::new(8, 8, 8, 8);
        assert!(VirtualIpv4::from_std(ip2).is_none());
    }

    #[test]
    fn virtual_ipv6_from_std_roundtrip() {
        let ip: Ipv6Addr = "200::1".parse().unwrap();
        assert!(VirtualIpv6::from_std(ip).is_some());
        let ip2: Ipv6Addr = "::1".parse().unwrap();
        assert!(VirtualIpv6::from_std(ip2).is_none());
    }

    #[test]
    fn config_default_is_empty() {
        let cfg = RouterConfig::default();
        // Just verify it constructs.
        let _ = cfg;
    }

    #[test]
    fn client_strict_config_preserved() {
        let c = Client::new(ClientConfig::default());
        assert!(c.config().require_explicit_gateway);
    }
}
