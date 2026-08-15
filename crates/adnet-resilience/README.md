# adnet-resilience

> ADNet 通用弹性层 — `RetryPolicy` / `CircuitBreaker` / `CancellationScope` / `ResourceLimiter` / `ResilientHttpClient`,可被 mesh、ipc、relay 等多个 crate 共用,不依赖 ADNet 业务类型。

## 概览 (Overview)

`adnet-resilience` 是 ADNet 中所有"对外调用"都会用到的弹性组件,设计目标是 **transport-agnostic** — 不绑定任何 ADNet 业务类型,可以被任意子模块直接复用:

- **`retry`** — 指数回退 + 抖动 + 可配置上限的通用重试函数 `retry_with_backoff(&RetryConfig, op)`。
- **`circuit_breaker`** — 三态熔断器 `CircuitBreaker`(`Closed` / `Open` / `HalfOpen`),按失败率与窗口自动翻转。
- **`cancellation`** — `CancellationToken` + `CancellationScope`,为长生命周期后台任务提供协调的取消 + 有界 drain,见 [§ Cancellation](#cancellation-p1-3)。
- **`resource`** — `ResourceLimiter` 全局 + per-key 并发限流,见 [§ ResourceLimiter](#resourcelimiter-p1-5)。
- **`http`** — 在 `reqwest::Client` 之上同时套上熔断 + 重试 + 超时,产出 `ResilientHttpClient`。

## 特性 (Features)

- **`RetryPolicy` preset** — `None` / `Conservative` / `Aggressive`,等价于一组 `RetryConfig` 参数,绝大多数场景直接选预设即可。
- **`retry_with_backoff`** — `async fn(&mut RetryConfig, op: F) -> Result<T, RetryError>`;自动按 `ErrorKind` 判定是否瞬时,瞬时错误继续重试。
- **`CircuitBreaker`** — 共享状态 `Arc<...>`,可在多 task 间复用;支持显式 `force_open()` / `force_close()` 与自动恢复计时。
- **`ResilientHttpClient`** — `with_config(ResilientHttpConfig)` 即可拥有"重试 + 熔断 + 超时"的 HTTP 调用。
- **`CancellationScope`** — 协调一组后台任务的有界 shutdown;`cancel()` 翻转 token,`join(timeout)` 在超时前等待,超时后 force-abort 残留任务。详见下文。
- **`ResourceLimiter<K>`** — 全局 + per-key 并发限流;`try_acquire()` 立即返回,`acquire().await` 排队等待;`per_key_limit == 0` 表示只走全局。详见下文。
- **零 P2P 依赖** — 仅依赖 `tokio`、`reqwest`、`tracing`、`parking_lot`,无 ADNet 类型,无循环依赖风险。

## Cancellation (P1-3)

P1-3 落地后,`adnet-node::Node` 自带一个 `Arc<CancellationScope>`,在 `Node::shutdown()` 时:

1. `scope.cancel()` 翻转 token,所有 `select! { _ = token.cancelled() => ..., _ = work() => ... }` 形式的后台任务进入退出分支;
2. `scope.join(Duration::from_secs(5))` 等待 5 秒(可调),残留任务被 `abort()`;
3. 已有 teardown(mesh / relay / transport / iroh)继续照常进行 — scope 不替代它们的关闭顺序,只补齐它们没覆盖的"通用后台任务"。

### 用法

```rust
use adnet_resilience::{CancellationScope, CancellationToken};
use std::time::Duration;

let scope = CancellationScope::new();
let token: CancellationToken = scope.token();

// 后台任务:观察 cancellation
scope.spawn(Some("refresh-loop"), async move {
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                // 周期工作
            }
        }
    }
});

// 主流程:
scope.cancel();
let summary = scope.join(Duration::from_secs(5)).await;
assert!(summary.is_clean()); // 没有 force-abort
```

### 不在 P1-3 范围

- 全工作区 `tokio::spawn` 站点改造 — 这是后续 P2 项,scope 只为**新增**后台任务提供默认入口。
- 父子 scope 树 — 当前实现是扁平 scope,如需组合可以在 `cancellation.rs` 基础上扩展。
- Cancellation across process boundaries — 这属于 iroh-style 的 wire cancel,见 `adnet-transport` 后续规划。

## ResourceLimiter (P1-5)

P1-5 落地后,`adnet-node::Node` 自带三个 limiter:

- `peer_limiter` — 256 global / 16 per peer,用于 mesh fetch / DHT query / gossip handshake 等按对端 NodeId 分类的工作。
- `room_limiter` — 64 global / 32 per room,用于 gossip fan-out / room-feed rebuild。
- `tag_limiter` — 512 global / 64 per tag,兜底的"按子系统分类"限流(键是 `"blobstore.fetch"` / `"relay.proxy"` 这类字符串)。

每个 limiter 都是 `ResourceLimiter<String>`(key 是 `String`)。底层是 `tokio::sync::Semaphore` × N:
- 1 个全局 semaphore,大小 = `global_limit`;
- 每个 key 1 个 per-key semaphore,大小 = `per_key_limit`,懒创建;
- `per_key_limit == 0` 时跳过 per-key 桶(只走全局)。

`acquire()` 返回 `ResourcePermit`,drop 时自动归还全局 + per-key 槽位并增加 `released` 计数。

### 用法

```rust
use adnet_resilience::{AcquireError, ResourceLimiter};
use std::time::Duration;

let lim = ResourceLimiter::<String>::new(Default::default());
let key = peer_id.to_string();

// 非阻塞:有槽位就拿,没有就 None
if let Some(_permit) = lim.try_acquire(key.clone()) {
    do_work().await;
}

// 阻塞:排队等待,超时或被 cancel 则失败
let token = scope.token(); // P1-3 联动
match lim.acquire(key, Duration::from_secs(5), Some(token)).await {
    Ok(_permit) => { do_work().await; }
    Err(AcquireError::Cancelled) => { /* shutdown */ }
    Err(AcquireError::Timeout)   => { /* 拒负载 */ }
    Err(_) => { /* 全局池满 */ }
}
```

### Metrics

`lim.snapshot()` 返回:
- `acquired` — 累计成功 acquire 次数
- `try_rejected` — `try_acquire` 失败次数
- `rejected_key` / `rejected_global` — 按原因分类的拒绝次数
- `cancelled` — `acquire` 因 token 取消的次数
- `released` — permit drop 次数
- `waiting` — 当前 `acquire().await` 排队的任务数(近似)

接入 metrics 后,运维可监控 `rejected_global / (acquired + rejected_global)` 判断是否需要扩 cap。

### 与 CancellationScope 的联动

`acquire(key, timeout, Some(token))` 把 `CancellationToken` 接进了 select!。
P1-3 的 `Node::cancel_scope()` 返回的 token 直接传入,可以让正在排队的 acquire 在 shutdown 时立刻退出而不是继续占位等待。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-resilience = { workspace = true }
```

## 使用 (Usage)

### 1. 重试 + 指数回退

```rust
use adnet_resilience::{retry_with_backoff, RetryPolicy};

let policy = RetryPolicy::Aggressive.to_config();
let value = retry_with_backoff(&policy, || async {
    // 任意 async 操作,返回 Result<T, SomeError>
    fetch_remote_value().await
}).await?;
```

### 2. 熔断器

```rust
use std::sync::Arc;
use adnet_resilience::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

let breaker = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
    failure_threshold: 5,
    open_cooldown: std::time::Duration::from_secs(30),
    ..Default::default()
}));

// 在调用前检查:
if breaker.allow().await {
    match do_call().await {
        Ok(v) => { breaker.record_success().await; Ok(v) }
        Err(_) => { breaker.record_failure().await; Err(()) }
    }
} else {
    Err(()) // 熔断器已经 Open,直接快速失败
}
```

### 3. 弹性 HTTP 客户端

```rust
use std::time::Duration;
use adnet_resilience::{ResilientHttpClient, ResilientHttpConfig, RetryPolicy};

let client = ResilientHttpClient::with_config(ResilientHttpConfig {
    retry: RetryPolicy::Aggressive.to_config(),
    request_timeout: Duration::from_secs(5),
    ..Default::default()
});

let body = client.get_bytes("https://example.org/data").await?;
```

## 应用案例 (Use Cases / Examples)

- **`adnet-mesh`** — 所有"对远端节点"的调用都通过 `ResilientHttpClient`,把瞬时网络抖动吸收在 mesh 层,不污染业务。
- **`adnet-ipc`** — Unix socket 调用容易瞬时失败(对端重启 / 升级),`retry_with_backoff` 让客户端自动重连。
- **`adnet-relay`** — relay 拉取上游 CDN / IPFS 网关时,熔断器在多个 gateway 同时宕机时快速失败,避免长尾延迟。
- **DHT 查询** — `adnet-dht` 用 `retry_with_backoff` 包裹对 bootstrap 节点的 ping,网络抖动不再触发报警。
- **混沌工程** — `adnet-chaos` 的故障场景直接复用同一套 `RetryPolicy` / `CircuitBreaker` 配置来对比 baseline / chaos 行为。

## 许可

MIT OR Apache-2.0