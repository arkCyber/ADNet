# `adnet-webdav`

> 把 `adnet-blobstore` 命名空间以 WebDAV 协议暴露给 Finder / Explorer / 第三方客户端;每个动词经能力令牌校验、NDJSON 审计;DO-178C DAL-A 安全规范。
>
> Expose the `adnet-blobstore` namespaces over RFC 4918 WebDAV so Finder / Explorer / any third-party client can mount the home NAS; every verb is capability-gated and NDJSON-audited; DO-178C DAL-A safety profile.

## 概览(Overview)

`adnet-webdav` 是一个小型 DO-178C **DAL-A** 模块,作用是把 ADNet 的 NAS 命名空间(`adnet-blobstore::Nas`)以 RFC 4918 WebDAV 协议挂载出来,让 macOS Finder / Windows Explorer / 任何 WebDAV 客户端都能像挂载 SMB 一样挂载家里的 NAS。设计上由五块组成:

1. **handlers (`handlers.rs`)** —— `HandlerState` 把 `Nas`、`AclMiddleware`、`TokenVerifier`、`Clock` 拼到一起,所有 WebDAV 动词(`OPTIONS` / `PROPFIND` / `GET` / `PUT` / `HEAD` / `MKCOL` / `DELETE` / `MOVE` / `COPY`)都从这里出。
2. **server (`server.rs`)** —— `WebdavServer::new(config, state)` + `.start().await` 返回 `WebdavServerHandle`,带 `shutdown()` / `local_addr()` 生命周期控制。底层用 hyper 1.x HTTP/1。
3. **acl (`acl.rs`)** —— `AclMiddleware<R: CapabilityResolver>` 是 capability 校验入口,`StaticCapabilityResolver` 是默认内存实现,生产可注入持久后端。每个能力 `ResolvedCapability { caps, nonce, expires_unix_ms, revoked }`,`register(id, rc)` 注册。
4. **token (`token.rs`)** —— `TokenVerifier::new(key)` + `.verify(&token)` 用 HMAC-SHA256 校验 `CapabilityToken { capability_id, nonce, expires_unix_ms, signature }`,常量时间比较。
5. **aerospace (`aerospace.rs`, feature `aerospace`)** —— `SAFETY_REVISION` / `DAL_LEVEL="A"` / `HAZARD_REGISTER_REV` / 100% MC/DC + branch + stmt 覆盖率目标。CI 用 `tests/dal_a_compliance.rs` 验证合规基线。

`PROPFIND` 支持 `Depth: 0 | 1 | infinity`,标准 DAV: 属性(`resourcetype` / `getcontentlength` / `getcontenttype` / `displayname` / `getetag` / `supportedlock`)齐全。`LOCK` / `UNLOCK` 故意返回 `405 Not Implemented`(家庭 NAS 不需要 WEBDAV 锁,OS 自己有文件锁)。

## 特性(Features)

- **`WebdavServer::new` / `start`** —— 一行启动 HTTP server;`config = WebdavConfig { host, port }`,默认 `127.0.0.1:8780`。
- **`HandlerState::new(nas, resolver, verifier)`** —— 装配所有依赖。
- **完整 RFC 4918 §9 动词集**:`OPTIONS` / `PROPFIND` / `GET` / `HEAD` / `PUT` / `MKCOL` / `DELETE` / `MOVE` / `COPY`。
- **`Depth` 枚举** + DAV: 属性。
- **`StaticCapabilityResolver::register(id, ResolvedCapability)`** —— 注册 capability 记录。
- **`AclMiddleware::authorise(credential_id, verb, required)`** —— 返回 `AclDecision::Allow` / `Deny` / `Unauthorized`。
- **`CapabilityToken::sign(verifier, id, nonce, expires_unix_ms)`** —— 客户端签名能力令牌;`.from_header("Authorization: Capability …")` 反向解码。
- **NDJSON 审计**(SR-15):状态变更动词在返回前向 `audit.jsonl` 落盘一条不可抵赖记录。
- **`aerospace` feature**:打开后启用 `aerospace` 模块 + `tests/dal_a_compliance.rs` 合规测试。
- **零 unsafe**(`#![forbid(unsafe_code)]`),所有错误统一为 `TokenError` / `HttpError` / `AclError`。

## 安装(Installation)

工作空间内 path 依赖:

```toml
# 你的 crate 的 Cargo.toml
adnet-webdav = { workspace = true }
adnet-pairing = { workspace = true }
```

```rust
use adnet_webdav::{
    WebdavConfig, WebdavServer, WebdavServerHandle, HandlerState,
    AclMiddleware, StaticCapabilityResolver, ResolvedCapability,
    CapabilityToken, TokenVerifier, AclDecision,
};
use adnet_pairing::CapabilitySet;
```

## 使用(Usage)

### 1. 启动一个最小 WebDAV server

```rust
use std::sync::Arc;
use adnet_webdav::{
    HandlerState, StaticCapabilityResolver, TokenVerifier, WebdavConfig, WebdavServer,
};
use adnet_blobstore::Nas;

let nas = Nas::open_default().await?;
let resolver = Arc::new(StaticCapabilityResolver::new());
let verifier = TokenVerifier::new([0u8; 32]);
let state = Arc::new(HandlerState::new(nas, resolver, verifier));
let server = WebdavServer::new(WebdavConfig::default(), state);
let handle: WebdavServerHandle = server.start().await?;
println!("WebDAV at http://{}", handle.local_addr());
```

### 2. 注册一个只读 capability

```rust
use adnet_webdav::{ResolvedCapability, StaticCapabilityResolver};
use adnet_pairing::CapabilitySet;

let resolver = StaticCapabilityResolver::new();
resolver.register(
    "device-1".into(),
    ResolvedCapability {
        caps: CapabilitySet::from_names(["files.read"]),
        nonce: [1u8; 32],
        expires_unix_ms: 9_999_999_999_999,
        revoked: false,
    },
);
```

### 3. 签名 / 校验 capability 令牌

```rust
use adnet_webdav::{CapabilityToken, TokenVerifier};

let verifier = TokenVerifier::new([0xAA; 32]);
let token: CapabilityToken = verifier.sign("device-1", [0x11; 32], 9_999_999_999_999);
assert!(verifier.verify(&token).is_ok());

let header = format!("Capability {}", token.to_header_b64());
let parsed = CapabilityToken::from_header(&header)?;
```

### 4. ACL 决策

```rust
use adnet_webdav::{AclMiddleware, AclDecision};
use adnet_pairing::CapabilitySet;

let mw = AclMiddleware::new(Arc::new(resolver));
let decision = mw.authorise(Some("device-1"), "get", &CapabilitySet::from_names(["files.read"]));
assert_eq!(decision, AclDecision::Allow);
```

### 5. 关闭 server

```rust
handle.shutdown();
```

## 应用案例(Use Cases / Examples)

- **Finder / Explorer 挂载 NAS**:`http://nas.local:8780/` 配 `files.read` + `files.write` capability,Finder 直接挂载读写。
- **第三方 WebDAV 客户端**(Cyberduck / RaiDrive):登录走 `Authorization: Capability <b64url>`,审计所有上传下载。
- **CI 自动化**:`curl --upload-file` 经 PUT 写日志,NDJSON 审计保留每条非授权尝试。
- **DO-178C 合规检查**:`cargo test -p adnet-webdav --features aerospace` 跑 `tests/dal_a_compliance.rs`,确认 `SAFETY_REVISION` 与 `HAZARD_REGISTER_REV` 匹配。
- **跨设备配对**:`adnet-pairing` 颁发的 capability 天然带 `nonce` / `expires_unix_ms`,挂在 Finder 上 30 天后自动失效,无需重新配对。

## 许可(License)

MIT OR Apache-2.0
