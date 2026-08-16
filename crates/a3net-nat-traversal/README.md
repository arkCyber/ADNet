# a3net-nat-traversal

> NAT 穿透综合方案：STUN 探测、TURN 中继、UPnP 端口映射、UDP/TCP 打洞。 / Comprehensive NAT traversal: STUN discovery, TURN relay, UPnP port mapping, hole punching.

## 概览 (Overview)

`a3net-nat-traversal` 把 4 种穿透手段装到同一个 `NatTraversalManager` 后面：

1. **STUN (RFC 5389)** — 探测 NAT 类型 + 我们的公网地址。
2. **UPnP IGD** — 直接在 NAT 网关上开端口映射。
3. **Hole Punching** — 对称 NAT 之外的 UDP/TCP 路径。
4. **TURN (RFC 5766)** — 最后兜底，relay 流量。

`NatTraversalManager::discover()` 会按顺序尝试：直连 → UPnP → 打洞 → TURN，把
`NatInfo` 写回共享状态；上层（`a3net-transport`、`a3net-node`）拿到 `NatInfo` 后
决定 direct / relayed / turned 哪条链路走。

## 特性 (Features)

- `NatConfig` — 全功能开关 + STUN 服务器列表（默认 `StunServer::default_servers()`）。
- `NatTraversalManager::new(config)` + `discover()` — 主入口。
- `NatType::{OpenInternet, FullCone, RestrictedCone, PortRestrictedCone, Symmetric, Unknown}`。
- `StunClient::binding_request(server)` — 单次 STUN 探测。
- `UpnpClient::add_port_mapping(...)` — 添加 / 列出 / 删除映射。
- `TurnClient` + `TurnCredentials` — TURN 中继（可选 RFC 5766 鉴权）。
- `HolePunch` + `HolePunchResult` — UDP / TCP 打洞。

## 安装 (Installation)

```rust
use a3net_nat_traversal::{NatTraversalManager, NatConfig, NatInfo};
```

## 使用 (Usage)

### 1. 构造一个 manager

```rust,no_run
use a3net_nat_traversal::{NatConfig, NatTraversalManager};
let cfg = NatConfig::default();
let mgr = NatTraversalManager::new(cfg);
```

### 2. 跑一次 discover

```rust,no_run
# use a3net_nat_traversal::{NatConfig, NatTraversalManager};
# async fn demo(mgr: NatTraversalManager) -> Result<(), Box<dyn std::Error>> {
let info = mgr.discover().await?;
println!("NAT type   : {:?}", info.nat_type);
println!("public addr: {}", info.public_addr);
println!("can direct : {}", info.can_connect_direct());
Ok(())
# }
```

### 3. 直接用 STUN

```rust,no_run
use a3net_nat_traversal::{StunClient, StunServer};

let mut client = StunClient::new()?;
let resp = client
    .binding_request(&StunServer::new("stun.l.google.com:19302".parse()?))
    .await?;
println!("mapped: {:?}", resp.mapped_addr);
```

### 4. UPnP 加一条映射

```rust,no_run
use a3net_nat_traversal::{UpnpClient, PortMappingProtocol, PortMapping};

let upnp = UpnpClient::discover().await?;
let mapping = PortMapping {
    protocol: PortMappingProtocol::Udp,
    internal_port: 9000,
    external_port: 9000,
    description: "a3net".into(),
    lease_seconds: 3600,
};
upnp.add_port_mapping(&mapping).await?;
```

## 应用案例 (Use Cases / Examples)

- **`a3net-transport`** 在 `QuicTransport` 拨号失败时调用 `NatTraversalManager::discover()`
  重新拿公网地址，更新自己的 `Endpoint`。
- **`a3net-node`** 把 `NatInfo::external_addr` 放到 provider record 里，其它 peer 能直接 dial。
- **`adnat-cli`** 的 `ray nat status` 子命令：展示当前 NAT 类型与公网地址。

## 许可

MIT OR Apache-2.0