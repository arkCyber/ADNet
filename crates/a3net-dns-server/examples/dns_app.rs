//! `a3net-dns-server` 应用示例：构造 `HttpApi` + 写两条记录 + 走一遍 publish/fetch/list
//! 接口；这正是 `a3net-dns-server` 二进制内部走的逻辑。
//!
//! 运行：`cargo run -p a3net-dns-server --example dns_app`

use a3net_dns_server::{
    http::{HttpApi, PublishBody},
    DnsServerConfig,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-dns-server app demo ---");

    let cfg = DnsServerConfig::default().with_zone("a3net.example");
    let api = HttpApi::from_config(cfg)?;

    // publish
    api.publish(
        "alice",
        PublishBody {
            payload: "BASE64PKARR".into(),
            ttl_secs: Some(3600),
        },
    )?;
    api.publish(
        "bob",
        PublishBody {
            payload: "BASE64PKARR_BOB".into(),
            ttl_secs: Some(60),
        },
    )?;

    // fetch
    if let Some(rec) = api.fetch("alice") {
        println!("alice record: {} -> {:?}", rec.key, rec.kind);
    } else {
        println!("alice missing");
    }

    // list
    let all = api.list();
    println!("zone records:");
    for r in &all {
        println!("  {} -> {:?}", r.key, r.kind);
    }
    println!("total: {}", all.len());

    Ok(())
}