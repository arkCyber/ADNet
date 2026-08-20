# CI 性能测试集成指南

本指南说明如何在 CI/CD 流程中集成性能基准测试。

---

## 📋 概述

我们使用 GitHub Actions 自动运行性能基准测试，跟踪性能指标，并检测性能回归。

### Workflow 文件

- **文件**: `.github/workflows/benchmarks.yml`
- **触发条件**:
  - Push 到 `main` 或 `develop` 分支
  - Pull Request 到 `main` 或 `develop` 分支
  - 每天 UTC 02:00 自动运行
  - 手动触发

---

## 🚀 功能特性

### 1. 自动基准测试运行

在 CI 中自动运行 4 种基准测试：
- **吞吐量测试** (Throughput)
- **延迟测试** (Latency)
- **大批量测试** (Large Batch)
- **并发测试** (Concurrent)

### 2. 结果持久化

- 保存基准测试结果为 GitHub Artifacts
- 保留期: 90 天
- 格式: JSON + JSONL

### 3. PR 评论

自动在 Pull Request 中添加基准测试结果评论，包含：
- 测试日期和时间
- Git commit SHA
- 分支名称
- 测试结果摘要

### 4. 性能回归检测

对 Pull Request 进行性能回归检测：
- 与基线（main 分支）对比
- 标记显著性能下降
- 阈值可配置（默认 10%）

### 5. 每日性能报告

每天生成详细的性能报告：
- 性能趋势分析
- 历史数据对比
- 异常检测

---

## 🛠️ 使用方法

### 自动触发

#### Push 到主分支
```bash
git push origin main
```

#### 创建 Pull Request
基准测试会自动运行，并在 PR 中添加评论。

#### 每日自动运行
无需手动操作，系统会在每天 UTC 02:00 自动运行。

---

### 手动触发

1. 进入 GitHub 仓库
2. 点击 **Actions** 标签
3. 选择 **Performance Benchmarks** workflow
4. 点击 **Run workflow**
5. 选择分支
6. (可选) 选择基准测试持续时间倍数
7. 点击 **Run workflow** 确认

---

## 📊 查看结果

### 方法 1: GitHub Artifacts

1. 进入 Actions 运行页面
2. 找到对应的 workflow 运行
3. 向下滚动到 **Artifacts** 部分
4. 下载 `benchmark-results-<commit-sha>`
5. 解压查看 JSON 和 Markdown 文件

### 方法 2: PR 评论

在 Pull Request 页面查看自动添加的基准测试结果评论。

### 方法 3: 命令行

```bash
# 下载最新的 artifact
gh run download --name benchmark-results-<commit-sha>

# 查看摘要
cat summary.md

# 查看详细结果
cat benchmark_latest.json | jq .
```

---

## 📈 结果格式

### summary.md

```markdown
## 📊 Benchmark Results

**Date**: 2026-08-21 02:00:00 UTC
**Commit**: abc123def456
**Branch**: main

### Latest Results
\`\`\`json
{
  "timestamp": "2026-08-21T02:00:00Z",
  "git_commit": "abc123def456",
  "version": "0.1.0",
  "throughput": {
    "total_messages": 10000,
    "elapsed_ms": 5234,
    "msg_per_sec": 1910.5
  },
  "latency": {
    "samples": 1000,
    "p50_ms": 12.3,
    "p95_ms": 45.6,
    "p99_ms": 89.2
  }
}
\`\`\`
```

### benchmark_latest.json

最新一次完整的基准测试结果（JSON 格式）。

### benchmark_history.jsonl

历史基准测试结果（JSONL 格式，每行一个结果）。

---

## 🔧 配置

### 修改触发条件

编辑 `.github/workflows/benchmarks.yml`:

```yaml
on:
  push:
    branches:
      - main
      - develop
      - feature/*  # 添加 feature 分支
  schedule:
    - cron: '0 2 * * *'  # 修改为每天 14:00
```

### 修改基准测试参数

在 workflow 中调整测试参数:

```yaml
- name: Run throughput benchmark
  run: |
    cargo test --package a3chat-app \
      --features iroh \
      --release \
      -- \
      benchmarks_throughput \
      --nocapture \
      --ignored
  env:
    BENCHMARK_DURATION: 5  # 设置持续时间
    BENCHMARK_MESSAGES: 50000  # 设置消息数
```

### 配置性能回归阈值

在 `compare-performance` job 中设置:

```bash
REGRESSION_THRESHOLD=10  # 10% 性能下降即视为回归
```

---

## 🔍 性能回归检测

### 工作原理

1. **获取基线**: 从 `main` 分支下载最新的基准测试结果
2. **比较指标**: 对比当前 PR 的结果与基线
3. **计算差异**: 计算百分比变化
4. **判断回归**: 如果性能下降超过阈值，标记为回归
5. **生成报告**: 创建详细的比较报告

### 示例输出

```markdown
## ⚠️ Performance Regression Detected

| Metric | Baseline | Current | Change |
|--------|----------|---------|--------|
| Throughput | 2000 msg/s | 1700 msg/s | -15% ⚠️ |
| P95 Latency | 45 ms | 48 ms | +6.7% ✅ |
| P99 Latency | 89 ms | 95 ms | +6.7% ✅ |

**Verdict**: ⚠️ Throughput regression exceeds 10% threshold
```

---

## 💡 最佳实践

### 1. 定期监控

- 查看每日基准测试报告
- 跟踪性能趋势
- 设置性能告警

### 2. PR 审查

- 在合并前检查基准测试结果
- 调查任何显著的性能变化
- 记录预期的性能影响

### 3. 基准测试维护

- 定期更新基准测试代码
- 添加新的性能关键路径测试
- 清理过时的测试

### 4. 结果分析

- 使用 JSONL 历史数据进行趋势分析
- 识别性能模式和异常
- 与代码变更关联

---

## 🐛 故障排查

### 问题: 基准测试失败

**原因**: 测试超时或崩溃

**解决方案**:
1. 检查测试日志
2. 增加超时时间
3. 减少测试规模

### 问题: Artifact 未生成

**原因**: 基准测试未生成结果文件

**解决方案**:
1. 确保基准测试代码调用了 `save_to_file`
2. 检查文件路径
3. 验证权限

### 问题: PR 评论未显示

**原因**: GitHub token 权限不足

**解决方案**:
1. 确保 workflow 有 `pull_requests: write` 权限
2. 检查 `GITHUB_TOKEN` 配置

---

## 📚 相关文档

- [基准测试持久化指南](BENCHMARK_PERSISTENCE_GUIDE.md)
- [P1 RPC 方法文档](P1_RPC_METHODS_DOCUMENTATION.md)
- [集成测试指南](P2_1_INTEGRATION_TEST_COMPLETION_2026-08-21.md)

---

## 🎯 下一步优化

### 短期 (1-2周)
- [ ] 实现真实的性能回归检测逻辑
- [ ] 添加更多基准测试场景
- [ ] 优化基准测试运行时间

### 中期 (1个月)
- [ ] 集成性能可视化仪表板
- [ ] 自动性能告警
- [ ] 多平台基准测试 (Linux/Mac/Windows)

### 长期 (3个月+)
- [ ] 历史趋势分析工具
- [ ] A/B 性能对比
- [ ] 生产环境性能监控集成

---

**文档版本**: 1.0.0  
**最后更新**: 2026-08-21  
**维护者**: A3Net Team
