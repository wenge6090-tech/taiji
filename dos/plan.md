# V32 实现计划：DMN-MCTS 认知树（分区 + MCTS 四算子 + UCB 检索）

> 基于 BCP-蓝图-完型协议.md V32（DMN-MCTS 认知树：归藏按模型分区 + 蒙特卡洛学习）
> 生成日期：2026-08-11

## 目标

把归藏从静态知识库升级为按模型分区的蒙特卡洛认知树：UCB 检索（P0）→ 真实统计回传与 MCTS 四算子（P1）→ 元权重模型路由与主动学习（P2），全程零新增持久化文件、保持 DMN 纯符号层。

## 阻塞点（实现前必须决策）

1. **【时序矛盾】§8.8 检索先于路由，但分区检索依赖路由结果**——MetaAgent 必须先用 ModelRouter 决策 model（读根级 model_stats），才能按该模型分区检索资产。BCP §8.8 步骤顺序需修正为：模式决策 → **模型路由** → 按路由分区检索（UCB）→ 编排。
2. **【数据缺失】TraceRecord 无 token 字段**——usage 埋在 output JSON（Value）里，解析脆弱。方案：TraceRecord 加 `tokens_in: Option<u64>`（serde default），TraceHook 写 completion_response 时提取 `usage.input_tokens` 填入——比 evolver 事后解析 JSON 稳。
3. **【签名断裂】`CognitionEvolver::evolve(task_id, &[])` 拿不到任务数据**——DmnConsumer 现在传空 trace。方案：新增 `TraceRewardExtractor` 在 dmn_consumer 侧提取（读 task_dir 的 meta_ctx/trace/verify_state），evolve 接口改为接收提取结果。

另注：运行时只有 MetaAgent 走归藏检索 API（Fitting/Causal 经 MetaContext 注入获得资产），**分区只影响 meta.rs 检索 + DMN 写回**——分区改造面比预想小。

---

## P0 — 数据结构根（分区 + 资产树字段 + UCB 检索）

### 模块清单

| 文件 | 变更 |
|---|---|
| `src/types/agent.rs` | `PromptAsset` 加 `env_tags: Vec<String>` / `parent_id: Option<String>` / `variant_of: Option<String>` / `stats: AssetStats`（全部 serde default）；新增 `WorkflowAsset`、`VerificationAsset`（同构字段）；`MetaContext` 加 `model: Option<ModelKey>` + `assets_used: Vec<AssetRef>`（serde default）；新增 `ModelKey`（provider+model 字符串）、`AssetRef`、`AssetStats` |
| `src/infra/knowledge.rs` | `LiluoClient` 加 `model_key: Option<String>` 字段 + `for_model(root, key)` 构造 + 分区路径解析；`CognitiveAsset` enum 加 `Workflow/Verification` 变体；`type_dir_name` 加 `workflows`/`verifications`；`ensure_dirs` 建新目录；根级 `model_stats.yaml` 读写（`load_model_stats`/`save_model_stats`，原子写）；懒迁移函数 `migrate_to_partitioned(root)`（幂等：旧根资产 → `default/`） |
| `src/infra/ucb.rs`（新增） | 纯函数模块，无 IO，可单测 |
| `src/infra/config.rs` | `RuntimeConfig` 加 `dmn: DmnConfig`（serde default，全字段默认） |
| `src/agents/meta.rs` | 检索路径改为：路由决策 → 分区 client → search + UCB 排序 → 编排 → 注入 `assets_used`；`MetaContext` 构造处填充新字段 |

### 关键签名

```rust
// types/agent.rs
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetStats {
    pub n: u64,                  // 采样次数
    pub pass_count: u64,
    pub cost_tokens_sum: u64,
    pub cost_tokens_sq_sum: u64, // 增量方差
    pub quality_sum: f64,
    pub verify_rounds_sum: u64,
}
impl AssetStats {
    pub fn record(&mut self, signal: &RewardSignal);   // 增量更新
    pub fn avg_reward(&self, w: &RewardWeights) -> f64; // 0.5·pass + 0.3·quality − 0.2·cost − 0.1·rounds
    pub fn pass_rate(&self) -> f64;
}
pub struct ModelKey(pub String);   // "{provider}-{model}" slug
pub struct AssetRef { pub partition: ModelKey, pub id: String, pub kind: String }

// infra/ucb.rs（纯函数）
pub fn ucb_score(avg_reward: f64, n: u64, n_total: u64, c: f64) -> f64;
// n==0 → f64::INFINITY（最大探索分）；否则 avg_reward + c·√(ln n_total / n)
pub fn rank_by_ucb(assets: Vec<PromptAsset>, n_total: u64, c: f64, min_samples: u64) -> Vec<PromptAsset>;
// n < min_samples 的资产只按探索分参与排序（stats 全零 → 探索分排序）

// infra/config.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DmnConfig {
    pub reward_weights: RewardWeights,   // w_pass=0.5/w_quality=0.3/w_cost=0.2/w_rounds=0.1
    pub ucb_c: f64,                      // 1.414
    pub min_samples: u64,                // 3
    pub prune_n: u64,                    // 5
    pub backprop_gamma: f64,             // 0.5
    pub active_learning: Option<ActiveLearningConfig>, // P2
}
```

---

## P1 — 学习闭环（assets_used 断点修复 + MCTS 四算子）

### 模块清单

| 文件 | 变更 |
|---|---|
| `src/infra/trace.rs` | `TraceRecord` 加 `tokens_in: Option<u64>`（serde default） |
| `src/hooks/trace.rs` | completion_response 记录时提取 `usage.input_tokens` 填入 `tokens_in` |
| `src/orchestration/dmn_consumer.rs` | 处理 pending 文件时：读 `{data_root}/tasks/{task_id}/meta_ctx.json`（assets_used）+ `trace.jsonl`（tokens_in）+ `verify_state.json`（route/confidence）→ `TraceRewardExtractor::extract` → 调新 evolve 签名 |
| `src/orchestration/reward.rs`（新增） | 回报提取与质量分派生（route 映射 PASS=1.0/TPN=0.4/META=0.2 × confidence）+ 质量信号结构 |
| `src/orchestration/cognition_evolver.rs` | 占位算子替换为真实 MCTS 四算子 + `model_stats` 更新 |
| `src/main.rs` | `--with-dmn` 接线（DmnConsumer 构造传 data_root 与配置） |

### 关键签名

```rust
// orchestration/reward.rs
pub struct RewardSignal {
    pub pass: bool,
    pub quality: f64,        // route 映射 × confidence
    pub cost_tokens: u64,    // trace tokens_in 累加
    pub verify_rounds: u64,  // BACK_TO_TPN 次数（trace 路由记录）
}
pub struct AssetUsage { pub asset: AssetRef, pub signal: RewardSignal }

pub struct TraceRewardExtractor;
impl TraceRewardExtractor {
    pub async fn extract(task_dir: &Path) -> Result<Vec<AssetUsage>, TaijiError>;
    // 读 meta_ctx.json.assets_used（缺失 → 空 Vec，不报错：旧任务无断点数据）
    // 读 trace.jsonl 累加 tokens_in；读 verify_state.json 派生 quality
}

// orchestration/cognition_evolver.rs（替换占位实现）
pub async fn backprop(&self, usage: &[AssetUsage]) -> Result<u64, TaijiError>;
// 沿 assets_used 链回传：stats.record(signal)；parent_id 资产按 γ=0.5 衰减计入（需 load 父资产）
pub async fn fork(&self, asset_id: &str, reason: &str) -> Result<(), TaijiError>;
// 复制资产为变体：id 加 `__v{N}`，confidence×0.5，parent_id 指向原资产，stats 清零，version++
pub async fn merge(&self, variant_of: &str) -> Result<u64, TaijiError>;
// 组内相似变体：|Δavg| < 2σ 且内容相似 → 统计合并到父，删除变体文件
pub async fn prune(&self, min_n: u64, sigma: f64) -> Result<u64, TaijiError>;
// n ≥ prune_n 且 avg_reward 低于组内最优 >2σ → 删除（保留审计日志）
pub async fn evolve(&self, usages: &[AssetUsage]) -> Result<EvolutionReport, TaijiError>;
// 顺序: backprop → fork(触发条件: 使用资产回报低于阈值且任务 FAIL) → merge → prune → model_stats
```

---

## P2 — 元权重路由 + 主动学习（依赖 P0/P1 统计数据）

### 模块清单

| 文件 | 变更 |
|---|---|
| `src/orchestration/model_router.rs`（新增） | 从 model_stats 读候选统计，UCB 决策 ModelKey；候选 = 配置 providers × models；无统计 → 配置默认（探索分） |
| `src/agents/meta.rs` | 权重更新插入路由步骤（P0 已留位）：`ModelRouter::route(tags)` → `LiluoClient::for_model` 分区检索 |
| `src/agents/factory.rs` | `agent_llm_config` 支持 `meta_ctx.model` 覆盖（MetaContext 已有字段，此处消费）；`create_fitting_agent`/`create_causal_agent` 按路由模型解析 provider |
| `src/orchestration/dmn_consumer.rs` | pending 空 + 预算内：`ActiveLearner::candidates` → 模板化探索任务写 `experiments/` 队列（静态模板，不调 LLM） |
| `src/orchestration/active_learner.rs`（新增） | 候选选择（UCB 探索项最大者）+ 探索任务描述模板 + 窗口/预算检查 |

### 关键签名

```rust
// orchestration/model_router.rs
pub struct ModelRouter { stats: ModelStats, config: DmnConfig }
impl ModelRouter {
    pub fn route(&self, tags: &[&str], task_desc: &str) -> ModelKey;
    // 候选 = 配置 providers×models；score = avg_reward(model,tag) + c·√(ln N_total/N_model_tag)
    // 全部无统计 → 返回配置默认（探索）
}
pub async fn update_model_stats(liluo: &LiluoClient, usage: &[AssetUsage]) -> Result<(), TaijiError>;
// (model_key × tag) → StatsRow 增量更新，原子写 model_stats.yaml
```

---

## 依赖顺序

1. **P0 先做**：类型/字段/分区/UCB 纯函数/配置——所有后续依赖这些数据结构；且 P0 不改运行时行为（检索排序变化在无统计时退化=现状），可安全合入。
2. **P1 次之**：依赖 P0 的 AssetStats/AssetRef + assets_used 字段；`tokens_in` 必须在 evolver 之前（数据源先行）。
3. **P2 最后**：依赖 P1 积累的 model_stats 真实统计（无数据路由无意义）；主动学习依赖 DmnConfig。

## 验收标准

| 阶段 | 验收 |
|---|---|
| P0 | ① `cargo build` 无新增警告；② `cargo test --lib` ≥ 213 passed / 0 failed / 9 ignored（基线不降）；③ 新增单测：`ucb_score` 边界（n=0 → ∞、min_samples 门槛）、AssetStats 增量更新与 avg_reward 计算、旧 meta_ctx.json / 旧资产 YAML 反序列化兼容（无新字段也能读）、migrate 幂等、分区路径解析；④ 冒烟：`taiji init` 后 `.taiji/knowledge/default/` 含四目录 + workflows/verifications/ 存在 |
| P1 | ① 单测：TraceRewardExtractor 从 fixture（含 tokens_in/verify_state）提取正确信号；backprop 统计回传与 γ 衰减；fork/prune 阈值规则；② `--with-dmn` + 手工 pending 文件冒烟：日志显示真实 backprop（非 no-op），资产 YAML stats 字段更新，model_stats.yaml 生成；③ 回归：`cargo test --lib` 基线不降 |
| P2 | ① 单测：ModelRouter 无统计→默认、有统计→UCB 选优、成本惩罚生效；探索任务模板生成；② 冒烟：配置双 provider，MetaContext.model 非空，Fitting 按路由模型执行；③ 全量 `cargo test` 回归 |

## 实现顺序（串行提交）

`P0 类型层 → P0 检索层 → P0 测试 → P1 数据源 → P1 evolver → P1 接线 → P1 测试 → P2 路由 → P2 主动学习 → P2 测试`——每步 `cargo test` 自动跑，失败修复重试 ≤3 次。
