# adnet-ipc-adapter

> 把 [`adnet-node::Node`](../../adnet-node) 通过 JSON-RPC over Unix socket 暴露给外部进程 — TUI、Tauri、web 前端、脚本都不需要链接进 ADNet 运行时。

## 概览 (Overview)

`adnet-ipc-adapter` 是 ADNet 守护进程模式的入口 crate:

- **`NodeRpc`** — 实现 `adnet_ipc::RpcHandler`,把每个 JSON-RPC 方法映射到 `adnet-node::Node` 的一个公开操作(详见下表)。
- **`start_daemon(socket_path, node)`** — 一行代码把 `Node` 包装成 Unix socket 上的 JSON-RPC 服务,并启动一个 `NotificationForwarder`,把 `subscribe_room` 接收到的所有远端 `Announcement` 转成 server-pushed notification 推给客户端。
- **方法名稳定** — `NodeRpc::METHODS` 是字符串常量数组,客户端可硬编码使用,改动受 API 稳定性约束。

## 特性 (Features)

### 方法参考(`NodeRpc`)

| method         | params                            | returns                     |
|----------------|-----------------------------------|-----------------------------|
| `init`         | `{}`                              | `NodeInfo` snapshot         |
| `info`         | `{}`                              | `NodeInfo` snapshot         |
| `list_rooms`   | `{}`                              | `[string]` room ids         |
| `join`         | `{room: string}`                  | `{}`                        |
| `leave`        | `{room: string}`                  | `{}`                        |
| `feed`         | `{room: string}`                  | `RoomFeed`                  |
| `announce`     | `{room, file, title?, kind?}`     | `{hash, ticket, sizeBytes}` |
| `peers_for`    | `{hash: string}`                  | `[string]` ticket strings   |
| `make_ticket`  | `{hash: string}`                  | `string` ticket             |

### Notification 参考(`start_daemon` 启动时附带)

- `announcement` — params 是序列化后的 `adnet_types::Announcement` 加 `room_id`。由 `subscribe_room` 观察到的所有 **远端** 发布触发(本地发布不重发,因为客户端已经知道)。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-ipc-adapter = { workspace = true }
adnet-node = { workspace = true }
```

## 使用 (Usage)

### 1. 启动守护进程

```rust
use std::path::PathBuf;
use adnet_ipc_adapter::start_daemon;
use adnet_node::Node;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let node = Node::start_default().await?;
    let handle = start_daemon(PathBuf::from("/tmp/adnet.sock"), node).await?;
    // 守护进程运行中,直到 handle 被 drop
    handle.shutdown().await;
    Ok(())
}
```

### 2. 自定义 handler

```rust
use adnet_ipc::JsonRpcServer;
use adnet_ipc_adapter::{NodeRpc, ANNOUNCEMENT_METHOD};
use std::sync::Arc;

let node = Node::start_default().await?;
let handler = Arc::new(NodeRpc::new(node));
let handle = JsonRpcServer::start(PathBuf::from("/tmp/adnet.sock"), handler.clone()).await?;
// 如果你需要自定义推送,handler.serve_with_notifier(handle.notifier()) 暴露入口
```

### 3. 客户端订阅 announcement

任何 `adnet-ipc` 客户端连上之后,可以订阅 `announcement` notification:

```rust
use adnet_ipc::json_rpc_stream;
use serde_json::json;

let mut stream = json_rpc_stream(&sock, "announcement", json!({})).await?;
while let Some(item) = stream.next().await {
    println!("announcement: {item:?}");
}
```

## 应用案例 (Use Cases / Examples)

- **`adnet-cli`** — CLI 直接通过 IPC 调用 `NodeRpc::list_rooms` / `make_ticket` / `announce`,与 HTTP gateway 完全对齐。
- **`adnet-tui`** — TUI 在前台开一个 JSON-RPC 长连接,后台订阅 `announcement` notification,实现实时 feed。
- **Tauri / Web 前端** — 前端通过 Electron 风格的 IPC bridge 调用 Unix socket 上的 JSON-RPC,无需在浏览器里运行 WASM 节点。
- **CI 集成测试** — 集成测试 `start_daemon` 一个临时 socket,然后用 Python / Go / curl-style 客户端驱动节点。
- **本地多进程拆分** — 把 `bitswap` 守护与 `gossip` 守护拆成两个进程,通过 `NodeRpc` IPC 互通,比单一进程更易调试。

## 许可

MIT OR Apache-2.0