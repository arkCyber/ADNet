# adnet-token

> ADNet 中继(relay)计费令牌 — 自包含、签名、可序列化、可由中继离线接受的付款承诺(`Pledge` / `Claim` / `Receipt`)。

## 概览 (Overview)

`adnet-token` 定义了 ADNet 中"中继收费"使用的链下支付凭证:

- **不是链上交易** — 是一个由 EVM 钱包签名的"承诺付款",中继离线接受,后续统一清算。
- **自包含** — 一个 `Pledge` 包含金额、收款方地址、过期时间、nonce、签名,可序列化进 QR 码 / 深链 / gossip。
- **URL 形式** — `adnet-token://<chain_id>/<contract>/<token>/<amount>/<recipient>/<nonce>/<expiry>/<sig>`,扫码即付。
- **三段生命周期** — `Pledge`(签名承诺) → `Claim`(中继向清算服务提交) → `Receipt`(清算回执)。

设计动机是把 token 逻辑与 `adnet-identity` 的原始密码学原语、与 `adnet-types` 的轻量类型层解耦,这样:

- `adnet-types` 保持无密码学依赖,继续做廉价的 wire 合约。
- `adnet-identity` 拥有钱包与签名原语。
- `adnet-token` 在它们之上做"中继计费"应用。

## 特性 (Features)

- **`Pledge`** — 签名后的付款承诺,可由 `Pledge::body(...)` 构造、`Pledge::sign(body, &wallet).sign()` 签名、`Pledge::verify(...)` / `verify_for_relay(...)` 验证。
- **`Pledge::to_url()` / `Pledge::from_url()`** — 序列化到扫码 URL,反序列化回 `Pledge`。
- **`Claim`** — 中继向清算服务提交的统一条目,聚合多笔 `Pledge`。
- **`Receipt`** — 清算服务回执,中继 / 客户端都可校验。
- **`MAX_AMOUNT_ATOMIC`** — 安全上限,防止意外铸造天量金额。
- **`TokenError`** — 验证失败、过期、签名错配的统一错误类型。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-token = { workspace = true }
adnet-identity = { workspace = true }   # 签名依赖
```

## 使用 (Usage)

### 1. 构造并签名一个 `Pledge`

```rust
use adnet_identity::Wallet;
use adnet_token::Pledge;

let wallet = Wallet::generate();
let recipient = "0x52908400098527886E0F7030069857D2E4169EE7".parse().unwrap();

let body = Pledge::body(
    /* chain_id */ 1,
    /* contract */ "0xa0b8...eb48".into(),   // USDC
    /* token    */ "0xa0b8...eb48".into(),
    /* amount   */ 1_500_000,                // 1.50 USDC
    recipient,
    /* nonce    */ "00".repeat(32),
    /* expiry   */ chrono::Utc::now().timestamp() + 1800,
).expect("valid body");

let pledge = Pledge::sign(body, &wallet).expect("sign");
pledge.verify(chrono::Utc::now().timestamp()).expect("verify");
```

### 2. 中继视角:校验链 + 过期

```rust
pledge.verify_for_relay(now, chain_id).expect("relay accepts");
```

### 3. URL ↔ 二进制往返(QR 码场景)

```rust
let url = pledge.to_url();          // adnet-token://...
let parsed = Pledge::from_url(&url).expect("parse url");
let recovered = parsed.verify_with_recovered(now).expect("verify");
assert_eq!(recovered, wallet.public().address());
```

### 4. 中继 → 清算:`Claim`

```rust
use adnet_token::Claim;

let claim = Claim::from_pledges(chain_id, vec![pledge.clone()]);
let submitted = claim.submit_to_clearing_service(&relay_wallet).await?;
```

## 应用案例 (Use Cases / Examples)

- **`adnet-relay` 中继流量计费** — 客户端访问中继,先签一笔 `Pledge`,中继按流量扣款;客户端只需要在第一次建立连接时扫码即可。
- **`adnet-exit-node` 出口节点计费** — 出口节点同样消费 `Pledge` / `Receipt`,与 `adnet-relay` 共用同一套 token 协议。
- **QR 码离网支付** — 用户在不联网的状态下,通过扫码把 `Pledge` 传给收款方;中继在后续连接时再批量清算。
- **批量清算** — 中继一天收集数百笔 `Pledge`,统一打包成 `Claim` 提交上链,降低 gas。
- **AI agent 自助付费** — `adnet-mail` / `adnet-share` 中的智能体自动续费,直接调用 `Pledge::sign` 不需要人工介入。

## 许可

MIT OR Apache-2.0