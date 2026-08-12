# adnet-mesh-firewall

> ADNet mesh VPN 的用户态包过滤：方向性规则 + conntrack + 默认策略。 / Userspace packet filter for the ADNet mesh VPN — directional rules, conntrack, default policy.

## 概览 (Overview)

`adnet-mesh-firewall` 在 ADNet mesh VPN 里充当 "第二道闸"（第一道是宿主内核防火墙）。
包路径上的 `FirewallEngine::decide()` 同步判定 ALLOW / DENY，并把判定原因
（`DecisionReason`）留作审计。

默认策略对齐 rayfish / Tailscale：

- **入站** TCP/UDP 默认拒绝；ICMP / ICMPv6 ping 默认放行。
- **出站** 全部放行。
- **Conntrack**：本机发起的连接会被记入 `(proto, src_port, peer_node_id)` 表，
  对端回流包在连接生命周期内自动放行；超时由 `DEFAULT_CONN_TIMEOUT` 控制。

规则形态是 `(direction, action, proto, port, peer?)`，first-match-wins。
顺序由 `RuleSet::push` 控制；`MAX_RULES = 1024`。

## 特性 (Features)

- `FirewallEngine::decide(Packet) -> (Decision, DecisionReason)` — 同步判定入口。
- `Rule::allow / Rule::deny` + `with_peer(NodeId)` — 构造规则。
- `DefaultPolicy { inbound_tcp_udp, inbound_icmp, outbound }` — 默认行为。
- `ConnTracker` / `ConnProto` / `ConnTrackerConfig` — 出站对称回流。
- `declarative::FirewallSpec` + `StaticPeerResolver` — YAML / TOML 装载。
- `FirewallStats` — 度量计数器（接 `adnet-observability`）。

## 安装 (Installation)

```rust
use adnet_mesh_firewall::{
    FirewallEngine, FirewallConfig, DefaultPolicy, FirewallStats, Packet,
    rule::{Action, Direction, PortSpec, ProtoSpec, Rule, RuleSet},
};
```

## 使用 (Usage)

### 1. 默认 deny inbound TCP + allow SSH

```rust,no_run
use adnet_mesh_firewall::{
    DefaultPolicy, FirewallConfig, FirewallEngine, FirewallStats,
    rule::{Action, Direction, PortSpec, ProtoSpec, Rule, RuleSet},
};
use std::sync::Arc;

let mut rs = RuleSet::new();
rs.push(Rule::allow(Direction::In, ProtoSpec::Tcp, PortSpec::Single(22)));

let cfg = FirewallConfig {
    default_policy: DefaultPolicy {
        inbound_tcp_udp: Action::Deny,
        inbound_icmp: Action::Allow,
        outbound: Action::Allow,
    },
    rules: rs,
    ..Default::default()
};
let engine = FirewallEngine::new(cfg, Arc::new(FirewallStats::default()));
```

### 2. 判定一个包

```rust,no_run
use adnet_mesh_firewall::{Decision, Packet, rule::Direction};
use std::net::{IpAddr, Ipv4Addr};

let peer = adnet_types::NodeId::random();
let (decision, reason) = engine.decide(Packet {
    direction: Direction::In,
    proto: 6,           // TCP
    port: 22,
    peer: &peer,
    src_ip: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 5)),
});
assert_eq!(decision, Decision::Allow);
```

### 3. 记录一条出站连接让回流包自动放行

```rust,no_run
use adnet_mesh_firewall::ConnProto;
engine.open_outbound(ConnProto::Tcp, peer.clone(), 443, "100.64.0.5".parse().unwrap(), 44444)?;
```

### 4. 加载 YAML / TOML 规则

```rust,no_run
use adnet_mesh_firewall::declarative::{FirewallSpec, StaticPeerResolver};

let yaml = "policy: { inbound_tcp_udp: deny, inbound_icmp: allow, outbound: allow }\nrules: []";
let spec: FirewallSpec = serde_yaml::from_str(yaml)?;
let _peers = StaticPeerResolver::default();
```

## 应用案例 (Use Cases / Examples)

- **`adnet-tun`** 在每个包的出口/入口都问 `FirewallEngine::decide`，按结果路由或丢弃。
- **`adnet-cli`** 用 `FirewallSpec` 从 `~/.config/ray/firewall.yaml` 加载规则，
  实现 `ray firewall apply`。
- **多租户 mesh**：每条 `Rule::with_peer(node_id)` 把规则限定到一个特定 peer，
  类似 nftables / ipset 的语法。

## 许可

MIT OR Apache-2.0