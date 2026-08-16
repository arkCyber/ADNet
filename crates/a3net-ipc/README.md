# a3net-ipc

> A3Net 的本地进程间 JSON-RPC 通道 — `JsonRpcServer` + `json_rpc_call`,在 Unix domain socket 上跑标准 JSON-RPC 2.0。

## 概览 (Overview)

`a3net-ipc` 把 A3Net 节点暴露为可在同一台机器上跨进程调用的 JSON-RPC 服务:

- **服务端** — `JsonRpcServer::start(sock, handler)` 在 Unix socket 上监听,把所有请求分发到用户实现的 `RpcHandler` trait;同时支持 server-initiated `Notification`(推送)。
- **客户端** — `json_rpc_call` / `json_rpc_stream` 通过任意 Unix socket 发请求并等待响应。
- **校验层** — `validation::{Validate, ValidationPolicy, ValidationOutcome}` 让 handler 在请求进入业务逻辑前先过一次静态校验(消息长度、字段边界、嵌套深度)。
- **类型化封装** — 在裸 JSON-RPC 之上,`blobs_service` / `gossip_service` / `group_chat_service` 提供了直接面向 A3Net 业务的服务包装。

## 特性 (Features)

- **`RpcHandler` trait** — `async fn handle(&self, method, params) -> Result<Value, RpcHandlerError>`,用 `#[async_trait]` 暴露。
- **`JsonRpcServer::start()`** — 内部 `tokio` 任务,返回 `JsonRpcServerHandle` 持有 `shutdown()` 句柄。
- **`Notification` + `NotificationSender`** — server-initiated push,客户端不用订阅就能收到异步通知。
- **`json_rpc_call`** — 一次性请求-响应,适合 RPC 调用。
- **`json_rpc_stream`** — 长连接流式响应,适合订阅 / 推送 / 大列表分页。
- **`Validate` + `ValidationPolicy`** — 在 handler 内调一次 `request.validate(&policy)?` 即可拒绝畸形负载。
- **类型化 IPC service** — `BlobsIpcService` / `GossipIpcService` / `GroupChatIpcService` 直接面向 `a3net-types` 暴露业务方法,无需手写 RPC 方法名。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
a3net-ipc = { workspace = true }
```

## 使用 (Usage)

### 1. 服务端:实现 `RpcHandler`,起一个 `JsonRpcServer`

```rust
use std::path::PathBuf;
use std::sync::Arc;

use a3net_ipc::{JsonRpcServer, RpcHandler};
use async_trait::async_trait;
use serde_json::{Value, json};

struct Adder;
#[async_trait]
impl RpcHandler for Adder {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, a3net_ipc::RpcHandlerError> {
        match method {
            "add" => {
                let a = params["a"].as_i64().unwrap_or(0);
                let b = params["b"].as_i64().unwrap_or(0);
                Ok(json!({ "sum": a + b }))
            }
            "echo" => Ok(params),
            other => Err(a3net_ipc::RpcHandlerError::method_not_found(other)),
        }
    }
}

#[tokio::main]
async fn main() {
    let sock = PathBuf::from("/tmp/a3net-adder.sock");
    let handle = JsonRpcServer::start(sock, Arc::new(Adder)).await.unwrap();
    // ... 客户端发请求 ...
    handle.shutdown();
}
```

### 2. 客户端:`json_rpc_call`

```rust
use a3net_ipc::{json_rpc_call, JsonRpcError};
use serde_json::json;

async fn client(sock: &std::path::Path) -> Result<(), JsonRpcError> {
    let resp = json_rpc_call(sock, "add", json!({ "a": 1, "b": 2 })).await?;
    assert_eq!(resp["sum"], 3);
    Ok(())
}
```

### 3. 服务端主动推送 `Notification`

```rust
use a3net_ipc::{JsonRpcServer, Notification};
use serde_json::json;

let handle = JsonRpcServer::start(sock, handler).await?;
let note = Notification::new("tick", json!({ "t": 1 }));
handle.notify(note).await;   // 推给所有已连接客户端
```

### 4. 校验

```rust
use a3net_ipc::{Validate, ValidationPolicy, ValidationOutcome};

let policy = ValidationPolicy::default();   // 默认限制字段长度与嵌套深度
match request.validate(&policy) {
    ValidationOutcome::Ok => { /* 进入业务 */ }
    ValidationOutcome::Rejected(why) => { /* 直接返回 RpcHandlerError::BadRequest */ }
}
```

## 应用案例 (Use Cases / Examples)

- **CLI ↔ Node IPC** — `a3net-cli` 通过 `json_rpc_call` 与 `a3net-node` 守护进程通信,调用 RPC 方法名以 `/api/v0` 开头,与 HTTP gateway 完全对齐。
- **TUI 前端** — `a3net-tui` 在用户切换标签页时订阅 `Notification`,实时显示 gossip / bitswap 状态。
- **CI / 集成测试** — `a3net-integration-tests` 在同一进程里启动一个 `JsonRpcServer` 再用客户端连过去,跑端到端场景。
- **多语言客户端** — JSON-RPC 协议天然跨语言,Python / Go 客户端只需打开 Unix socket 即可使用 A3Net 节点。
- **本地守护拆分** — 节点内部把"gossip 维护"和"blob 下载"拆成两个进程,通过 `GossipIpcService` / `BlobsIpcService` 通信。

## 许可

MIT OR Apache-2.0