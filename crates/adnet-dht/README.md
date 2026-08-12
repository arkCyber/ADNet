# adnet-dht

> 基于 Kademlia 的 DHT 路由，为 ADNet 提供内容路由与对等节点发现。 / Kademlia-style DHT routing for ADNet — content routing and peer discovery.

## 概览 (Overview)

`adnet-dht` 在 ADNet 既有的 Pkarr / DNS / mDNS 发现机制之上叠加了一个简化版
Kademlia DHT，让节点能够：

- **公告内容**：将 `<内容哈希, 节点地址>` 作为 *Provider Record* 广播给最近的 K 个 peer。
- **查找内容提供者**：根据内容哈希发起 `GetProviders` 查询，沿 K-bucket 表并行探测。
- **可变命名**：通过 IPNS 风格的 `IpnRecord` 提供带序号和过期的可变指针。
- **O(log N) 路由**：每个 peer 维护一张按 XOR 距离排序的 K-bucket 表。

DHT 的键直接复用 `adnet_types::ContentHash`（BLAKE3），peer ID 复用 `NodeId`
（Ed25519 公钥哈希），因此可以无缝嵌入使用 iroh 传输层的应用。

## 特性 (Features)

- `DhtNode` — DHT 节点门面，包含 `DhtConfig`、`RoutingTable`、`SharedDhtStore`。
- `DhtKey` — 内容键，支持 `from_content_hash_hex`、`xor_distance`、`log_distance`。
- `ProviderRecord` / `IpnRecord` — 提供者记录与可变 IPNS 记录，含签名与 TTL。
- `RoutingTable` — K-bucket 表实现（K=20 默认），`insert`、`closest`、`num_contacts`。
- `DhtCodec` / `DhtProtocolHandler` — 线协议编解码（ALPN `DHT_ALPN`）。
- 可选 `rocksdb` feature — 用 RocksDB 替换默认内存存储。

## 安装 (Installation)

`adnet-dht` 在 workspace 中已经作为 `path` 依赖被 `adnet-node`、`adnet-namespace` 等
crate 引用。在你的代码里直接 `use`：

```rust
use adnet_dht::{DhtNode, DhtConfig, DhtKey, RoutingTable};
use adnet_types::NodeId;
```

## 使用 (Usage)

### 1. 创建一个最小 DHT 节点

```rust,no_run
use adnet_dht::{DhtConfig, DhtNode};
use adnet_types::NodeId;

let local_id = NodeId::random();
let dht = DhtNode::new(DhtConfig {
    local_id,
    ..Default::default()
});
println!("dht local id = {}", dht.local_id().short());
```

### 2. 内容键的 XOR / log2 距离

```rust,no_run
use adnet_dht::DhtKey;

let a = DhtKey::from_bytes([1u8; 32].to_vec());
let b = DhtKey::from_bytes([2u8; 32].to_vec());
let xor = a.xor_distance(&b);
let bucket = a.log_distance(&b);
println!("xor len = {}, log distance = {:?}", xor.len(), bucket);
```

### 3. 公告 + 查找提供者

```rust,no_run
use adnet_dht::{DhtKey, DhtNode, DhtConfig};
use adnet_types::NodeId;

let dht = DhtNode::new(DhtConfig::default());
let key = DhtKey::from_content_hash_hex(
    "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12",
);
dht.set_local_addr("/ip4/127.0.0.1/tcp/9000".into());

// 公告（无 sender 时为纯本地注册）。
dht.announce_content(&key).await;
// 查找
let providers = dht.find_providers(&key).await;
println!("found {} providers", providers.len());
```

### 4. 直接使用 K-bucket 路由表

```rust,no_run
use adnet_dht::{Contact, RoutingTable};
use adnet_types::NodeId;

let me = NodeId::random();
let mut rt = RoutingTable::new(me.clone());
rt.add_bootstrap_node(Contact::new(NodeId::random(), "127.0.0.1:9000".parse().unwrap()));
println!("peers in routing table = {}", rt.num_contacts());
```

## 应用案例 (Use Cases / Examples)

- **`adnet-node`** 在装配 DHT（feature `dht`）时把 `DhtNode` 作为内容寻址层的
  后端，对 `announce` / `fetch_blob` 的 `find_providers` 调用提供最近 K 个 peer。
- **`adnet-namespace`** 用 `DhtIpnTransport` 把 IPNS 记录以 `PutValue` /
  `GetValue` 形式落到 DHT，提供一种 "无需信任公共 pkarr relay" 的命名方案。
- **`adnet-gateway`** 通过 DHT 解析 `DAGService::get` / `DhtService::find_providers`
  的查询，把 IPFS 网关请求路由到最近的存有该 CID 的节点。

## 许可

MIT OR Apache-2.0