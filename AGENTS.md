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
- **测试**: `cargo test`（124 pass, 6 ignored, 5 doc-test ignored）。单个测试: `cargo test <test_name>`。
- **格式化**: 未配置 cargo fmt / clippy。
- **Vendor**: Rig v0.39 本地化在 `vendor/`，通过 `Cargo.toml` 的 `[patch.crates-io]` 重定向。`cargo package --allow-dirty` 可验证 vendor 自恰性。**不要直接修改 vendor 目录，除非明确需要修补 Rig 源码。**

### 配置文件
- 配置来源**仅配置文件**（不读环境变量），搜索顺序: `.taiji/config.json` → `taiji.config.json`。
- `api_key` 为空是硬错误。
- CLI: `taiji run <desc...>` / `taiji init` / `taiji trace <id>` / `taiji list` / `taiji status` / `taiji mcp`。

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

## 3. Agent 关键约束

### AgentMode（重要）
- `AgentMode` 是 `Orchestration` | `Execution`，**不由 depth 自动推导**，由父 LLM 在 `SubtaskSpec.mode` 中显式分配。
- depth=0 固定 Orchestration；depth+1 >= max_depth 时 `RecursiveDecomposeTool` 强制覆盖为 Execution。
- `TpnCycle.execute()` 必须接收 `mode: AgentMode`，并逐层向下传播到 FittingAgent 和 CausalAgent。
- FittingAgentBuilder 构造时接收 mode，据此选择 system prompt 模板——不允许运行时动态切换。

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
- 动态工具（L1 Skills）通过 `.tools(Vec<Box<dyn ToolDyn>>)` 注册。
- Rig 有 `impl<T: Tool> ToolDyn for T` blanket impl，实现 `Tool` 后自动获得 `ToolDyn`，无需重复实现。

### 内置 L1 Skills（占位实现）
`read`、`write`、`bash`、`search`、`webfetch` 均为占位实现（返回模拟结果），位于 `src/agents/tools/skills/mod.rs`。

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
├── grids/       ← L3 推理角色
├── models/      ← L2 贝叶斯经验
├── skills/      ← L1 可执行工具
└── index.yaml   ← tag 反向索引（衍生数据，自动维护）
```

- TPN 执行期间**只读**，DMN Consumer 是唯一写者。
- `save_asset()` 前必须 `load_asset()` 确认版本不冲突，写入时 `version++`。
- `index.yaml` 损坏时从原始 YAML 重建。`traverse_relations()` BFS 必须 dedup visited set。

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
