# 实现计划 — 阶段 C：异层同构收敛（V26 蓝图对齐）

## 目标

删除 `AgentMode` 分裂，使**子任务与根任务完全同构**（代码结构 / 工具面 / prompt 模板 / 参数 / 日志 / 持久化 / 恢复 / 状态管理全部一致），根任务获得恢复入口，WorkerPool permit 语义简化为「并行分解节点上限」，项目回归简单简洁。

**用户原话**：「为什么异层同构不会生效，所有的子任务应当与根任务一致才对（代码结构、日志工具、持久化等等），我们的项目应该很简单简洁」

**前置状态**：A1/A2/B 阶段完成并提交（151 passed / 0 failed / 9 ignored）。冒烟 + T1-T4 实测完成。

## 现状事实（已审计确认，2026-08-05）

三个断裂点（详见 T4 实测分析）：

1. **权限同构断裂**：`AgentMode`（Orchestration / Execution）导致工具面随 depth 分化——Execution 模式 FittingAgent 不注册 `recursive_decompose`（`recursive_decompose.rs:110-114` guard + `fitting.rs` 注册分支），与蓝图 §8.2「权限与 depth 无关」矛盾。58 处 AgentMode 引用（types/agent.rs:15 枚举、MetaContext.mode、PromptAsset.agent_mode、SubtaskSpec.mode、TaskNode.mode、fitting/causal 模板分支、tpn_cycle/runner 参数、ws/types、plan.rs、task_tree_builder）。
2. **恢复不对称**：根任务 `runner.rs:59` 每次 `Uuid::new_v4()`，恢复链（`resume_history > decompose_result.json > checkpoint.json`，tpn_cycle.rs:104-115）对根任务永不触发；子任务有 children/ 扫描复用 + rerun_of。实测 0f172693 等超时任务变孤儿。
3. **状态/超时不对称**：`runner.rs:126-133` timeout 路径不更新 task.status、不调 cancel；status 只在成功路径 L135 更新。NodeStatus 已有 Failed/Cancelled 变体（task_tree_builder.rs:166-167），前端协议无需新增。

附加实测问题（纳入本阶段）：verify/converge `max_turns=3`（causal.rs:91/442）真实任务不足（T4 子任务与父任务均 MaxTurnsError）；exec_timeout 默认 60s（config.rs:104）与蓝图 §8.6 600s 不符。

## 模块清单（按依赖顺序编号）

- [ ] C1 `src/types/agent.rs` — **删除 `AgentMode` 枚举**（L15）与 `MetaContext.mode`（L44）；`PromptAsset.agent_mode`（L191）删除（serde 默认宽容，旧归藏 YAML 自动兼容零迁移）
- [ ] C2 `src/types/task.rs` — 删除 `SubtaskSpec.mode`（L29）
- [ ] C3 `src/types/frontend.rs` — 删除 `TaskNode.mode`（L59）
- [ ] C4 `src/types/plan.rs` — 删除 `agent_mode`（L22）/`mode`（L51）
- [ ] C5 `src/ws/types.rs` — 删除 `mode`（L45，若 TaskEvent 携带）
- [ ] C6 `src/agents/tools/recursive_decompose.rs` — 删除 mode guard（L110-114）、`SubtaskMeta.mode`（L212）、强制 Execution（L263）；**permit 语义改造**：工具入口 `acquire()` 1 个 permit 持有到 join 完成，spawn 闭包**不再**捕获 permit（L373-375 drop 移除）——子任务运行不持 permit，任意深度 decompose 在各自入口 acquire，无嵌套持有 → 无死锁
- [ ] C7 `src/agents/tools/causal_verify.rs` — 删除 `mode` 字段（L36/51）
- [ ] C8 `src/agents/fitting.rs` — 删除模式分支（L249/L305）：`build_system_prompt` 合并 ORC/EXEC 为单一模板（保留「拆解优先 + 执行优先」融合引导，prompt 中说明「可用 recursive_decompose 拆解，也可直接产出，由你判断」）；FittingAgentBuilder 删 `mode` 字段；工具注册无分支（全工具全层级）
- [ ] C9 `src/agents/causal.rs` — 模板合并：`VERIFY_ORC/EXEC` → 单一 `VERIFY_SYSTEM_PROMPT`，`CONVERGE_ORC/EXEC` → 单一（L240-241/L514-515）；删 `mode` 参数（L186/L498）；**max_turns 3→6**（L91/L442）
- [ ] C10 `src/agents/meta.rs` — 编排 prompt 删除 mode 决策指令；`MetaContext::empty()` 等构造适配；删测试中的 mode 断言（L358/382）
- [ ] C11 `src/agents/factory.rs` — `create_fitting_agent` 删 `mode` 参数（L168）；create_causal_* 同步
- [ ] C12 `src/orchestration/tpn_cycle.rs` — `execute()` 删 `mode` 参数（L101）；**status 统一管理**：每阶段结束原子写 `meta.json` status（Running→…→Completed/Failed/Cancelled，`save_json_atomic` 模式）；失败/取消路径写 Failed/Cancelled
- [ ] C13 `src/orchestration/runner.rs` — `execute_with_context` 加 `resume_task_id: Option<String>`：Some 时复用 task_id（跳过 L59 uuid）、从 meta.json 读 depth 恢复 EngineContext；timeout 分支（L126-133）改为 `cancel.cancel()` + 原子写 status=Failed + 返回错误；删 L129 mode 参数；成功路径保留
- [ ] C14 `src/orchestration/task_tree_builder.rs` — 删 L206-208 mode 分支
- [ ] C15 `src/infra/config.rs` — `exec_timeout` 默认 60→600（对齐蓝图 §8.6）；max_rounds/max_cycles 默认值与蓝图统一（以 config.rs 为准，蓝图表格更新）
- [ ] C16 `src/main.rs` — `taiji run` 增加 `--resume <task_id>` 解析，透传 runner
- [ ] C17 测试 — 适配：fitting/causal/meta/recursive_decompose/knowledge（L975-1021 agent_mode 断言删）/task_tree_builder/worker_pool；新增：根任务 resume 恢复测试、permit 新语义测试（decompose 并发上限）、status 失败写入测试、causal max_turns=6 断言
- [ ] C18 附带排查 — TraceHook tool_call 事件缺失（T4 问题 2）：确认 rig hook 事件完整性，缺失则补手动记录
- [ ] C19 前端 — `taiji-web/src/types/index.ts` 删 TaskNode.mode；组件渲染 mode 处清理；确认 NodeStatus.Failed 渲染（失败节点显示）

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

- [ ] `cargo build` 0 errors、无新增警告（既有 3 个 lib 警告允许）
- [ ] `cargo test --lib` 全绿：151 基线 + 新增，0 failed（LLM 相关维持 `#[ignore]`）
- [ ] `grep -rn AgentMode src/` 零命中（注释性历史说明可豁免）；前端 `grep AgentMode taiji-web/src/` 零命中
- [ ] 根任务与子任务执行同一 `TpnCycle::execute` 路径：工具面、模板、max_turns（Meta 6 / Fitting 30 / Causal 6）、目录布局、恢复链、status 写入全部一致——**无任何 depth 特例**
- [ ] permit 语义 = 并行分解节点上限：decompose 入口 acquire 1 个、join 后释放，spawn 闭包不持 permit；无死锁路径（持 permit 者不再 acquire）
- [ ] `taiji run --resume <task_id>` 可恢复超时/失败任务（实测 0f172693 可恢复或正确标记）
- [ ] 超时/取消/失败任务 `meta.json` status = Failed/Cancelled（前端不再 Running 残留）
- [ ] 蓝图 V26 与代码一致（§1.1/§1.2/§2/§5.2/§8.1/§8.2/§8.6/§8.8/§8.10 同步）

## 范围外（明确不做）

- LLM 工具错误后重试去重（T4 问题 3：max_turns=6 后缓解，观察）
- DMN 激活、CI/README/LICENSE、归藏资产 YAML 迁移（serde 宽容自动兼容）
- AgentMode 删除引起的 PromptAsset 兼容读取测试（旧 YAML 加载，如 knowledge.rs 测试改用无 agent_mode 资产）
