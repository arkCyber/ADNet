# `a3chat-core`

> a3chat 业务领域类型 + JSON Schema 导出 + 零依赖 `RpcClient` trait。
>
> **跨端共享**:Rust server / Tauri desktop / Flutter mobile 三端共用这一份类型契约。
>
> **不持有加密逻辑**:加密在 `a3chat-crypto`,本 crate 只定义"密文是什么样子"。

## 模块

| 模块 | 内容 |
|---|---|
| `error` | `A3chatError` 统一错误类型 |
| `id` | `UserId` / `DeviceId` / `ConversationId` / `MessageId` / `NodeId` 类型别名 + 生成器 |
| `conversation` | `ConversationRecord` / `ConversationKind` (Dm / Group) / `ConversationMeta` |
| `message` | `ChatMessage` / `MessageType` (Text / Image / File / Voice / Video / System) / `MessageBody` (Plain / Encrypted) / `Attachment` |
| `contact` | `Contact` / `ContactRequest` / `ContactRequestStatus` / `BlocklistEntry` |
| `group` | `Group` / `GroupMember` / `MemberRole` (Owner / Admin / Member) / `GroupInvitation` |
| `presence` | `Presence` (Online / Away / Offline / Invisible) / `PresenceEvent` |
| `event` | `A3chatEvent` 服务端推送事件枚举 + `A3chatNotification` 通知包装 |
| `schema` | 输出 JSON Schema 文档,供前端 quicktype 生成 TS/Dart 类 |
| `rpc` | `RpcClient` trait(异步 HTTP 调用抽象) + `A3chatRpc` 方法常量 |
| `validation` | 通用字段校验函数 |

## 设计原则

1. **serde 字段名一律 `snake_case`**——前端通过 quicktype 生成的 TS/Dart 类也是 `snake_case`(配合 JSON 习惯)。
2. **每个类型实现 `Validate` trait**——边界校验统一。
3. **`chrono::DateTime<Utc>` 时间戳**——所有时间字段统一 RFC3339。
4. **`NodeId` 复用 `a3net_types::NodeId`**——P2P 路由直接对接 A3Net 传输层。
5. **密文是 opaque**:`MessageBody::Encrypted` 携带 `algorithm` + `nonce` + `ciphertext` + `tag`,本 crate 不解析。

## License

MIT OR Apache-2.0
