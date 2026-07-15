# 实现计划：绝对路径单向传递与多层递归收敛

## 目标

打通多层递归中 agent 输入输出端的文件路径传递链：**阳产出 → 绝对路径注入子 context → 子产出 → 返回父 → 阴收敛验证**。路径必须用绝对路径硬编码保证（不依赖 LLM 推断），权限单向向下覆盖。

## 数据流设计

```
阳 (FittingAgent) 产出文件
    │  TPNResult.deliverables = ["/abs/path/task/deliverables/design.md", ...]
    │
    ├─ recursive_decompose ─────────────────────────┐
    │  → 子 YangPrompt.parent_deliverables          │ 单向向下注入
    │     = 父 TPNResult.deliverables (只读参照)     │
    │                                                │
    ▼                                                ▼
阴 (CausalAgent)                               阳 (子 FittingAgent)
  verify()                                      读取 parent_deliverables
  ├── 接收 deliverable 路径列表                  产出自己的 deliverables
  └── prompt 硬编码要求读文件验证                  TPNResult.deliverables (绝对路径)
                                                      │
  converge()                                          │
  ├── 接收子 DecomposeResult.deliverables             │
  └── prompt 硬编码要求逐文件检查 ←──────────────────┘ 向上传递
```

## 模块清单

- [ ] **types/task.rs** — `DecomposeResult +deliverables: Vec<String>`
- [ ] **types/agent.rs** — `YangPrompt +parent_deliverables: Vec<String>`
- [ ] **agents/fitting.rs** — Execution/Orchestration 硬编码模板加 deliverable 路径指令
- [ ] **agents/fitting.rs** — `run()` 填充 `TPNResult.deliverables`（解析 LLM 输出中的路径列表）
- [ ] **agents/causal.rs** — verify/converge 硬编码模板要求读文件验证
- [ ] **agents/tools/recursive_decompose.rs** — 子 TPNResult.deliverables → DecomposeResult.deliverables
- [ ] **agents/tools/recursive_decompose.rs** — 父 deliverables → 子 MetaContext.parent_deliverables 注入
- [ ] **.taiji/knowledge/prompts/*.yaml** — 6 个种子模板全量刷新

## 接口签名

```rust
// DecomposeResult 新增字段
pub struct DecomposeResult {
    pub summary: String,
    pub status: ConvergenceStatus,
    pub subtask_count: u32,
+   pub deliverables: Vec<String>,  // 所有子任务的产物绝对路径聚合
}

// YangPrompt 新增字段
pub struct YangPrompt {
    pub task_description: String,
    pub reasoning_path_summaries: Vec<String>,
    pub constraint_summaries: Vec<String>,
+   pub parent_deliverables: Vec<String>,  // 父层产物绝对路径（只读参照）
}
```

## 依赖顺序

```
1. types/task.rs + types/agent.rs   ← 类型先行
2. agents/fitting.rs               ← 阳 agent 硬编码 + 路径填充
3. agents/causal.rs                ← 阴 agent 硬编码
4. recursive_decompose.rs          ← 路径传递桥
5. 种子 YAML 刷新                  ← 模板落地
6. BCP V11 更新                    ← 蓝图最后归档
```

## 硬编码保证（关键设计决策）

每份硬编码模板 / 种子 YAML 中必须包含以下不可被 LLM 绕过的指令：

| Agent | 模板 | 硬编码指令 |
|-------|------|-----------|
| FittingAgent (Execution) | execution_fitting.yaml | "所有产物文件的**绝对路径**必须在 TPNResult.deliverables 字段中逐一列出。如果你的工作目录是 `/data/tasks/{id}/`，写出 `/data/tasks/{id}/deliverables/report.md` 而非 `report.md`" |
| FittingAgent (Orchestration) | orchestration_fitting.yaml | "产物文件的绝对路径需列出。若你使用 recursive_decompose 拆解任务，子任务产物路径将在收敛阶段由 CausalAgent 汇总可见" |
| CausalAgent (verify) | exec/orc_verify.yaml | "接收 deliverables 字段中的文件绝对路径列表。你必须调用 read 工具逐一读取并检查文件内容是否满足约束" |
| CausalAgent (converge) | exec/orc_converge.yaml | "接收每个子任务的 deliverables 字段，包含所有子产物的绝对路径。你必须逐一读取每个文件，检查跨子任务的一致性和完整性" |

## 权限模型

- 父层 deliverables 对子层是**只读参照**（注入到 `parent_deliverables`）
- 子层只能写入自己的 `task_dir/deliverables/`
- 兄弟节点目录不可见（结构保证，不通过路径渗透）
- 绝对路径以 `task_dir` 为根，每层递归有自己的 `task_dir`

## 验收标准

- `cargo build` 零错误
- `cargo test` 全部通过
- `DecomposeResult` 包含 `deliverables` 字段
- `YangPrompt` 包含 `parent_deliverables` 字段
- Execution 模板中明确要求 LLM 列出绝对路径
- verify/converge 模板中明确要求 LLM 调用 read 工具检查文件
- `recursive_decompose` 将子 TPNResult.deliverables 传递到 DecomposeResult
- `recursive_decompose` 将父 deliverables 注入子 MetaContext
- BCP §4/§5/§8.9 与代码一致
