//! MetaAgent builder (权重更新·元) — "weight update, the meta phase".
//!
//! The MetaAgent is the **first** agent in the TPN cycle.  It traverses the
//! 理络 (cognitive network) via Rig's `dynamic_context` mechanism to extract
//! reasoning paths that serve as cognitive bias for downstream agents.
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

use std::sync::Arc;

use crate::infra::error::TaijiError;
use crate::infra::knowledge::LiluoClient;
use crate::infra::provider::ProviderRegistry;
use crate::types::agent::{MetaContext, YangPrompt};

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
        }
    }

    /// Run the MetaAgent: traverse the 理络 and produce a [`MetaContext`].
    ///
    /// # Production path
    /// In the fully wired implementation, `run()` will:
    /// 1. Obtain the provider client from `self.provider.client()`.
    /// 2. Build a Rig agent with `dynamic_context(5, self.liluo)`:
    ///    ```ignore
    ///    let client = self.provider.client("deepseek")?;
    ///    let agent = client
    ///        .agent(&self.model)
    ///        .preamble(META_SYSTEM_PROMPT)
    ///        .max_turns(1)
    ///        .dynamic_context(5, self.liluo)
    ///        .build();
    ///    ```
    /// 3. Call `agent.prompt(task_description).await` to get structured output.
    /// 4. Parse the output into `MetaContext`.
    ///
    /// # Current behaviour (degraded mode)
    /// Returns an empty `MetaContext` with no reasoning paths.  This allows the
    /// system to compile and run without a wired dynamic context index, albeit
    /// without cognitive bias from prior knowledge.
    pub async fn run(&self) -> Result<MetaContext, TaijiError> {
        // ── Production implementation (pinned for Rig API verification) ──
        //
        // use rig::providers::deepseek;
        // use rig_core::agent::AgentBuilder;
        //
        // let client = self.provider.client("deepseek")
        //     .map_err(|e| TaijiError::LLMCallFailed {
        //         context: format!("failed to get provider client: {e}"),
        //     })?;
        //
        // let agent = client
        //     .agent(&self.model)
        //     .preamble(META_SYSTEM_PROMPT)
        //     .max_turns(1)
        //     // TODO: dynamic_context(5, self.liluo) once the 理络 index is wired
        //     .build();
        //
        // let response = agent
        //     .prompt("Traverse the 理络 and extract reasoning paths for task")
        //     .await
        //     .map_err(|e| TaijiError::LLMCallFailed {
        //         context: format!("MetaAgent LLM call failed: {e}"),
        //     })?;
        //
        // let meta_ctx: MetaContext = serde_json::from_str(response.as_ref())
        //     .map_err(|e| TaijiError::StructuredOutputParseFailed {
        //         context: format!("failed to parse MetaContext: {e}"),
        //     })?;
        //
        // Ok(meta_ctx)

        // ── Degraded mode: return empty defaults ──
        tracing::debug!(
            task_id = %self.task_id,
            "MetaAgent.run() returning empty MetaContext (degraded mode)"
        );

        Ok(MetaContext {
            reasoning_paths: vec![],
            constraints: vec![],
            matched_skills: vec![],
            yang_prompt: YangPrompt {
                task_description: String::new(),
                reasoning_path_summaries: vec![],
                constraint_summaries: vec![],
            },
        })
    }
}

/// System prompt for the MetaAgent.
///
/// Instructs the LLM to traverse the 理络, discover relevant reasoning chains,
/// and emit a structured [`MetaContext`] JSON object.
///
/// The Chinese prefix anchors the agent's role per project convention
/// (see AGENTS.md §2: "CausalAgent verify mode starts with '你是因果验证器'").
#[allow(dead_code)] // R2 production path reserve — used in run() production path
const META_SYSTEM_PROMPT: &str = r#"你是权重更新专家 (Weight Update · Meta Agent).

Your role is to traverse the 理络 (cognitive network) and extract
reasoning paths relevant to the current task.

Instructions:
1. Use the dynamic context to query the 理络 for relevant knowledge grids.
2. Follow 1-3 hop BFS relations from matching source grids.
3. Compile a list of ReasoningPath objects, each showing the chain of evidence.
4. Identify any L4 Truth constraints that apply to the current task type tags.
5. Match available L1 skills that are relevant to the task.

Output must be a valid JSON object matching the MetaContext schema:
{
  "reasoning_paths": [{ "source_grid": "...", "chains": [...], "depth": 1, "task_type_tags": [...] }],
  "constraints": [{ "id": "...", "name": "...", "description": "...", "severity": "Hard|Soft" }],
  "matched_skills": [{ "id": "...", "name": "...", "tool_name": "...", "match_weight": 0.8 }],
  "yang_prompt": {
    "task_description": "...",
    "reasoning_path_summaries": ["..."],
    "constraint_summaries": ["..."]
  }
}

Do not fabricate reasoning paths. Only include chains that are grounded
in the 理络 data returned by the context index.
"#;

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
        let ctx = builder.run().await.expect("MetaAgent run");

        // In degraded mode, all fields are empty.
        assert!(ctx.reasoning_paths.is_empty());
        assert!(ctx.constraints.is_empty());
        assert!(ctx.matched_skills.is_empty());
        assert!(ctx.yang_prompt.task_description.is_empty());

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[test]
    fn test_meta_system_prompt_is_valid() {
        // Verify the prompt compiles and contains the required Chinese header.
        assert!(META_SYSTEM_PROMPT.starts_with("你是权重更新专家"));
        assert!(META_SYSTEM_PROMPT.contains("reasoning_paths"));
        assert!(META_SYSTEM_PROMPT.contains("MetaContext"));
    }
}
