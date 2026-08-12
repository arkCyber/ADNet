//! Demo of the ADNet exit-node stack.
//!
//! Runs through:
//! 1. Build a `Router`.
//! 2. Show that with no gateway configured, outbound
//!    Internet destinations are dropped.
//! 3. Mark the local node as a gateway (`allow`) and
//!    verify `is_gateway()`.
//! 4. Pick an arbitrary other node as our egress gateway
//!    (`use_gateway`) and verify Internet destinations
//!    route via it.
//! 5. Show mesh destinations are always routed to the
//!    mesh, regardless of egress choice.

use std::net::IpAddr;

use adnet_exit_node::{
    Client, Gateway, GatewayState, RouteAction, Router,
};
use adnet_types::NodeId;

fn main() {
    let local_node = NodeId::random();
    let client = Client::default();
    let gateway = Gateway::new(local_node.clone());
    let router = Router::with_default(client.clone(), gateway.clone());

    println!("=== ADNet exit-node demo ===\n");

    let internet: IpAddr = "8.8.8.8".parse().unwrap();
    println!("Initial state:");
    println!("  client: {:?}", client.state());
    println!("  gateway: {:?}\n", gateway.state());

    let action = router.route(internet);
    println!("route {internet} -> {action:?}");
    assert!(matches!(action, RouteAction::Drop { .. }));

    // Become a gateway.
    let _advert = gateway.allow("100.64.0.1:9999");
    println!("\nAfter `ray exit-node allow`:");
    println!("  gateway state: {:?}", gateway.state());
    assert_eq!(gateway.state(), GatewayState::Offering);
    assert!(router.is_gateway());

    // Pick an egress gateway.
    let egress = NodeId::random();
    client.use_gateway(egress.clone()).unwrap();
    println!("\nAfter `ray exit-node use <peer>`:");
    println!("  client state: {:?}", client.state());

    let action = router.route(internet);
    println!("route {internet} -> {action:?}");
    assert_eq!(
        action,
        RouteAction::ForwardViaGateway {
            gateway: egress.clone()
        }
    );

    // Mesh destinations always go to the mesh.
    let mesh_node = NodeId::random();
    let vip = adnet_types::VirtualIp::from_node_id(&mesh_node);
    let action = router.route(vip.ipv4.as_std().into());
    println!(
        "route {} (mesh v4) -> {action:?}",
        vip.ipv4.as_std()
    );
    assert_eq!(action, RouteAction::ForwardToMesh);

    let action = router.route(vip.ipv6.as_std().into());
    println!(
        "route {} (mesh v6) -> {action:?}",
        vip.ipv6.as_std()
    );
    assert_eq!(action, RouteAction::ForwardToMesh);

    // Unset the egress gateway — Internet drops again.
    client.unset().unwrap();
    let action = router.route(internet);
    println!("\nAfter `ray exit-node unset`:");
    println!("route {internet} -> {action:?}");
    assert!(matches!(action, RouteAction::Drop { .. }));

    println!("\n=== OK ===");
}
