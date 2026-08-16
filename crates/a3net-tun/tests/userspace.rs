//! End-to-end tests that drive a [`UserspaceTun`] through
//! several roundtrip scenarios. The tests live under
//! `tests/` (not `src/`) so they exercise only the public
//! API of the crate, mirroring the way a downstream mesh
//! firewall / exit-node / coordinator would consume it.

use a3net_tun::packet::{IpProtocol, IpVersion, parse_packet};
use a3net_tun::{DeviceState, TunDevice, UserspaceTun, UserspaceTunConfig};
use std::net::Ipv4Addr;

/// Build a 32-byte IPv4/ICMP packet addressed from `src` to
/// `dst`. The payload is zero-filled, so it is not a valid
/// ICMP echo but it parses cleanly.
fn ipv4_icmp(src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
    let mut p = vec![0u8; 32];
    p[0] = 0x45;
    let total = p.len() as u16;
    p[2] = (total >> 8) as u8;
    p[3] = (total & 0xff) as u8;
    p[9] = 1; // ICMP
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    p
}

#[tokio::test]
async fn many_packets_roundtrip_in_order() {
    let dev = UserspaceTun::new(UserspaceTunConfig::default());
    dev.bring_up();
    let mut pkts = Vec::new();
    for i in 0..16u8 {
        let src = Ipv4Addr::new(100, 64, 0, i);
        let dst = Ipv4Addr::new(100, 64, 0, 100);
        let pkt = ipv4_icmp(src, dst);
        dev.inject_from_kernel(pkt.clone()).await.unwrap();
        pkts.push(pkt);
    }
    // Read them back in order.
    for expected in &pkts {
        let got = dev.recv().await.unwrap().unwrap();
        assert_eq!(&got, expected);
        let parsed = parse_packet(&got).unwrap();
        assert_eq!(parsed.protocol, IpProtocol::Icmp);
        assert_eq!(parsed.version, IpVersion::V4);
    }
    // The same packets sent back via `send` should be
    // drained in order.
    for expected in &pkts {
        dev.send(expected.clone()).await.unwrap();
    }
    for expected in &pkts {
        let out = dev.drain_to_kernel().await.unwrap().unwrap();
        assert_eq!(&out, expected);
    }
}

#[tokio::test]
async fn recv_returns_none_after_shutdown() {
    let dev = UserspaceTun::new(UserspaceTunConfig::default());
    dev.shutdown().await.unwrap();
    assert_eq!(dev.state(), DeviceState::Closed);
    let got = dev.recv().await.unwrap();
    assert!(got.is_none(), "recv after shutdown must return None");
}

#[tokio::test]
async fn send_after_shutdown_errors() {
    let dev = UserspaceTun::new(UserspaceTunConfig::default());
    dev.shutdown().await.unwrap();
    let res = dev.send(vec![0u8; 20]).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn oversized_packet_rejected() {
    let dev = UserspaceTun::new(UserspaceTunConfig::default());
    dev.bring_up();
    let oversized = vec![0u8; 5000];
    let err = dev.send(oversized).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("too large"), "got: {msg}");
}

#[tokio::test]
async fn info_reflects_lifecycle() {
    let dev = UserspaceTun::new(UserspaceTunConfig::default());
    // Even before bring_up(), info() should report the
    // configured values — only state() distinguishes.
    let info = dev.info().expect("info is Some when device is constructed");
    assert_eq!(info.name, "a3net-tun0");
    assert_eq!(info.mtu, 1420);
}

#[tokio::test]
async fn parallel_recv_and_send_tasks() {
    use std::sync::Arc;
    let dev = Arc::new(UserspaceTun::new(UserspaceTunConfig::default()));
    dev.bring_up();

    let dev_w = dev.clone();
    let writer = tokio::spawn(async move {
        for i in 0..8u8 {
            let src = Ipv4Addr::new(100, 64, 0, i);
            let dst = Ipv4Addr::new(100, 64, 0, 99);
            let pkt = ipv4_icmp(src, dst);
            dev_w.inject_from_kernel(pkt).await.unwrap();
        }
    });

    let dev_r = dev.clone();
    let reader = tokio::spawn(async move {
        let mut received = 0usize;
        while let Some(pkt) = dev_r.recv().await.unwrap() {
            let parsed = parse_packet(&pkt).unwrap();
            assert_eq!(parsed.version, IpVersion::V4);
            received += 1;
            if received == 8 {
                break;
            }
        }
        received
    });

    writer.await.unwrap();
    let got = reader.await.unwrap();
    assert_eq!(got, 8);
}
