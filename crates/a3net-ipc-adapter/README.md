# a3net-ipc-adapter

> 把 [`a3net-node::Node`](../../a3net-node) 通过 JSON-RPC over Unix socket 暴露给外部进程 — TUI、Tauri、web 前端、脚本都不需要链接进 A3Net 运行时。

## 概览 (Overview)

`a3net-ipc-adapter` 是 A3Net 守护进程模式的入口 crate:

- **`NodeRpc`** — 实现 `a3net_ipc::RpcHandler`,把每个 JSON-RPC 方法映射到 `a3net-node::Node` 的一个公开操作(详见下表)。
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

- `announcement` — params 是序列化后的 `a3net_types::Announcement` 加 `room_id`。由 `subscribe_room` 观察到的所有 **远端** 发布触发(本地发布不重发,因为客户端已经知道)。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
a3net-ipc-adapter = { workspace = true }
a3net-node = { workspace = true }
```

## 使用 (Usage)

### 1. 启动守护进程

```rust
use std::path::PathBuf;
use a3net_ipc_adapter::start_daemon;
use a3net_node::Node;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let node = Node::start_default().await?;
    let handle = start_daemon(PathBuf::from("/tmp/a3net.sock"), node).await?;
    // 守护进程运行中,直到 handle 被 drop
    handle.shutdown().await;
    Ok(())
}
```

### 2. 自定义 handler

```rust
use a3net_ipc::JsonRpcServer;
use a3net_ipc_adapter::{NodeRpc, ANNOUNCEMENT_METHOD};
use std::sync::Arc;

let node = Node::start_default().await?;
let handler = Arc::new(NodeRpc::new(node));
let handle = JsonRpcServer::start(PathBuf::from("/tmp/a3net.sock"), handler.clone()).await?;
// 如果你需要自定义推送,handler.serve_with_notifier(handle.notifier()) 暴露入口
```

### 3. 客户端订阅 announcement

任何 `a3net-ipc` 客户端连上之后,可以订阅 `announcement` notification:

```rust
use a3net_ipc::json_rpc_stream;
use serde_json::json;

let mut stream = json_rpc_stream(&sock, "announcement", json!({})).await?;
while let Some(item) = stream.next().await {
    println!("announcement: {item:?}");
}
```

## 应用案例 (Use Cases / Examples)

- **`a3net-cli`** — CLI 直接通过 IPC 调用 `NodeRpc::list_rooms` / `make_ticket` / `announce`,与 HTTP gateway 完全对齐。
- **`a3net-tui`** — TUI 在前台开一个 JSON-RPC 长连接,后台订阅 `announcement` notification,实现实时 feed。
- **Tauri / Web 前端** — 前端通过 Electron 风格的 IPC bridge 调用 Unix socket 上的 JSON-RPC,无需在浏览器里运行 WASM 节点。
- **CI 集成测试** — 集成测试 `start_daemon` 一个临时 socket,然后用 Python / Go / curl-style 客户端驱动节点。
- **本地多进程拆分** — 把 `bitswap` 守护与 `gossip` 守护拆成两个进程,通过 `NodeRpc` IPC 互通,比单一进程更易调试。

## 许可

MIT OR Apache-2.0