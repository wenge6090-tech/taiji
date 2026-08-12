# AI 行为约束（自动加载）

> taiji Rust 规则清单。BCP-蓝图-完型协议.md 是唯一事实，本文件是实施避坑补充。

---

## 0. BCP 首要规则

- **先更新 BCP，后执行修改**：任何涉及模块结构、类型设计、接口契约、数据流的变更必须先更新 `BCP-蓝图-完型协议.md`。
- 纯内部实现细节（bug 修复、测试补全、重构不改变接口）无需更新 BCP。
- BCP 与代码冲突时 BCP 优先；实现层命名不一致以代码为准，不修改蓝图。

## 1. 项目结构与关键约定

### Rust 项目
- **语言**: Rust 2024 edition，单 crate 项目 `taiji`。
- **构建**: `cargo build`。
- **测试**: `cargo test`。单个测试: `cargo test <test_name>`。
- **Vendor**: Rig v0.39 本地化在 `vendor/`，通过 `[patch.crates-io]` 重定向。**不要直接修改 vendor 目录，除非明确需要修补 Rig 源码。**

### 配置文件
- 配置来源**仅配置文件**（不读环境变量），搜索顺序: `.taiji/config.json` → `taiji.config.json`。
- `api_key` 为空是硬错误。

### 命名约定
- BCP 已统一命名为 **归藏 (Guizang)**。新代码使用 `GuizangClient` / `guizang`；已有旧代码中的 `LiluoClient` 可保留。

## 2. 周易 TPN 循环防护

- `BACK_TO_TPN` 递增 `round_counter`，达 `max_rounds` 时只能返回 PASS/FAIL，禁止再跳转。
- `BACK_TO_META` 递增 `cycle_counter`，达 `max_cycles` 时只能返回 PASS/FAIL。
- `recursive_decompose` 创建子任务前必须检查 `depth < max_depth`（默认 2），超限返回错误。
- 子任务数量上限 `max_subtasks`（默认 4），超出截断。
- `CancellationToken` 必须通过 `child_token()` 传递到所有递归层级。
- 子任务并发使用 `JoinSet::spawn`，`join_next()` 流式收集，任何子任务失败时 `abort_all()` 清理。

## 3. Agent 关键约束

- **AgentMode 阴阳配对**：`Orchestration`（编排拆解+综合）| `Execution`（直接产出）。由 MetaAgent 权重更新时按递归层数规则 + 任务难易程度决策。`depth+1 >= max_depth` 时强制 Execution。
- **配对模板**：Orchestration → 阳编排+阴收敛；Execution → 阳执行+阴验证。
- **工具注册**：`recursive_decompose` 仅 Orchestration 模式 FittingAgent 注册。Execution 模式 LLM 不可见此工具。
- **四象温度默认值**：FittingOrch 0.8 / FittingExec 0.5 / CausalVerify 0.2 / CausalConverge 0.2。

## 4. 带工具必有安全钩子（硬约束）

- 任何注册工具的 Agent 必须挂载 SafetyHook。Meta/Causal 注册只读收集工具（read/search/webfetch），Fitting 注册执行工具（read/write/bash/search/webfetch + recursive_decompose/causal_verify）。
- **Rig 0.39 `AgentBuilder::hook()` 是单槽覆盖式**：`.hook(a).hook(b)` 只有最后一个生效。FittingAgent 的多 hook 必须经 `FittingHookSet` 组合（safety → trace → snapshot），一次 `.hook()` 挂载。

## 5. 错误处理与测试

- `TaijiError` 变体必须携带 `context: String`。
- LLM 调用失败重试 3 次 → 降级 → `TaijiError::LLMCallFailed`。
- async 上下文中禁止 `panic!` / `unwrap()`，全部用 `Result`。
- 测试中创建的临时目录用 `tmp_dir`，测试末尾必须清理。并行测试的临时目录必须唯一（静态 `AtomicUsize` 计数器）。

## 6. LLM 结构化输出解析

- **LLM 响应解析一律走 `src/infra/json_util.rs` 的 `parse_llm_json<T>`**，禁止直接 `serde_json::from_str` 解析 LLM 输出。
- `parse_llm_json` 四级容错：① 直接解析 → ② ` ```json ` 围栏提取 → ③ 全文首尾大括号切片 → ④ 返回原始错误。

## 7. 上下文窗口预算（BCP §8.19）

- `handoff_tokens`（默认 250k）：超限 → 写交接文件 → BACK_TO_TPN。
- `hard_cutoff_tokens`（默认 300k）：硬截止 → 写交接文件 → FAIL。
- `max_turns` 降级为防死循环兜底（200），不承担上下文管理。

## 8. 无降级原则

- 新代码读身份册失败 / 会盟扫描失败 / 归藏 I/O 失败 → `TaijiError` 上抛（错误信息必须携带路径）。
- 「无父（根任务）」与「无兄弟」是**状态分支**，非降级——不应用 `unwrap_or_default()` 吞错。
- 既有降级点（MetaContext::empty、Base 模板、LLM 重试等）维持现状，改造另立章节。
