# a3net-agent

> A3Net AI Agent 桥接层:provider-agnostic 的 `ChatModel` + `Tool` + `Agent` 编排 / Provider-agnostic AI agent seams — ChatModel, Tool, and orchestration loop for A3Net nodes.

## 概览(Overview)

`a3net-agent` 是 A3Net 与"AI 代理"对话的"接缝层"。它**故意不**包含任何 LLM 提供商 SDK(不在这里引入 `openai` / `anthropic` / `rig` / `genai` 等),而是把 LLM 抽象成 `ChatModel` trait、把工具调用抽象成 `Tool` trait,并提供一个轻量的 `Agent` 循环驱动两者。

中间层的方式可以让上层(节点 UI / CLI / 网关)灵活地接入 OpenAI、Anthropic、本地 Ollama,或者在测试里塞 `MockChatModel`,无需修改节点代码。本 crate 同时附带:

- 一个 **Ollama 适配器 (`OllamaModel`)**:通过 `http://localhost:11434/api/chat` 调用本地模型,支持工具调用;
- 一组 **Mock 实现 (`MockChatModel` / `MockTool`)**:无网络,可在 `cargo test` 中跑 agent 回路;
- 一个 **Agent 循环 (`Agent` / `AgentHandle`)**:收到 `ToolCall` → 跑 `Tool::invoke` → 把 `ToolCallResult` 反馈给模型,循环直到 `FinishReason::Stop` 或 `max_steps` 截止。

## 特性(Features)

| 名称 | 描述 |
|------|------|
| `ChatModel` trait | `model_id()` + `async complete(ChatRequest) -> ChatResponse` |
| `ChatRequest` / `ChatResponse` | messages + tool schemas + 平铺键值对参数 |
| `Tool` trait + `ToolRegistry` | 名称 → JSON-Schema → async invoke,线程安全 |
| `Agent` / `AgentHandle` | 工具调用循环,产量化为 `AgentEvent` 流 |
| `OllamaModel` | 即开即用,连接本地 Ollama 完成真实工具调用 |
| `MockChatModel` / `MockTool` | 离线测试使用 |
| `RunOptions::allowlist` | 节点侧过滤:模型只看到允许的工具 |
| `RunOptions::max_steps` | 防止无界循环(默认 16) |

## 安装(Installation)

`a3net-agent` 是 A3Net workspace 的 path 依赖。直接 `use` 即可:

```rust
use a3net_agent::{
    Agent, ChatMessage, ChatModel, ChatRequest, Tool, ToolContext, ToolRegistry,
    RunOptions,
};
```

CLI 旁路入口:`a3net` 主命令行不经 Agent,但任何 host 进程(Node UI、gateway、CLI 子命令)都可以把 `Arc::new(OllamaModel::new(Default::default()))` 注入到节点。

## 使用(Usage)

```rust
use std::sync::Arc;
use a3net_agent::{Agent, ChatMessage, ChatModel, ToolRegistry, RunOptions};

// 1. 准备一个 ChatModel(Ollama / Anthropic / Mock 都行)
let model: Arc<dyn ChatModel> = Arc::new(a3net_agent::mock::MockChatModel::text("hello"));

// 2. 准备工具注册中心
let registry = ToolRegistry::new();
registry.register(a3net_agent::mock::MockTool::echo("echo"));

// 3. 装配 Agent 并跑
let agent = Agent::new(model, registry);
let handle = agent.handle();
handle.push_user("hi");
let (resp, _rx) = handle.run(RunOptions::default()).await?;
println!("{}", resp.content);
```

```rust
// 4. 自定义 Tool:实现 Tool trait
use a3net_agent::{Tool, ToolContext, ToolDescriptor, ToolResult};
use async_trait::async_trait;

struct TimeTool;
#[async_trait]
impl Tool for TimeTool {
    fn name(&self) -> &str { "now" }
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "now".into(),
            description: Some("Return current unix timestamp".into()),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }
    async fn invoke(&self, _args: serde_json::Value, _ctx: ToolContext) -> ToolResult {
        Ok(serde_json::json!(chrono::Utc::now().timestamp()))
    }
}
```

```rust
// 5. 桥接到本地 Ollama
use a3net_agent::providers::OllamaModel;
let model = Arc::new(OllamaModel::new(Default::default()));
println!("{}", model.model_id()); // "qwen3"
```

```rust
// 6. 限制模型只能看到部分工具
let opts = RunOptions {
    allowlist: vec!["now".into()],
    ..Default::default()
};
let (resp, _rx) = handle.run(opts).await?;
```

## 应用案例(Use Cases / Examples)

1. **节点内置的"自然语言设个闹钟"。** 用户在 A3Net 节点 UI 里输入"两小时后叫我起床",Agent 看到 `ToolRegistry` 里有 `set_alarm(at_epoch_secs)` 工具,模型返回 `tool_call`,Tool 把任务塞进 `a3net-smarthome` 的场景调度器,UI 反馈"已经设好"。整个过程不暴露任何 OpenAI / Anthropic SDK 依赖,host 端可以无副作用替换底层模型。
2. **企业自托管 LLM 路由。** 客服机器人在公司内网运行 Ollama + Qwen3,在 `OllamaModel::new(cfg)` 里把 `base_url` 指向公司 gateway,`RunOptions::allowlist` 限定只能调用 `crm.lookup_*` 这几个工具,所有非白名单调用直接被 Agent 拒绝,模型也不可能拿到 `fs.write` 这种危险工具。审计日志由 host 落到 `a3net-observability`。
3. **离线单元测试。** 不接任何 LLM,只把 `MockChatModel::scripted(..., vec![...])` 编排的几段 `ChatResponse` 塞进 Agent,搭配 `MockTool::echo`,就可以端到端验证"模型请求 → 工具调用 → 结果回填 → 模型继续"四条消息的历史是否正确。CI 不依赖网络,且每次都是确定性输出。

## 许可

MIT OR Apache-2.0
