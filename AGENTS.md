# taiji 开发 agent 项目全量简况（自动加载）

> **实现事实层**：环境 + 路径索引 + 避坑规则。代码文件是最精确的实现——本文件**不重复存实现细节**，只存「代码里读不出来的」：环境信息、模块路径索引、行为规则。
> 设计问题看 `Blueprint.md`（设计哲学 + 所有 Mermaid 图）；需要计划套 `plan.md` 转化协议。

## 三文件关系（事实分层）

| 文件 | 本质 | 加载时机 |
|------|------|---------|
| `Blueprint.md` | 设计定论（哲学 + Mermaid 图） | 设计/架构/改契约时读 |
| `plan.md` | 转化协议（空壳） | 任务需计划时套用 |
| `AGENTS.md`（本文） | 实现事实（路径索引 + 规则） | 每次会话实时加载 |

数据流：`Blueprint(设计) --plan协议转化--> 计划 --执行--> 代码`；经验固化回流本文件（避坑）/ `Blueprint.md`（定论）。

---

## 环境信息

- **语言/构建/测试**：Rust 2024 edition，单 crate `taiji`；`cargo build` / `cargo test`（单个 `cargo test <name>`）。
- **配置**：仅配置文件（不读环境变量），`.taiji/config.json` → `taiji.config.json`；`api_key` 空是硬错误。
- **命名**：易经体系——`ZhouyiCycle` / `lianshan.rs` / `YangAgent` / `YinAgent` / `GuizangClient` / `BACK_TO_ZHOUYI`。
- **Vendor**：Rig v0.39 在 `vendor/`（`[patch.crates-io]` 重定向），不直接改。
- **技术栈**：Rig(deepseek provider) + tokio + axum + serde + clap + tracing；前端 React 18 + TS + Tailwind + React Flow（`taiji-web/`）。
- **CLI**：`taiji run / --resume / list / status / trace / serve / init / migrate`（入口 `src/main.rs`）。

---

## 模块路径索引（七层）

| 层 | 路径 | 职责（一句话） |
|----|------|------|
| L0 类型 | `src/types/` | task / agent / verification / execution / frontend / plan / manifold 核心类型 |
| L1 基础设施 | `src/infra/` | config(配置) / error(TaijiError) / provider(ProviderRegistry) / knowledge(归藏读写+UCB检索) / trace / handoff / git_backend / migrate / json_util / meta_skills / skill_catalog / task_id / task_spec |
| L2 Hook | `src/hooks/` | safety(ToolSafetyGuard) / trace(TraceHook) / context_limiter / chat_history_snapshot / yang_hook_set(YangHookSet) / yin_hook_set(YinHookSet) |
| L3 Agent | `src/agents/` | factory(中枢) / meta(元) / yang(阳) / yin(阴) / chat(前端对话,含 guizang_query 归藏检索工具) / plan(PlanBuilder) / tools/(recursive_decompose, yin_verify, text_call, skills/, guizang_query) |
| L4 编排 | `src/orchestration/` | zhouyi(三相循环) / runner / constraint_engine / skill_engine / trigger_engine / worker_pool / lianshan(压缩算子) / manifold(迹拓扑压缩) / cognition_evolver / model_router / active_learning / task_tree_builder |
| L5 MCP | `src/mcp/` | server(暴露 taiji 工具, rmcp 3.x stdio) / client(消费外部 MCP) |
| L6 WS+HTTP | `src/ws/` + `src/main.rs serve` | WebSocket 事件推送 + 请求响应 + axum 静态托管 |
| L7 前端 | `taiji-web/` | React 纯浏览器 UI（纺锤树 / Zhouyi 弹窗 / 聊天面板）；外部前端 Agent 对接见 `taiji-web/MCP_INTEGRATION.md` |

## 关键类型路径索引

| 类型 | 路径 |
|------|------|
| `Task` / `SubtaskSpec` / `DecomposeResult` / `ZhouyiResult` | `src/types/task.rs` |
| `ManifoldTopology` / `TopologyNode` / `TopologyEdge` / `TopologyNodeKind` / `TopologyEdgeKind` | `src/types/manifold.rs` |
| `MetaContext` / `AgentMode` / `YangPrompt` / `SkillRef` | `src/types/agent.rs` |
| `SkillAsset` / `SkillImpl` / `SkillKind` / `SkillResult` / `SkillReport` | `src/types/verification.rs` |
| `VerificationReport` / `ConvergenceDecision` / `VerificationRoute` | `src/types/verification.rs` |
| `AssetStats` / `AssetRef` / `ModelStats` | `src/types/*` |
| `TaijiError` | `src/infra/error.rs` |
| 归藏资产字段契约（YAML schema） | `src/infra/knowledge.rs`（struct 定义）+ `Blueprint.md` 资产树图 |

---

## 避坑规则（硬约束）

## 0. 文档首要规则（先更新文档，后执行修改）

- **先更新 BCP，后执行修改**：任何涉及模块结构、类型设计、接口契约、数据流的变更必须先更新 `Blueprint.md`（设计定论）/ `AGENTS.md`（实现事实）。
- 纯内部实现细节（bug 修复、测试补全、重构不改变接口）无需更新文档。
- 设计定论（`Blueprint.md`）与代码冲突时以设计定论为准；实现事实（本文件）与代码冲突时以代码为准；命名不一致以代码为准。

## 2. 周易循环防护

- `BACK_TO_ZHOUYI` 递增 `round_counter`，达 `max_rounds` 时只能返回 PASS/FAIL，禁止再跳转。
- `BACK_TO_META` 递增 `cycle_counter`，达 `max_cycles` 时只能返回 PASS/FAIL。
- `recursive_decompose` 创建子任务前必须检查 `depth < max_depth`（默认 2），超限返回错误。
- 子任务数量上限 `max_subtasks`（默认 4），超出截断。
- `CancellationToken` 必须通过 `child_token()` 传递到所有递归层级。
- 子任务并发使用 `JoinSet::spawn`，`join_next()` 流式收集；取消/panic 时 `abort_all()` 清理，任务级失败记录 Diverged 继续收集（V31）。

## 3. Agent 关键约束

- **AgentMode 阴阳配对**：`Orchestration`（编排拆解+综合）| `Execution`（直接产出）。由 MetaAgent 权重更新时按递归层数规则 + 任务难易程度决策。`depth+1 >= max_depth` 时强制 Execution。
- **配对模板**：Orchestration → 阳编排+阴收敛；Execution → 阳执行+阴验证。
- **工具注册**：`recursive_decompose` 仅 Orchestration 模式 YangAgent 注册。Execution 模式 LLM 不可见此工具。
- **四象温度默认值**：YangOrch 0.8 / YangExec 0.5 / YinVerify 0.2 / YinConverge 0.2。

## 4. 带工具必有安全钩子（硬约束）

- 任何注册工具的 Agent 必须挂载 SafetyHook。Meta/Yin 注册只读收集工具（read/search/webfetch），Yang 注册执行工具（read/write/bash/search/webfetch + recursive_decompose/yin_verify）。
- **Rig 0.39 `AgentBuilder::hook()` 是单槽覆盖式**：`.hook(a).hook(b)` 只有最后一个生效。YangAgent 的多 hook 必须经 `YangHookSet` 组合（safety → trace → snapshot），YinAgent 经 `YinHookSet` 组合（safety → limiter），各一次 `.hook()` 挂载。

## 5. 错误处理与测试

- `TaijiError` 变体必须携带上下文信息（`context: String` 或结构化字段如 `threshold`/`max`/`reason`）。
- LLM 调用失败重试 3 次 → 降级 → `TaijiError::LLMCallFailed`。
- async 上下文中禁止 `panic!` / `unwrap()`，全部用 `Result`。
- 测试中创建的临时目录用 `tmp_dir`，测试末尾必须清理。并行测试的临时目录必须唯一（静态 `AtomicUsize` 计数器）。

## 6. LLM 结构化输出解析

- **LLM 响应解析一律走 `src/infra/json_util.rs` 的 `parse_llm_json<T>`**，禁止直接 `serde_json::from_str` 解析 LLM 输出。
- `parse_llm_json` 四级容错：① 直接解析 → ② ` ```json ` 围栏提取 → ③ 全文首尾大括号切片 → ④ 返回原始错误。

## 7. 上下文窗口预算（V29 起弃 max_turns，见 §14）

- `max_turns` 降级为防死循环兜底（200），不承担上下文管理——窗口预算由 §14 的 ContextLimiter 承担。

## 8. 无降级原则

- 新代码读身份册失败 / 会盟扫描失败 / 归藏 I/O 失败 → `TaijiError` 上抛（错误信息必须携带路径）。
- 「无父（根任务）」与「无兄弟」是**状态分支**，非降级——不应用 `unwrap_or_default()` 吞错。
- 既有降级点（MetaContext::empty、Base 模板、LLM 重试等）维持现状，改造另立章节。

## 9. Skill 双轨架构（V45，字段契约见「关键类型路径索引」）

- **双轨加载**：[`infra::skill_catalog::load_skill_catalog`] = 元层（[`infra::meta_skills`] Rust 硬编码）∪ 资产层（`skills/{cat}/{id}/skill.yaml`），**同 id 资产优先**（资产层覆盖元层教学字段，执行体恒为 Rust builtin）。空知识库 → 元层保底，基础周易闭环照常。
- **`SkillAsset` 统一类型**（`types/verification`，serde tag=type rename=skill）：`implementations: Vec<SkillImpl>`（复数 ≥1，兼容多 check 迁移）；`dual: String` 硬约束——保存时在合并视图域校验（元层 ∪ 资产层）目标存在且类别互补，缺失 = 硬错误。
- **`SkillKind`** 含阴 6（FileExists..TraceConsistency，SkillEngine 机械执行）+ 阳 6（Bash/Write/Read/Search/Webfetch/RecursiveDecompose，映射 builtin）。`is_yin()`/`is_yang()` 辅助；`run_checks_assets` 跳过阳面与 LlmJudgement。
- **弱模型协议双通道**（实现见 `src/agents/tools/skills/mod.rs` + `src/agents/tools/text_call.rs`）：通道 A 扁平 schema（`definition()` 按 inputModes 生成顶层 `{path,content}` 废除双 JSON 转义；`type Args = serde_json::Value`）；通道 B 文本调用块 fallback（[`tools::text_call::extract_tool_calls`]）；**旧 `{"input":"{\"path\":...}"}` 双转义形态经 `normalize_args` 三级展开兼容**（顶层键直读 → input JSON 字符串展开 → input 纯字符串单参直传）。
- **ToolProfile 路由**（[`agents::factory::profile_for_model`]）：模型 key 含 flash/lite/mini/small → `Minimal`（**仅隐藏 webfetch 高代价联网；V47 不再隐藏 recursive-decompose**——拆解正是弱模型小上下文规避超限的核心手段，见上条弱模型协议双通道）；其余 `Full`。
- **文件夹格式**：资产层每 skill 一文件夹 `skills/{cat}/{id}/skill.yaml`（[`GuizangClient::save_skill`] atomic write + version++）；`load_skill_assets` 兼容**旧单文件** `yin/skills/{cat}/*.yaml`（`verification_to_skill_asset` 转换，dual 按 check.kind 推导），文件夹优先、同 id 去重。
- **冷启动保底**：删除资产层后 `taiji run` 简单任务仍走完整 verify 闭环（元层判据生效）。
- **双类型禁令**：`infra::knowledge::LegacyToolSkillAsset`（旧 L1，CognitiveAsset::Skill）≠ `types::verification::SkillAsset`（V45 统一）。新代码只用后者 + `save_skill`/`load_skill_assets`；禁止再引入 `SkillAsset` 同名。
- **连山加载桥**：`load_all_verifications` 扫 `verify/{id}/skill.yaml` + 旧扁平 `*.yaml`（**原样**保留 checks.stats/variant_of，禁止经 SkillAsset 往返丢字段）+ **仅空库**注入元层。运行时 verify 走 catalog（元层∪资产层始终合并）；连山有磁盘种子时不以元层混计数。
- **空 command 跳过**：元层 `command-succeeds` 默认 `params.command=""`——`run_checks_assets` 与 `skill_asset_to_verification` 均跳过，避免 soft-fail 噪声；资产层覆盖 command 后才机械执行。
- **dual 真互补**：`save_skill` 校验 dual 存在 **且** `effective_category` 落在 Orch↔Converge / Exec↔Verify。

## 10. 曾用名数据迁移（V45）

- 持久化旧值（`"FittingDone"`/`"VerifyDone"`/`"BackToTpn"`/键 `fitting_system_prompt`）经 `taiji migrate` 一次性替换为新值（`YangDone`/`YinDone`/`BackToZhouyi`/`yang_system_prompt`）——文本替换 + 原子写，幂等可复跑（`src/infra/migrate.rs`）。
- 代码**不保留**任何旧值兼容（无 serde alias、无旧键 fallback）——遇到旧任务目录先跑 `taiji migrate` 再 resume。

## 11. 归藏 git 版本控制 + 元短路（V46）

- **GitBackend 快照式版本控制**（`infra::git_backend`）：commit = 全量快照到 `{data_dir}/.history/{id}/tree/`（**必须排除 `.history` 自身**，防递归复制）；commit id = `{ts_millis:x}-{hash:x}`；rollback = `clear_tree(data_dir, exclude=.history)` + 从快照 `copy_tree` 回。`.history` 不被 `scan_assets` 扫描（只扫固定资产目录），但**断言 data_dir 根目录内容白名单的测试**需把 `.history` 加进去。
- **save_* 写路径自动 commit**：`save_asset` 与 `save_skill` 在 atomic rename 成功后调 `self.git.commit("save {type}:{id}")`；commit 失败**上抛**（归藏 I/O 硬错误，无降级）。`GuizangClient` 结构新增 `git: GitBackend` 字段，`new()` 与 `new_sparse()` 都 init（.history 是版本基础设施，不是资产目录）。
- **SkillAsset 渐进披露三字段**（`types/verification`）：`summary`（层0，进 tool 列表）/ `description`（层1，现有）/ `detail`（层2，`Option<String>`，LLM 决定调用后按需加载）——均 serde default 零迁移。**所有 SkillAsset 构造点**（meta_skills、knowledge 转换、测试）必须补 summary/detail 字段，否则 E0063。
- **SkillRef.summary**（`types/agent`）：serde default；**所有 `SkillRef {..}` 字面量构造点**（skills/mod.rs、yang.rs、trigger_engine.rs）必须补 `summary`，否则 E0063。`SkillTool::definition()` 披露层0 summary（空回退 `id — name`）。
- **MetaOutcome 双出口**（`types/agent`，V46）：`Context(MetaContext)` = 行动类走完整循环；`Answer(String)` = 应答类短路。`MetaAgentBuilder::run()` 返回 `Result<MetaOutcome>`——**所有调用点**（zhouyi 的 needs_meta + BACK_TO_META、plan.rs）必须 match 两个出口，否则 E0308。
- **短路判断并入 LLM 语义层**（B3）：`MetaComposeResult` 加 `answer: Option<String>`（serde default）；`mode` 加 `#[serde(default)]`（短路 JSON 只给 answer 不给 mode 也能 parse）。prompt 里"短路判断"先于"模式决策"：应答类（产出不改变世界）填 answer，其余字段可省略。
- **短路路径**：Answer → `write_short_circuit_answer`（写 `deliverables/answer.md`）→ `write_task_status(Completed)` → 直接 `return Ok(ZhouyiResult)` 跳过阳阴。验证规则：符号校验保底（引用真实性）+ 交互判断兜底（父节点/用户读 answer.md 裁定），**阴不做语义验证**（同源概率回路（Blueprint §1.3））。

## 12. context_overflow 模式分流（V47）

- **分流规则**（分流表见下方规则）：`run_yang_with_v28_routing` 遇 `ContextOverflow` 按 `meta_ctx.mode` + `depth` 三路分流——**编排模式** → BACK_TO_ZHOUYI（阳有 recursive_decompose 直接拆）；**执行模式且 depth+1 < max_depth** → BACK_TO_META（粒度错误 = 认知偏差，元重判编排）；**执行模式且叶节点** → BACK_TO_ZHOUYI（残缺产出兜底）。
- **YangOutcome 三态枚举**（zhouyi.rs）：`Success(ZhouyiResult)` / `BackToZhouyi` / `BackToMeta`——`run_yang_with_v28_routing` 返回 `Result<YangOutcome>`，调用方（Phase 2 两处）与 Phase 4 路由均须 match 三态，否则 E0308。
- **rerun_meta 共用流程**（zhouyi.rs）：BACK_TO_META 流程（cycle++、元读 handoff 校准重判、apply_leaf_depth_rule、持久化、重置 chat_history）提取为 `rerun_meta` 自由函数，Phase 2（context_overflow 分流）与 Phase 4（VerificationRoute::BackToMeta）共用——改 BACK_TO_META 流程只改一处。
- **元 prompt 强制编排教学**（meta.rs META_COMPOSE_SYSTEM_PROMPT 规则 3）：handoff 含 context_overflow 且 depth+1 < max_depth → 必须判 Orchestration；叶节点才允许维持 Execution。

## 13. write 路径基准 = task_dir（V47 P0）

- **write 工具相对路径按 task_dir 解析，非进程 cwd**（`skills/write.rs`）：`path` 非绝对 → `task_dir.join(path)`；绝对路径须落在 task_dir 内（`enforce_cwd_scope(_, task_dir)`）。此前用 `std::env::current_dir()`（项目根）解析，导致 LLM 传 `"deliverables/x.md"` 时写到项目根而非任务目录，父层 converge 扫 task_dir 收不到产出物（「产出物转移 → 收敛失效」）。
- **`BuiltinSkill::call` 签名带 `task_dir: &Path`**（read/bash/search/webfetch 忽略此参、按进程 cwd 操作项目源码；write 使用）。`SkillTool`/`SkillRegistry` 持有 `task_dir`，构造点（yang/yin 传 `&self.engine_ctx.task_dir`，meta/chat 只读工具传 `Path::new(".")`）必须补参，否则 E0061。
- **回归测试**：`write.rs::test_write_relative_path_resolves_to_task_dir`——相对路径必须落 task_dir 下。改 write 路径逻辑后跑 `cargo test --lib`（基线 306）。

## 14. 上下文预算 = 单次窗口占用，非跨轮累计（V48）+ 阴预算对称（V49）

- **单次窗口占用语义**：`ContextLimiter` 取每次 `completion_response.usage.input_tokens` 单次值（非跨轮累计——每轮 input_tokens 含完整历史重放，累计会多重计数同一段历史，导致窗口远未用满即触顶假爆）。
- **阈值单一事实源**：`config::ContextLimits`——`effective_handoff() = 窗口 30%`、`effective_hard_cutoff() = 窗口 35%`（绝对值覆盖优先）；5% 余量 = 收尾写交接预算。`max_turns` 降级为防死循环兜底（200），不承担上下文管理。
- **阴预算对称（V49）**：YinAgent（verify/converge）挂 `YinHookSet`（safety → limiter），`max_turns 10→200`。两相预算模型对称、溢出语义不对称——阳（产出相）30% → handoff.md + `Err(ContextOverflow)` 重路由；阴（终审相）30% → **保守裁决（非 error）**：verify → `VerificationReport{ route=BackToZhouyi, confidence=0.0 }`，converge → `ConvergenceDecision{ status=Partial }`；35% → 两相一致 `Err(HardCutoff)` 上抛 FAIL。
- **溢出检查时序**：`limiter.triggered()` 必须在 `agent.prompt()/chat()` 返回后、`.map_err`/parse 之前检查（Terminate 可能以 Err 或部分 Ok 浮现，与 yang.rs 同构）。

## 15. 蓝图文件·迹拓扑（V50）

- **数据源双轨**：统计压缩读 `trace.jsonl`（度量·「干了什么」）；迹拓扑读任务目录树（结构·「要干什么/产出什么」：`meta.json` + `deliverables/` + `handoff.md`），**不碰 trace.jsonl**（§6.0 三层定论）。
- **拓扑契约**：`knowledge/manifold/{root_task}.yaml`（**serde YAML**，与资产文件一致，非 JSON）；节点 `Task/Asset/Deliverable/Handoff`，边 `Decompose/Invoke/Dataflow/Handoff/Verify`。`decompose` 边来自 `meta.json.parent_id`（精确，非 depth 近似）；deliverable 节点 id = 相对 root task_dir 的路径（树内唯一）。
- **`enqueue_lianshan_pending` 新增 `task_dir: &Path` 参数**（4 处调用点：zhouyi 生产 + active_learning + zhouyi 测试×2）——pending 负载加 `task_dir` 字段（`task_dir.display().to_string()`），Lianshan 经此获得任务树入口。
- **拓扑是增强层**：backprop 成功后压缩，失败仅 warn 不阻断 backprop 主流程（与 model_stats/backprop_prompts 同构）；`compress_task_tree_to_topology` 是纯函数（同步、零 LLM），`save_topology`/`load_topology` 复用 save_asset 的 tmp+rename+git commit 模式。
- **MVP 边界**：根 pending 只含根任务 `assets_used`，子任务资产归因 → 只产出根级 `invoke`/`verify` 边（子级归因列为后续）。

## 16. 危险隔离 + env_tags 降权 + 冷启动先验（V50）

- **safe_for_exploration 字段**：`SkillAsset`（types/verification）+ `VerificationAsset`（types/agent）各加 `safe_for_exploration: bool`（serde default false）；`skill_asset_to_verification` 透传。`pick_exploration_target` 过滤 `!safe_for_exploration`（危险隔离）。**所有构造点**（meta_skills、knowledge 转换、测试）已补 false。
- **主动学习冷启动**：`exploration_score` 的 `n=0 → f64::MAX` 改为 `confidence` 先验映射 μ（α=1+10c, β=1+10(1−c)）+ C·√(ln n_total)——非最大探索分（与 prompts 路径同构）。
- **env_tags 降权**：`rank_prompts_by_ucb` 新增 `current_env_tags: &[String]` 参数——当前环境指纹非空、候选 env_tags 非空且无交集 → ×0.5（降权非过滤；候选 env_tags 空 = 环境无关不降权）。**源已接（§18）**：`meta_ctx.model` → `model_class()` → `["flash"|"strong"]`。
- **漂移检测 / 退化诊断 / compile 调度**：定论已写（Blueprint §6.4/§6.5），实现待 /plan 阶段三；SW-UCB / Pareto-MCTS / 奖励归一化为已知边界延后（§6.5）。
- **待补**：safe_for_exploration 的「人工/流程标记」机制 + fork_variants 继承标记（当前全默认 false → 主动学习需标记后才激活）。

## 17. 编译任务 = 一次周易任务执行（V50，§6.0 契约）

- **新模块** `src/orchestration/compile.rs`：`enqueue_compile_task`（单写者入队 `compile/{root_task}.json`，幂等：同 root_task 已存在不覆盖）/ `spawn_compiler`（main.rs `--with-lianshan` 时 spawn，`compile_enabled` 关不启动）/ `run_compile_queue`（空闲窗口消费）/ `parse_skill_deliverable`（去围栏 → YAML → JSON → parse_llm_json）/ `compile_task_description`（「标准 skill 编写规范」模板 + 拓扑注入 + 元层对偶候选表）。
- **入队位置**：lianshan.rs backprop 成功后 `save_topology` 之后调 `enqueue_compile_task(&self.data_root, task_id)`（增强层 warn-only）。
- **调度**（§6.0 定稿）：compile/ 与 pending/ 分离，单写者 = Lianshan Consumer；执行触发 = pending 空（空闲窗口）+ `compile_enabled`（config `runtime.lianshan.compile_enabled`，默认 false）。
- **不写 model_stats**：编译任务 PASS 后 zhouyi 会入队 pending → compile runner 立即删 `pending/{compile_task_id}.json`（只产 skill YAML，不污染路由统计、不触发二次拓扑/编译）。
- **阴验证 = save_skill 机械判据**：解析 deliverables/skill.yaml → `save_skill`（内建 dual 存在 + 类别互补 + git commit）+ `implementations` 非空校验。失败重试 ≤3 → `.failed` + `.error` 日志（记录 manifold 引用 + 错误）。
- **测试基线**：`cargo test --lib` → **321 passed, 0 failed**（compile.rs 5 测试 + lianshan 拓扑测试加 compile 入队断言）。
- **遗留（已知边界）**：①「原任务变体复跑」未实现（阴验证只做机械判据，未重跑原任务验证复现）；② `compile_budget` 字段已加（config）但未强制执行——token 预算仍由既有 ContextLimiter 承担；③ 删 pending 与 Lianshan consumer 存在极短竞态窗口（空闲窗口下 consumer backoff 已增长，实际风险低）。

## 18. 环境维度轴（env_tags = 模型类，V50 §6.3.1 定稿）

- **定论**（Blueprint §6.3.1）：`env_tags` 是统一环境维度轴，模型类（flash/strong）是首要维度；4 类归藏资产（prompts/skills/verifications）共用「env_tags 隔离 + UCB 维度内排序 + 四算子维度内演化」一条轴，不给每类各写一套主动学习。
- **模型类指纹**：`factory::model_class(&ModelKey)` / `model_class_from_str(&str)`——key 含 flash/lite/mini/small → "flash"，其余 → "strong"（与 `profile_for_model` 同一检测源，零新判定逻辑）。
- **检索层源已接**：meta.rs `rank_prompts_by_ucb` 传 `[model_class(&model_key)]`（路由模型类 → current_env_tags）——同维度变体优先，异维度 ×0.5 降权。
- **`VerificationAsset` 补 `env_tags` 字段**（serde default）+ `skill_asset_to_verification` 透传 `s.env_tags`（与 safe_for_exploration 同构）。
- **fork 打维度标签**：`evolve_contracts(model_key)` 派生 `model_env_tag` → 线程到 `fork_variants` / `evolve_prompts` → `fork_prompts`——变体 `env_tags = [model_class]`（None → 空 = 环境无关）。
- **统计后验天然按变体 id 隔离**（每变体独立 id 独立后验）——V44 去分区化不动，不按模型复制资产树。
- **边界**：Rust 硬编码元层宪法（meta.rs/yang/yin 模板）不参与主动学习，宪法自适应靠资产层变体覆盖（V45 双轨同 id 优先）。
- **测试基线**：`cargo test --lib` → **323 passed, 0 failed**（+2：model_class 检测 + fork env_tags 标签）。

## 19. 本体挖掘（OntologyMiner，V50 §6.6）

- **类型层** `src/types/ontology.rs`（新）：`SemanticType`/`TypeSource`（词汇表）+ `OntologyEdge`（**type→type**，from/to 是 SemanticType id，`evidence` 审计支撑资产 id）+ `OntologyEdgeKind`（只 WeakDependency/Sequence，**不挖 Forbid**）+ `OntologyRule`/`RuleCondition`（type-level 规则）+ `TaskOntologyView`（实体链接输出）+ `CooccurPair`/`FailureGroup`（挖掘输入）。
- **归藏存取** `infra/knowledge.rs`：`load/save_semantic_types`（types.yaml，`SemanticTypeFile{types}` 包装）、`load/save_relations`（relations.yaml）、`load/save_rules`（rules.yaml）、`load/save_cooccur`（cooccur.yaml）、`load/save_failures`（failures.yaml）——全部经 `load_ontology_yaml`/`save_ontology_yaml` 私有 helper（原子写 + git commit）；`asset_type_map`（资产 id → 语义类型 id，扫 tags 匹配词表，MVP-1 只映射 prompts）。
- **挖掘器** `orchestration/ontology_miner.rs`（新，零 LLM）：纯函数 `accumulate_cooccur`/`merge_cooccur`/`abstract_to_types`（**id→type 抽象**，无映射跳过）/`mine_dependencies`（联合通过率 ≥ 0.8 + 样本 ≥ 50）/`mine_constraints`（失败率=1.0 + 样本 ≥ 50）/`merge_failures` + async 入口 `run_ontology_mining`（共现→边 + 失败×model_class→规则）。常量 `ONTOLOGY_MIN_SAMPLES=50`、`ONTOLOGY_LIFT_THRESHOLD=0.8`。
- **lianshan hook**：backprop 后（compile 入队之后）调 `run_ontology_mining(self.evolver.guizang(), &assets_used, passed, &checks, model_key)`（warn-only 增强层）。
- **Meta 消费** `agents/meta.rs`：`MetaComposeResult` 加 `ontology: Option<TaskOntologyView>`（serde default，**合并进既有 compose LLM，零新增调用**）+ `META_COMPOSE_SYSTEM_PROMPT` 实体链接教学段（domain/action/objects/env）；`ontology_expand`（类型级软查询：objects 命中边→注入对侧类型资产）/`ontology_validate`+`rule_matches`（约束匹配→constraint_summaries）/`apply_ontology`（async 消费，失败 warn）。
- **约束升级** `orchestration/constraint_engine.rs`：`load_truths(task_type_tags, rules)` —— 元层 4 truth ∪ 挖掘规则（id 前缀 `ontology:`）；`yin.rs::verify` 从 `self.guizang` 加载 rules（None=测试路径，I/O 失败上抛）。
- **测试基线**：`cargo test --lib` → **335 passed, 0 failed**（+12：miner 5 + types 2 + meta 3 + constraint 1 + knowledge 1）。
- **遗留/边界**：① **种子词表前置**——`types.yaml` 空则 `asset_type_map` 空 → 类型抽象/挖掘空转（需人工种 5-10 类型 + 给资产 tags 打语义类型 id）；② **skill 共现数据源缺失**——`assets_used` 现只含 prompt，skill 级挖掘延后；③ MVP 用**联合通过率**（pass/co）非 lift（个体基线 P(pass|a) 扩展后续）；④ 宏节点 `Abstract_Concept`（命名走 compile）延后。

## 20. 全量代码审查避坑回流（V51）

- **max_depth override 必须同步 factory.config**：`AgentFactory::with_config(config)` 重建 factory（clone 共享字段 + 替换 config），不可只改 config 副本——否则 `RecursiveDecomposeTool`（读 `factory.config.runtime.max_depth`）与 `ZhouyiCycle`（读副本）深度来源分裂，override 半生效（mcp/ws 两处同构）。
- **task_type_tags 链贯通**：`zhouyi classify_task_tags(description)`（纯符号关键词零 LLM，代码/编译/重构/调试→`code`，否则 `general`）→ `meta.run(description, tags)` 透传 → `MetaContext.task_type_tags` → `yin verify load_truths(&meta_ctx.task_type_tags, rules)`。新增标签型约束（如 `code-safety`）必须确认链贯通，否则死代码。已知边界：关键词启发式，LLM 语义标签（MetaComposeResult 输出）后续增强。
- **`safe_canonicalize` symlink 逃逸**（`skills/common.rs`）：fallback 分支的 `strip_prefix` 必须用**原始祖先 p（词法）**，不是已解析祖先 c——否则含 symlink 祖先段时 strip_prefix 失败退回词法路径，`starts_with` 退化为词法比较漏检。
- **reqwest 0.11 redirect SSRF 每跳校验**（`skills/webfetch.rs`）：`redirect(Policy::custom)` 闭包内对 `attempt.url()` 做 IP 归一化校验（`attempt.follow()/stop()`），不可只校验首跳。
- **归藏资产缺失用 `TaijiError::KnowledgeAssetNotFound { id }` 变体匹配**（`matches!`），禁 `e.to_string().contains("failed to read asset")`（字符串匹配脆弱）。
- **`save_model_stats` 有意不 commit**（高频统计衍生，非资产契约——避免快照爆炸 + 污染回滚语义）；资产写路径（save_asset/save_skill/ontology_yaml）仍必须 tmp+rename+git commit。
- **LLM 围栏解析走 `json_util::find_json_fence`**（case-insensitive + 允许空格，覆盖 ```JSON / ``` json）；所有 LLM 响应解析一律经 `parse_llm_json<T>`。
