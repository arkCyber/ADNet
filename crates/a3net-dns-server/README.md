# a3net-dns-server

> 自建权威 DNS 服务器，承载 pkarr 兼容的 IPNS zone。 / Self-hostable authoritative DNS server — pkarr-compatible zone for A3Net.

## 概览 (Overview)

`a3net-dns-server` 把"自建 pkarr zone"这件事做成了一个最小 DNS 服务器。它对外提供两类记录：

- `_a3net.<ipns-name>.<zone>` —— TXT 记录，承载 base64 编码的 pkarr 包（即该 IPNS 名
  在公共 pkarr relay 上原本应该发的内容）。
- `<relay-name>.<zone>` —— A / AAAA 记录，公布本机运行的 A3Net relay 入口。

这让"自建 zone"成为可能：

- 公有 pkarr relay 是 *联邦的*（一条记录会在所有 relay 上可见）。大多数运营商只想
  跑自己的 `*.a3net.example` 区。
- 该 crate 是 iroh-dns-server 的对位实现：相同的 wire 格式，操作者可以无缝切换。

DNS 协议部分用 tokio 手写（hickory 太大）。只覆盖一个最小子集：
TXT（`TXT` query）、A、AAAA；UDP 与 TCP 都支持。

## 特性 (Features)

- `DnsServerConfig::with_zone(...).with_bind(...)` — fluent 配置。
- `serve(cfg)` — 启动 UDP/TCP DNS 服务，返回 `DnsServerHandle`。
- `ZoneStore::put(ZoneRecord)` / `get(key)` / `all()` — 区域记录的 CRUD。
- `HttpApi::publish / fetch / list` — HTTP 管理 API（同进程的 `PUT /zones/.../ipns/<n>`）。
- `RecordKind::{AdnetIpnsTxt, RelayAddr}`。

## 安装 (Installation)

```rust
use a3net_dns_server::{DnsServerConfig, serve, DnsServerHandle};
use a3net_dns_server::zone::{ZoneRecord, RecordKind};
```

## 使用 (Usage)

### 1. 启动一个最小 zone

```rust,no_run
use std::net::SocketAddr;
use a3net_dns_server::{DnsServerConfig, serve};

let cfg = DnsServerConfig::default()
    .with_zone("a3net.example")
    .with_bind("127.0.0.1:5353".parse::<SocketAddr>().unwrap());
let _handle = serve(cfg).await?;
```

### 2. 手工塞一条记录

```rust,no_run
use a3net_dns_server::zone::{ZoneRecord, RecordKind};
let store = a3net_dns_server::zone::open(
    a3net_dns_server::DnsServerConfig::default(),
)?;
let rec = ZoneRecord {
    key: store.ipns_txt_key("aliceipnsname"),
    kind: RecordKind::AdnetIpnsTxt {
        ipns_name: "aliceipnsname".into(),
        payload: "BASE64PKARR".into(),
        ttl_secs: 3600,
    },
};
store.put(rec)?;
```

### 3. HTTP admin API

```rust,no_run
use a3net_dns_server::http::{HttpApi, PublishBody};

let api = HttpApi::from_config(a3net_dns_server::DnsServerConfig::default())?;
let body = PublishBody { payload: "BASE64".into(), ttl_secs: Some(3600) };
let rec = api.publish("alice", body)?;
println!("stored {}", rec.key);
```

### 4. 列出所有记录

```rust,no_run
use a3net_dns_server::http::HttpApi;
let api = HttpApi::from_config(a3net_dns_server::DnsServerConfig::default())?;
for rec in api.list() {
    println!("{:?} -> {:?}", rec.key, rec.kind);
}
```

## 应用案例 (Use Cases / Examples)

- **运营自建** —— 把 DNS 服务暴露到公网 (`0.0.0.0:53`)，让团队成员走
  `dig _a3net.<ipns>.a3net.example TXT` 而不是 `pkarr.pub`。
- **`a3net-cli`** 的 `a3net-dns-server` 二进制直接调用 `serve()` + `HttpApi`。
- **混合部署** —— 多个 zone 之间可以转发，本 crate 与 iroh-dns-server 共享数据格式。

## 许可

MIT OR Apache-2.0