# adnet-namespace

> ADNet 的 IPNS（InterPlanetary Naming System）实现：可变名字指向不可变内容。 / IPNS — mutable naming over immutable content for ADNet.

## 概览 (Overview)

`adnet-namespace` 把"可变名字 ↔ 不可变内容哈希"的映射做完整体：

- **记录格式** [`IpnRecord`] — 序列号 + TTL + 签名 + value。
- **签名** `Ed25519SecretKey` / `Ed25519Verifier` — Ed25519 自签。
- **发布** `IpnPublisher::publish(name, value, ttl)` — 维护本地副本。
- **解析** `IpnResolver` — TTL-aware cache，支持去重与签名校验。
- **传输** `IpnTransport` — 把记录从本节点广播出去，可插拔：
  - `PkarrTransport` — 走 DNS-TXT，与 irp / pkarr.pub 联邦。
  - `GossipIpnTransport` — 走房间内 gossip 主题（feature `pubsub`）。
  - `DhtIpnTransport` — 走 DHT（feature `dht`）。
  - `DiskJournalTransport` — 本地落盘，重启后能 replay。
  - `MultiTransport` — 把上面若干个 fanout/fanin 串起来。
- **DNSLink** [`DnsLinkResolver`] — `_dnslink.<domain>` TXT 路径（兼容 IPFS 规范）。

## 特性 (Features)

- `IpnPublisher` / `IpnResolver` / `IpnRecord` — 核心 record API。
- `Ed25519SecretKey::generate()` + `ipns_name()`。
- `PubsubIpnsResolver::run(bus)` — 把 IPNS 灌进 gossip。
- `MultiTransport::new(vec)` — fanout/fanin 多后端。
- `InMemoryLookup` + `DnsLinkResolver` — DNSLink 解析。
- feature `pubsub` 启用 gossip transport，`dht` 启用 DHT transport。

## 安装 (Installation)

```rust
use adnet_namespace::{
    IpnPublisher, IpnResolver, Ed25519SecretKey, IpnRecord,
    SecretKey, public_key_to_ipns_name,
    IpnGossipPayload, IPNS_PUBSUB_ROOM,
    PkarrTransport, PkarrRelay, PkarrConfig,
    MultiTransport,
};
use std::sync::Arc;
```

## 使用 (Usage)

### 1. 生成密钥对 + 拿到 IPNS 名

```rust,no_run
use adnet_namespace::Ed25519SecretKey;

let sk = Ed25519SecretKey::generate();
let name = sk.ipns_name();           // blake3(pubkey)
let pk_bytes = sk.public_key_bytes();
```

### 2. Publisher / Resolver

```rust,no_run
use std::sync::Arc;
use std::time::Duration;
use adnet_namespace::{IpnPublisher, IpnResolver, Ed25519SecretKey};

let sk = Arc::new(Ed25519SecretKey::generate());
let publisher = IpnPublisher::new(sk.clone());
let resolver = IpnResolver::new(Duration::from_secs(3600));

let name = sk.ipns_name();
let rec = publisher.publish(&name, "/ipfs/QmNewCid...".into(), Duration::from_secs(3600)).await?;
resolver.cache_record(rec.clone());
let got = resolver.resolve(&name)?;
assert_eq!(got.value, rec.value);
```

### 3. Pkarr + MultiTransport

```rust,no_run
use std::sync::Arc;
use adnet_namespace::{PkarrTransport, PkarrConfig, PkarrRelay, MultiTransport};

let pkarr: Arc<dyn adnet_namespace::IpnTransport> = Arc::new(
    PkarrTransport::new(PkarrConfig {
        relays: vec![PkarrRelay::public()],
        request_timeout: std::time::Duration::from_secs(5),
    })?,
);
let _multi = MultiTransport::new(vec![pkarr]);
```

### 4. DNSLink

```rust,no_run
use adnet_namespace::{DnsLinkResolver, InMemoryLookup};

let store = InMemoryLookup::new();
store.insert_dnslink("example.com", "/ipfs/bafy...");
let resolver = DnsLinkResolver::with_lookup(std::sync::Arc::new(store));
let path = resolver.resolve("example.com")?;
println!("{path}");
```

## 应用案例 (Use Cases / Examples)

- **`adnet-node`** 在 `import_and_announce` 之外还提供 `ipns publish` /
  `ipns resolve` 子命令，背后就是 `IpnPublisher` / `IpnResolver`。
- **`adnet-dns-server`** 跑一个 pkarr 兼容 DNS 服务器，让公开解析器也能拉到 IPNS 记录。
- **`adnet-cli`** 用 `MultiTransport` 把 `pkarr.pub` + 本地 gossip + 落盘三种方式
  fanout 出去，零停机升级到任何一种新通道都能继续工作。

## 许可

MIT OR Apache-2.0