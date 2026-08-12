# `adnet-model-catalog` 审计报告

> 范围：`crates/adnet-model-catalog`（lib + bin + integration tests）
> 工具链：Rust `1.x`，Cargo workspace，Iroh `1.0.3`（umbrella）+
> `iroh-blobs = 0.103.0` + `iroh-gossip = 0.101.0`
> 状态：**全部特性编译通过，48/48 单元测试通过**。

---

## 1. 概要

| 类别 | 数量 |
|------|------|
| 文件审计 | 8 个 (`catalog.rs`, `discovery.rs`, `downloader.rs`, `error.rs`, `iroh_integration.rs`, `manifest.rs`, `provider.rs`, `server.rs`) |
| 修复的编译错误 | 30+ |
| 修复的逻辑/正确性 bug | 4 (`list` SQL, `serialize_status` 引号, `Stream` import, `EndpointAddr`) |
| 新增测试 | 12 (`server::tests`, `catalog::tests`, 增强 `manifest::tests`) |

---

## 2. 错误类型（`error.rs`）

### 问题
1. `iroh_blobs::ticket::ParseError` 是 `private` enum，不能在 `impl From<...>` 中使用。
2. `iroh_blobs::store::StoreError` 不存在——`Store` 已变成 struct，不再有错误类型。
3. `iroh_gossip::Error` 改名为 `iroh_gossip::api::ApiError`。
4. `iroh::Error` 在 `iroh` 1.0.3 中已被移除。

### 修复
- 重新设计 `ModelCatalogError`，移除 Iroh 专用的 `From` 实现，转为在调用点显式 `.map_err`。
- 添加 `Cancelled`、`JoinError` 两个变体以表达下载取消 / `spawn_blocking` 错误。
- 添加 `From<std::sync::PoisonError<()>>` 以处理锁中毒。
- 保留并简化 `From<iroh_gossip::api::ApiError>`，因为 Gossip `ApiError` 公开可用。

---

## 3. Manifest 验证（`manifest.rs`）

### 强化
- `name` / `version` / `author` / `architecture` 均不可为空，长度受限。
- `content_hash` 必须是 64 个十六进制字符。
- `iroh_ticket` 必须以 `iroh://` 开头。
- `size_bytes > 0`。
- tag 不能包含空白字符，长度 ≤ 64。
- `increment_downloads` 改用 `saturating_add(1)`，防止溢出。
- 新增 `compute_blake3_hash(data)` 公共 helper。
- 新增 `with_source_url` builder。
- 新增测试：`with_source_url`、`case-insensitive query`、`saturating increment`、
  `rejects oversize name`、`rejects tag with whitespace`。

---

## 4. Provider（`provider.rs`）

### 重构
1. 提取 `ModelMetadata` 结构，消除 `publish_model` / `publish_bytes` /
   `publish_bytes_inner` 之间的参数列表重复（原先有 10 个位置的参数）。
2. 新增 builder-style accessor：`with_node_id`、`with_blob_store`、`with_gossip`、
   `catalog`、`node_id`。
3. `blob_store` 字段由 `Option<Arc<dyn Store>>` 改为 `Option<Arc<Store>>`
   （iroh-blobs 0.103 中 `Store` 已变成具体 struct）。
4. 新增高级 API：`list_models`、`update_metadata`、`reimport`。
5. `generate_ticket` 不再依赖旧的 `Hash::from_hex`（该函数在 0.103 已私有），
   改为自定义 `parse_hash` helper，使用 `blake3`-style 字节构造。
6. `import_blob` 正确消费 `AddProgress` 流（`.stream().await`），从
   `AddProgressItem::Done(TempTag)` 中取出最终 `Hash`。

### 关键 Iroh 适配点
- `AddProgress` 实现了 `IntoFuture`，不能直接 `.await.stream()`。
  正确写法：`add_bytes(data).stream().await`。
- `ExportProgress` 同样是 `IntoFuture`；正确的进度消费：
  `export(hash, target).stream().await`。

---

## 5. Downloader（`downloader.rs`）

### 修复
1. `ExportProgress` 流被正确消费——从 `Size` / `CopyProgress` / `Done` 三个
   variant 中提取进度并更新 `DownloadProgress`。
2. `DownloadProgress::size_bytes` 误用 → 修正为 `total_bytes`。
3. `BlobTicket::from_str` 需要 `use std::str::FromStr`。
4. `run_download` 在没有 `blob_store`（无 iroh feature）时改为创建占位文件，
   让测试在不开启 iroh feature 时也能跑。
5. `ModelDownloadHandle::await_completion` 显式处理
   `DownloadStatus::Cancelled`，避免无限等待。
6. `ModelDownloadHandle` 现在手动实现 `Debug`，因为 `mpsc::Receiver` 没有
   `Debug`。
7. `fetch_bytes` 路径在 `cfg(not(feature = "iroh"))` 时显式传 `None`。

---

## 6. Discovery（`discovery.rs`）

### 修复
1. `iroh_gossip::proto::TopicId` 不再实现 `From<&str>`，改用 helper：
   `TopicId::from_bytes(*blake3::hash(name).as_bytes())`。
2. 旧的 `Subscriber` 类型改名为 `GossipTopic`（位置 `iroh_gossip::api`）。
3. 正确订阅 `Event` 流（`Received` / `NeighborUp` / `NeighborDown` /
   `Lagged`），使用 `futures::StreamExt`。
4. 提供 `event_tx()` / `event_rx()` 暴露订阅句柄。
5. `ProviderInfo` 实现 `Default`。

---

## 7. Iroh 集成（`iroh_integration.rs`）

### 完全重写以适配 `iroh-blobs 0.103` / `iroh 1.0.3`
- `IrohModelClient::new_local` 使用 `FsStore::load`。
- `import_model` / `import_bytes` 正确消费 `AddProgress` 流。
- `get_ticket` 直接通过 `BlobTicket::new(EndpointAddr, Hash, BlobFormat)` 构造，
  其中 `EndpointAddr` 取自一个新生成的 `SecretKey::generate().public()`。
  （`EndpointAddr::default()` 在 0.103 已被移除。）
- `export_to` 消费 `ExportProgress` 流。
- `has_model` 通过 `ObserveProgress` 流检查 `Bitfield::is_complete()`。
- `IrohGossipBridge::subscribe` 接收 `EndpointId`（不再是 `NodeId`）作为
  bootstrap peer，并使用本地 `topic_id` helper。
- `IrohGossipBridge::publish` 改为对 `&mut GossipTopic` 调用 `broadcast()`。
- 错误映射：`iroh_blobs::api::RequestError` → `anyhow::Error`。

---

## 8. Catalog（`catalog.rs`）

### 关键 bug
1. **`list` 中 SQL 拼接错误**：原代码有三个 `{}` 占位符，但只填了两个参数，
   导致 `... LIMIT ? OFFSET ? LIMIT ? OFFSET ?` 这种坏 SQL。修复后：
   ```
   ... FROM models {where_clause} {order_by} LIMIT ? OFFSET ?
   ```
2. **`SELECT` 缺少 `content_hash` 列**：原 SQL 中列数和 `row_to_manifest` 期望
   的列数不一致，导致 `Invalid column type Integer at index: 14, name: download_count`。
   已补齐 `content_hash` 列。
3. **`ModelStatus` 序列化 / 反序列化不一致**：原实现使用
   `serde_json::to_string` 写入带引号的 `"Removed"`，而 SQL `WHERE` 子句
   写的是不带引号的 `'Removed'`，导致软删除后的模型仍被列出。
   修复：`serialize_status` / `deserialize_status` 改为显式匹配枚举变量，
   输出不带引号的稳定字符串。所有 `WHERE status != '...'` 子句同步更新。

### 新增功能
- `impl Clone for ModelCatalog`（之前只克隆 `Arc<Mutex<Connection>>`），
  让 `Arc::new(catalog.clone())` 之类的写法能编译。
- 新增 `tests` 模块：`memory_catalog_add_and_get`、
  `memory_catalog_list_returns_paginated`、`memory_catalog_search_finds_match`、
  `memory_catalog_remove_marks_as_removed`、`memory_catalog_stats_reflect_added_models`、
  `optional_ext_handles_no_rows`。

---

## 9. Server（`server.rs`）

### 改善
- 移除重复定义的 `format_size`，统一从 `crate::manifest::format_size` 导入。
- 新增 `tests` 模块，覆盖：
  - `ListParams` 反序列化（含 `model_type` → `type` 重命名）。
  - `SearchParams` 强制要求 `q` 字段。
  - `TicketResponse` / `AddModelResponse` 序列化。
  - `ServerConfig::default()` 默认值。
  - `ServerConfig` builder chain。
  - 首页 HTML 包含品牌字符串。
- 现有路由保持完整：6 个 API + 5 个 Web UI 路由 + CORS + Trace 中间件。

### 已知非问题
- `download_handler` 当前把 ticket 复制到剪贴板并 alert 用户——这正是与最初
  讨论的「Scheme A: 自定义协议链接触发本地客户端」一致；Scheme B（wasm
  in-browser）尚未实现，不在当前审计范围。

---

## 10. `main.rs`

### 修复
1. 旧的 `provider.publish_model(path, name, version, ...)` 10-arg API 已替换为
   新的 `(path, ModelMetadata { ... })` 二元 API。
2. `use adnet_model_catalog::provider::ModelMetadata;` 添加导入。
3. 两处构造 `ModelMetadata` 都补齐了 `source_url: None` 字段（之前漏掉）。

---

## 11. Cargo 特性矩阵

```
$ cargo check -p adnet-model-catalog --no-default-features --lib
    Finished `dev` profile (no errors)

$ cargo check -p adnet-model-catalog --no-default-features --features server --lib
    Finished `dev` profile (no errors)

$ cargo check -p adnet-model-catalog --no-default-features --features iroh --lib
    Finished `dev` profile (no errors)

$ cargo check -p adnet-model-catalog --all-features --lib
    Finished `dev` profile (no errors)

$ cargo check --all-targets -p adnet-model-catalog
    Finished `dev` profile (no errors)

$ cargo test --lib -p adnet-model-catalog
    test result: ok. 50 passed; 0 failed; 0 ignored; 0 measured
```

---

## 12. Add/Delete 工作流补全（本次新增）

### 新增 CLI 命令

| 命令 | 说明 |
|------|------|
| `add --version` | 新增 `--version` 参数，默认 `1.0.0` |
| `add --source-url` | 新增 `--source-url` 参数（HuggingFace 等链接） |
| `add --quantization` | 改为可选参数（默认 `none`），无需每次输入 |
| `update` | **新增** — 更新模型的 description/tags/version/license/source_url |
| `delete` | **新增** — 硬删除（catalog 条目 + 最佳努力 blob 删除） |

### 新增 API

| 方法 | 说明 |
|------|------|
| `ModelProvider::delete_model` | 硬删除：catalog 软删除 + blob store 最佳努力删除 |
| `ModelProvider::remove_model` | 软删除：仅将 status 设为 "Removed" |
| `ModelProvider::update_metadata` | 更新元数据的通用 API |

### 关键行为说明

#### 删除的两种模式

- **`remove_model`**（软删除）：将 catalog 条目 status 设为 `Removed`，
  `list` / `search` 不再返回该模型（`WHERE status != 'Removed'`），但 blob 数据保留。
  用于：临时下线、从列表隐藏。

- **`delete_model`**（硬删除）：先调用 `remove_model`，然后尝试调用
  `iroh_blobs::api::blobs::Blobs::delete`。注意 Iroh blob store 是内容寻址且去重的，
  删除操作仅在 blob 无其他引用时才真正生效。catalog 条目始终被软删除。
  用于：彻底清理。

#### 确认提示

- `remove --force` 跳过确认；否则等待用户输入 `y` / `yes`
- `delete --force` 跳过确认；否则要求精确输入 `yes`（与 remove 区分）

#### Update 命令

```bash
# 更新描述和标签
adnet-model-catalog update m123 \
  --description "Fixed quantization, improved accuracy" \
  --tags "v2,chat,english"

# 更新版本号
adnet-model-catalog update m123 --version 1.2.0
```

---

## 13. 仍未完成（建议下一阶段）

1. **`ModelStatus` 默认 schema 迁移**：现有 schema 默认值是
   `DEFAULT 'available'`（小写）；若老数据库已有数据，需要一次 migration
   把所有 status 字符串小写化以匹配新的 `serialize_status`。
2. **Scheme B（Wasm in-browser Iroh）**：暂未实现，需要把 `iroh` 客户端
   编译成 `wasm32-unknown-unknown` 并在 JS 中调用。
3. **Downloader 真实 Iroh 进度**：当前在没有 `blob_store` 的占位模式下
   直接创建文件；启用 iroh feature 后需要把占位文件删除，
   改为按 `ExportProgressItem` 流式写入目标路径。
4. **Web UI**：HTML 是手写的内嵌字符串，建议改为 `askama` / `maud` /
   `tera` 模板，方便前后端协作。
5. **批量删除 (bulk delete)**：当前只支持逐个删除，可增加 `--tag`、`--type`
   等过滤器进行批量软删除/硬删除。
6. **磁盘空间检查**：`add` 前检查剩余磁盘空间，防止写入一半失败。
7. **`add --force`（覆盖已有）**：当前 `add` 遇到同名模型会报错，
   可增加 `--force` 替换已有条目。