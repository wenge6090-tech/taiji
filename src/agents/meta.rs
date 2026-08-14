//! MetaAgent builder (权重更新·元) — "weight update, the meta phase".
//!
//! The MetaAgent is the **first** agent in the Zhouyi cycle.  It queries the
//! 归藏 (cognitive warehouse) via Rig's `dynamic_context` mechanism to extract
//! matched prompt assets that serve as cognitive bias for downstream agents.
//!
//! # Constraints (AGENTS.md §2, §4)
//! - `max_turns = 6` — the MetaAgent is a multi-turn extractor: it can invoke
//!   read-only collection tools (`read` / `search` / `webfetch`) to gather task
//!   context, parent deliverables and web facts before composing weights.
//! - System prompt starts with Chinese identifier "你是权重更新专家".
//! - Output is parsed into [`MetaContext`] which is injected into the
//!   YangAgent (概率拟合·阳).
//!
//! # Lifecycle
//! 1. [`AgentFactory::create_meta_agent`] resolves LLM config and creates
//!    this builder.
//! 2. [`MetaAgentBuilder::run`] constructs a transient Rig agent, executes it,
//!    and returns a [`MetaContext`].
//! 3. The caller feeds the [`MetaContext`] into `create_yang_agent`.

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;

use crate::agents::tools::skills::SkillRegistry;
use crate::hooks::safety::SafetyHook;
use crate::infra::config::SafetyConfig;
use crate::infra::error::TaijiError;
use crate::infra::json_util::parse_llm_json;
use crate::infra::knowledge::GuizangClient;
use crate::infra::provider::ProviderRegistry;
use crate::types::agent::{AgentMode, MetaContext, MetaOutcome, PromptAsset, YangPrompt};
use crate::types::ontology::{OntologyEdge, OntologyRule, TaskOntologyView};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// V32：MetaAgent LLM 编排的**输出契约**——只含 LLM 能决定的字段。
/// 内部类型（constraints / matched_skills / yang_prompt 嵌套结构）由系统
/// 组装，不要求 LLM 输出（实测：LLM 把 matched_skills 输出为字符串数组
/// 导致 parse 必败，3 次重试全失败 → 编排静默失效）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetaComposeResult {
    /// 阴阳配对模式（LLM 按深度规则 + 难度决策）。
    /// V46：answer 短路场景可不给 mode，serde default = Orchestration。
    #[serde(default)]
    pub mode: AgentMode,
    #[serde(default)]
    pub yang_system_prompt: Option<String>,
    #[serde(default)]
    pub verify_system_prompt: Option<String>,
    #[serde(default)]
    pub converge_system_prompt: Option<String>,
    /// 可选温度覆盖（None = 使用 Base 模板默认温度）。
    #[serde(default)]
    pub temperature: Option<f32>,
    /// 任务描述复述（LLM 可能改写/精炼；空则回退原始描述）。
    #[serde(default)]
    pub task_description: Option<String>,
    /// 约束摘要（文本列表，供 prompt 注入——不是 TruthConstraint 结构）。
    #[serde(default)]
    pub constraint_summaries: Vec<String>,
    /// V46 短路：应答类任务（产出不改变世界）直接回答；非空即短路（跳过阳阴）。
    #[serde(default)]
    pub answer: Option<String>,
    /// V50 §6.6 实体链接输出：任务语义视图（domain/action/objects/env）。
    /// None = 未识别（回退纯 UCB，状态分支非错误）。
    #[serde(default)]
    pub ontology: Option<TaskOntologyView>,
}

/// V50 §6.6 类型级软查询（纯符号，零 LLM）：任务 objects 命中 relations 的
/// type→type 边 → 注入对侧类型的资产（硬依赖候选，进候选池仍走 UCB）。
/// MVP-1：资产只含 prompt（`asset_type_map` 现只映射 prompts）。
fn ontology_expand(
    view: &TaskOntologyView,
    relations: &[OntologyEdge],
    asset_types: &HashMap<String, String>,
) -> Vec<crate::types::agent::AssetRef> {
    use crate::types::agent::AssetRef;
    let mut refs = Vec::new();
    for obj in &view.objects {
        for e in relations {
            let target = if e.from == *obj {
                Some(&e.to)
            } else if e.to == *obj {
                Some(&e.from)
            } else {
                None
            };
            if let Some(t) = target {
                for (aid, tid) in asset_types {
                    if tid == t {
                        refs.push(AssetRef::new("prompt", aid));
                    }
                }
            }
        }
    }
    refs
}

/// V50 §6.6 约束校验（纯符号）：返回匹配任务语义视图的规则（阴 checklist 硬约束）。
fn ontology_validate(view: &TaskOntologyView, rules: &[OntologyRule]) -> Vec<OntologyRule> {
    rules.iter().filter(|r| rule_matches(r, view)).cloned().collect()
}

fn rule_matches(r: &OntologyRule, view: &TaskOntologyView) -> bool {
    let dom = r
        .when
        .domain
        .as_deref()
        .map(|d| d == view.domain)
        .unwrap_or(true);
    let env = r
        .when
        .env
        .as_deref()
        .map(|e| view.env.as_deref() == Some(e))
        .unwrap_or(true);
    let act = r
        .when
        .action
        .as_deref()
        .map(|a| a == view.action)
        .unwrap_or(true);
    dom && env && act
}

/// V50 §6.6 本体消费（零 LLM）：expand 硬依赖注入 + validate 规则注入。
/// 失败上抛（调用方 warn；ontology 缺失 = 状态分支回退纯 UCB，§6.6 无降级）。
async fn apply_ontology(
    guizang: &GuizangClient,
    view: &TaskOntologyView,
    ctx: &mut MetaContext,
) -> Result<(), TaijiError> {
    let relations = guizang.load_relations().await?;
    let rules = guizang.load_rules().await?;
    let asset_types = guizang.asset_type_map().await?;

    // expand：硬依赖候选注入 assets_used（去重；进候选池仍 UCB 排）
    for r in ontology_expand(view, &relations, &asset_types) {
        if !ctx.assets_used.iter().any(|a| a.id == r.id) {
            ctx.assets_used.push(r);
        }
    }

    // validate：匹配规则 → constraint_summaries（阴 checklist 硬约束）
    for rule in ontology_validate(view, &rules) {
        let mut summary = format!("[ontology] {}", rule.id);
        if !rule.require.is_empty() {
            summary.push_str(&format!(" 必须含类型: {}", rule.require.join(",")));
        }
        if !rule.forbid.is_empty() {
            summary.push_str(&format!(" 禁止类型: {}", rule.forbid.join(",")));
        }
        ctx.yang_prompt.constraint_summaries.push(summary);
    }
    Ok(())
}

/// 纯符号任务类型标签提取（批18 P2 修复：替代 zhouyi 硬编码 `["general"]`）。
/// 零 LLM——关键词匹配任务描述，识别代码类任务以激活 code-safety truth
/// （constraint_engine::load_truths）。无法归类时回退 `["general"]`（宁简勿误）。
/// 测试：`classify_task_tags_detects_code`。
pub fn classify_task_tags(description: &str) -> Vec<String> {
    let lower = description.to_lowercase();
    const CODE_KEYWORDS: &[&str] = &[
        "code", "coding", "compile", "compiler", "cargo", "rust", "refactor",
        "debug", "bug", "function", "trait", "struct", "enum", "module",
        "代码", "编译", "重构", "调试", "函数", "类型", "模块", "接口", "缺陷",
    ];
    if CODE_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return vec!["code".to_string()];
    }
    vec!["general".to_string()]
}

/// Builder for the MetaAgent (权重更新·元).
///
/// Encapsulates all configuration needed to construct and execute a Rig agent
/// that extracts reasoning paths from the 归藏.  Created by
/// [`AgentFactory::create_meta_agent`](super::factory::AgentFactory::create_meta_agent).
///
/// The Rig agent registers read-only collection tools (`read` / `search` /
/// `webfetch`) so the LLM can gather task context, parent deliverables and web
/// facts before composing the weight update; the [`SafetyHook`] is **always**
/// mounted (defaults to `SafetyConfig::default()` when no shared singleton is
/// injected) — "带工具必有安全钩子" is a type-level guarantee.
pub struct MetaAgentBuilder {
    task_id: String,
    guizang: Arc<GuizangClient>,
    provider: Arc<ProviderRegistry>,
    /// Resolved provider name (config `agent_overrides["meta"]` → default),
    /// used to select the LLM client — 批12 P1 修复（不再硬编码 "deepseek"）。
    provider_name: String,
    model: String,
    /// Recursion depth of the current node (root = 0) — injected into the
    /// mode-decision prompt (递归层数规则, V27).
    depth: u32,
    /// Max recursion depth from RuntimeConfig — depth rule floor for the
    /// mode decision (leaf nodes must be Execution).
    max_depth: u32,
    /// max_turns = 6 — allows tool loops (collect → extract) before the final
    /// structured MetaContext emission.
    max_turns: u32,
    /// Process-wide SafetyHook (or a default-configured instance) — always
    /// mounted on the Rig agent.
    safety_hook: Arc<SafetyHook>,
    /// V37：异源裁判开关（BCP §8.8 相位级）——true 且路由候选 ≥2 时决策
    /// `MetaContext.verify_model`（Yin 专用验证模型）。
    heterogeneous_verifier: bool,
}

impl MetaAgentBuilder {
    /// Create a new `MetaAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_meta_agent`] — external
    /// callers should use the factory rather than constructing this directly.
    pub fn new(
        task_id: &str,
        guizang: Arc<GuizangClient>,
        provider: Arc<ProviderRegistry>,
        model: &str,
    ) -> Self {
        Self {
            task_id: task_id.to_string(),
            guizang,
            provider,
            provider_name: "deepseek".to_string(),
            model: model.to_string(),
            depth: 0,
            max_depth: 2, // RuntimeConfig::default().max_depth
            max_turns: 6, // tool-loop headroom: collect → extract
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            heterogeneous_verifier: false,
        }
    }

    /// V37：异源裁判开关（BCP §8.8 相位级）——true 且路由候选 ≥2 时，
    /// Yin 验证相位使用与执行相位不同的模型（裁判 ≠ 运动员）。
    /// 默认 false（行为与 V36 一致）。由工厂从
    /// `runtime.model_routing.heterogeneous_verifier` 传入。
    pub fn heterogeneous_verifier(mut self, enabled: bool) -> Self {
        self.heterogeneous_verifier = enabled;
        self
    }

    /// Inject the current recursion depth (root = 0) — part of the
    /// 递归层数规则 input for the mode decision (V27).
    pub fn depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Inject the max recursion depth — the depth-rule floor for the mode
    /// decision: `depth + 1 >= max_depth` forces Execution (leaf nodes
    /// cannot decompose).
    pub fn max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Override the SafetyHook with the shared process-wide singleton.
    pub fn safety_hook(mut self, hook: Arc<SafetyHook>) -> Self {
        self.safety_hook = hook;
        self
    }

    /// Override the LLM turn budget (default 6).
    pub fn max_turns(mut self, n: u32) -> Self {
        self.max_turns = n;
        self
    }

    /// Override the resolved provider name (批12 P1 修复：由工厂从
    /// `agent_llm_config("meta")` 传入，替代硬编码 "deepseek")。
    pub fn provider_name(mut self, provider: &str) -> Self {
        self.provider_name = provider.to_string();
        self
    }

    /// Run the MetaAgent: query the 归藏, LLM-compose prompts, produce [`MetaContext`].
    ///
    /// # Flow
    /// 1. Query 归藏 via `search_prompts(task_type_tags)` for matching prompt assets.
    /// 2. Filter by confidence threshold (`CONFIDENCE_THRESHOLD = 0.3`).
    /// 3. When matching assets exist → call LLM to compose `MetaContext`. The
    ///    LLM may use read-only collection tools (`read` / `search` / `webfetch`)
    ///    to gather task context / parent deliverables / web facts first.
    /// 4. When no assets or LLM fails → fallback to `MetaContext::empty()`.
    ///
    /// # Parameters
    /// - `task_description` — the task the downstream agents will execute.
    /// - `task_type_tags` — tags for 归藏 prompt search.  Empty tags produce no
    ///   matches, triggering the fallback path.
    /// - `handoff` — V28 前一瞬态产出（交接文件内容，§8.18）：BACK_TO_META 重跑时
    ///   注入作产出校准（基于失败产物调整权重与资产）；首次运行传 None。
    pub async fn run(
        &self,
        task_description: &str,
        task_type_tags: &[&str],
        handoff: Option<&str>,
    ) -> Result<MetaOutcome, TaijiError> {
        // ── 0. 模型路由（V36，BCP §8.8 第 1 步——纯符号层）──
        // 读根级 model_stats 元权重表 → UCB 决策 model_key（全部无统计 → 默认）；
        // 模型键只影响路由与统计回传（V44 去分区化，§10.1——资产树共享）。
        // model_stats 损坏 → 空表（load_model_stats 内 warn），
        // 路由退化为默认模型。
        let model_key = {
            let stats = self.guizang.load_model_stats().await?;
            crate::orchestration::model_router::ModelRouter::new(&self.provider, stats).route()
        };
        tracing::debug!(
            task_id = %self.task_id,
            model_key = %model_key,
            "MetaAgent: model routed"
        );
        // V37 异源裁判（BCP §8.8 相位级）：开关开启时从非主候选按 UCB 同公式
        // 选 Yin 专用验证模型（裁判 ≠ 运动员，§1.3 偏置对抗）；候选 <2 →
        // None（继承主模型，warn 提示）。
        let verify_model = if self.heterogeneous_verifier {
            let stats = self.guizang.load_model_stats().await?;
            let router =
                crate::orchestration::model_router::ModelRouter::new(&self.provider, stats);
            match router.route_verifier(&model_key) {
                Some(v) => {
                    tracing::info!(
                        task_id = %self.task_id,
                        exec_model = %model_key,
                        verify_model = %v,
                        "MetaAgent: heterogeneous verifier routed (异源裁判)"
                    );
                    Some(v)
                }
                None => {
                    tracing::warn!(
                        task_id = %self.task_id,
                        exec_model = %model_key,
                        "heterogeneous_verifier enabled but <2 routing candidates — verifier inherits exec model"
                    );
                    None
                }
            }
        } else {
            None
        };

        // ── 1. Query 理絡 for prompt assets（V44：根级资产树共享）──
        let prompt_assets = self.guizang.search_prompts(task_type_tags).await?;

        // ── 2. Confidence filter ──
        const CONFIDENCE_THRESHOLD: f64 = 0.3;
        let matched: Vec<PromptAsset> = prompt_assets
            .iter()
            .filter(|p| p.confidence >= CONFIDENCE_THRESHOLD)
            .cloned()
            .collect();

        if matched.is_empty() {
            tracing::debug!(
                task_id = %self.task_id,
                model_key = %model_key,
                "No high-confidence prompt assets — returning empty MetaContext (fallback)"
            );
            let mut empty = MetaContext::empty();
            // V44：资产缺失 ≠ 路由失败——模型选择保持（Yang/Yin 按路由模型执行）。
            empty.model = Some(model_key.clone());
            // V37：降级路径同样保持异源裁判决策（模型选择与资产编排解耦）。
            empty.verify_model = verify_model.clone();
            return Ok(MetaOutcome::Context(empty));
        }

        // ── 2.5 UCB 排序（V35/MVP-5 检索数学化，§6.3 实现层定稿）──
        // 后验均值 μ + 探索项 C·√(ln N_total/(n+1))；n=0 冷启动退化为先验 μ 降序。
        // prior_strength 取 LianshanConfig 默认（MetaAgentBuilder 无 config——与 §6.4.1 默认一致）。
        let models = self.guizang.load_all_models().await?;
        let ranked = crate::infra::knowledge::rank_prompts_by_ucb(
            &matched,
            &models,
            crate::orchestration::active_learning::UCB_C,
            10.0,
            // V50 环境维度轴（§6.3.1）：current_env_tags 源 = 路由模型类
            //（flash/strong）——同维度变体优先，异维度变体 ×0.5 降权。
            &[crate::agents::factory::model_class(&model_key).to_string()],
        );
        let matched: Vec<PromptAsset> = ranked.into_iter().map(|i| matched[i].clone()).collect();
        let matched_refs: Vec<&PromptAsset> = matched.iter().collect();
        // 编排所选资产引用（UCB 序全部候选——MetaAgent 编排基于此列表，任务级归因代理）
        let assets_used: Vec<crate::types::agent::AssetRef> = matched
            .iter()
            .map(|p| crate::types::agent::AssetRef::new("prompt", &p.id))
            .collect();

        // ── 3. LLM call to compose MetaContext (mode-paired, V27) ──
        tracing::debug!(
            task_id = %self.task_id,
            depth = self.depth,
            max_depth = self.max_depth,
            matched_count = matched.len(),
            "Calling LLM to compose MetaContext from 归藏 prompt assets"
        );

        let llm_prompt = build_llm_input(
            task_description,
            self.depth,
            self.max_depth,
            &matched_refs,
        );
        // V28 产出校准：注入前一瞬态产出（handoff.md 全文）——元基于失败产物
        // 校准权重与认知资产，不再空手重跑（BCP §8.18 BACK_TO_META 语义）。
        let llm_prompt = match handoff {
            Some(h) if !h.trim().is_empty() => format!(
                "{llm_prompt}\n\n## 前一瞬态产出（V28 产出校准）\n{h}"
            ),
            _ => llm_prompt,
        };

        let client = self.provider.client_for(&self.provider_name).map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("MetaAgent: failed to get provider client: {e}"),
            }
        })?;

        // ── 收集工具（只读）：read / search / webfetch — 供 LLM 收集任务上下文、
        //    父层 deliverables 与网络信息后更新权重（V25 权限分工：收集工具三相共有）。
        //    带工具必有安全钩子（§8.5 硬约束，类型级保证）：无条件挂载 SafetyHook ──
        let skill_tools: Vec<Box<dyn rig::tool::ToolDyn>> = SkillRegistry::new(std::path::Path::new("."))
            .tools()
            .iter()
            .filter(|t| matches!(t.name(), "read" | "search" | "webfetch"))
            .map(|t| Box::new(t.clone()) as Box<dyn rig::tool::ToolDyn>)
            .collect();
        let agent = client
            .agent(&self.model)
            .preamble(META_COMPOSE_SYSTEM_PROMPT)
            .default_max_turns(self.max_turns as usize)
            .hook(self.safety_hook.as_ref().clone())
            .tools(skill_tools)
            .build();

        // ── 4. LLM compose with retry (V32 第一性原理) ──
        // LLM 是概率性的：一次调用失败 / 一次 parse 失败不代表永久失败。
        // 重试 3 次（AGENTS.md §6 规则），每次重试注入格式纠正提示；
        // 仍失败则返回带 degraded 标记的 empty——「编排失败」必须可见，
        // 不得静默降级（V32 实测：偶发 parse 失败曾导致任务裸奔）。
        const COMPOSE_ATTEMPTS: u32 = 3;
        let mut last_error: Option<String> = None;
        let mut composed: Option<MetaComposeResult> = None;
        for attempt in 0..COMPOSE_ATTEMPTS {
            let prompt = if attempt == 0 {
                llm_prompt.clone()
            } else {
                format!(
                    "{llm_prompt}\n\n## 上次输出解析失败（第 {attempt} 次重试）\n\
                     必须只输出一个完整的 JSON 对象，不能包含任何解释文本或代码围栏。\
                     必须包含字段：mode (\"Orchestration\" | \"Execution\"), \
                     yang_system_prompt, verify_system_prompt, converge_system_prompt, \
                     constraint_summaries, task_description"
                )
            };
            match agent.prompt(&prompt).await {
                Ok(response) => match parse_llm_json::<MetaComposeResult>(response.as_ref()) {
                    Ok(result) => {
                        composed = Some(result);
                        break;
                    }
                    Err(e) => {
                        last_error =
                            Some(format!("attempt {attempt}: parse failed: {e}"));
                        tracing::warn!(
                            task_id = %self.task_id,
                            attempt,
                            error = %e,
                            "MetaAgent: LLM response failed to parse as MetaComposeResult, retrying"
                        );
                    }
                },
                Err(e) => {
                    last_error = Some(format!("attempt {attempt}: llm call failed: {e}"));
                    tracing::warn!(
                        task_id = %self.task_id,
                        attempt,
                        error = %e,
                        "MetaAgent: LLM call failed, retrying"
                    );
                }
            }
        }

        match composed {
            Some(result) => {
                // V46 短路（BCP §8.8）：answer 非空 → 应答类任务直接产出，跳过阳阴。
                if let Some(answer) = result.answer.filter(|a| !a.trim().is_empty()) {
                    tracing::info!(
                        task_id = %self.task_id,
                        "MetaAgent: answer short-circuit (应答类任务，跳过阳阴)"
                    );
                    return Ok(MetaOutcome::Answer(answer));
                }

                // V32：LLM 只输出它能决定的字段（mode + 三份提示词 + 摘要），
                // 内部类型（constraints / matched_skills / yang_prompt 结构）由
                // 系统组装——不再要求 LLM 输出 SkillRef/TruthConstraint 结构
                // （实测：LLM 把 matched_skills 输出为字符串数组导致 parse 必败）。
                let mut ctx = MetaContext {
                    constraints: vec![],
                    matched_skills: vec![],
                    yang_prompt: YangPrompt {
                        task_description: result
                            .task_description
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| task_description.to_string()),
                        constraint_summaries: result.constraint_summaries,
                        parent_deliverables: vec![],
                        sibling_deliverables: vec![],
                    },
                    mode: result.mode,
                    degraded: None,
                    assets_used,
                    // V36：模型路由结果（BCP §8.8 第 7 步——降级路径也保持路由
                    // 结果：模型选择与资产编排解耦，Yang/Yin 仍按路由模型
                    // 执行；仅路由异常时 None = 配置默认）。
                    model: Some(model_key.clone()),
                    // V37：异源裁判（BCP §8.8 相位级）——Yin 专用验证模型；
                    // 开关关闭 / 候选不足时 None = 继承主模型。
                    verify_model,
                    yang_system_prompt: result.yang_system_prompt,
                    verify_system_prompt: result.verify_system_prompt,
                    converge_system_prompt: result.converge_system_prompt,
                    // V51：温度覆盖（批12 P1 死字段接线）——LLM 输出的 temperature
                    // 覆盖四象默认；None = 下游用四象默认。
                    temperature: result.temperature.map(|v| v as f64),
                    // 批18 P2：任务类型标签透传（zhouyi 提取 → 阴 load_truths）。
                    task_type_tags: task_type_tags.iter().map(|s| s.to_string()).collect(),
                };

                // V50 §6.6 本体消费：实体链接 → 类型级软查询（expand）+ 约束校验
                // （validate）。失败仅 warn（增强层）；ontology None/空 domain =
                // 状态分支回退纯 UCB（§6.6 无降级）。
                if let Some(view) = result.ontology.filter(|v| !v.domain.is_empty()) {
                    if let Err(e) = apply_ontology(self.guizang.as_ref(), &view, &mut ctx).await {
                        tracing::warn!(
                            task_id = %self.task_id,
                            error = %e,
                            "[meta] ontology apply failed — continuing"
                        );
                    }
                }

                // guard_pairing（§5.3 逻辑层公理）：mode 与 prompt 配对校验——
                // 不配对 → degraded 标记（不中断，下游有 Base 模板降级兑底）。
                if let Some(reason) = guard_pairing(ctx.mode, &ctx) {
                    ctx.degraded = Some(format!("guard_pairing: {reason}"));
                }
                // V30 会盟字段（parent_deliverables / sibling_deliverables）由
                // 分封时（RecursiveDecomposeTool）注入，此处保持空。
                tracing::info!(
                    task_id = %self.task_id,
                    mode = ?ctx.mode,
                    has_yang = ctx.yang_system_prompt.is_some(),
                    has_verify = ctx.verify_system_prompt.is_some(),
                    has_converge = ctx.converge_system_prompt.is_some(),
                    "MetaAgent: composed MetaContext"
                );
                Ok(MetaOutcome::Context(ctx))
            }
            None => {
                let reason = last_error
                    .unwrap_or_else(|| "unknown error".into());
                tracing::error!(
                    task_id = %self.task_id,
                    "MetaAgent: compose failed after {COMPOSE_ATTEMPTS} attempts: {reason}"
                );
                let mut empty = MetaContext::empty();
                empty.degraded = Some(format!(
                    "MetaAgent compose failed after {COMPOSE_ATTEMPTS} attempts: {reason}"
                ));
                // V36：降级路径保持模型路由结果（§8.8 第 8 步——模型选择与
                // 资产编排解耦；None 仅当路由异常/未执行）。
                empty.model = Some(model_key.clone());
                // V37：降级路径保持异源裁判决策。
                empty.verify_model = verify_model.clone();
                Ok(MetaOutcome::Context(empty))
            }
        }
    }
}

/// System prompt for the MetaAgent's LLM composition call.
///
/// Instructs the LLM to decide the node's 阴阳配对模式 (Orchestration |
/// Execution) from recursion depth rules + task difficulty, then compose
/// mode-paired system prompts for downstream agents from 归藏 prompt assets
/// (V27).  The Chinese prefix anchors the agent's role per project
/// convention (see AGENTS.md §2).
const META_COMPOSE_SYSTEM_PROMPT: &str = r#"你是权重更新专家 (Weight Update · Meta Agent)。

你的职责是根据任务描述、递归层数规则与认知仓库（归藏）中的提示词资产，
先决策当前节点的**阴阳配对模式**，再编排下游 Agent（YangAgent 概率拟合·阳
和 YinAgent 因果验证·阴）与该模式**配对**的系统提示词。

## 输入
- task_description：当前任务的完整描述
- depth / max_depth：当前递归层数与最大递归深度
- prompt_assets：归藏中匹配的提示词资产列表（按置信度降序排列）
  每项包含：id, name, content, agent_target, tags, confidence

## 模式决策（依据两条规则）
1. **递归层数规则**：若 depth+1 >= max_depth（当前节点是叶子，无法再拆解），
   必须选 "Execution"；深度越浅，越有空间拆解，复杂任务倾向 "Orchestration"。
2. **任务难易程度**：分析任务描述——复杂/多步骤/跨多个独立维度/需要多 Agent
   协作 → "Orchestration"（编排拆解 + 综合）；原子/单步/可直接用 L1 工具
   完成 → "Execution"（直接执行）。
3. **上下文超限强制编排（V47）**：若前一瞬态产出（handoff）表明上一轮因
   context_overflow（上下文超限）失败，且 depth+1 < max_depth（当前节点还能拆），
   必须选 "Orchestration"——上下文超限 = 任务粒度错误 = 上一轮把该拆的任务
   判成了执行，须编排拆解为多个小上下文子任务。仅当 depth+1 >= max_depth
   （叶节点无法再拆）时才允许维持 Execution。

## 阴阳配对
- "Orchestration"：阳 Agent 用**编排**提示词（recursive_decompose 拆解 + 综合），
  阴 Agent 用**收敛**提示词（converge 判决子结果聚合）。
- "Execution"：阳 Agent 用**执行**提示词（L1 工具直接产出，无 recursive_decompose），
  阴 Agent 用**验证**提示词（verify 判决直接产出）。

## 你需要做的
1. 按上述两条规则决策 "mode"
2. 从 prompt_assets 中选择**与所选模式匹配**的资产（按 name/tags/description
   判断：orchestration_* 资产对应编排模式，execution_* 资产对应执行模式）
3. 将其 content 字段组合为与该模式配对的三份完整系统提示词（配对外的
   提示词可设为 null，下游不会使用）

## 短路判断（先于模式决策）
先判断任务是否**应答类**（产出不改变世界状态）：纯信息查询、知识问答、
分析、讨论、解释（如"什么是 X""库里有没有 Y""分析这段代码""解释 Z 的原理"）。
- 若任务为应答类：直接给出完整回答，填入 answer 字段；其余字段（mode、
  system_prompt 等）可全部省略（系统会短路跳过执行与验证相）。
- 若任务需要产生文件/执行命令/改变世界（写代码、跑脚本、生成文档到文件）：
  answer 置 null，按上述模式决策走完整流程。

## 实体链接（V50 §6.6，先于模式决策）
将任务映射到语义本体结构（供系统按**类型**查依赖边与约束规则，非路由标签）：
- domain：领域分类（Security | Infra | Data | Finance | General）
- action：动作类型（Create | Read | Update | Delete | Debug | Fix）
- objects：涉及实体/资产类型（尽量对齐本体词汇表，如 security-check /
  deploy-action / data-mutation；无法对齐则输出自然实体名）
- env：运行环境（Production | Staging | Dev；无则 null）
- is_critical：是否安全/关键敏感任务（true/false）

填入 ontology 字段；若无法识别领域，ontology 置 null（系统回退纯 UCB，不报错）。

## 输出格式（严格 JSON，无额外注释）

{
  "mode": "Orchestration",
  "yang_system_prompt": "完整的 YangAgent 系统提示词，与所选模式配对（编排或执行）",
  "verify_system_prompt": "Execution 模式：完整的 verify 系统提示词，以'你是因果验证器'开头；Orchestration 模式可设为 null",
  "converge_system_prompt": "Orchestration 模式：完整的 converge 系统提示词，以'你是收敛判决器'开头；Execution 模式可设为 null",
  "constraint_summaries": [],
  "task_description": "（保持原始 task_description，可原样复制）",
  "answer": null,
  "ontology": {"domain": "Security", "action": "Fix", "objects": ["security-check"], "env": null, "is_critical": false}
}

## 降级规则
当 prompt_assets 为空或不适用时，将所有 system_prompt 字段设为 null，
但 mode 仍按上述两条规则给出。下游 Agent 将自动使用内置硬编码模板。

注意：strict JSON，不要包含 markdown 代码块标记或额外解释。
"#;

/// guard_pairing（§5.3 逻辑层公理）：mode 与 prompt 配对校验。
///
/// - Orchestration ⇒ converge_system_prompt 非空；
/// - Execution ⇒ verify_system_prompt 非空。
///
/// 返回 `Some(reason)` = 不配对（下游有 Base 模板降级兑底，不中断）；
/// `None` = 配对 OK。半 LLM 半符号的符号层公理——不调 LLM，纯字符串判定。
fn guard_pairing(mode: AgentMode, ctx: &MetaContext) -> Option<String> {
    match mode {
        AgentMode::Orchestration if ctx.converge_system_prompt.is_none() => {
            Some("Orchestration mode missing converge_system_prompt".to_string())
        }
        AgentMode::Execution if ctx.verify_system_prompt.is_none() => {
            Some("Execution mode missing verify_system_prompt".to_string())
        }
        _ => None,
    }
}

/// Build the user message for MetaAgent's LLM composition call.
///
/// Formats task description, depth rules and ranked prompt assets into a
/// structured prompt that the LLM can process to produce a [`MetaContext`].
fn build_llm_input(
    task_description: &str,
    depth: u32,
    max_depth: u32,
    matched: &[&PromptAsset],
) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "## Task Description\n\n{task_description}\n\n\
         ## Recursion Depth\n\
         - current depth: {depth}\n\
         - max depth: {max_depth}\n\
         - leaf rule: depth+1 >= max_depth → mode must be Execution\n\
         \n## Prompt Assets (UCB-ranked)\n"
    ));

    for (i, asset) in matched.iter().enumerate() {
        parts.push(format!(
            "\n### {idx}. {name} (id={id}, target={target})\n```\n{content}\n```",
            idx = i + 1,
            id = asset.id,
            name = asset.name,
            target = asset.agent_target,
            content = asset.content,
        ));
    }

    parts.push("\n\nProduce the MetaContext JSON as instructed.".into());
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::provider::ProviderRegistry;
    use crate::infra::config::TaijiConfig;
    use crate::types::agent::ModelKey;
    use crate::types::task::SubtaskSpec;

    fn make_config() -> TaijiConfig {
        TaijiConfig {
            version: "0.1.0".into(),
            workspace: "default".into(),
            data_root: "./data".into(),
            llm: crate::infra::config::LlmConfig {
                default_provider: "deepseek".into(),
                default_model: "deepseek-chat".into(),
                api_key: "test-key".into(),
                base_url: None,
                agent_overrides: std::collections::HashMap::new(),
                ..Default::default()
            },
            runtime: crate::infra::config::RuntimeConfig::default(),
            knowledge: crate::infra::config::KnowledgeConfig::default(),
            safety: crate::infra::config::SafetyConfig::default(),
            mcp_servers: vec![],
        }
    }

    #[test]
    fn verify_model_serde_roundtrip_and_default() {
        // V37 异源裁判字段：完整 round-trip + 缺字段反序列化 → None（零迁移）。
        let ctx = MetaContext {
            model: Some(ModelKey::from_parts("deepseek", "deepseek-chat")),
            verify_model: Some(ModelKey::from_parts("deepseek", "deepseek-reasoner")),
            ..MetaContext::empty()
        };
        let json = serde_json::to_string(&ctx).expect("serialize");
        assert!(json.contains("verify_model")); // Some → 序列化含字段
        let back: MetaContext = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.verify_model.as_ref().map(|k| k.key()),
            Some("deepseek-deepseek-reasoner")
        );

        // None 时省略（skip_serializing_if）。
        let no_vm = MetaContext { verify_model: None, ..ctx.clone() };
        let json2 = serde_json::to_string(&no_vm).expect("serialize");
        assert!(!json2.contains("verify_model"));

        // 旧文件（无 verify_model 键）→ None。
        let legacy = r#"{"constraints":[],"matched_skills":[],"yang_prompt":{"task_description":"t","constraint_summaries":[],"parent_deliverables":[],"sibling_deliverables":[]},"mode":"Orchestration"}"#;
        let parsed: MetaContext = serde_json::from_str(legacy).expect("legacy parse");
        assert!(parsed.verify_model.is_none());
        assert!(parsed.model.is_none());
    }

    #[test]
    fn subtask_spec_model_serde_default() {
        // V37 子任务级路由字段：缺字段 → None；字符串 ModelKey 直读。
        let spec: SubtaskSpec = serde_json::from_str(
            r#"{"description":"d","verification_spec":"v"}"#,
        )
        .expect("legacy parse");
        assert!(spec.model.is_none());

        let spec2: SubtaskSpec = serde_json::from_str(
            r#"{"description":"d","verification_spec":"v","model":"deepseek-deepseek-reasoner"}"#,
        )
        .expect("parse with model");
        assert_eq!(spec2.model.as_ref().map(|k| k.key()), Some("deepseek-deepseek-reasoner"));

        let round_trip: SubtaskSpec =
            serde_json::from_str(&serde_json::to_string(&spec2).unwrap()).unwrap();
        assert_eq!(round_trip.model.as_ref().map(|k| k.key()), Some("deepseek-deepseek-reasoner"));
    }

    #[test]
    fn model_routing_config_serde_default() {
        // V37 配置：缺字段 → 默认 false（行为与 V36 一致）；显式 true 可解析。
        use crate::infra::config::ModelRoutingConfig;
        let parsed: ModelRoutingConfig =
            serde_json::from_str("{}").expect("empty default");
        assert!(!parsed.heterogeneous_verifier);
        let on: ModelRoutingConfig =
            serde_json::from_str(r#"{"heterogeneous_verifier":true}"#).expect("on");
        assert!(on.heterogeneous_verifier);
    }

    #[tokio::test]
    async fn test_meta_agent_run_returns_empty_context() {
        let config = make_config();
        let tmp_dir = std::env::temp_dir()
            .join(format!("taiji_meta_test_{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()));
        let guizang = Arc::new(
            GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let provider = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        let builder = MetaAgentBuilder::new("test-task", guizang, provider, "deepseek-chat");
        // Empty tags → fallback path → empty MetaContext（Context 分支，非短路）。
        let outcome = builder
            .run("test task description", &[], None)
            .await
            .expect("MetaAgent run");

        let ctx = match outcome {
            crate::types::agent::MetaOutcome::Context(ctx) => ctx,
            crate::types::agent::MetaOutcome::Answer(a) => {
                panic!("empty tags must not short-circuit; got answer: {a}")
            }
        };

        assert!(ctx.constraints.is_empty());
        assert!(ctx.matched_skills.is_empty());
        assert!(ctx.yang_prompt.task_description.is_empty());
        // V27: 降级路径 mode 默认 Orchestration（安全默认）。
        assert_eq!(ctx.mode, crate::types::agent::AgentMode::Orchestration);
        assert!(ctx.yang_system_prompt.is_none());
        assert!(ctx.verify_system_prompt.is_none());
        assert!(ctx.converge_system_prompt.is_none());

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[test]
    fn test_meta_compose_system_prompt_is_valid() {
        // Verify the prompt compiles and contains the required Chinese header.
        assert!(META_COMPOSE_SYSTEM_PROMPT.starts_with("你是权重更新专家"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("yang_system_prompt"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("verify_system_prompt"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("converge_system_prompt"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("你是权重更新专家"));
        // V27: 模式决策（递归层数规则 + 任务难易程度）与阴阳配对引导。
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("递归层数规则"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("任务难易程度"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("阴阳配对"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("\"mode\": \"Orchestration\""));
    }

    #[test]
    fn test_guard_pairing_axioms() {
        // §5.3 逻辑层公理：Orchestration 缺 converge → Some；Execution 缺 verify → Some。
        let mut orch = MetaContext::empty();
        orch.mode = AgentMode::Orchestration;
        orch.verify_system_prompt = Some("v".into());
        assert!(guard_pairing(orch.mode, &orch).is_some());

        let mut exec = MetaContext::empty();
        exec.mode = AgentMode::Execution;
        exec.converge_system_prompt = Some("c".into());
        assert!(guard_pairing(exec.mode, &exec).is_some());

        // 配对 OK → None。
        let mut orch_ok = MetaContext::empty();
        orch_ok.mode = AgentMode::Orchestration;
        orch_ok.converge_system_prompt = Some("c".into());
        assert!(guard_pairing(orch_ok.mode, &orch_ok).is_none());

        let mut exec_ok = MetaContext::empty();
        exec_ok.mode = AgentMode::Execution;
        exec_ok.verify_system_prompt = Some("v".into());
        assert!(guard_pairing(exec_ok.mode, &exec_ok).is_none());
    }

    #[test]
    fn test_build_llm_input_includes_task_description() {
        let assets = vec![
            PromptAsset::new(
                "test-prompt",
                "Test",
                "",
                "You are a test agent",
                "YangAgent",
                vec![],
            ),
        ];
        let input = build_llm_input("Do something", 1, 2, &assets.iter().collect::<Vec<_>>());
        assert!(input.contains("Do something"));
        assert!(input.contains("test-prompt"));
        assert!(input.contains("You are a test agent"));
        // V27: 深度规则注入。
        assert!(input.contains("current depth: 1"));
        assert!(input.contains("max depth: 2"));
        assert!(!input.contains("agent_mode"));
    }

    #[tokio::test]
    async fn test_meta_builder_defaults_and_safety_hook_setter() {
        // 蓝图 V25 §8.5：Meta 带收集工具（read/search/webfetch）→ 必有安全钩子
        // （类型级保证，字段非 Option）；max_turns 默认 6（工具循环余量）。
        let config = make_config();
        let tmp_dir = std::env::temp_dir()
            .join(format!(
                "taiji_meta_builder_test_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let guizang = Arc::new(
            GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let provider = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        let builder = MetaAgentBuilder::new("test-task", guizang, provider, "deepseek-chat");
        // 默认值：max_turns=6，depth=0，max_depth=2，且 safety_hook 恒有值（默认配置实例）。
        assert_eq!(builder.max_turns, 6);
        assert_eq!(builder.depth, 0);
        assert_eq!(builder.max_depth, 2);

        // setter 生效：注入进程级单例后指针一致；depth/max_depth 注入递归层数规则。
        let hook = Arc::new(SafetyHook::new(&SafetyConfig {
            enabled: false,
            trusted_mcp_servers: vec![],
        }));
        let builder = builder
            .safety_hook(hook.clone())
            .max_turns(8)
            .depth(1)
            .max_depth(3);
        assert!(Arc::ptr_eq(&builder.safety_hook, &hook));
        assert_eq!(builder.max_turns, 8);
        assert_eq!(builder.depth, 1);
        assert_eq!(builder.max_depth, 3);

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[test]
    fn meta_compose_result_answer_serde_default() {
        // V46 短路：answer 字段 serde default 零迁移；缺 mode 也能 parse（default Orchestration）。
        let legacy = r#"{"mode":"Execution","answer":null}"#;
        let r: MetaComposeResult =
            serde_json::from_str(legacy).expect("legacy parse");
        assert!(r.answer.is_none());
        assert_eq!(r.mode, AgentMode::Execution);

        // 短路 JSON：只给 answer，不给 mode → mode default Orchestration。
        let short = r#"{"answer":"这是答案"}"#;
        let r2: MetaComposeResult =
            serde_json::from_str(short).expect("short-circuit parse");
        assert_eq!(r2.answer.as_deref(), Some("这是答案"));
        assert_eq!(r2.mode, AgentMode::Orchestration);
    }

    #[test]
    fn meta_outcome_serde_roundtrip() {
        // V46：MetaOutcome 两个出口的 serde 往返。
        let ctx = MetaOutcome::Context(MetaContext::empty());
        let j = serde_json::to_string(&ctx).expect("serialize context");
        let back: MetaOutcome = serde_json::from_str(&j).expect("deserialize context");
        assert!(matches!(back, MetaOutcome::Context(_)));

        let ans = MetaOutcome::Answer("答案".to_string());
        let j2 = serde_json::to_string(&ans).expect("serialize answer");
        let back2: MetaOutcome = serde_json::from_str(&j2).expect("deserialize answer");
        assert!(matches!(back2, MetaOutcome::Answer(ref s) if s == "答案"));
    }

    // ── V50 §6.6 本体消费（纯符号）──

    #[test]
    fn ontology_expand_injects_opposite_type_assets() {
        use crate::types::ontology::{OntologyEdge, OntologyEdgeKind, TaskOntologyView};
        let view = TaskOntologyView {
            domain: "Security".into(),
            action: "Fix".into(),
            objects: vec!["deploy-action".into()],
            env: None,
            is_critical: false,
        };
        let relations = vec![OntologyEdge {
            from: "deploy-action".into(),
            to: "security-check".into(),
            kind: OntologyEdgeKind::WeakDependency,
            strength: 0.9,
            samples: 60,
            evidence: vec![],
        }];
        let mut asset_types = HashMap::new();
        asset_types.insert("prompt-a".to_string(), "security-check".to_string());
        asset_types.insert("prompt-b".to_string(), "deploy-action".to_string());
        let refs = ontology_expand(&view, &relations, &asset_types);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "prompt-a", "对侧类型资产被注入");
    }

    #[test]
    fn ontology_validate_matches_rule_conditions() {
        use crate::types::ontology::{OntologyRule, RuleCondition};
        use crate::types::verification::CheckSeverity;
        let view = TaskOntologyView {
            domain: "Infra".into(),
            action: "Delete".into(),
            objects: vec![],
            env: Some("Production".into()),
            is_critical: true,
        };
        let rules = vec![OntologyRule {
            id: "guard-prod".into(),
            when: RuleCondition { domain: None, env: Some("Production".into()), action: None },
            require: vec!["check:approval".into()],
            forbid: vec![],
            severity: CheckSeverity::Hard,
        }];
        let matched = ontology_validate(&view, &rules);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "guard-prod");
    }

    #[test]
    fn meta_compose_result_ontology_serde_default() {
        // V50 §6.6：ontology 字段 serde default 零迁移；旧 JSON 无 ontology 也能 parse。
        let legacy = r#"{"mode":"Execution","answer":null}"#;
        let r: MetaComposeResult = serde_json::from_str(legacy).expect("legacy parse");
        assert!(r.ontology.is_none());

        let with_onto = r#"{"mode":"Execution","ontology":{"domain":"Security","action":"Fix","objects":["security-check"]}}"#;
        let r2: MetaComposeResult = serde_json::from_str(with_onto).expect("ontology parse");
        assert_eq!(r2.ontology.as_ref().map(|v| v.domain.as_str()), Some("Security"));
    }

    #[test]
    fn classify_task_tags_detects_code() {
        // 批18 P2：代码类任务描述 → ["code"]，激活 code-safety truth。
        assert_eq!(
            classify_task_tags("重构日志模块，修复编译错误"),
            vec!["code".to_string()]
        );
        assert_eq!(
            classify_task_tags("Refactor the logging module and fix compile errors"),
            vec!["code".to_string()]
        );
    }

    #[test]
    fn classify_task_tags_falls_back_general() {
        // 非代码任务（写作/分析等）→ ["general"]。
        assert_eq!(
            classify_task_tags("写一份项目周报，总结本周进展"),
            vec!["general".to_string()]
        );
        assert_eq!(
            classify_task_tags("Summarize the meeting notes"),
            vec!["general".to_string()]
        );
    }
}
