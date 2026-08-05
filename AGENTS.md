# AI 行为约束（自动加载）

> taiji Rust 重构规则清单。BCP-蓝图-完型协议.md 是唯一事实，本文件是实施避坑补充。

---

## 0. BCP 首要规则

- **先更新 BCP，后执行修改**：任何涉及模块结构、类型设计、接口契约、数据流的变更必须先更新 `BCP-蓝图-完型协议.md`。
- 纯内部实现细节（bug 修复、测试补全、重构不改变接口）无需更新 BCP。
- BCP 与代码冲突时 BCP 优先；实现层命名不一致时以代码为准，不修改蓝图。

## 1. 项目结构与关键约定

### Rust 项目
- **语言**: Rust 2024 edition，单 crate 项目 `taiji`。
- **构建**: `cargo build`（预期 19+ 个 vendor cfg 警告，忽略即可）。
- **测试**: `cargo test`（144 pass, 9 ignored, 5 doc-test ignored）。单个测试: `cargo test <test_name>`。
- **格式化**: 未配置 cargo fmt / clippy。
- **Vendor**: Rig v0.39 本地化在 `vendor/`，通过 `Cargo.toml` 的 `[patch.crates-io]` 重定向。`cargo package --allow-dirty` 可验证 vendor 自恰性。**不要直接修改 vendor 目录，除非明确需要修补 Rig 源码。**

### 配置文件
- 配置来源**仅配置文件**（不读环境变量），搜索顺序: `.taiji/config.json` → `taiji.config.json`。
- `api_key` 为空是硬错误。
- CLI: `taiji run <desc...>` / `taiji init` / `taiji trace <id>` / `taiji list` / `taiji status` / `taiji mcp` / `taiji serve`。

### 命名不一致（已知）
- 蓝图 V12 已统一命名为 **归藏 (Guizang)**，但代码中仍大量使用旧名 **理络 (Liluo)**：`LiluoClient`、`liluo` 变量名、注释中的"理络"。
- **写入新代码时必须使用 `GuizangClient` / `guizang` / "归藏"**。只在修改已有旧代码时保留旧命名。

## 2. TPN 循环防护

- `BACK_TO_TPN` 递增 `round_counter`，达 `max_rounds` 时只能返回 PASS/FAIL，禁止再跳转。
- `BACK_TO_META` 递增 `cycle_counter`，达 `max_cycles` 时只能返回 PASS/FAIL。
- `recursive_decompose` 创建子 Agent 前必须检查 `depth < max_depth`（默认 2），超限返回错误。
- 子任务数量上限 `max_subtasks`（默认 4），超出截断。
- `CancellationToken` 必须通过 `child_token()` 传递到所有递归层级，子任务 spawn 前和内部执行前都需检查取消信号。
- 每层递归结构同构：权重更新→概率拟合→因果验证，唯一变量是 depth。
- 子任务并发执行使用 `JoinSet::spawn` 替代 `Vec<JoinHandle>`，用 `join_next()` 流式收集（最快优先），任何子任务失败时 `abort_all()` 清理剩余任务。

## 3. Agent 关键约束

### AgentMode（重要）
- `AgentMode` 是 `Orchestration` | `Execution`，**不由 depth 自动推导**，由父 LLM 在 `SubtaskSpec.mode` 中显式分配。
- depth=0 固定 Orchestration；depth+1 >= max_depth 时 `RecursiveDecomposeTool` 强制覆盖为 Execution。
- `TpnCycle.execute()` 必须接收 `mode: AgentMode`，并逐层向下传播到 FittingAgent 和 CausalAgent。
- FittingAgentBuilder 构造时接收 mode，据此选择 system prompt 模板——不允许运行时动态切换。
- **`RecursiveDecomposeTool` 仅在 Orchestration 模式下注册**：Execution 模式 FittingAgent 不注册此工具，LLM 不可调用。工具内部同时有 mode guard 兜底（belt-and-suspenders）。这同时也防止了 WorkerPool 信号量死锁（Execution 模式持有 permit 不应再尝试获取更多 permit）。
- `registered_tool_names` 用于从 LLM 响应中提取 `tools_used`，必须与工具的实际注册状态一致：Execution 模式不包含 "recursive_decompose"。

### System Prompt 动态编排
- MetaAgent 查询归藏 `prompts/` 层，标签匹配 + 置信度排序，LLM 编排三份 prompt（fitting/verify/converge）。
- 无归藏资产或编排失败时降级为 Base 硬编码模板。

### 四象温度默认值
| 模板 | 默认 temperature |
|------|:---:|
| FittingAgent Orchestration | 0.8 |
| FittingAgent Execution | 0.5 |
| CausalAgent verify | 0.2 |
| CausalAgent converge | 0.2 |

## 4. 工具注册与安全

### FittingAgent 工具接线顺序
严格顺序：`hook()` → `.tool(static_tool)` → `.tools(dyn_tools)` → `.build()`

- 静态工具（`RecursiveDecomposeTool`、`CausalVerifyTool`）通过 `.tool()` 注册。
- `RecursiveDecomposeTool` **仅 Orchestration 模式注册**：Execution 模式分支不调用 `.tool(recursive_decompose)`，防止 LLM 看到此工具。
- 动态工具（L1 Skills）通过 `.tools(Vec<Box<dyn ToolDyn>>)` 注册。
- Rig 有 `impl<T: Tool> ToolDyn for T` blanket impl，实现 `Tool` 后自动获得 `ToolDyn`，无需重复实现。

### 内置 L1 Skills（真实实现）
`read`、`write`、`bash`、`search`、`webfetch` 均为真实实现（参考 pi_agent_rust 算法，适配 tokio 运行时），位于 `src/agents/tools/skills/` 各模块文件中。FittingAgent 自动注册这 5 个 SkillTool 为 Rig 工具，同时支持前端 agent 通过 MCP ExternalContext 注入额外上下文。

### SafetyHook 拦截
- `check_file_path`: 拦截 `../`、`~`、`/etc/passwd` 等路径穿越
- `check_exec_command`: 拦截 `rm -rf`、`eval`、`sudo` 等
- `check_web_url`: 拦截 localhost / 127.0.0.1 / 内网地址（SSRF）
- 白名单 MCP 服务器工具放行，非白名单强制执行安全检查

## 5. 归藏 (Guizang) 文件系统

### 目录布局
```
.taiji/knowledge/
├── prompts/     ← L5 提示词
├── truths/      ← L4 约束
├── models/      ← L2 贝叶斯经验（V22 起使用）
├── skills/      ← L1 可执行工具
└── index.yaml   ← tag 反向索引（衍生数据，自动维护）
```

- V22 起归藏**三层化**：`prompts/` `truths/` `models/` 为资产层，`skills/` 为工具层；`grids/`（L3 推理角色）及 GridAsset 已删除，代码 `ensure_dirs()` 只创建 truths/models/skills/prompts 四目录，新增资产层需同步修改 `ensure_dirs()`。

- TPN 执行期间**只读**，DMN Consumer 是唯一写者。
- `save_asset()` 前必须 `load_asset()` 确认版本不冲突，写入时 `version++`。
- `index.yaml` 损坏时从原始 YAML 重建。`index.yaml` 只含 `tag_index`（V22 起 `dependency_index` 已删除）。

### 深层递归产物传递
- 产出目录必须使用绝对路径。父层 deliverables 注入子 `YangPrompt.parent_deliverables`（只读）。
- 子 deliverables 向上聚合到 `DecomposeResult.deliverables`。
- Causal 验证模板必须要求 LLM 用 read 工具逐文件验证。

## 6. 错误处理与测试

### 错误处理
- `TaijiError` 变体必须携带 `context: String`。
- LLM 调用失败重试 3 次 → 降级 → `TaijiError::LLMCallFailed`。
- 归藏 I/O 失败重试 3 次 → `TaijiError::KnowledgeStoreUnavailable`。
- 文件系统 I/O 错误直接返回，不重试。
- async 上下文中禁止 `panic!` / `unwrap()`，全部用 `Result`。

### 测试注意事项
- 测试中创建的临时目录用 `tmp_dir`（非 `_tmp_dir`），测试末尾必须 `remove_dir_all` 清理。
- 依赖文件系统 I/O 的测试标有 `#[ignore]`。
- 通用运行所有测试: `cargo test`；运行特定模块: `cargo test --lib <module>`。

### 重构后清理
- 重构 agent 代码后必须检查并移除旧的 `use` 导入、死变量、未读字段。本次实现后留下的 6 个警告表明这类残留容易积累，应当通过编译警告逐一清除。

## 7. 状态持久化与崩溃恢复

### 恢复优先级链
TpnCycle 恢复历史时严格按此顺序：
`resume_history`（显式传入） > `decompose_result.json` > `checkpoint.json`
- `resume_history` 非 None 时直接使用，忽略文件。
- 无 resume_history 时尝试从 `children/<idx>/decompose_result.json` 恢复。
- 仅有 `checkpoint.json` 时走崩溃恢复逻辑：加载检查点，skip 已完成阶段。

### 检查点生命周期
- 每个阶段（MetaDone / FittingDone / VerifyDone）完成后原子写入 `checkpoint.json`。
- **PASS** 时写入 `decompose_result.json`，然后**删除** `checkpoint.json`。
- **FAIL** 时保留 `checkpoint.json` 供后续崩溃恢复。
- **rerun** 时必须显式删除旧 `checkpoint.json`，防止陈旧状态干扰。
- Cancellation 时跳过检查点写入（保持磁盘状态不被部分写入污染）。

### Chat::chat() 与历史持久化
- 需要支持对话历史恢复的 Agent 必须使用 `Chat::chat()` 而不是 `Prompt::prompt`，因为后者不返回完整对话历史。
- `run()` 中每次调用后必须将完整 `chat_history` 原子写入 `chat_history.json`。
- `chat_history.json` 本身不向上传播给调用方（通过磁盘共享），调用方通过 `load_json_optional` 按需加载。

### 子任务索引合并
- `RecursiveDecomposeTool` 扫描 `children/` 目录时使用 `BTreeMap` 保持有序，现有最大索引记为 `max_existing`。
- 新子任务索引 = `max_existing + 1 + 在本次新增子任务列表中的位置`，确保不与已有索引冲突。
- 传入 converge 的 `child_results` = 旧结果 + 新结果合并后的完整列表。

### 原子写入约定
- 所有关键状态文件（checkpoint、chat_history、verify_state、converge_state、meta_conversation、decompose_result）必须使用 `save_json_atomic` 写入。
- 模式：序列化为 JSON 字符串 → 写入 `.tmp` 临时文件 → `fs::rename` 到目标路径，防止部分写入导致文件损坏。

## 8. 前端 (taiji-web，纯 Web)

### 架构与构建验证基线
- 前端项目 `taiji-web/`：React 18 + TS + Vite，**纯 Web 架构**。构建命令：`cd taiji-web && npm run build`（脚本为 `tsc --noEmit && vite build`，tsc 0 错误 + vite build 成功；`npm run typecheck` 可单独跑 tsc）。
- 前端运行方式：核心 `taiji serve` 命令（`src/main.rs`，axum 0.8 静态托管 `taiji-web/dist` + 独立 WS 监听 17890 + xdg-open 自动开浏览器）。开发时 `npm run dev` 起 vite，经 wsClient 直连 17890。
- 核心依赖：axum 0.8（ws 特性）/ tower-http 0.6（fs 特性）/ tower 0.5 已加入 Cargo.toml；**修改核心 lib 公共 API 时以前端 TS 消费方为回归对象**。
- 核心回归基线：`cargo test --lib` = 142 passed / 0 failed / 9 ignored（V24 新增 3 个 chat 测试）；前端相关改动后不得低于此基线。基线从 161 下降属 BCP V22 预期（删除 22 个 grid/relation 测试）。

### WS 广播与事件总线
- WS 服务器固定端口 **17890**（`src/ws/server.rs`，tokio-tungstenite），前端 WS 客户端默认连接该端口，不得随意更改。
- 核心事件统一经 `src/orchestration/event_bus.rs` 的 `OnceLock` 全局总线发射（TaskCreated/TaskStatusChanged、PhaseChanged/TpnRouteDecision、ChildSpawned/ChildCompleted），事件发射必须非阻塞且失败仅告警，不得影响 TPN 主流程。
- **不经过 WS 的事件不收**：17890 端口上的入站消息全部走 `src/ws/handler.rs` 的 6 个处理函数（execute_task/submit_review/list_tasks/get_task_tree/get_tpn_state/chat_message），禁止新增绕过 handler 的消息分支。

### WS 双向协议（ClientMessage / ServerResponse）
- 请求方向：前端发 `ClientMessage`（`{type, requestId, payload}`）；响应/事件方向：`ServerResponse`（`{type, requestId, status, payload}`）。
- **requestId 关联**：每个请求携带唯一 `requestId`，`src/ws/server.rs` 用 mpsc 定向通道将响应送回对应连接（`select!` 复用 broadcast 与 mpsc），前端 wsClient 按 requestId 匹配 promise。服务端主动推送的事件型 ServerResponse（如 taskCreated/phaseChanged）requestId 为空字符串。
- 超时约定：请求响应默认 30s 超时；chatMessage 对话类请求走 `CHAT_TIMEOUT_MS = 120s`（LLM 推理慢），不得使用默认超时。
- 断线重连：wsClient 断线后 3s 自动重连；重连成功后 **outbox 补发**未收到响应的请求（幂等前提：execute_task 依赖服务端去重，不得重复派发同一 requestId 的任务）。

### 前端 wsClient 使用约定
- 前端只经 `src/lib/wsClient.ts` 单例（请求-响应式 `send` + `onEvent`/`onStatusChange` 订阅）访问核心，禁止任何组件直接 new WebSocket 或复用 fetch/HTTP 调核心接口。
- 组件通过 `hooks/useWebSocket.ts` 订阅单例事件；`useTaskTree.ts` 中 **TaskCreated 事件 parentId=null 驱动根任务切换**（根任务树只有一个，新根出现即整体替换），`useTpnState.ts` 消费 GetTpnState 轮询/响应。
- 前端 TS 消息类型定义在 `src/types/index.ts`（含 ServerResponse 联合类型），字段名 camelCase，与 Rust `ClientMessage`/`ServerResponse` 序列化字段严格一致，新增消息类型两端同步。

### 前后端类型对齐
- 前端消费类型在 `src/types/frontend.rs` 中定义，必须使用 `#[serde(rename_all = "camelCase")]` 序列化；前端 TS 接口字段名（camelCase）必须与 Rust 序列化后字段严格一致，新增字段两端同步。

### V24 聊天升级（ChatAgent + 流式协议）
- **WS 流式聊天协议**：`ClientMessage::ChatMessage` 带 `sessionId`/`contextTaskId`（无 session 时服务端 Uuid 生成并经 `stream_done` 帧的 `data.sessionId` 返回前端）；流式响应为中间帧 `{requestId, ok, chunk}`（文本增量）+ 最终帧 `{requestId, ok, data:{text, sessionId}, chunk:"", streamDone:true}`；`chunk`/`stream_done` 均 `skip_serializing_if`，非聊天请求 JSON 不变；chatMessage 走 `CHAT_TIMEOUT_MS = 120s`，不得用默认 30s。
- **ChatAgent 会话历史**：`{data_root}/chat/{session_id}.json` 原子写入（`save_json_atomic`）；历史经 Rig `stream_chat` 的 `FinalResponse.history()` 回填，为 None 时手动 push user+assistant 消息；新代码命名用 `GuizangClient`（`LiluoClient as GuizangClient` 别名仅允许出现在 chat.rs 主代码与测试各 1 处，其余文件既有旧名不改）。
- **多 Provider**：`LlmConfig.providers: Vec<ProviderEntry>`——`name="deepseek"` 或无 `base_url` 走 deepseek map，其余走 OpenAI 兼容 map（`base_url` 必填否则 `LLMCallFailed`）；`ChatAgentBuilder` 经 `resolve_chat_provider` 双 map 解析。
- **任务感知**：`build_system_prompt` 注入 `context_task_id` 对应 `meta.json`（description/status/depth）+ 归藏摘要（L5 prompts top-3 confidence 排序 + L4 active truths 前 5，knowledge 目录缺失时降级）。

## 9. BCP V22 精简产物（grid/relation 移除）

- V22 已删除且**不得再引用**的模块与类型：`src/infra/relation_engine.rs`、`src/orchestration/grid_rewire.rs`、`src/orchestration/propagation_engine.rs`，类型 `GridAsset` / `Relation` / `ReasoningPath` / `Chain`，字段 `justification_depends_on`，以及 `index.yaml` 的 `dependency_index`、`traverse_relations()`、`build_reasoning_paths()`。
- `PlanSummary.reasoning_path_summary` 已改名为 `matched_prompts_summary`（归藏 L5 Prompt 匹配摘要），旧字段名只允许出现在注释性历史说明中。
- `EvolutionReport.grids_rewired` 是保留的兼容字段（V22 δ₃ 删除后恒为 0），只允许读取/透传，不允许重新引入 grid 重连逻辑。
- 删除模块后必须清理残留：用残留检查模式 `ReasoningPath|GridAsset|grid_rewire|RelationEngine|dependency_index|justification_depends_on` 扫 `src/` + `taiji-web/src/`，注释性历史说明可豁免；同时清除编译警告（unused import / dead_code 字段）。

## 10. WorkerPool 错误路径与测试数据污染（V25 实测前置收尾）

- `WorkerPool::execute()` / `acquire()` 返回 `Result<T, TaijiError>`：semaphore 关闭（permit 永久丢失）时返回 `TaijiError::WorkerPoolUnavailable { context }` 而非 panic——async 上下文禁止 `expect()`/`panic!` 打崩 `taiji serve` 进程；`new()` 的 `assert!(max_concurrent > 0)` 保留（同步构造期快速失败）。
- 调用方（如 `RecursiveDecomposeTool` spawn 循环内）acquire 失败时，必须先 `join_set.abort_all()` 清理已 spawn 子任务，再传播错误（`?` 前无 abort 会悬挂子任务）。
- 测试不得把 task_dir 指向**已跟踪**的 `test_data/` 目录：trace hook 会向 `trace.jsonl` 追加记录，导致每次 `cargo test` 后 `git status` 变脏（如 fitting.rs depth-check 测试）。测试写入路径一律用 `tmp_dir` 或在 `#[ignore]` 下运行。

## 11. Meta/Causal 收集工具与安全钩子（V25）

- **带工具必有安全钩子（硬约束）**：任何注册工具的 Agent 必须挂载 SafetyHook（同一 `Arc<SafetyHook>` 单例）。MetaAgent 注册只读收集工具 read/search/webfetch；CausalAgent verify/converge 注册 read/webfetch；执行工具（write/bash/recursive_decompose/causal_verify）仅 Fitting 持有，Meta/Causal 不得注册。
- Rig agent 构建保持**一次链式调用**：`preamble(...).default_max_turns(...).hook(...).tools(...)` 一次 build 完成，不得分多次构建或中途覆盖配置；收集工具从 `SkillRegistry` 按名过滤克隆（`matches!(t.name(), "read" | ...)`），带工具 agent 的 `max_turns` 需 ≥3（Meta 默认 6，允许收集→提取工具循环）。
