# a3net-relay

> A3Net mesh 流量的 WAN 中继 HTTP 代理：让 NAT 之后的节点也能相互访问。 / WAN relay HTTP proxy that lets A3Net mesh nodes behind NAT reach each other.

## 概览 (Overview)

`a3net-relay` 既是 *客户端*（构造代理 URL），也是 *服务端*（一个 axum 实现的轻量 HTTP 转发器）。

- **客户端** [`RelayClient`] 负责把 `(host, port, path)` 拼成
  `/exodus-mesh/fetch?host=…&port=…&path=…` 这样的代理 URL。`RelayConfig` 控制
  `enabled`、`relay_base_url`、host policy、上游超时、重试次数等。
- **服务端** [`RelayServer`] 监听端口，校验 path 必须以 `/blobs/...` 开头（防滥用），
  转发到内网 mesh HTTP 端点。重试 / backoff 由 `a3net-resilience` 提供。
- **可选 `billing` feature**：开启后 `RelayServer` 会接受签名化的 pledge 并兑换
  receipt；默认不启用，relay 就是个纯转发代理。

设计要点：

- 严格 path 校验（仅放行 `/blobs/...` 前缀，不允许目录穿越）。
- 复用 `a3net-resilience`，上游失败自动重试。
- 兼容 iroh 风格 relay URL 措辞，但不依赖 iroh。

## 特性 (Features)

- `RelayConfig` — 持久化配置（`save` / `load`）。
- `RelayClient::proxy_url(host, port, mesh_path)` — 构造代理 URL。
- `RelayServer::start(bind, port, billing_mode)` — 启动中继。
- `RelayServerHandle::shutdown` — 优雅关闭。
- `HostPolicy` — host 黑白名单 / 通配。
- 可选 `BillingMode::Disabled | Local { treasury }`。

## 安装 (Installation)

```rust
use a3net_relay::{RelayClient, RelayConfig, RelayServer, BillingMode};
```

## 使用 (Usage)

### 1. 启动本地 relay

```rust,no_run
use a3net_relay::{BillingMode, RelayServer};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let handle = RelayServer::start("127.0.0.1", 18790, BillingMode::Disabled).await?;
    println!("up at {}", handle.base_url);
    handle.shutdown();
    Ok(())
}
```

### 2. 构造代理 URL（客户端）

```rust,no_run
use a3net_relay::{RelayClient, RelayConfig};

let cfg = RelayConfig {
    enabled: true,
    relay_base_url: Some("https://relay.example.com".into()),
    ..Default::default()
};
let client = RelayClient::new(cfg);
if let Some(url) = client.proxy_url("10.0.0.1", 7878, "/blobs/abc/meta") {
    println!("proxy URL = {url}");
}
```

### 3. 持久化配置

```rust,no_run
use a3net_relay::RelayConfig;
use std::path::Path;

let cfg = RelayConfig::default();
cfg.save(Path::new("./app_data"))?; // writes ./app_data/relay.json
let loaded = RelayConfig::load(Path::new("./app_data"));
```

### 4. 自定义 host policy

```rust,no_run
use a3net_relay::{HostPolicy, RelayConfig};

let cfg = RelayConfig {
    enabled: true,
    relay_base_url: Some("https://relay.example.com".into()),
    host_policy: HostPolicy::allow_list(&["10.0.0.0/8"]),
    ..Default::default()
};
```

## 应用案例 (Use Cases / Examples)

- **`a3net-node`** 在 `RelayEndpointInfo` 处把 relay URL 注入节点的传输层；fetch 走不通时
  自动通过 relay 兜底。
- **`a3net-cli`** 暴露 `ray relay start / stop / status` 命令，直接复用 `RelayServer` 与
  `RelayConfig::save/load`。
- **运营商自建**：在公网机器上跑 `RelayServer`，让分布在多个 NAT 私网中的 mesh 节点
  通过该中继共享 `/blobs/*`。

## 许可

MIT OR Apache-2.0