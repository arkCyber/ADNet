# adnet-pairing

> ADNet 设备配对协议核心:签名邀请、能力位、可信设备存储 / Secure device-pairing protocol for ADNet — signed invitations, capability bits, and a trusted-device store.

## 概览(Overview)

`adnet-pairing` 是 ADNet 网络中负责**设备配对 (Pairing)** 的核心 crate。它处于 ADNet 身份层 (`adnet-identity`、`adnet-types`) 与传输层 (`adnet-transport`) 之间,提供:

- **离线邀请 (Invitation)** — 由钱包以 EIP-191 签名,可在 QR / 邮件 / 短码中传递;
- **建立证明 (Pairing Ceremony)** — 邀请接受方必须用其 Ed25519 私钥对挑战签名,证明其确实控制该 `NodeId`;
- **能力位 (Capability Set)** — 16-bit tag 组成的细粒度权限集,可被邀请方授予,可被存储与检查;
- **可信设备存储 (Trusted Device Store)** — 落盘的 `devices.jsonl` 记录所有配对成功的设备,丢失设备可凭 `credential_id` 撤销。

这个 crate 是**纯数据**的:不依赖 iroh,不依赖 tokio,所有 IO 都由调用方决定如何落地。它的输出喂给 `adnet-qr`、`adnet-invite` 和 `adnet-ssh` 等用户可见流程。

## 特性(Features)

| 名称 | 描述 |
|------|------|
| `SignedInvitation` | 钱包签名 + 32 字节 salt + 能力 + 过期;支持 `create / verify / to_json / from_json` |
| `InvitationCode` | 24 字符短码 `ADNET:XXXX-YYYY-ZZZZ-NNNN`,用于手动输入 |
| `PairingInvitation`(`wire`) | `adnet-pairing://<base64url>` URL 形式,可 QR 扫描 |
| `CapabilitySet` | 16-bit tag 位集,提供 `contains / intersects / bitmask / canonical` |
| `PairingRequest` / `PairingResponse` | 双向挑战与权限下发,Ed25519 签名 |
| `TrustedDeviceStore` | 落盘 JSONL,支持 `insert / get / revoke / check_capability` |
| `CredentialId` | 由 issuer / invitee / salt 派生的稳定 ID,作为存储键 |

## 安装(Installation)

`adnet-pairing` 已经是 ADNet workspace 的 path 依赖,使用方式:

```rust
// 是否启用额外的 reputation hook(需要底层仓库存活)
use adnet_pairing::{
    SignedInvitation, InvitationCode, PairingInvitation,
    CapabilitySet, Capability, TrustedDeviceStore,
};
```

CLI 子命令:`adnet-cli` 暴露 `adnet pair ...` 系列子命令,内部就是 `adnet-pairing` 的 API。

## 使用(Usage)

```rust
use adnet_pairing::{SignedInvitation, CapabilitySet, PairingInvitation};
use adnet_identity::wallet::Wallet;
use adnet_types::node::NodeId;

// 1. Issuer 端:生成一份带 15 分钟 TTL 的签名邀请
let wallet = Wallet::generate();
let issuer_node_id = NodeId::from_bytes(&[0xAAu8; 32]).unwrap();
let caps = CapabilitySet::from_names(["chat", "files.read"]);
let inv = SignedInvitation::create(
    &issuer_node_id, &wallet, caps, 15 * 60, Some("Alice's iPhone".into()),
)?;

// 2. 转成 QR URL(可直接渲染为二维码)
let url = PairingInvitation::to_url(&inv)?;
println!("Scan: {url}");

// 3. 接收方解析 + 验签
let parsed = PairingInvitation::parse_url(&url)?;
let decoded = parsed.decode()?.unwrap();
decoded.verify(chrono::Utc::now().timestamp())?;
```

```rust
// 4. 受限的短码(用于口头传递)
let code = InvitationCode::from_invitation(&inv)?;
println!("Enter this code: {code}");
let parsed: InvitationCode = "ADNET:AB23-CD45-EF67-GH89".parse()?;
```

```rust
// 5. 派发部分能力
let mut granted = CapabilitySet::empty();
granted.insert(Capability::CHAT);
granted.insert(Capability::FILES_READ);
let grant = CapabilitySet::from_names(["chat", "files.read"]);
assert!(grant.contains(Capability::CHAT));
assert!(!grant.contains(Capability::FILES_WRITE));
```

```rust
// 6. 落盘保存(可选)
use adnet_pairing::{TrustedDeviceStore, TrustedDeviceStoreConfig};
let store = TrustedDeviceStore::open(TrustedDeviceStoreConfig::default())?;
let active = store.is_active(&credential_id, chrono::Utc::now().timestamp());
```

## 应用案例(Use Cases / Examples)

1. **手机扫描家庭服务器 QR。** 用户在 ADNet 桌面端点 `生成邀请`,得到一个 `adnet-pairing://` URL,渲染成 QR 贴在客厅桌面上。手机扫码后,服务端验签 + 接受一个 Ed25519 签名,落地一条 `TrustedDeviceRecord`,从此刻起手机和家庭服务器之间便能以 `chat + files.read` 的能力互相通信。
2. **远程同事不支持 QR,口头传码。** 离线场景下,服务端给出一段 `ADNET:XXXX-YYYY-ZZZZ-NNNN` 24 字符短码,电话另一端手动输入。`InvitationCode` 自身只携带 64 位截断 hash,完整邀请由数据库补全,这就是 `generate_random` + `from_invitation(data)` 的用法。
3. **权限隔离子集设备。** 孩子的平板只能 `chat + sync`,门锁中枢只能 `files.write`,审计台既能 `chat` 还能 `files.read`。`CapabilitySet::from_names(&["chat", "sync"])` 在每次握手前被 `check_capability` 校验,孩子平板永远碰不到门锁。丢失设备时通过 `revoke(&credential_id)` 立即吊销。

## 许可

MIT OR Apache-2.0
