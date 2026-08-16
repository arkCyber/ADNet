//! `a3net-namespace` 最小示例：生成一个 Ed25519 密钥、构造 publisher + resolver，
//! 演示一次完整的 publish / resolve 往返。**不**接外部传输。
//!
//! 运行：`cargo run -p a3net-namespace --example namespace_basic`

use std::sync::Arc;
use std::time::Duration;

use a3net_namespace::{Ed25519SecretKey, IpnPublisher, IpnResolver};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sk = Arc::new(Ed25519SecretKey::generate());
    let name = sk.ipns_name();
    println!("ipns name : {}", &name[..32]);

    let publisher = IpnPublisher::new(sk.clone());
    let resolver = IpnResolver::new(Duration::from_secs(3600));

    // 第一次发布。
    let rec = publisher
        .publish(&name, "/ipfs/QmFirst...".into(), Duration::from_secs(3600))
        .await?;
    resolver.cache_record(rec.clone());
    println!("first pub : value={} seq={}", rec.value, rec.sequence);

    // 解析。
    let cached = resolver.resolve(&name).await?;
    println!("resolved  : value={}", cached);
    assert_eq!(cached, "/ipfs/QmFirst...");

    Ok(())
}