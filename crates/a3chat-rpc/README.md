# `a3chat-rpc`

> JSON-RPC 2.0 server, owner 多路复用通知总线。
>
> **Endpoints**:
>
> | Method | Path | 用途 |
> |---|---|---|
> | `POST` | `/rpc` | JSON-RPC 2.0 calls(单条 / 批量) |
> | `GET`  | `/rpc/stream` | Server-Sent Events,认证 owner 拉取推送 |
> | `POST` | `/rpc/notify` | 服务端内部广播(同进程内 publish) |
> | `GET`  | `/rpc/health` | Liveness probe |
> | `GET`  | `/rpc/version` | 构建信息 |
> | `GET`  | `/metrics` | Prometheus 抓取 |
> | `GET`  | `/rpc/stats` | 内部 RPC 指标(JSON) |
> | `GET`  | `/rpc/methods` | 返回标准 `A3chatRpcMethod::ALL` 列表 |
>
> **Methods**: every constant in [`a3chat_core::rpc::A3chatRpcMethod`] is dispatched to the matching [`a3chat_app`] service. The handler is a thin wrapper around `A3chatApp::dispatch`。
>
> **Authentication**: owner identity is supplied via the `X-A3Chat-Owner` header (per P0 design). P1 will swap this for Noise_XX-authenticated token exchange.
>
> **Telemetry**: every call lives inside a `tracing` span carrying `request_id` (mirror of `X-A3Chat-Request-Id` header). `tower_http::trace::TraceLayer` records request lifecycle at `info`.

## 配置

```rust
use a3chat_rpc::{RpcServer, RpcServerConfig};
use std::time::Duration;

let cfg = RpcServerConfig::new("127.0.0.1:53421".parse().unwrap())
    .with_timeout(Duration::from_secs(30))   // 单请求执行预算
    .allow_origin("https://a3chat.example");  // CORS 白名单,可多次调用
let server = RpcServer::new(app, cfg);
let handle = server.start().await?;
```

| 字段 | 默认 | 说明 |
|---|---|---|
| `bind_addr` | `127.0.0.1:0` | 必须显式设置 |
| `log_requests` | `true` | span 是否在 info 级别 |
| `request_timeout` | `30s` | `DEFAULT_REQUEST_TIMEOUT`。`dispatch_one` 从 `ServerState` 读取,可通过 `with_timeout` 调整 |
| `allowed_origins` | `[]` | CORS 允许的 origin,空意味着同源 |

`start()` 返回 `RpcServerHandle` — drop 即关闭,或显式 `handle.stop().await`。

## Module

| Module | 内容 |
|---|---|
| `error` | `RpcError` — JSON-RPC 2.0 标准错误码 + `a3chat-*` 扩展 |
| `server` | `RpcServer` (axum) + builder + lifecycle + `ServerState` (注入 `request_timeout`) |
| `dispatch` | `dispatch_rpc_call` — axum handler 调用入口 |
| `sse` | `sse_handler` — 把 `NotificationBus` 接到 SSE 流 |
| `metrics` | `Metrics` + `RpcOutcome` — 内部 RPC 计数器 / histogram |

## License

MIT OR Apache-2.0
