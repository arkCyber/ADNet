# P1 功能 RPC 方法文档

本文档详细说明了 P1 阶段新增功能的 RPC 方法接口。

---

## 📋 目录

1. [关键词通知服务](#关键词通知服务-keyword-notification-service)
2. [消失消息服务](#消失消息服务-disappearing-message-service)
3. [熔断器状态查询](#熔断器状态查询-circuit-breaker)
4. [错误码说明](#错误码说明)

---

## 关键词通知服务 (Keyword Notification Service)

命名空间: `a3chat.keyword.*`

### `add_keyword`

添加全局关键词，当消息中包含该关键词时触发通知。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |
| `keyword` | `String` | 是 | 关键词内容（最长 100 字符） |
| `is_regex` | `bool` | 是 | 是否为正则表达式 |

#### 返回值

```rust
KeywordEntry {
    keyword_id: String,      // 关键词唯一 ID
    keyword: String,         // 关键词内容
    is_regex: bool,          // 是否为正则表达式
    created_at: DateTime,    // 创建时间
    match_count: u64,        // 匹配次数
}
```

#### 错误码

| 错误码 | 说明 |
|-------|------|
| `InvalidInput` | 关键词为空或超过长度限制 |
| `Conflict` | 关键词已存在 |
| `QuotaExceeded` | 已达到关键词数量上限 (100) |
| `InvalidRegex` | 正则表达式格式错误 |

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.keyword.add_keyword",
  "params": {
    "user_id": "user_123",
    "keyword": "urgent",
    "is_regex": false
  },
  "id": 1
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": {
    "keyword_id": "kw_abc123",
    "keyword": "urgent",
    "is_regex": false,
    "created_at": "2026-08-21T06:00:00Z",
    "match_count": 0
  },
  "id": 1
}
```

---

### `remove_keyword`

删除已添加的关键词。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |
| `keyword_id` | `String` | 是 | 关键词 ID |

#### 返回值

```rust
bool  // true: 删除成功, false: 关键词不存在
```

#### 错误码

| 错误码 | 说明 |
|-------|------|
| `NotFound` | 关键词不存在 |

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.keyword.remove_keyword",
  "params": {
    "user_id": "user_123",
    "keyword_id": "kw_abc123"
  },
  "id": 2
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": true,
  "id": 2
}
```

---

### `list_keywords`

列出用户的所有全局关键词。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |

#### 返回值

```rust
Vec<KeywordEntry>  // 关键词列表
```

#### 错误码

无特定错误码。

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.keyword.list_keywords",
  "params": {
    "user_id": "user_123"
  },
  "id": 3
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": [
    {
      "keyword_id": "kw_abc123",
      "keyword": "urgent",
      "is_regex": false,
      "created_at": "2026-08-21T06:00:00Z",
      "match_count": 5
    },
    {
      "keyword_id": "kw_def456",
      "keyword": "bug-\\d+",
      "is_regex": true,
      "created_at": "2026-08-21T06:10:00Z",
      "match_count": 12
    }
  ],
  "id": 3
}
```

---

### `update_keyword`

更新关键词的匹配模式。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |
| `keyword_id` | `String` | 是 | 关键词 ID |
| `new_keyword` | `String` | 是 | 新的关键词内容 |
| `is_regex` | `bool` | 是 | 是否为正则表达式 |

#### 返回值

```rust
KeywordEntry  // 更新后的关键词信息
```

#### 错误码

| 错误码 | 说明 |
|-------|------|
| `NotFound` | 关键词不存在 |
| `InvalidInput` | 新关键词为空或超过长度限制 |
| `InvalidRegex` | 正则表达式格式错误 |

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.keyword.update_keyword",
  "params": {
    "user_id": "user_123",
    "keyword_id": "kw_abc123",
    "new_keyword": "URGENT",
    "is_regex": false
  },
  "id": 4
}
```

---

### `add_conversation_keyword`

为特定会话添加关键词（仅在该会话中触发通知）。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |
| `conversation_id` | `ConversationId` | 是 | 会话 ID |
| `keyword` | `String` | 是 | 关键词内容 |
| `is_regex` | `bool` | 是 | 是否为正则表达式 |

#### 返回值

```rust
KeywordEntry  // 关键词信息
```

#### 错误码

同 `add_keyword`

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.keyword.add_conversation_keyword",
  "params": {
    "user_id": "user_123",
    "conversation_id": "conv_abc",
    "keyword": "deadline",
    "is_regex": false
  },
  "id": 5
}
```

---

### `get_conversation_keywords`

获取特定会话的关键词列表。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |
| `conversation_id` | `ConversationId` | 是 | 会话 ID |

#### 返回值

```rust
Vec<KeywordEntry>  // 会话关键词列表
```

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.keyword.get_conversation_keywords",
  "params": {
    "user_id": "user_123",
    "conversation_id": "conv_abc"
  },
  "id": 6
}
```

---

### `get_rate_limiter_stats`

获取关键词通知速率限制统计信息。

#### 参数

无参数。

#### 返回值

```rust
RateLimiterStats {
    total_requests: u64,      // 总请求数
    total_limited: u64,       // 被限流的请求数
    active_buckets: usize,    // 活跃的令牌桶数
}
```

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.keyword.get_rate_limiter_stats",
  "params": {},
  "id": 7
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": {
    "total_requests": 1500,
    "total_limited": 42,
    "active_buckets": 15
  },
  "id": 7
}
```

---

## 消失消息服务 (Disappearing Message Service)

命名空间: `a3chat.ephemeral.*`

### `register_message`

将消息注册为临时消息（阅后即焚）。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `message_id` | `MessageId` | 是 | 消息 ID |
| `conversation_id` | `ConversationId` | 是 | 会话 ID |
| `sender_id` | `UserId` | 是 | 发送者 ID |

#### 返回值

```rust
bool  // true: 注册成功, false: 该会话未启用临时消息
```

#### 错误码

| 错误码 | 说明 |
|-------|------|
| `InvalidInput` | 消息 ID 或会话 ID 无效 |
| `NotFound` | 会话不存在 |

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.ephemeral.register_message",
  "params": {
    "message_id": "msg_123",
    "conversation_id": "conv_abc",
    "sender_id": "user_123"
  },
  "id": 8
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": true,
  "id": 8
}
```

---

### `mark_read`

标记临时消息为已读，触发删除倒计时。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |
| `message_id` | `MessageId` | 是 | 消息 ID |

#### 返回值

```rust
bool  // true: 标记成功, false: 消息不存在或非临时消息
```

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.ephemeral.mark_read",
  "params": {
    "user_id": "user_456",
    "message_id": "msg_123"
  },
  "id": 9
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": true,
  "id": 9
}
```

---

### `get_ephemeral_stats`

获取用户的临时消息统计信息。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |

#### 返回值

```rust
EphemeralStats {
    user_id: UserId,            // 用户 ID
    total_tracked: usize,       // 跟踪的临时消息总数
    pending_deletions: usize,   // 待删除的消息数
    read_messages: usize,       // 已读消息数
}
```

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.ephemeral.get_ephemeral_stats",
  "params": {
    "user_id": "user_123"
  },
  "id": 10
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": {
    "user_id": "user_123",
    "total_tracked": 15,
    "pending_deletions": 3,
    "read_messages": 8
  },
  "id": 10
}
```

---

### `set_ephemeral_settings`

设置会话的临时消息配置。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `user_id` | `UserId` | 是 | 用户 ID |
| `conversation_id` | `ConversationId` | 是 | 会话 ID |
| `timer` | `DisappearingTimer` | 是 | 删除计时器 |

**DisappearingTimer 枚举**:
- `Off`: 关闭临时消息
- `After30Seconds`: 30 秒后删除
- `After5Minutes`: 5 分钟后删除
- `After1Hour`: 1 小时后删除
- `After1Day`: 1 天后删除
- `After1Week`: 1 周后删除

#### 返回值

```rust
()  // 无返回值
```

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.ephemeral.set_ephemeral_settings",
  "params": {
    "user_id": "user_123",
    "conversation_id": "conv_abc",
    "timer": "After5Minutes"
  },
  "id": 11
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": null,
  "id": 11
}
```

---

### `cleanup_orphaned_messages`

清理孤儿临时消息（已过期但未删除的消息）。

#### 参数

无参数。此方法通常由后台任务定期调用。

#### 返回值

```rust
()  // 无返回值
```

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.ephemeral.cleanup_orphaned_messages",
  "params": {},
  "id": 12
}
```

---

## 熔断器状态查询 (Circuit Breaker)

命名空间: `a3chat.circuit_breaker.*`

### `get_circuit_state`

获取熔断器当前状态。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `service` | `String` | 是 | 服务名称（如 "group_sync"） |

#### 返回值

```rust
CircuitState  // 熔断器状态枚举
```

**CircuitState 枚举**:
- `Closed`: 正常状态，请求可通过
- `Open`: 熔断状态，阻止所有请求
- `HalfOpen`: 半开状态，允许部分请求探测

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.circuit_breaker.get_circuit_state",
  "params": {
    "service": "group_sync"
  },
  "id": 13
}
```

**响应**:
```json
{
  "jsonrpc": "2.0",
  "result": "Closed",
  "id": 13
}
```

---

### `reset_circuit`

手动重置熔断器到关闭状态。

#### 参数

| 参数名 | 类型 | 必需 | 说明 |
|-------|------|------|------|
| `service` | `String` | 是 | 服务名称 |

#### 返回值

```rust
()  // 无返回值
```

#### 错误码

| 错误码 | 说明 |
|-------|------|
| `NotFound` | 服务不存在 |
| `PermissionDenied` | 无权限操作 |

#### 示例

**请求**:
```json
{
  "jsonrpc": "2.0",
  "method": "a3chat.circuit_breaker.reset_circuit",
  "params": {
    "service": "group_sync"
  },
  "id": 14
}
```

---

## 错误码说明

### 通用错误码

| 错误码 | HTTP状态码 | 说明 |
|-------|-----------|------|
| `InvalidInput` | 400 | 输入参数无效 |
| `NotFound` | 404 | 资源不存在 |
| `Conflict` | 409 | 资源冲突 |
| `QuotaExceeded` | 429 | 超出配额限制 |
| `PermissionDenied` | 403 | 权限不足 |
| `InternalError` | 500 | 服务器内部错误 |

### 特定错误详情

**InvalidRegex**:
- 当正则表达式语法错误时返回
- 错误消息会包含具体的语法错误位置

**QuotaExceeded**:
- 关键词数量超过 100 个/用户
- 速率限制超出（10 通知/分钟/用户/关键词）

---

## 使用建议

### 1. 关键词通知最佳实践

- **全局关键词**: 用于重要的、跨会话的通知（如姓名、项目名）
- **会话关键词**: 用于临时的、特定场景的通知
- **正则表达式**: 适用于模式匹配（如 bug 单号、PR 编号）
- **速率限制**: 注意每分钟通知频率，避免通知风暴

### 2. 临时消息最佳实践

- **默认计时器**: 建议使用 5 分钟或 1 小时
- **敏感信息**: 使用 30 秒计时器
- **后台清理**: 系统每 5 分钟自动清理孤儿消息

### 3. 熔断器监控

- **定期检查**: 通过 `get_circuit_state` 监控服务健康
- **告警配置**: 当熔断器打开时触发告警
- **手动恢复**: 仅在确认问题解决后手动重置

---

## 版本信息

- **文档版本**: 1.0.0
- **API 版本**: P1 (2026-08-21)
- **向后兼容**: 是

---

## 变更日志

### 2026-08-21
- 初始版本
- 添加关键词通知服务 6 个方法
- 添加消失消息服务 5 个方法
- 添加熔断器状态查询 2 个方法

---

**文档生成时间**: 2026-08-21  
**维护者**: A3Net Team
