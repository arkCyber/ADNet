# adnet-verify

> ADNet 协议的形式化验证套件 — TLA+ 规范(`verification/tla/`)与 Kani 模型检查器集成,覆盖 DHT/Kademlia、Gossip、Bitswap 三大协议。

## 概览 (Overview)

`adnet-verify` 把 ADNet 的核心 P2P 协议放进形式化验证流水线:

- **DHT / Kademlia** — 路由表不变量(`RoutingTable::add` 去重、`closest_peers` 排序、`xor_distance` 对称 / 反身)。
- **Gossip** — 主题传播、消息去重、mesh 维护。
- **Bitswap** — `WantList` 完整性 / 去重,`LedgerBook` 借 / 贷守恒。

每个协议都有:

1. **TLA+ 规范**(`verification/tla/<Protocol>.tla`)— 系统级并发模型,用 TLC 检查死锁 / 安全 / 活性。
2. **Kani 证明**(`#[cfg(kani)]` 模块)— 用 AWS Kani 对核心数据结构做有界模型检查。

## 特性 (Features)

- **`RoutingTable`** — Kademlia 风格的 k-bucket 表:`add(peer) -> bool`、`closest_peers(target, k)`、`len()`。
- **`KBucketEntry`** — 单个 bucket 节点条目,带 `last_seen` / `failed_pings`。
- **`xor_distance(a, b) -> u256`** — Kademlia 距离度量,`xor_distance(a, a) == 0` 且对称。
- **`bitswap::WantList`** — 待拉取块集合:`add(entry)` 去重、`top()` 取最高优先级、`BitswapInvariants::wantlist_valid` 形式化校验。
- **`bitswap::LedgerBook`** — 每 peer 借 / 贷记:`record_sent` / `record_received` / `balance`,`BitswapInvariants::debt_ratio_valid` 保证非负与平衡性。
- **`gossip::*`** — 主题传播 / mesh 维护的形式化模型。
- **`VERSION`** — `env!("CARGO_PKG_VERSION")`,便于 runtime 自报。
- **`#![forbid(unsafe_code)]` + `#![deny(unused_must_use)]`** — 严格的代码卫生,Kani 才能信任其前置条件。

## 安装 (Installation)

```toml
# crates/<your-crate>/Cargo.toml
[dependencies]
adnet-verify = { workspace = true }
```

Kani 证明只在带 `#[cfg(kani)]` 注解的模块中编译,不会污染默认构建。

## 使用 (Usage)

### 1. DHT 路由表

```rust
use adnet_types::NodeId;
use adnet_verify::{RoutingTable, xor_distance};

let me = NodeId::random();
let mut table = RoutingTable::new(me.clone(), /*k=*/ 3);

for _ in 0..20 {
    let peer = NodeId::random();
    let _ = table.add(peer);
}

let target = NodeId::random();
let closest = table.closest_peers(&target, 3);
for p in closest {
    let d = xor_distance(&p.node_id, &target);
    println!("{}  distance={:x}", p.node_id.short(), d);
}

assert_eq!(xor_distance(&me, &me).to_u128().unwrap(), 0);
```

### 2. Bitswap `WantList` / `LedgerBook`

```rust
use adnet_verify::bitswap::{WantEntry, WantList, LedgerBook, BitswapInvariants};

let mut want = WantList::new();
want.add(WantEntry::new(b"cid-1".into(), 1));
want.add(WantEntry::new(b"cid-2".into(), 5));

let have = vec![b"cid-3".into()];
let valid = BitswapInvariants::wantlist_valid(&want, &have);

let mut book = LedgerBook::new();
{
    let entry = book.get_or_create(b"peer-A");
    entry.record_sent(1000);
    entry.record_received(500);
}
let peer_a = book.get_or_create(b"peer-A").clone();
assert!(BitswapInvariants::debt_ratio_valid(&peer_a));
assert!(peer_a.is_balanced(/*ratio=*/ 2.0));
```

### 3. Gossip 验证原语

```rust
use adnet_verify::gossip::{Message, MessageDeduper, GossipInvariants};

let mut deduper = MessageDeduper::default();
let msg = Message::new("topic", b"payload");
deduper.observe(&msg);
assert!(deduper.contains(&msg));
```

### 4. 运行 Kani 证明

```bash
cargo kani --package adnet-verify
# 期望输出: 0 unproved checks; N proven properties for DHT / Bitswap / Gossip
```

### 5. 运行 TLA+ 规范

```bash
cd verification/tla
java -jar tla2tools.jar -deadlock -config DHT.cfg DHT.tla
java -jar tla2tools.jar -deadlock -config Bitswap.cfg Bitswap.tla
java -jar tla2tools.jar -deadlock -config Gossip.cfg Gossip.tla
```

## 应用案例 (Use Cases / Examples)

- **`adnet-dht`** — 复用 `RoutingTable` 与 `xor_distance` 的实现,且 `closest_peers` 行为与 Kani 证明中的 `proof_closest_peers_sorted` 一致。
- **`adnet-blobstore`** — bitswap 借 / 贷账户由 `LedgerBook` 维护,`is_balanced` 是 gossip / ledger 闸门;`BitswapInvariants::debt_ratio_valid` 在 release build 里被断言。
- **`adnet-gossip`** — `MessageDeduper` 保证同一消息不会被重复广播;`GossipInvariants` 检查 mesh 不变量。
- **CI 流水线** — Kani 证明在 `cargo kani --package adnet-verify` 步骤中跑,任意"已证"属性失败都会阻止 merge。
- **协议演进** — 改 DHT 桶大小 / 改 ledger 借 / 贷比例时,TLA+ 规范允许先证明再实现。
- **学术 / 文档** — TLA+ 文件可作为 ADNet 协议规范的官方参考,与实现一一对应。

## 许可

MIT OR Apache-2.0