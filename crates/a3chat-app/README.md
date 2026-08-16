# `a3chat-app`

> 业务服务层。把 `a3chat-rpc`(RPC 注册)与 `a3chat-core`(领域类型)+ `a3chat-crypto`(E2E)+ `a3net-chatstore`(SQLite 落盘)连起来。
>
> **服务清单**:
>
> | 服务 | 职责 |
> |---|---|
> | `ChatService` | `chat.conversation.list` / `chat.conversation.open` / `chat.message.send` / `chat.message.recall` / `chat.message.ack` |
> | `ContactService` | `contact.list` / `contact.add_request` / `contact.accept_request` / `contact.block` / `contact.unblock` |
> | `GroupService` | `group.create` / `group.invite` / `group.member.add` / `group.member.remove` / `group.member.role` |
> | `SyncService` | `chat.sync.snapshot` / `chat.sync.delta` / `chat.sync.compressed` |
> | `PresenceService` | `presence.publish` / `presence.subscribe` |
> | `NotificationBus` | 跨服务广播 chat.message.received / presence.changed / group.member.* 给订阅者 |
> | `ChatStorage` | SQLite 落库 + E2E 加密字段自动 wrap |
>
> **依赖**: `a3chat-core` + `a3chat-crypto` + `a3net-chatstore` + `a3net-roster` + `a3net-userstore` + `a3net-blobstore`。**不依赖** `axum` / `tokio-tungstenite` 等传输层。
>
> **设计原则**: 每个 service 都是 `Arc<…>`-wrapped,共享 `ChatStorage` / `NotificationBus` / `E2eKeyring`。无内部锁 — 全部用 `tokio::sync::RwLock` 或 `DashMap`。

## 模块

| 模块 | 内容 |
|---|---|
| `error` | `AppError` (`a3chat_core::A3chatError` 扩展) |
| `storage` | `ChatStorage` — SQLite 落库 + 字段级 E2E wrap/unwrap |
| `notification_bus` | `NotificationBus` — broadcast::Sender 路由 SSE 推送 |
| `chat_service` | `ChatService` |
| `contact_service` | `ContactService` |
| `group_service` | `GroupService` |
| `sync_service` | `SyncService` |
| `presence_service` | `PresenceService` |
| `keyring` | `E2eKeyring` — per-peer Noise session + per-group Sender Key 缓存 |
| `app` | `A3chatApp` 总装 |

## License

MIT OR Apache-2.0