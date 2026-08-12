# adnet-rpc

> 与 IPFS HTTP API 兼容的统一 RPC 命令集合 — `dag/put` / `block/get` / `pin/add` / `name/publish` / `dht/findprovs` 等,既可作 HTTP API,也可作本地 RPC。

## 概览 (Overview)

`adnet-rpc` 提供与 IPFS HTTP API 兼容的命令面(command surface),让 ADNet 节点对外部世界呈现"长得像 IPFS 节点"的行为:

- **`commands`** — 一组与 IPFS 同名的命令函数:`dag_put(&store, payload, pin)` / `block_get(&store, &cid)` / `pin_add(&store, &cid)` / `name_publish(...)` / `dht_findprovs(...)` 等。
- **`results`** — `RpcResult<T>` / `RpcError` 类型,所有命令统一返回。
- **`client::RpcClient`** — 一个 `Arc<BlobStore>` 包装的便捷客户端,提供 `put_block` / `get_block` / `block_stat` / `node_id` 等方法,适合测试与嵌入式调用。
- **HTTP 兼容** — `adnet-gateway` 直接把命令函数挂到 `/api/v0/<command>` 路径上,符合 IPFS 客户端的预期。

## 特性 (Features)

### 命令分类

| Category | Command                          |
|----------|----------------------------------|
| DAG      | `dag/put`, `dag/get`, `dag/resolve`, `dag/import` |
| Block    | `block/put`, `block/get`, `block/stat`, `block/rm` |
| Pin      | `pin/add`, `pin/rm`, `pin/ls`   |
| GC       | `gc`                            |
| DHT      | `dht/findprovs`, `dht/provide`  |
| IPNS     | `name/publish`, `name/resolve`  |
| 节点     | `version`, `node/id`            |

### 关键类型

- **`RpcClient`** — 高层封装,直接以方法名(`put_block` / `get_block`)调用,内部走 `commands::*` 函数。
- **`RpcResult<T>`** — 包含 `Value` 数据载荷(命令特定的 JSON)+ `error` 错误载荷,与 IPFS HTTP API 保持一致。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-rpc = { workspace = true }
adnet-blobstore = { workspace = true }   # 命令实现依赖
```

## 使用 (Usage)

### 1. 直接调用命令函数

```rust
use std::sync::Arc;
use adnet_blobstore::BlobStore;
use adnet_rpc::{block_put, block_get, version};

let store = Arc::new(BlobStore::new(std::path::Path::new("/tmp/blobs"))?);

let v = version().await?;
println!("version: {v:?}");

let cid = block_put(&store, b"hello".to_vec()).await?;
let back = block_get(&store, &cid).await?;
assert_eq!(back, b"hello");
```

### 2. 使用便捷 `RpcClient`

```rust
use std::sync::Arc;
use adnet_blobstore::BlobStore;
use adnet_rpc::client::RpcClient;

let store = Arc::new(BlobStore::new(std::path::Path::new(""))?);
let client = RpcClient::new(store);

let cid = client.put_block(b"abc").await?;
let bytes = client.get_block(&cid).await?;
let stat = client.block_stat(&cid).await?;
println!("stat: size={}", stat.size);
```

### 3. 模拟 gateway HTTP 路径

```rust
use adnet_rpc::commands::{dag_put, dag_get, block_put, block_rm};

let put = dag_put(&store, payload, /*pin=*/ false).await?;
// put.cid, put.size
let got = dag_get(&store, &put.cid, None).await?;
// got.data — base64 字符串

let blk_cid = block_put(&store, payload).await?;
let rm = block_rm(&store, &blk_cid, /*force=*/ false).await?;
// rm.removed == true
```

## 应用案例 (Use Cases / Examples)

- **`adnet-gateway`** — 把所有命令函数挂到 `/api/v0/<command>` HTTP handler 上;现有 IPFS SDK(`kubo-client`、IPFS Companion)可直接对 ADNet 节点进行读写。
- **CI 测试** — 集成测试使用 `RpcClient` 在内存 `BlobStore` 上跑端到端场景,无需启动 HTTP server。
- **CLI** — `adnet-cli` 的 `put` / `get` / `pin` 子命令都通过 RPC 调用,逻辑与 gateway 一致,避免两套实现漂移。
- **多语言 SDK** — 因为命令集与 IPFS HTTP API 兼容,任何已存在的 IPFS 客户端 SDK(JS / Python / Go)都可直接对接 ADNet 节点。
- **DHT 索引** — `dht/findprovs` 与 `name/resolve` 让外部世界像查询普通 IPFS 节点一样查询 ADNet 节点。

## 许可

MIT OR Apache-2.0