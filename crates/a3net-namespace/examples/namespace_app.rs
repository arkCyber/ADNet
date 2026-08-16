//! `a3net-namespace` 应用示例：演示一个 A3Net 节点如何把可变 IPNS 名字映射到
//! 多次发布的内容更新上，并演示 DNSLink 解析与 InMemoryLookup 后端。
//!
//! 运行：`cargo run -p a3net-namespace --example namespace_app`

use std::sync::Arc;
use std::time::Duration;

use a3net_namespace::{DnsLinkResolver, Ed25519SecretKey, InMemoryLookup, IpnPublisher};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- a3net-namespace app demo ---");

    // 1. 三次发布：依次指向不同的 CID。
    let sk = Arc::new(Ed25519SecretKey::generate());
    let name = sk.ipns_name();
    let publisher = IpnPublisher::new(sk.clone());

    for (i, cid) in [
        "QmVersion1...",
        "QmVersion2...",
        "QmVersion3...",
    ]
    .iter()
    .enumerate()
    {
        let rec = publisher
            .publish(&name, format!("/ipfs/{cid}"), Duration::from_secs(3600))
            .await?;
        println!(
            "v{} : {} -> seq={}",
            i + 1,
            rec.value,
            rec.sequence
        );
    }

    // 2. DNSLink：把 IPNS 名字绑到一个域名上。
    let store = InMemoryLookup::new();
    store.insert_dnslink("alice.a3net.example", &format!("/ipns/{name}"));
    let resolver = DnsLinkResolver::with_lookup(Arc::new(store));
    let path = resolver.resolve("alice.a3net.example")?;
    println!("dnslink alice.a3net.example -> {path}");
    assert!(path.starts_with("/ipns/"));

    Ok(())
}