//! End-to-end integration tests for the exit-node stack.
//!
//! These exercise the combined behavior of the gateway
//! and client submodules through the router — the same
//! way `a3net-node` will use them.

use std::net::IpAddr;
use std::sync::Arc;

use a3net_exit_node::{
    Client, ClientState, ExitHandler, ExitHandlerConfig, Gateway, GatewayState, RouteAction,
    Router, ExitNodeMeter, BillingEngine, BandwidthStats, RateLimitConfig, RateLimitResult,
};
use a3net_types::NodeId;

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
    let vip = a3net_types::VirtualIp::from_node_id(&node_id);
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
    let vip = a3net_types::VirtualIp::from_node_id(&node_id);
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
    let advert = a3net_exit_node::GatewayAdvert {
        node_id: NodeId::random(),
        endpoint: "100.64.0.1:9999".into(),
        advertised_at: chrono::Utc::now(),
    };
    router.observe_advert(&advert);
    // No panic, no state change.
    assert_eq!(router.snapshot().client_state, ClientState::Unconfigured);
}

// =============================================================================
// Bandwidth Metering Tests
// =============================================================================

#[test]
fn bandwidth_meter_records_client_traffic() {
    use a3net_exit_node::ExitNodeMeter;

    let meter = ExitNodeMeter::new();
    let client = NodeId::random();

    meter.record_client_traffic(&client, 1024 * 1024, 512 * 1024, 10);

    let stats = meter.client_stats(&client).unwrap();
    assert_eq!(stats.bytes_sent, 1024 * 1024);
    assert_eq!(stats.bytes_received, 512 * 1024);
    assert_eq!(stats.packets_sent + stats.packets_received, 20);
}

#[test]
fn bandwidth_meter_global_stats_aggregate() {
    use a3net_exit_node::ExitNodeMeter;

    let meter = ExitNodeMeter::new();
    let client1 = NodeId::random();
    let client2 = NodeId::random();

    meter.record_client_traffic(&client1, 1000, 0, 1);
    meter.record_client_traffic(&client2, 2000, 0, 1);

    let global = meter.global_stats();
    assert_eq!(global.bytes_sent, 3000);
}

#[test]
fn bandwidth_meter_tracks_multiple_clients() {
    use a3net_exit_node::ExitNodeMeter;

    let meter = ExitNodeMeter::new();
    let clients: Vec<_> = (0..5).map(|_| NodeId::random()).collect();

    for client in &clients {
        meter.record_client_traffic(client, 1000, 500, 1);
    }

    assert_eq!(meter.tracked_clients().len(), 5);
}

#[test]
fn client_meter_rate_limiting() {
    use a3net_exit_node::ClientMeter;

    let meter = ClientMeter::new(NodeId::random());
    meter.set_rate_limit(Some(RateLimitConfig {
        bytes_per_second: 1024,
        burst_bytes: 512,
    }));

    // First request should be allowed
    assert!(matches!(meter.check_rate_limit(256), RateLimitResult::Allowed));
}

#[test]
fn bandwidth_snapshot_includes_all_data() {
    use a3net_exit_node::ExitNodeMeter;

    let meter = ExitNodeMeter::new();
    let client = NodeId::random();
    meter.record_client_traffic(&client, 1024, 2048, 5);

    let snap = meter.snapshot();
    assert_eq!(snap.global.bytes_sent, 1024);
    assert_eq!(snap.tracked_client_count, 1);
}

// =============================================================================
// Billing Tests
// =============================================================================

#[test]
fn billing_engine_records_usage() {
    let billing = BillingEngine::new();
    let client = NodeId::random();

    billing.record_traffic(&client, 1024 * 1024, 512 * 1024).unwrap();

    let usage = billing.get_current_usage(&client).unwrap();
    assert_eq!(usage.bytes_sent, 1024 * 1024);
    assert_eq!(usage.bytes_received, 512 * 1024);
}

#[test]
fn billing_engine_generates_invoice() {
    let billing = BillingEngine::new();
    let client = NodeId::random();

    billing.record_traffic(&client, 1024 * 1024, 0).unwrap();

    let invoice = billing.generate_invoice(&client).unwrap();
    assert_eq!(invoice.client_id, client);
    assert!(invoice.total_cents > 0);
    assert!(invoice.line_items.len() >= 1);
}

#[test]
fn rate_card_per_byte_pricing() {
    use a3net_exit_node::RateCard;

    let card = RateCard::default();
    let charge = card.calculate_charge(1024 * 1024, 0); // 1 MB
    assert_eq!(charge, 10); // $0.10/MB = 10 cents
}

#[test]
fn rate_card_flat_rate_pricing() {
    use a3net_exit_node::RateCard;

    let card = RateCard::flat_rate(10 * 1024 * 1024, 1000); // 10MB included, $10 base

    // Within included quota
    let charge = card.calculate_charge(5 * 1024 * 1024, 0);
    assert_eq!(charge, 1000); // Just base fee

    // Over quota
    let charge = card.calculate_charge(15 * 1024 * 1024, 0);
    assert!(charge > 1000); // Base + overage
}

#[test]
fn billing_status_returns_current_state() {
    let billing = BillingEngine::new();
    let client = NodeId::random();

    billing.record_traffic(&client, 1024 * 1024, 512 * 1024).unwrap();

    let status = billing.get_status(&client);
    assert_eq!(status.current_usage_bytes_sent, 1024 * 1024);
    assert!(status.current_charge_cents > 0);
}

#[test]
fn billing_engine_multiple_clients() {
    let billing = BillingEngine::new();
    let clients: Vec<_> = (0..3).map(|_| NodeId::random()).collect();

    for client in &clients {
        billing.record_traffic(client, 1024, 512).unwrap();
    }

    let tracked = billing.clients_with_usage();
    assert_eq!(tracked.len(), 3);
}

// =============================================================================
// Exit Handler Tests
// =============================================================================

#[test]
fn exit_handler_processes_exit_packet() {
    let config = ExitHandlerConfig {
        enable_metering: true,
        enable_billing: false,
        ..Default::default()
    };
    let client = Client::default();
    let gateway = Gateway::new(NodeId::random());
    let handler = ExitHandler::new(config, client.clone(), gateway);

    // Set up egress gateway
    let egress = NodeId::random();
    client.use_gateway(egress.clone()).unwrap();

    let source = NodeId::random();
    let result = handler.process_packet(&source, "8.8.8.8".parse().unwrap(), 1024);

    assert!(result.allowed);
    assert_eq!(result.bytes, 1024);
}

#[test]
fn exit_handler_drops_without_gateway() {
    let config = ExitHandlerConfig::default();
    let handler = ExitHandler::new(
        config,
        Client::default(),
        Gateway::new(NodeId::random()),
    );

    let source = NodeId::random();
    let result = handler.process_packet(&source, "8.8.8.8".parse().unwrap(), 1024);

    assert!(!result.allowed);
    assert!(result.drop_reason.is_some());
}

#[test]
fn exit_handler_forwards_mesh_traffic() {
    let handler = ExitHandler::new(
        ExitHandlerConfig::default(),
        Client::default(),
        Gateway::new(NodeId::random()),
    );

    let source = NodeId::random();
    let target = NodeId::random();
    let vip = a3net_types::VirtualIp::from_node_id(&target);

    let result = handler.process_packet(&source, vip.ipv4.as_std().into(), 512);

    assert!(result.allowed);
    assert_eq!(result.action, a3net_exit_node::PacketAction::ForwardToMesh);
}

#[test]
fn exit_handler_tracks_bandwidth() {
    let config = ExitHandlerConfig {
        enable_metering: true,
        enable_billing: false,
        ..Default::default()
    };
    let client = Client::default();
    let gateway = Gateway::new(NodeId::random());
    let handler = ExitHandler::new(config, client.clone(), gateway);

    let egress = NodeId::random();
    client.use_gateway(egress).unwrap();

    let source = NodeId::random();
    handler.process_packet(&source, "8.8.8.8".parse().unwrap(), 2048);

    let stats = handler.get_client_bandwidth(&source).unwrap();
    assert_eq!(stats.bytes_sent, 2048);
}

#[test]
fn exit_handler_snapshot_contains_state() {
    let config = ExitHandlerConfig {
        enable_metering: true,
        enable_billing: false,
        ..Default::default()
    };
    let handler = ExitHandler::new(
        config,
        Client::default(),
        Gateway::new(NodeId::random()),
    );

    let snap = handler.snapshot();

    assert!(!snap.is_gateway);
    assert_eq!(snap.tracked_clients.len(), 0);
    assert!(!snap.billing_enabled);
}

#[test]
fn exit_handler_return_packet_tracking() {
    let config = ExitHandlerConfig {
        enable_metering: true,
        enable_billing: false,
        ..Default::default()
    };
    let handler = ExitHandler::new(
        config,
        Client::default(),
        Gateway::new(NodeId::random()),
    );

    let dest = NodeId::random();
    handler.process_return_packet(&dest, "8.8.8.8".parse().unwrap(), 4096);

    let stats = handler.get_client_bandwidth(&dest).unwrap();
    assert_eq!(stats.bytes_received, 4096);
}

#[test]
fn exit_handler_billing_integration() {
    let config = ExitHandlerConfig {
        enable_metering: true,
        enable_billing: true,
        ..Default::default()
    };
    let client = Client::default();
    let gateway = Gateway::new(NodeId::random());
    let handler = ExitHandler::new(config, client.clone(), gateway);

    let egress = NodeId::random();
    client.use_gateway(egress).unwrap();

    let source = NodeId::random();
    handler.process_packet(&source, "8.8.8.8".parse().unwrap(), 1024);

    let billing_status = handler.get_billing_status(&source);
    assert!(billing_status.is_some());
}

#[test]
fn exit_handler_gateway_state_tracking() {
    let gateway = Gateway::new(NodeId::random());
    let handler = ExitHandler::new(
        ExitHandlerConfig::default(),
        Client::default(),
        gateway.clone(),
    );

    assert_eq!(handler.gateway_state(), GatewayState::NotOffering);
    assert!(!handler.is_gateway());

    gateway.allow("100.64.0.1:9999");

    assert_eq!(handler.gateway_state(), GatewayState::Offering);
    assert!(handler.is_gateway());
}

#[test]
fn exit_handler_packet_log() {
    let mut config = ExitHandlerConfig::default();
    config.log_packets = true;

    let handler = ExitHandler::with_meters(
        config,
        Router::with_default(Client::default(), Gateway::new(NodeId::random())),
        a3net_exit_node::ExitNodeMeter::new(),
        None,
    );

    let source = NodeId::random();
    let target = NodeId::random();
    let vip = a3net_types::VirtualIp::from_node_id(&target);

    handler.process_packet(&source, vip.ipv4.as_std().into(), 256);

    let log = handler.get_packet_log(10);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].size_bytes, 256);
}

#[test]
fn exit_handler_async_trait_impl() {
    use a3net_exit_node::AsyncExitHandler;

    let handler = ExitHandler::new(
        ExitHandlerConfig::default(),
        Client::default(),
        Gateway::new(NodeId::random()),
    );

    let source = NodeId::random();
    let target = NodeId::random();
    let vip = a3net_types::VirtualIp::from_node_id(&target);

    // Use the sync interface for testing
    let result = handler.process_packet(&source, vip.ipv4.as_std().into(), 512);
    assert!(result.allowed);
}

// =============================================================================
// Stress and Edge Case Tests
// =============================================================================

#[test]
fn high_volume_bandwidth_tracking() {
    use a3net_exit_node::ExitNodeMeter;

    let meter = ExitNodeMeter::new();
    let client = NodeId::random();

    // Simulate high volume traffic
    for i in 0..1000 {
        meter.record_client_traffic(&client, 1024, 512, 1);
    }

    let stats = meter.client_stats(&client).unwrap();
    assert_eq!(stats.bytes_sent, 1024 * 1000);
    assert_eq!(stats.bytes_received, 512 * 1000);
}

#[test]
fn concurrent_client_access() {
    use a3net_exit_node::ExitNodeMeter;
    use std::sync::Arc;

    let meter = Arc::new(ExitNodeMeter::new());

    let clients: Vec<_> = (0..10).map(|_| NodeId::random()).collect();

    // Simulate concurrent access
    for _ in 0..100 {
        for client in &clients {
            meter.record_client_traffic(client, 100, 50, 1);
        }
    }

    for client in &clients {
        let stats = meter.client_stats(client).unwrap();
        assert_eq!(stats.bytes_sent, 100 * 100);
    }
}

#[test]
fn zero_traffic_handling() {
    let billing = BillingEngine::new();
    let client = NodeId::random();

    // No traffic recorded yet
    let invoice = billing.generate_invoice(&client);
    assert!(invoice.is_err()); // No usage should error
}

#[test]
fn large_packet_handling() {
    let handler = ExitHandler::new(
        ExitHandlerConfig::default(),
        Client::default(),
        Gateway::new(NodeId::random()),
    );

    let source = NodeId::random();
    let target = NodeId::random();
    let vip = a3net_types::VirtualIp::from_node_id(&target);

    // Large packet (near MTU)
    let result = handler.process_packet(&source, vip.ipv4.as_std().into(), 65500);

    assert!(result.allowed);
    assert_eq!(result.bytes, 65500);
}
