# `adnet-workspace`

> 每节点的 P2P 文件交换工作区 —— `shared / inbox / outbox` 三目录,JSON 清单 + gossip 公告。
>
> Per-node shared folder for P2P file exchange — `shared / inbox / outbox` directories, JSON manifest, gossip announcements.

## 概览(Overview)

`adnet-workspace` 是 ADNet 中"每节点共享文件夹"那一块的最小可用实现。它源自 `Exodus@src-backup/.../exodus_workspace.rs`,把一个节点对外暴露的共享文件 / 收到的文件 / 待发送的文件统一放在同一棵目录树下,以 JSON 清单记录,再通过 gossip 公告给 peer。

设计上由四个角色组成:

- **目录布局** —— 节点根目录下固定三个子目录:`shared/`(对 peer 公开)、`inbox/`(从 peer 收到)、`outbox/`(待发出)。根目录默认在 `<app_data>/ExodusWorkSpace/`,目录名沿用原 Exodus 命名,确保老 peer 兼容。
- **清单 (`WorkspaceManifest`)** —— JSON 文件 `workspace.json`,记录所有 `WorkspaceFileEntry { name, ext, size, content_hash, updated_at }`。
- **发布 API** —— `publish_file(&Path, Option<content_hash>)`:把文件复制到 `shared/` 并更新清单;`manifest_snapshot()` 拿到完整快照。
- **gossip 公告** —— `workspace_room_topic()` 返回 `adnet-room-{WORKSPACE_ROOM_ID}`,与 `adnet-gossip` 的 `adnet-room-*` 主题约定一致;peer 订阅该主题就能在清单变更时收到增量。

整个 crate 体积小,零外部 dep(`serde` / `serde_json` / `thiserror` / `tracing`),#![forbid(unsafe_code)],所有错误统一为 `String`(兼容 `Box<dyn Error>`)。

## 特性(Features)

- **`Workspace::new(app_data_dir, node_id)`** —— 一行打开 / 创建工作区。
- **`shared_dir() / inbox_dir() / outbox_dir()`** —— 三个目录路径。
- **`publish_file(&src, Option<hash>) -> WorkspaceFileEntry`** —— 复制文件到 `shared/` 并记录清单。
- **`manifest_snapshot() -> WorkspaceManifest`** —— 拿到完整清单(序列化为 JSON 喂给 gossip)。
- **`workspace_room_topic()`** —— 公告主题名(`adnet-room-{room_id}`)。
- **`split_name_ext(path)`** —— helper:把 `report.pdf` → `("report", "pdf")`。
- **零 unsafe**(`#![forbid(unsafe_code)]`),所有错误 `Result<_, String>`。

## 安装(Installation)

工作空间内 path 依赖:

```toml
# 你的 crate 的 Cargo.toml
adnet-workspace = { workspace = true }
```

```rust
use adnet_workspace::{
    Workspace, WorkspaceFileEntry, WorkspaceManifest,
    DIR_SHARED, DIR_INBOX, DIR_OUTBOX,
    workspace_room_topic, split_name_ext,
};
```

## 使用(Usage)

### 1. 打开工作区

```rust
use adnet_workspace::Workspace;
let ws = Workspace::new(std::env::temp_dir().as_path(), "my-node-id")?;
println!("root: {}", ws.root().display());
```

### 2. 发布一个文件

```rust
use std::io::Write;
use std::path::Path;

let src = Path::new("/tmp/payload.bin");
std::fs::File::create(src)?.write_all(b"hello workspace")?;

let entry = ws.publish_file(src, Some("blake3:abc123".into()))?;
println!("published: {} ({} bytes)", entry.name, entry.size);
```

### 3. 读 manifest

```rust
use adnet_workspace::WorkspaceManifest;

let m: WorkspaceManifest = ws.manifest_snapshot()?;
let json = serde_json::to_string_pretty(&m)?;
println!("{json}");
```

### 4. 公告到 gossip

```rust
let topic = workspace_room_topic();
println!("announce on topic: {topic}");
// adnet_gossip::GossipTransport::broadcast(topic, json_payload).await?;
```

### 5. split helper

```rust
use adnet_workspace::split_name_ext;
let (stem, ext) = split_name_ext("report.pdf");
assert_eq!(stem, "report");
assert_eq!(ext, Some("pdf".into()));
```

## 应用案例(Use Cases / Examples)

- **小公司内 P2P 文件交换**:`adnet-cli` 默认把 `~/Documents/shared/` 映射到 `shared/`,同事只需要 `adnet-cli pull report.pdf` 就拿到最新版本。
- **离线 NAS 投送**:运营把固件放到 `outbox/`,邻居节点起来后从 gossip 收到清单变更,自动通过 `inbox/` 拉取。
- **临时分享**:`adnet-ffi` 移动端拍照片后调 `publish_file()`,在 `shared/` 留副本,gossip 推到家庭成员的设备。
- **可重现构建**:`workspace.json` 加进 git,任何 CI 拿 `manifest_snapshot()` 对照 hash 即可校验文件是否被篡改。
- **跨平台桥接**:Mac 节点把目录挂到 `shared/`,Windows peer 走 WebDAV(`adnet-webdav`)看到的就是同一份 `shared/`。

## 许可(License)

MIT OR Apache-2.0
