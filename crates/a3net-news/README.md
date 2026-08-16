# `a3net-news`

> A3Net 新闻 / 公告公告板 —— gossip 背书的 pub/sub + SQLite 持久化 + 离线追补 + 签名修正与撤回。
>
> A3Net news + authoritative announcement bulletin service — gossip-backed pub/sub with SQLite persistence, offline catch-up, signed corrections and retractions.

## 概览(Overview)

`a3net-news` 把"权威公告"和"普通新闻流"统一在同一类记录(`a3net_types::BulletinItem`)下,服务既可以广播本地"节点 A 上线新固件"这种权威消息,也可以承载类似新闻的滚动内容。一份 service 对应一个 gossip 主题族,本地所有 bulletin 都会通过 `a3net_gossip::GossipTransport` 推给加入相同房间的 peer。

设计上由四块组成:

1. **存储 (`store`)** —— `BulletinStore`(SQLite,`journal_mode=WAL`),三张表:`bulletins`(主数据,`sequence` 在 `(room_id, author_id)` 内 UNIQUE)、`bulletin_receipts`(已读回执)、`bulletin_cursors`(本地与远端各一条 `last_seq`)。任何写都强制 `sequence > last_seq`,所以崩溃中途也不能回退序号。
2. **envelope (`envelope`)** —— `BulletinEnvelope` 是 gossip 上的 wire 形式,带 `version` 字段(从 `1` 起,任何破坏性升级必须 bump,接收端拒绝更高版本)。
3. **service (`service`)** —— `NewsService` 把存储 + gossip + 事件流串起来;`publish` / `publish_signed` 出本地公告,`ingest_envelope` 接远端公告,`subscribe` 返回 `broadcast::Receiver<BulletinEvent>`,首次订阅时自动 replay 本地全量历史(离线追补)。
4. **错误 (`error`)** —— `NewsError` 统一错误类型,通过 `thiserror` 派生。

校验策略三档:`Strict`(必须签名 + `envelope.from_node == item.author_id`)、`Audit`(允许未签名但 author 一致)、`Lenient`(仅做 `BulletinItem::validate`)。DO-178C 风格不变量:`validate()` 在每个边界(publish / ingest / replay)都跑一次,损坏 payload 在落地前就被拦截。

## 特性(Features)

- **`NewsService::open / open_in_memory`**:一行启动本地公告板,共享节点已有的 `GossipTransport`。
- **`NewsServiceBuilder`**:链式构造,可调 `policy` / `store_dir` / `event_channel_capacity`。
- **`ValidationPolicy::{Strict, Audit, Lenient}`**:三档严格度,默认 `Strict`。
- **`publish` / `publish_signed`**:本地公告,前者由 `Wallet` 在 service 外签名,后者接收已签好的 `BulletinItem`。
- **`ingest_envelope`**:从 gossip 接收 envelope → 校验 → 入库 → 派发事件。
- **`subscribe() -> broadcast::Receiver<BulletinEvent>`**:首次订阅自动 replay 全量历史,后续收到 `Insert` / `Correction` / `Retraction` 三类事件。
- **`acknowledge_bulletin`**:写 `bulletin_receipts`,UI 用来标记已读。
- **`BulletinStore::{list_for_room, since_cursor, fetch_supersedes}`**:支持时间线翻页、增量同步、修正链追溯。
- **`BULLETIN_ENVELOPE_VERSION = 1` / `BULLETIN_TOPIC_PREFIX = "a3net-news"`**:版本与主题前缀常量,便于跨进程 / 跨语言 wire 兼容。
- **零 unsafe**(`#![forbid(unsafe_code)]`),所有错误统一为 `NewsError`。

## 安装(Installation)

工作空间内 path 依赖:

```toml
# 你的 crate 的 Cargo.toml
a3net-news = { workspace = true }
a3net-gossip = { workspace = true }
```

```rust
use a3net_news::{
    NewsService, NewsServiceConfig, NewsServiceBuilder, ValidationPolicy,
    BulletinEnvelope, BulletinEnvelopePayload, BulletinEvent, BULLETIN_TOPIC_PREFIX,
    BulletinCursor, BulletinStore, BulletinStoreConfig, StoredBulletin,
};
use a3net_gossip::{GossipTransport, InProcessGossip};
use a3net_types::{BulletinItem, BulletinAttachment, BulletinCategory, BulletinId,
                  BulletinKind, BulletinSeverity, NodeId, RoomId};
```

## 使用(Usage)

### 1. 用 InProcessGossip 起一个 service

```rust
use std::sync::Arc;
use a3net_gossip::InProcessGossip;
use a3net_news::{NewsService, NewsServiceConfig};
use a3net_types::NodeId;

let transport = Arc::new(InProcessGossip::default());
let local = NodeId::random();
let svc = NewsService::open(
    local.clone(),
    transport.clone(),
    NewsServiceConfig::default(),
)?;
```

### 2. 发布一条公告

```rust
use a3net_news::BulletinItem;
use a3net_types::{BulletinCategory, BulletinKind, BulletinSeverity};

let item = BulletinItem {
    bulletin_id: String::new(),
    room_id: a3net_types::RoomId::new("general"),
    author_id: local.clone(),
    title: "Firmware 1.2.0 released".into(),
    body: "Includes security patches and a new gossip layer.".into(),
    kind: BulletinKind::Announcement,
    category: BulletinCategory::Software,
    severity: BulletinSeverity::Info,
    created_at_unix_ms: chrono::Utc::now().timestamp_millis(),
    updated_at_unix_ms: chrono::Utc::now().timestamp_millis(),
    supersedes: None,
    attachments: vec![],
    signer: None,
    signature: None,
    sequence: 0,
    received_at_unix_ms: None,
    integrity_hash: None,
};
let stored = svc.publish(item).await?;
println!("published: {} seq={}", stored.bulletin_id, stored.sequence);
```

### 3. 订阅事件流

```rust
use a3net_news::BulletinEvent;
let mut rx = svc.subscribe();
while let Ok(ev) = rx.recv().await {
    match ev {
        BulletinEvent::Insert(item) => println!("insert: {}", item.title),
        BulletinEvent::Correction { superseded_id, corrected } => {
            println!("correction: {} -> {}", superseded_id, corrected.title);
        }
        BulletinEvent::Retraction { superseded_id, retraction } => {
            println!("retract: {} -> {}", superseded_id, retraction.title);
        }
    }
}
```

### 4. 离线追补

`svc.subscribe()` 在第一次被调用时会自动 replay 整个持久层的历史(按 `(room_id, sequence)` 顺序),所以新启动的 UI 能立刻看到完整公告板。

### 5. 把 envelope 包成 gossip 帧

```rust
use a3net_news::{BulletinEnvelope, BulletinEnvelopePayload, BULLETIN_ENVELOPE_VERSION};
use a3net_gossip::AnnouncementPayload;

let env = BulletinEnvelope::wrap(stored, local.clone());
let payload = BulletinEnvelopePayload::from_envelope(&env)?;
let frame = AnnouncementPayload {
    from_node: payload.from_node,
    payload: payload.payload,
};
transport.broadcast(env.topic_id(), frame).await?;
```

## 应用案例(Use Cases / Examples)

- **固件 / 公告权威发布**:`a3net-node` 在检测到固件更新时,构造 `BulletinKind::Announcement`,用 `publish_signed` 带上 `Wallet` 签名,gossip 自动广播给所有 `general` 房间的节点。
- **运营事件审计**:UI 用 `BulletinStore::list_for_room` 拉 30 天公告,`acknowledge_bulletin` 标记已读。
- **撤回 / 修正链**:发错了就发一条 `BulletinKind::Correction` 或 `BulletinKind::Retraction` + `supersedes: Some(old_id)`,接收端 `BulletinEvent::Correction` / `Retraction` 事件触发 UI 立即抹掉旧内容。
- **离线节点恢复**:长期下线的节点重连后首次 `subscribe()`,自动收到"我之前没看到的所有"事件,无需额外 catch-up 调用。
- **跨节点审计**:本地 `Strict` 模式拒绝任何伪造 author_id 的 envelope,运营面板把 rejection 计数推 `a3net-observability` 当告警。

## 许可(License)

MIT OR Apache-2.0