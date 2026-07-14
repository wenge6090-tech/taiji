# AI 行为约束（自动加载）

> taiji Rust 重构规则清单。本文件由 IDE/Agent 自动预加载，不在蓝图文档中展示。
>
> **BCP-蓝图-完型协议.md 是项目唯一事实（唯一事实）。** AGENTS.md 是其实施约束补充（AI 自检清单），与 BCP 冲突时以 BCP 为准。

---

## 0. BCP 首要规则（先更新，后执行）

- [ ] **BCP-蓝图-完型协议.md 是项目架构的唯一事实来源（唯一事实）**，AGENTS.md 是 BCP 的实施约束和避坑清单。两者冲突时以 BCP 为准。
- [ ] **先更新 BCP，后执行修改**：任何涉及项目架构、模块结构、类型设计、接口契约、数据流、执行流程、文件系统布局的变更，必须先更新 `BCP-蓝图-完型协议.md`，然后才能进行代码修改。不允许跳过 BCP 直接编码。
- [ ] BCP 更新后，根据 BCP 变更同步更新 AGENTS.md 中的实施约束条目，确保两者一致。
- [ ] 纯内部实现细节（如 bug 修复、测试补全、重构不改变接口）无需更新 BCP，但若涉及模块职责、外部接口或架构层面的变化，必须走 BCP 先行流程。

## 1. TPN 循环规则

- [ ] `BACK_TO_TPN` 跳转必须递增 `round_counter`，达到 `max_rounds` 时只能返回 `PASS`/`FAIL`，禁止再跳转
- [ ] `BACK_TO_META` 跳转必须递增 `cycle_counter`，达到 `max_cycles` 时只能返回 `PASS`/`FAIL`
- [ ] `recursive_decompose` tool 创建子 Agent 前必须检查 `depth < max_depth`（默认 2），超限直接返回错误
- [ ] 子任务数量上限 `max_subtasks`（默认 4），LLM 拆解超出时截断或拒绝
- [ ] `CancellationToken` 必须传递到所有递归层级的子 Agent，取消信号必须立即终止所有子任务
- [ ] 单层递归与多层递归结构同构：每层都是 权重更新→概率拟合→因果验证，唯一变量是 depth

## 2. LLM 调用规则

- [ ] `provider.chat()` / Rig Agent prompt 输入必须为 `Vec<Message>` 结构，严禁拼接裸字符串
- [ ] 每次 LLM 调用前必须 `rate_limiter.acquire().await`（全局 token bucket）
- [ ] DeepSeek 结构化输出失败时：重试最多 3 次 → fallback 到 verbatim 提取 → regex JSON 修复 → 返回 `TaijiError::StructuredOutputParseFailed`
- [ ] MetaAgent 的 `dynamic_context` 必须设置 `top_k=5`，防止上下文过长
- [ ] FittingAgent 的 system prompt 必须包含 MetaContext（reasoning_paths + constraints）
- [ ] CausalAgent verify 模式的 system prompt 必须以 "你是因果验证器" 开头，converge 模式以 "你是收敛判决器" 开头
- [ ] 所有 Agent 的 `max_turns` 硬限制：MetaAgent=1, FittingAgent=30, CausalAgent=3

## 3. 工具安全规则（SafetyHook）

- [ ] `check_file_path(args)`: 拦截 `../`、`~`、`/etc/passwd`、`C:\Windows` 等路径穿越
- [ ] `check_exec_command(args)`: 拦截 `rm -rf`、`curl | sh`、`eval`、`sudo` 等危险命令
- [ ] `check_web_url(args)`: 拦截 `localhost`、`127.0.0.1`、`169.254.x.x`、`10.x.x.x` 等内网地址（SSRF）
- [ ] config.safety.trusted_mcp_servers 列表中的 MCP 工具默认放行，不扫描参数
- [ ] 非白名单 MCP 工具必须强制执行上述三项安全检查
- [ ] 拦截时返回 `Flow::skip("denied by safety: {reason}")`，不执行工具

## 4. 约束检查规则（ConstraintEngine）

- [ ] MetaAgent 加载阶段：`load_truths(task_type_tags)` → 注入 `MetaContext.constraints`
- [ ] CausalAgent.verify 前置：`check_constraints(output, constraints)` 必须在校验 LLM 调用之前执行
- [ ] 任一硬约束（severity=Hard）违反 → 直接返回 `VerificationReport { route: BACK_TO_META }`，不进入 LLM
- [ ] 软约束（severity=Soft）违反 → 作为额外上下文注入 CausalAgent 的 system prompt，由 LLM 裁决

## 5. 工具选择规则（SkillTriggerEngine）

- [ ] 在 FittingAgent 创建时（非运行时）执行匹配：`match_skills(task_description, tags) → Vec<SkillRef>`
- [ ] 正则匹配优先：skill.trigger.pattern 匹配 task_description
- [ ] 标签匹配次之：skill.task_type_tags ∩ task.tags
- [ ] 权重排序降序，top_k=10
- [ ] 匹配结果作为 FittingAgent 的 static tools 注册（`.tool(matched_skills)`）
- [ ] Rig `dynamic_tools` 仅作为 fallback：当显式匹配结果为空时启用

## 6. DMN 演化规则（CognitionEvolver）

- [ ] DMN Consumer 是独立 `tokio::spawn` 后台任务，与 TPN 执行完全解耦
- [ ] 指数退避轮询 pending/ 队列：1s → 2s → 4s → 8s → 16s → 32s → 60s（上限）
- [ ] δ₀ 修剪：移除 `confidence < 0.1` 的低信度资产（不可恢复）
- [ ] δ₁ L1 技能调优：`success_rate = success_count / (success_count + fail_count)`，`use_count++`
- [ ] δ₂ L2 贝叶斯更新：`alpha += success_count, beta += fail_count`，`confidence = alpha / (alpha + beta)`
- [ ] δ₃ L3 网格重连：调整 relation.weight ±0.1，范围 [0, 1]
- [ ] 所有演化结果写入 Qdrant 时必须 `version++`（乐观并发控制）

## 7. Qdrant 一致性规则

- [ ] 单 collection `nskg`，type + layer 区分四层认知资产
- [ ] `insert_document()` 之前：如果同 ID 已存在 → `update_document()`，否则 `create_document()`
- [ ] `credit_attribution()` 写入前检查 version 字段：与读取时一致才写入，否则重试（最多 3 次）
- [ ] `traverse_relations()` BFS 必须 dedup visited set，防止回环
- [ ] `build_reasoning_paths()` 的 `max_hops` 默认 3

## 8. Trace 写入规则

- [ ] 概率拟合（阳）的执行 trace：通过 Rig `TraceHook` 自动捕获所有 StepEvent
- [ ] 权重更新（元）和因果验证（阴）的 trace：手动 `TraceWriter::write()` 写入单条记录
- [ ] trace.jsonl 轮转：单文件超过 10MB → 归档为 trace.{N}.jsonl，保留最近 5 代
- [ ] 敏感信息（API Key、token）必须在写入前脱敏
- [ ] `read_tree()` 递归合并所有 `**/trace.jsonl`，按 `ts` 时间戳排序
- [ ] 嵌套子任务的 trace 写入各自子目录 `tasks/{parent}/{n}/trace.jsonl`

## 9. 并发与限流规则

- [ ] WorkerPool 使用 `tokio::sync::Semaphore(max_concurrent)` 限制并发 Agent 数
- [ ] RateLimiter 使用 token bucket 算法：全局共享 `Arc<RateLimiter>`
- [ ] `requests_per_minute` 和 `tokens_per_minute` 分别限流，超限时 `.acquire().await` 阻塞等待
- [ ] `recursive_decompose` 内的子任务并发使用 `tokio::join_all`，受 Semaphore 限制

## 10. MCP 规则

- [ ] MCP Server 使用 rmcp crate 实现 stdio 传输，暴露 TPN/DMN/认知资产 操作工具
- [ ] MCP Client 连接外部服务器时：stdio 子进程管理 + SSE 重连（3 次，指数退避）
- [ ] 外部 MCP 工具注入 FittingAgent 时：非白名单服务器的工具强制 SafetyHook 检查
- [ ] MCP 工具与 L1 Skills 并列注册，LLM 自行选择调用

## 11. 错误处理规则

- [ ] 所有 `TaijiError` 变体必须携带 `context: String` 字段（可追溯到调用链）
- [ ] LLM 调用失败（网络/超时/限流）→ 重试最多 3 次 → 降级标记 `degraded=true` → 返回 `TaijiError::LLMCallFailed`
- [ ] Qdrant 连接失败 → 指数退避重连（最多 5 次）→ 返回 `TaijiError::QdrantUnavailable`
- [ ] 文件系统 I/O 错误 → 直接返回 `TaijiError::IO(io::Error)`，不重试
- [ ] 死信队列：不可恢复的任务写入 `pending/dead/`，附带完整 error 上下文
- [ ] `panic!` 和 `unwrap()` 禁止在 async 上下文中使用 → 全部改为 `Result<T, TaijiError>`

## 12. 测试映射

以下规则对应内嵌单元测试（`#[cfg(test)]` 模块，非独立 `tests/` 目录文件）。
标有 ⚠ 的测试依赖外部 Qdrant 服务，默认 `#[ignore]`。

| 规则 | 实际位置（`src/` 内嵌） | 状态 |
|------|------------------------|------|
| TPN 循环死循环防护 | `src/orchestration/runner.rs`（循环境辑）+ `src/agents/fitting.rs`（depth check） | ⚠ runner 无独立 UT；fitting depth check 可用 |
| LLM 结构化输出回退 | 无独立单元测试（需 mock LLM） | — |
| 工具安全拦截 | `src/hooks/safety.rs`（23 项测试：文件路径/命令/URL/路由） | ✅ 完整 |
| 约束检查前置 | `src/orchestration/constraint_engine.rs`（13 项测试） | ✅ 完整 |
| Qdrant 并发写入冲突 | 无独立单元测试（需 Qdrant） | — |
| Trace 轮转与脱敏 | `src/hooks/trace.rs`（7 项测试：脱敏 + 写入） | ✅ 脱敏覆盖；轮转未覆盖 |
| RateLimiter token bucket | `src/infra/rate_limiter.rs` 无内嵌测试 | — |
| DMN 演化正确性 | `src/orchestration/cognition_evolver.rs`（7 项测试） | ⚠ 需 Qdrant |
| 递归深度限制 | `src/agents/fitting.rs` `test_fitting_agent_depth_check` | ✅ |
