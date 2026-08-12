# taiji 周易层 BCP 对齐 — 交接文件

> 日期：2026-08-12（V44 更新：2026-08-13 去分区化）  
> 范围：提示词重设计 + 归藏目录重构 + BCP 蓝图更新  
> 基线：`cargo test --lib` = 277 passed / 0 failed / 9 ignored

---

## 〇、V44 去分区化（2026-08-13）

**决策**：取消围绕模型的分区资产树，改为**单一根级资产树**（BCP §10.1 已重写）。

- `GuizangClient` 删除 `for_model`/`partition`/`root_dir` 双路径 → 单 `data_dir`
- `migrate_to_partitioned` → `migrate_from_partitioned`（旧 `{model_key}/` 资产幂等合并回根）
- MetaAgent/CausalAgent 检索直连根级；连山回传统一落根（model_key 仅作统计键）
- `taiji seed <key>` 语义改为：从旧分区目录恢复种子到根
- 模型维度仅在统计层区分（model_stats.yaml 按 model_key 索引，UCB 路由不变）
- 磁盘已迁移：`.taiji/knowledge/yang|yin|models/` 根级，`deepseek-deepseek-v4-flash/` 已删

---

## 一、达成的一致设计决策

### 1. 三层提示词精简（6 资产 + 3 Base 模板）

| 层 | 资产位置 | 配对 |
|------|------|------|
| 元·权重更新 | `meta.rs::META_COMPOSE_SYSTEM_PROMPT` | 模式决策 + prompt 编排 |
| 阳·执行 | `yang/prompts/exec-fitting.yaml` | 直接产出（Execution） |
| 阳·编排 | `yang/prompts/orch-fitting.yaml` | 拆解+综合（Orchestration） |
| 阴·验证 | `yin/prompts/exec-verify.yaml` | 直接产出核验 |
| 阴·收敛 | `yin/prompts/orch-converge.yaml` | 子结果聚合判决 |

- 每份提示词三部分结构：角色声明 → 核心职责 → 输出格式/路由
- 资产层内容与 Base 模板语义同构（资产优先，Base 降级兜底）
- 剥离所有 V22-V37 历史版本标记

### 2. 命名统一

- `LiluoClient` → `GuizangClient`（结构体），旧名保留为 `pub type LiluoClient = GuizangClient` 兼容别名
- `AgentFactory` 字段：`liluo` → `guizang`
- 全仓 ~124 处引用已更新，277 测试全通过

### 3. 归藏目录结构：阴阳嵌套树（V44：根级单一资产树，无 {model_key}/ 层）

与 BCP §1.1 异层同构原则一致——归藏树与周易任务树同构（yang=decompose，yin=converge）：

```
.taiji/knowledge/
├── yang/                          ← 阳轨：生成/发散/执行
│   ├── prompts/                   ← 2 份教学提示词
│   └── skills/
│       ├── orch/                  ← 编排 Skill
│       └── exec/                  ← 执行 Skill（write/bash/search/webfetch/read）
├── yin/                           ← 阴轨：验证/收敛/裁决
│   ├── prompts/                   ← 2 份教学提示词
│   └── skills/
│       ├── verify/                ← 验证 Skill（exec 的阴面对偶）
│       └── converge/              ← 收敛 Skill（orch 的阴面对偶）
├── models/                        ← 贝叶斯后验（跨阴阳）
├── manifold/                      ← 流型拓扑（后置）
└── model_stats.yaml               ← (model_key × tag) 统计（按模型区分，路由依据）
```

### 4. Skill 资产统一字段契约

兼容 **Google A2A 协议 `AgentSkill` 标准**，叠加 taiji 特有的 MCTS 演化层：

```
A2A 标准层（外部 Agent 可发现）
─────────────────────────────
id, name, description, tags
examples, inputModes, outputModes

taiji 演化层（认知压缩 + MCTS）
─────────────────────────────
category (orch/exec/verify/converge)
dual (对偶 Skill id，硬约束)
implementation (机械可执行体)
agent_target (注册面隔离)
confidence, version, status
stats (四维 MCTS 统计)
env_tags, parent_id, variant_of
```

### 5. 删除的概念

| 删除 | 替代 |
|------|------|
| `yin/verifications/` + `VerificationAsset` | `yin/skills/verify/` + 统一 `SkillAsset` |
| `yang/workflows/` + `WorkflowAsset` | `yang/skills/orch/`（大 Skill = 工作流） |
| `ContractEngine` / `ContractReport` | `SkillEngine` / `SkillReport` |
| `CheckSpec` / `CheckResult` | `SkillSpec` / `SkillResult` |
| V43 阴元工具（6 个 `verify_*` 类） | 统一 Skill 对偶体系 |

---

## 二、BCP 更新清单

| 章节 | 变更内容 |
|------|---------|
| §1.2 | CausalAgent 描述更新为 SkillEngine + skills/verify/+converge/ |
| §1.7 | 心流表更新：skills/ 四类别沉淀/持续语义 |
| §4 | 类图：删除 YinToolResult + 6 工具类，新增 SkillResult/SkillEngine/SkillReport/SkillCategory |
| §5.3 | 路由表术语：ContractEngine → SkillEngine |
| §6.6 | 验证三权分立 → 阴阳对偶验证机制；L1 覆盖 verify + converge |
| §8.22 | ContractEngine → SkillEngine（加载 yin/skills/verify/ + yin/skills/converge/） |
| §10.0 | 资产表恢复 yang/yin 嵌套结构；新增对偶映射表（三相 × 四类）；A2A 标准字段 |
| §10.1 | 目录树：yang/ + yin/ 顶层，skills 嵌套其下 |
| §10.2 | SkillAsset 字段契约：A2A 兼容层 + taiji 演化层 |
| §10.3 | L1 契约 → L1 Skill |

---

## 三、当前实现缺口（对偶映射表）

| 周易相位 | 阳 Skill | 阴 Skill | 状态 |
|------|------|------|:---:|
| exec | write | file_exists + schema_valid | ✅ |
| exec | bash | command_succeeds | ✅ |
| exec | search | reference_resolves | ✅ |
| exec | webfetch | trace_consistency | ✅ |
| exec | read | reference_resolves（读而未用） | ❌ |
| orch | recursive_decompose | MECE | ❌ |
| orch | recursive_decompose | cross-consistency | ❌ |
| orch | recursive_decompose | granularity | ❌ |
| meta | MetaAgent LLM | mode-appropriateness | ❌ |
| meta | MetaAgent LLM | routing-effectiveness | ❌ |
| meta | MetaAgent LLM | asset-relevance | ❌ |

**P0 优先**：converge 类（3 个缺失）— orch 的 recursive_decompose 已有阳面实现，阴面完全空白。

---

## 四、代码层面待办

1. **归藏目录迁移**：当前磁盘结构仍是旧扁平布局（`prompts/` + `verifications/`），需执行 `migrate_to_yang_yin` → 已通过 `taiji init` 触发，幂等
2. **Skill 类型实现**：`SkillAsset` + `SkillCategory` + `dual` 字段尚未在 `src/types/agent.rs` 中实现（当前仍是 `VerificationAsset`）
3. **SkillEngine 重命名**：`contract_engine.rs` → `skill_engine.rs`，内部类型同步
4. **converge Skill 补齐**：3 个机械检查 Skill（mece-check / cross-consistency / granularity-check）
5. **AGENTS.md**：已精简至 71 行，8 条核心规则
6. **`--with-dmn` 回归测试**：连山 DMN 消费者需要适配新的 skills/ 路径

---

## 五、不变项

- AGENTS.md 8 条核心约束保持不变
- `cargo test --lib` 基线 = 277 passed
- BCP §1.3 为核心不变公理（概率系统不能验证概率系统）
- 异层同构原则（BCP §1.1）
- 上下文窗口预算（BCP §8.19）
- 分封制身份段（BCP §8.20）
