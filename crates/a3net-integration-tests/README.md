# a3net-integration-tests

> 跨 crate 的端到端集成测试套件,验证整个 A3Net 协议栈在真实多节点网络条件下的一致性与弹性(End-to-end integration tests for the full A3Net stack)。

## 概览 (Overview)

`a3net-integration-tests` 是 A3Net 协议栈的"系统测试"层。它把 `a3net-node`、`a3net-dht`、`a3net-gossip`、`a3net-blobstore`、`a3net-relay`、`a3net-identity`、`a3net-transport`、`a3net-simulator` 等多个 crate 串起来,在受控的临时目录、临时节点 ID、临时网络条件之下,跑出真实协议交互的端到端 happy-path 和故障恢复路径。

它**不是一个发布给最终用户的二进制**,而是一个被 `cargo test --workspace` 消费的测试 crate。出于这个原因,本 crate 没有 `bin` target,public surface 仅暴露给 `#[cfg(test)]` 用例使用的 helpers(配置结构、超时控制、`tracing` 初始化、临时目录)。

测试集按子目录分组,每组都 gate 在一个 cargo feature 之后:

- `network_tests` — `src/network.rs`,覆盖 DHT bootstrap、Gossip fan-out、Transport 拨号。
- `storage_tests` — `src/storage.rs`,覆盖 Bitswap / GraphSync / BAO 树。
- `protocol_tests` — `src/protocol.rs`,覆盖 Bitswap wantlist 协议交互。
- `chaos_tests` — `src/chaos.rs`,使用 `a3net-simulator` 的 `NetworkEmulator` 制造故障。
- `multi_node_tests` — `src/multi_node.rs`,验证多节点共识路径。
- `legacy_tests` — 一个总开关,启用全部子模块。日常 CI 不开启,可以用来"复活"过去某次重构后暂未跟上的旧测试。

`legacy_tests` 这个总开关的来源是为了在跨大版本重构 DHT / simulator 时,旧的 `mod tests` 块可以暂时关掉,避免阻塞日常工作流。新测试应当直接写在 `#[cfg(test)] mod tests`,并选用更精确的子 feature。

## 特性 (Features)

- **Cargo feature 矩阵**:`network_tests`(默认)、`storage_tests`(默认)、`protocol_tests`、`chaos_tests`、`legacy_tests`,允许 CI 选择性开启。
- **多节点拓扑**:内建 `presets::three_node_mesh()` 与 `relay_topology`,拿来即可拼出一个最小可测的集群。
- **可注入网络条件**:通过 `a3net-simulator::NetworkEmulator`,可以在测试运行时人为拉低延迟、加丢包或者制造分区。
- **公共测试 helpers**:`TestConfig`、`init_tracing`、`wait_for`、`run_with_timeout`、`test_runtime`、`temp_dir`,所有 `#[cfg(test)]` 函数都可复用。
- **`#[cfg(feature = "...")]` 开关**:可以按子集运行,反馈速度快且不会因为某一个失败就拖垮 CI。

## 安装 (Installation)

在 workspace 内部通过 path 依赖被引用。日常使用你不需要手动安装,只需通过以下命令运行:

```bash
# 默认特征(network + storage)
cargo test -p a3net-integration-tests

# 单独跑网络层
cargo test -p a3net-integration-tests --no-default-features --features network_tests

# 把全部模块打开
cargo test -p a3net-integration-tests --features legacy_tests
```

如果你希望在自己的 CI 子任务里复用这些 helpers,可以这样写 `Cargo.toml`:

```toml
[dev-dependencies]
a3net-integration-tests = { path = "../a3net-integration-tests" }
```

## 使用 (Usage)

最小的 happy-path 风格测试骨架,只走公共 helpers,不依赖任何被 gated 关闭的模块:

```rust
use a3net_integration_tests::{init_tracing, TestConfig, run_with_timeout};

#[tokio::test]
async fn smoke_wait_for_returns() {
    init_tracing();

    let reached = run_with_timeout(
        async {
            // 业务逻辑 …
            true
        },
        TestConfig::default().timeout_secs,
    )
    .await
    .expect("did not time out");

    assert!(reached);
}
```

启用某个子 feature 后,你可以直接调用对应模块的 API:

```rust,ignore
#[cfg(feature = "network_tests")]
use a3net_integration_tests::network;

#[tokio::test]
async fn dht_bootstrap_reaches_quorum() {
    let cfg = TestConfig::default();
    // network::two_node_dht(...).await.unwrap()
}
```

跑一个超短的 "wait for condition" 模式:

```rust,ignore
let ok = a3net_integration_tests::wait_for(|| async { false }, 1).await;
assert!(!ok, "should have timed out");
```

构造一个临时数据目录:

```rust,ignore
let dir = a3net_integration_tests::temp_dir();
let data_path = dir.path().join("node.db");
assert!(!data_path.exists());
```

## 应用案例 (Use Cases / Examples)

1. **冒烟测试 / 发布门槛** —— 在每次发布前 `cargo test --workspace --features legacy_tests` 跑一遍,确保多模块一起编译并通过 baseline。
2. **网络层回归** —— 持续跑 `cargo test -p a3net-integration-tests --features network_tests`,每次 DHT / Gossip 的改动都能在此被捕获。
3. **故障演练** —— `chaos_tests` feature 打开后,通过 `a3net-simulator::NetworkEmulator` 注入丢包 / 分区 / 限速,验证 gossip / bitswap 的重试与降级路径。
4. **跨平台 matrix** —— 在 `linux-x86_64`、`aarch64-apple-darwin`、`wasm32-unknown-unknown` 等不同目标上分别运行 `protocol_tests`,验证协议层的字节序和无锁假设。

## 许可

MIT OR Apache-2.0
