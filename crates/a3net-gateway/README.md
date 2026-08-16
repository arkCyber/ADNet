# a3net-gateway

> IPFS 兼容的 HTTP 网关：让现有 IPFS 客户端能通过 `/ipfs/<cid>`、`/ipns/<name>` 路径访问 A3Net 上的内容。 / IPFS-compatible HTTP Gateway — fetch A3Net content via `/ipfs/<cid>` and `/ipns/<name>`.

## 概览 (Overview)

`a3net-gateway` 提供一个 axum-based HTTP 服务，对外实现 IPFS HTTP Gateway 规范的核心子集：

- `GET /ipfs/{cid}` — 通过 CID 取内容。
- `GET /ipfs/{cid}/{path}` — 在 DAG 内沿 UnixFS 路径取子节点。
- `GET /ipns/{name}` — 解析可变名字，再走 IPFS 路径。
- `POST /api/v0/dag/...`、`/api/v0/pin/...`、`/api/v0/cat`、`/api/v0/block/...` — IPFS RPC 接口。
- WebSocket 事件流（PubSubService）。
- Bearer / Basic 认证 + 速率限制（`auth.rs`）。
- 可选 IPC Unix-socket 后端（`ipc`），让进程内客户端能 JSON-RPC 调网关。

Crate 把"配置 + 服务 + 度量"封装得很细：`GatewayConfig` 决定绑定地址、CORS、TTL、IPC、auth，
`GatewayRouter` 是 axum router；上层（`a3net-cli` 的 `a3net-gateway` 子命令）只是薄壳。

## 特性 (Features)

- `GatewayConfig::default()` — 一键起一个 `0.0.0.0:8080` 的只读网关。
- `GatewayHandler` / `DagService` / `PinService` / `DhtService` / `IpnService` — 子服务。
- `bitswap_api::create_bitswap_router` — Bitswap 兼容端点。
- `PubSubService::start_websocket_server` — 事件 WebSocket。
- `GatewayMetrics::register(&registry)` — 暴露 Prometheus 风格 metrics。
- `AuthService` + `bearer_token` / `basic_auth` 工具函数。

## 安装 (Installation)

```rust
use a3net_gateway::{
    GatewayConfig, GatewayRouter, GatewayMetrics,
    auth::AuthService, bitswap_api::create_bitswap_router,
};
```

## 使用 (Usage)

### 1. 用默认配置起一个网关

```rust,no_run
use a3net_gateway::GatewayConfig;

let cfg = GatewayConfig::default();
println!("bind : {}", cfg.bind_addr);
println!("cors : {}", cfg.cors_enabled);
```

### 2. 自定义配置

```rust,no_run
use a3net_gateway::{GatewayConfig, TlsConfig};
let cfg = GatewayConfig {
    bind_addr: "127.0.0.1:8080".into(),
    writable: false,
    enable_ipns: true,
    rate_limit: 100,
    rate_limit_window: 60,
    ..Default::default()
};
let _ = cfg;
```

### 3. 注册 metrics

```rust,no_run
use a3net_gateway::metrics::GatewayMetrics;
let registry = std::sync::Arc::new(a3net_observability::registry::Registry::default());
let metrics = GatewayMetrics::register(&registry);
println!("requests_total = {}", metrics.requests_total.name());
```

### 4. Bitswap 兼容路由

```rust,no_run
use a3net_gateway::bitswap_api::create_bitswap_router;
let router = create_bitswap_router(/* … */);
```

## 应用案例 (Use Cases / Examples)

- **`a3net-cli`** 把 `GatewayRouter` 暴露成 `a3net-gateway` 二进制，监听 `0.0.0.0:8080`，
  接受 IPFS HTTP 客户端的只读请求。
- **`a3net-blobstore`** 的 `BlobStore` 作为网关后端的存储源（`/blobs/<hash>`）。
- **第三方 IPFS 工具**：`ipfs cat <cid>`、`ipfs add`、`pin` 命令都能通过本 gateway 工作，
  适合把现有 IPFS 应用栈平迁到 A3Net。

## 许可

MIT OR Apache-2.0