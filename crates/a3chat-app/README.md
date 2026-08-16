# `a3chat-app`

> 业务服务层。把 `a3chat-rpc`(RPC 注册)与 `a3chat-core`(领域类型)+ `a3chat-crypto`(E2E)+ `a3net-chatstore`(SQLite 落盘)连起来。
>
> **服务清单**:
>
> | 服务 | 职责 | RPC 命名空间 |
> |---|---|---|
> | `ChatService` | `chat.conversation.list` / `chat.conversation.open` / `chat.message.send` / `chat.message.recall` / `chat.message.ack` / `chat.message.edit` / `chat.message.delete` / `chat.search` / `chat.typing` | `a3chat.chat.*` |
> | `ContactService` | `contact.list` / `contact.add_request` / `contact.accept_request` / `contact.block` / `contact.unblock` / `contact.qr_invite` | `a3chat.contact.*` |
> | `GroupService` | `group.create` / `group.invite` / `group.join` / `group.member.add` / `group.member.remove` / `group.member.role` / `group.announcement.set` | `a3chat.group.*` |
> | `SyncService` | `chat.sync.snapshot` / `chat.sync.delta` / `chat.sync.compressed` | `a3chat.chat.sync.*` |
> | `PresenceService` | `presence.publish` / `presence.subscribe` | `a3chat.presence.*` |
> | `ProfileService` | `profile.get` / `profile.put` / `profile.preferences_put` / `profile.public_key_*` / `profile.device_*` / `profile.digit_get` / `profile.avatar_set` | `a3chat.profile.*` |
> | `MediaService` | `media.upload_init` / `media.upload_chunk` / `media.upload_finalize` / `media.download_get` | `a3chat.media.*` |
> | `ModerationService` | 内容审核(`check_content` / `check_attachment` / `block_hash` / `policy.stats` 等) | `a3chat.moderation.*` |
> | `PeerFeedbackService` | 信任等级 + 举报 + fused score | `a3chat.peerfeedback.*` |
> | `NotificationBus` | 跨服务广播 `chat.message.received` / `presence.changed` / `group.member.*` 给 SSE 订阅者 | 内部总线 |
> | `ChatStorage` | SQLite 落库 + E2E 加密字段自动 wrap | — |
> | `E2eKeyring` | per-peer Noise session + per-group Sender Key 缓存 | — |
>
> **依赖**: `a3chat-core` + `a3chat-crypto` + `a3net-chatstore` + `a3net-roster` + `a3net-userstore` + `a3net-blobstore` + `a3net-moderation` + `a3net-reputation`。**不依赖** `axum` / `tokio-tungstenite` 等传输层。
>
> **设计原则**:
> - 每个 service 都是 `Arc<…>`-wrapped,共享 `ChatStorage` / `NotificationBus` / `E2eKeyring`。
> - `ChatStorage` 对每个 user 维护一个 `tokio::sync::Mutex<Connection>`(`DashMap` 索引),首次访问时打开 + 跑 migration;后续访问复用同一连接 — 避免 `ensure_schema()`-per-op 模式在高并发下看到不一致 WAL 状态。
> - 多语句写都在 `unchecked_transaction` 中完成。
> - `AppError` 统一桥接到 `a3chat_core::error::A3chatError`,wire code 在 `a3chat-rpc` 翻译成 JSON-RPC 2.0 码。

## 模块

| 模块 | 内容 |
|---|---|
| `error` | `AppError` (`a3chat_core::A3chatError` 扩展) + `AppResult` |
| `storage` | `ChatStorage` — SQLite 落库 + 字段级 E2E wrap/unwrap。导出 `with_connection` helper 给后续 `H-4b` 迁移调用方 |
| `notification_bus` | `NotificationBus` — `broadcast::Sender` 路由 SSE 推送 |
| `chat_service` | `ChatService`(`with_moderation` 注入审核闸) |
| `contact_service` | `ContactService` |
| `group_service` | `GroupService` |
| `sync_service` | `SyncService` |
| `presence_service` | `PresenceService` |
| `profile_service` | `ProfileService`(`a3net-userstore` 桥) |
| `media_service` | `MediaService`(`a3net-blobstore` 桥) |
| `moderation_service` | `ModerationService`(`a3net-moderation` 桥) |
| `peer_feedback_service` | `PeerFeedbackService`(`a3net-reputation` 桥) |
| `keyring` | `E2eKeyring` — per-peer Noise session + per-group Sender Key 缓存 |
| `app` | `A3chatApp` 总装 + `dispatch` |

## 构造

```rust
use a3chat_app::{A3chatApp, StorageConfig};
use a3chat_core::id::UserId;

let app = A3chatApp::new(StorageConfig::default(), UserId::local())?;
app.init_user(&owner).await?;
```

`A3chatApp::new` 同时打开 `SQLite` 持久目录下的 `chat` / `user` / `media` / `moderation` 四个子目录,统一迁移与默认配置。

可选 builder:

```rust
app.with_reputation(reputation_reporter).await;  // 打开 PeerFeedbackService 的 reporter
```

`ChatService` 默认带 `moderation`。

## 测试

`cargo test -p a3chat-app --lib` — **177 个单元测试**,覆盖:

- `ChatStorage` E2E 包装、sequence 单调性、并发安全、迁移幂等
- `ChatService` 路由、send/ack/recall/edit/delete 路径
- `NotificationBus` publish/subscribe 语义
- `E2eKeyring` dm/group session 缓存
- `ProfileService` / `MediaService` / `ModerationService` / `PeerFeedbackService` 各自基础契约

## License

MIT OR Apache-2.0
