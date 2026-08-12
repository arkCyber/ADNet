//! `adnet-tun` userspace roundtrip — drives the
//! [`adnet_tun::UserspaceTun`] through one full packet cycle
//! without involving a real kernel interface.

use adnet_tun::{TunDevice, UserspaceTun, UserspaceTunConfig};
use adnet_types::{VirtualIpv4, VirtualIp};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = UserspaceTunConfig {
        name: "adnet-tun-demo".into(),
        mtu: 1420,
        local_ipv4: VirtualIpv4::from_node_id(&adnet_types::NodeId::random()).as_std(),
    };
    let dev = UserspaceTun::new(cfg.clone());
    dev.bring_up();

    // Build a tiny IPv4/TCP packet addressed to a virtual
    // mesh member.
    let src_id = adnet_types::NodeId::random();
    let dst_id = adnet_types::NodeId::random();
    let src_vip = VirtualIp::from_node_id(&src_id);
    let dst_vip = VirtualIp::from_node_id(&dst_id);

    let total_len = 20u16 + 12u16; // IPv4 header + 12-byte TCP stub.
    let mut pkt = vec![0u8; total_len as usize];
    pkt[0] = 0x45; // ver=4, ihl=5
    pkt[2] = (total_len >> 8) as u8;
    pkt[3] = (total_len & 0xff) as u8;
    pkt[9] = 6; // TCP
    pkt[12..16].copy_from_slice(&src_vip.ipv4.as_std().octets());
    pkt[16..20].copy_from_slice(&dst_vip.ipv4.as_std().octets());
    // Source / destination ports in the TCP header (bytes 20..24).
    pkt[20..22].copy_from_slice(&12345u16.to_be_bytes());
    pkt[22..24].copy_from_slice(&80u16.to_be_bytes());

    // Inject from kernel → read with `recv`.
    dev.inject_from_kernel(pkt.clone()).await?;
    let got = dev.recv().await?.unwrap();
    assert_eq!(got, pkt);
    println!(
        "kernel→tunnel: {} bytes, src={} → dst={}",
        got.len(),
        std::net::Ipv4Addr::new(got[12], got[13], got[14], got[15]),
        std::net::Ipv4Addr::new(got[16], got[17], got[18], got[19]),
    );

    // Send back tunnel→kernel and drain.
    dev.send(pkt.clone()).await?;
    let out = dev.drain_to_kernel().await?.unwrap();
    assert_eq!(out, pkt);
    println!("tunnel→kernel: {} bytes", out.len());

    dev.shutdown().await?;
    println!("tun closed cleanly");
    Ok(())
}
