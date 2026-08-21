# a3net-mailbox

> Offline message store-and-forward for A3Net applications. / 离线消息中转节点，给 A3Net 各应用复用。

## 概览 (Overview)

`a3net-mailbox` 是 `a3net-relay` 的**配套** crate。Relay 是**透明转发代理**（无状态、无身份），Mailbox 则是**有状态收件箱**：

- 接收方在线 → 走 P2P 投递（Relay 帮你穿透 NAT）
- 接收方离线 → 投递方把消息送进 Mailbox，接收方上线后第一时间拉取

`a3net-mailbox` 是给所有 A3Net 应用（`a3chat`, `a3net-share`, `a3net-mail`, `a3net-socialfeed`, ...）复用的**基础设施层**，不耦合任何具体应用协议。

## 状态 (Status)

**Phase 3 (current).** 完整 Phase 3 功能已完成：

- ✅ Rate limiting (per-IP token bucket, 60 req/min enqueue, 300 req/min read)
- ✅ `billing` feature — `a3net-token` pledge 验证接入 enqueue
- ✅ EIP-712 风格 timestamp 绑定（5 分钟 stale 窗口防重放）
- ✅ Per-recipient TTL override (`RetentionPolicy`)
- ✅ 所有 Phase 0–2 功能

阶段划分：

| Phase | 范围 | 状态 |
|---|---|---|
| 0 | crate 骨架 + route 表 + MemoryStore + 6 个 stub | ✅ |
| 1 | 签名验证 + 配额/TTL + MailboxClient | ✅ |
| 2 | `SqliteStore` + 连接池 + 分页 + Prometheus export | ✅ |
| 3 | Rate limiting + Billing + EIP-712 timestamp + Per-recipient TTL | ✅ |

## 架构 (Architecture)

```
crates/a3net-mailbox/
  src/
    lib.rs              pub mod server; pub mod client; pub mod config;
                        pub mod storage; pub mod policy; pub mod metrics;
    config.rs           MailboxConfig (bind/db_path/ttl/quota)
    server.rs           axum Router (Phase 1 实现)
    client.rs           reqwest-based Client
    storage.rs          trait MailboxStore + MemoryStore + SqliteStore (Phase 1)
    policy.rs           SizePolicy / QuotaPolicy / TtlPolicy / RetentionPolicy
    rate_limit.rs       Per-IP token-bucket middleware (P3-4)
    billing.rs          Pledge 验证与 bonus quota (P3-3)
    auth.rs             EIP-191 + EIP-712 timestamp binding (P3-7)
    metrics.rs          MailboxMetrics singleton
  tests/                Phase 1
  examples/             Phase 1
```

## 设计要点 (Design Pillars)

- **End-to-end opaque**：服务端**不读 envelope 明文**。只验证 sender 签名、按 recipient_id 路由。
- **Pull-only delivery（Phase 1）**：上线时拉取 + 30s 周期轮询。**不维护 WebSocket / 反向连接状态**，运维简单。
- **Per-recipient quota / TTL**：每个 `recipient_id` 受消息条数、字节总量、单条 TTL 约束。
- **Plug-in storage**：`trait MailboxStore` 抽象。Phase 1 提供 `SqliteStore`（单文件 SQLite，默认）。
- **复用 a3net-relay 模式**：`axum` Router / `ServerPolicy::from_config` / `MailboxServerHandle::Drop` 优雅关闭 / `MailboxMetrics::get()` 单例。
- **可选 `billing` feature**（仿 `a3net-relay`）：开启后接受签名 pledge 兑换更大配额。默认关闭。

## 协议 (Wire Protocol)

```
POST /v1/inbox/{recipient_id}
  Headers: X-A3Net-Sender-Id, X-A3Net-Sender-Sig, X-A3Net-Timestamp
  Body:   { msg_id, ciphertext_b64, ttl_secs? }
  → 202 Accepted | 413 (too large) | 401 (bad sig) | 429 (quota)

GET /v1/inbox/{recipient_id}?since=<watermark>&limit=100
  Headers: X-A3Net-Recipient-Sig
  → 200 { messages: [{msg_id, sender_id, ciphertext_b64, queued_at}, ...],
          next_watermark, has_more }

POST /v1/inbox/{recipient_id}/ack  body: { msg_ids: [...] }
  Headers: X-A3Net-Recipient-Sig
  → 200 { acked: N }

GET /healthz   → 200 "ok"
GET /metrics   → 200 { enqueues, rejected, pulls, acks, purged, queue_depth, ... }
GET /metrics?format=prometheus → 200 text/plain; Prometheus 0.0.4 format
```

客户端推荐使用 `MailboxClient::pull_all` 进行自动分页拉取（`has_more` 循环）。

## 部署 (Deployment)

与 `a3net-relay` **同一台固定 IP 服务器不同端口**：

| 服务 | 端口 |
|---|---|
| `a3net-relay` (HTTP 转发代理) | 18790 |
| `a3net-mailbox` (离线收件箱) | 18791 |

未来所有 A3Net 应用都**复用** `a3net-mailbox`，无需各自部署中转节点。

## 当前限制 (Phase 2 Caveats)

- 部署为多节点时需要共享存储（Postgres / S3）—— 当前 SQLite 仅支持单节点（下一版本）
- `billing` feature（`a3net-token` pledge 验证）未实现
- 尚未接入 `a3chat-app`（Phase 2 之后）

## 安装 (Installation)

```rust
use a3net_mailbox::{MailboxClient, MailboxConfig, MailboxServer};
```

## 启动本地 mailbox (Phase 1+)

```rust,no_run
use a3net_mailbox::MailboxServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let handle = MailboxServer::start("127.0.0.1", 18791).await?;
    println!("up at {}", handle.base_url);
    handle.shutdown();
    Ok(())
}
```

## 持久化配置 (Phase 1+)

```rust,no_run
use a3net_mailbox::MailboxConfig;
use std::path::Path;

let cfg = MailboxConfig::default();
cfg.save(Path::new("./app_data"))?;
let loaded = MailboxConfig::load(Path::new("./app_data"));
# let _ = loaded;
```
