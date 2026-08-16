# a3net-mesh

> HTTP 网格传输：把 BlobStore 暴露成可被 HTTP range 请求的端点，并提供并行 chunk-aware fetcher。 / HTTP mesh fallback transport for A3Net blobs — serve a BlobStore over HTTP and fetch with parallel / range-aware clients.

## 概览 (Overview)

`a3net-mesh` 是 A3Net 在 *传输层不可用* 时的兜底。它把本地 `BlobStore` 暴露成
一个最小的 `tokio` HTTP 服务，路由如下：

```text
GET /health
GET /blobs/<hash>                完整 blob 或 200 + 全字节
GET /blobs/<hash>/meta           { hash, sizeBytes, chunkCount }
GET /blobs/<hash>/chunks/<index> 单个 16 KiB chunk
```

`fetch_from_mesh` 客户端会先 `GET /meta` 拿到 chunk 数，然后并行下载每个 chunk；
如果传入 `RangeSpec::Single` / `RangeSpec::Multi`，则使用 HTTP `Range:` 走单连接
按需下载。响应也支持 `multipart/byteranges` 以满足 `Multi` 范围请求。

传输层位于 `a3net-transport`（QUIC / iroh）之上，是 *首选* 路径；本 crate 只在
首选不可达或开发场景下被使用。

## 特性 (Features)

- `MeshServer::start(store)` — 一键起 HTTP server，bind 到 `0.0.0.0:0`，返回
  `MeshServerHandle { port, host, shutdown_tx }`。
- `fetch_from_mesh(store, hash, peers, dest, range)` — 并行 / range-aware 下载。
- `MeshFetchResult { bytes, peer }` — 成功后报告来自哪个 peer。
- `MeshConfig` — bind 地址占位（与 `MeshServer` 配合使用）。

## 安装 (Installation)

```rust
use a3net_mesh::{MeshServer, MeshConfig, fetch_from_mesh};
```

## 使用 (Usage)

### 1. 启动一个 mesh server

```rust,no_run
use std::sync::Arc;
use a3net_blobstore::BlobStore;
use a3net_mesh::MeshServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::Error>> {
    let store = Arc::new(BlobStore::new("/var/lib/a3net/blobs")?);
    let handle = MeshServer::start(store).await?;
    println!("mesh up at {}:{}", handle.host, handle.port);
    handle.shutdown();
    Ok(())
}
```

### 2. 抓整个 blob

```rust,no_run
use a3net_mesh::fetch_from_mesh;
use a3net_types::{ContentHash, RangeSpec};

let dest = std::path::Path::new("/tmp/blob.bin");
let result = fetch_from_mesh(
    &store, &hash, &["http://127.0.0.1:8080".into()],
    dest, RangeSpec::All,
).await?;
println!("got {} bytes from {}", result.bytes, result.peer);
```

### 3. Range fetch

```rust,no_run
use a3net_types::{ByteRange, RangeSpec};
let range = RangeSpec::Single(ByteRange::new(0, 1024)?);
let _ = fetch_from_mesh(&store, &hash, &peers, &dest, range).await?;
```

### 4. 并行 chunk 抓取（默认）

`fetch_from_mesh` 对 `RangeSpec::All` 自动并行 `GET /chunks/<i>`，无需额外配置。

## 应用案例 (Use Cases / Examples)

- **`a3net-node`** 在 `Transport` 拨号失败时退到 `fetch_from_mesh`，让 mesh 内任何
  节点都能帮忙 fetch 缺失 blob。
- **`a3net-cli`** 通过 `MeshServer` 启动一个开发节点，浏览器直接访问 `http://…:port/blobs/...`。
- **CI 测试**：单元测试用 mesh 替代真实 QUIC，端到端验证 `BlobStore` 序列化。

## 许可

MIT OR Apache-2.0