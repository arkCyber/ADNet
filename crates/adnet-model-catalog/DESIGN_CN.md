# ADNet AI 模型分发网络设计文档

## 项目概述

ADNet AI 模型分发网络是一个基于 P2P 技术的去中心化 AI 模型分发系统，利用 ADNet 的 Iroh 基础设施实现模型的安全、高效分发。

## 核心设计理念

### 1. 为什么需要 P2P 分发？

传统 AI 模型分发面临以下挑战：

| 问题 | 传统方案 | P2P 方案 |
|------|----------|----------|
| **下载速度** | 依赖服务器带宽，单点瓶颈 | 多源并行下载，带宽聚合 |
| **服务器成本** | 需要大量服务器和带宽 | 去中心化，边际成本趋近零 |
| **地理延迟** | 全球用户访问单点延迟高 | 就近下载，边缘缓存 |
| **可用性** | 服务器故障导致服务中断 | 无单点故障，自动冗余 |
| **隐私** | 数据经过中心服务器 | 点对点直连，端到端加密 |

### 2. 为什么选择 Iroh？

Iroh 是新一代 P2P 网络库，提供：

- **Bao 验证**: BLAKE3 认证组织，确保内容完整性
- **NAT 穿透**: 自动打洞，防火墙友好
- **DERP 中继**: 可靠的 NAT 回退机制
- **Ticket 机制**: 自包含的下载凭证
- **WASM 支持**: 浏览器原生运行

## 系统架构

### 3.1 整体架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           用户层 (User Layer)                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐          │
│  │   Web 浏览器     │    │   桌面客户端     │    │   移动端 App     │          │
│  │  (Iroh WASM)   │    │   (ADNet CLI)   │    │   (ADNet SDK)   │          │
│  └────────┬────────┘    └────────┬────────┘    └────────┬────────┘          │
│           │                      │                      │                    │
└───────────┼──────────────────────┼──────────────────────┼────────────────────┘
            │                      │                      │
            ▼                      ▼                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                           网关层 (Gateway Layer)                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────┐        │
│  │                    模型目录 Web 服务                              │        │
│  │  ┌──────────────┐  ┌──────────────┐  ┌────────────────────┐   │        │
│  │  │  模型浏览 UI   │  │   搜索 API   │  │   下载管理器        │   │        │
│  │  └──────────────┘  └──────────────┘  └────────────────────┘   │        │
│  └─────────────────────────────────────────────────────────────────┘        │
│                                    │                                        │
│  ┌─────────────────────────────────┼─────────────────────────────────┐      │
│  │                    索引数据库 (SQLite)                              │      │
│  │         模型元数据 │ 标签 │ 搜索索引 │ 下载统计                    │      │
│  └─────────────────────────────────┼─────────────────────────────────┘      │
└────────────────────────────────────┼────────────────────────────────────────┘
                                     │
                                     ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                          P2P 网络层 (P2P Network Layer)                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐ │
│  │  Provider   │◄──►│  Provider   │◄──►│   Peer      │◄──►│   Peer      │ │
│  │   Node      │    │   Node      │    │   Node      │    │   Node      │ │
│  │  (NAS)      │    │  (NAS)      │    │  (Desktop)  │    │  (Mobile)   │ │
│  └──────┬──────┘    └──────┬──────┘    └──────┬──────┘    └──────┬──────┘ │
│         │                   │                   │                   │          │
│         │    ┌──────────────┴──────────────┐   │                   │          │
│         │    │         Iroh 网络          │   │                   │          │
│         │    │  ┌───────────────────────┐ │   │                   │          │
│         │    │  │  Blobs (模型数据)     │ │   │                   │          │
│         │    │  │  Gossip (模型发现)    │ │   │                   │          │
│         │    │  │  DERP (NAT 中继)     │ │   │                   │          │
│         │    │  └───────────────────────┘ │   │                   │          │
│         │    └───────────────────────────┘   │                   │          │
│         │                                     │                   │          │
└─────────┼─────────────────────────────────────┼───────────────────┼──────────┘
          │                                     │                   │
          ▼                                     ▼                   ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                          存储层 (Storage Layer)                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐                     │
│  │   NAS 存储   │    │   本地磁盘   │    │   缓存存储   │                     │
│  │  (源头供种)  │    │ (用户下载)   │    │ (边缘节点)   │                     │
│  └─────────────┘    └─────────────┘    └─────────────┘                     │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 3.2 数据流向

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                           模型分发流程                                         │
└──────────────────────────────────────────────────────────────────────────────┘

┌─────────────┐    1. 导入模型    ┌─────────────┐    2. 生成 Ticket  ┌─────────────┐
│   NAS       │ ─────────────────►│  Blob Store │ ─────────────────►│  Catalog   │
│  (Provider) │                   │  (Iroh)     │                   │  (SQLite)  │
└─────────────┘                   └─────────────┘                   └──────┬──────┘
                                                                           │
                          4. 浏览下载                      3. 广播模型          │
                                  │                          ▲               │
                                  ▼                          │               │
┌─────────────┐    6. 传递 Ticket    ┌─────────────┐          │               │
│   用户       │ ◄────────────────── │   Web 服务   │ ────────┴───────────────┘
│  (Consumer) │                    │   (Catalog)  │
└──────┬──────┘                    └─────────────┘
       │
       │ 7. P2P 下载
       ▼
┌─────────────┐    8. 多源并行下载     ┌─────────────┐    9. 校验完整性     ┌─────────────┐
│  Iroh       │ ◄────────────────── │   多 Peer    │ ─────────────────►│ Bao 验证   │
│  Downloader │                     │   供种       │                    │  (BLAKE3)  │
└──────┬──────┘                     └─────────────┘                    └─────────────┘
       │
       │ 10. 写入本地存储
       ▼
┌─────────────┐
│  本地模型    │
│  可用！      │
└─────────────┘
```

### 3.3 Iroh Ticket 结构

Iroh Ticket 是自包含的下载凭证，格式如下：

```
iroh://blob/<NodeId>/<Hash>[?offset=<start>&end=<end>][#integration=<type>]
```

示例：
```
iroh://blob/ndn7jua4htynj3vk6gfnas667w7aiois7gzmwjurp6p36d7c53q/abc123...deh456
```

组件说明：

| 组件 | 说明 |
|------|------|
| `blob` | 协议类型，表示 blob 传输 |
| `NodeId` | 节点公钥（Base32 编码） |
| `Hash` | BLAKE3 内容地址 |
| `offset` | 可选：范围下载起始位置 |
| `end` | 可选：范围下载结束位置 |

## 核心组件

### 4.1 ModelCatalog

SQLite 数据库，存储模型元数据。

```rust
pub struct ModelCatalog {
    // 模型列表
    models: Vec<ModelManifest>,

    // 全文本搜索索引 (FTS5)
    fts_index: FtsIndex,

    // 标签索引
    tag_index: TagIndex,

    // 下载统计
    download_stats: DownloadStats,
}
```

### 4.2 ModelProvider

模型发布者，将模型导入 P2P 网络。

```rust
pub struct ModelProvider {
    // 本地 blob 存储
    blob_store: Arc<dyn Store>,

    // 模型目录
    catalog: Arc<ModelCatalog>,

    // Gossip 广播
    gossip: Option<Arc<Gossip>>,
}

impl ModelProvider {
    // 发布模型到网络
    pub async fn publish_model(&self, path: &Path, metadata: ModelMetadata) -> Result<ModelManifest>;

    // 广播模型可用性
    pub async fn announce_model(&self, manifest: &ModelManifest) -> Result<()>;
}
```

### 4.3 ModelDownloader

模型下载器，支持多源并行下载。

```rust
pub struct ModelDownloader {
    // 下载目录
    download_dir: PathBuf,

    // Blob 存储
    blob_store: Arc<dyn Store>,

    // 活跃下载追踪
    active_downloads: HashMap<String, DownloadProgress>,
}

impl ModelDownloader {
    // 下载模型
    pub async fn download(&self, ticket: &str) -> Result<DownloadHandle>;

    // 暂停/恢复下载
    pub fn pause(&self, id: &str) -> Result<()>;
    pub fn resume(&self, id: &str) -> Result<()>;
}
```

### 4.4 ModelDiscovery

Gossip 发现服务，自动发现网络中的模型。

```rust
pub struct ModelDiscovery {
    // Gossip 连接
    gossip: Arc<Gossip>,

    // 已知的 providers
    known_providers: HashMap<NodeId, ProviderInfo>,

    // 模型缓存
    model_cache: HashMap<Hash, ModelAnnouncement>,
}
```

## 数据库模式

### 5.1 Models 表

```sql
CREATE TABLE models (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    version         TEXT NOT NULL,
    model_type      TEXT NOT NULL,
    size_bytes      INTEGER NOT NULL,
    content_hash    TEXT NOT NULL,  -- BLAKE3, 64 hex chars
    iroh_ticket     TEXT NOT NULL,
    author          TEXT NOT NULL,
    description     TEXT NOT NULL,
    tags            TEXT NOT NULL,  -- JSON array
    architecture    TEXT NOT NULL,
    quantization    TEXT NOT NULL,  -- JSON object
    license         TEXT NOT NULL,
    source_url      TEXT,
    status          TEXT NOT NULL DEFAULT 'available',
    download_count  INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

-- FTS5 全文搜索
CREATE VIRTUAL TABLE models_fts USING fts5(
    name, description, author, tags,
    content='models',
    content_rowid='rowid'
);
```

### 5.2 Downloads 表

```sql
CREATE TABLE downloads (
    id                  TEXT PRIMARY KEY,
    model_id            TEXT NOT NULL,
    started_at          TEXT NOT NULL,
    completed_at        TEXT,
    bytes_downloaded    INTEGER NOT NULL DEFAULT 0,
    total_bytes         INTEGER NOT NULL,
    status              TEXT NOT NULL DEFAULT 'pending',
    error_message       TEXT,
    sources             TEXT,  -- JSON array of peer sources
    FOREIGN KEY (model_id) REFERENCES models(id)
);
```

### 5.3 Providers 表

```sql
CREATE TABLE providers (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    node_id         TEXT NOT NULL,
    address         TEXT NOT NULL,
    is_local        INTEGER NOT NULL DEFAULT 0,
    last_seen       TEXT NOT NULL,
    total_models    INTEGER NOT NULL DEFAULT 0
);
```

## Web UI 设计

### 6.1 页面结构

```
/
├── index.html          # 首页 - 分类浏览
├── models/             # 模型列表
│   └── [id].html      # 模型详情
├── search.html         # 搜索结果
└── api/                # REST API
    ├── models          # 模型 CRUD
    ├── stats           # 统计数据
    └── tags            # 标签列表
```

### 6.2 UI 组件

| 组件 | 功能 |
|------|------|
| ModelCard | 模型卡片，显示缩略信息 |
| ModelDetail | 模型详情页 |
| SearchBar | 搜索框，支持全文搜索 |
| TagCloud | 标签云 |
| DownloadProgress | 下载进度条 |
| CategoryNav | 分类导航 |

### 6.3 响应式设计

- **桌面端**: 3-4 列网格布局
- **平板端**: 2 列网格
- **移动端**: 单列卡片，底部导航

## 安全考虑

### 7.1 内容完整性

- 所有模型使用 BLAKE3 哈希
- Bao 验证确保传输完整性
- Ticket 包含完整地址，防止中间人攻击

### 7.2 隐私保护

- 端到端加密传输
- 无中心服务器数据泄露
- 节点间直连

### 7.3 访问控制

- 可选的认证机制
- 模型可设置为公开/私有
- 下载权限可配置

## 性能优化

### 8.1 下载优化

- **多源并行**: 同时从多个 peer 下载
- **断点续传**: 支持暂停和恢复
- **智能缓存**: 热数据边缘缓存

### 8.2 索引优化

- **FTS5**: 全文搜索索引
- **分页**: 大列表分页加载
- **预加载**: 滚动时预加载更多

### 8.3 网络优化

- **连接池**: 复用 P2P 连接
- **拥塞控制**: 自适应速率
- **NAT 穿透**: 直连优先，中继备选

## 部署场景

### 9.1 个人使用 (单机)

```bash
# 在 NAS 上运行
adnet-model-catalog serve --host 0.0.0.0 --port 8080

# 通过局域网访问
http://192.168.1.100:8080
```

### 9.2 社区分享 (多节点)

```
┌────────────────────────────────────────────────────────┐
│                   社区模型网络                         │
│                                                        │
│   ┌────────┐                                           │
│   │ Node A │ ─── Gossip ─── ┌────────┐                 │
│   │ (NAS)  │                │ Node B │                 │
│   └────────┘                │ (NAS)  │                 │
│       │                     └────────┘                 │
│       │                           │                    │
│       └────────── Gossip ─────────┘                    │
│                           │                             │
│                           ▼                             │
│                    ┌────────────┐                       │
│                    │ Web Gateway │ (可选，统一入口)      │
│                    └────────────┘                       │
└────────────────────────────────────────────────────────┘
```

### 9.3 企业部署 (高可用)

```
                    ┌─────────────┐
                    │   负载均衡   │
                    └──────┬──────┘
                           │
         ┌─────────────────┼─────────────────┐
         │                 │                 │
         ▼                 ▼                 ▼
    ┌─────────┐       ┌─────────┐       ┌─────────┐
    │ Gateway │       │ Gateway │       │ Gateway │
    │ Node 1  │       │ Node 2  │       │ Node 3  │
    └────┬────┘       └────┬────┘       └────┬────┘
         │                 │                 │
         └─────────────────┼─────────────────┘
                           │
                    ┌──────┴──────┐
                    │   SQLite    │
                    │  (复制模式)  │
                    └─────────────┘
```

## 未来规划

### Phase 1: 基础功能 ✅

- [x] 模型元数据存储
- [x] Iroh blob 集成
- [x] 基本下载功能
- [x] Web UI

### Phase 2: 增强功能 🔄

- [ ] 多源并行下载
- [ ] Gossip 发现
- [ ] 下载进度追踪
- [ ] 搜索优化

### Phase 3: 高级功能 📋

- [ ] 模型评分系统
- [ ] 增量更新
- [ ] WASM 客户端
- [ ] IPNS 命名

### Phase 4: 生态系统 📋

- [ ] HuggingFace 导入
- [ ] 模型推荐
- [ ] 社区治理
- [ ] 商业变现

## 附录

### A. 参考资料

- [Iroh 文档](https://iroh.computer/)
- [Bao 格式](https://bao-tree.org/)
- [ADNet 架构](../ARCHITECTURE.md)

### B. 术语表

| 术语 | 说明 |
|------|------|
| Provider | 模型提供者，持有模型并供种 |
| Consumer | 模型消费者，下载使用模型 |
| Ticket | 下载凭证，包含所有连接信息 |
| Blob | Iroh 中的内容寻址存储单元 |
| Bao | BLAKE3 认证组织格式 |
| Gossip | 流行病广播协议 |
| DERP | 分布式中继协议 |
