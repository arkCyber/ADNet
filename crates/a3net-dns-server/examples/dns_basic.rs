//! `a3net-dns-server` 最小示例：构造一个 `DnsServerConfig`、建一个内存 zone，
//! 添加一条 IPNS TXT 记录和一条 A 记录，不真正监听端口。
//!
//! 运行：`cargo run -p a3net-dns-server --example dns_basic`

use a3net_dns_server::{
    zone::{open, RecordKind, ZoneRecord},
    DnsServerConfig,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = DnsServerConfig::default().with_zone("a3net.example");
    let store = open(cfg)?;

    let txt = ZoneRecord {
        key: store.ipns_txt_key("alice"),
        kind: RecordKind::AdnetIpnsTxt {
            ipns_name: "alice".into(),
            payload: "BASE64PKARR".into(),
            ttl_secs: 3600,
        },
    };
    let a = ZoneRecord {
        key: store.relay_key("relay1"),
        kind: RecordKind::RelayAddr {
            ipns_name: "relay1".into(),
            addr: "203.0.113.10".into(),
            ttl_secs: 3600,
        },
    };
    store.put(txt)?;
    store.put(a)?;

    println!("zone   : {}", store.zone());
    println!("records: {}", store.all().len());
    for r in store.all() {
        println!("  {} -> {:?}", r.key, r.kind);
    }
    Ok(())
}