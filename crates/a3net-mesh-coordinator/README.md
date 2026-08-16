# a3net-mesh-coordinator

> 封闭 mesh 网络的协调者：邀请码、roster 生命周期、加入审批队列。 / Coordinator-side admission for closed mesh networks.

## 概览 (Overview)

`a3net-mesh-coordinator` 把 "封闭 mesh 网络的看门人" 拆成三件事：

1. **邀请码** — 单次使用、可过期；URL 形式是 `a3net-invite://<network>:<code>`，
   兑换端点走 `Coordinator::redeem`。
2. **Roster 生命周期** — 协调者维护一份签名化的 [`MeshMembership`]，每次有人加入 / 离开 /
   被踢 / 过期都会自增版本号。
3. **实时审批队列** — 收到 `request_join` 后入队，运营者通过 `accept_request` /
   `deny_request` 决定是否接纳。

签名本身由 `a3net-identity` 负责，本 crate 只关心 *wire 格式* 与 *状态*。生产部署
会把 `InMemoryCoordinator` 包装一个 SQLite-backed store。

## 特性 (Features)

- `Coordinator` trait — `create / mint_invite / redeem / kick / request_join / accept_request / deny_request / roster / snapshot`。
- `InMemoryCoordinator` — 默认实现。
- `JoinRequest` / `JoinRequestId` / `JoinRequestStatus` — 加入请求的 DTO。
- `MAX_INVITE_TTL` / `MAX_REQUESTS` / `MAX_NOTE_LEN` — 限速常量。
- `Peerings` / `PeeringGrant` — 跨 mesh 联邦授权。

## 安装 (Installation)

```rust
use a3net_mesh_coordinator::{Coordinator, CoordinatorConfig, InMemoryCoordinator};
use a3net_types::{MeshNetworkId, MeshPolicy, NodeId};
```

## 使用 (Usage)

### 1. 创建一个封闭网络

```rust,no_run
use a3net_mesh_coordinator::{CoordinatorConfig, InMemoryCoordinator};
use a3net_types::{MeshNetworkId, MeshPolicy, NodeId};

let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
let network = MeshNetworkId::from_bytes(&[1u8; 32])?;
let creator = NodeId::random();
let initial = coord.create(network.clone(), "gaming".into(), MeshPolicy::Closed, creator)?;
println!("network created, version {}", initial.version);
```

### 2. 发邀请码 + 兑换

```rust,no_run
use a3net_mesh_coordinator::{Coordinator, InMemoryCoordinator, CoordinatorConfig};
# use a3net_types::{MeshNetworkId, MeshPolicy, NodeId};

let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
let network = MeshNetworkId::from_bytes(&[1u8; 32])?;
let invite = coord.mint_invite(&network, None)?;
let friend = NodeId::random();
let member = coord.redeem(&network, &invite.code, friend, "alice".into())?;
println!("admitted {} as {}", member.hostname, member.node_id.short());
```

### 3. 实时审批队列

```rust,no_run
use a3net_mesh_coordinator::Coordinator;
# use a3net_mesh_coordinator::{CoordinatorConfig, InMemoryCoordinator};
# use a3net_types::{MeshNetworkId, MeshPolicy, NodeId};

let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
let network = MeshNetworkId::from_bytes(&[1u8; 32])?;
let peer = NodeId::random();
let req_id = coord.request_join(&network, peer, "bob".into(), "please".into())?;
let _ = coord.accept_request(&network, req_id)?;
```

### 4. 踢人 + snapshot

```rust,no_run
# use a3net_mesh_coordinator::{Coordinator, CoordinatorConfig, InMemoryCoordinator};
# use a3net_types::{MeshNetworkId, MeshPolicy, NodeId};
let coord = InMemoryCoordinator::new(CoordinatorConfig::default());
let network = MeshNetworkId::from_bytes(&[1u8; 32])?;
let bad = NodeId::random();
coord.kick(&network, &bad)?;
let snap = coord.snapshot();
println!("{} networks, {} pending", snap.networks.len(), snap.pending_requests.len());
```

## 应用案例 (Use Cases / Examples)

- **`a3net-cli`** 用 `Coordinator::mint_invite` 实现 `ray invite` 命令，
  `redeem` 给 `ray join`，`accept_request` 给 `ray accept`，`deny_request` 给 `ray deny`。
- **`a3net-magicdns`** 在 roster 更新时调用 `Coordinator::roster(network)` 拿到最新的
  `MeshMembership`，驱动 DNS 解析。
- **`a3net-tun`** 在允许新 peer 加入 mesh VPN 前先确认 `coord.roster` 包含对方 `NodeId`。

## 许可

MIT OR Apache-2.0