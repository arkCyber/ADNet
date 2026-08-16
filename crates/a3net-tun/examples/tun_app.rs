//! `a3net-tun` 应用示例：模拟一次 mesh 内两个节点之间的 UDP 包往返。
//! 把 `UserspaceTun` 当成一个 packet loop 沙盒，演示 send / inject / drain 的组合用法。
//!
//! 运行：`cargo run -p a3net-tun --example tun_app`

use a3net_tun::{TunDevice, UserspaceTun, UserspaceTunConfig};
use a3net_types::{VirtualIp, VirtualIpv4};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-tun app demo ---");

    let tun = UserspaceTun::new(UserspaceTunConfig {
        name: "a3net-tun-app".into(),
        mtu: 1420,
        local_ipv4: VirtualIpv4::from_node_id(&a3net_types::NodeId::random()).as_std(),
    });
    tun.bring_up();

    // 构造一个 IPv4/UDP 包（src / dst 用 mesh VIP）。
    let src_id = a3net_types::NodeId::random();
    let dst_id = a3net_types::NodeId::random();
    let src_vip = VirtualIp::from_node_id(&src_id);
    let dst_vip = VirtualIp::from_node_id(&dst_id);

    let total = 20 + 8 + 32;
    let mut pkt = vec![0u8; total];
    pkt[0] = 0x45;
    let len = total as u16;
    pkt[2..4].copy_from_slice(&len.to_be_bytes());
    pkt[9] = 17; // UDP
    pkt[12..16].copy_from_slice(&src_vip.ipv4.as_std().octets());
    pkt[16..20].copy_from_slice(&dst_vip.ipv4.as_std().octets());
    let udp_len = (8 + 32) as u16;
    pkt[24..26].copy_from_slice(&udp_len.to_be_bytes());

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        // 1) 内核 → TUN
        tun.inject_from_kernel(pkt.clone()).await?;
        let got = tun.recv().await?.unwrap();
        assert_eq!(got, pkt);
        println!("kernel → tun: {} bytes", got.len());

        // 2) TUN → 内核（mesh 写包）
        tun.send(pkt.clone()).await?;
        let out = tun.drain_to_kernel().await?.unwrap();
        assert_eq!(out, pkt);
        println!("tun   → kernel: {} bytes", out.len());

        // 3) shutdown
        tun.shutdown().await?;
        println!("tun closed");
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}