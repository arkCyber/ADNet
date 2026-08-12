# adnet-resilience

> ADNet 通用弹性层 — `RetryPolicy` / `CircuitBreaker` / `ResilientHttpClient`,可被 mesh、ipc、relay 等多个 crate 共用,不依赖 ADNet 业务类型。

## 概览 (Overview)

`adnet-resilience` 是 ADNet 中所有"对外调用"都会用到的弹性组件,设计目标是 **transport-agnostic** — 不绑定任何 ADNet 业务类型,可以被任意子模块直接复用:

- **`retry`** — 指数回退 + 抖动 + 可配置上限的通用重试函数 `retry_with_backoff(&RetryConfig, op)`。
- **`circuit_breaker`** — 三态熔断器 `CircuitBreaker`(`Closed` / `Open` / `HalfOpen`),按失败率与窗口自动翻转。
- **`http`** — 在 `reqwest::Client` 之上同时套上熔断 + 重试 + 超时,产出 `ResilientHttpClient`。

## 特性 (Features)

- **`RetryPolicy` preset** — `None` / `Conservative` / `Aggressive`,等价于一组 `RetryConfig` 参数,绝大多数场景直接选预设即可。
- **`retry_with_backoff`** — `async fn(&mut RetryConfig, op: F) -> Result<T, RetryError>`;自动按 `ErrorKind` 判定是否瞬时,瞬时错误继续重试。
- **`CircuitBreaker`** — 共享状态 `Arc<...>`,可在多 task 间复用;支持显式 `force_open()` / `force_close()` 与自动恢复计时。
- **`ResilientHttpClient`** — `with_config(ResilientHttpConfig)` 即可拥有"重试 + 熔断 + 超时"的 HTTP 调用。
- **零 P2P 依赖** — 仅依赖 `tokio`、`reqwest`、`tracing`,无 ADNet 类型,无循环依赖风险。

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