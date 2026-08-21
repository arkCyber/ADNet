# `a3chat-core`

> a3chat 业务领域类型 + JSON Schema 导出 + 零依赖 `RpcClient` trait。
>
> **跨端共享**:Rust server / Tauri desktop / Flutter mobile 三端共用这一份类型契约。
>
> **不持有加密逻辑**:加密在 `a3chat-crypto`,本 crate 只定义"密文是什么样子"。

## 模块

| 模块 | 内容 |
|---|---|
| `error` | `A3chatError` 统一错误类型(永久 / 瞬时 / 内部 / 用法 4 类 + 错误码常量) |
| `id` | `UserId` / `DeviceId` / `ConversationId` / `MessageId` / `NodeId` 类型别名 + 生成器 |
| `conversation` | `ConversationRecord` / `ConversationKind` (Dm / Group) / `ConversationMeta` |
| `message` | `ChatMessage` / `MessageType` (Text / Image / File / Voice / Video / System) / `MessageBody` (Plain / Encrypted) / `Attachment` / `MessageEnvelope` |
| `contact` | `Contact` / `ContactRequest` / `ContactRequestStatus` / `BlocklistEntry` |
| `group` | `Group` / `GroupMember` / `MemberRole` (Owner / Admin / Member) / `GroupInvitation` |
| `presence` | `Presence` (Online / Away / Offline / Invisible) / `PresenceEvent` |
| `event` | `A3chatEvent` 服务端推送事件枚举 + `A3chatNotification` 通知包装 |
| `schema` | 输出 JSON Schema 文档,供前端 quicktype 生成 TS/Dart 类 |
| `rpc` | `RpcClient` trait(异步 HTTP 调用抽象) + `A3chatRpcMethod` 方法常量(39 条) |
| `validation` | 通用字段校验函数 + `MAX_*` 常量边界 |

## 设计原则

1. **serde 字段名一律 `snake_case`**——前端通过 quicktype 生成的 TS/Dart 类也是 `snake_case`(配合 JSON 习惯)。
2. **每个类型实现 `Validate` trait**——边界校验统一。
3. **`chrono::DateTime<Utc>` 时间戳**——所有时间字段统一 RFC3339。
4. **`NodeId` 复用 `a3net_types::NodeId`**——P2P 路由直接对接 A3Net 传输层。
5. **密文是 opaque**:`MessageBody::Encrypted` 携带 `algorithm` + `nonce` + `ciphertext` + `tag`,本 crate 不解析。

## RPC 方法常量

`A3chatRpcMethod` 暴露 39 个稳定的 JSON-RPC 字符串(覆盖 `chat.*` / `contact.*` / `group.*` / `chat.sync.*` / `presence.*` / `profile.*` / `media.*` / `e2e.*` / `stream.*`),并以 `A3chatRpcMethod::ALL` 静态数组对外暴露,供客户端 / CI 做契约测试。组分类:

| 命名空间 | 数量 | 备注 |
|---|---|---|
| `a3chat.chat.*` | 9 | conversation + message + typing + search |
| `a3chat.contact.*` | 6 | 联系人 + 邀请 |
| `a3chat.group.*` | 7 | 群组管理 |
| `a3chat.chat.sync.*` | 3 | 多设备同步 |
| `a3chat.presence.*` | 2 | 在 / 离线 |
| `a3chat.profile.*` | 10 | 桥接 `a3net-userstore` |
| `a3chat.media.*` | 4 | 桥接 `a3net-blobstore` |
| `a3chat.e2e.*` | 2 | 密钥 bundle 导入 / 导出 |
| `a3chat.stream.*` | 1 | SSE 订阅 |

> 通知事件(`a3chat.chat.message.received` 等)名以 `NOTIFICATION_*` 常量另列,与 RPC 方法隔离。

## License

MIT OR Apache-2.0
