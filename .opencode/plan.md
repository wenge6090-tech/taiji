# 实现计划 — 阶段 C：异层同构收敛（V26 蓝图对齐）✅ + V26.1 修复

## 目标

删除 `AgentMode` 分裂，使**子任务与根任务完全同构**（代码结构 / 工具面 / prompt 模板 / 参数 / 日志 / 持久化 / 恢复 / 状态管理全部一致），根任务获得恢复入口，WorkerPool permit 语义简化为「并行分解节点上限」，项目回归简单简洁。

**用户原话**：「为什么异层同构不会生效，所有的子任务应当与根任务一致才对（代码结构、日志工具、持久化等等），我们的项目应该很简单简洁」

**前置状态**：A1/A2/B 阶段完成并提交（151 passed / 0 failed / 9 ignored）。冒烟 + T1-T4 实测完成。

**V26 状态**：✅ 已实现并提交（`660c4ea`，2026-08-06）——C1-C19 全部完成，E2E 验收通过（见下方「V26 验收结果」）。V26.1 修复轮见文末。

## 现状事实（已审计确认，2026-08-05）

三个断裂点（详见 T4 实测分析）：

1. **权限同构断裂**：`AgentMode`（Orchestration / Execution）导致工具面随 depth 分化——Execution 模式 FittingAgent 不注册 `recursive_decompose`（`recursive_decompose.rs:110-114` guard + `fitting.rs` 注册分支），与蓝图 §8.2「权限与 depth 无关」矛盾。58 处 AgentMode 引用（types/agent.rs:15 枚举、MetaContext.mode、PromptAsset.agent_mode、SubtaskSpec.mode、TaskNode.mode、fitting/causal 模板分支、tpn_cycle/runner 参数、ws/types、plan.rs、task_tree_builder）。
2. **恢复不对称**：根任务 `runner.rs:59` 每次 `Uuid::new_v4()`，恢复链（`resume_history > decompose_result.json > checkpoint.json`，tpn_cycle.rs:104-115）对根任务永不触发；子任务有 children/ 扫描复用 + rerun_of。实测 0f172693 等超时任务变孤儿。
3. **状态/超时不对称**：`runner.rs:126-133` timeout 路径不更新 task.status、不调 cancel；status 只在成功路径 L135 更新。NodeStatus 已有 Failed/Cancelled 变体（task_tree_builder.rs:166-167），前端协议无需新增。

附加实测问题（纳入本阶段）：verify/converge `max_turns=3`（causal.rs:91/442）真实任务不足（T4 子任务与父任务均 MaxTurnsError）；exec_timeout 默认 60s（config.rs:104）与蓝图 §8.6 600s 不符。

## 模块清单（按依赖顺序编号）

- [x] C1 `src/types/agent.rs` — **删除 `AgentMode` 枚举**（L15）与 `MetaContext.mode`（L44）；`PromptAsset.agent_mode`（L191）删除（serde 默认宽容，旧归藏 YAML 自动兼容零迁移）
- [x] C2 `src/types/task.rs` — 删除 `SubtaskSpec.mode`（L29）
- [x] C3 `src/types/frontend.rs` — 删除 `TaskNode.mode`（L59）
- [x] C4 `src/types/plan.rs` — 删除 `agent_mode`（L22）/`mode`（L51）
- [x] C5 `src/ws/types.rs` — 删除 `mode`（L45，若 TaskEvent 携带）
- [x] C6 `src/agents/tools/recursive_decompose.rs` — 删除 mode guard（L110-114）、`SubtaskMeta.mode`（L212）、强制 Execution（L263）；**permit 语义改造**：工具入口 `acquire()` 1 个 permit 持有到 join 完成，spawn 闭包**不再**捕获 permit（L373-375 drop 移除）——子任务运行不持 permit，任意深度 decompose 在各自入口 acquire，无嵌套持有 → 无死锁
- [x] C7 `src/agents/tools/causal_verify.rs` — 删除 `mode` 字段（L36/51）
- [x] C8 `src/agents/fitting.rs` — 删除模式分支（L249/L305）：`build_system_prompt` 合并 ORC/EXEC 为单一模板（保留「拆解优先 + 执行优先」融合引导，prompt 中说明「可用 recursive_decompose 拆解，也可直接产出，由你判断」）；FittingAgentBuilder 删 `mode` 字段；工具注册无分支（全工具全层级）
- [x] C9 `src/agents/causal.rs` — 模板合并：`VERIFY_ORC/EXEC` → 单一 `VERIFY_SYSTEM_PROMPT`，`CONVERGE_ORC/EXEC` → 单一（L240-241/L514-515）；删 `mode` 参数（L186/L498）；**max_turns 3→6**（L91/L442）
- [x] C10 `src/agents/meta.rs` — 编排 prompt 删除 mode 决策指令；`MetaContext::empty()` 等构造适配；删测试中的 mode 断言（L358/382）
- [x] C11 `src/agents/factory.rs` — `create_fitting_agent` 删 `mode` 参数（L168）；create_causal_* 同步
- [x] C12 `src/orchestration/tpn_cycle.rs` — `execute()` 删 `mode` 参数（L101）；**status 统一管理**：每阶段结束原子写 `meta.json` status（Running→…→Completed/Failed/Cancelled，`save_json_atomic` 模式）；失败/取消路径写 Failed/Cancelled
- [x] C13 `src/orchestration/runner.rs` — `execute_with_context` 加 `resume_task_id: Option<String>`：Some 时复用 task_id（跳过 L59 uuid）、从 meta.json 读 depth 恢复 EngineContext；timeout 分支（L126-133）改为 `cancel.cancel()` + 原子写 status=Failed + 返回错误；删 L129 mode 参数；成功路径保留
- [x] C14 `src/orchestration/task_tree_builder.rs` — 删 L206-208 mode 分支
- [x] C15 `src/infra/config.rs` — `exec_timeout` 默认 60→600（对齐蓝图 §8.6）；max_rounds/max_cycles 默认值与蓝图统一（以 config.rs 为准，蓝图表格更新）
- [x] C16 `src/main.rs` — `taiji run` 增加 `--resume <task_id>` 解析，透传 runner
- [x] C17 测试 — 适配：fitting/causal/meta/recursive_decompose/knowledge（L975-1021 agent_mode 断言删）/task_tree_builder/worker_pool；新增：根任务 resume 恢复测试、permit 新语义测试（decompose 并发上限）、status 失败写入测试、causal max_turns=6 断言
- [x] C18 附带排查 — TraceHook tool_call 事件缺失（T4 问题 2）：确认 rig hook 事件完整性，缺失则补手动记录（**E2E 未复现**：实际工具调用全部有 trace 记录）
- [x] C19 前端 — `taiji-web/src/types/index.ts` 删 TaskNode.mode；组件渲染 mode 处清理；确认 NodeStatus.Failed 渲染（失败节点显示）

## 接口签名（关键变更）

```rust
// tpn_cycle.rs — 删 mode 参数
pub async fn execute(
    &self,
    description: &str,
    initial_meta_ctx: Option<MetaContext>,
    engine_ctx: &mut EngineContext,
    cancel: CancellationToken,
) -> Result<TPNResult, TaijiError>;

// runner.rs — 恢复入口
pub async fn execute_with_context(
    &self,
    description: &str,
    external_ctx: Option<ExternalContext>,
    resume_task_id: Option<String>,   // 新增：Some 时复用 task_id + 恢复链
) -> Result<TPNResult, TaijiError>;

// fitting.rs — 单模板单模式
fn build_system_prompt(meta_ctx: &MetaContext, task_dir: &Path, context_dir: Option<&Path>) -> String;

// recursive_decompose.rs — 入口持 permit
pub async fn execute(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError>;
// 内部：let _permit = self.pool.acquire().await?;  // 工具入口，持有到 join 完成
```

## 依赖顺序

1. **C1-C5** types 层删枚举/字段（编译错密集期，按此顺序先清）
2. **C6-C7** 工具层（decompose permit 语义 + causal_verify）
3. **C8-C11** agent 层（fitting/causal/meta/factory）
4. **C12-C14** orchestration（tpn_cycle status 统一 → runner resume+timeout → task_tree_builder）
5. **C15-C16** config + CLI
6. **C17** 测试适配与新增
7. **C18** trace 排查（可与 1-6 并行观察）
8. **C19** 前端同步

## 验收标准

- [x] `cargo build` 0 errors、无新增警告（既有 2 个 lib 警告允许）
- [x] `cargo test --lib` 全绿：156 passed / 0 failed / 9 ignored（AGENTS.md 记录 142 为 V24 基线）
- [x] `grep -rn AgentMode src/` 仅 3 处注释性历史说明（src/types/agent.rs:15、plan.rs:291-292 测试断言），零代码引用，豁免；前端 `grep AgentMode taiji-web/src/` 零命中
- [x] 根任务与子任务执行同一 `TpnCycle::execute` 路径：工具面、模板、max_turns（Meta 6 / Fitting 30 / Causal 10）、目录布局、恢复链、status 写入全部一致——**无任何 depth 特例**
- [x] permit 语义 = 并行分解节点上限：decompose 入口 acquire 1 个、join 后释放，spawn 闭包不持 permit；无死锁路径（持 permit 者不再 acquire）
- [x] `taiji run --resume <task_id>` 可恢复超时/失败任务：task_id 复用、depth 从 meta.json 读回、恢复链日志正常（三次 resume 因 LLM 瞬态错误/任务过大/超时未收敛，机制本身验证通过）
- [x] 超时/取消/失败任务 `meta.json` status = Failed/Cancelled（实测 45s 超时 → Failed + checkpoint 保留）
- [x] 蓝图 V26 与代码一致（§1.1/§1.2/§2/§5.2/§8.1/§8.2/§8.6/§8.8/§8.10 同步）

## V26 验收结果（2026-08-07，E2E 完整验收）

- **回归基线**：`cargo build` 0 errors；`cargo test --lib` 156 passed / 0 failed / 9 ignored
- **真实任务冒烟**：task_id `d25fc099-89ee-429d-92b5-1447df1c4e03`，7 分 02 秒 → **Completed**；checkpoint 已删 / decompose_result 留（Converged）/ chat_history 164KB / trace 完整；无 children（LLM 自主判断不分解）
- **超时验证**：task_id `6f95dacd-d5bb-4b33-8d38-b2b1d5c87c24`，exec_timeout=45 → 45s 后 status=**Failed** + checkpoint 保留（phase=MetaDone）；exec_timeout 已恢复 600
- **--resume 验证**：恢复机制全部正常（task_id 复用、depth 读回、Crash recovery 日志）；三次 resume 因 LLM 瞬态 JSON 错误 / 任务过大 MaxTurnsError=30 / 600s 超时未收敛（非机制缺陷）
- **C18 未复现**：实际工具调用（bash×33 / read×4 / write×1 / causal_verify×2）全部有 trace tool_call 事件
- **前端**：`npm run build` 0 错误
- **git**：除 BCP 文档外干净（test_data 污染为发现 4，见下）

**E2E 暴露 4 个问题 → 全部进入 V26.1 修复轮**（用户确认）。

---

# V26.1 修复计划（E2E 实测问题收敛）

## 目标

收敛 V26 E2E 验收发现的 4 个问题：tools_used 统计伪阳性、causal max_turns 不足、resume 无对话增量恢复、测试污染已跟踪 test_data。

## 现状事实（2026-08-07 审计确认）

1. **tools_used 伪阳性**：`fitting.rs:288-296` 用 `response.contains("recursive_decompose")` / `contains("causal_verify")` + `skill_registry.get_tool_names()` 文本子串匹配统计——LLM 报告正文提到工具名即误计（冒烟任务 decompose_result 声称用过 recursive_decompose，实际 trace 无 tool_call 事件、subtask_count=0、无 children/）。
2. **causal max_turns=6 仍溢出**：真实任务第一次 causal_verify 触发 `MaxTurnsError: reached max turns limit: 6`（99.5s），重试 25.3s 成功——6 轮不够 LLM 逐文件 read 核验。
3. **resume 无对话增量恢复（根因）**：Rig `chat()`（vendor/rig-core/src/agent/completion.rs:392-407）`extended_details().await?` 出错即提前返回，`chat_history.extend(messages)` 仅成功时执行；fitting.rs:264-278 的 save 在 chat() 之后 → 失败时保存空历史（实测恒为 `[]`）。resume 每次从空历史重跑整个 Fitting 阶段，「继续完成」语义对大任务失效。**但 Rig `PromptHook::on_completion_call(prompt, history)` 暴露调用前完整对话**（src/hooks/trace.rs:198-202），可在每次 LLM 调用前（含工具循环内）快照。
4. **test_data 污染**：`fitting.rs:648 test_fitting_agent_depth_check` 仍把 task_dir 指向已跟踪 `test_data/tasks/depth-test`，`cargo test --lib` 后 git status 变脏（AGENTS.md §10 已写规则但测试未修）。

## 模块清单

- [ ] F1 `src/hooks/trace.rs` — TraceHook 增加 `tools_called: Arc<Mutex<Vec<String>>>`：`on_tool_call` 时 push 真实工具名（现只存 tool_starts map）；暴露 `pub fn tools_called(&self) -> Vec<String>`（去重保序）。**BCP §7.2 已声明该职责**
- [ ] F2 `src/hooks/chat_history_snapshot.rs`（新文件）— `ChatHistorySnapshotHook` 实现 rig `PromptHook`：`on_completion_call` 时把 `history.to_vec() + [prompt.clone()]`（`Vec<Message>`，与 chat_history.json 现有格式一致）用 `save_json_atomic` 原子快照到 `{task_dir}/chat_history.json`；写失败仅告警（不影响主流程，同 TraceHook 模式）；异步函数内禁止 panic/unwrap（AGENTS.md §6）。**BCP §8.1 已声明该策略**
- [ ] F3 `src/agents/fitting.rs` — ① FittingAgentBuilder 注册 `ChatHistorySnapshotHook`（hook 链顺序：safety → trace → snapshot，一次链式 build）；② `run()` 后 tools_used 改为 `trace_hook.tools_called()`（删 L288-296 contains 匹配，保留 skill_registry 不再需要）；③ 成功路径 L272 全量 save 保留（最终一致性）；④ 测试 `test_fitting_agent_depth_check`（L648）task_dir 改 tmp_dir（AGENTS.md §10：测试路径一律 tmp_dir，末尾 remove_dir_all）
- [ ] F4 `src/agents/causal.rs` — `max_turns: 6` → `10`（L91 CausalVerifyAgentBuilder + 对应 CausalConvergeAgentBuilder 默认值两处）
- [ ] F5 配置与蓝图 — `.taiji/config.json` causal max_turns 6→10（gitignore，仅本地）；BCP §8.2/§8.5/§8.1 已同步（V26.1 摘要 + 角色表 + 单 max_turns 行 + §7.2 TraceHook 职责 + §8.1 快照策略）
- [ ] F6 测试 — 新增：tools_used 真实记录断言（TraceHook on_tool_call 后 tools_called 含工具名）、ChatHistorySnapshotHook 快照测试（tmp_dir，验证 on_completion_call 后文件含 history+prompt）；适配既有断言（若有 max_turns=6 断言 → 10）
- [ ] F7 收尾 — `git checkout -- test_data/tasks/depth-test/trace.jsonl` 恢复 E2E 污染行（该文件被 cargo test 追加 completion_call 记录）；grep 确认无 tools_used contains 残留；清理编译警告

## 接口签名（关键变更）

```rust
// trace.rs — 真实工具调用记录
pub fn tools_called(&self) -> Vec<String>;   // 去重保序（on_tool_call 收集）

// chat_history_snapshot.rs — 新 hook
#[derive(Clone)]
pub struct ChatHistorySnapshotHook { task_dir: Arc<PathBuf> }  // Arc<PathBuf> 保证 Clone + Send + Sync
impl ChatHistorySnapshotHook { pub fn new(task_dir: &Path) -> Self }
impl<M: CompletionModel> PromptHook<M> for ChatHistorySnapshotHook { /* on_completion_call 快照 */ }

// fitting.rs — tools_used 改真实记录（内部实现，签名不变）
```

## 依赖顺序

1. **F1** TraceHook 记录真实工具名（独立，无依赖）
2. **F2** ChatHistorySnapshotHook 新 hook（独立，无依赖）
3. **F3** fitting.rs 接线（依赖 F1+F2）+ 测试污染修复（同文件）
4. **F4** causal max_turns 10（独立）
5. **F5** 配置 + 蓝图同步（已先行完成）
6. **F6** 测试新增/适配（依赖 F1-F4）
7. **F7** 收尾：git checkout 恢复 test_data、残留清理（依赖全部）

## 验收标准

- [ ] `cargo build` 0 errors、0 新增警告
- [ ] `cargo test --lib` 全绿：156 基线 + 新增，0 failed（LLM 相关维持 `#[ignore]`）
- [ ] tools_used 来自 TraceHook 真实调用：`grep -n "contains(" src/agents/fitting.rs` 无工具名文本匹配残留；decompose_result.tools_used 与 trace tool_call 事件一致（无伪阳性）
- [ ] 失败/超时任务 `chat_history.json` 非空（至少含最近一次工具循环后快照），`--resume` 可从失败点增量继续（非空历史进入 Fitting 阶段）
- [ ] `cargo test --lib` 后 `git status` 干净（test_data/tasks/depth-test/ 无污染）
- [ ] causal max_turns 默认 = 10（代码 + .taiji/config.json + BCP §8.2 一致）
- [ ] BCP V26.1 与代码一致（§7.2/§8.1/§8.2/§8.5 同步）

## 范围外（明确不做）

- vendor Rig `chat()` 错误路径回写历史（改动 vendor 面大，快照 hook 方案已覆盖）
- verify/converge prompt 模板措辞优化（10 轮内大概率覆盖，观察后再定）
- DMN 激活、CI/README/LICENSE、归藏资产迁移

---

# V26.3 修复计划（任务 6f95dacd 分析结论落地）

## 目标

收敛任务 6f95dacd 深度分析（用户问题「完成情况与任务描述是否矛盾」）暴露的 4 个问题：abort 子任务状态不落盘、L1 Skills 工具参数契约断裂、trace 脱敏误伤、预算-规模错配。核心结论（已与用户确认）：无虚假完成（status=Failed 诚实），但存在三层不一致——①子任务真实产出交付物却停在 Running；②工具 schema 与实现契约断裂导致 LLM 反复摸索；③任务规模超出预算。

## 现状事实（2026-08-07 审计确认）

1. **abort 子任务状态残留 Running**：recursive_decompose.rs L277 join_set / L357 join_next / L371+L375 两处 `abort_all()` 错误路径——abort 后 children/ 子任务 meta.json status 停在 Running（实测 8 个子任务 7 个 Running、1 个 Failed），与系统宣称「超时/失败/取消正确落盘」不符；且 children/1、children/3 实际产出了完整 SECURITY 交付物（ENTRY-INFRA.md 220 行/29.8KB、ORCH.md 4 条高危链路）
2. **SkillTool 契约断裂（实锤）**：`src/agents/tools/skills/mod.rs`——L134 `SkillTool`、L217-219 `SkillToolArgs { input: Option<String> }`（Rig derive 单参）、L238-247 ToolDefinition 暴露 `input: string` 且 description 仅「Raw input arguments for the skill」（**无用法说明**）、L254-261 call 内 `serde_json::from_str(input).ok()` 二级解析失败则保留原字符串传给 `execute(&input)`。BashTool 读 `command` 键（bash.rs L22-33）、ReadTool 读 `path` 键（read.rs L22）——LLM 传 `{"input": "ls"}` 永远 `missing required 'command' argument`；唯一活路 `{"input": "{\"command\": \"ls\"}"}`（JSON 字符串塞 input）纯靠 LLM 试错摸索，每次 resume 重新踩坑
3. **trace 脱敏误伤**：`redact_sensitive`（trace.rs L109-150）value-based 正则 `[a-zA-Z0-9_-]{40,}` 误伤一切长字符串（UUID、任务 id、文件正文），LLM 读 .taiji/config.json 等文件内容被整段 REDACTED 遮蔽（config.json 的 api_key 应脱敏，但正文不该全灭）
4. **预算-规模错配**：src/ 56 个 Rust 文件 15,917 行，「逐一审计」在 30 turns/600s 内不可能；LLM 正确拆解（两次 decompose 共 8 子任务 depth=1）但无法聚合收敛；children/1 causal_verify 现场复现 MaxTurnsError=6（76s 重试成功，V26.1 已修 6→10）

## 模块清单

- [ ] E1 `src/agents/tools/recursive_decompose.rs` — **abort 子任务状态落盘**：L371/L375 两处 `abort_all()` 后统一遍历 `children/` 目录，将 meta.json status=Running 的子任务原子写为 Failed（复用 `write_task_status` 或 load-modify-save；写失败仅 warn 不阻断父任务错误传播）
- [ ] E2 `src/agents/tools/skills/mod.rs` + `bash.rs` + `read.rs` — **工具契约对齐（双保险）**：①ToolDefinition description 补充用法示例（「input 可传纯字符串命令，或传 JSON 字符串对象如 {\"command\": \"ls\"}」）；②BashTool/ReadTool 支持 `input` 键直读——`args.get("input")` 为字符串时直接当 command/path（不再要求 command/path 键）；write/search/webfetch 同步检查参数名（write 的 path/content、search 的 query、webfetch 的 url），一并容错或至少补 description
- [ ] E3 `src/hooks/trace.rs` — **脱敏精确化**：value-based 正则收紧——仅匹配明确密钥前缀（`sk-`/`ds-`/`ghp_` 等）或移除 value-based 规则、仅保留 key-based（api_key/token/secret/password 键名）；对 `tool_call::read` / `tool_call::bash` 的输出豁免 value-based 脱敏（key-based 保留）——api_key 仍脱敏但文件正文可见
- [ ] E4 `src/agents/fitting.rs` — **规模感知引导**：Base Fitting 模板（或 build_system_prompt 尾部）加一句「任务规模过大（大量文件/大量行数）时优先使用 recursive_decompose 拆解并按模块分批完成；单轮预算内无法逐一完成时，在报告/交付物中明确说明覆盖范围与未覆盖部分」——低优先、范围最小化
- [ ] E5 测试 — E1：单测模拟 abort 后 children meta.json 非 Running（tmp_dir + 伪造子任务目录）；E2：SkillTool/BashTool/ReadTool 单测——input 纯字符串与 JSON 字符串两种形式均可执行；E3：单测——40+ 字符 UUID 不再误伤、api_key 键仍脱敏、read 输出正文可见；E4：prompt 断言含规模引导文案
- [ ] E6 收尾 — `grep -rn` 确认无残留；cargo build 0 errors 0 新增警告；cargo test --lib 全绿（158 基线 + 新增）；cargo test 后 git status 干净（test_data 无污染）

## 接口签名（关键变更）

```rust
// recursive_decompose.rs — abort 后落盘（内部实现）
// join_set.abort_all() 之后：
//   for child in fs::read_dir(children_root)? { write_status_failed_if_running(&child) }
fn write_aborted_children_status(children_root: &Path);   // 写失败仅 warn

// skills/mod.rs — description 补充（ToolDefinition 构建处）
"input": { "description": "Raw input arguments for the skill. 可传纯字符串（命令/路径），或传 JSON 字符串对象（如 {\"command\": \"ls -la\"}）", "type": "string" }

// bash.rs / read.rs — input 键直读
let command = args.get("command").or_else(|| args.get("input").and_then(Value::as_str)).ok_or(ToolCallError)?;
```

## 依赖顺序

1. **E2** 工具契约（影响面最大，先行；独立无依赖）
2. **E1** abort 落盘（独立）
3. **E3** 脱敏精确化（独立）
4. **E4** prompt 引导（独立）
5. **E5** 测试（依赖 E1-E4）
6. **E6** 收尾验证

## 验收标准

- [ ] `cargo build` 0 errors、0 新增警告（基线 2 个允许）
- [ ] `cargo test --lib` 全绿：158 基线 + 新增，0 failed（LLM 相关维持 `#[ignore]`）
- [ ] E1：abort 后子任务 meta.json status != Running（新增单测）；父任务错误路径行为不变
- [ ] E2：BashTool/ReadTool 接受 `{"input": "纯字符串"}` 与 `{"input": "{\"command\":...}"}` 两种形式；5 个 L1 Skills 的 ToolDefinition description 均含用法说明
- [ ] E3：长 UUID/文件正文不再被误伤脱敏；api_key/token 键值仍脱敏；config.json 读出的 api_key 值隐藏但正文可读
- [ ] E4：Fitting 模板含规模引导（prompt 断言）
- [ ] `cargo test --lib` 后 `git status` 干净
- [ ] BCP 同步：V26.3 变更摘要 + §7.2（脱敏策略）+ §8.5（工具契约/description 约定）

## 范围外（明确不做）

- 任务预算机制动态化（max_rounds/exec_timeout 自适应任务规模）
- MetaAgent 降级 meta_ctx 增强（归藏资产引导）
- 子任务审计报告中的真实安全发现（WS 无认证、恢复链完整性校验、MCP max_depth 无上限、SafetyHook 正则绕过等）——属安全整改清单，另立计划
- WS 认证、前端改动、vendor 改动

---

# V26.2 清理计划（持久化去冗余）

## 目标

删除 2 个只写不读的死文件（`meta_conversation.json`、`converge_state.json`），将任务目录持久化文件清单固化进 BCP §8.1（新增文件必须先入清单，只写不读者禁止引入），恢复路径不受影响。

## 现状事实（2026-08-07 审计确认）

- **`meta_conversation.json`**（meta.rs:200-216 写）：内容 {task_description, llm_input, llm_response, meta_ctx} 四字段全部可推导——task_description→meta.json、llm_input/llm_response→trace.jsonl（completion_call/response 事件）、meta_ctx→meta_ctx.json（逐字节重复）。`MetaAgentBuilder.task_dir` 字段（meta.rs:57/86/87）仅为写它存在，调用点 tpn_cycle.rs:290/542。
- **`converge_state.json`**（causal.rs:524-537 写）：converge 由 RecursiveDecomposeTool 内部调用（recursive_decompose.rs:391-395）；崩溃窗口（converge 后崩溃）被「父任务失败→重跑父 Fitting（chat_history 增量恢复）→重新 decompose→children/ 扫描复用旧结果→重新 converge（幂等重放）」天然覆盖，全仓库无读点。
- 审计其余 9 项文件均有写有读（verify_state 的 report 是 VerifyDone 恢复必需品；round/cycle 字段与 checkpoint 同值但低收益保留）。

## 模块清单

- [ ] D1 `src/agents/meta.rs` — 删除 meta_conversation 写入块（L200-216）与 `task_dir` 字段（L57 注释/L86 setter/L87 实现）；`run()` 不再需要 task_dir
- [ ] D2 `src/orchestration/tpn_cycle.rs` — 删除 `create_meta_agent(...).task_dir(...)` 两处调用（L290/L542）
- [ ] D3 `src/agents/causal.rs` — 删除 converge_state 写入块（L524-537）与 L424 doc 注释中「persisted to converge_state.json」表述
- [ ] D4 测试适配 — 检查 meta.rs / causal.rs 测试是否断言 meta_conversation / converge_state 文件存在或 task_dir 行为，同步删除/改写；无则跳过
- [ ] D5 收尾 — `grep -rn "meta_conversation\|converge_state" src/` 零命中（注释性历史说明可豁免）；cargo build 0 errors 0 新增警告；cargo test --lib 全绿（158 基线）；git status 干净

## 依赖顺序

1. D1（meta 写点 + 字段）
2. D2（调用点清理，依赖 D1）
3. D3（causal 写点，独立）
4. D4（测试适配，依赖 D1-D3）
5. D5（验证收尾）

## 验收标准

- [ ] `cargo build` 0 errors、0 新增警告
- [ ] `cargo test --lib` 全绿：158 passed / 0 failed / 9 ignored（无测试删除，仅可能改写断言）
- [ ] `grep -rn "meta_conversation\|converge_state" src/` 零命中（注释性历史说明可豁免）
- [ ] `git status` 干净（含 cargo test 后 test_data 无污染）
- [ ] BCP §8.1 文件清单与代码一致（9 项，V26.2 已同步）；AGENTS.md §7 关键状态文件清单同步删除两项（verify agent 负责）
- [ ] 恢复链不受影响：`resume_history > decompose_result.json > checkpoint.json` 行为不变（死文件本无消费者，逻辑零改动）

## 范围外（明确不做）

- verify_state.round/cycle 字段精简（低收益，与 checkpoint 同值但 report 本体必要）
- checkpoint 与 meta.json 合并（不同抽象层：循环进度 vs 任务生命周期，合并风险大于收益）
- trace.jsonl 冗余评估（独立审计视角，非恢复数据）
- 既有历史任务目录中已产生的死文件清理（.taiji/tasks 被 gitignore，新代码不再产生即可）
