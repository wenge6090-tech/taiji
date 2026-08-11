# AI 行为约束（自动加载）

> taiji Rust 规则清单。BCP-蓝图-完型协议.md 是唯一事实，本文件是实施避坑补充。

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

### AgentMode（V27 阴阳配对模式，重要）
- `AgentMode` 是 `Orchestration` | `Execution`，**不由 depth 自动推导**：由 MetaAgent 权重更新时按**递归层数规则 + 任务难易程度**决策（BCP §8.8）——根节点与 BACK_TO_META 重跑时 MetaAgent LLM 决策；子节点由父 LLM 在 `SubtaskSpec.mode` 中按难度分配（编排模板教学）。
- 深度规则兑底：`depth+1 >= max_depth` 时 `RecursiveDecomposeTool` 强制子任务为 Execution；`TpnCycle.apply_leaf_depth_rule()` 对根任务/崩溃恢复/BACK_TO_META 路径同样强制 Execution（两条路径互为镜像）。
- **阴阳配对**：Orchestration 节点——阳 Agent 编排模板（拆解+综合）、阴 Agent 收敛模板（converge）；Execution 节点——阳 Agent 执行模板（直接产出）、阴 Agent 验证模板（verify）。降级模板按 `MetaContext.mode` 选：`VERIFY_ORC/EXEC_SYSTEM_PROMPT`、`CONVERGE_ORC/EXEC_SYSTEM_PROMPT`。
- `TpnCycle.execute()` 逐层传播 `MetaContext.mode`（不再有独立 mode 参数）：FittingAgentBuilder 构造时读 meta_ctx.mode 选模板，CausalAgent verify/converge 读 meta_ctx.mode 选配对模板。
- **`RecursiveDecomposeTool` 仅在 Orchestration 模式注册**：Execution 模式 FittingAgent 不注册此工具，LLM 不可调用。工具内部同时有 mode guard 兜底（belt-and-suspenders，Execution 模式调用直接拒绝）。这同时也防止了 WorkerPool 信号量死锁（Execution 模式持有 permit 不应再尝试获取更多 permit）。
- `registered_tool_names` 用于从 LLM 响应中提取 `tools_used`，必须与工具的实际注册状态一致：Execution 模式不包含 "recursive_decompose"。

### System Prompt 动态编排
- MetaAgent 查询归藏 `prompts/` 层，标签匹配 + 置信度排序，LLM 编排三份 prompt（fitting/verify/converge）。
- 无归藏资产或编排失败时降级为 Base 硬编码模板。

### 四象温度默认值
| 模板 | 默认 temperature |
|------|:---:|
| FittingAgent Orchestration（编排） | 0.8 |
| FittingAgent Execution（执行） | 0.5 |
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
TpnCycle 恢复历史时严格按此顺序（V28 产出继承，BCP §1.4 / §8.18）：
`deliverables/（含 handoff.md）` > `decompose_result.json` > 重跑
- 有交接文件（`deliverables/handoff.md`）时从产出重建（执行事实是唯一记忆），**不再从 chat_history 重放作为结果事实来源**。
- `resume_history`（显式传入）仅作本节点断点续聊的最终兜底（省 token），不跨层传播。
- 无交接文件时尝试从 `children/<idx>/decompose_result.json` 恢复子任务结果。
- 仅有 `checkpoint.json` 时走崩溃恢复逻辑：加载检查点，skip 已完成阶段。
- 旧语义（resume_history > decompose_result > checkpoint 全链重放）已废弃。

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
- 所有关键状态文件（checkpoint、chat_history、verify_state、meta_ctx、decompose_result）必须使用 `save_json_atomic` 写入。
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

## 12. LLM 结构化输出解析（V25 冒烟实测修复）

- **LLM 响应解析一律走 `src/infra/json_util.rs` 的 `parse_llm_json<T>`**，禁止直接 `serde_json::from_str` 解析 LLM 输出文本（冒烟实测发现 LLM 常在 JSON 前后输出叙述文本或包 ` ```json ` 围栏，直接解析抛 `StructuredOutputParseFailed` 导致 TPN 任务失败）。
- `parse_llm_json` 四级容错顺序（勿改动）：①直接 `from_str` → ②` ```json ` 围栏提取 + 内容内首尾大括号切片 → ③全文首个 `{` 到最后一个 `}` 切片 → ④返回原始完整文本的解析错误（调用方包装 `StructuredOutputParseFailed` 时保留 Raw 供诊断）。
- 覆盖范围：MetaAgent（MetaContext）、CausalAgent verify/converge（VerificationReport/ConvergenceDecision）、PlanBuilder（PlanSummary）；新增 LLM 结构化输出解析点必须复用此函数。`serde_json::from_str` 仅允许用于非 LLM 输出（trace/ws/配置文件等本地数据）；causal.rs 文档注释中的旧示例为历史说明可豁免，不得照抄。

## 13. tools_used 真实记录 / 对话快照 / causal max_turns（V26.1 修复轮）

- **tools_used 统计唯一来源是 TraceHook**：FittingAgent 的 `tools_used` 必须读 `TraceHook::tools_called()`（`on_tool_call` 收集的真实工具调用，去重 + 首调顺序），**禁止**对 LLM 响应文本做工具名 contains 匹配——LLM 正文提及工具名会产生伪阳性（V26.1 修复）。
- **ChatHistorySnapshotHook**：FittingAgent 在 safety → trace 之后挂载（`hook()` 链尾），实现 `PromptHook::on_completion_call`，每次 LLM 调用前将完整对话（调用前 `history` + 本轮 `prompt`，`Vec<Message>` 格式与 `chat_history.json` 一致）经 `save_json_atomic` 原子快照到 `{task_dir}/chat_history.json`——弥补 Rig `chat()` 出错即提前返回、不回写历史的缺陷。快照写失败仅 `warn!`，不得中断 agent 运行。
- **对话历史可增量恢复**：`--resume` / 子任务 rerun 恢复 Fitting 阶段时，若 `chat_history.json` 非空则从该快照继续（失败点增量推进），而非从空历史重跑整个 Fitting 阶段。
- **CausalAgent verify/converge `max_turns` 默认 10**（3→6→10 演进：T4 实测 3 轮不足、E2E 实测 6 轮仍溢出 MaxTurnsError），代码默认值与 `.taiji/config.json` 的 `causal.max_turns` 保持一致。
- **hook 测试用 `src/hooks/test_support.rs` 的 `TestCompletionModel`**：rig 的 `test_utils` 需要 feature 且 vendor 不可改，新增实现 `PromptHook` 的 hook 若要测试 `on_tool_call` / `on_completion_call`，复用该测试模型（`PromptHook::<TestCompletionModel>::on_completion_call(...)` 显式调用）。

## 14. 任务目录持久化文件清单与死文件禁令（V26.2 清理轮）

- **任务目录持久化文件清单唯一事实在 BCP §8.1（9 项）**：`meta.json` / `checkpoint.json` / `meta_ctx.json` / `chat_history.json` / `verify_state.json` / `decompose_result.json` / `deliverables/` / `children/` / `trace.jsonl`。新增持久化文件必须先入清单（含写者/读者/用途），禁止绕过清单直接引入。
- **禁止引入只写不读的持久化文件**：V26.2 已删 `meta_conversation.json` 与 `converge_state.json`——前者四字段全部可推导（task_description→meta.json、llm_input/llm_response→trace.jsonl、meta_ctx→meta_ctx.json），后者崩溃窗口由「父任务失败→重跑→children/ 复用→重新 converge」幂等重放天然覆盖。写新持久化文件前必须确认存在读者。
- **`MetaAgentBuilder` 无 `task_dir` 字段**（V26.2 删除，仅为写 meta_conversation.json 存在），不要重新引入。
- **converge 决策不持久化**；`verify_state.json` 是唯一保留的 Causal 状态文件（CausalAgent.verify 写，TpnCycle VerifyDone 恢复消费），不得随 converge 一起删除。

## 15. V26.3 修复轮（abort 落盘 / 工具契约 / 脱敏精确化 / 规模感知）

- **abort 子任务状态落盘**：`RecursiveDecomposeTool` 错误路径 `abort_all()` 后必须调用 `mark_aborted_children_failed`，把 `children/` 下 Running 子任务原子写 Failed（写失败仅 `warn!`，不阻断父错误传播）——中止不产生虚假 Running 残留。
- **L1 Skills 参数契约**：SkillTool 暴露单参 `input`，双形式——纯字符串（直通单参工具主参数）或 JSON 对象字符串（`serde_json::from_str` 二级解析后按键分发）；BashTool 读 `command` / ReadTool 读 `path` / SearchTool 读 `query` / WebfetchTool 读 `url`，必须兼容 `input` 键直读；工具 description 必须含用法示例（`input_desc`）。write 是双参（path+content）必须 JSON 对象形式。新增 SkillTool 必须遵守此契约。
- **trace 脱敏精确化**：value-based 脱敏仅限带前缀密钥模式（`sk-`/`ds-`/`ghp_` + 20 字符、`AKIA` + 16 大写字母数字），禁止通用 `{40,}` 长字符串匹配（误伤正常代码/长文本）；key-based 脱敏（api_key/token/secret/password 键名）保留。
- **Base 模板规模感知**：FittingAgent Base 模板含规模引导（大任务优先拆解分批、预算不足时明确覆盖范围而非无限重试），归藏资产模板不受影响。

## 16. 测试临时目录唯一性（V26.1-3 验证轮实测教训）

- **测试辅助函数使用的临时目录必须每次调用唯一**：并行测试若共享 pid 基路径（如 `taiji_tpn_factory_{pid}`），两个测试并发时一方 `remove_dir_all` 会删除另一方初始化中的目录（`LiluoClient::new` 内部 rename index 文件）→ 偶发 `KnowledgeStoreUnavailable { failed to rename index file }`（全量 `cargo test` 5 次中约 1 次复现，单独跑永远通过）。修复模式：静态 `AtomicUsize` 计数器拼唯一子目录（参考 `tpn_cycle.rs::build_factory`），测试末尾照常 `remove_dir_all` 清理。
- 新增会创建临时目录/临时文件的测试，先检查目标路径是否被同一进程内其他测试共享；pid 基路径 ≠ 唯一路径。

## 17. Rig 0.39 hook 单槽语义（V26.4 E2E 冒烟实测：FittingAgent 三个 hook 只有链尾生效）

- **`AgentBuilder::hook()` 是单槽覆盖式**：每次调用直接替换先前注册的 hook（builder 内 `hook: Some(hook)`），`.hook(a).hook(b).hook(c)` 最终只有 `c` 生效——不是追加列表。FittingAgent 必须经 `FittingHookSet`（`src/hooks/fitting_hook_set.rs`，safety → trace → snapshot 组合，一次 `.hook()` 挂载）转发全部 `PromptHook` 方法；禁止再写 `.hook().hook()` 链式挂载。
- **历史教训（本次修复的隐性 bug）**：V25 起 FittingAgent 链式挂载导致 SafetyHook 从未真正生效（V26.1 加 snapshot 后 TraceHook 也失效）——E2E 冒烟表现为 `trace.jsonl` 缺失 + `tools_used` 为空，单测无法覆盖（rig builder 单槽语义只在真实构建路径暴露）。任何 agent 新增第二个 hook 时，先检查现有挂载点是否单槽。
- **FittingHookSet 转发语义**：按 safety → trace → snapshot 顺序调用，首个非 Continue 动作短路返回——SafetyHook 拒绝的违规工具调用不会进入 trace 记录（`tools_called()` 不含被拒工具）；`on_invalid_tool_call` 聚合取首个非 Fail。`tools_used` 读外部保留的 `trace_hook.tools_called()`（clone 进 FittingHookSet 共享 Arc 状态）。

## 18. 脱敏单实现与崩溃恢复数据源（V26.5 自我分析修复轮）

- **trace 脱敏唯一实现是 `infra/trace.rs::TraceWriter::redact_sensitive`**（V26.3 前缀型规则：`sk-`/`ds-`/`ghp_`/`AKIA`，无 `{40,}` 通用匹配）；`hooks/trace.rs::TraceHook::redact_sensitive` 必须保持薄转发，禁止再复制实现（复制必然漂移，旧 `{40,}` 正则经 `TraceWriter::write` 二次脱敏会整段遮蔽 UUID/文件正文，V26.3 修复被完全抵消——V26.5 P1 实证）。改动脱敏规则时两端同步检查。
- **崩溃恢复的 Fitting 结果重建数据源是 `chat_history.json`，不是 trace**：TraceHook 只写 `completion_call`/`completion_response`/`tool_call::*`，任何匹配 `phase=="output"/"result"` 的恢复逻辑恒失败（V26.5 P2 实证：FittingDone 恢复恒重跑 LLM 浪费 token）。重建走 `construct_tpn_result_from_state`（tpn_cycle.rs）：content 取 chat_history 最后一条含文本的 assistant 消息，tools_used 正序遍历 trace 的 `tool_call::*` 去重（首调顺序，勿用 rev+reverse——那会变成末次出现顺序）。
- **CausalAgent verify/converge 不挂 TraceHook**（只挂 SafetyHook），其 LLM 调用与 read/webfetch 不进 trace.jsonl——崩溃恢复的 verify 行为天然不可审计，勿据此推断 Fitting 未执行；判断 Fitting 是否重跑以 RUST_LOG 日志（`Could not reconstruct ... re-running`）或 Fitting 工具调用记录（read/bash/write/recursive_decompose）为准。
- **JoinSet 超时语义**：`tokio::task::JoinSet` 被 Drop 时自动 abort 全部子任务（tokio 保证），runner 超时路径 `TpnCycle` drop 即中止子树；残余问题是 abort 不执行子任务状态落盘（meta.json 滞留 Running），Fitting 内错误路径的 `abort_all() + mark_aborted_children_failed`（V26.3）只覆盖主动错误路径，覆盖不了 timeout drop。判断"孤儿子任务"时先区分：join_next 主动收集 vs JoinSet drop 隐式 abort。
- **verify 对超大重构 content 有 MaxTurnsError 边界**：FittingDone 崩溃恢复后 verify 重新跑，若 task_output 极大（如含旧上下文的长报告）10 轮可能超限（V26.5 人工场景实证）。真实同 task_id 恢复（01:06 案例）verify 单轮通过；大输出任务建议描述控制输出规模。

## 19. 任务 ID 可读化（V26.6）

- **task_id 格式 `{简述slug}-{YYYYMMDD-HHMMSS}`**（如 `分析源码-20260807-061530`），唯一生成点在 `src/infra/task_id.rs`：slug 取描述前 24 字符路径安全化（非字母数字→`-`，含 `/ \ : . " * ?` 与空格，杜绝 `..` 穿越；空描述→`task`），时间戳本地时间秒级。**禁止在 runner/recursive_decompose/mcp 等处直接拼 task_id 或重新引入 UUID**；`generate_task_id` 本身不保证唯一，根任务必须经 `ensure_unique`（查 `tasks/` 目录，同秒同名追加 `-2/-3`），子任务追加 `-{index}`（同父并行不撞）。
- **chat session_id 保持 UUID**：`{data_root}/chat/{session_id}.json` 会话文件已持久化，session_id 不属任务 ID，`ws/handler.rs` 的 `Uuid::new_v4()` 勿改。
- task_id 为纯字符串、无 UUID 格式假设（已排查无 `len()==36`/`parse_str` 校验）；新增代码读 task_id 时按不透明字符串处理。前端 `taskId: string` 与 CLI `--resume`/`trace <id>` 自动兼容可读 ID。

## 20. 死代码甄别 / 归藏资产对齐 / 超时子任务落盘（V26.7 续修轮）

- **死代码甄别三分类**：① 真死代码（无任何引用）→ 删（V26.7 已删 `rate_limiter.rs` + governor 依赖——governor 仅 rate_limiter 使用；`TaskStatus::Decomposed` 变体无写者仅 3 处匹配 → 删变体+匹配）；② 规划未激活（BCP §8.3 承诺的架构设计）→ **保留不删**（`dmn_consumer`/`cognition_evolver`，DMN 单写者激活后使用）；③ 误判在用 → 不动（`trigger_engine`/`match_skills` 被 main/factory/tpn_cycle/chat 全部构造）。删变体前先确认无写者（grep 构造点而非匹配点）。
- **归藏 prompts 资产必须与当前 Agent 架构对齐**：V26.7 发现 6 个 prompts 资产仍是 V25 AgentMode 分裂内容（教 LLM 设置已删除的 `mode: "Execution"/"Orchestration"`、按模式差异化裁决）——资产被 MetaAgent 按标签加载后由 LLM 编排注入，过时指令会真实污染 Fitting/Causal 行为。资产更新后 `version++`；**tags 保持不变**（index.yaml 是衍生缓存，改 tags 需重建索引）。新增/修改资产前对照 `src/agents/fitting.rs::build_system_prompt` / `causal.rs::VERIFY_SYSTEM_PROMPT`/`CONVERGE_SYSTEM_PROMPT` 的 V26 语义。
- **BACK_TO_META 分支的 MetaAgent 调用标签必须与首次运行一致**（`&["general"]`）：V26.7 修复 tpn_cycle.rs BACK_TO_META 分支的 `&[]` 空标签——空标签 `search_prompts` 零匹配 → 降级空 MetaContext，元相循环永远无法注入新鲜推理偏置。
- **超时路径子任务落盘**：runner 超时分支（`tokio::timeout` Err）drop TpnCycle → JoinSet drop 隐式 abort 子任务（不走状态写路径）→ 必须在超时分支调用 `mark_aborted_children_failed(&task_dir.join("children"))`（V26.7 接线，函数在 recursive_decompose.rs 为 `pub(crate)`）。V26.3 的主动错误路径覆盖不了 timeout drop，两条路径都要落盘。

## 21. 阴阳配对模式（V27，用户框架恢复）

- **模式决策链**：MetaAgent 权重更新时按 ① 递归层数规则（builder 注入 `depth()`/`max_depth()`，LLM prompt 含 depth/max_depth 与叶节点规则）② 任务难易程度（LLM 评估）决策 `MetaContext.mode`。根节点与 BACK_TO_META 重跑由 MetaAgent 决策；子节点由父 LLM 在 `SubtaskSpec.mode` 按难度分配（编排模板教学）；`RecursiveDecomposeTool` 与 `TpnCycle.apply_leaf_depth_rule()` 按深度规则兜底强制叶节点 Execution（两条路径互为镜像，缺一不可）。
- **配对模板**：Orchestration = 阳编排模板（拆解+综合，注册 recursive_decompose）+ 阴收敛模板（CONVERGE_ORC）；Execution = 阳执行模板（直接产出，不注册 recursive_decompose）+ 阴验证模板（VERIFY_EXEC）。Fitting 降级模板按 mode 分支（`build_orchestration_prompt`/`build_execution_prompt`）；Causal verify/converge 降级模板按 `meta_ctx.mode` 选 ORC/EXEC 常量。
- **`MetaContext.mode` 是模式唯一载体**：`#[serde(default)]` = Orchestration（旧 meta_ctx.json 零迁移兼容）；`SubtaskSpec.mode` 同理默认 Orchestration。禁止重新引入独立 mode 参数链（V25 方式已废弃）——mode 随 MetaContext 传播，TpnCycle 无需逐层传参。
- **MetaAgent 降级路径（无归藏资产）仍返回 `MetaContext::empty()`（mode 默认 Orchestration）**：不做额外的 LLM 模式决策调用，叶节点强制由 `apply_leaf_depth_rule` 覆盖；若未来需要无资产也决策模式，需先入 BCP §8.8 再改。
- **归藏资产 V27 已重写为配对语义（version→3，tags 不动）**：资产按 name/tags/description 与所选模式匹配（`orchestration_*` ↔ 编排、`execution_*` ↔ 执行），PromptAsset 无 `agent_mode` 字段（勿恢复，否则需重建 index.yaml）。修改资产前对照 fitting.rs 双模板 / causal.rs 四常量的 V27 语义。
- **提示词教学 vs 注册面是双保险**：执行模板明示「你没有 recursive_decompose」是教学层；工具不注册 + 工具内 mode guard 是注册面层——LLM 结构化输出解析失败/编排异常时注册面兜底，教学层只降低 LLM 尝试调用未注册工具的频率。

## 22. 交接文件机制与上下文预算（V28/V29 实现轮）

- **交接文件 = `deliverables/handoff.md`，产出物之一**：写者 Fitting 错误路径（ContextOverflow / HardCutoff / LLMCallFailed），读者父层/verify/Meta 校准/恢复链均经 deliverables/ 既有路径发现。**禁止**引入 deliverables/ 外的独立交接文件（可发现性问题）。写失败仅 `warn!`，不阻断错误传播。
- **ContextLimiter 挂在 FittingHookSet 转发链末尾**：`on_completion_response` 累计 `response.usage.input_tokens`（provider 报告真实请求 token，含历史重放与工具结果），≥ handoff_tokens → `Terminate("context_overflow")`，≥ hard_cutoff_tokens → `Terminate("hard_cutoff")`。阈值默认 250k/300k，config `runtime.context_limits` 可覆盖。
- **Rig `HookAction::Terminate` 是 struct variant**：`Terminate { reason: String }`，不是函数调用；`HookAction` derive PartialEq 可直接断言。
- **`AgentFactory::create_fitting_agent` receiver 是 `self: &Arc<Self>`**：帮助函数/闭包接收 `&Arc<AgentFactory>`（`&self.factory` 直接传），传 `&AgentFactory` 编译失败。
- **max_turns 降级为防死循环兜底（200）**：不再承担上下文管理（V29）；Meta/Causal 同样挂 ContextLimiter 预算。`agent_overrides[*].max_turns` 配置仍可覆盖。
- **BACK_TO_TPN（含 verify 驱动与 ContextOverflow 驱动）一律清空 chat_history**：下一轮基于验证报告 + 产出文件（`build_handoff_description` 注入 deliverables/ + handoff.md）继续，禁止重放中间记忆。
- **`MetaAgentBuilder.run(description, tags, handoff)` 三参**：BACK_TO_META 时注入 `read_handoff` 内容作产出校准；首次运行/PlanBuilder 传 None。改签名需同步调用点（tpn_cycle ×2、plan.rs ×1、测试）。
- **崩溃恢复重建 Fitting 结果优先读 handoff**（`construct_tpn_result_from_state`）：有 handoff.md → content=交接全文；无 → chat_history 兜底；再无 → 重跑。
- **冒烟验证交接路径**：`runtime.context_limits` 调小（如 1200/3000）跑 `taiji run` 简单任务，观察 BACK_TO_TPN 日志 + `deliverables/handoff.md` 生成；每轮 `Current conversation depth: 1/N` 证明 chat_history 未重放。冒烟后必须恢复 config.json。
- **上限是保险丝，不是配额（用户框架）**：整个系统方向是 token 高效——少消耗、高质量。`context_limits` 是护栏：LLM 必须感知预算并主动收敛，禁止设计成"让 agent 消费到阈值才交接"。落地 = `build_budget_discipline` 把阈值数字 + 保险丝语义 + 高效引导（少工具调用/控制篇幅/完成即止/残缺产出兜底）追加进 Fitting system prompt，对归藏资产与 Base 模板两条路径统一生效（在 `build_system_prompt` 返回值后 push_str）。Causal/Meta 不注入（输入不可控/单次小调用，无操作空间）。
- **模型窗口**：deepseek-v4-flash 原生 1M tokens 上下文（2026-04 发布），250k/300k 默认阈值在窗口内有效；换模型时须确认 `context_limits` < 模型窗口，否则护栏永不触发（死配置）。
- **交接 = 压缩产物 + 编排失败证据（用户框架，V29+）**：`deliverables/handoff.md` 是上下文压缩的产物（与 Prime Agent compaction 同构：结构化摘要 + 保留执行状态），但消费方向不同（摘要跨层传给下一瞬态 agent 作恢复，而非回注入同会话）且多了失败语义（超限触发本身就是任务粒度错误 = 编排失败的硬证据，Prime Agent 无此信号——其压缩是常规操作）。**交接正文 = LLM 压缩收尾**（`compress_history_to_handoff`）：chat_history 序列化（`[User]/[Assistant]/[Tool result]`，工具结果截断 2000 字符）→ 截断到 `compress_input_tokens`（默认 20k，首部 ≤2k 保留目标 + 尾部最新状态，中间省略标记）→ 一次聚焦瞬态调用（max_turns 1 / max_tokens 2048 / temperature 0.2 / 30s 超时）→ 结构化正文（## 进度 / ## 剩余工作 / ## 决策 / ## 约束状态 / ## 已产出文件）→ `write_handoff(..., Some(body))`。**降级链**：压缩失败 / 超时 / 空输出 → `write_handoff(..., None)` 静态正文，仅 warn 不阻断。**llm_failed 路径不压缩**（同一 provider 刚失败，压缩大概率同样失败；对话通常短，静态正文 + output_refs 足够）。压缩输入构建纯函数在 `handoff.rs`（serialize_history / truncate_compress_input / build_compress_prompt，可单测）；LLM 调用在 fitting.rs（agents 层持有 provider）。
- **Rig chat() 在 hook Terminate 时不追加消息到内存 history（冒烟实证）**：ContextLimiter 超限 Terminate 早于首次 completion 时，`agent.chat()` 传入的 `&mut history` 保持空 Vec。因此：① fitting.rs 保存历史必须 `if !history.is_empty()` 才 `save_json_atomic`，否则空数组覆盖 ChatHistorySnapshotHook 已写的磁盘快照，LLM 压缩收尾输入变空（本次冒烟 debug 2 小时的根因）；② 压缩收尾输入一律读磁盘 `chat_history.json`（快照），禁用内存 history。

## 23. 分封制：任务自我认知与无降级原则（V30 实现轮）

- **任务自我认知（身份 + 地位）注入阳 Agent**：`build_identity_section(engine_ctx, meta_ctx, max_depth)`（fitting.rs 同步函数）把「身份与地位（分封制）」段 push 到 system_prompt 末尾（预算纪律后，归藏资产与 Base 模板统一生效）——身份五要素：内容（meta.json.description）、类别（meta_ctx.mode：编排/执行，元权重更新阶段确定，禁止 LLM 分类）、兄弟（会盟陈列室）、父（parent_id + 父册 description，根任务注明「根任务（天子）」）、子（subtask_ids）；地位二要素：层级（depth/max_depth）、权限（可读写本任务 deliverables/；父产出与兄弟贡品只读；中间记忆仅本节点可见）。Causal/Meta 不注入。
- **会盟 = 贡品陈列室目录注入（冒烟实证修正）**：`collect_sibling_deliverables` 注入的是兄弟任务 **deliverables/ 目录绝对路径**（BTreeMap 有序、排除自身），不是文件清单——同批并行兄弟在分封时点无产出，文件级快照恒空（冒烟实证会盟失效）；目录路径 = 动态发现入口（子任务执行中 read 随时发现陆续陈列的贡品，跨轮/rerun 同样有效）。身份段教学文案含「贡品陆续陈列，需要时用 read 工具查看」。
- **无降级原则（V30 起新代码）**：禁止降级兜底——新代码读身份册失败 / 会盟扫描失败一律 `TaijiError` 上抛（错误信息必须携带路径，诊断性），禁止 `unwrap_or_default()` / `.ok().flatten()` 吞错。「无父（根任务 parent_id=None）」与「无兄弟（children/ 不存在或空）」是**状态分支**非降级。既有降级点（MetaContext::empty、Base 模板、压缩静态正文、load_json_optional、LLM 重试）维持现状，改造另立章节——**新增代码一律先问：这个失败要不要暴露？**
- **父任务目录推导**：子 task_dir = `{父task_dir}/children/{idx}` → 父目录 = `task_dir.parent().parent()`（同构目录树保证，BCP §7.1）；推导失败（如 task_dir 畸形）→ Err 上抛。
- **`parse_task_roll`**（fitting.rs）：身份册读取/解析错误包装为 `TaijiError::Other` 携带路径（`TaijiError::IO` 是裸 io::Error 无路径，诊断性不足）。
- **身份册（meta.json）零 schema 变更**：身份五要素全部来自既有字段（description/parent_id/subtask_ids）+ MetaContext.mode——V30 不新增 Task/SubtaskSpec 字段，只加 `YangPrompt.sibling_deliverables`（serde default，meta_ctx.json 旧文件兼容）。
- **冒烟验证会盟**：跑拆解任务后检查子任务 `meta_ctx.json` 的 `yang_prompt.sibling_deliverables` 应含兄弟 `children/<idx>/deliverables` 目录（互指）。身份段本身不进 trace.jsonl（completion_call 只记本轮 user 消息），单测覆盖身份段内容。
- **能看不能写（V30 执行层强制，BCP §8.20）**：兄弟关系 = 单向观摩——read 开放（贡品公开陈列：父产出 + 兄弟贡品可读），write 封闭（封地自治：写入必须落在本任务 `task_dir` 内）。执行层 = `FittingHookSet` 的 `check_write_domain`（on_tool_call 内 safety 之后、trace 之前）：write 工具目标路径经 `normalize_path`（词法解析 `.`/`..`，不碰文件系统——目标可能尚不存在）后必须 `starts_with(task_dir)`，越界 → `ToolCallHookAction::skip` + warn，**不进 trace**（tools_called 不含被拒调用）。相对路径按 task_dir 解析（sandbox 语义永不出封地）。SafetyHook 黑名单只拦 `..`/`~`/`/etc`——绝对路径直写兄弟目录（无 `..`）不触发，域校验是唯一强制（SafetyHook 全局单例无 task_dir，域校验放 FittingHookSet 持有 per-agent task_dir）。`FittingHookSet::new` 第 5 参 task_dir；改动同步 fitting.rs 构造点与测试 make_hook_set。
- **兄弟通信汇总经父层**：子任务不直连通信——聚合 → converge → BACK_TO_TPN 下一轮注入；编排模板教学（Base `build_orchestration_prompt` + 归藏 `orchestration_fitting.yaml` version→4，tags 不动）：兄弟封地自治/拆解弱耦合/通信经父层三原则。
- **V31 失败汇报（收敛树不中断）**：子任务任务级失败**不再整体上抛**——`build_failure_entry` 构造 Diverged 条目（summary=`[{kind}] {reason}`、deliverables=子任务现存产出含 handoff.md 交接路径）进 prior_results，成功兄弟继续收集（**不 abort_all**），converge 收到完整汇报后裁决 Partial/Diverged，task_summary 输出失败分析与 rerun 建议，父阳据此 rerun_of 再启用或接受残缺综合。**保留硬中止**：取消（`self.cancel.is_cancelled()` 检查）与 join panic 仍 abort_all + mark_aborted_children_failed + 上抛。child_results 映射改走 `prior_results.iter()`（带 idx）以注入 `failure_reason`/`failure_kind`（`failure_kinds: BTreeMap<usize, String>` 局部表，Diverged 条目才有）。`classify_failure` 词汇表：context_overflow/hard_cutoff/llm_failed/cognitive/constraint_violation/io/config/cancelled/other。
- **build_failure_entry 交接产物收集失败仅 warn（有意例外）**：原始失败原因必须优先传播，叠加 IO 错误会掩盖根因——与无降级原则（§23）不冲突：无降级针对系统数据完整性（身份册/会盟），失败汇报上下文以根因优先。
- **冒烟实测（预算 3000/6000 人为压低）**：两子任务 hard_cutoff 失败 → 子任务写 handoff → 失败条目进 child_results（failure_kind=hard_cutoff + 交接路径）→ converge 运行 → 父阳读汇报采取补救（读取交接产物完成综合）——失败→汇报→再指导闭环验证。冒烟后必须恢复 config.json（250000/300000/20000）。
- **V33 验证三权分立（归藏本体论重构 MVP-1）**：verify 管线 = ConstraintEngine（truths Hard 短路）→ **ContractEngine 机械执行 verifications checks（hard 失败直接短路返回 BackToMeta，LLM 不可翻案）** → LLM 只裁决 llm_judgement 项（L2 兜底 + 反偏置指令）。`llm_judgement` 不参与机械裁决（run_checks 跳过）；**机械全过 + 有契约 + 无 llm_judgement 项 → 直接 PASS（LLM 零调用）**；无契约资产（verifications 空）→ 维持纯 LLM 验证（降级路径不改）。`CausalVerifyAgentBuilder` 经 `.guizang(Arc<LiluoClient>)` builder 方法接线（工厂 `create_causal_verify_agent` 已接；None = 未接线 → 契约层跳过仅 warn）。
- **ContractEngine 命令白名单（BCP §8.22）**：`command_succeeds` 仅允许 `COMMAND_ALLOWLIST` 前缀（cargo check / cargo test --no-run / rustc --emit=metadata）+ 禁止 shell 元字符（`&&`/`;`/`|`/`>`/`<`/`` ` ``/`$(`），命令经 `split_whitespace` 直接执行不经过 shell，30s 超时，cwd = task_dir，输出截断 2KB。新增命令进白名单须同步审查。
- **契约资产 target 路径防护**：CheckSpec.target 含 `..` 段一律拒绝（`contains_path_traversal`），契约不得离开 task_dir；glob 仅支持最后一段单段 `*`（MVP-1 简化，不新增 glob 依赖）；`reference_resolves` 用 `extract_front_matter` 解析 `---` 围栏，`field` 缺省 `output_refs`。
- **新增归藏资产层必须同步四处**：`ensure_dirs()` + `type_dir_name()` + `build_index()` 扫描数组 + `CognitiveAsset` 枚举（含 asset_type/id/version/set_version 匹配）——V33 加 verifications/ 就是这四处（历史教训：V22 grids/ 删除与 V33 verifications/ 新增都因漏改 build_index 导致索引漂移）。`search_by_tags(&[])` 返回空（for 循环不迭代），全量加载须走目录遍历（`load_all_verifications` 仿 `load_active_truths`）。
- **手写资产 YAML 的 serde 对齐**：`type: verification`（CognitiveAsset tag）、`kind`/`severity` 为 snake_case（CheckKind/CheckSeverity rename_all）、checks 项必须含 id/kind/target/params/severity/pass_condition；缺省字段靠 `#[serde(default)]` 容错。种子契约写入后字段契约用 python yaml 校验（type/kind/severity/target/params/pass_condition 六项）。
- **verify_state.json 的 checks 键兼容**：`{"report", "round", "cycle", "checks"}` 新增 checks（CheckResult 数组）——tpn_cycle 读取端只取 `state.get("report")`（serde_json::Value），加键零迁移；旧文件无 checks 键不报错。
- **replace 编辑吞代码事故教训**：edit 替换时 newText 必须完整保留 oldText 中被匹配的语义内容（本次 `Ok(prompts)` 与 search_prompts 闭合被吞导致方法嵌入函数内部、impl 未闭合——`cargo build` 报 unclosed delimiter 在文件尾）。替换涉及函数结尾/impl 闭合时，newText 先复制 oldText 的尾部结构再追加新内容。
- **V33/MVP-2 DMN 被动学习闭环**：TPN PASS 分支经 `enqueue_dmn_pending`（tpn_cycle.rs，pub(crate)）读 verify_state.json 的 checks → 原子写 `{data_root}/pending/{task_id}.json`（`{task_id, source:"tpn", checks}`，同 task_id 覆盖写幂等，I/O 失败仅 warn 不阻断 PASS）。`--with-dmn` flag（main.rs `Command::Run`）spawn DmnConsumer；MVP 时序：任务结束后 sleep 3s（消费者 1s 首扫 + 处理）再退出——正式生命周期（serve 常驻/主动学习）归 MVP-3。
- **backprop 单次执行不重试（幂等性硬约束）**：`CognitionEvolver::backprop_checks`（cognition_evolver.rs）按 check_id 匹配 `CheckSpec.stats`（n++ / pass_count+=passed）→ save_verification（version++）。**DMN Consumer 对 checks 格式 pending 单次执行**（不进入 MAX_EVOLVE_RETRIES 重试循环）——统计累加不可重复，失败直接进死信供人工诊断；仅无 checks 的旧格式走重试 evolve（δ₀-δ₂ 占位保留，MVP-3 统一重构）。backprop 是归藏统计的唯一写路径（TPN 只读归藏 §8.3 保持）。
- **一次 backprop 调用 = 一个任务的检查结果，check_id 唯一**（ContractEngine.run_checks 每 check 一个结果）；跨任务累加靠多次调用。同一回传内重复 check_id 只匹配第一个（find 语义）。新增检查项统计测试时勿构造同任务重复 check（真实流程不会发生）。
- **dmn_consumer 分发分支**：pending JSON 含 `checks` 键 → `serde_json::from_value::<Vec<CheckResult>>` 解析（失败 → 死信）→ backprop；无 checks → 既有 evolve。backprop 错误不得用 `?` 上抛（会绕过重试/死信路径直接终止 run 循环），必须作为 Result 走既有 match 分支。
- **`save_json_atomic` 是同步函数**（返回 `Result<(), std::io::Error>`），调用**不加 `.await`**（MVP-2 在 enqueue_dmn_pending 中误加导致 E0277）。写新调用点先查 infra/trace.rs 签名。
- **V33/MVP-3 契约演化闭环（§6.4/§8.21 定稿）**：`CognitionEvolver::evolve_contracts(config)` 按序 fork→merge→prune（纯符号层，零 LLM）；激活门槛（`runtime.dmn.activation_min_assets`=5 + `activation_min_samples`=50）不过则零操作；backprop 无条件（数据积累期）。dmn_consumer 分发：checks → backprop → evolve_contracts（单次，错误进死信——死信移动防重复 backprop）。`DmnConsumer::new` 第 4 参 `DmnConfig`（构造点：main.rs + 测试 ×6）。
- **fork = 严格度参数化变体（BCP 内容修订空洞的机械解）**：根资产（含 llm_judgement 项）资产级通过率 < 0.6（FORK_PASS_RATE_THRESHOLD）且采样 ≥ min_samples → 生成 `{root}-v1` 变体：llm_judgement 项 `params.strictness="strict"`（causal.rs 按档位注入「从严裁决」指令）+ **check id 重命名 `{base}@{variant}`**（backprop 全 id 精确落位变体，原资产零污染）+ stats 清零 + confidence×0.8 + variant_of 链接。防重复：已有变体的根不重复 fork；变体不 fork 变体。
- **merge 同分根优先（read_dir 顺序陷阱）**：`sort_by` 稳定排序依赖初始顺序，load_all_verifications 的 read_dir 顺序不确定——同通过率时 best 可能落到变体上，导致根契约被误 pruned（MVP-3 实测：grp-a 消失）。sort 必须加二级键：同分时 `variant_of.is_none()`（根）优先。pruned 资产被 `load_all_verifications` 过滤——测试断言 pruned 状态必须用 `load_verification(id)` 直读，勿用 load_all 查找。
- **merge 浮点边界**：0.9−0.8 的 f64 差可能 = 0.09999999999999998 < 0.1 → 差 0.1 的资产被误合并。测试数据远离边界（用 0.3/0.5 等明确差值）；真实数据 0.1 边界误合并可接受（近似相等语义）。
- **四维 CheckStats 回传**：`{ n, pass_count, cost_sum, rounds_sum, quality_sum }`——cost/rounds/quality 任务级信号（trace usage.input_tokens 求和 / verify_state.round / route×confidence）由 tpn_cycle PASS 分支摊派给同任务所有检查项（`sum_trace_input_tokens` 私有 helper 可单测）；`backprop_checks` 必须四维累加（MVP-3 曾漏 cost/rounds/quality 只加 n/pass——类型扩展与实现扩展同步检查）。CheckResult 加字段后 Rust 构造点全部要补（contract_engine run_check + 测试多处；json! 宏内勿用正则补字段——会吞 `}`）。
- **主动学习（默认关闭）**：`runtime.dmn.active_learning_enabled=false`。开启时：pick_exploration_target 选活跃**变体**资产中 UCB 探索分最大者（N=0 → f64::MAX）→ enqueue_exploration_task 写 `experiments/{asset_id}.json`（目录自动创建、队列非空防堆积、每窗口限量）→ spawn_runner 消费（RecursiveRunner 执行 → ContractEngine 机械检查变体契约 → enqueue_dmn_pending 回传 → 删文件；失败改名 `.failed`）。探索任务描述教学层含「不递归、不分解、完成即止」。
- **EvolutionReport 加 `forked/merged` 字段**（derive Default，既有构造点用 `..Default::default()` 补）；grids_rewired 保留恒 0（§9 兼容）。`VerificationAsset` 加 `status`（"active"/"pruned"，对齐 truths 先例）+ `variant_of: Option<String>`（serde default，new() 签名不变）。
- **V33/MVP-3.5 贝叶斯后验（models/ 层激活，§6.4.1）**：`bayesian_update(asset_id, success, fail, prior_confidence, prior_strength)` 5 参——load_model 无则 `ModelAsset::from_prior`（α=1+k·c, β=1+k·(1−c)）初始化 → α/β 累加 → save_model（version++）→ 返回后验均值。旧 3 参签名已废弃（evolve() 占位路径调用点用 `(…, 0.5, 10.0)` 补参）。
- **backprop 双轨**：`backprop_checks(task_id, checks, &DmnConfig)` 第三参——同一次回传 CheckStats（频率四维）与 ModelAsset（贝叶斯聚合成败）同时更新；`bayesian_enabled=false` → 仅频率；**贝叶斯失败仅 warn 不阻断**（频率是主数据已持久化，贝叶斯是增强维度）。
- **演化决策升级（§6.4.1 表）**：fork 阈值 / merge 差 / prune 判定用**后验均值 μ**；prune 的 σ 用**候选自身 Beta 后验标准差**（频率版用组内率标准差，两版并存）。无 ModelAsset（未采样）→ 纯先验回退（confidence 映射）。`evolve_contracts` 内 `asset_posterior_map` 一次构建传入三算子；决策门槛的 n 仍读 CheckStats（α/β 无法可靠反推采样数）。
- **测试陷阱：stats 与 ModelAsset 必须同步构造**：直接手工构造 VerificationAsset.stats（有 n）但不 seed ModelAsset 时，贝叶斯决策把 μ 当纯先验（c=0.5 → 0.5）——会误 prune 根资产（MVP-3.5 实测：root μ=0.5 < best(变体 0.833)−2σ → 根被淘汰）。测试模拟双轨状态须 `ModelAsset::from_prior + α/β 累加 + save_model`（参考 bayesian_tests::seed_model）。既有演化测试显式 `bayesian_enabled=false` 走频率路径保断言。
- **merge 贝叶斯合并**：根吸收候选**采样增量**（cand.α − 先验伪计数 1+k·c_cand），先验不叠加；候选无后验 → 仅频率合并（warn/debug 不中断——频率状态标记不得被 `continue` 跳过）。
- **fork 变体后验独立**：fork 时同步 `ModelAsset::from_prior(variant.id, …, variant.confidence(降权后), k)` ——变体后验与 check id 重命名机制同构（`{base}@{variant}`），回传精确落位变体。
- **主动学习探索分升级**：`pick_exploration_target(assets, weights, &BTreeMap<String, f64>)` 第三参 = 后验均值 map（enqueue 时 load_all_models 构建）；reward 的 pass 分量 = μ（无后验 → 频率回退）。调用点同步：测试用 `&BTreeMap::new()`。
- **V35/MVP-5 UCB 检索（§6.3 实现层定稿）**：`rank_prompts_by_ucb(prompts, models, C, prior_strength) -> Vec<usize>` 纯函数——`score = μ + C·√(ln N_total/(n+1))`，μ = ModelAsset 后验均值（无 model → §6.4.1 先验映射 α=1+k·c），n = stats.n；**(n+1) 平滑保证 n=0 有有限探索分**（冷启动 = 先验 μ 降序）；score 相等按 id 字典序（确定性，与 read_dir 无关）。meta.rs 调用点 `prior_strength` 硬编码 10.0（MetaAgentBuilder 无 config）——改签名需同步。
- **V35/MVP-6 prompts 对称演化**：`PromptAsset` 补 `stats: CheckStats`/`status`（"active"/"pruned"，new() 初始化 "active"）/`variant_of`/`parent_id`/`env_tags`（serde default 零迁移）。`load_all_prompts` **必须过滤非 active**（与 load_all_verifications 同语义——注释承诺与实现必须一致，实测漏过滤被测试抓出）。
- **backprop_prompts 任务级信号**（pending `assets_used`/`passed` 键，serde default 旧文件零迁移）：四维从 checks 首项摊派（同任务摊派值一致）；usage_count/success_rate 同步（兼容既有消费方）；贝叶斯双轨同构（prior=confidence，失败仅 warn）。dmn_consumer 分发顺序：assets_used → backprop_prompts（失败仅 warn 不阻断）→ checks → backprop_checks → evolve_contracts（尾接 prompts 演化，单写者串行）。
- **共享公式防漂移**：`stats_pass_rate`（CheckStats 级）是 verifications/prompts 通过率的单份实现，`asset_pass_rate` 经 `asset_stats` 聚合后调用它；阈值常量（FORK_PASS_RATE_THRESHOLD/MERGE_PASS_RATE_DIFF）两算子共用。新增算子决策公式一律走共享 helper，禁止复制公式。
- **merge/prune 测试陷阱**：候选 n 必须 ≥ min_samples 才 eligible（n=2 < 3 被门槛排除，组内只剩 1 成员 → 不合并）；测试数据远离浮点边界（0.9 vs 1.0 差恰好 0.1 不触发 `< 0.1`）。
- **prompts 演化激活门槛独立**：`activation_gate_prompts`（prompts 层自身资产数 ≥5 + 总采样 ≥50），与 verifications 独立判定；evolve_contracts 返回合并报告（pruned/forked/merged 相加）。
- **V34/MVP-4 TraceConsistency 检查器（§8.22）**：`check_trace_consistency(spec, task_dir)` 读 `task_dir/trace.jsonl` 提取 `tool_call::*` 工具索引（损坏行跳过、仅 allowed_tools 计数）→ 扫描 `deliverables/` 下 target glob（仅直接子项，复用 `basename_glob_match`）→ 提取 `[证据: 工具名]` 精确格式引用 → 校验存在性。**宁漏勿误（硬约束）**：无匹配格式视为推测处理不 FAIL；`(推测)` 标记计数注入 detail（质量信号）；trace 缺失/产出为空 → PASS（零误报优先）。params 键：evidence_pattern/speculation_marker/allowed_tools/trace_glob（复用 params: Value，零 schema 变更）。
- **soft 失败语义（§6.6）**：soft 检查失败 → `run_checks` passed 仍 true（不短路），但 CheckResult 记录失败——注入 verify prompt 供参考 + DMN 回传统计。测试断言 soft 场景必须断言 `report.passed == true && tc.passed == false`。
- **断言标记是半角**：`(推测)` / `[证据: X]` 半角括号——全角（推测）不匹配（教学段与种子契约统一半角，测试构造勿用全角）。
- **教学段追加位置**：`build_assertion_discipline_prompt()` 在 build_identity_section 之后追加（预算纪律→身份→断言分级顺序）；纯函数返回 String 需 `.to_string()`（字符串字面量是 &str）。

## 24. 按模型分区与分区路由（V36 实现轮）

- **分区布局（BCP §6.1）**：`{knowledge}/{model_key}/`（model_key = `{provider}-{model}` slug）内含五资产层 + index.yaml；`model_stats.yaml`（元权重表）恒在 knowledge **根**（跨分区共享）。`LiluoClient` 双路径：`root_dir`（构造传入）恒为根、`data_dir`（活动目录）= 根或 `root/{model_key}`；`for_model(key)` 派生分区 client（自动建分区目录 + 五层 + 空 index），`partition_key()` 读回；根 client 与分区 client 的 `root_dir` 一致（model_stats 读写互见）。
- **迁移时机**：`migrate_to_partitioned(root, default_key)` 幂等（目标已存在即跳过），在 `main.rs build_engine`（失败上抛——无降级原则）与 `cmd_init`（失败仅提示，人工可重跑）各调一次。**rename 前必须先 create_dir_all 分区目录**（os error 2 实测）。
- **路由先于检索（plan.md V32 阻塞点 #1 修正）**：MetaAgent.run() 第一步是 ModelRouter（纯符号层，读 model_stats，无 LLM）→ `for_model` 分区检索 → LLM 编排。MetaContext.model = 路由结果（降级路径也保持——模型选择与资产编排解耦；None 仅当路由异常）。
- **路由候选仅 deepseek 系**：default + `llm.providers` 中 base_url 为空或 name=="deepseek" 的条目；OpenAI-compat 不参与（Fitting/Causal 执行层 agent builder 跨 provider 类型动态分发未实现，MVP 边界）。`resolve_model` 按候选表精确匹配（模型名可含 `-`，禁止字符串拆解）。
- **模型路由消费**：`AgentFactory::agent_llm_config_with(agent_type, meta_ctx.model)` 覆盖 agent_overrides（路由是元权重决策）；Fitting/Causal builder 加 `provider_name` 字段（默认 "deepseek"），run() 内 `client_for(&provider_name)`。CausalVerifyAgent 的契约加载也分区：`guizang.for_model(meta_ctx.model)`。
- **`agent_llm_config_with` 的 fallback 必须内联静态配置逻辑，禁止调 `agent_llm_config`**（它转发回 with(None)——无限递归栈溢出 SIGABRT 实测）。对称地 agent_llm_config 保持薄转发 = with(None)。
- **ModelRouter 语义**：N_total=0 → 配置默认；score = avg_reward（w_pass·pass_rate + w_quality·avg_quality − w_cost·avg_cost_norm[组内归一化] − w_rounds·avg_rounds）+ C·√(ln N_total/(n+1))。**测试陷阱：N_total=1 时 ln(1)=0 → 探索分 0，无统计候选不会被探索**（断言探索行为需 N_total≥3）。
- **DMN 回传分区**：pending 负载带 `model_key`（serde default 零迁移）；dmn_consumer 解析后传 `backprop_checks`/`backprop_prompts`/`evolve_contracts`（各加 model_key 参数，内部 `partition_liluo(model_key)` 派生）；backprop 成功后按 checks 首项四维聚合回传 model_stats（`update_model_stats`，失败仅 warn——增强层不阻断频率主流程）。bayesian_update 加 liluo 参数（分区后 models/ 也在分区内）。
- **`--with-dmn` 等待 pending 清空**（轮询 60s/1s，dead/ 子目录不计）——固定 3s 对长任务失效（消费者 backoff 指数增长到 32-60s，任务结束时 3s 内不会扫描到新 pending，实测 pending 滞留）。
- **探索任务回传**：active_learning 的 liluo 由 main.rs 传默认分区 client（变体资产迁移后落位分区）；enqueue 带 `liluo.partition_key()`。

## 25. V37 多级路由（异源裁判 + 子任务级覆盖，2026-08 实现轮）

- **相位级异源裁判**：`MetaContext.verify_model: Option<ModelKey>`（serde default + skip_serializing_if，None = 继承 `model`）——Causal verify/converge 优先用 verify_model（factory 两处构造同式：`meta_ctx.verify_model.as_ref().or(meta_ctx.model.as_ref())`）；**Causal 契约加载分区同样 verify_model 优先**（causal.rs Step 1.5——异源模型的分区可能持有不同契约集，§6.1 学习单元语义）。
- **开关**：`runtime.model_routing.heterogeneous_verifier`（serde default false）；factory `create_meta_agent` 经 `.heterogeneous_verifier(config.runtime.model_routing.heterogeneous_verifier)` 注入 MetaAgentBuilder。
- **`ModelRouter.route_verifier(&exec_key)`**：从非主候选按 UCB 同公式选验证模型；候选 <2 → None（warn 降级继承主模型）；全冷启动（N_total=0）→ 声明顺序第一个非主候选（确定性）。MVP 边界：复用任务级 stats，相位维度 (model_key × tag × phase) 后置。
- **子任务级**：`SubtaskSpec.model`（serde default None）经 `apply_subtask_model(child_meta_ctx, model)` 纯函数覆盖（spawn 闭包内调用，可单测）；None 继承父；**verify_model 随父继承**（异源方向不逐层重决策）。子任务 meta 传递链：SubtaskMeta（局部 struct）加 `model` 字段。
- **ModelKey 是 transparent 字符串**（`{provider}-{model}` slug）——LLM 输出 `"model":"deepseek-deepseek-reasoner"` 直接解析，无嵌套结构。
- **冒烟要点**：单候选环境异源恒 None（无源可异）；双模型（同 provider 不同 model 即可，无需第二个 API key 供应商）才可验证；**model_stats 有历史统计时 UCB 可能探索新候选**（N_total 小时探索分大——冒烟实测 N_total=3 时 reasoner 探索分 1.48 > v4-flash 利用分 1.30，执行模型变成 reasoner）——异源断言目标是 `verify_model ≠ model`，不是"验证模型=默认模型"。
- **MetaContext 构造点全量检查**：加新字段后 grep `MetaContext {` 逐个补（types/agent.rs empty()、meta.rs 组装、fitting.rs 测试、constraint_engine.rs 测试——漏一个就是 E0063）。
- **`skip_serializing_if` 断言方向**：Some 时序列化含字段，None 时省略——测试断言 `json.contains("verify_model")` 只在 Some 分支成立。
