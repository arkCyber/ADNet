# `a3chat-crypto`

> 端到端加密 (E2E) 协议栈,仅依赖 `a3chat-core` 的领域类型,**业务层**(`a3chat-app` / `a3chat-rpc` / `a3chat-tauri`)使用。
>
> **加密算法**:
>
> - **私聊**: Noise_XX (`snow` 实现) → 派生出 32-byte 会话密钥 →  ChaCha20-Poly1305 AEAD(`chacha20poly1305` 实现)每条消息独立 nonce。
> - **群聊**: Signal-style **Sender Keys**(每群一条 chain key),群成员退出 → 群主轮换。
> - **跨设备**: Argon2id 派生 KEK → ChaCha20-Poly1305 加密私钥 + Sender Keys 序列。
>
> **AEAD 关联数据 (AD) 合同**: `sender | receiver | conversation_id | sequence | timestamp`。`session::seal` 把 5 字段串联后做 `Poly1305` 认证,接收端 `open` 必须用**字节一致**的 AD 才能验签通过 — 这就是 `a3chat-app::storage::edit_message` 在重封时必须复用原 envelope `timestamp` / `sequence` 的原因。
>
> **安全边界**: 本 crate 不接触文件系统;密钥在内存中,依赖 `zeroize::ZeroizeOnDrop`。落盘由 `a3chat-app::ChatStorage` 完成,落盘格式是 `a3chat-core::message::MessageBody::Encrypted`(`algorithm = "chacha20-poly1305-v1"`)。

## 模块

| 模块 | 内容 |
|---|---|
| `error` | `CryptoError` 加密错误类型(桥接到 `a3chat-core::A3chatError::CryptoError`) |
| `session` | `DmSession` Noise_XX + ChaCha20-Poly1305 会话抽象(`SessionKey` / `SessionKeys`) |
| `sender_keys` | `SenderKey` 群密钥 + `SenderKeyChain` |
| `kek` | Argon2id KEK 派生 + `EncryptedBundle` |
| `random` | 跨 OS / 跨调用稳定接口的安全随机数 + nonce 生成 |

## 不做

- 不做密钥存储(由 `a3net-crypto::KeyStore` 负责)。
- 不做网络握手状态机(由 `a3chat-app` 在 P2P 通道上承载 Noise 字节流)。
- 不做 KDF 重新设计 — 直接调用 `a3net-crypto::kdf`。
- 不做身份层 — 长期私钥来自 `a3net-identity`(后续 P6 接入)。

## License

MIT OR Apache-2.0
