# adnet-identity

> ADNet 身份层 — secp256k1 `Wallet` / EVM `Address` / EIP-191 `PersonalSignature` / X25519 ECIES `SealedEnvelope`,为签名、加密、地址生成提供统一原语。

## 概览 (Overview)

`adnet-identity` 是 ADNet 所有密码学原语的中心:

- **钱包** — `Wallet` 生成 secp256k1 密钥对,从其中派生 `Address`(EVM 兼容 0x… 20 字节地址)与 `WalletPublic`。
- **签名** — `PersonalSignature` 实现 EIP-191(`personal_sign`)风格签名,可被 `recover_personal` 反解出签名者地址。
- **ECIES 加密** — `EciesSecretKey` / `EciesPublicKey`(X25519)+ AES-256-GCM 实现的 `SealedEnvelope`,用于在 gossip / RPC 上保护 payload。
- **公告签名** — `sign_announcement` / `verify_announcement` 是 `Announcement` 的标准签名路径。
- **国库** — `Treasury` / `ReceiptWallet` 在钱包之上做一次性的收入归集。

## 特性 (Features)

- **`Wallet::generate()`** — 随机 secp256k1 密钥对,内部走 `k256` crate。
- **`Wallet::sign_personal(&[u8;32]) -> PersonalSignature`** — EIP-191 风格签名,可恢复签名者地址。
- **`WalletPublic::recover_personal(&digest, &sig) -> Address`** — 反解签名者。
- **`Address` / `Address::from_hex(...)`** — EVM 兼容地址,20 字节十六进制。
- **`EciesSecretKey::generate()` + `SealedEnvelope::seal(&peer_pub, payload)`** — 一次性加密,接收方用 `open(&secret)` 解。
- **`SealedEnvelope::encode() -> Vec<u8>`** — 把 envelope 编码成带 magic header 的字节流,`decode` 反向。
- **`ADR_ENVELOPE_MAGIC` / `ADR_ENVELOPE_VERSION`** — ADNet envelope 的协议常量,避免误用其他 ECIES 格式。
- **`sign_announcement` / `verify_announcement`** — 为 `adnet_types::Announcement` 提供的"消息签名 + 验签"工具。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-identity = { workspace = true }
adnet-types = { workspace = true }   # 公告签名需要
```

## 使用 (Usage)

### 1. 钱包 + EIP-191 签名

```rust
use adnet_identity::{Wallet, WalletPublic};

let wallet = Wallet::generate();
let digest: [u8; 32] = blake3::hash(b"hello adnet").into();

let sig = wallet.sign_personal(&digest).expect("sign");
let recovered = WalletPublic::recover_personal(&digest, &sig).expect("recover");
assert_eq!(recovered, wallet.public().address());
```

### 2. ECIES / SealedEnvelope

```rust
use adnet_identity::{EciesSecretKey, EncryptedPayload, SealedEnvelope};

let recipient = EciesSecretKey::generate();
let payload = EncryptedPayload::from(b"some message".to_vec());

let env = SealedEnvelope::seal(recipient.public_key(), payload).expect("seal");
let wire = env.encode();   // ADR envelope bytes
let back = SealedEnvelope::decode(&wire).expect("decode");
let opened = back.open(&recipient).expect("open");
```

### 3. 公告签名 / 验签

```rust
use adnet_identity::{sign_announcement, verify_announcement};
use adnet_types::Announcement;

let ann = Announcement { /* … fields … */ };
let signed = sign_announcement(&ann, &wallet)?;
verify_announcement(&signed, &wallet.public().address())?;
```

### 4. 公共密钥 ↔ 压缩字节

```rust
let pk_bytes: [u8; 33] = wallet.public().public_key_bytes();
let same = WalletPublic::from_compressed(&pk_bytes)?;
```

## 应用案例 (Use Cases / Examples)

- **`adnet-token`** — `Pledge::sign(body, &wallet)` 用 `Wallet::sign_personal` 对 `PledgeBody` 摘要签名,`verify_for_relay` 用 `recover_personal` 反解 pledgor 地址。
- **`adnet-gossip`** — gossip 上传递的 `Announcement` 若带 `signature`,接收端走 `verify_announcement` 校验。
- **`adnet-ipc` / `adnet-rpc`** — daemon 之间传输敏感 payload 时使用 `SealedEnvelope`,接收方只用自己的 X25519 secret key 打开。
- **`adnet-mail`** — 邮件 / 通知附带的 sender 字段走 `Address` + `PersonalSignature` 校验身份。
- **`adnet-share`** — share 文件时把 owner 标记为 `Address`,接收端用签名校验控制权变更。
- **AI agent wallet** — 自动续费 / 自动签名的 agent 直接持有 `Wallet`,在 `adnet-token` 中发起 pledge。

## 许可

MIT OR Apache-2.0