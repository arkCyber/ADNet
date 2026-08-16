# a3net-node

> A3Net 节点编排：把 BlobStore、GossipBus、MeshServer、Transport 拼成一个统一运行时。 / A3Net node orchestration — wire BlobStore, GossipBus, MeshServer and Transport into a single runtime.

## 概览 (Overview)

`a3net-node` 是 A3Net 对外的"门面 crate"。一个 `Node` 持有：

- `BlobStore` — 本地 blob 存储 + chunk 切分。
- `GossipBus` — 房间主题 pub/sub。
- `MeshServer` — HTTP 兜底传输。
- `Transport` — 主传输（默认 QUIC，可选 iroh）。
- 可选 `DhtHandle` / `IpnHandle` / `BitswapHandle`（feature 开启）。

对外暴露的常用操作：

- `announce` / `import_and_announce` — 把本地文件导入 + 广播到房间。
- `room_feed` — 列出已知的 assets + 它们的 peer source。
- `fetch_blob` — 定位 peers + 下载（首选 transport，失败时退到 mesh）。
- `subscribe` / `subscribe_room` — 收房间里的公告。

特性 features：

- `iroh` — 启 iroh transport + iroh-blobs + iroh-gossip + iroh-docs。
- `bitswap` — IPFS 兼容 bitswap 协议。
- `dht` — 启 DHT + IPNS。
- `billing` — 启 a3net-token + a3net-identity。
- `news` — 启用 a3net-news。

## 特性 (Features)

- `Node::builder(NodeConfig)` — 构造 `NodeBuilder`。
- `Node::import_and_announce(room, path, title, kind)` — 一站式导入 + 广播。
- `Node::announce(room, Announcement)` — 手动广播。
- `Node::subscribe(room)` — 收房间公告流。
- `Node::fetch_blob(hash, peers, dest)` — 拉 blob。
- `Node::room_feed(room)` — 列出房间内容。

## 安装 (Installation)

```rust
use a3net_node::{Node, NodeConfig, NodeBuilder};
use a3net_types::{NodeId, RoomId};
```

## 使用 (Usage)

### 1. 构造 + 启动一个最小节点

```rust,no_run
use a3net_node::{Node, NodeConfig};
use a3net_types::NodeId;
use tempfile::tempdir;

let dir = tempdir()?;
let node = Node::builder(NodeConfig::new(dir.path(), NodeId::random()))
    .build()
    .await?;
println!("node up at {:?}", node.local_node_id());
```

### 2. 导入文件并广播

```rust,no_run
use a3net_node::Node;
use a3net_types::{CdnContentKind, RoomId};

let ann = node
    .import_and_announce(
        &RoomId::new("lobby"),
        std::path::Path::new("./demo.txt"),
        "demo.txt".into(),
        CdnContentKind::GenericFile,
    )
    .await?;
println!("announced: hash={} ticket={:?}", ann.content_hash, ann.ticket);
```

### 3. 订阅一个房间

```rust,no_run
use a3net_types::RoomId;
let mut rx = node.subscribe(&RoomId::new("lobby"));
while let Ok(ann) = rx.recv().await {
    println!("new: {}", ann.title);
}
```

### 4. 拉一个已知 blob

```rust,no_run
# use a3net_types::{ContentHash, RangeSpec};
let dest = std::path::Path::new("./copy.bin");
node.fetch_blob(
    ContentHash::from_bytes(b"..."),
    &["http://peer:port".into()],
    dest,
    RangeSpec::All,
).await?;
```

## 应用案例 (Use Cases / Examples)

- **`a3net-cli`** 的 `a3net` 二进制背后就是一个 `Node`。
- **DApp 嵌入** —— 任何 Rust 应用都能在自己进程里跑一个 `Node`，做 room 公告 / 拉取。
- **测试 / CI** —— `Node::builder` 接 in-memory transport 跑端到端测试，不需要任何网络。

## 许可

MIT OR Apache-2.0