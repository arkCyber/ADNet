# a3net-observability

> 轻量级 metrics、histogram、health-check 与 Prometheus 导出 — 无 `prometheus` crate 依赖、进程内实现的 A3Net 运行时观测层。

## 概览 (Overview)

`a3net-observability` 是 A3Net 工作空间的"指标 + 健康检查"中心:

- **零外部 Prometheus 依赖** — `Counter` / `Gauge` / `Histogram` 用原子整数 + 比特位技巧实现,无锁线程安全。
- **进程内注册表** — `Registry` 持有命名 metric + 标签;支持快照与排序迭代。
- **Prometheus 文本导出** — `PrometheusExporter::render()` 产出与 Prometheus 抓取协议兼容的纯文本。
- **HTTP 暴露** — `http::serve()` 在任意 `tokio` 监听器上挂载 `/metrics` 与 `/health` 端点,供运维抓取。
- **健康检查** — `HealthCheck` trait + 全局注册表,允许各子系统注册自己的就绪探针。
- **HTTP tracing bridge** — `bridge` 模块把 `a3net-transport` 的 HTTP 指标接入本 crate 的 `Registry`。

## 特性 (Features)

- **`Registry`** — 进程内全局注册表,`register_counter` / `register_gauge` 直接返回 `Arc<…>`,无需再 `register`。
- **`Counter`** — 单调递增计数器,`inc()` / `inc_by(n)`,底层 `AtomicU64`。
- **`Gauge`** — 可增可减的值,`set(v)` / `inc()` / `dec()`,底层 `AtomicI64`。
- **`Histogram`** — 默认桶布局 `1ms / 10ms / 100ms / 500ms / 1s / 5s / 10s / +Inf`;`f64` 累计 sum 用 `f64::to_bits` 转 `AtomicU64`。
- **`LabelSet` / `LabelKey`** — 固定 shape 的标签契约;支持过滤导出时的标签组合。
- **`PrometheusExporter`** — 渲染 `# HELP` / `# TYPE` / metric 行的标准 Prometheus 文本格式。
- **`http::serve()`** — 在 `tokio::TcpListener` 上同时挂载 `/metrics` 和 `/health`。
- **`HealthCheck` trait + `run_checks()`** — 子系统注册布尔健康探针,HTTP 端点返回 200 / 503。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
a3net-observability = { workspace = true }
```

无 `Cargo.toml` 改动即可在 A3Net workspace 内任意 crate 引用。

## 使用 (Usage)

### 1. 注册 Counter / Gauge / Histogram

```rust
use a3net_observability::histogram::Histogram;
use a3net_observability::registry::Registry;

let registry = Registry::default();

let requests = registry.register_counter(
    "a3net_demo_requests_total",
    "Total demo requests",
);
requests.inc(); requests.inc_by(2);

let active = registry.register_gauge(
    "a3net_demo_active_connections",
    "In-flight connections",
);
active.set(7); active.dec();

let latency = Histogram::new("a3net_demo_latency_seconds", "request latency");
latency.observe(0.05); latency.observe(0.5); latency.observe(2.0);
```

### 2. Prometheus 文本导出

```rust
use a3net_observability::prometheus::PrometheusExporter;

let exporter = PrometheusExporter::new(&registry);
let out = exporter.render();
println!("{}", out.text());
// out.content_type == "text/plain; version=0.0.4; charset=utf-8"
```

### 3. 快照迭代

```rust
let snap = registry.snapshot();
for m in snap.sorted() {
    println!("{}  ({})", m.name(), m.kind().as_prometheus_str());
}
```

### 4. HTTP `/metrics` 端点

```rust
use std::net::SocketAddr;
use a3net_observability::http;

let addr: SocketAddr = "0.0.0.0:9090".parse().unwrap();
// 在一个 tokio runtime 中:
http::serve(addr, registry.clone()).await?;
// GET /metrics → Prometheus 文本
// GET /health  → 200 OK + JSON body(若所有 health-check 通过)
```

### 5. 健康检查

```rust
use a3net_observability::health::{HealthCheck, run_checks};

struct DbPing;
#[async_trait::async_trait]
impl HealthCheck for DbPing {
    fn name(&self) -> &str { "db" }
    async fn check(&self) -> bool { /* ping */ true }
}

a3net_observability::health::register(DbPing);
// 之后 run_checks() 会聚合所有注册的 check,全部通过返回 true。
```

## 应用案例 (Use Cases / Examples)

- **A3Net 节点入口** — `a3net-node` 在启动时 `Registry::default()`,注册 bitswap 流量、DHT 查询、gossip 速率等 Counter / Histogram,并通过 `http::serve` 在 `:9090` 暴露 `/metrics`;Kubernetes scrape 直接生效。
- **网关观测** — `a3net-gateway` 在每个 HTTP handler 前后观察一次延迟 Histogram,把状态码打成 Label;运维通过 `histogram_quantile` 计算 p95 / p99。
- **混沌测试** — `a3net-chaos` 在每次注入故障时增加 `chaos_injections_total{kind}` 计数器,实验报告里直接读取导出。
- **可观测性回路** — `a3net-error` 在 `metrics` feature 开启后,把 `AdnetErrorReport::emit()` 接入 `a3net_error_total{code,kind,severity}` 计数器;`a3net-observability` 的 HTTP 端点一起暴露给 Prometheus。
- **告警面板** — `dashboard.html` 配合 Registry 的 `Snapshot` API,把指标拉到浏览器内做实时图。

## 许可

MIT OR Apache-2.0