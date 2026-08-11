//! MetaAgent builder (权重更新·元) — "weight update, the meta phase".
//!
//! The MetaAgent is the **first** agent in the TPN cycle.  It queries the
//! 归藏 (cognitive warehouse) via Rig's `dynamic_context` mechanism to extract
//! matched prompt assets that serve as cognitive bias for downstream agents.
//!
//! # Constraints (AGENTS.md §2, §4)
//! - `max_turns = 6` — the MetaAgent is a multi-turn extractor: it can invoke
//!   read-only collection tools (`read` / `search` / `webfetch`) to gather task
//!   context, parent deliverables and web facts before composing weights.
//! - System prompt starts with Chinese identifier "你是权重更新专家".
//! - Output is parsed into [`MetaContext`] which is injected into the
//!   FittingAgent (概率拟合·阳).
//!
//! # Lifecycle
//! 1. [`AgentFactory::create_meta_agent`] resolves LLM config and creates
//!    this builder.
//! 2. [`MetaAgentBuilder::run`] constructs a transient Rig agent, executes it,
//!    and returns a [`MetaContext`].
//! 3. The caller feeds the [`MetaContext`] into `create_fitting_agent`.

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;

use crate::agents::tools::skills::SkillRegistry;
use crate::hooks::safety::SafetyHook;
use crate::infra::config::SafetyConfig;
use crate::infra::error::TaijiError;
use crate::infra::json_util::parse_llm_json;
use crate::infra::knowledge::LiluoClient;
use crate::infra::provider::ProviderRegistry;
use crate::types::agent::{AgentMode, MetaContext, PromptAsset, YangPrompt};
use serde::{Deserialize, Serialize};

/// V32：MetaAgent LLM 编排的**输出契约**——只含 LLM 能决定的字段。
/// 内部类型（constraints / matched_skills / yang_prompt 嵌套结构）由系统
/// 组装，不要求 LLM 输出（实测：LLM 把 matched_skills 输出为字符串数组
/// 导致 parse 必败，3 次重试全失败 → 编排静默失效）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetaComposeResult {
    /// 阴阳配对模式（LLM 按深度规则 + 难度决策）。
    pub mode: AgentMode,
    #[serde(default)]
    pub fitting_system_prompt: Option<String>,
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
    liluo: Arc<LiluoClient>,
    provider: Arc<ProviderRegistry>,
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
}

impl MetaAgentBuilder {
    /// Create a new `MetaAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_meta_agent`] — external
    /// callers should use the factory rather than constructing this directly.
    pub fn new(
        task_id: &str,
        liluo: Arc<LiluoClient>,
        provider: Arc<ProviderRegistry>,
        model: &str,
    ) -> Self {
        Self {
            task_id: task_id.to_string(),
            liluo,
            provider,
            model: model.to_string(),
            depth: 0,
            max_depth: 2, // RuntimeConfig::default().max_depth
            max_turns: 6, // tool-loop headroom: collect → extract
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
        }
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
    /// - `task_type_tags` — tags for 理络 prompt search.  Empty tags produce no
    ///   matches, triggering the fallback path.
    /// - `handoff` — V28 前一瞬态产出（交接文件内容，§8.18）：BACK_TO_META 重跑时
    ///   注入作产出校准（基于失败产物调整权重与资产）；首次运行传 None。
    pub async fn run(
        &self,
        task_description: &str,
        task_type_tags: &[&str],
        handoff: Option<&str>,
    ) -> Result<MetaContext, TaijiError> {
        // ── 0. 模型路由（V36，BCP §8.8 第 1 步——纯符号层，先于分区检索）──
        // 读根级 model_stats 元权重表 → UCB 决策 model_key（全部无统计 → 默认）；
        // 分区检索依赖路由结果（plan.md V32 阻塞点 #1 修正：路由是符号决策，
        // 不需要 LLM）。model_stats 损坏 → 空表（load_model_stats 内 warn），
        // 路由退化为默认模型。
        let model_key = {
            let stats = self.liluo.load_model_stats().await?;
            crate::orchestration::model_router::ModelRouter::new(&self.provider, stats).route()
        };
        tracing::debug!(
            task_id = %self.task_id,
            model_key = %model_key,
            "MetaAgent: model routed (分区检索目标)"
        );

        // ── 1. Query 理絡 for prompt assets（按路由模型分区）──
        let partition = self.liluo.for_model(model_key.key()).await?;
        let prompt_assets = partition.search_prompts(task_type_tags).await?;

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
                "No high-confidence prompt assets in partition — returning empty MetaContext (fallback)"
            );
            let mut empty = MetaContext::empty();
            // V36：分区空 ≠ 路由失败——模型选择保持（Fitting/Causal 按路由模型执行）。
            empty.model = Some(model_key.clone());
            return Ok(empty);
        }

        // ── 2.5 UCB 排序（V35/MVP-5 检索数学化，§6.3 实现层定稿）──
        // 后验均值 μ + 探索项 C·√(ln N_total/(n+1))；n=0 冷启动退化为先验 μ 降序。
        // prior_strength 取 DmnConfig 默认（MetaAgentBuilder 无 config——与 §6.4.1 默认一致）。
        let models = partition.load_all_models().await?;
        let ranked = crate::infra::knowledge::rank_prompts_by_ucb(
            &matched,
            &models,
            crate::orchestration::active_learning::UCB_C,
            10.0,
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
            "Calling LLM to compose MetaContext from 理络 prompt assets"
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

        let client = self.provider.client("deepseek").map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("MetaAgent: failed to get provider client: {e}"),
            }
        })?;

        // ── 收集工具（只读）：read / search / webfetch — 供 LLM 收集任务上下文、
        //    父层 deliverables 与网络信息后更新权重（V25 权限分工：收集工具三相共有）。
        //    带工具必有安全钩子（§8.5 硬约束，类型级保证）：无条件挂载 SafetyHook ──
        let skill_tools: Vec<Box<dyn rig::tool::ToolDyn>> = SkillRegistry::new()
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
                     fitting_system_prompt, verify_system_prompt, converge_system_prompt, \
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

        let ctx = match composed {
            Some(result) => {
                // V32：LLM 只输出它能决定的字段（mode + 三份提示词 + 摘要），
                // 内部类型（constraints / matched_skills / yang_prompt 结构）由
                // 系统组装——不再要求 LLM 输出 SkillRef/TruthConstraint 结构
                // （实测：LLM 把 matched_skills 输出为字符串数组导致 parse 必败）。
                let ctx = MetaContext {
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
                    // 结果：模型选择与资产编排解耦，Fitting/Causal 仍按路由模型
                    // 执行；仅路由异常时 None = 配置默认）。
                    model: Some(model_key.clone()),
                    fitting_system_prompt: result.fitting_system_prompt,
                    verify_system_prompt: result.verify_system_prompt,
                    converge_system_prompt: result.converge_system_prompt,
                };
                // V30 会盟字段（parent_deliverables / sibling_deliverables）由
                // 分封时（RecursiveDecomposeTool）注入，此处保持空。
                tracing::info!(
                    task_id = %self.task_id,
                    mode = ?ctx.mode,
                    has_fitting = ctx.fitting_system_prompt.is_some(),
                    has_verify = ctx.verify_system_prompt.is_some(),
                    has_converge = ctx.converge_system_prompt.is_some(),
                    "MetaAgent: composed MetaContext"
                );
                ctx
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
                empty
            }
        };

        Ok(ctx)
    }
}

/// System prompt for the MetaAgent's LLM composition call.
///
/// Instructs the LLM to decide the node's 阴阳配对模式 (Orchestration |
/// Execution) from recursion depth rules + task difficulty, then compose
/// mode-paired system prompts for downstream agents from 理络 prompt assets
/// (V27).  The Chinese prefix anchors the agent's role per project
/// convention (see AGENTS.md §2).
const META_COMPOSE_SYSTEM_PROMPT: &str = r#"你是权重更新专家 (Weight Update · Meta Agent)。

你的职责是根据任务描述、递归层数规则与认知仓库（理络）中的提示词资产，
先决策当前节点的**阴阳配对模式**，再编排下游 Agent（FittingAgent 概率拟合·阳
和 CausalAgent 因果验证·阴）与该模式**配对**的系统提示词。

## 输入
- task_description：当前任务的完整描述
- depth / max_depth：当前递归层数与最大递归深度
- prompt_assets：理络中匹配的提示词资产列表（按置信度降序排列）
  每项包含：id, name, content, agent_target, tags, confidence

## 模式决策（依据两条规则）
1. **递归层数规则**：若 depth+1 >= max_depth（当前节点是叶子，无法再拆解），
   必须选 "Execution"；深度越浅，越有空间拆解，复杂任务倾向 "Orchestration"。
2. **任务难易程度**：分析任务描述——复杂/多步骤/跨多个独立维度/需要多 Agent
   协作 → "Orchestration"（编排拆解 + 综合）；原子/单步/可直接用 L1 工具
   完成 → "Execution"（直接执行）。

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

## 输出格式（严格 JSON，无额外注释）

{
  "mode": "Orchestration",
  "fitting_system_prompt": "完整的 FittingAgent 系统提示词，与所选模式配对（编排或执行）",
  "verify_system_prompt": "Execution 模式：完整的 verify 系统提示词，以'你是因果验证器'开头；Orchestration 模式可设为 null",
  "converge_system_prompt": "Orchestration 模式：完整的 converge 系统提示词，以'你是收敛判决器'开头；Execution 模式可设为 null",
  "constraint_summaries": [],
  "task_description": "（保持原始 task_description，可原样复制）"
}

## 降级规则
当 prompt_assets 为空或不适用时，将所有 system_prompt 字段设为 null，
但 mode 仍按上述两条规则给出。下游 Agent 将自动使用内置硬编码模板。

注意：strict JSON，不要包含 markdown 代码块标记或额外解释。
"#;

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
        "## Task Description\n\n{}\n\n## Recursion Depth Rules\n\n- current depth: {depth}\n- max depth: {max_depth}\n- leaf rule: depth+1 >= max_depth → mode must be Execution\n\n## Prompt Assets (ranked by UCB score — 数学排序：后验均值 + 探索项，序即优先级)\n",
        task_description
    ));

    for (i, asset) in matched.iter().enumerate() {
        parts.push(format!(
            "\n### Asset {idx}\n\
             - id: {id}\n\
             - name: {name}\n\
             - agent_target: {target}\n\
             - confidence: {conf}\n\
             - content:\n```\n{content}\n```",
            idx = i + 1,
            id = asset.id,
            name = asset.name,
            target = asset.agent_target,
            conf = asset.confidence,
            content = asset.content,
        ));
    }

    parts.push(
        "\n\nBased on the above, produce the MetaContext JSON as instructed.".into(),
    );

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::provider::ProviderRegistry;
    use crate::infra::config::TaijiConfig;

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

    #[tokio::test]
    async fn test_meta_agent_run_returns_empty_context() {
        let config = make_config();
        let tmp_dir = std::env::temp_dir()
            .join(format!("taiji_meta_test_{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()));
        let liluo = Arc::new(
            LiluoClient::new(&tmp_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let provider = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        let builder = MetaAgentBuilder::new("test-task", liluo, provider, "deepseek-chat");
        // Empty tags → fallback path → empty MetaContext.
        let ctx = builder
            .run("test task description", &[], None)
            .await
            .expect("MetaAgent run");

        assert!(ctx.constraints.is_empty());
        assert!(ctx.matched_skills.is_empty());
        assert!(ctx.yang_prompt.task_description.is_empty());
        // V27: 降级路径 mode 默认 Orchestration（安全默认）。
        assert_eq!(ctx.mode, crate::types::agent::AgentMode::Orchestration);
        assert!(ctx.fitting_system_prompt.is_none());
        assert!(ctx.verify_system_prompt.is_none());
        assert!(ctx.converge_system_prompt.is_none());

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[test]
    fn test_meta_compose_system_prompt_is_valid() {
        // Verify the prompt compiles and contains the required Chinese header.
        assert!(META_COMPOSE_SYSTEM_PROMPT.starts_with("你是权重更新专家"));
        assert!(META_COMPOSE_SYSTEM_PROMPT.contains("fitting_system_prompt"));
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
    fn test_build_llm_input_includes_task_description() {
        let assets = vec![
            PromptAsset::new(
                "test-prompt",
                "Test",
                "",
                "You are a test agent",
                "FittingAgent",
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
        let liluo = Arc::new(
            LiluoClient::new(&tmp_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let provider = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        let builder = MetaAgentBuilder::new("test-task", liluo, provider, "deepseek-chat");
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
}
