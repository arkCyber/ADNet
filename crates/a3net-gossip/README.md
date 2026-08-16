# a3net-gossip

> 主题（topic）式 pub/sub 覆盖层：抽象了 gossip 传输契约，便于 in-process 与未来的 iroh-gossip 后端互换。 / Topic-based pub/sub overlay with a pluggable transport trait — in-process today, iroh-gossip ready.

## 概览 (Overview)

`a3net-gossip` 是 A3Net 房间（room）通信的基础设施。它由三层组成：

1. **`GossipTransport`** — 一个 `async_trait`，定义 `join / leave / broadcast / subscribe`。
   默认实现 [`InProcessGossip`] 在同一进程内多节点共享消息总线，方便测试与单节点 demo。
   feature `iroh` 启用的 [`IrohGossipTransport`] 把消息放到 iroh-gossip 的 HyParView + PlumTree
   树里。
2. **`GossipBus`** — 类型化的 *room-aware* 外观：自动把传输层 payload 解码成
   `Announcement`，按 `Topic` 路由，订阅者通过 `tokio::sync::broadcast` 拿到结果。
3. **辅助模块** — 访问控制 (`access`)、持久化 (`persistence`)、去重 (`dedup`)、
   跨网桥 (`bridge`)。

## 特性 (Features)

- `GossipBus::new(local_node, transport)` — 构造外观。
- `join_room` / `leave_room` / `publish` / `subscribe` — 房间 API。
- `InProcessGossip::new()` — 默认内存后端，可被多 `GossipBus` 共享。
- `IpnGossipPayload` / `IPNS_PUBSUB_ROOM` — IPNS-over-gossip 桥接（见 `a3net-namespace`）。
- 可选 feature `iroh` — 启用 `IrohGossipTransport`。

## 安装 (Installation)

```rust
use a3net_gossip::{GossipBus, GossipTransport, InProcessGossip};
use a3net_types::{NodeId, RoomId};
```

## 使用 (Usage)

### 1. 创建一个 in-process 房间

```rust,no_run
use std::sync::Arc;
use a3net_gossip::{GossipBus, InProcessGossip};
use a3net_types::NodeId;

let me = NodeId::random();
let transport: Arc<InProcessGossip> = Arc::new(InProcessGossip::default());
let bus = GossipBus::new(me, transport);
```

### 2. 发布 / 订阅一个房间

```rust,no_run
use a3net_gossip::GossipBus;
use a3net_types::{Announcement, RoomId};
# use a3net_types::{CdnContentKind, ContentHash};
# use chrono::Utc;

async fn demo(bus: GossipBus, room: RoomId) -> anyhow::Result<()> {
    bus.join_room(&room).await?;
    let mut rx = bus.subscribe(&room);
    let ann = Announcement {
        room_id: room.clone(),
        content_hash: ContentHash::from_bytes(b"hi"),
        node_id: bus.local_node().clone(),
        title: "demo".into(),
        kind: CdnContentKind::GenericFile,
        size_bytes: 0,
        mime_type: None,
        source_url: None,
        ticket: None,
        timestamp: Utc::now(),
        message_id: None,
        ttl_secs: None,
        signer: None,
        signature: None,
    };
    bus.publish(&room, &ann).await?;
    bus.leave_room(&room).await?;
    Ok(())
}
```

### 3. 直接使用底层传输

```rust,no_run
use std::sync::Arc;
use a3net_gossip::{InProcessGossip, GossipTransport};
use a3net_types::{AnnouncementPayload, NodeId, Topic};

let transport: Arc<dyn GossipTransport> = Arc::new(InProcessGossip::default());
let topic = Topic::from_label("room/lobby");
let mut rx = transport.subscribe(topic);
// broadcast()
let _ = transport.broadcast(topic, AnnouncementPayload::default()).await;
```

### 4. 访问控制（ACL）

```rust,no_run
use a3net_gossip::{AccessControl, RoomAccessPolicy, RoomCredential};

let policy = RoomAccessPolicy::allow_anonymous();
let _check = policy.evaluate(&RoomCredential::Anonymous);
let control = AccessControl::new(policy);
```

## 应用案例 (Use Cases / Examples)

- **`a3net-node`** 通过 `GossipBus::publish` 把 `import_and_announce` 的结果广播到房间主题，
  其他订阅者直接收到 `Announcement`。
- **`a3net-namespace`** 把 IPNS 记录作为 `IpnGossipPayload` 投递到 `IPNS_PUBSUB_ROOM`，
  让房间内的节点 0-RTT 看见名字变化（无需走 DHT 或 pkarr relay）。
- **`a3net-news` / `a3net-chatstore`** 用同一个 `GossipBus` 主题分发文章与消息记录。

## 许可

MIT OR Apache-2.0