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

- **数据源双轨**：统计压缩读 `trace.jsonl`（度量·「干了什么」）；迹拓扑读任务目录树（结构·「要干什么/产出什么」：`meta.json` + `deliverables/` + `handoff.md`），**不碰 trace.jsonl**（Blueprint §5.0 三层定论）。
- **拓扑契约**：`knowledge/manifold/{root_task}.yaml`（**serde YAML**，与资产文件一致，非 JSON）；节点 `Task/Asset/Deliverable/Handoff`，边 `Decompose/Invoke/Dataflow/Handoff/Verify`。`decompose` 边来自 `meta.json.parent_id`（精确，非 depth 近似）；deliverable 节点 id = 相对 root task_dir 的路径（树内唯一）。
- **`enqueue_lianshan_pending` 新增 `task_dir: &Path` 参数**（4 处调用点：zhouyi 生产 + active_learning + zhouyi 测试×2）——pending 负载加 `task_dir` 字段（`task_dir.display().to_string()`），Lianshan 经此获得任务树入口。
- **拓扑是增强层**：backprop 成功后压缩，失败仅 warn 不阻断 backprop 主流程（与 model_stats/backprop_prompts 同构）；`compress_task_tree_to_topology` 是纯函数（同步、零 LLM），`save_topology`/`load_topology` 复用 save_asset 的 tmp+rename+git commit 模式。
- **MVP 边界**：根 pending 只含根任务 `assets_used`，子任务资产归因 → 只产出根级 `invoke`/`verify` 边（子级归因列为后续）。

## 16. 危险隔离 + env_tags 降权 + 冷启动先验（V50）

- **safe_for_exploration 字段**：`SkillAsset`（types/verification）+ `VerificationAsset`（types/agent）各加 `safe_for_exploration: bool`（serde default false）；`skill_asset_to_verification` 透传。`pick_exploration_target` 过滤 `!safe_for_exploration`（危险隔离）。**所有构造点**（meta_skills、knowledge 转换、测试）已补 false。
- **主动学习冷启动**：`exploration_score` 的 `n=0 → f64::MAX` 改为 `confidence` 先验映射 μ（α=1+10c, β=1+10(1−c)）+ C·√(ln n_total)——非最大探索分（与 prompts 路径同构）。
- **env_tags 降权**：`rank_prompts_by_ucb` 新增 `current_env_tags: &[String]` 参数——当前环境指纹非空、候选 env_tags 非空且无交集 → ×0.5（降权非过滤；候选 env_tags 空 = 环境无关不降权）。**源已接（§18）**：`meta_ctx.model` → `model_class()` → `["flash"|"strong"]`。
- **漂移检测 / 退化诊断 / compile 调度**：定论已写（Blueprint §5.5/§5.6），实现待 /plan 阶段三；SW-UCB / Pareto-MCTS / 奖励归一化为已知边界延后（§5.6）。
- **待补**：safe_for_exploration 的「人工/流程标记」机制 + fork_variants 继承标记（当前全默认 false → 主动学习需标记后才激活）。

## 17. 编译任务 = 一次周易任务执行（V50，Blueprint §5.0 契约）

- **新模块** `src/orchestration/compile.rs`：`enqueue_compile_task`（单写者入队 `compile/{root_task}.json`，幂等：同 root_task 已存在不覆盖）/ `spawn_compiler`（main.rs `--with-lianshan` 时 spawn，`compile_enabled` 关不启动）/ `run_compile_queue`（空闲窗口消费）/ `parse_skill_deliverable`（去围栏 → YAML → JSON → parse_llm_json）/ `compile_task_description`（「标准 skill 编写规范」模板 + 拓扑注入 + 元层对偶候选表）。
- **入队位置**：lianshan.rs backprop 成功后 `save_topology` 之后调 `enqueue_compile_task(&self.data_root, task_id)`（增强层 warn-only）。
- **调度**（Blueprint §5.0 定稿）：compile/ 与 pending/ 分离，单写者 = Lianshan Consumer；执行触发 = pending 空（空闲窗口）+ `compile_enabled`（config `runtime.lianshan.compile_enabled`，默认 false）。
- **不写 model_stats**：编译任务 PASS 后 zhouyi 会入队 pending → compile runner 立即删 `pending/{compile_task_id}.json`（只产 skill YAML，不污染路由统计、不触发二次拓扑/编译）。
- **阴验证 = save_skill 机械判据**：解析 deliverables/skill.yaml → `save_skill`（内建 dual 存在 + 类别互补 + git commit）+ `implementations` 非空校验。失败重试 ≤3 → `.failed` + `.error` 日志（记录 manifold 引用 + 错误）。
- **测试基线**：`cargo test --lib` → **321 passed, 0 failed**（compile.rs 5 测试 + lianshan 拓扑测试加 compile 入队断言）。
- **遗留（已知边界）**：①「原任务变体复跑」未实现（阴验证只做机械判据，未重跑原任务验证复现）；② `compile_budget` 字段已加（config）但未强制执行——token 预算仍由既有 ContextLimiter 承担；③ 删 pending 与 Lianshan consumer 存在极短竞态窗口（空闲窗口下 consumer backoff 已增长，实际风险低）。

## 18. 环境维度轴（env_tags = 模型类，V50 §5.4 定稿）

- **定论**（Blueprint §5.4）：`env_tags` 是统一环境维度轴，模型类（flash/strong）是首要维度；4 类归藏资产（prompts/skills/verifications）共用「env_tags 隔离 + UCB 维度内排序 + 四算子维度内演化」一条轴，不给每类各写一套主动学习。
- **模型类指纹**：`factory::model_class(&ModelKey)` / `model_class_from_str(&str)`——key 含 flash/lite/mini/small → "flash"，其余 → "strong"（与 `profile_for_model` 同一检测源，零新判定逻辑）。
- **检索层源已接**：meta.rs `rank_prompts_by_ucb` 传 `[model_class(&model_key)]`（路由模型类 → current_env_tags）——同维度变体优先，异维度 ×0.5 降权。
- **`VerificationAsset` 补 `env_tags` 字段**（serde default）+ `skill_asset_to_verification` 透传 `s.env_tags`（与 safe_for_exploration 同构）。
- **fork 打维度标签**：`evolve_contracts(model_key)` 派生 `model_env_tag` → 线程到 `fork_variants` / `evolve_prompts` → `fork_prompts`——变体 `env_tags = [model_class]`（None → 空 = 环境无关）。
- **统计后验天然按变体 id 隔离**（每变体独立 id 独立后验）——V44 去分区化不动，不按模型复制资产树。
- **边界**：Rust 硬编码元层宪法（meta.rs/yang/yin 模板）不参与主动学习，宪法自适应靠资产层变体覆盖（V45 双轨同 id 优先）。
- **测试基线**：`cargo test --lib` → **323 passed, 0 failed**（+2：model_class 检测 + fork env_tags 标签）。

## 19. 本体挖掘（OntologyMiner，V50 §5.7）

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

## 21. 资产层统一 Python（V52，Blueprint §6.0/§5.0/§3）

- **SkillKind 12→3 坍缩**（`types/verification.rs`）：`Builtin`（Rust 种子层，**builtin 名 = skill.id**）/ `Python`（资产层脚本，**脚本相对路径 = impl.target**）/ `LlmJudgement`（唯一 LLM kind）。`target` 语义三分：阴 builtin=检查目标 / Python=脚本路径 / 阳 builtin=留空。
- **单一映射源**：`builtin_check_kind(name) -> Option<CheckKind>`（file-exists→FileExists…，阳 builtin 返回 None）+ `builtin_category(name)`（→SkillCategory）。**新增阴机械判据必须同步这两个函数**，否则 `knowledge.rs::skill_asset_to_verification` 与 `skill_engine.rs::impl_to_check_spec` 静默丢失（旧两处手写同构映射已删除）。
- **`is_yin()`/`is_yang()` 已移除**——`Builtin` 二义（write 是阳 / file-exists 是阴），阴阳判定改用 `skill.effective_category()`（Orch/Exec=阳，Verify/Converge=阴）。`run_checks_assets` 只执行 `builtin_check_kind(skill.id)` 命中的 Builtin 实现。
- **Python 执行引擎** `orchestration/python_engine.rs`：`run_python_skill(script, params, task_dir)`——`python3` 子进程，stdin JSON 进 / stdout JSON 出，`env_clear`（只留 PATH/HOME，**去掉 OPENAI_API_KEY = §1.3 第一闸门**）+ 30s 超时（内部 `run_python_skill_with_timeout` 可注入短超时供测试）+ cwd=task_dir。脚本契约：`def execute(params) -> dict` + `if __name__ == "__main__": print(json.dumps(execute(json.loads(sys.stdin.read()))))`。
- **`taiji builtin <name> --args <json> [--task-dir <dir>]` syscall 子命令**（main.rs）：资产层 Python skill 经 `subprocess.run(["taiji","builtin",<name>,"--args",json])` 调 Rust 种子层原语（用户态调 syscall）。`skills::lookup_builtin` 公开为免费函数，CLI 与 SkillTool 共用同一注册表。
- **SkillTool 双 runner**（`skills/mod.rs`）：`SkillRunner::Builtin(Arc<dyn BuiltinSkill>) | Python(PathBuf)`。`SkillTool::new_python` / `SkillRegistry::load_python_skills`。**Python skill 用通用对象 schema**（`additionalProperties: true`，参数直传 execute(params)，不经 normalize_args 双 JSON 转义）。
- **YangAgent 接线**（yang.rs `load_python_skills` 免费函数）：加载 `load_skill_catalog(Exec|Orch)` → 只取 `kind: python` 实现 → `guizang.skill_script_path(cat, id, target)` 解析脚本 → 注册。**脚本缺失仅 warn 跳过**（资产层损坏不阻断 builtin 种子层闭环）。
- **YinAgent 接线**（`skill_engine.rs::run_checks_assets` 签名新增 `guizang: Option<&GuizangClient>`）：阴面类别（Verify/Converge）的 `Python` 实现经 `python_engine` 子进程执行，产出 `CheckResult { kind: CheckKind::Python, passed, detail }`；`passed=false` + Hard → hard 短路。**`CheckKind` 新增 `Python` 变体**（serde `python`，仅运行时产生，不落盘到 `VerificationAsset.checks`——`skill_asset_to_verification` 对 Python kind 返回 None）。yin.rs 两处调用点（verify/converge）传 `self.guizang.as_deref()` / `Some(guizang.as_ref())`。
- **编译管道产出 Python**（compile.rs）：模板教 LLM 写 `deliverables/skill.py`（`PYTHON_SKILL_CONTRACT` 常量注入 few-shot）+ `skill.yaml`（`kind: python` + `target: skill.py`）。`extract_skill_script` 读脚本；`save_skill_script` 落盘旁车文件 `{cat}/{id}/skill.py`（tmp+rename+git commit）。
- **种子 YAML 迁移**：`.taiji/knowledge/yin/skills/verify/{file-exists,schema-valid,reference-resolves,trace-consistency}/skill.yaml` 的 `kind: file_exists|schema_valid|reference_resolves|trace_consistency` → `kind: builtin`（llm_judgement 不变）。**旧 kind 名无 serde alias**（§10 规则）——资产层旧 YAML 需手动迁移。
- **避坑**：① Rust 2024 `std::env::set_var/remove_var` 是 unsafe（测试里包 unsafe 块）；② `save_skill`（skill.yaml）与 `save_skill_script`（skill.py）分两次 git commit——非原子，已知边界；③ `load_skill_catalog` 返回元层∪资产层合并视图，YangAgent 接线只取 `kind: python`（builtin 元层已注册，勿重复）。
- **测试基线**：`cargo test --lib` → **360 passed, 0 failed**（+7：python_engine 4 + SkillTool Python 1 + save_skill_script 1 + run_checks_assets Python verify 1）。

## 22. 归藏星云图（GetGuizangGraph，WS 协议）

- **数据源必须用 `load_skill_catalog`（元层∪资产层合并视图），禁止只扫 `load_skill_assets`（仅磁盘资产层）**：磁盘技能的对偶边（如 `file-exists.dual=write`）引用的是 Rust 元层技能 `write`——磁盘上不存在，只扫磁盘会导致 dual 边悬空/丢失。合并视图保证对偶边全解析。
- **节点键带类型前缀** `{type}:{id}`（prompt:xxx / skill:xxx / model:xxx）：prompt 与 model 可同名（如 `orch-yang` 提示词 ↔ `orch-yang` 贝叶斯后验），不加前缀撞键。
- **dual 边去重**：`skill.dual` 双向对称（write↔file-exists 双方都填对偶），只保留字典序小的一端（`s.id < s.dual`），避免 A↔B 重复线。
- **模型样本数**：`stats_n = (α+β−2).max(0)`（Beta(1,1) 先验下的等效采样次数，对齐前端节点尺寸）。
- **前端力导向零依赖手写**（`taiji-web/GuizangGraph.tsx`）：排斥 O(n²)（资产量级几十个可忽略）+ 向心引力防爆炸 + 阻尼收敛 + 160 帧 rAF 定帧停止（非无限动画，省 CPU）。
- **空库态**：`load_all_prompts`/`load_skill_assets` 过滤 `status != active`（pruned 留盘审计不入图）；知识库为空 → 前端「归藏知识库为空」提示，非错误。

## 23. 语义层视图（GetOntologyView，元的先验智能可视化）

- **`OntologyView` 直接透传 ontology 类型（字段 snake_case），不另造 camelCase 视图层**：字段与磁盘 `types.yaml`/`relations.yaml`/`rules.yaml`/`cooccur.yaml`/`failures.yaml` 契约一致（`env_tags`/`check_kind`/`weak_dependency`），转换层只会引入字段名两套并存。前端 TS 类型注释标明「snake_case 透传」。
- **种子词表是启动钥匙（鸡生蛋）**：`asset_type_map` 扫 assets tags 匹配 `types.yaml` 词表（路径 `knowledge/ontology/types.yaml`）；词表空 → 映射空 → `abstract_to_types` 全跳过 → 无边/无规则 → `apply_ontology` 注入零先验。**种子必须对齐现有资产 tags 的实际值**（现有 4 个 prompt 的 tags 是 `orchestration`/`execution`/`verify`/`converge` 全称，非 SkillCategory 的 `orch`/`exec`）——乱种领域类型（如 `deploy-action`）会因无资产打该 tag 而映射仍空。现状词表两根轴：模式轴（orchestration/execution/verify/converge，对齐 tags，立即映射 4 prompt）+ 领域轴 canonical 6 根（§6.6：code-action/verification-check/deploy-action/data-action/doc-action/knowledge-action 及子树，供未来语义标签）。**避坑：重写/扩展词表时不得删模式轴 4 id**（实测曾只剩 canonical 领域轴 → 映射全空 → §5.7 类型级软查询整个空转）。**根 `knowledge/types.yaml` 是遗留旧种子（代码不读，勿改勿引用）**。
- **「无规则/无边」是常态非 bug**：挖掘门槛高——依赖边需共现 ≥ `ONTOLOGY_MIN_SAMPLES`(50) 且联合通过率 ≥0.8；约束规则需失败样本 ≥50 且失败率=1.0。可视化里规则/边为空是「先验未激活」，不是错误（状态分支回退纯 UCB，§5.7 红线 3）。
- **`apply_ontology` 失败仅 warn（meta.rs 调用点），语义层是增强层**：本体读失败 = 归藏 I/O 硬错误上抛，但调用方 catch 后 warn 继续（增强层失败不阻断主循环）；「ontology 缺失/空 domain」是状态分支，非降级。

## 24. skill 嵌套 + 编译即演化（V53，Blueprint §5.0/§6.0 V53 定论）

- **`taiji skill <id>` 子命令（main.rs cmd_skill）——用户态调用户态**：与 `taiji builtin`（用户态调 syscall）正交。四类扫描（Exec/Orch/Verify/Converge）`load_skill_catalog` 找 `kind: python` 实现 → `skill_script_path` 解析 → `run_python_skill` 执行。**只查资产层 Python skill，不回落 builtin**。
- **循环/深度护栏载体 = `TAIJI_SKILL_CHAIN` 环境变量（JSON 数组）**：`cmd_skill` 读它判循环（id 已在链中→拒绝）+ 深度（链长 ≥ `runtime.max_depth`→拒绝）；`python_engine::run_python_skill` 在 `env_clear` 后注入（空链不注入）。**改 run_python_skill 签名（新增 `chain: &[String]`）后，所有调用点（SkillTool / SkillEngine / 测试）必须传 `&[]`，否则 E0061**。
- **Python skill 统计走 `SkillAsset.stats`（非 `VerificationAsset.checks`）**：V52 定论 `CheckKind::Python` 不落盘 VerificationAsset.checks，故 `backprop_checks` 末尾新增 `backprop_python_skills` 独立路径——按 `check_id` 前缀（`{skill.id}#{idx}`）匹配 SkillAsset，更新其 `stats`（n/pass/cost/rounds/quality）。**只有 `kind: python` 的资产层 skill 被更新（元层 builtin 无 Python 执行体，过滤跳过）**。
- **阳面 Python skill 统计信号链（损失函数 pass 分量，V53）**：阴面 skill 经 SkillEngine 产 CheckResult（已有）；阳面 skill（exec/orch）经 `SkillTool` 工具调用，结果不进 verify_state——故新增 `SkillTool::execute` Python 分支记录 `{task_dir}/tool_calls.jsonl`（每行 JSON：skill_id/passed/detail，同步 append 失败仅 warn），`zhouyi.rs` PASS 时 `load_tool_calls` 读它转 CheckResult（check_id=`{skill_id}#0`）合并进 checks → `backprop_python_skills` 回传。**cost/rounds/quality 留 0（工具调用级无 token 信号，任务级摊派在 verify_state 分支已做，MVP 边界）**。
- **fork 对象类型分裂（两个 fork 不混）**：`fork_variants` 操作 VerificationAsset（llm_judgement 判据改 strictness 参数）；`fork_python_skills` 操作 SkillAsset（Python 执行体，低通过率时**入队 compile 变体重新生成执行体**，非 clone+改参数）。**不要试图合并两者——V52 定论 CheckKind::Python 不落盘 VerificationAsset，Python skill 的演化必须走 SkillAsset 路径**。
- **编译演化算子闭环**：`fork_python_skills`（空闲窗口，lianshan `files.is_empty()` 分支）发现低通过率（`stats.n ≥ min_samples` 且 `pass_rate < FORK_PASS_RATE_THRESHOLD=0.6`）Python skill → `enqueue_compile_task_variant(data_root, variant_id={id}-v1, variant_of, failure_detail)`（幂等：compile 文件已存在跳过）→ `run_compile_queue` 消费 `recompile: true` payload → `compile_recompile_description` 教 LLM 产出变体（id=variant_id、继承 dual/parent_id）→ **save_skill 前冒烟压测**（python_engine 跑空 params，crash/非法 JSON/超时 = 编译失败重试）。
- **冒烟压测是「主动学习压测」的 MVP 形态**：连山符号裁决第一道闸（零 LLM），验证脚本可执行，非复跑原任务；「原任务变体复跑」仍是后续（AGENTS.md §17 遗留①）。
- **测试基线**：`cargo test --lib` → **367 passed, 0 failed**（+6：chain 注入 / compile 变体幂等 / 重编译模板 / python skill 回传 / fork 入队 / 阳面 tool_calls 解析）。

## 25. 损失函数全修（V51，四维回报接线 + cost 归一化 + 摊派修复）

- **`CheckStats::reward(w, cost_norm)` 签名加 `cost_norm`**（[0,1] 归一化成本）——原始 token 量级（~1e5）不归一化会以 4 个数量级碾压 pass/quality 项，回报退化为 `≈ −w_cost·avg_cost`（实测 file-exists：0.5+0.29−48754−0.1 = −48753）。归一化对齐 model_router 的 `avg_cost/max_group_avg_cost` 模式。**原生产路径零调用**（fork/merge/prune 只用 pass_rate；model_router 内联自己一份归一化实现）——现统一。
- **四维回报接线**：fork/merge/prune 六算子（variants + prompts）决策值由单一 pass_rate 升级为 `decision_value(stats, mu, cost_norm, w) = w_pass·μ + w_quality·avg_quality − w_cost·cost_norm − w_rounds·avg_rounds`（pass 项用后验 μ，空 map 回退频率 pass_rate）。阈值：`FORK_REWARD_THRESHOLD=0.3`（≈旧 pass_rate 0.6）、`MERGE_REWARD_DIFF=0.05`（≈旧 pass 差 0.1）。`FORK_PASS_RATE_THRESHOLD=0.6` 保留给 `fork_python_skills`（Python 执行体路径）。
- **cost 摊派除以 check 数**：`backprop_checks`/`backprop_python_skills` 的 `cost_sum += cost_tokens / n_checks`（修「同任务全额摊派给每个 check → 一笔成本记 N 次」的 4× 重复）。model_stats 仍取 `checks.first().cost_tokens`（全额，不受影响）。
- **观测事实（两套 stats 分家）**：backprop 回传**扁平** `*.yaml`（legacy VerificationAsset），不回传 V45 文件夹 `{id}/skill.yaml`（其 stats 恒 0）——Python skill 走 `backprop_python_skills`，builtin 判据走扁平。改 stats 语义前先确认改哪套。
- **测试基线**：`cargo test --lib` → **367 passed, 0 failed**。

## 26. 编译管线实测修复（V54：路径纪律 + 冒烟压测 + handoff 契约）

- **编译 prompt 路径纪律**（`compile.rs` 的 `compile_task_description` + `compile_recompile_description` 模板）：显式要求「写产物只用 write 工具（相对路径），禁止 bash cp/mkdir/重定向写到绝对路径或项目根」。起因：编译任务 LLM 用 `bash cp` 把 skill.py 拷到项目根（bash 按设计忽略 task_dir、scope 只到项目根），绕过 write 工具的 task_dir scope → 阴验证收不到产物。
- **`run_python_skill` 脚本路径 canonicalize**（`python_engine.rs`）：入口把 script_path canonicalize 成绝对路径（相对路径按进程 cwd=项目根解析）。起因：调用方传 `.taiji/...` 相对路径时被 cwd=task_dir 二次拼接成 `.taiji/tasks/<id>/.taiji/...`（冒烟压测 can't open file）。**这是 SkillTool / SkillEngine / 冒烟压测所有调用点的共用修复**（测试用绝对 tmp 目录所以此前没暴露）。回归测试 `run_python_skill_relative_path_resolves_absolutely`。
- **编译模板要求 handoff.md front matter**：阴验证 reference-resolves 解析 handoff.md 的 output_refs 逐项验存在；编译模板必须要求 LLM 写标准 front matter（task/result/status/output_refs），否则 LLM 写纯 markdown → YAML 解析失败 → BackToZhouyi 死循环。编译输出三文件 = skill.py + skill.yaml + handoff.md。
- **观测：bash 冒烟自测污染项目根**：编译任务 LLM 用 bash（cwd=项目根，设计使然）冒烟自测时往项目根 `deliverables/` 落 check_file_exists.py + `__pycache__` 垃圾——不阻断管线，但与 cp 污染同类，会干扰后续任务 LLM 视野（实测已两次撞上，污染目录需人工清理）。
- **测试基线**：`cargo test --lib` → **367 passed, 0 failed**（+1：python_engine 相对路径回归）。

## 27. 编译 skill 分类（V55：判据类强制归阴 + 模板教学）

- **实测 bug**：编译任务 LLM 按「来源任务类型」（写脚本 → exec）给 skill 分类，把「检查文件是否存在」这类**判据**（输出 passed 布尔、机械判定是否满足）误标 `category: exec` + `agent_target: YangAgent` 落到阳面，且 dual 选了同侧同类（file-exists，verify 侧）——非互补。save_skill 的 dual 互补校验拦不住（exec↔verify 表面互补），只靠 LLM 自觉。
- **模板教学**（`compile.rs` 两处模板 `compile_task_description` + `compile_recompile_description` 的「skill 分类规则」段）：按**功能本质**分类——判据类（输入目标/引用/内容，输出 passed 布尔）→ `verify` + `YinAgent` + dual 从 exec 侧选（write/read/bash/search/webfetch）；执行类（主动操作）→ `exec` + `YangAgent` + dual 从 verify 侧选；拆解 → orch；收敛 → converge。反例教学：检查文件存在的脚本 = 判据 → verify，不是 exec。
- **机械护栏**（`compile.rs::enforce_judgment_category`，runner 在 extract_skill 后调用）：description 命中强判据词（判定/验证/存在性/当且仅当/合法性/一致性）**且** pass_condition 含 passed 布尔判定 → 强制 `category=verify` + `agent_target=YinAgent`；dual 若仍同侧（verify）→ 取其对偶 exec skill（如 file-exists.dual = write）。改动用 warn 审计。保守性：只取强判据词，弱词（检查/判断）不取防误伤动作类；已 verify 或动作类不动。**V61 起归阴后进弃置闸**（`discard_yin_category`：verify/converge 产出不落盘，直接删 compile 文件）——判据类不再产出 skill，复用内置原子判据（constraint_engine 硬编码）。
- **已落库错误资产修正方式**：直接改 yaml（category/agent_target/dual）+ `mv` 目录（yang/skills/exec → yin/skills/verify）；`.history` 旧快照保留作审计，下次任何 save_* 全量快照自然包含修正。
- **测试基线**：`cargo test --lib` → **370 passed, 0 failed**（+3：判据类强制归阴 / 动作类不动 / 已 verify 不动）。
## 28. serde(default) 结构体级 vs 字段级（V55：LianshanConfig 默认值全部退化为 0 的 bug）

- **规则**：Rust serde 的 `#[serde(default)]` 在**结构体级**（struct 上方）= 缺失字段取**容器类型 Default::default() 的对应值**；在**字段级** = 缺失字段取**字段类型 Default**（u64/usize/f64/bool → 0/false/0.0）。需要语义默认值的配置结构必须用**结构体级**（与 LlmConfig/ContextLimits/ModelRoutingConfig 同模式）。
- **实测 bug**：`LianshanConfig` 全是字段级 `#[serde(default)]`，而 `.taiji/config.json` 的 `runtime.lianshan` 只显式写了 `compile_enabled` → 其余字段反序列化为 0：
  - `min_samples=0` → `fork_python_skills` 对 **n=0 刚落库的 skill** 也入队重编译（实测：check-file-exists 刚编译落库就被 fork 成 check-file-exists-v1，`failure_detail: "pass_rate 0.00 < 0.6 (n=0)"`）——白白消耗一次 LLM 编译。
  - `activation_min_samples=0` + `activation_min_assets=0` → **演化激活门槛失效**（S2 实验的 fork→prune FIRED 就是在零门槛下观察到的，修复后行为更保守，旧观察结论需重估）。
  - `prior_strength=0.0` → 冷启动先验 α=1+0·c 退化为均匀 Beta(1,1)，confidence 先验失效。
- **回归测试**：`config.rs::lianshan_missing_fields_use_container_default`（对象存在但缺字段 → 3/5/50/10.0/20000）+ `lianshan_whole_object_missing_uses_default`（整个对象缺失 → 整体 Default）。注意 RuntimeConfig 的 max_concurrent_agents/max_depth/max_rounds/max_cycles/max_subtasks 与 TaijiConfig.version 是**必填无默认**——测试 JSON 必须补全。
- **测试基线**：`cargo test --lib` → **372 passed, 0 failed**（+2 回归）。

## 29. V56-V59 实现（阴判断节点 + 实时录入 + 连山收缩 + 晶体归藏 + V51 恢复）

> 本次为跨版本架构过渡（Blueprint V57/V58/V59 落地），修改面大。以下四条按子版本分述，最后是事故恢复与测试基线。

### V57 阴判断节点（半符号半 LLM，非 Agent）

- **阴不是 Agent**：不持有 skill、不注册工具、不跑 SkillEngine、不持有 system prompt（资产层）。`SkillEngine` 已删除（`orchestration/skill_engine.rs` 移除，`pub mod skill_engine` 同步删除）；`YinHookSet`（`hooks/yin_hook_set.rs`）变死代码（保留未删，V57 后阴无 hook 挂载点）。
- **阴 = 半符号半 LLM 判断节点**（`agents/yin.rs` `YinJudge`）：符号层优先恒在（LLM 不可翻案），LLM 层兑底（唯一 LLM 介入点，**不注册工具**——read/webfetch 移除）。
- **判断依据三层**：逻辑层（`load_truths` + `check_yin_output` 机械对碰阳产出，hard 违反 → `BackToMeta` 认知偏差）+ 因果层（`match_relations` 因果先验注入 LLM 兑底 prompt）+ 运行保障（`check_atomics` 无条件 Rust 原子判据，hard 失败 → `BackToZhouyi` 执行偏差）。
- **无系统提示资产**：硬编码 `VERIFY_FALLBACK_PROMPT` / `CONVERGE_FALLBACK_PROMPT`（与元「半 LLM 半符号」对称、顺序相反：元先语义后符号，阴先符号后语义）。
- **AgentFactory**：`create_yin_verify_agent`/`create_yin_converge_agent` → 单一 `create_yin_judge`（zhouyi/yin_verify/recursive_decompose 调用点同步）。
- **`check_atomics` 返回 `(Vec<CheckResult>, bool)`**（结果 + hard 失败旗标）；`MetaContext` 新增 `ontology_objects`（compose 实体链接产物透传阴，零新增 LLM）。
- **已知边界（V33 路径停止）**：V33 的 llm_judgement 判据变体树（fork/merge/prune 演化主要对象）在 V57 后失去数据源——SkillEngine 删除（判据不再逐条执行）+ backprop_checks 删除（stats 不再回传）。`load_all_verifications()` 的 llm_judgement 资产 stats 恒 0 → `total_n < min_samples` → 永不触发演化。这是 V57「判据从资产层 llm_judgement 迁移到 ontology 三层 + 原子判据」的自然结果，演化焦点转移到 Python skill + prompts + ontology 因果。非 bug，是架构迁移的既定边界。

### V59 实时录入 + 连山收缩（C 方案）

- **实时录入**（替代连山 backprop）：`GuizangClient::record_prompt_signal`/`record_python_skill_stats`/`update_posterior`（knowledge.rs，贝叶斯 α/β 更新自 `CognitionEvolver::bayesian_update` 迁移）——PASS 时 zhouyi 调 `record_judgment` 写入 stats + 后验（非阻塞：失败仅 warn）。
- **连山收缩**：`lianshan.rs` 去 backprop（`backprop_prompts`/`backprop_checks`/`model_stats` 移除），只保留深压缩：拓扑压缩（`save_topology`）+ compile 入队 + 本体挖掘（`run_ontology_mining`）+ `evolve_contracts`。
- **C 方案（字段级写隔离 + git commit 互斥）**：`GitBackend` 加 `commit_lock: tokio::sync::Mutex<()>`（`init()` 初始化），`commit()` 顶部加锁序列化全量快照——阴实时写与连山异步写不再竞态。
- **死代码删除**：`cognition_evolver.rs` 的 `backprop_checks`/`backprop_prompts`/`backprop_python_skills` 已删（§24/§25 中 backprop_* 相关描述**过时**）；`bayesian_update`/`partition_guizang` 保留（evolve/fork 仍用）。

### V58 晶体归藏（撤销概率化，观测坍缩二值）

- **定论**：归藏是晶体智能——确定、可观测、二值，不是概率性的东西。`OntologyEdge`/`OntologyRule` **无** `alpha`/`beta`/`p()`（撤销阶段一的概率化字段，回到 V50 晶体版）。
- **二值存在**：边/规则存在性由挖掘判定二值决定（`strength ≥ 阈值 && samples ≥ min_samples` → 存在，否则不存在）。无「半存在」中间态。
- **观测强度 ≠ 存在概率**：`strength`（联合通过率）是「观测 N 次通过 M 次」的精确统计（晶体数据），非「边存在的信念强度」（气体概率）。
- **概率分层**：概率只活在决策瞬间（阳阴循环、UCB 路由），不 commit 进归藏（与 §20「save_model_stats 有意不 commit」同原则）。
- **消费端本就二值**：`ontology_expand`/`load_truths`/`match_relations` 只做存在性消费（边存在 → 生效），未用 p() 加权——撤销概率化后自动回到晶体语义，无消费端改动。

### V51 四维回报恢复（git checkout 事故后重建）

- **事故**：`git checkout src/orchestration/cognition_evolver.rs` 误恢复到 HEAD（V52 提交），静默丢弃工作区 V51-V55 未提交修改（`fork_python_skills`/`backprop_python_skills`/`decision_value` 等）。
- **恢复**：`fork_python_skills`（V53，lianshan 空闲窗口 fork 低通过率 Python skill → `enqueue_compile_task_variant`）从 §24 描述重建；`decision_value` 四维回报 + `FORK_REWARD_THRESHOLD=0.3`/`MERGE_REWARD_DIFF=0.05` + 六算子接线（fork/merge/prune × variants/prompts，组内 cost 归一化）从 §25 描述重建；`FORK_PASS_RATE_THRESHOLD=0.6` 保留给 fork_python_skills。
- **避坑（硬约束）**：工作区有未提交的跨版本修改时，`git checkout <file>` 会静默丢弃它们（恢复到 HEAD/索引）。操作前必须先 `git status` 确认 + 备份；项目 HEAD 滞后于工作区时尤其危险（本次 HEAD=V52，工作区=V55）。

### 测试基线

- **当前**：`cargo test --lib` → **360 passed, 0 failed, 4 ignored**。
- 变化链：V55 372 → 阶段一（V57）360（skill_engine 删除 + backprop/旧 yin 测试移除）→ +V51 decision_value 测试 361 → −V58 概率化测试（ontology_edge_p）360。

## 30. V57 阴判断死循环修复（V60 真实任务实测）

- **trace-consistency 白名单必须含全部执行工具**：`check_atomics` 的 allowed_tools 曾漏 `write` → `tool_call::write`（builtin skill 执行路径）证据永远不可验证 → soft-FAIL → 语义裁决必 BackToZhouyi → 死循环（普通任务 LLM 手写 [证据: write] 每轮重写同格式，永不过）。**改白名单要覆盖 builtin skill 全集（write/read/bash/search/webfetch），新增阳 builtin 时同步。**
- **reference-resolves 对「无 output_refs」跳过而非 FAIL**：普通任务成功路径程序不写权威 handoff（write_handoff 仅在失败/恢复路径跑，output_refs 只存在于编译任务/失败恢复），LLM 手写 handoff 无 output_refs → 照单 FAIL = 死循环燃料。已改为「**有 output_refs 才验证**，字段缺失/不可解析 = passed + skip 说明」。编译任务契约（§26 模板强制 output_refs → 验证照常）保持。**禁止把 reference-resolves 当「handoff 必须存在」判据**——它是引用完整性判据，无引用可验 = 状态分支跳过。
- **front matter 解析宽容**：手写 handoff 无 `---` 围栏时全文直解报 serde_yaml「multiple documents」误导性错误（§26 死循环的另一形态）→ `extract_first_yaml_block` 先截首个文档分隔符前。
- **真实闭环基线**：`taiji run` 最小任务（write 产出）实测 Pass/confidence 1.0——4 判据（file-exists/schema-valid/reference-resolves-skip/trace-consistency-write 证据可验）+ LLM 兑底全过；pending 入队 + V59 实时录入（4 prompt usage_count +1）。
- **测试基线**：`cargo test --lib` → **363 passed, 0 failed, 4 ignored**（+3：write 白名单回归 / output_refs 缺失跳过 / output_refs 严格验证保持）。

## 31. 阴/元去资产化（V61，A 定论落地）

- **定论**（Blueprint §6.0 V57 已有，V61 实现落地）：阴（YinJudge）与元（MetaAgent）已变为**归藏因果世界模型的消费者**——阴消费 ontology 因果（truths/relations/rules）做符号层对碰 + LLM 兑底（硬编码 `VERIFY_FALLBACK`/`CONVERGE_FALLBACK`），元消费语义层先验 + UCB 路由。**不再持有晶体资产**（prompts/skills）。
- **数据**：`.taiji/knowledge/yin/` 全删（2 prompts + 6 verify + 3 converge 文件夹 + 编译产物 check-file-exists/review-deliverables-verify + 旧扁平 yaml，共 19 tracked + untracked）。元层 `meta_skills`（Rust 硬编码 8 builtin）**保留**——作 catalog 对偶候选表/接线结构，非判据执行源（判据执行 = `check_atomics` 硬编码，Blueprint 902/966）。
- **代码**：
  - `ensure_dirs` 不再创建 yin/prompts + yin/skills/{verify,converge}
  - `load_all_prompts` 只扫 `["yang/prompts", "prompts"]`（不再扫 yin/prompts）
  - **compile 弃置闸** `compile.rs::discard_yin_category`：verify/converge 类别产物不落盘（删 compile 文件，成功消费不重试）——内置原子判据 + 语义裁决已覆盖，落盘即死资产。`enforce_judgment_category`（§27）保留：判据误标 exec → 归 verify → 进弃置闸
  - compile 模板（`compile_task_description` / `compile_recompile_description` 分类规则段）：判据类/收敛类 → **不产出 skill**（复用内置原子判据/语义裁决），只产阳面 exec/orch；反例教学同步
- **保留**（V57 定论，无数据时空转无害）：`fork_variants`/`bayesian_update`/`save_verification` 写路径（VerificationAsset 演化代码留痕）、`type_dir_name` 的 yin 分支（旧库读取兼容）、`load_all_verifications` 连山加载桥（读旧 verify 兼容）。
- **测试基线**：`cargo test --lib` → **364 passed, 0 failed, 4 ignored**（+1 弃置闸：verify/converge 弃置 / exec/orch 放行；ensure_dirs 断言改反向）。

- **prompts stats 批次提交惯例**:V59 实时录入每次任务 PASS 写 `usage_count += 1` / `version += 1` 到 prompts YAML（纯统计衍生,无契约价值,§20 原则）。随批次提交（跑完一轮后一次 commit,message 标注「运行时统计批次」）,不逐任务碎提交;content/契约字段变化则单独提交。
