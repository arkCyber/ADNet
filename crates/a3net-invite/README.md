# a3net-invite

> 邮件邀请投递层:把 `a3net-pairing` 的签名邀请打包成 MIME 邮件 / 短文本码,完成双向配对流程 / Email-invitation delivery layer for A3Net — wraps signed pairing invitations as MIME messages and short text codes.

## 概览(Overview)

`a3net-invite` 是 A3Net 邮件配对通道的"最后一公里"。它不做 SMTP 投递本身,也不做密码学 — 它只负责**把现有邀请变成可投递的载体**:

- 把 `SignedInvitation` 序列化为 `application/x-a3net-pairing` 的 MIME 附件,再附带一封可读的邮件(纯文本 + 可选 HTML + 内嵌 QR SVG),通过 `a3net-mail` 投递;
- 支持 `build_invitation_email_with_qr` 形式,直接渲染一张可在邮件正文里显示的 QR 码;
- 提供 `TextCode` 短文本码(`ADNET-XXXX-XXXX-XXXX-XXXX#CC`),IMAP 不可达、SMTP 被屏蔽或收件人只有电话时,人能口头读出 / 抄写下来;
- 接收端 `extract_from_mail` / `extract_from_wire` 把邮件还原回 `PairingInvitation`,供 `a3net-pairing` 验签。

整个 crate 假定**TLS-保护的 SMTP 投递**提供机密性,邀请本身始终是签名而非加密,这是银行 / GitHub 重置链接的常用做法。

## 特性(Features)

| 名称 | 描述 |
|------|------|
| `build_invitation_email` | 生成带 `.inv` 附件 + `a3net-pairing://` URL 内联的纯文本邮件 |
| `build_invitation_email_with_qr` | 在邮件正文内嵌 QR SVG,附件含 `.inv` + `.svg` |
| `extract_from_mail` | 从 `Mail` 中筛 `application/x-a3net-pairing` 附件并解析 |
| `extract_from_wire` | 直接从 IMAP `BODY[]` 返回的 RFC 5322 字节里解析 |
| `create_text_code` / `parse_text_code` | `ADNET-…#CC` 短文本码,带 CRC8 校验 |
| `MAX_INVITATION_SIZE = 32 KiB` | 解析时的硬上限,防止恶意巨大的 MIME 块 |
| `InvitationContent` | 受 SMTP 限制驱动的 `from / to / subject / body` 字段 |

## 安装(Installation)

`a3net-invite` 已经是 A3Net workspace 的 path 依赖。其调用方通常是 `a3net-cli` / `a3net-ssh` 的"发送邀请"路径,以及 `a3net-mail` 的 IMAP 拉取回调。直接 `use` 即可:

```rust
use a3net_invite::{InvitationMailer, InvitationContent, create_text_code, parse_text_code};
use a3net_pairing::SignedInvitation;
```

## 使用(Usage)

```rust
use a3net_identity::wallet::Wallet;
use a3net_invite::{InvitationContent, InvitationMailer};
use a3net_mail::mime::Address;
use a3net_pairing::{CapabilitySet, SignedInvitation};
use a3net_types::node::NodeId;

// 1. 创建钱包签名的邀请
let wallet = Wallet::generate();
let node_id = NodeId::from_bytes(&[0xAAu8; 32])?;
let inv = SignedInvitation::create(
    &node_id, &wallet,
    CapabilitySet::from_names(["chat", "files.read"]),
    15 * 60, Some("Alice's Laptop".into()),
)?;

// 2. 装配邮件内容
let content = InvitationContent {
    from: Address::new("alice@example.com").with_name("Alice"),
    to: vec![Address::new("bob@example.com")],
    subject: "A3Net Pairing Invitation".into(),
    body: "scan the QR or open a3net-pairing://…".into(),
};

// 3. 转成一封可投邮件
let mail = InvitationMailer::build_invitation_email(&inv, &content)?;
// smtp::send(&mail, &smtp_config).await?;
```

```rust
// 4. 接收端:从 IMAP 拉到的字节直接还原
let inv = InvitationMailer::extract_from_wire(&raw_email)?;
inv.verify(chrono::Utc::now().timestamp())?;
```

```rust
// 5. 短文本码(适合电话 / SMS / 抄录)
use a3net_invite::{create_text_code, parse_text_code};
let code = create_text_code(&inv)?;
println!("Tell your peer: {code}");
let parsed = parse_text_code(&code.to_string())?.unwrap();
assert_eq!(parsed.payload.issuer_wallet, inv.payload.issuer_wallet);
```

```rust
// 6. 富邮件 + 内嵌 QR
let mail = InvitationMailer::build_invitation_email_with_qr(&inv, &content)?;
assert!(mail.html.is_some());
assert_eq!(mail.attachments.len(), 2); // .inv + .svg
```

## 应用案例(Use Cases / Examples)

1. **跨团队异地不再交换 QR。** 客户支持人员的 MacBook 在广东,用户的 desktop 在北京,两人隔着防火墙看不到对方的二维码。客服在自己的 A3Net 桌面端发"邀请此邮箱",对方在邮件里点击 `a3net-pairing://…` 链接,导入 IMAP 收到的 `a3net-pairing.inv` 附件,即可完成配对。`extract_from_wire` 把整个流程闭合到一行代码。
2. **老式电话激活。** 用户的设备只能收 SMS,不能扫码 / 收邮件。后台给出 `ADNET-XXXX-XXXX-XXXX-XXXX#CC` 短文本码,人工通过电话读出,对方抄下来,`parse_text_code` 校验 CRC8 然后同样完成配对。CRC8 在抄错一位时即时报错,避免浪费 15 分钟 TTL。
3. **企业 SSO 邮件集成。** 公司希望走自己 SMTP 网关,模板打到 `mail.text` + `mail.attachments` 之后,客服直接接管。`build_invitation_email` 已经填好 `X-Adnet-Invite` / `X-Mailer` 自定义头,以及 `a3net-pairing.inv` 标准附件名,企业 MTA 的 filter 规则可以直接匹配。

## 许可

MIT OR Apache-2.0
