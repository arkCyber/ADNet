# a3net-exit-node

> 网关 / 出口节点路由：让 mesh 成员能 "借用" 其他成员的网络出口。 / Gateway / exit-node routing for the A3Net mesh VPN.

## 概览 (Overview)

`a3net-exit-node` 把 mesh VPN 拆成两个角色：

- **Gateway** — 愿意把自己 Internet 连接 "借给" 其它成员用的节点。
  `Gateway::allow` 注册自身为候选；`Gateway::revoke` 撤回。状态通过 gossip topic
  广播给 mesh 其他成员。
- **Client** — 想借用 gateway 上网的节点。
  `Client::use_gateway` 设置当前使用的 gateway；`Client::unset` 清除。
  发往 *非 mesh* 目的地的包会被转发到 gateway，而不是被丢弃。

中间是 [`Router`]：纯函数式决策机，输入目的 IP，输出 `RouteAction`：

- `ForwardMesh` — 目标在 mesh 内，直接走 mesh。
- `ForwardGateway` — 目标在外网，走 gateway。
- `Drop` — 既不在 mesh 也不该走 gateway 的目标。

Crate 只负责"决策"，不发包。Linux/macOS 上的真实 IP 转发、iptables、NAT 由 `a3net-tun`
或运维脚本完成。

附加能力：

- `bandwidth` — 每个 client / 全局的带宽计量。
- `billing` — 用 `a3net-token` 做出口流量计费。
- `transit` — 中转拓扑（节点允许作为跨 mesh 路径中的中转）。

## 特性 (Features)

- `Gateway` / `GatewayAdvert` / `GatewayState` — gateway 注册表。
- `Client` / `ClientConfig` / `ClientState` — 客户端侧状态。
- `Router::decide(&IpAddr)` — 路由决策。
- `RouterSnapshot` — 路由决策快照（审计用）。
- `BandwidthSnapshot` / `RateLimitConfig` — 限流。
- `BillingEngine` / `Invoice` / `RateCard` — 计费。

## 安装 (Installation)

```rust
use a3net_exit_node::{Gateway, Client, Router, RouterConfig, RouteAction};
use std::net::IpAddr;
```

## 使用 (Usage)

### 1. 决策：包该走哪？

```rust,no_run
use a3net_exit_node::{RouteAction, Router, RouterConfig};
use std::net::IpAddr;

let router = Router::new(RouterConfig::default());
let target: IpAddr = "8.8.8.8".parse().unwrap();
match router.decide(target) {
    RouteAction::ForwardMesh(peer)     => println!("→ mesh peer {peer}"),
    RouteAction::ForwardGateway(gw)    => println!("→ gateway {gw}"),
    RouteAction::Drop                   => println!("drop"),
}
```

### 2. 注册 Gateway 自身

```rust,no_run
use a3net_exit_node::{Gateway, GatewayAdvert};
use a3net_types::NodeId;

let gw = Gateway::default();
let me = NodeId::random();
gw.allow(GatewayAdvert { node_id: me, region: "eu-west".into(), max_mbps: 1000 });
```

### 3. 客户端选择 gateway

```rust,no_run
use a3net_exit_node::Client;

let me  = a3net_types::NodeId::random();
let cli = Client::new(me);
let gw  = a3net_types::NodeId::random();
cli.use_gateway(gw);
// ... 用完
cli.unset();
```

### 4. 带宽计量

```rust,no_run
use a3net_exit_node::{ClientMeter, RateLimitConfig};

let meter = ClientMeter::new(RateLimitConfig::default());
meter.record_tx(1024);
meter.record_rx(2048);
let snap = meter.snapshot();
println!("tx={} rx={}", snap.tx_bytes, snap.rx_bytes);
```

## 应用案例 (Use Cases / Examples)

- **`a3net-cli`** 用 `Client::use_gateway` 实现 `ray gateway use <name>` 子命令。
- **`a3net-tun`** 把每个出 mesh 包送给 `Router::decide`，按结果分发给 mesh / gateway。
- **多 mesh 联邦**：`transit` / `transit_gossip` 让 gateway 状态在多个 mesh 之间传播，
  形成跨 mesh 出口。

## 许可

MIT OR Apache-2.0