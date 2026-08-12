# adnet-reputation

> ADNet 跨子系统的全局对等信誉(PeerScore) — bitswap / gossip / pairing / chat 共享同一份评分表,统一"接受 / 丢弃对等"决策。

## 概览 (Overview)

`adnet-reputation` 把 ADNet 原本散落在各子系统里的 `HashMap<String, usize>` 计数器统一为一个共享的 `PeerScoreTable`:

- **跨子系统** — bitswap / gossip / pairing / chat 各自产生 `ReputationEvent`,写入同一张表。
- **可调权重** — `ReputationParams` 提供权重、上下限、衰减速率的统一配置。
- **后台衰减** — `DecayLoop` 周期性地把所有分数向 0 收敛,避免永久"惩罚 / 奖励"。
- **可选持久化** — `ReputationStore` 写 JSONL delta + 周期性快照,启动时快速恢复。
- **可观测** — `metrics` feature 把 score / event 接到 `adnet-observability::Registry`。

## 特性 (Features)

- **`ReputationEvent`** — 类型化事件:
  - `ValidMessage { peer, topic, size_bytes }` — bitswap / gossip 收到合法消息。
  - `InvalidMessage { peer, topic, reason }` — 收到不合法消息,带 `InvalidReason`。
  - `Behavior(BehaviourKind)` — 行为层面调整(spam、slow、promotion、demotion)。
  - `Report(ReportKind)` — 用户报告(spam / abuse)。
- **`PeerScoreTable`** — 分片(sharded)线程安全表;`apply(event) -> Result<(), _>` 与 `score(&peer) -> f64`。
- **`ReputationReporter::in_memory(table)`** — 提供 `BitswapSignal` / `GossipSignal` / `PairingSignal` 等 facade。
- **`ReputationParams`** — 调节每种事件的权重、`MIN_SCORE` / `MAX_SCORE` 边界、衰减因子。
- **`DecayLoop`** — 后台 task 周期衰减,可用 `decay::run_until_shutdown` 跑。
- **`ReputationStore`** — `reputation.jsonl` 追加写 + `reputation.state.json` 周期性快照;启动时按 snapshot + replay 恢复。
- **`TrustLevel` / `TrustFusion`** — chat 用户层面的信任级,与全局 score 做加权融合。
- **`MAX_SCORE` / `MIN_SCORE`** — 上下限常量,防止极端事件把分数推出区间。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-reputation = { workspace = true }
adnet-types      = { workspace = true }   # 事件带 NodeId
```

## 使用 (Usage)

### 1. 直接构造 `ReputationEvent`

```rust
use adnet_reputation::{PeerScoreTable, ReputationEvent, ReputationParams, InvalidReason};
use adnet_types::NodeId;

let table = PeerScoreTable::new(ReputationParams::default());
let peer = NodeId::random();

let _ = table.apply(ReputationEvent::ValidMessage {
    peer: peer.clone(),
    topic: None,
    size_bytes: 1024,
});

let _ = table.apply(ReputationEvent::InvalidMessage {
    peer: peer.clone(),
    topic: None,
    reason: InvalidReason::BadSignature,
});

let score = table.score(&peer).unwrap_or(0.0);
```

### 2. 通过 `ReputationReporter` + 子系统 facade

```rust
use adnet_reputation::{
    ReputationReporter, BitswapSignal, GossipSignal, InvalidReason,
};
use adnet_types::NodeId;

let table = PeerScoreTable::default();
let reporter = ReputationReporter::in_memory(table);

let bitswap = BitswapSignal(&reporter);
let gossip = GossipSignal(&reporter);

bitswap.valid(NodeId::random(), 2048);
gossip.invalid(NodeId::random(), InvalidReason::BadSignature);
```

### 3. 决策:这个 peer 还在 mesh 中吗?

```rust
let s = reporter.table().score(&peer).unwrap_or(0.0);
if s < 0.0 {
    drop_from_mesh(&peer);
}
```

### 4. 持久化

```rust
use adnet_reputation::{ReputationStore, ReputationStoreConfig};

let store = ReputationStore::open(ReputationStoreConfig::default_in("/var/lib/adnet/rep"))?;
// 每产生 delta 自动 append 到 jsonl
```

## 应用案例 (Use Cases / Examples)

- **`adnet-blobstore` / bitswap** — 每个 `want_have` / `block_received` 事件通过 `BitswapSignal` 上报,负分直接踢出 ledger。
- **`adnet-gossip` / gossipsub** — `GossipSignal::valid` / `GossipSignal::invalid` 与 gossip 校验路径并行,gossip 层直接读 score 决定 mesh 维护。
- **`adnet-pairing`** — 首次 QR 配对写入一次 `PairingSignal::promote`,撤销时 `demote`,分数直接反馈到外发决策。
- **`adnet-chat`** — 用户对某 peer 标"不可信"产生 `TrustSignal`,与全局 score 融合后立刻影响其他子系统。
- **`adnet-observability`** — `metrics` feature 开启后,`adnet_reputation_score` / `adnet_reputation_event_total` 可在 Grafana 仪表盘上展示。

## 许可

MIT OR Apache-2.0