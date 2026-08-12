# adnet-error

> ADNet 统一的错误报告模型(`AdnetErrorReport` + `ErrorKind` + `Severity`),在 RPC、FFI、CLI、HTTP gateway 等边界处提供稳定的、可机器解析的错误形态。

## 概览 (Overview)

ADNet 中每个 crate 各自定义 `thiserror::Error` 枚举,适合内部使用;但在 **边界**(RPC、FFI、CLI 退出码、HTTP gateway)上需要一个稳定、可被运维分组计数与追踪的统一错误模型。`adnet-error` 就是 ADNet 的"边界错误契约":

- **`AdnetErrorReport`** — 跨层传输的扁平 JSON 形态,模仿 Hessian 2 错误信封(Dubbo / Motan)。包含 `code`、`kind`、`severity`、`message`、`source_loc`、`correlation`、`cause`、`details`。
- **`IntoReport` trait** — 让每个 crate 的内部错误枚举通过 `into_report("crate-name")` 在边界处被举升为 `AdnetErrorReport`。
- **`ErrorKind`** — 粗粒度的可重试判定(`NotFound` / `Timeout` / `Internal` / `BadRequest` / `Unauthorized` 等),可被重试 / 熔断策略直接消费。
- **`Severity`** — `Info` / `Warn` / `Error` / `Fatal`,决定 tracing 输出等级与告警门限。
- **`emit()`** — 在 `tracing` 子系统中输出,并在开启 `metrics` feature 时自增 `adnet_error_total` 计数器。

## 特性 (Features)

- **`AdnetErrorReport`** — 序列化稳定的 JSON 形态,`code` 由 crate 自行约定(如 `BLB-001`、`RPC-014`),运维依赖其稳定性,**禁止重排**。
- **`IntoReport`** — 在边界处将内部错误举升为统一报告,自动走过 `std::error::Error` 链构造 `cause` 字段。
- **`ErrorKind`** — 内置十余种分类,每种都关联推荐 HTTP 状态码(`http_status()`)与"是否瞬时"(`is_transient()`)。
- **`Severity`** — 决定日志输出等级;`emit()` 走 `tracing`,JSON log shipper 可按 `code` / `kind` / `crate` 分组。
- **Details** — `BTreeMap<String, serde_json::Value>`,允许携带任意上下文(`hash`、`size_bytes`、`peer`、…)。
- **Optional features**:
  - `default` — tracing emit,无 metrics。
  - `source-loc` — 捕获调用点的 `file:line`。
  - `metrics` — 启用 `adnet_error_total` 计数器,注册到 `adnet-observability::Registry`。

## 安装 (Installation)

`adnet-error` 是一个工作空间内的 lib crate,被 ADNet 所有面向边界的 crate 依赖。

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-error = { workspace = true }
adnet-observability = { workspace = true }   # 若要使用 metrics feature
```

在代码中:

```rust
use adnet_error::{AdnetErrorReport, ErrorKind, Severity, IntoReport};
```

## 使用 (Usage)

### 1. 直接构造 `AdnetErrorReport` 并发送

```rust
use adnet_error::{AdnetErrorReport, ErrorKind, Severity};

let report = AdnetErrorReport::new(
    "BLB-001",
    ErrorKind::NotFound,
    Severity::Warn,
    "blob not found",
    "adnet-blobstore",
)
.with_correlation("op-42")
.with_detail("hash", "ab12cd34…")
.with_detail("size_bytes", 4096_u64);

let json = serde_json::to_string_pretty(&report).expect("serialize");
report.emit(); // → tracing + (可选)metrics counter
```

### 2. 在自定义错误类型上实现 `IntoReport`

```rust
use adnet_error::{AdnetErrorReport, IntoReport, ErrorKind, Severity};

#[derive(Debug, thiserror::Error)]
pub enum MyError {
    #[error("bad ticket: {0}")]
    BadTicket(String),
}

impl IntoReport for MyError {
    fn code(&self) -> &'static str { "MYC-001" }
    fn kind(&self) -> ErrorKind { ErrorKind::BadRequest }
    fn severity(&self) -> Severity { Severity::Warn }
}

// 边界处:
let report: AdnetErrorReport = MyError::BadTicket("…".into()).into_report("my-crate");
```

### 3. JSON 往返 / HTTP 状态码

```rust
use adnet_error::{ErrorKind, AdnetErrorReport};

let report: AdnetErrorReport = serde_json::from_str(r#"{
  "code":"BLB-001","kind":"NotFound","severity":"Warn",
  "message":"blob not found","crate_name":"adnet-blobstore","details":{}
}"#).unwrap();

assert_eq!(report.kind.http_status(), 404);
assert!(!report.kind.is_transient());
```

### 4. cause 链(自动走 `std::error::Error`)

```rust
use adnet_error::{AdnetErrorReport, IntoReport, Severity};

let inner = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read");
let report = inner.into_report("adnet-demo");
// report.cause 会被填为 "short read",更深的链也会被折叠进 cause 字段
```

## 应用案例 (Use Cases / Examples)

- **`adnet-rpc` / `adnet-ffi`** — RPC handler 在边界上把所有内部错误举升为 `AdnetErrorReport`,JSON-RPC 客户端在拿到错误码后可由 `code` 直接跳到 UX 提示,无需解析 `message` 字符串。
- **CLI 退出码** — `adnet-cli` 把 `AdnetErrorReport` 映射为 `process::ExitCode`,`code` 决定退出码分组,`Severity::Fatal` 直接 `ExitCode::from(70)`(EX_SOFTWARE)。
- **HTTP gateway** — `adnet-gateway` 走 `ErrorKind::http_status()` 把错误转换为 4xx / 5xx,并把 `correlation` 透传回响应头(`X-Adnet-Correlation`)。
- **告警平台** — 开启 `metrics` feature 后,告警平台按 `adnet_error_total{code,kind,severity}` 分组,`Severity::Fatal` 直接触发页面。
- **可观测性聚合** — `tracing` 子系统按 `code` / `crate_name` 标签做日志聚合;同一错误的不同请求被合并成同一条规则,避免爆炸式告警。

## 许可

MIT OR Apache-2.0