# adnet-chain

> ADNet 区块链节点脚手架:把 NAS 兼作 archive / observer / validator 二重角色 / ADNet chain-node framework — turn a NAS into an optional blockchain node (Observer / FullNode / Validator).

## 概览(Overview)

`adnet-chain` 目前是**框架骨架**而不是真实链客户端。它给 ADNet NAS 节点预留一个**可选**的第二身份:除了主业(存储 / 智能家居 / 模型目录)外,还能在用户开启时同时作为某条公共区块链的节点。它的存在不是为了"现在就要立刻跑 EVM / Substrate",而是为了:

- 锁定 `ChainNodeConfig` / `ChainKind` / `ChainRole` / `ChainStatus` 这些围绕实际链客户端的"形状";
- 让 `adnet-node` 可以在 `chain` feature 启用时无侵入地接入 `ChainNode::start`;
- 让运维 / 配置文件能立刻看到"这个节点同时跑哪些链、什么角色、数据目录在哪",但运行时是一个 no-op,直到具体链客户端被实现。

未来真正接 EVM / Substrate / Bitcoin-style 节点时,只需要在 `node.rs` 里替换 `start()` 的实现,`ChainNodeHandle` / `ChainStatus` 已经表达出所有需要的状态。

## 特性(Features)

| 名称 | 描述 |
|------|------|
| `ChainNodeConfig` | enabled / kind / role / data_subdir / bind |
| `ChainNodeConfig::enabled(kind, role)` | 快捷构造 |
| `ChainKind` | `None` / `Evm` / `Substrate` / `Custom(String)` |
| `ChainRole` | `Observer` / `FullNode` / `Validator` |
| `ChainStatus` | `Stopped` / `Starting` / `Syncing` / `Synced` / `Error` |
| `ChainNode::start()` | 异步,`enabled=false` 时 no-op,`enabled=true` 时返回 `ChainError::Unimplemented` |
| `ChainNodeHandle::status()` / `::shutdown()` | 预留生命周期钩子 |
| `ChainError` | 配置 / IO / 未实现 / 启动失败 等 |

## 安装(Installation)

`adnet-chain` 是 ADNet workspace 的 path 依赖。

```rust
use adnet_chain::{ChainNode, ChainNodeConfig, ChainKind, ChainRole, ChainStatus};
```

CLI 旁路入口:`adnet-node` 在编译时启用 `chain` feature 后,启动器会读 `ChainNodeConfig` 并调用 `ChainNode::start()`。

## 使用(Usage)

```rust
use adnet_chain::{ChainNode, ChainNodeConfig};

// 1. 默认配置:enabled = false,NAS 行为完全不变
let default_cfg = ChainNodeConfig::default();
let node = ChainNode::new(default_cfg);
let handle = node.start().await?;
assert!(handle.is_none()); // 没启用,所以返回 None
```

```rust
// 2. 启用 EVM / Observer 角色
let cfg = ChainNodeConfig::enabled(ChainKind::Evm, ChainRole::Observer);
let node = ChainNode::new(cfg);
match node.start().await {
    Ok(Some(handle)) => println!("{:?}", handle.status()),
    Ok(None) => println!("disabled"),
    Err(e) => println!("backend not implemented yet: {e}"),
}
```

```rust
// 3. JSON 序列化 / 反序列化
let cfg = ChainNodeConfig::enabled(ChainKind::Substrate, ChainRole::Validator);
let json = serde_json::to_string(&cfg)?;
let back: ChainNodeConfig = serde_json::from_str(&json)?;
assert_eq!(back.kind, ChainKind::Substrate);
```

```rust
// 4. 自定义链种类(实验 / 新链对接)
let cfg = ChainNodeConfig::enabled(ChainKind::Custom("solana".into()), ChainRole::FullNode);
```

```rust
// 5. 关闭链节点
let handle = /* previously started */ todo!();
handle.shutdown();
println!("{:?}", handle.status()); // ChainStatus::Stopped
```

## 应用案例(Use Cases / Examples)

1. **家庭 NAS 顺便读 BTC 链。** 用户的 ADNet NAS 装在客厅,跑着全天候的存储任务。开启 `kind: Evm, role: Observer` 模式,未来一旦 EVM 客户端实现,直接把业内 `reth` / `geth` 接进来,不需要重新写上层 wiring;`bind` 字段已经预留了 RPC 监听地址。
2. **企业子公司运营一个 Substrate 验证节点。** 在 ADNet 配置文件里设 `kind: Substrate, role: Validator`,公司合规审计时,`ChainNodeConfig::enabled(...)` 即是审计员能直接 grep 的"我们是不是真在跑链节点"依据;若是 `enabled = false`,审计员能明确知道"没跑"。
3. **运营测试网试验场。** 实验室想用实验链(非 EVM,非 Substrate)压测 ADNet 节点是否能容下"链 + 存储"双重身份,直接 `ChainKind::Custom("my-experiment-chain".into())` 即可,代码与未来主网升级无冲突。

## 许可

MIT OR Apache-2.0
