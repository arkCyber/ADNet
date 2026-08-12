# adnet-transport

> 抽象传输层：把上层（`adnet-node`）与底层 QUIC / iroh 解耦，提供统一的连接与成帧 API。 / Abstract transport layer — uniform connection / framing API decoupling `adnet-node` from QUIC / iroh.

## 概览 (Overview)

`adnet-transport` 是 ADNet P2P 节点之间的"线"。它对外暴露一组最小但完整的 trait：

- `Transport`：拨号、接受、发送 `Frame`。
- `Frame` + `FrameCodec`：长度前缀成帧协议，所有后端共享。
- `OutgoingConnection` / `ConnectionType` / `StreamPriority`：连接类型与优先级。

后端实现：

- `QuicTransport`（默认，基于 `quinn`）：用 quinn + rustls 建立的本地 QUIC 端点，证书哈希即为
  `NodeId`，不需要 PKI。
- `IrohTransport`（feature `iroh`）：占位但可启用，复用 `iroh-net` 自带 NAT 穿透 + DERP 中继
  + iroh-gossip + iroh-blobs 全套生态。
- 可选 feature `mdns` 加上 `iroh-mdns-address-lookup`，让同一 LAN 的节点无需 DHT/relay 也能发现。

`adnet-mesh` 提供的 HTTP fallback 永远可用，传输层离线时上层会无缝退化。

## 特性 (Features)

- `Transport` trait — `dial`、`accept`、`local_node_id` 等。
- `QuicTransportBuilder` — 构造 `QuicTransport`，可指定本地 `NodeId` 与 bind 地址。
- `Frame::text` / `Frame::binary` / `Frame::json` — 构造 wire 帧。
- `EndpointAddr` — 节点地址抽象。
- `ConnectionType::{Direct, Relay, Loopback, Mixed, Closed}` — 拨号路径分类。

## 安装 (Installation)

```rust
use adnet_transport::{
    Frame, QuicTransportBuilder, Transport, TransportIdentity, derive_node_id_from_cert,
};
```

## 使用 (Usage)

### 1. 构造一个 QUIC 端点并拨号

```rust,no_run
use std::net::SocketAddr;
use adnet_transport::{QuicTransportBuilder, Transport};

let server = QuicTransportBuilder::new(
    adnet_types::NodeId::random(),
    "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
)
.build()
.expect("server build");

let client = QuicTransportBuilder::new(
    adnet_types::NodeId::random(),
    "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
)
.build()
.expect("client build");

let target = adnet_types::NodeAddr::new(server.local_node_id().clone())
    .with_direct(adnet_types::Endpoint::new("127.0.0.1", 9000));
let conn = client.dial_addr(target).await.expect("dial");
```

### 2. 收发 `Frame`

```rust,no_run
use adnet_transport::Frame;

let greeting = Frame::text("hello");
let reply   = Frame::json(&serde_json::json!({ "ok": true }))?;
```

### 3. 派生 NodeId from 自签证书

```rust,no_run
use adnet_transport::derive_node_id_from_cert;

let pem = std::fs::read("cert.pem")?;
let node_id = derive_node_id_from_cert(&pem);
```

### 4. 接受连接

```rust,no_run
use adnet_transport::Transport;

async fn accept_loop<T: Transport>(t: &T) -> anyhow::Result<()> {
    while let Some((peer, mut conn)) = t.accept().await? {
        if let Some(frame) = conn.recv().await? {
            println!("from {}: {:?}", peer.short(), frame);
        }
    }
    Ok(())
}
```

## 应用案例 (Use Cases / Examples)

- **`adnet-node`** 在 `NodeBuilder::with_transport` 处接收任何实现 `Transport` 的实例，
  生产环境是 `QuicTransport`，开发环境可以换成 in-memory loopback。
- **`adnet-blobstore`** 通过 `fetch_blob_over_transport` / `serve_blob_request` 利用
  `Frame` 把 blob 切成 chunk 流过同一条传输链路。
- **`adnet-dht`** 在 `DhtTransportAdapter` / `TransportBridge` 处复用 `Transport` 的连接
  抽象承载 DHT 协议消息，无需再写一遍 socket 逻辑。

## 许可

MIT OR Apache-2.0