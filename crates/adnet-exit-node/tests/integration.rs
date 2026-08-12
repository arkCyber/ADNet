//! End-to-end integration tests for the exit-node stack.
//!
//! These exercise the combined behavior of the gateway
//! and client submodules through the router — the same
//! way `adnet-node` will use them.

use std::net::IpAddr;
use std::sync::Arc;

use adnet_exit_node::{Client, ClientState, Gateway, GatewayState, RouteAction, Router};
use adnet_types::NodeId;

fn gw() -> Router {
    let client = Client::default();
    let gateway = Gateway::new(NodeId::random());
    Router::with_default(client, gateway)
}

#[test]
fn full_lifecycle_allow_and_use() {
    let gateway = Gateway::new(NodeId::random());
    let client = Client::default();
    let router = Router::with_default(client.clone(), gateway.clone());

    // Initially nothing is configured.
    let snap = router.snapshot();
    assert_eq!(snap.client_state, ClientState::Unconfigured);
    assert_eq!(snap.gateway_state, GatewayState::NotOffering);
    assert!(!router.is_gateway());

    // Enable the local node as a gateway.
    gateway.allow("100.64.0.1:9999");
    assert!(router.is_gateway());
    assert!(gateway.is_offering());

    // Pick a (different) node as our egress gateway.
    let egress = NodeId::random();
    client.use_gateway(egress.clone()).unwrap();

    // Public destinations now route via egress.
    let action = router.route("8.8.8.8".parse::<IpAddr>().unwrap());
    assert_eq!(
        action,
        RouteAction::ForwardViaGateway {
            gateway: egress.clone()
        }
    );
}

#[test]
fn dropping_then_revoking_disables_internet() {
    let gateway = Gateway::new(NodeId::random());
    gateway.allow("100.64.0.1:9999");
    let client = Client::default();
    let router = Router::with_default(client.clone(), gateway.clone());

    // Initially no egress -> drops.
    let action = router.route("1.1.1.1".parse::<IpAddr>().unwrap());
    assert!(matches!(action, RouteAction::Drop { .. }));

    // Pick an egress -> forwards.
    let egress = NodeId::random();
    client.use_gateway(egress).unwrap();
    let action = router.route("1.1.1.1".parse::<IpAddr>().unwrap());
    assert!(action.is_forward());

    // Unset the egress -> drops again.
    client.unset().unwrap();
    let action = router.route("1.1.1.1".parse::<IpAddr>().unwrap());
    assert!(matches!(action, RouteAction::Drop { .. }));

    // Revoke the local gateway state.
    gateway.revoke().unwrap();
    assert!(!gateway.is_offering());
}

#[test]
fn mesh_traffic_always_routes_to_mesh() {
    let client = Client::default();
    let egress = NodeId::random();
    client.use_gateway(egress.clone()).unwrap();
    let router = Router::with_default(client, Gateway::new(NodeId::random()));

    // Even with an active egress, mesh destinations are
    // never forwarded via the egress.
    let node_id = NodeId::random();
    let vip = adnet_types::VirtualIp::from_node_id(&node_id);
    let action = router.route(vip.ipv4.as_std().into());
    assert_eq!(action, RouteAction::ForwardToMesh);
    let action = router.route(vip.ipv6.as_std().into());
    assert_eq!(action, RouteAction::ForwardToMesh);
}

#[test]
fn gateway_can_be_disabled_after_failure() {
    let gateway = Gateway::new(NodeId::random());
    gateway.allow("100.64.0.1:9999");
    let router = Router::with_default(Client::default(), gateway.clone());

    // Pretend the NAT setup failed.
    gateway.disable();
    assert!(!gateway.is_offering());
    assert!(!router.is_gateway());

    // Even if we have a client gateway, mesh destinations
    // still work.
    let action = router.route("8.8.8.8".parse::<IpAddr>().unwrap());
    assert!(matches!(action, RouteAction::Drop { .. }));
}

#[test]
fn shared_router_via_arc() {
    // The router must be cheaply cloneable so multiple
    // packet-processing tasks can hold a reference.
    let router = Arc::new(gw());
    let r2 = router.clone();

    // Both copies observe the same state.
    let s1 = router.snapshot();
    let s2 = r2.snapshot();
    assert_eq!(s1.client_state, s2.client_state);
    assert_eq!(s1.gateway_state, s2.gateway_state);
}

#[test]
fn require_route_returns_action_for_mesh() {
    let router = gw();
    let node_id = NodeId::random();
    let vip = adnet_types::VirtualIp::from_node_id(&node_id);
    let action = router
        .require_route(vip.ipv4.as_std().into())
        .expect("mesh should route");
    assert_eq!(action, RouteAction::ForwardToMesh);
}

#[test]
fn observe_advert_is_a_noop_for_now() {
    // The advert-observation API is wired so future
    // iterations can build a "best available" picker.
    let router = gw();
    let advert = adnet_exit_node::GatewayAdvert {
        node_id: NodeId::random(),
        endpoint: "100.64.0.1:9999".into(),
        advertised_at: chrono::Utc::now(),
    };
    router.observe_advert(&advert);
    // No panic, no state change.
    assert_eq!(router.snapshot().client_state, ClientState::Unconfigured);
}
