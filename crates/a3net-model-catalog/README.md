# a3net-model-catalog

> A3Net 模型目录:去中心化 AI 模型(P2P + Bao 校验 + gossip 发现 + 信誉标签)分发网络 / A3Net model catalog — decentralized AI model distribution backed by P2P, Bao verification, gossip discovery, and provider reputation.

## 概览(Overview)

`a3net-model-catalog` 处于 A3Net 体系的应用层,负责把 AI 模型(LLM、LoRA、SDXL、Whisper 等)从一台 provider 节点,以 P2P 方式、以 Bao 内容可校验的格式,投递到任意数量的 consumer 节点。具体来说:

- **元数据目录 (SQLite)** — 记录 manifest(`ModelManifest`)的所有属性(name / version / type / architecture / quantization / license / size / content_hash / iroh_ticket / tags);
- **协议层 (iroh feature)** — 模型文件经 iroh blob store 导入,生成 `BlobTicket`;之后任何 peer 都能从该 ticket 拉取并由 Bao 校验;
- **发现层 (iroh feature)** — provider 通过 `a3net-gossip` 广播 `ModelAnnouncement`,其他节点无需预先知道 node id 也能发现新模型;
- **信誉层** — `ProviderReputationTracker` 汇总下载结果、用户举报与 manifest 完整性校验,提供 trust tier 给 UI 排序;
- **HTTP 网关 (server feature)** — axum 暴露 REST API(`/api/models`, `/api/search?q=…`),自带 minijinja 模板渲染的 Web UI。

整个 crate 是 `a3net-cli` 命令行 `a3net model-catalog …` 的后端,也可独立运行(`a3net-model-catalog serve`)。

## 特性(Features)

| 名称 | 描述 |
|------|------|
| `ModelCatalog::open(path)` | 打开 / 创建 SQLite 库,自动建表 + FTS5 触发器 |
| `ModelProvider::publish_model(path, metadata)` | 读取文件 → 算 blake3 → 导入 blob store → 生成 ticket → 写 manifest |
| `ModelProvider::publish_bytes(bytes, metadata)` | 同上,接受内存里已经装配好的字节 |
| `ModelCatalog::list(filter)` | 按 type / tag / architecture / size / query 过滤 + 分页 |
| `ModelCatalog::search(q)` | 全文搜索(name / description / author / tags) |
| `ModelProvider::update_status / remove_model / delete_model` | 软删除 + 硬删除(后端尽力回收 blob) |
| `ProviderReputationTracker` | 记录下载结果,持久化 trust tier |
| `ModelAnnouncement` (iroh feature) | gossip 广播 / 订阅 |

## 安装(Installation)

`a3net-model-catalog` 是 A3Net workspace 的 path 依赖。

```rust
use a3net_model_catalog::{
    ModelCatalog, ModelProvider, ModelType, Quantization,
    ModelMetadata, ModelFilter,
};
use std::sync::Arc;
```

CLI 子命令入口:
```bash
a3net-model-catalog add    --path ./model.bin --name "…" --model-type llm …
a3net-model-catalog list
a3net-model-catalog search "cyberpunk"
a3net-model-catalog serve  --host 0.0.0.0 --port 8080
```

## 使用(Usage)

```rust
let catalog = Arc::new(ModelCatalog::open("models.db").await?);
let provider = ModelProvider::new(catalog.clone());

let metadata = ModelMetadata::new("llama3-8b", ModelType::Llm)
    .with_author("Meta")
    .with_description("Llama 3 8B chat model")
    .with_tags(vec!["chat".into(), "instruct".into()])
    .with_architecture("llama3")
    .with_quantization(Quantization::Q4("K_M".into()))
    .with_license("LLAMA-3")
    .with_source_url("https://huggingface.co/meta/llama3-8b");

let manifest = provider.publish_model("llama3-8b-instruct-q4_k_m.gguf", metadata).await?;
println!("Published: id={} hash={}…", manifest.id, &manifest.content_hash[..16]);
```

```rust
// 列出全部 LLM
let page = catalog.list(ModelFilter {
    model_type: Some(ModelType::Llm),
    ..Default::default()
}).await?;
for m in page.items {
    println!("{} v{} ({} bytes)", m.name, m.version, m.size_bytes);
}

// 全文搜索
let hits = catalog.search("cyberpunk").await?;
assert!(hits.iter().any(|m| m.tags.contains(&"cyberpunk".into())));
```

```rust
// 软删除 / 硬删除
provider.remove_model(&manifest.id).await?;     // AVAILABLE → REMOVED
provider.delete_model(&manifest.id).await?;    // REMOVED + 期望 blob 回收
```

```rust
// 拉取部署用的下载 ticket
let ticket = catalog.get_ticket(&manifest.id).await?.unwrap();
assert!(ticket.starts_with("iroh://blob/"));
```

## 应用案例(Use Cases / Examples)

1. **家庭 NAS 当作社区模型源。** 用户的家里有一台 24 小时开机的 Mac mini,跑着 `a3net-model-catalog serve`,把 `~/.cache/huggingface/hub/` 里的几十个 GGUF 全部 publish 出去。同事在地铁里用 `a3net-model-catalog search "qwen"` 就能看到 ticket,后台 P2P 拉取,Bao 校验保证分毫不差。
2. **离线实验室的可重现部署。** 实验室的实验脚本里 `a3net-model-catalog list --model-type lora` 拿到当前可用的所有 LoRA,按 `Quantization::Q4` 过滤再用 `get_ticket` 拼出 `BlobTicket`,丢进 docker base image。CI 跑回归时不需要从中央站点下载,完全本地闭环。
3. **公司内网 model 商店 + 信誉。** 内部 P2P 网络上,IT 管理员把通过审计的若干个 Llama / BGE 模型发布出去,把 `ProviderReputationTracker` 集成进 SSO 控制台,reject 任何 trust tier < 0.3 的 provider。一切更新经过 gossip,manifest 与历史比对,blake3 不匹配直接拒绝消费。

## 许可

MIT OR Apache-2.0
