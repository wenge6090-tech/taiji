//! MetaAgent builder (权重更新·元) — "weight update, the meta phase".
//!
//! The MetaAgent is the **first** agent in the TPN cycle.  It queries the
//! 归藏 (cognitive warehouse) via Rig's `dynamic_context` mechanism to extract
//! matched prompt assets that serve as cognitive bias for downstream agents.
//!
//! # Constraints (AGENTS.md §2, §4)
//! - `max_turns = 1` — the MetaAgent is a single-shot structured extractor.
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

use std::path::PathBuf;
use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;

use crate::infra::error::TaijiError;
use crate::infra::knowledge::LiluoClient;
use crate::infra::provider::ProviderRegistry;
use crate::infra::trace::save_json_atomic;
use crate::types::agent::{MetaContext, PromptAsset};

/// Builder for the MetaAgent (权重更新·元).
///
/// Encapsulates all configuration needed to construct and execute a Rig agent
/// that extracts reasoning paths from the 理络.  Created by
/// [`AgentFactory::create_meta_agent`](super::factory::AgentFactory::create_meta_agent).
#[allow(dead_code)] // R2 production path reserve — fields used when Rig agent is wired
pub struct MetaAgentBuilder {
    task_id: String,
    liluo: Arc<LiluoClient>,
    provider: Arc<ProviderRegistry>,
    model: String,
    /// max_turns = 1 — MetaAgent is always single-shot structured extraction.
    max_turns: u32,
    /// Optional task directory for persisting meta_conversation.json.
    task_dir: Option<PathBuf>,
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
            max_turns: 1, // MetaAgent is always single-shot
            task_dir: None,
        }
    }

    /// Set an optional task directory for persisting meta_conversation.json.
    pub fn task_dir(mut self, path: PathBuf) -> Self {
        self.task_dir = Some(path);
        self
    }

    /// Run the MetaAgent: query the 理络, LLM-compose prompts, produce [`MetaContext`].
    ///
    /// # Flow
    /// 1. Query 理络 via `search_prompts(task_type_tags)` for matching prompt assets.
    /// 2. Filter by confidence threshold (`CONFIDENCE_THRESHOLD = 0.3`).
    /// 3. When matching assets exist → call LLM to compose `MetaContext`.
    /// 4. When no assets or LLM fails → fallback to `MetaContext::empty()`.
    ///
    /// # Parameters
    /// - `task_description` — the task the downstream agents will execute.
    /// - `task_type_tags` — tags for 理络 prompt search.  Empty tags produce no
    ///   matches, triggering the fallback path.
    pub async fn run(
        &self,
        task_description: &str,
        task_type_tags: &[&str],
    ) -> Result<MetaContext, TaijiError> {
        // ── 1. Query 理絡 for prompt assets ──
        let prompt_assets = self.liluo.search_prompts(task_type_tags).await?;

        // ── 2. Confidence filter ──
        const CONFIDENCE_THRESHOLD: f64 = 0.3;
        let matched: Vec<&PromptAsset> = prompt_assets
            .iter()
            .filter(|p| p.confidence >= CONFIDENCE_THRESHOLD)
            .collect();

        if matched.is_empty() {
            tracing::debug!(
                task_id = %self.task_id,
                "No high-confidence prompt assets — returning empty MetaContext (fallback)"
            );
            return Ok(MetaContext::empty());
        }

        // ── 3. LLM call to compose MetaContext ──
        tracing::debug!(
            task_id = %self.task_id,
            matched_count = matched.len(),
            "Calling LLM to compose MetaContext from 理络 prompt assets"
        );

        let llm_prompt = build_llm_input(task_description, &matched);

        let client = self.provider.client("deepseek").map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("MetaAgent: failed to get provider client: {e}"),
            }
        })?;

        let agent = client
            .agent(&self.model)
            .preamble(META_COMPOSE_SYSTEM_PROMPT)
            .default_max_turns(1)
            .build();

        let response = agent.prompt(&llm_prompt).await.map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("MetaAgent LLM call failed: {e}"),
            }
        })?;

        // ── 4. Parse response into MetaContext ──
        let ctx = match serde_json::from_str::<MetaContext>(response.as_ref()) {
            Ok(ctx) => {
                tracing::debug!(
                    task_id = %self.task_id,
                    mode = ?ctx.mode,
                    has_fitting = ctx.fitting_system_prompt.is_some(),
                    has_verify = ctx.verify_system_prompt.is_some(),
                    has_converge = ctx.converge_system_prompt.is_some(),
                    "MetaAgent: successfully composed MetaContext"
                );
                ctx
            }
            Err(e) => {
                tracing::warn!(
                    task_id = %self.task_id,
                    "MetaAgent: failed to parse LLM response as MetaContext: {e} — falling back"
                );
                MetaContext::empty()
            }
        };

        // ── 5. Persist meta_conversation.json for crash recovery ──
        if let Some(ref dir) = self.task_dir {
            let meta_state = serde_json::json!({
                "task_description": task_description,
                "llm_input": llm_prompt,
                "llm_response": &response,
                "meta_ctx": ctx,
            });
            let meta_path = dir.join("meta_conversation.json");
            if let Err(e) = save_json_atomic(&meta_state, &meta_path) {
                tracing::warn!(
                    path = %meta_path.display(),
                    error = %e,
                    "Failed to save meta_conversation"
                );
            }
        }

        Ok(ctx)
    }
}

/// System prompt for the MetaAgent's LLM composition call.
///
/// Instructs the LLM to compose system prompts for downstream agents from
/// 理络 prompt assets.  The Chinese prefix anchors the agent's role per
/// project convention (see AGENTS.md §2).
const META_COMPOSE_SYSTEM_PROMPT: &str = r#"你是权重更新专家 (Weight Update · Meta Agent)。

你的职责是根据任务描述和认知仓库（理络）中的提示词资产，编排下游 Agent
（FittingAgent 概率拟合·阳 和 CausalAgent 因果验证·阴）的系统提示词。

## 输入
- task_description：当前任务的完整描述
- prompt_assets：理络中匹配的提示词资产列表（按置信度降序排列）
  每项包含：id, name, content, agent_target, agent_mode, confidence

## 你需要做的
1. 分析任务描述，判断任务复杂度：
   - 复杂/多步骤/需要多 Agent 协作 → Orchestration 模式
   - 简单/单步骤/可直接执行 → Execution 模式
2. 从 prompt_assets 中选择置信度最高且与 mode 匹配的资产
3. 将其 content 字段组合为三份完整的系统提示词

## 输出格式（严格 JSON，无额外注释）

{
  "mode": "Orchestration",
  "fitting_system_prompt": "完整的 FittingAgent 系统提示词，包含角色定义、指令和约束",
  "verify_system_prompt": "完整的 verify 系统提示词，以'你是因果验证器'开头",
  "converge_system_prompt": "完整的 converge 系统提示词，以'你是收敛判决器'开头",
  "constraints": [],
  "matched_skills": [],
  "yang_prompt": {
    "task_description": "（保持原始 task_description）",
    "constraint_summaries": []
  }
}

## 降级规则
当 prompt_assets 为空或不适用时，将所有 system_prompt 字段设为 null。
下游 Agent 将自动使用内置硬编码模板。

注意：strict JSON，不要包含 markdown 代码块标记或额外解释。
"#;

/// Build the user message for MetaAgent's LLM composition call.
///
/// Formats task description and ranked prompt assets into a structured
/// prompt that the LLM can process to produce a [`MetaContext`].
fn build_llm_input(task_description: &str, matched: &[&PromptAsset]) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "## Task Description\n\n{}\n\n## Prompt Assets (ranked by confidence)\n",
        task_description
    ));

    for (i, asset) in matched.iter().enumerate() {
        parts.push(format!(
            "\n### Asset {idx}\n\
             - id: {id}\n\
             - name: {name}\n\
             - agent_target: {target}\n\
             - agent_mode: {mode:?}\n\
             - confidence: {conf}\n\
             - content:\n```\n{content}\n```",
            idx = i + 1,
            id = asset.id,
            name = asset.name,
            target = asset.agent_target,
            mode = asset.agent_mode,
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
    use crate::types::agent::AgentMode;

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
        let ctx = builder.run("test task description", &[]).await.expect("MetaAgent run");

        assert!(ctx.constraints.is_empty());
        assert!(ctx.matched_skills.is_empty());
        assert!(ctx.yang_prompt.task_description.is_empty());
        assert!(ctx.fitting_system_prompt.is_none());
        assert!(ctx.verify_system_prompt.is_none());
        assert!(ctx.converge_system_prompt.is_none());
        assert_eq!(ctx.mode, AgentMode::Orchestration);

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
                AgentMode::Orchestration,
                vec![],
            ),
        ];
        let input = build_llm_input("Do something", &assets.iter().collect::<Vec<_>>());
        assert!(input.contains("Do something"));
        assert!(input.contains("test-prompt"));
        assert!(input.contains("You are a test agent"));
        assert!(input.contains("Orchestration"));
    }
}
