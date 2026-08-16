# `a3net-chatstore`

> A3Net 聊天数据持久层 —— SQLite(WAL)+ 分用户命名空间 + 序列号 + 已读回执 + zstd 同步压缩,提供本地存储(`ChatStorage`)与中心服务器存储(`ImManager`)两层。
>
> A3Net chat persistence — SQLite (WAL) with per-user namespacing, sequence tracking, read receipts, and zstd-compressed sync. Provides both the per-user store (`ChatStorage`) and the hub-server store (`ImManager`).

## 概览(Overview)

`a3net-chatstore` 是 A3Net 中负责聊天记录本地持久化的核心 crate。它把两个历史实现统一到同一个 crate 内:

1. `Exodus/src-backup/src-tauri/src/microservice/chat_storage.rs` —— **per-user** 视角的本地存储:每个 `user_id` 各自维护好友表、点对点消息、群消息、序列号、已读回执。
2. `Exodus/src-backup/exodus-hub-server/src/manager.rs` —— **hub-server** 视角的中心化存储:用户、对话、群成员、发件方序号、待发消息、zstd+bincode 压缩的批量同步。

设计上保持 SQLite 单库 + WAL 模式以允许 IPC 与 gossip 写入并发;所有字段都是 `a3net_types::group_chat` 的强类型记录,绝不出现 `String` / `i64` 直写的"stringly-typed"持久化层;同步层走 zstd+bincode 让一次拉取能压缩数千条历史。Public API 全部返回 `Result<_, ChatStoreError>`,`std::sync::Mutex` 中毒归一化为 `ChatStoreError::Lock`,不需要给上层暴露 `PoisonError` 类型。

可选的 `iroh` feature 把"消息表 + 自定义 `SyncRequest`/`SyncResponse` 协议"替换成 `iroh_docs::Doc` per-conversation,friendships / group membership / user accounts / pending messages / read receipts 仍然走 SQLite。

## 特性(Features)

- **双层存储 API**:`ChatStorage`(per-user,同步)+ `ImManager`(hub-server,异步)并存,各自适配不同业务面。
- **`journal_mode=WAL`** 默认开,IPC 读与 gossip 写并发零冲突。
- **`user_id` 分区**:同一进程可承载多用户,每个 `user_id` 拥有独立的 friend list / DM 索引 / 群消息副本。
- **完整性哈希**:每条消息写入前 `stamp_integrity_hash()` 重算 `SHA-256(sender|receiver|content|sequence|timestamp)`,读取后可 `verify_integrity_hash()` 校验。
- **`SyncRequest`/`SyncResponse` + zstd**:批量同步整段对话历史到新设备时压缩比通常 > 70%。
- **`MessageReceipt`**:每条消息可以独立确认已读,`save_receipt` / `get_message_receipts` 用于未读清零。
- **`SchemaVersion` 校验**:`schema_version()` 暴露当前数据库 schema 版本,`check_integrity()` 跑基础健康检查。
- **`iroh` feature**:`IrohDocsChat` 把消息表换成 `iroh-docs::Doc`,提供 `ConversationTicket` / `MessageEvent` 流式订阅。
- **零 unsafe**(`#![forbid(unsafe_code)]`),统一错误类型 `ChatStoreError`。

## 安装(Installation)

工作空间内 path 依赖:

```toml
# 你的 crate 的 Cargo.toml
a3net-chatstore = { workspace = true }
```

```rust
use a3net_chatstore::{
    ChatStorage, ChatStorageConfig, Friend, MessageAttachment,
    ImManager, ChatType,
};
use a3net_types::group_chat::{DirectMessage, GroupMessage, MessageReceipt};
use a3net_types::invariants::{MessageType, AttachmentKind};
```

## 使用(Usage)

### 1. 打开 per-user 存储并保存一条 1:1 消息

```rust
use a3net_chatstore::{ChatStorage, ChatStorageConfig, Friend};
use a3net_chatstore::error::ChatStoreError;
use a3net_types::group_chat::{DirectMessage, MessageAttachment};
use a3net_types::invariants::{AttachmentKind, MessageType};

let dir = tempfile::tempdir()?;
let storage = ChatStorage::new(ChatStorageConfig { storage_dir: dir.path().to_path_buf() })?;
storage.save_friend("alice", Friend {
    friend_id: "bob".into(), name: "Bob".into(),
    avatar_url: None, status: None, last_seen: None,
    created_at: None, updated_at: None,
})?;

let mut msg = DirectMessage {
    message_id: "m1".into(),
    chat_id: "dm:alice:bob".into(),
    sender_id: "alice".into(), receiver_id: "bob".into(),
    content: "hi".into(),
    message_type: MessageType::Text,
    attachments: vec![],
    reply_to: None, sequence: 1, timestamp: 1_700_000_000,
    integrity_hash: None, is_edited: false, edited_at: None,
};
msg.stamp_integrity_hash();
storage.save_direct_message("alice", msg)?;
```

### 2. 用 hub-server 模式做对话 + 压缩同步

```rust
use a3net_chatstore::{ChatType, ImManager};

let mgr = ImManager::new(dir.path().join("hub.db"))?;
let alice = mgr.create_user("alice", "Alice").await?;
let bob = mgr.create_user("bob", "Bob").await?;
let conv = mgr.create_conversation(ChatType::OneOnOne, "alice<->bob").await?;
mgr.add_group_member(&conv.id, &alice.id, "member").await?;
mgr.add_group_member(&conv.id, &bob.id, "member").await?;

mgr.send_message(&conv.id, &alice.id, Some(&bob.id), "hello", None).await?;
let blob = mgr.get_compressed_messages_for_sync(&conv.id, None, 100).await?;
let decoded = ImManager::decompress_messages(&blob)?;
assert!(!decoded.is_empty());
```

### 3. 读取已读回执

```rust
use a3net_types::group_chat::MessageReceipt;
storage.save_receipt("bob", MessageReceipt {
    receipt_id: "r1".into(), message_id: "m1".into(),
    receiver_id: "bob".into(), sequence: 1,
    received_at: 1_700_000_500,
})?;
let receipts = storage.get_message_receipts("bob", "m1")?;
assert_eq!(receipts.len(), 1);
```

### 4. 12 位数字 ID 生成(兼容 hub 服务器)

```rust
use a3net_chatstore::im::generate_12digit_id;
let id = generate_12digit_id();
assert_eq!(id.len(), 12);
```

## 应用案例(Use Cases / Examples)

- **桌面端聊天客户端**:`ChatStorage` 每用户独立 DB,UI 读 `get_direct_messages(user, chat_id)` 显示对话列表,收到新消息后调 `save_receipt` 把未读清零。
- **移动端首次同步**:新设备用 `ImManager::get_compressed_messages_for_sync` 一次拉取整段对话历史(zstd 压缩后 < 30%),解码后落地到本地 SQLite。
- **跨节点 gossip fan-out**:`send_message` 后 IPC 通知 gossip 桥,远端节点用 `save_direct_message` 落库,UI 通过 `verify_integrity_hash` 校验收到的内容。
- **实验性 iroh-docs 迁移**:开启 `iroh` feature 后把 messages 表换成 `IrohDocsChat`,每条对话对应一个 `Doc`,自然获得 CRDT 风格的离线合并 + 端到端加密。

## 许可(License)

MIT OR Apache-2.0
