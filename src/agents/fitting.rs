//! FittingAgent builder (概率拟合·阳) — "probability fitting, the yang phase".
//!
//! The FittingAgent is the **second** agent in the TPN cycle.  It receives the
//! [`MetaContext`] produced by the MetaAgent (权重更新·元) as a reasoning bias
//! and executes along the extracted reasoning paths, either solving the task
//! directly or recursively decomposing it into subtasks.
//!
//! # Toolset
//! The FittingAgent's transient Rig agent is wired with:
//! - **L1 Skills**: tools matched by [`SkillTriggerEngine`] (e.g. `read`,
//!   `write`, `bash`, `webfetch`, `search`).
//! - **`recursive_decompose`**: spawns child FittingAgents for subtasks.
//! - **`causal_verify`**: invokes CausalAgent.verify() on intermediate outputs.
//! - **`SafetyHook`** and **`TraceHook`**: registered as Rig `PromptHook`s.
//!
//! # Constraints (AGENTS.md §2)
//! - `max_turns = config.runtime.max_rounds` (default 30).
//! - System prompt is built from `meta_ctx.yang_prompt`.
//!
//! # Lifecycle
//! 1. [`AgentFactory::create_fitting_agent`] creates this builder.
//! 2. The runner calls [`FittingAgentBuilder::run`] with the task description.
//! 3. The internal Rig agent executes, calling tools and possibly recursing.
//! 4. Returns a [`TPNResult`] with the final content and tool usage summary.

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::deepseek;

use crate::agents::factory::AgentFactory;
use crate::hooks::trace::TraceHook;
use crate::infra::error::TaijiError;
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;
use crate::types::task::TPNResult;

/// Builder for the FittingAgent (概率拟合·阳).
///
/// Created by [`AgentFactory::create_fitting_agent`].  Encapsulates the
/// reasoning bias ([`MetaContext`]), engine context (depth, cycle, round),
/// and a handle to the factory for spawning sub-agents during recursion.
pub struct FittingAgentBuilder {
    depth: u32,
    meta_ctx: MetaContext,
    engine_ctx: EngineContext,
    factory: Arc<AgentFactory>,
    model: String,
}

impl FittingAgentBuilder {
    /// Create a new `FittingAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_fitting_agent`] — external
    /// callers should use the factory rather than constructing this directly.
    pub fn new(
        depth: u32,
        meta_ctx: MetaContext,
        engine_ctx: EngineContext,
        factory: Arc<AgentFactory>,
        model: &str,
    ) -> Self {
        Self {
            depth,
            meta_ctx,
            engine_ctx,
            factory,
            model: model.to_string(),
        }
    }

    /// Run the FittingAgent: execute the task along reasoning paths.
    ///
    /// This method constructs a transient Rig agent equipped with:
    /// - `System prompt`: composed from [`MetaContext::yang_prompt`], which
    ///   includes the task description, reasoning path summaries, and
    ///   constraint summaries.
    /// - `max_turns`: set to `factory.config.runtime.max_rounds`.
    /// - `Tools`: L1 skills matched by [`SkillTriggerEngine`], plus
    ///   `recursive_decompose` and `causal_verify`.
    /// - `Hooks`: [`SafetyHook`] and [`TraceHook`] for security and tracing.
    ///
    /// # Production wiring (pinned for Rig API verification)
    ///
    /// ```ignore
    /// use rig::providers::deepseek;
    /// use rig_core::agent::AgentBuilder;
    /// use crate::hooks::safety::SafetyHook;
    /// use crate::hooks::trace::TraceHook;
    /// use crate::agents::tools::recursive_decompose::RecursiveDecompose;
    /// use crate::agents::tools::causal_verify::CausalVerifyTool;
    ///
    /// let client = self.factory.providers.client("deepseek")?;
    ///
    /// let system_prompt = build_system_prompt(&self.meta_ctx);
    ///
    /// let mut agent = client
    ///     .agent(&self.model)
    ///     .preamble(&system_prompt)
    ///     .max_turns(self.factory.config.runtime.max_rounds);
    ///
    /// // Register matched L1 skills as tools
    /// for skill in &self.meta_ctx.matched_skills {
    ///     agent = agent.tool(skill.tool_name.clone());
    /// }
    ///
    /// // Register built-in tools
    /// let task_dir = self.factory.task_dir(&self.engine_ctx.task_id);
    /// let trace_hook = TraceHook::new(&task_dir, &self.engine_ctx, &self.model);
    /// let safety_hook = self.factory.safety_hook.clone();
    ///
    /// // Wire hooks via builder .hook() method (Rig 0.39)
    /// // agent = agent.hook(safety_hook).hook(trace_hook);
    ///
    /// let agent = agent.build();
    ///
    /// let response = agent
    ///     .prompt(task_description)
    ///     .await
    ///     .map_err(|e| TaijiError::LLMCallFailed {
    ///         context: format!("FittingAgent LLM call failed: {e}"),
    ///     })?;
    ///
    /// Ok(TPNResult {
    ///     task_id: self.engine_ctx.task_id.clone(),
    ///     content: response.as_ref().to_string(),
    ///     tools_used: vec![],
    ///     deliverables: vec![],
    ///     depth: self.depth,
    ///     rounds: 1,
    /// })
    /// ```
    ///
    /// # Current state (TODO)
    /// The actual Rig agent construction and LLM invocation is stubbed with
    /// [`todo!`] here.  The struct, constructor, and method signatures are
    /// complete and correct.  The LLM wiring will be filled in once the
    /// Rig 0.39 API surface is finalised.
    ///
    /// # Returns
    /// - `Ok(TPNResult)` on successful execution.
    /// - `Err(TaijiError::LLMCallFailed)` if the underlying LLM call fails
    ///   (after retries).
    pub async fn run(&self, task_description: &str) -> Result<TPNResult, TaijiError> {
        // ── Guard against runaway recursion ──
        let max_depth = self.factory.config.runtime.max_depth;
        if self.depth > max_depth {
            return Err(TaijiError::MaxDepthExceeded { max: max_depth });
        }

        // ── Build system prompt from MetaContext ──
        let system_prompt = build_system_prompt(&self.meta_ctx, &self.engine_ctx.task_dir);

        // ── Obtain LLM client ──
        let client: Arc<deepseek::Client> = self.factory.providers.client("deepseek")?;

        // ── Build Rig agent with preamble, max_turns, max_tokens, temperature ──
        // Check if agent-specific max_turns is configured; fallback to max_rounds.
        let max_turns = self.factory.config.llm.agent_overrides
            .get("fitting")
            .and_then(|o| o.max_turns)
            .unwrap_or(self.factory.config.runtime.max_rounds) as usize;
        #[allow(unused)]
        let max_tokens = self.factory.config.llm.agent_overrides
            .get("fitting")
            .and_then(|o| o.max_tokens)
            .map(|v| v as u64);
        #[allow(unused)]
        let temperature = self.factory.config.llm.agent_overrides
            .get("fitting")
            .and_then(|o| o.temperature);
        let mut agent_builder = client
            .agent(&self.model)
            .preamble(&system_prompt)
            .default_max_turns(max_turns);
        // Applies to Rig AgentBuilder if the method is available
        if let Some(v) = max_tokens {
            agent_builder = agent_builder.max_tokens(v);
        }
        if let Some(v) = temperature {
            agent_builder = agent_builder.temperature(v);
        }

        // ── Register hooks (safety + trace) ──
        let trace_hook = TraceHook::new(&self.engine_ctx, &self.model);
        let safety_hook = self.factory.safety_hook.as_ref().clone();
        let agent_builder = agent_builder.hook(safety_hook).hook(trace_hook);

        // ── Register L1 skill tools ──
        // Each matched SkillRef is wrapped in a SkillTool adapter.
        // The adapter implements Rig's Tool trait so the LLM can call it.
        for skill in &self.meta_ctx.matched_skills {
            let skill_tool = crate::agents::tools::skills::SkillTool::new(skill.clone());
            // TODO: Register as Rig Tool<M> once SkillTool implements the Rig Tool trait.
            // In the current phase (pre-Rig-API-verification), skills are tracked
            // in the system prompt but not registered as callable tools.
            let _ = skill_tool;
        }

        // ── Register built-in composite tools ──
        // RecursiveDecomposeTool and CausalVerifyTool need Rig Tool<M> adapters.
        // For now they are documented in the system prompt instructions.
        // TODO: agent_builder = agent_builder.tool(RecursiveDecomposeRigAdapter::new(...));

        // ── Build the agent ──
        let agent = agent_builder.build();

        // ── Execute the prompt ──
        let response = agent
            .prompt(task_description)
            .await
            .map_err(|e| TaijiError::LLMCallFailed {
                context: format!("FittingAgent LLM call failed: {e}"),
            })?;

        Ok(TPNResult {
            task_id: self.engine_ctx.task_id.clone(),
            content: response,
            tools_used: Vec::new(),
            deliverables: Vec::new(),
            depth: self.depth,
            rounds: 1,
        })
    }

    /// Return a reference to the engine context.
    pub fn engine_ctx(&self) -> &EngineContext {
        &self.engine_ctx
    }

    /// Return the configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

/// Build the system prompt for the FittingAgent from the MetaContext.
///
/// The prompt includes:
/// - The Chinese role header for 概率拟合·阳.
/// - The task description from the yang prompt.
/// - Summaries of the reasoning paths from the NSKG traversal.
/// - Summaries of applicable L4 Truth constraints.
/// - The output directory path (`task_dir/deliverables/`) so LLM-generated
///   artifacts land in the correct layer-specific folder.
/// - Instructions for tool usage and recursion.
fn build_system_prompt(meta_ctx: &MetaContext, task_dir: &std::path::Path) -> String {
    let mut prompt = String::with_capacity(1024);

    prompt.push_str("你是概率拟合专家 (Probability Fitting · Yang Agent).\n\n");

    // Task description
    if !meta_ctx.yang_prompt.task_description.is_empty() {
        prompt.push_str("## Task\n");
        prompt.push_str(&meta_ctx.yang_prompt.task_description);
        prompt.push_str("\n\n");
    }

    // Reasoning path summaries
    if !meta_ctx.yang_prompt.reasoning_path_summaries.is_empty() {
        prompt.push_str("## Reasoning Paths (from NSKG)\n");
        for (i, summary) in meta_ctx.yang_prompt.reasoning_path_summaries.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, summary));
        }
        prompt.push_str("\n");
    }

    // Constraint summaries
    if !meta_ctx.yang_prompt.constraint_summaries.is_empty() {
        prompt.push_str("## Constraints\n");
        for summary in &meta_ctx.yang_prompt.constraint_summaries {
            prompt.push_str(&format!("- {}\n", summary));
        }
        prompt.push_str("\n");
    }

    // Output directory — every layer (root or recursive child) gets its own.
    let deliverables_dir = task_dir.join("deliverables");
    prompt.push_str("## 产出目录\n");
    prompt.push_str(&format!(
        "所有产物文件请放入以下目录: `{}`\n",
        deliverables_dir.display()
    ));
    prompt.push_str("使用相对路径即可（如 `report.md`）。递归子任务也会有自己的同名目录，结构一致。\n\n");

    // Available tools
    if !meta_ctx.matched_skills.is_empty() {
        prompt.push_str("## Available Tools\n");
        for skill in &meta_ctx.matched_skills {
            prompt.push_str(&format!(
                "- `{}` ({}): {}\n",
                skill.tool_name, skill.name, skill.id
            ));
        }
        prompt.push_str("\n");
    }

    // General instructions
    prompt.push_str(
        "## Instructions\n\
         - Use the available tools to gather information and execute actions.\n\
         - If the task is too complex, use the `recursive_decompose` tool to\n\
           break it into subtasks.\n\
         - Use `causal_verify` to check intermediate results against constraints.\n\
         - When all subtasks complete, provide a final summary.\n\
         - Follow all constraints strictly — hard violations will cause immediate failure.\n"
    );

    prompt
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::factory::AgentFactory;
    use crate::hooks::safety::SafetyHook;
    use crate::infra::config::{LlmConfig, SafetyConfig, TaijiConfig};
    use crate::infra::provider::ProviderRegistry;
    use crate::infra::qdrant::NskgClient;
    use crate::orchestration::constraint_engine::ConstraintEngine;
    use crate::orchestration::trigger_engine::SkillTriggerEngine;
    use crate::orchestration::worker_pool::WorkerPool;
    use crate::types::agent::{ReasoningPath, SkillRef, YangPrompt};
    use crate::types::verification::TruthConstraint;

    fn make_config() -> TaijiConfig {
        TaijiConfig {
            version: "0.1.0".into(),
            workspace: "default".into(),
            data_root: "./data".into(),
            llm: LlmConfig {
                default_provider: "deepseek".into(),
                default_model: "deepseek-chat".into(),
                api_key: "test-key".into(),
                base_url: None,
                agent_overrides: std::collections::HashMap::new(),
                ..Default::default()
            },
            runtime: crate::infra::config::RuntimeConfig::default(),
            qdrant: crate::infra::config::QdrantConfig::default(),
            safety: SafetyConfig::default(),
            mcp_servers: vec![],
        }
    }

    /// Build an Arc<AgentFactory> for testing (requires Qdrant).
    async fn build_factory_arc(config: TaijiConfig) -> Arc<AgentFactory> {
        let nskg = Arc::new(
            NskgClient::new(&config.qdrant)
                .await
                .expect("Qdrant must be running"),
        );
        let providers = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        Arc::new(AgentFactory::new(
            nskg,
            providers,
            config,
            Arc::new(SafetyHook::new(&SafetyConfig::default())),
            Arc::new(WorkerPool::new(4)),
            Arc::new(ConstraintEngine::new()),
            Arc::new(SkillTriggerEngine::new()),
        ))
    }

    fn sample_meta_context() -> MetaContext {
        MetaContext {
            reasoning_paths: vec![ReasoningPath {
                source_grid: "grid-42".into(),
                chains: vec![],
                depth: 1,
                task_type_tags: vec!["code".into()],
            }],
            constraints: vec![TruthConstraint {
                id: "truth:no-fabrication".into(),
                name: "不编造事实".into(),
                description: "Don't fabricate facts".into(),
                severity: crate::types::verification::ConstraintSeverity::Hard,
            }],
            matched_skills: vec![SkillRef {
                id: "read".into(),
                name: "文件读取".into(),
                tool_name: "read".into(),
                match_weight: 0.9,
            }],
            yang_prompt: YangPrompt {
                task_description: "Refactor the logging module.".into(),
                reasoning_path_summaries: vec![
                    "Path 1: grid-42 → grid-17 (depends_on)".into(),
                ],
                constraint_summaries: vec![
                    "Hard: Do not fabricate facts".into(),
                ],
            },
        }
    }

    #[test]
    fn test_build_system_prompt_contains_role_header() {
        let ctx = sample_meta_context();
        let prompt = build_system_prompt(
            &ctx,
            &std::path::PathBuf::from("./test_data/tasks/prompt-test"),
        );
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("Refactor the logging module"));
        assert!(prompt.contains("grid-42"));
        assert!(prompt.contains("recursive_decompose"));
        assert!(prompt.contains("产出目录"));
    }

    #[test]
    fn test_build_system_prompt_empty_context() {
        let ctx = MetaContext {
            reasoning_paths: vec![],
            constraints: vec![],
            matched_skills: vec![],
            yang_prompt: YangPrompt {
                task_description: String::new(),
                reasoning_path_summaries: vec![],
                constraint_summaries: vec![],
            },
        };
        let prompt = build_system_prompt(&ctx, &std::path::PathBuf::from("./test_data/tasks/empty-test"));
        // Should still have the role header and instructions.
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("Instructions"));
        assert!(prompt.contains("产出目录"));
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_fitting_agent_builder_construction() {
        let config = make_config();
        let factory = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();
        let engine_ctx = EngineContext {
            task_id: "test-task-1".into(),
            depth: 0,
            task_dir: std::path::PathBuf::from("./test_data/tasks/test-task-1"),
            cycle: 1,
            round: 0,
        };

        let builder = factory.create_fitting_agent(0, &meta_ctx, &engine_ctx);
        assert!(builder.is_ok());
        let builder = builder.unwrap();
        assert_eq!(builder.engine_ctx().task_id, "test-task-1");
    }

    #[tokio::test]
    #[ignore = "requires Qdrant + LLM API key"]
    async fn test_fitting_agent_run_integration() {
        // Integration test: runs the full Rig agent pipeline.
        // Requires Qdrant (for factory construction) + a valid DEEPSEEK_API_KEY.
        let config = make_config();
        let factory = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();
        let engine_ctx = EngineContext {
            task_id: "test-task-2".into(),
            depth: 1,
            task_dir: std::path::PathBuf::from("./test_data/tasks/test-task-2"),
            cycle: 1,
            round: 0,
        };

        let builder = factory
            .create_fitting_agent(1, &meta_ctx, &engine_ctx)
            .expect("builder");

        let result = builder.run("Write a test for the logging module").await;
        // In an environment without LLM access, expect LLMCallFailed.
        // With a valid API key + Qdrant, this should return Ok(TPNResult).
        match result {
            Ok(tpn) => {
                assert!(!tpn.content.is_empty());
                assert_eq!(tpn.depth, 1);
            }
            Err(e) => {
                // Expected: provider client or LLM call failure.
                assert!(
                    format!("{:?}", e).contains("LLMCallFailed")
                        || format!("{:?}", e).contains("Safety")
                        || format!("{:?}", e).contains("Provider"),
                    "unexpected error: {e:?}"
                );
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_fitting_agent_depth_check() {
        let config = make_config();
        let factory = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();

        // Depth 1, but max_depth defaults to 2 — should pass the guard.
        let engine_ctx = EngineContext {
            task_id: "depth-test".into(),
            depth: 1,
            task_dir: std::path::PathBuf::from("./test_data/tasks/depth-test"),
            cycle: 1,
            round: 0,
        };
        let builder = factory
            .create_fitting_agent(1, &meta_ctx, &engine_ctx)
            .expect("builder");

        // run() should NOT return MaxDepthExceeded because 1 <= 2.
        // It will return LLMCallFailed because no API key is available.
        let result = builder.run("test").await;
        match result {
            Ok(_) => { /* valid result — requires Qdrant + API key */ }
            Err(e) => {
                match e {
                    TaijiError::MaxDepthExceeded { .. } => {
                        panic!("depth <= max_depth should not trigger MaxDepthExceeded")
                    }
                    _ => {
                        // Expected: LLMCallFailed or SafetyViolation in test env
                        tracing::debug!("run() returned expected error: {e:?}");
                    }
                }
            }
        }
    }
}
