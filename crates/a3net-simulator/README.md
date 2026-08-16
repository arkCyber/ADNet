# a3net-simulator

> 网络条件仿真器,用于在受控环境中复现真实的网络行为(网络条件模拟器,用于在受控环境中复现真实网络行为)。

## 概览 (Overview)

`a3net-simulator` 提供了一套组合式的网络模拟原语,既可以在单元测试里零依赖地驱动确定性场景,也能够把真实的 Tokio 连接挂到 `NetworkEmulator` 后面,精确地复刻"次大陆移动 4G"、"跨太平洋卫星链路"或者"间歇性断网"等网络特性。

整个 crate 由四个互相独立的子模块构成:

- `conditions` — 一组可序列化的 `NetworkCondition` 结构体,字段化地描述延迟 / 丢包 / 带宽 / 篡改 / 分区 / 重排。
- `emulator` — 一个基于 `tokio` 的 `NetworkEmulator`,为每条 `ConnectionId` 维护独立的延迟、丢包和 token-bucket 带宽跟踪,并提供异步的 `send` / `receive` 接口。
- `topology` — 用来描述节点图(`NetworkTopology`、`TopologyNode`、`ConnectionConfig`、`NodeRole`),为后续更复杂的网格模拟打基础。
- `scenarios` — `Scenario`、`ScenarioRunner` 以及 `presets::*`(good / moderate / poor / mobile / satellite / packet_loss_storm),让回归测试可以以"网络类型"为单位复用。

这套工具既被 `a3net-integration-tests` 用来在 CI 里制造真实的弱网条件,也直接服务于 A3Net 自身的"分层降级"策略。

## 特性 (Features)

- **可序列化** 的 `NetworkCondition` —— 可以从 `config.toml` 读入,也可以在测试里通过 `serde_json` dump。
- **内置预设**:good / moderate / poor / mobile / satellite / packet_loss_storm / network_partition / intermittent,涵盖 A3Net 主要的目标运行环境。
- **`NetworkEmulator`** 支持每个连接独立的延迟 / 丢包 / 带宽 / 分区 + token-bucket 限速,通过 `Arc<RwLock<...>>` 共享且 `tokio` 安全。
- **`Scenario` + `ScenarioRunner`** 通过 `tokio::time::sleep` 驱动时序,并产生结构化的 `ScenarioEvent` 事件流。
- **零外部依赖** (`tokio`、`serde`、`rand` 都是 workspace 级复用),可单独作为 dev-dep 引入。

## 安装 (Installation)

它已经是 workspace 成员,通过 path 依赖被 `a3net-integration-tests` 等 crate 引用:

```toml
[dev-dependencies]
a3net-simulator = { path = "../a3net-simulator" }
```

也可以直接在自己的二进制 crate 里作为普通依赖使用:

```toml
[dependencies]
a3net-simulator = { path = "../a3net-simulator" }
```

## 使用 (Usage)

构造一个简单的延迟 + 丢包条件:

```rust
use a3net_simulator::{Latency, NetworkCondition, PacketLoss};

let condition = NetworkCondition {
    latency: Some(Latency::new(120).with_jitter(30)),
    packet_loss: Some(PacketLoss::new(0.02)),
    corruption_rate: 0.0,
    partition: None,
    bandwidth: None,
    reordering_rate: 0.0,
};
```

利用预设快速搭建一个卫星网络场景:

```rust
use a3net_simulator::scenarios::presets;

let condition = presets::satellite_network();
assert_eq!(condition.latency.unwrap().base_ms, 600);
```

通过 `NetworkEmulator` 把模拟挂到真实连接上:

```rust
use std::sync::Arc;
use a3net_simulator::{ConnectionId, NetworkCondition, NetworkEmulator};

let emulator = Arc::new(NetworkEmulator::new());
let conn = ConnectionId("client-1".into());
emulator.add_connection(conn.clone(), NetworkCondition::default()).await;

// 触发后台 partition 更新器
let _updater = emulator.clone().spawn_partition_updater();

// 发包并取得模拟延迟
let delay = emulator.send(&conn, vec![0u8; 64]).await;
let packets = emulator.receive(&conn).await;
```

串一个三节点 mesh 场景并跑:

```rust
use std::time::Duration;
use a3net_simulator::{
    scenarios::{Scenario, ScenarioRunner, presets},
};

let runner = ScenarioRunner::new()
    .add(
        Scenario::new("smoke", "baseline smoke")
            .with_duration(Duration::from_millis(50))
            .with_topology(presets::three_node_mesh())
            .expect("completed"),
    );

for result in runner.run_all().await {
    println!("{} ok={} events={}", result.scenario, result.success, result.events.len());
}
```

## 应用案例 (Use Cases / Examples)

1. **CI 烟雾测试** —— `a3net-integration-tests` 用 `presets::moderate_network()` 复现"普通 4G",确保 gossip / dht / bitswap 协议栈在 100ms 延迟 + 1% 丢包下也能完成 happy-path 拉取。
2. **离线 / 间歇网络守护** —— `a3net-cli` 的 `status` 子命令在客户端连接到远端节点之前,通过 `presets::intermittent()` 调用 `Partition::update()` 来模拟"地铁出入站"的不可达。
3. **跨大陆回放** —— `a3net-bench` 在 `--release` 模式下利用 `presets::satellite_network()` 给出"卫星链路下 blob 同步时间下界"报告,为容量规划提供数据。
4. **多节点 chaos 实验** —— `a3net-chaos` 把 `presets::packet_loss_storm()` 注入到 `NetworkEmulator`,验证节点在 30% 丢包 + 1% 篡改 + 10% 重排下仍能满足 "5xx 假阳性率 < 0.1%" 的可用性假设。

## 许可

MIT OR Apache-2.0
