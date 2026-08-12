# ADNet DHT 路由 & IPNS 实现指南

> **日期**: 2026-08-11
> **目标**: 实现 DHT 路由和 IPNS 可变命名功能

---

## 一、架构概览

```
┌─────────────────────────────────────────────────────────────────────┐
│                        adnet-dht (新增)                              │
├─────────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │  Routing    │  │   Store      │  │       Query             │  │
│  │  Table      │  │   (存储)     │  │   (查询引擎)             │  │
│  │  KBucket    │  │ Provider/IPNS│  │  find_node/get_provider  │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
├─────────────────────────────────────────────────────────────────────┤
│                         adnet-namespace (新增)                       │
├─────────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │ IPNS Record │  │  Publisher  │  │       Resolver          │  │
│  │  (签名记录) │  │  (发布者)    │  │      (解析器)            │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────┐
│                       adnet-transport (已有)                          │
├─────────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │  Pkarr      │  │   mDNS       │  │     iroh               │  │
│  │  DNS        │  │   (局域网)   │  │  (传输层)              │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 二、已创建的文件结构

```
crates/
├── adnet-dht/                    # 新增: DHT 路由
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                # 模块入口
│       ├── bucket.rs             # K-Bucket 路由表
│       ├── record.rs             # Provider/IPNS 记录
│       ├── store.rs              # 存储层
│       ├── query.rs              # 查询引擎
│       ├── node.rs               # DhtNode 整合
│       ├── protocol.rs           # DHT 协议编解码 (新增)
│       ├── handler.rs            # DHT 协议处理器 (新增)
│       ├── network.rs            # DHT 网络发送器 (新增)
│       └── service.rs            # DHT 后台服务 (新增)
│
├── adnet-namespace/              # 新增: IPNS 命名
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                # 模块入口
│       └── ipns.rs               # IPNS 实现
│
└── adnet-transport/              # 更新: DHT 集成
    └── src/
        └── dht_integration.rs   # DHT 传输层集成 (新增)
```

---

## 三、核心数据类型

### 3.1 DHT Key
```rust
// DHT 键 - 使用内容哈希
DhtKey::from_content_hash_hex("abc123...")  // 从 BLAKE3 哈希创建
dht_key.xor_distance(&other_key)             // XOR 距离
dht_key.log_distance(&other_key)              // Kademlia 距离
```

### 3.2 Provider Record (内容提供者)
```rust
// 声明某个节点可以提供内容
ProviderRecord {
    key: DhtKey,           // 内容键
    provider_id: NodeId,    // 提供者节点 ID
    provider_addr: String,  // 地址 "1.2.3.4:8080"
    ttl_secs: 86400,       // 24小时 TTL
    signature: Vec<u8>,    // 提供者签名
}
```

### 3.3 IPNS Record (可变命名)
```rust
// 可变名称记录
IpnRecord {
    name: String,          // 名称 (pubkey 哈希)
    value: String,         // 值 (通常是 /ipfs/Qm...)
    sequence: 1,           // 版本号
    ttl_secs: 3600,        // 1小时 TTL
    created: 1234567890,   // 创建时间
    expires: 1234571490,   // 过期时间
    signature: Vec<u8>,    // Ed25519 签名
}
```

---

## 四、使用示例

### 4.1 基本 DHT 操作
```rust
use adnet_dht::{DhtNode, DhtKey};
use adnet_types::NodeId;

// 创建 DHT 节点
let local_id = NodeId::random();
let dht = DhtNode::with_id(local_id);

// 添加 peers 到路由表
dht.add_peer(peer_id, "192.168.1.100:8080".parse().unwrap()).await;

// 宣布提供内容
let content_key = DhtKey::from_content_hash_hex("abc123...");
dht.announce_content(&content_key).await;

// 查找内容提供者
let providers = dht.find_providers(&content_key).await;
```

### 4.2 IPNS 发布和解析
```rust
use adnet_namespace::{IpnPublisher, IpnResolver, Ed25519SecretKey};
use std::sync::Arc;

// 创建发布者
let secret_key = Arc::new(Ed25519SecretKey::generate());
let publisher = IpnPublisher::new(secret_key.clone());

// 发布可变名称
let name = secret_key.public_key().to_ipns_name();
let record = publisher.publish(&name, "/ipfs/QmNewContent...".to_string())?;
assert!(record.verify(&secret_key.public_key()));

// 创建解析器
let resolver = IpnResolver::new(Duration::from_secs(3600));
resolver.cache_record(record);

// 解析名称
let value = resolver.resolve(&name).await?;
assert_eq!(value, "/ipfs/QmNewContent...");
```

---

## 五、集成到 adnet-node

### 5.1 Cargo.toml 依赖
```toml
# adnet-node/Cargo.toml
[dependencies]
adnet-dht = { path = "../adnet-dht" }
adnet-namespace = { path = "../adnet-namespace" }
```

### 5.2 Node 集成
```rust
// crates/adnet-node/src/node.rs

use adnet_dht::{DhtNode, DhtConfig};
use adnet_namespace::{IpnPublisher, IpnResolver};

pub struct AdnetNode {
    // ... existing fields ...
    dht: DhtNode,
    ipns_publisher: IpnPublisher,
    ipns_resolver: IpnResolver,
}

impl AdnetNode {
    pub async fn new() -> Result<Self> {
        let dht_config = DhtConfig {
            local_id: node_id.clone(),
            bootstrap_nodes: vec![/* 引导节点 */],
            ..Default::default()
        };

        let dht = DhtNode::new(dht_config);

        // IPNS 需要持久化密钥
        let secret_key = load_or_create_secret_key()?;
        let ipns_publisher = IpnPublisher::new(Arc::new(secret_key));
        let ipns_resolver = IpnResolver::new(Duration::from_secs(3600));

        Ok(Self {
            // ...
            dht,
            ipns_publisher,
            ipns_resolver,
        })
    }

    /// 宣布内容可用
    pub async fn provide(&self, content_hash: &ContentHash) -> Result<()> {
        let key = DhtKey::from(content_hash);
        self.dht.announce_content(&key).await;
        Ok(())
    }

    /// 查找内容提供者
    pub async fn find_providers(&self, content_hash: &ContentHash) -> Result<Vec<ProviderRecord>> {
        let key = DhtKey::from(content_hash);
        Ok(self.dht.find_providers(&key).await)
    }

    /// 发布 IPNS 名称
    pub async fn publish_ipns(&self, value: String) -> Result<IpnRecord> {
        let name = self.ipns_publisher.public_key().to_ipns_name();
        let record = self.ipns_publisher.publish(&name, value)?;
        // 可选: 发布到 DHT
        Ok(record)
    }

    /// 解析 IPNS 名称
    pub async fn resolve_ipns(&self, name: &str) -> Result<String> {
        Ok(self.ipns_resolver.resolve(name).await?)
    }
}
```

---

## 六、网络协议消息

### 6.1 DHT 消息格式
```rust
enum DhtMessage {
    FindNode { key: DhtKey, request_id: String },
    Nodes { request_id: String, nodes: Vec<NodeInfo> },
    GetProviders { key: DhtKey, request_id: String },
    Providers { request_id: String, providers: Vec<ProviderRecord> },
    AddProvider { key: DhtKey, provider: ProviderRecord, request_id: String },
    GetValue { key: DhtKey, request_id: String },
    Value { request_id: String, value: Option<DhtValue> },
    PutValue { key: DhtKey, value: DhtValue, request_id: String },
}
```

### 6.2 协议处理 (示例)
```rust
// 在 adnet-transport 中添加 DHT 消息处理

impl ProtocolHandler for DhtProtocolHandler {
    async fn handle_message(&self, msg: Vec<u8>, peer: NodeId) -> Option<Vec<u8>> {
        let message: DhtMessage = bincode::deserialize(&msg)?;

        match message {
            DhtMessage::FindNode { key, request_id } => {
                let nodes = self.find_closest_nodes(&key);
                let response = DhtMessage::Nodes { request_id, nodes };
                Some(bincode::serialize(&response).ok()?)
            }
            DhtMessage::GetProviders { key, request_id } => {
                let providers = self.store.get_providers(&key);
                let response = DhtMessage::Providers { request_id, providers };
                Some(bincode::serialize(&response).ok()?)
            }
            // ... 其他消息类型
        }
    }
}
```

---

## 七、与现有组件集成

### 7.1 与 adnet-blobstore 集成
```rust
// 当 blob 被导入时，自动宣布提供
impl BlobStore {
    pub async fn import_with_provision(&self, data: Vec<u8>) -> Result<ContentHash> {
        let hash = self.import(data).await?;

        // 自动宣布提供内容
        let key = DhtKey::from(&hash);
        self.dht.announce_content(&key).await;

        Ok(hash)
    }
}
```

### 7.2 与 adnet-gossip 集成
```rust
// 通过 gossip 传播 provider 记录
impl GossipBus {
    pub async fn announce_via_gossip(&self, record: ProviderRecord) {
        let topic = format!("providers/{}", record.key.as_hex().split_at(8).0);
        self.publish(&topic, bincode::serialize(&record)).await;
    }
}
```

---

## 八、实现路线图

### Phase 1: 核心基础设施 ✅ 完成
- [x] `adnet-dht` crate 骨架
- [x] KBucket 路由表
- [x] Provider 记录
- [x] IPNS 记录
- [x] 基础存储

### Phase 2: 网络集成 ✅ 完成
- [x] DHT 消息协议 (protocol.rs)
- [x] DHT 协议处理器 (handler.rs)
- [x] DHT 网络发送器 (network.rs)
- [x] DHT 后台服务 (service.rs)
- [x] 与 adnet-transport 集成 (dht_integration.rs)

### Phase 3: IPNS 完整实现 🚧 进行中
- [ ] IPNS DHT 发布/订阅
- [ ] IPNS 缓存策略
- [ ] DNSLink 支持

### Phase 4: 测试和优化 📋 待开始
- [ ] 单元测试
- [ ] 集成测试
- [ ] 性能基准测试
- [ ] 网络模拟测试

---

## 九、关键设计决策

### 9.1 为什么复用 Ed25519?
ADNet 已经使用 Ed25519 作为身份系统，IPNS 的自认证命名可以直接复用：
- 名称 = hash(pubkey)
- 签名 = Ed25519(记录数据)

### 9.2 为什么用 BLAKE3 作为 DHT 键?
ADNet 的 `ContentHash` 已经使用 BLAKE3，与 IPFS 的 CID 兼容性好：
- 可转换为 IPFS CID (如果需要互操作)
- BLAKE3 比 SHA256 更快

### 9.3 TTL 设置
- Provider 记录: 24 小时
- IPNS 记录: 1 小时
- 可根据网络规模调整

---

## 十、测试策略

### 单元测试
```rust
#[cfg(test)]
mod tests {
    use adnet_dht::*;

    #[test]
    fn test_bucket_insert_eviction() {
        // 测试 K-Bucket 满时的驱逐
    }

    #[test]
    fn test_provider_record_signature() {
        // 测试签名验证
    }

    #[test]
    fn test_ipns_sequence_ordering() {
        // 测试序列号比较
    }
}
```

### 集成测试
```rust
#[tokio::test]
async fn test_multi_node_provider_lookup() {
    // 启动多个节点
    // 一个节点宣布提供内容
    // 另一个节点查询提供者
    // 验证能找到
}
```

---

## 十一、故障排除

### 问题: DHT 查询超时
- 增加 `query_timeout` 配置
- 检查网络连通性
- 验证引导节点配置

### 问题: IPNS 解析失败
- 检查记录签名
- 验证序列号正确
- 检查 TTL 未过期

### 问题: 路由表为空
- 确保引导节点可达
- 检查 NAT 配置
- 验证防火墙规则

---

## 十二、参考实现

- [libp2p Kademlia](https://github.com/libp2p/rust-libp2p/tree/master/protocols/kad)
- [IPFS DHT](https://github.com/ipfs/go-delegated-routing)
- [IPNS spec](https://specs.ipfs.tech/ipns/ipns-record/)

---

*文档更新时间: 2026-08-11*
