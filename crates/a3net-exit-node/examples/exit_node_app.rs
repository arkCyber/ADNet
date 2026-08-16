//! `a3net-exit-node` 应用示例：演示完整的 mesh 内网 + 网关出口 + 限流决策。
//! 不真正发送包，只在 `Router` 层做决策验证。
//!
//! 运行：`cargo run -p a3net-exit-node --example exit_node_app`

use std::net::IpAddr;

use a3net_exit_node::{
    Client, ClientMeter, ExitNodeMeter, Gateway, GatewayState, RouteAction, Router,
};
use a3net_types::{NodeId, VirtualIp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-exit-node app demo ---");

    let me = NodeId::random();
    let gateway = Gateway::new(me.clone());
    let client = Client::default();
    let meter = ExitNodeMeter::new();
    let router = Router::with_default(client.clone(), gateway.clone());

    // Step 1: 没启用 gateway / 没选出口 → Internet 包全 drop。
    let internet: IpAddr = "1.1.1.1".parse().unwrap();
    let action = router.route(internet);
    println!("[1] route {internet} -> {action:?}");
    assert!(matches!(action, RouteAction::Drop { .. }));

    // Step 2: 本节点 allow 自己为 gateway。
    let _advert = gateway.allow("100.64.0.1:9999");
    assert_eq!(gateway.state(), GatewayState::Offering);
    println!("[2] gateway state = {:?}", gateway.state());

    // Step 3: 选另一个 mesh 节点做 Internet 出口。
    let egress = NodeId::random();
    client.use_gateway(egress.clone())?;
    println!("[3] egress gateway = {}", egress.short());

    let action = router.route(internet);
    println!("    route {internet} -> {action:?}");
    assert!(matches!(action, RouteAction::ForwardViaGateway { .. }));

    // Step 4: mesh 内 vip 永远走 mesh，不走 gateway。
    let mesh_peer = NodeId::random();
    let vip = VirtualIp::from_node_id(&mesh_peer);
    let action = router.route(vip.ipv4.as_std().into());
    println!("[4] mesh vip -> {action:?}");
    assert_eq!(action, RouteAction::ForwardToMesh);

    // Step 5: 出口节点的总计量器记一次流量。
    meter.record_traffic(64 * 1024, 8 * 1024, 4);
    let snap = meter.snapshot();
    println!(
        "[5] meter: tx={} rx={} pkts_sent={} pkts_recv={} tracked_clients={}",
        snap.global.bytes_sent,
        snap.global.bytes_received,
        snap.global.packets_sent,
        snap.global.packets_received,
        snap.tracked_client_count
    );

    // Step 6: 解除 gateway。
    client.unset()?;
    let action = router.route(internet);
    println!("[6] after unset -> {action:?}");

    let _ = ClientMeter::new(me.clone()); // re-export smoke check
    Ok(())
}