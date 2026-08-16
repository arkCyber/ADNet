# a3net-tun

> 跨平台 TUN 设备抽象：在内核和 mesh 用户栈之间搬运 IPv4/IPv6 包。 / Cross-platform TUN device abstraction — carry IPv4/IPv6 packets between the kernel and the A3Net mesh userspace stack.

## 概览 (Overview)

`a3net-tun` 把"虚拟网卡"这件事统一成一个 trait [`TunDevice`]，并提供两种实现：

- **`UserspaceTun`**（默认）—— 纯 tokio-channel 实现的虚拟 TUN。
  - `inject_from_kernel(pkt)` 模拟"内核写入 TUN"。
  - `recv()` 是 mesh 栈读包。
  - `send(pkt)` 是 mesh 栈写包。
  - `drain_to_kernel()` 把 mesh 写出的包交给"内核侧"消费者。
  不需要 root / 真实 TUN，CI 与 unprivileged 进程都用它。

- **`NativeTun`**（feature `native`，macOS / Linux / Windows 通过 `tun` crate）——
  真正打开 OS TUN（macOS `utun`、Linux `/dev/net/tun`、Windows `wintun`）。

把 trait 拆开的三个好处：

1. mesh 栈在 CI 里能完整跑通，不需要 root。
2. 整条数据通路可以无 flakiness 端到端测试。
3. Android / FreeBSD 等新平台可以独立插 trait，不动 workspace 其他 crate。

## 特性 (Features)

- `TunDevice::{recv, send, info, shutdown}` trait。
- `UserspaceTun::{new, bring_up, inject_from_kernel, drain_to_kernel}`。
- `UserspaceTunConfig { name, mtu, local_ipv4 }`。
- `parse_packet(bytes)` + `packet_to_bytes(...)` — IPv4/IPv6 头解析。
- `ParsedPacket { version, protocol, src, dst, payload }`。
- `IPV4_HEADER_MIN` / `IPV6_HEADER_MIN` 常量。
- feature `native` 启用 `NativeTun`。

## 安装 (Installation)

```rust
use a3net_tun::{
    TunDevice, UserspaceTun, UserspaceTunConfig,
    parse_packet, packet_to_bytes, ParsedPacket, IpVersion, IpProtocol,
};
```

## 使用 (Usage)

### 1. 创建 userspace TUN

```rust,no_run
use a3net_tun::{UserspaceTun, UserspaceTunConfig};
let cfg = UserspaceTunConfig::default();
let tun = UserspaceTun::new(cfg);
tun.bring_up();
```

### 2. 注入一个包并读回

```rust,no_run
# use a3net_tun::TunDevice;
# async fn demo(tun: a3net_tun::UserspaceTun) -> Result<(), Box<dyn std::Error>> {
let pkt = vec![0u8; 64];
tun.inject_from_kernel(pkt.clone()).await?;
if let Some(got) = tun.recv().await? {
    assert_eq!(got, pkt);
}
Ok(())
# }
```

### 3. 解析 IP 包头

```rust,no_run
use a3net_tun::{parse_packet, IpVersion};
let bytes = vec![0x45, 0, 0, 64, /* … */];
let parsed = parse_packet(&bytes)?;
assert_eq!(parsed.version, IpVersion::V4);
```

### 4. Send + drain

```rust,no_run
# use a3net_tun::TunDevice;
# async fn demo(tun: a3net_tun::UserspaceTun) -> Result<(), Box<dyn std::Error>> {
tun.send(vec![0u8; 32]).await?;
let drained = tun.drain_to_kernel().await?;
assert_eq!(drained, Some(vec![0u8; 32]));
Ok(())
# }
```

## 应用案例 (Use Cases / Examples)

- **`a3net-mesh-firewall`** + **`a3net-magicdns`** + **`a3net-mesh-coordinator`** + **`a3net-exit-node`** 共同在 `UserspaceTun` 之上跑 packet loop，端到端测试在 `tests/end_to_end.rs`。
- **`a3net-cli`** 的 `ray up` 子命令在 Linux 上用 `NativeTun` 真正打开 `/dev/net/tun`。
- **应用嵌入**：任何 Rust 应用都可以把 `UserspaceTun` 当成 "包沙盒" 用，写包进栈读包而不真打开网卡。

## 许可

MIT OR Apache-2.0