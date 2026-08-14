# taiji MCP 对接契约（前端 Agent）

前端 Agent（独立 Rig 项目）通过 MCP 把「任务 + 已收集的上下文」交给 taiji 的 Zhouyi 认知引擎执行。本文档是这条对接链的完整契约，可直接转给前端 Agent 开发者。

## 1. 传输与启动

- **协议**：标准 MCP，**stdio 传输**（子进程 stdin/stdout）。
- **启动命令**：`taiji mcp`（阻塞式，stdin 关闭即退出）。
- **前置条件**：`taiji init` + `.taiji/config.json`（`api_key` 为空是硬错误）。
- **协议版本**：taiji 声明 `2025-11-25`（rmcp 3.x `ProtocolVersion::LATEST`），向后协商兼容 `2024-11-05` ~ `2026-07-28`。

## 2. 工具清单（6 个）

| 工具 | 用途 | 参数 |
|------|------|------|
| `taiji_plan` | 预执行计划（MetaAgent + LLM 结构化计划，**不进** Zhouyi 循环） | `description` |
| `taiji_run` | 执行任务（Meta → Yang → Yin 完整循环） | `description` + 可选 `max_depth` / `context` |
| `taiji_explain` | 执行后报告（读 trace/meta/deliverables 生成推理树摘要） | `task_id` |
| `taiji_trace` | 读 trace 记录 | `task_id` + 可选 `tree` / `tail` |
| `taiji_list` | 列出所有任务 | — |
| `taiji_status` | 引擎版本 / 工作区 / 任务数 | — |

## 3. 核心能力：`taiji_run` 的 `context` 注入

前端 Agent 已读的文件、已跑的工具结果、会话摘要，通过 `context` 直接喂给 taiji 内部 LLM——省去 taiji 重复 read/webfetch，省 token 且上下文更准。这是 MCP 相对 CLI（只能传一句描述）的核心优势。

```json
{
  "description": "任务描述",
  "max_depth": 2,
  "context": {
    "files": [{"path": "src/types/task.rs", "content": "..."}],
    "tool_results": [{"tool": "read", "output": "..."}],
    "session_summary": "会话摘要"
  }
}
```

## 4. 客户端接入（Rig 0.39+，零桥接代码）

Rig 从 0.39 起内置 MCP 客户端（`rmcp` feature），无需手写桥接层。

```rust
use rig::tool::{rmcp::McpClientHandler, server::ToolServer};
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;

// 1. 共享工具服务器
let handle = ToolServer::new().run();

// 2. MCP client handler：连接 + 自动拉工具 + 监听 list_changed 自动刷新
let handler = McpClientHandler::new(ClientInfo::default(), handle.clone());
let _svc = handler
    .connect(TokioChildProcess::new(Command::new("taiji").arg("mcp")))
    .await?;

// 3. 挂到 agent
let agent = client.agent(model)
    .tool_server_handle(handle)
    .build();
```

依赖：`rig = { version = "0.39", features = ["rmcp"] }`（0.40/0.41 同样支持）。

## 5. 版本兼容

| 端 | rmcp 版本 |
|---|---|
| taiji MCP server | `rmcp 3.1.2` |
| 前端 Agent（rig 0.39+ 的 `rmcp` feature） | `rmcp ^1.7` |

协议层向后协商（taiji 支持 2024-11-05 ~ 2026-07-28 全版本），无 major 版本握手风险。已实测：initialize 握手 + `tools/list` 返回 6 个工具。

## 6. 角色定位

前端 Agent 是**编排入口 + 上下文收集**；**认知循环（Meta/Yang/Yin 三相）始终在 taiji 内部**——不要把 taiji 当普通工具池，它自己会递归拆解执行。`max_depth` 可安全传给 `taiji_run` 控制递归深度。
