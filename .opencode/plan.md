# 实现计划 — 阶段 B：Meta/Causal 相位「LLM + 收集工具」落地（V25 蓝图对齐）

## 目标

按用户框架要求「Meta 与 Causal 都需要 LLM 与收集信息的工具，才能更新权重、验证收敛」+「需要能上网查询网络信息核实」，将 MetaAgent / CausalAgent 从「无工具」升级为「LLM + 只读收集工具（read / search / webfetch）+ SafetyHook 挂载」，使蓝图 V25（§1.2 权限面、§8.2 权限同构、§8.5 Hook 安全模型）与代码一致。**webfetch 归入收集工具**（只读网络信息获取，不改变世界），write / bash / recursive_decompose / causal_verify 为仅 Fitting 持有的执行工具。

**前置状态**：A1 安全验证、A2 WorkerPool 整改已完成并验收（144 passed / 0 failed / 9 ignored）。本计划独立于此，不重复。

## 现状事实（已审计确认，2026-08-05）

- **LLM 已存在，注释过时**：`src/agents/causal.rs` `verify()`（L236-277）与 `converge()`（L471-504）**已真实调用 LLM**（`agent.prompt` → serde 解析 `VerificationReport`/`ConvergenceDecision`），但文档注释仍写"degraded mode（跳过 LLM）"——过时注释需修正。`src/agents/meta.rs` `run()`（L126-136）也已调 LLM 做 MetaContext 编排。
- **真正的缺口 = 收集工具 + 安全钩子**：
  - MetaAgent：Rig agent 无任何工具注册（`max_turns=1` 单次提取，L129 `.default_max_turns(1)`），LLM 无法主动收集任务上下文/父层 deliverables/归藏资产
  - CausalAgent：verify/converge 的 agent 均未注册工具（L237-242、L472-477 只有 preamble/max_tokens/max_turns/build），**但系统提示模板明确要求「MUST use the read tool to open each referenced file」**（causal.rs L315/L351/L541/L572）——模板要求与工具注册脱节，LLM 无法真正逐文件验证
  - 三相位均未挂 SafetyHook（factory 中 `Arc<SafetyHook>` 单例已存在，仅 FittingAgent 挂载）
- **追踪约束**（§7.2，不改）：元/阴相位用手动 TraceWriter 单条记录，**Meta/Causal 不加 TraceHook**，仅加 SafetyHook。
- **测试现状**：causal.rs LLM 测试已 `#[ignore]`（L630/L666）；非 ignore 的 `test_verify_empty_summary_triggers_back_to_meta`（L645）走 ConstraintEngine Hard 短路不调 LLM——144 基线无真实 LLM 调用。
- factory.rs：`safety_hook: Arc<SafetyHook>`（L56），`create_meta_agent`（L111）构造 `MetaAgentBuilder::new(task_id, liluo, providers, model)` 后直接返回——需链式接线。
- 命名约束（AGENTS.md §1）：新代码用 `GuizangClient`/`guizang`/「归藏」，不改既有 `LiluoClient` 旧名。

## 模块清单

- [ ] `src/agents/meta.rs` — MetaAgentBuilder 加收集工具 + SafetyHook + max_turns 提升
- [ ] `src/agents/causal.rs` — CausalVerifyAgentBuilder / CausalConvergeAgentBuilder 加 read 工具 + SafetyHook；修正过时注释
- [ ] `src/agents/factory.rs` — create_meta_agent / create_causal_verify_agent / create_causal_converge_agent 接线 SafetyHook（与 create_fitting_agent 同一单例 Arc）
- [ ] `src/agents/meta.rs` / `src/agents/causal.rs` 测试 — 新增 builder 配置断言测试；确认既有非 ignore 测试仍不触发真实 LLM

## 接口签名（关键变更）

```rust
// ── src/agents/meta.rs ──
pub struct MetaAgentBuilder {
    task_id: String,
    liluo: Arc<LiluoClient>,
    provider: Arc<ProviderRegistry>,
    model: String,
    max_turns: u32,                      // 默认 1 → 6（允许工具循环后二次提取）
    task_dir: Option<PathBuf>,
    safety_hook: Option<Arc<SafetyHook>>, // 新增
}

impl MetaAgentBuilder {
    pub fn safety_hook(mut self, hook: Arc<SafetyHook>) -> Self; // 新增 setter
    pub fn max_turns(mut self, n: u32) -> Self;                  // 新增 setter（默认值 6）
    // run() 内 agent 构建：.tool(read).tool(search) + safety_hook 为 Some 时 .hook(...)
}

// ── src/agents/causal.rs ──
pub struct CausalVerifyAgentBuilder {
    // ...既有字段
    safety_hook: Option<Arc<SafetyHook>>, // 新增
}
pub struct CausalConvergeAgentBuilder {
    // ...既有字段
    safety_hook: Option<Arc<SafetyHook>>, // 新增
}
// 两 builder 各新增 pub fn safety_hook(mut self, hook: Arc<SafetyHook>) -> Self
// verify()/converge() 内 agent 构建：.tool(ReadTool) + safety_hook 为 Some 时 .hook(...)

// ── src/agents/factory.rs（接线，签名不变）──
create_meta_agent:        MetaAgentBuilder::new(...).max_turns(6).safety_hook(self.safety_hook.clone())
create_causal_verify_agent:   ...builder.safety_hook(self.safety_hook.clone())
create_causal_converge_agent: ...builder.safety_hook(self.safety_hook.clone())
```

## 依赖顺序

1. **B1 `src/agents/meta.rs`**：新增 `safety_hook` 字段 + setter；`max_turns` 默认 1→6 + setter；`run()` 构建 agent 时注册只读收集工具 read + search + webfetch（复用 `src/agents/tools/skills/` 既有实现），`safety_hook` 为 Some 时挂载。更新 L37 附近 `#[allow(dead_code)]` 注释（若字段不再死代码则移除该 allow，消除既有警告）。
2. **B2 `src/agents/causal.rs`**：两 builder 新增 `safety_hook` 字段 + setter；`verify()`/`converge()` 构建 agent 时注册 read + webfetch 工具 + 挂 SafetyHook；修正过时注释（L158-162、L442-444「degraded mode」→ 实际 LLM 路径描述：ConstraintEngine 预检 → LLM 逐文件验证 + 联网核实 → 结构化裁决；LLM 失败 → `LLMCallFailed`）。
3. **B3 `src/agents/factory.rs`**：三个 create_* 方法接线（依赖 B1/B2 的 setter）。
4. **B4 测试与回归**：新增 builder 配置测试（safety_hook/max_turns 断言）；`cargo test --lib` 全量回归；`cargo build` 检查无新增警告；清理修改文件中的旧 `use` 导入/死字段。

## 验收标准

- [ ] `cargo build` 无新增警告（允许既有 3 个 lib 警告 + vendor cfg 警告）
- [ ] `cargo test --lib` 全绿：144 基线 + 新增测试，0 failed（LLM 相关测试维持 `#[ignore]`）
- [ ] 蓝图 V25 与代码一致：MetaAgent 注册 read+search+webfetch、CausalAgent verify/converge 均注册 read+webfetch、三相位（Meta/Fitting/Causal）挂载同一 SafetyHook 单例 Arc
- [ ] 权限分工符合 §1.2：Meta/Causal 仅只读收集工具（read/search/webfetch），无 write/bash/recursive_decompose 等执行工具；webfetch 受 SafetyHook SSRF 检查约束（check_web_url）
- [ ] 模板要求与工具注册闭合：causal.rs 系统提示中 read 工具要求（L315/L351/L541/L572）不再悬空；webfetch 供 LLM 按需联网核实（build 可酌情在 verify/converge 模板补充联网核实引导句）
- [ ] Meta/Causal 不挂 TraceHook（§7.2 手动 TraceWriter 约定不变）
- [ ] 新代码命名遵守 Guizang/guizang/归藏；修改文件无残留死代码/旧 use

## 范围外（明确不做）

- Causal/Meta LLM 调用失败**重试机制**（AGENTS.md §6 提到重试 3 次，但现状代码无重试——既有偏差，本次不扩范围，单独跟进）
- TraceHook 挂载 Meta/Causal（蓝图 §7.2 明确手动记录）
- 温度调整（四象温度表不变，Meta 不在表内）
- PlanBuilder（预演编排，不进 TPN，非目标相位；如内部复用 MetaAgentBuilder 路径则顺带获得工具）
- 前端 / ChatAgent / DMN 激活
