# a3net-security

> A3Net 安全原语 — ACL 访问控制 / 会话加密 / 入侵检测 / 密钥管理 / 审计日志,零信任 + 纵深防御 + 可审计。

## 概览 (Overview)

`a3net-security` 把 A3Net 缺失的安全基础设施集中到一个 crate:

- **ACL** — 基于策略的访问控制,deny-by-default,支持 owner / group / wildcard 规则。
- **Session** — Signal-style 双棘轮 E2E 加密会话(`SessionManager` / `EncryptedMessage`)。
- **Intrusion Detection** — 异常流量 / 行为检测,产出 `SecurityEvent` 与 `ThreatLevel`。
- **Key Management** — `KeyStore` 持有多版本密钥,`KeyRotationPolicy` 自动 / 手动轮换。
- **Audit Log** — 安全事件记录,带 severity 过滤、内存上限、可选落盘。

## 特性 (Features)

- **`AccessControl`** — `can(subject, resource, permission) -> bool`;支持多 policy + default policy。
- **`AclPolicy`** — 一组 `AclEntry`,带 deny-by-default 或 allow-by-default 开关。
- **`AclEntry`** — `(Subject, Resource, Permission[], allow)` 四元组,支持 owner-of-resource 绕过。
- **`Session`** — 双棘轮会话,加密每条 `EncryptedMessage`,自动派生新链密钥。
- **`SessionManager`** — 多会话管理,提供 `create_session(peer)` / `encrypt` / `decrypt`。
- **`IntrusionDetector`** — 接收 `SecurityEvent`,按 `ThreatPattern` 匹配,产出 `AnomalyScore`。
- **`KeyStore`** — 内存 / 磁盘后端(`KeyStoreBackend::Memory` / `File`);`create_key` / `get_key` / `rotate_key` / `get_active_key_data`。
- **`KeyRotationPolicy`** — 配置轮换间隔、最少保留版本数、自动轮换开关。
- **`AuditLog`** — 内存环形缓冲,按 `AuditSeverity` 过滤;`stats()` 暴露按 severity / type / outcome 计数。
- **`AuditEventType`** — 枚举: `UserLogin` / `AccessGranted` / `AccessDenied` / `KeyGenerated` / `KeyRotated` / `DataRead` / `ConnectionFailed` / …
- **`AuditOutcome`** — `Success` / `Failure`。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
a3net-security = { workspace = true }
```

## 使用 (Usage)

### 1. ACL:deny-by-default,给 `bob` 开一个读权限

```rust
use a3net_security::{
    AccessControl, AclEntry, AclPolicy, AccessLevel,
    Permission, Resource, ResourceType, Subject,
};

let acl = AccessControl::default_config();
let mut policy = AclPolicy::new("default".into());
policy.default_access_level = AccessLevel::DenyAll;
policy.add_entry(AclEntry::new(
    Subject::new_user("bob".into()),
    Resource::new(ResourceType::Blob, "*.pdf".into()),
    vec![Permission::Read],
    false,
));
acl.add_policy(policy).await?;

let bob = Subject::new_user("bob".into());
let report = Resource::new(ResourceType::Blob, "report.pdf".into());
assert!(acl.can(&bob, &report, Permission::Read).await);
assert!(!acl.can(&bob, &report, Permission::Write).await);
```

### 2. 审计日志

```rust
use a3net_security::audit::{
    AuditConfig, AuditEventType, AuditLog, AuditOutcome, AuditRecord,
    AuditSeverity,
};

let audit = AuditLog::new(AuditConfig {
    min_severity: AuditSeverity::Info,
    max_in_memory: 1024,
    ..Default::default()
});
audit.record(AuditRecord::new(
    AuditEventType::UserLogin,
    "user bob login".into(),
    AuditOutcome::Success,
)).await;

let stats = audit.stats().await;
println!("recorded {} events", stats.total_records);
```

### 3. 密钥管理:创建、轮换、读取活跃密钥

```rust
use a3net_security::key_management::{KeyStore, KeyRotationPolicy, KeyType};
use chrono::Duration;

let store = KeyStore::memory();
let policy = KeyRotationPolicy::new(
    "node-encryption".into(),
    KeyType::Symmetric,
    Duration::days(30),
);
let key_id = store.create_key(
    "node-encryption".into(),
    KeyType::Symmetric,
    b"key-bytes-v1".to_vec(),
    Some(policy),
).await?;
let v0 = store.get_key(&key_id).await?.active_version().cloned().unwrap();

// Rotate.
let v1 = store.rotate_key(&key_id, b"key-bytes-v2".to_vec()).await?;
let active = store.get_active_key_data(&key_id).await?;
assert_eq!(active, b"key-bytes-v2".to_vec());
```

### 4. 入侵检测

```rust
use a3net_security::intrusion::{IntrusionDetector, SecurityEvent, ThreatLevel};

let detector = IntrusionDetector::default_config();
let level = detector.observe(SecurityEvent::RapidHandshakes { peer, count: 100 }).await;
if level >= ThreatLevel::High {
    // ban the peer
}
```

## 应用案例 (Use Cases / Examples)

- **`a3net-gateway` HTTP 鉴权** — 每次 `/api/v0/*` 走 `AccessControl::can(user, resource, permission)`,deny-by-default,审计日志记录每次访问。
- **`a3net-blobstore` 所有权** — `Resource::with_owner("alice")` 让 owner 绕过 ACL;分享时通过 `AccessLevel::AllowAll` 临时开放。
- **`a3net-node` 节点密钥轮换** — `KeyStore` 在节点启动时检查所有密钥的 `KeyRotationPolicy`,到期自动 `rotate_key`,告警平台读取 `KeyRotated` 审计事件。
- **`a3net-chat` E2E 加密** — `SessionManager` 为每个 chat 房间维护一个双棘轮会话,消息以 `EncryptedMessage` 形式入库。
- **合规审计** — 关键操作(密钥生成、访问决策、登录、连接失败)全部落 `AuditLog`,按 severity 过滤后导出 JSON,供 SOC2 审计。
- **威胁响应** — `IntrusionDetector` 检测到扫描/洪泛后,触发审计 `ThreatPattern::BruteForce` 事件,自动 ban peer 并记录到 `a3net-reputation`。

## 许可

MIT OR Apache-2.0