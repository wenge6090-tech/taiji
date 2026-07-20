# 实现计划：pi_agent_rust（前端）+ taiji（认知引擎）MCP 桥接

## 目标

taiji 不直接做 agent CLI——改为通过 MCP 协议向 pi_agent_rust 暴露认知推理能力。pi 拥有所有对外能力（界面/工具/会话/扩展），taiji 只做一件事：接收上下文 → TPN 循环 → 返回收敛结论。

## 通讯模型

```
pi_agent_rust (前端 Agent CLI)
  │
  │  MCP stdio (pi 作为 MCP client, taiji 作为 MCP server)
  │
  ├─ taiji_run:    深度推理任务
  ├─ taiji_trace:  查询推理轨迹
  ├─ taiji_list:   列出历史任务
  └─ taiji_status: 引擎健康检查
         │
         ▼
taiji (认知引擎 —— 只做推理，不碰工具/界面)
  ┌─────────────────────────────────┐
  │ TPN 循环 (MCP 每次调用为一次)   │
  │  MetaAgent → FittingAgent →     │
  │  CausalAgent → PASS/BACK        │
  └─────────────────────────────────┘
```

## 现状

| 组件 | 状态 |
|------|------|
| `src/mcp/server.rs` | ✅ 已实现 4 个工具（`rmcp`） |
| `src/mcp/client.rs` | ✅ 已实现 MCP client manager |
| `taiji mcp` CLI | ✅ stdio transport，阻塞等待连接 |
| L1 占位 Skills | ⚠️ 仍注册在 FittingAgent 中 |
| `taiji_run` context | ⚠️ 只接受 `description` 字符串 |

## Phase 1：taiji_run 增强 — 接收 pi 上下文

### 1.1 类型扩展

```rust
// src/types/agent.rs — 新增
/// pi_agent_rust 通过 MCP 传递的上下文
pub struct ExternalContext {
    /// 文件列表（pi 的 read 工具读取的文件内容）
    pub files: Vec<ExternalFile>,
    /// pi 执行工具的结果
    pub tool_results: Vec<ExternalToolResult>,
    /// 会话摘要（pi 的对话历史简要）
    pub session_summary: Option<String>,
}

pub struct ExternalFile {
    pub path: String,
    pub content: String,
}

pub struct ExternalToolResult {
    pub tool: String,
    pub output: String,
}
```

### 1.2 MCP tool schema 更新

```rust
// src/mcp/server.rs — list_tools() 中 taiji_run 的参数 schema
Tool::new(
    "taiji_run",
    "Execute a task via the TPN cognitive engine.",
    Arc::new(object_schema(serde_json::json!({
        "description": {
            "type": "string",
            "description": "Natural-language task description"
        },
        "context": {
            "type": "object",
            "description": "Optional external context from the calling agent",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "content": {"type": "string"}
                        }
                    }
                },
                "tool_results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": {"type": "string"},
                            "output": {"type": "string"}
                        }
                    }
                },
                "session_summary": {"type": "string"}
            }
        }
    }))),
)
```

### 1.3 上下文注入到 TPN 循环

在 `handle_run()` 中解析 context 参数：
1. 将 `context.files` 写入 `task_dir/context/files/`（FittingAgent 的 read skill 可读）
2. 将 `context.tool_results` 序列化为 context JSON 文件
3. 将 `context.session_summary` 注入 FittingAgent 的 system prompt（作为任务背景）

### 1.4 FittingAgent 模板更新

Execution 模板新增段落：
```
## External Context (provided by parent agent)

The following context was collected by the agent that delegated to you:
- {session_summary}
- {tool_results_summary}

Files are available at: <task_dir>/context/files/
Use the `read` tool to inspect them if needed.
```

### 1.5 文件位置

| 改动 | 文件 |
|------|------|
| `ExternalContext` / `ExternalFile` / `ExternalToolResult` | `src/types/agent.rs` |
| `taiji_run` schema 扩展 | `src/mcp/server.rs` |
| context 物化到 task_dir | `src/mcp/server.rs::handle_run()` |
| context 注入 FittingAgent prompt | `src/agents/fitting.rs` |

---

## Phase 2：剥离 L1 占位 Skills

### 2.1 移除占位实现

当前 FittingAgent 注册的 5 个 L1 Skills 从代码中移除：

```
移除: src/agents/tools/skills/mod.rs 中 FittingAgent 的 skills 注册代码
保留: SafetyHook 拦截逻辑（pi 的工具执行在 pi 侧，不影响 taiji 的安全模型）
```

### 2.2 FittingAgent 剩余工具

移除 Skills 后，FittingAgent 仅保留：
- `RecursiveDecomposeTool`（任务拆解）
- `CausalVerifyTool`（因果验证）

这两个是 TPN 循环的内建能力，不依赖外部。

### 2.3 硬编码模板调整

FittingAgent 模板中移除"你可以使用以下工具: read/write/bash/search/webfetch"段落。
Execution 模板改为指引 LLM 使用 `context/files/` 中的已有文件进行推理。

### 2.4 文件位置

| 改动 | 文件 |
|------|------|
| 移除 Skills 注册 | `src/agents/tools/skills/mod.rs` |
| 模板去 Skills 引用 | `src/agents/fitting.rs` |

---

## Phase 3：BCP 更新 V12→V13

### 3.1 变更要点

1. **系统概览新增**：taiji 作为 MCP 认知引擎角色（不再独立做 agent CLI）
2. **L1 Skill 定义调整**：L1 Skills 由前端 agent（pi）提供，taiji 不内置
3. **技术栈更新**：新增 `rmcp`（MCP 协议实现）
4. **架构总纲图更新**：入口从 `taiji run <description>` 改为 `taiji mcp`（pid/trace 来自 pi）
5. **§8 流程调整**：新增"pi 上下文注入"步骤在 MetaAgent 之前

### 3.2 不变的部分

- TPN 三元循环结构
- 归藏 5 层仓库
- 异层同构递归
- AgentMode 规则
- DMN Consumer
- 动态提示词注入

---

## Phase 4：pi_agent_rust 侧集成指南（非 taiji 代码）

> 此章节为 pi 侧实现的规格说明，不在此仓库实现。

### 4.1 MCP 连接配置

pi 需添加 MCP server 配置（`.pi/config.toml` 或类似）：

```toml
[mcp_servers.taiji]
command = "taiji"
args = ["mcp"]
# taiji mcp 命令在 pi 的工作目录下启动
# 自动发现 .taiji/knowledge/ 归藏仓库
```

### 4.2 路由策略

pi 的 LLM system prompt 中增加：
```
When the task requires deep analysis, multi-step reasoning, causal verification,
or architectural assessment, use the `taiji_run` tool.

Provide:
- description: a clear statement of what to analyze
- context: relevant files and tool results you've already collected
```

### 4.3 调用示例

```json
// pi → taiji MCP request
{
  "tool": "taiji_run",
  "arguments": {
    "description": "Analyze the concurrency safety of this Rust project's async code",
    "context": {
      "files": [
        {"path": "src/orchestration/tpn_cycle.rs", "content": "..."},
        {"path": "src/agents/fitting.rs", "content": "..."}
      ],
      "tool_results": [
        {"tool": "bash", "output": "cargo build: 19 warnings"},
        {"tool": "grep", "output": "Found 12 uses of tokio::spawn"}
      ],
      "session_summary": "User asked about Rust async best practices. We found several tokio::spawn calls..."
    }
  }
}

// taiji → pi response
{
  "task_id": "abc123",
  "content": "Converged: The project's concurrency model is sound...",
  "deliverables": ["/data/tasks/abc123/deliverables/analysis.md"],
  "depth": 1,
  "rounds": 3
}
```

---

## 依赖顺序

```
1. types/agent.rs          ← ExternalContext 类型定义
2. mcp/server.rs           ← taiji_run schema 扩展 + context 解析
3. agents/fitting.rs       ← 模板加 context 注入段落
4. agents/tools/skills/    ← 移除 L1 Skills 注册
5. agents/fitting.rs       ← 模板去 Skills 引用
6. BCP V13 更新           ← 架构蓝图归档
```

**Phase 1+2 可以并行**（类型定义 → 两路独立改动）。

---

## 验收标准

- [ ] `cargo build` 零错误
- [ ] `cargo test` 124 passed, 0 failed
- [ ] `taiji_run` MCP tool 接受 `context` 可选参数
- [ ] context.files 物化到 `task_dir/context/files/`
- [ ] context.session_summary 注入 FittingAgent system prompt
- [ ] 无 context 参数时行为不变（向后兼容）
- [ ] L1 占位 Skills 不再注册到 FittingAgent
- [ ] FittingAgent 保留 recursive_decompose + causal_verify
- [ ] BCP V13 反映新角色定位
