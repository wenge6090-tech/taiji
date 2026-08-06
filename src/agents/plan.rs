//! PlanBuilder (预演编排) — MetaAgent + LLM plan composition.
//!
//! Produces a [`PlanSummary`] by running the MetaAgent (权重更新·元) to obtain
//! cognitive context from 归藏, then asking an LLM to compose a structured
//! execution plan **without** entering the TPN loop.
//!
//! # Lifecycle
//! 1. [`AgentFactory::create_plan_agent`] resolves LLM config and creates
//!    this builder.
//! 2. [`PlanBuilder::plan`] runs MetaAgent, then calls the LLM to compose
//!    a [`PlanSummary`].
//! 3. The caller (MCP handler) returns the [`PlanSummary`] as JSON.
//!
//! # No side effects
//! `plan()` is read-only (reads 归藏, calls LLMs).  It does NOT create a
//! task directory, does NOT materialise external context, and does NOT
//! enter the TPN cycle.

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;

use crate::infra::error::TaijiError;
use crate::infra::json_util::parse_llm_json;
use crate::infra::knowledge::LiluoClient;
use crate::infra::provider::ProviderRegistry;
use crate::types::agent::MetaContext;
use crate::types::plan::PlanSummary;

/// Builder for producing a pre-execution plan summary.
///
/// Encapsulates the MetaAgent (to query 归藏) and a dedicated LLM call for
/// plan composition.  Created by [`AgentFactory::create_plan_agent`].
pub struct PlanBuilder {
    task_id: String,
    liluo: Arc<LiluoClient>,
    provider: Arc<ProviderRegistry>,
    model: String,
}

impl PlanBuilder {
    /// Create a new `PlanBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_plan_agent`] — external
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
        }
    }

    /// Produce a [`PlanSummary`] by:
    ///
    /// 1. Running the MetaAgent to obtain a [`MetaContext`] (cognitive bias).
    /// 2. Using an LLM to compose a structured plan from the task description
    ///    and cognitive context.
    ///
    /// # Arguments
    ///
    /// * `description` — the task description to plan for.
    /// * `task_type_tags` — tags for 归藏 prompt search (empty → no assets).
    pub async fn plan(
        &self,
        description: &str,
        task_type_tags: &[&str],
    ) -> Result<PlanSummary, TaijiError> {
        // ── Step 1: Run MetaAgent ──────────────────────────────────────
        let meta_ctx = self.run_meta_agent(description, task_type_tags).await?;

        // ── Step 2: Compose PlanSummary via LLM ────────────────────────
        let plan = self.compose_plan(description, &meta_ctx).await?;

        Ok(plan)
    }

    /// Run the MetaAgent (权重更新·元) to obtain cognitive context.
    ///
    /// This queries 归藏 `prompts/` assets, filters by confidence, and
    /// calls the LLM to compose a [`MetaContext`].  When no assets match,
    /// returns an empty (degraded) MetaContext.
    async fn run_meta_agent(
        &self,
        description: &str,
        task_type_tags: &[&str],
    ) -> Result<MetaContext, TaijiError> {
        let meta_agent = crate::agents::meta::MetaAgentBuilder::new(
            &self.task_id,
            self.liluo.clone(),
            self.provider.clone(),
            &self.model,
        );
        meta_agent.run(description, task_type_tags).await
    }

    /// Ask the LLM to compose a [`PlanSummary`] from the task description
    /// and the MetaAgent's cognitive context.
    ///
    /// When `meta_ctx` is empty (no 归藏 assets matched), the LLM still
    /// produces a reasonable plan from the task description alone.
    async fn compose_plan(
        &self,
        description: &str,
        meta_ctx: &MetaContext,
    ) -> Result<PlanSummary, TaijiError> {
        let llm_input = build_plan_prompt(description, meta_ctx);

        let client = self.provider.client("deepseek").map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("PlanBuilder: failed to get provider client: {e}"),
            }
        })?;

        let agent = client
            .agent(&self.model)
            .preamble(PLAN_COMPOSE_SYSTEM_PROMPT)
            .max_tokens(2048u64)
            .default_max_turns(1usize)
            .build();

        let response = agent.prompt(&llm_input).await.map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("PlanBuilder LLM call failed: {e}"),
            }
        })?;

        // Parse the LLM response as PlanSummary JSON
        let plan: PlanSummary = parse_llm_json(response.as_ref()).map_err(|e| {
            TaijiError::StructuredOutputParseFailed {
                context: format!(
                    "Failed to parse PlanSummary from LLM response: {e}. Raw: {response}"
                ),
            }
        })?;

        tracing::debug!(
            task_id = %self.task_id,
            complexity = %plan.estimated_complexity,
            subtask_count = plan.estimated_subtasks.len(),
            "PlanBuilder: plan composed successfully"
        );

        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// System prompt for plan composition
// ---------------------------------------------------------------------------

/// System prompt for the PlanBuilder's LLM composition call.
///
/// Instructs the LLM to produce a structured [`PlanSummary`] from a task
/// description and cognitive context.  The Chinese prefix anchors the agent's
/// role per project convention (see AGENTS.md).
const PLAN_COMPOSE_SYSTEM_PROMPT: &str = r#"你是任务规划专家 (Task Planning Expert)。

你的职责是根据任务描述和认知上下文（MetaContext），编排一份结构化的 PlanSummary 执行计划。

## 输入
- task_description：需要规划的任务描述
- MetaContext：认知上下文，包含约束条件、技能匹配等

## 你需要做的
1. 分析任务描述，评估复杂度（simple / moderate / complex）
2. 判断是否需要拆分子任务：复杂任务可拆解，简单任务直接执行
3. 列出预估的子任务（每个子任务含描述、验证方式、所需技能）
4. 推荐可能需要的 L1 Skills（read / write / bash / search / webfetch）
5. 描述预期交付产物
6. 总结匹配的归藏提示词和相关约束

## 输出格式（严格 JSON，无额外注释）

{
  "task_analysis": "1-2 句话的任务分析",
  "estimated_subtasks": [
    {
      "description": "子任务描述",
      "verification_approach": "验证方法说明",
      "required_skills": ["read", "write"]
    }
  ],
  "recommended_skills": ["read", "write", "bash"],
  "expected_deliverables": ["交付产物描述1", "交付产物描述2"],
  "estimated_complexity": "moderate",
  "matched_prompts_summary": "归藏提示词匹配摘要",
  "relevant_constraints": ["约束说明1"]
}

## 降级规则
当 MetaContext 为空时（认知上下文无匹配资产），仍根据 task_description
做出合理估计。将 matched_prompts_summary 设为 "未匹配归藏资产，基于任务描述直
接判断"，对应的约束字段留空。

注意：strict JSON，不要包含 markdown 代码块标记或额外解释。
子任务列表可以为空（简单任务不需要拆解）。
"#;

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

/// Format the LLM input for the plan composition call.
///
/// Combines the task description and a summary of the MetaContext into a
/// structured prompt that the LLM can process.
fn build_plan_prompt(description: &str, meta_ctx: &MetaContext) -> String {
    let mut parts = Vec::new();

    // Task description
    parts.push(format!(
        "## Task Description\n\n{description}\n\n## MetaContext\n"
    ));

    // Constraints
    if !meta_ctx.constraints.is_empty() {
        parts.push(format!(
            "- Constraints: {} constraint(s)",
            meta_ctx.constraints.len()
        ));
        for (i, c) in meta_ctx.constraints.iter().enumerate() {
            parts.push(format!("  {}: {} ({:?})", i + 1, c.name, c.severity));
        }
    }

    // Matched skills
    if !meta_ctx.matched_skills.is_empty() {
        parts.push(format!(
            "- Matched skills: {}",
            meta_ctx
                .matched_skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Fitting system prompt (summary)
    if let Some(prompt) = &meta_ctx.fitting_system_prompt {
        // Truncate to first 200 chars for the prompt
        let max_chars = 200usize;
        let snippet = if prompt.len() > max_chars {
            format!("{}...", &prompt[..max_chars])
        } else {
            prompt.clone()
        };
        parts.push(format!(
            "- Fitting system prompt (first {max_chars} chars): {snippet}"
        ));
    }

    // Closing instruction
    parts.push(
        "\n\nBased on the above, produce the PlanSummary JSON as instructed.".into(),
    );

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_compose_system_prompt_starts_with_chinese() {
        assert!(
            PLAN_COMPOSE_SYSTEM_PROMPT.starts_with("你是任务规划专家"),
            "System prompt must start with Chinese identifier"
        );
        assert!(
            PLAN_COMPOSE_SYSTEM_PROMPT.contains("PlanSummary"),
            "Must reference PlanSummary"
        );
    }

    #[test]
    fn test_build_plan_prompt_empty_meta_ctx() {
        let ctx = MetaContext::empty();
        let prompt = build_plan_prompt("Do something", &ctx);
        assert!(prompt.contains("Do something"));
        assert!(prompt.contains("MetaContext"));
        // V26: prompt 不再携带 AgentMode（异层同构）
        assert!(!prompt.contains("AgentMode"));
        // Empty context should not list constraints
        assert!(!prompt.contains("Constraints:"));
    }

    #[test]
    fn test_build_plan_prompt_includes_constraints() {
        use crate::types::verification::TruthConstraint;

        let mut ctx = MetaContext::empty();
        ctx.constraints.push(TruthConstraint::hard(
            "t1",
            "NoFabrication",
            "Don't fabricate facts",
        ));
        let prompt = build_plan_prompt("Test task", &ctx);
        assert!(prompt.contains("NoFabrication"));
        assert!(prompt.contains("Hard"));
    }
}
