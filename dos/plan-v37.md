# V37 实现计划：多级路由（相位级异源裁判 + 子任务级模型覆盖）

> 依据：BCP V37（§8.8 路由分层 / §6.1 学习单元 / §8.21 元权重多级路由）。
> 基线：`cargo test --lib` = 266 pass / 0 failed / 9 ignored（V36 后）。
> 遵循 AGENTS.md：先 BCP 后实现（V37 已定稿）；新代码用 `GuizangClient` 命名；
> 新字段一律 `serde(default)` 零迁移；无降级原则（系统数据读失败上抛）。

## 目标（一句话）

实现 V37 承诺的**多级模型路由**：相位级异源裁判（Causal 用独立验证模型，裁判 ≠ 运动员）+ 子任务级模型覆盖（父 LLM 按子任务难度/领域分配模型），为「一个模型 + 它的约束系统 = 一个领域学习单元」的本地多模型形态铺路。

## 模块清单（精确到文件）

| # | 文件 | 变更 | 说明 |
|---|------|------|------|
| 1 | `src/types/agent.rs` | `MetaContext` 加 `verify_model: Option<ModelKey>` | V37 异源裁判载体（serde default，None = 继承 `model`） |
| 2 | `src/infra/config.rs` | `RuntimeConfig` 加 `model_routing: ModelRoutingConfig` | 新结构体 `{ heterogeneous_verifier: bool }`（默认 false，显式开关） |
| 3 | `src/orchestration/model_router.rs` | `ModelRouter` 加 `route_verifier(&ModelKey) -> Option<ModelKey>` | 异源决策：非主候选按 UCB 同公式选验证模型；候选 <2 → None |
| 4 | `src/agents/meta.rs` | `run()` 路由段接线 verify_model | 开关 on 且候选 ≥2 → `route_verifier`；写入 MetaContext |
| 5 | `src/agents/factory.rs` | `create_causal_verify_agent` / `create_causal_converge_agent` 消费 verify_model | `meta_ctx.verify_model.as_ref().or(meta_ctx.model.as_ref())`（异源优先） |
| 6 | `src/agents/causal.rs` | verify 契约加载分区改 verify_model 优先 | `meta_ctx.verify_model → for_model`；None → 现逻辑（model → 根） |
| 7 | `src/types/task.rs` | `SubtaskSpec` 加 `model: Option<ModelKey>` | 子任务级覆盖（serde default，None = 继承父） |
| 8 | `src/agents/tools/recursive_decompose.rs` | 构造 `child_meta_ctx` 处加 model 覆盖 | `subtask.model` 优先；`verify_model` 继承父（子验证跟随父异源） |
| 9 | `src/orchestration/tpn_cycle.rs` | BACK_TO_META 重路由兼容 | 子节点 BACK_TO_META 重跑 MetaAgent 时 model 重新路由（现有行为保留，verify_model 一并重路由） |
| 10 | 测试 | 新增单测（见验收） | 类型层 + 路由层 + 传递层 |

**P2 本地模型候选（`llm.model_roles`）— 本次不做，标注后置**：无本地模型运行环境，冒烟不可验证；ProviderRegistry 候选构建改动大且与 P0/P1 解耦。

## 接口签名

```rust
// 1. src/types/agent.rs
pub struct MetaContext {
    // ...既有字段...
    /// V37：验证相位（Causal）专用模型——异源裁判（§8.8 相位级）。
    /// Some = CausalAgent 用此模型（及对应分区加载契约）验证；None = 继承 model。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_model: Option<ModelKey>,
}

// 2. src/infra/config.rs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelRoutingConfig {
    /// 异源裁判开关（默认 false）。true 且路由候选 ≥2 时，Causal 用独立验证模型。
    #[serde(default)]
    pub heterogeneous_verifier: bool,
}
// RuntimeConfig 加：#[serde(default)] pub model_routing: ModelRoutingConfig

// 3. src/orchestration/model_router.rs
impl ModelRouter {
    /// V37 异源裁判决策（§8.8 相位级；MVP 边界：复用任务级 stats，相位维度后置）。
    /// 从非主模型候选中按 UCB 同公式（avg_reward + C·√(ln N_total/(n+1))）选验证模型；
    /// 候选数 <2 → None（无源可异，调用方 warn 降级）。
    pub fn route_verifier(&self, exec_key: &ModelKey) -> Option<ModelKey>;
}

// 4. src/agents/meta.rs（run() 路由段末尾）
let verify_model = if self.model_routing.heterogeneous_verifier {
    router.route_verifier(&model_key)
} else { None };
// ...MetaContext.verify_model = verify_model（空降级路径同样写入）

// 5. src/agents/factory.rs（两处 Causal 构造，同一行替换）
let model_key = meta_ctx.verify_model.as_ref().or(meta_ctx.model.as_ref());
let (provider, model) = self.agent_llm_config_with("causal", model_key);

// 6. src/agents/causal.rs（契约加载分区）
// verify 契约分区：meta_ctx.verify_model → for_model；None → meta_ctx.model → 根（现逻辑）

// 7. src/types/task.rs
pub struct SubtaskSpec {
    // ...既有字段...
    /// V37：子任务模型覆盖（§8.8 子任务级）。None = 继承父模型（子 TpnCycle
    /// 注入父 MetaContext，model 默认继承）。serde default 零迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelKey>,
}

// 8. src/agents/tools/recursive_decompose.rs（~L369 构造 child_meta_ctx 处）
let mut child_meta_ctx = parent_meta_ctx;
// ...既有 mode/parent_deliverables/sibling_deliverables 赋值...
// V37 子任务级路由：SubtaskSpec.model 覆盖子模型；verify_model 继承父（异源随父）
if let Some(m) = &subtask.model {
    child_meta_ctx.model = Some(m.clone());
}
```

## 依赖顺序

1. **P0a 类型层**（#1 + #2 + #7）：三个 serde default 字段——纯类型变更，零行为，先落地基。`cargo test` 确认无回归。
2. **P0b 路由层**（#3）：`route_verifier` 纯函数 + 单测——不依赖接线，可独立验证决策正确性（候选 1 个 → None / 候选 2 个 → 异源 / 无统计 → 冷启动探索 / 异源成本过高 → 主模型）。
3. **P0c 接线层**（#4 + #5 + #6）：meta.rs 决策 → MetaContext 落盘 → factory 消费 → causal 契约分区——数据流贯通，冒烟验证。
4. **P1 子任务级**（#7 已在 P0a 加字段 + #8 + #9）：child_meta_ctx 覆盖 + BACK_TO_META 兼容检查——依赖 P0c 的分区跟随语义（子 model 变化后 Causal 契约分区自动跟随）。
5. **收尾**：全量测试 + 冒烟 + 恢复 config。

原因：类型层先行避免接线时改签名；路由纯函数先行可单测锁定决策语义（异源选择是核心价值，不能靠冒烟碰运气）；接线最后做（三个消费点一次贯通）。

## 验收标准（可测量）

**测试命令**：
- `cargo build` — 0 编译错误（vendor cfg 警告忽略）
- `cargo test --lib` — ≥ 266 + 新增全部通过
- 新增单测：
  - `model_router.rs`：`route_verifier` 候选=1 → None；候选=2 → 返回非主模型；全无统计 → 探索选非主；tie → 声明顺序（≥3 个）
  - `config.rs`：`ModelRoutingConfig` serde default（缺字段 → false）
  - `agent.rs`：`MetaContext.verify_model` 序列化 round-trip + 缺字段反序列化 → None
  - `task.rs`：`SubtaskSpec.model` round-trip + 缺字段 → None
  - `recursive_decompose.rs`：child_meta_ctx 构造处 model 覆盖（subtask.model 优先 / None 继承父）
  - `factory.rs`：`agent_llm_config_with("causal", verify_model优先)` 解析

**冒烟步骤**（V37 冒烟前备份 config.json）：
1. `.taiji/config.json` 加第二个 deepseek 系 provider（如 `{name:"deepseek-r", model:"deepseek-reasoner", api_key:同key}`）+ `runtime.model_routing.heterogeneous_verifier: true`
2. `taiji run "写一个 hello.txt 到 deliverables"` → 检查任务 `meta_ctx.json`：`verify_model` 存在且 ≠ `model`
3. 日志确认：`Creating CausalVerifyAgent` 的 model = 异源模型（RUST_LOG=debug）
4. 恢复 config.json（删 provider + 开关）

**回归验收**：`heterogeneous_verifier` 默认 false 时 `verify_model` 不落盘（skip_serializing_if），行为与 V36 完全一致——`taiji run` 简单任务 `meta_ctx.json` 无 verify_model 键。

## 阻塞点与替代方案（实现前声明）

1. **【真实阻塞】异源裁判依赖 ≥2 候选**：当前默认配置只有 deepseek-chat 单候选 → `route_verifier` 恒 None，冒烟无法验证异源路径。**替代**：冒烟用临时双 provider 配置（见上）；代码层候选 <2 时 warn 降级 None（行为明确，不 panic）。候选 = `ProviderRegistry.model_candidates`（default + deepseek 系 providers 条目）——同 provider 多 model（chat/reasoner）即可构成异源，无需第二个 API key 供应商。
2. **【设计边界】相位统计维度后置**：`route_verifier` 复用任务级 stats（(model_key × tag)），不新增 (model_key × tag × phase) 维度——BCP §8.8 明示 MVP 边界。异源模型的验证质量暂不单独回传 model_stats（任务级回传挂主模型），**文档标注即可，不阻塞**。
3. **【范围裁剪】P2 本地候选（model_roles）后置**：无本地模型环境，冒烟不可验证；且 ProviderRegistry 候选构建改动（候选判定从 name/base_url 启发式 → 显式 role 映射）与 P0/P1 完全解耦。V37 附录已写明演进方向，本次不实现。
4. **【兼容确认】BACK_TO_META 重路由**：子节点 BACK_TO_META 重跑 MetaAgent 时 model/verify_model 重新路由（脱离父覆盖）——语义合理（认知校准 = 重新决策），保留现状，不改。
