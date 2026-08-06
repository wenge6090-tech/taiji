//! FittingAgent builder (概率拟合·阳) — "probability fitting, the yang phase".
//!
//! The FittingAgent is the **second** agent in the TPN cycle.  It receives the
//! [`MetaContext`] produced by the MetaAgent (权重更新·元) as a reasoning bias
//! and executes along the extracted reasoning paths, either solving the task
//! directly or recursively decomposing it into subtasks.
//!
//! # Toolset
//! The FittingAgent's transient Rig agent is wired with:
//! - **`recursive_decompose`**: spawns child FittingAgents for subtasks.
//! - **`causal_verify`**: invokes CausalAgent.verify() on intermediate outputs.
//! - **5 L1 Skills**: `read`, `write`, `bash`, `search`, `webfetch`.
//! - **`SafetyHook`** and **`TraceHook`**: registered as Rig `PromptHook`s.
//!
//! L1 Skills are real built-in implementations.  A frontend agent may also
//! inject additional context via MCP ExternalContext.
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
use rig::completion::{Chat, Message};
use rig::providers::deepseek;
use rig::tool::ToolDyn;
use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::agents::tools::causal_verify::CausalVerifyTool;
use crate::agents::tools::recursive_decompose::RecursiveDecomposeTool;
use crate::agents::tools::skills::SkillRegistry;
use crate::hooks::trace::TraceHook;
use crate::infra::error::TaijiError;
use crate::infra::trace::save_json_atomic;
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;
use crate::types::task::TPNResult;

/// Builder for the FittingAgent (概率拟合·阳).
///
/// Created by [`AgentFactory::create_fitting_agent`].  Encapsulates the
/// reasoning bias ([`MetaContext`]), engine context (depth, cycle, round),
/// cancellation token, and a handle to the factory for spawning sub-agents
/// during recursion.
///
/// V26 起无模式分化（异层同构）：任意深度注册全部执行工具
/// （5 L1 Skills + recursive_decompose + causal_verify），单一 system prompt。
pub struct FittingAgentBuilder {
    depth: u32,
    meta_ctx: MetaContext,
    engine_ctx: EngineContext,
    factory: Arc<AgentFactory>,
    model: String,
    /// Cancellation token propagated from the runner.
    /// Used by [`RecursiveDecomposeTool`] to signal cancellation to subtasks.
    cancel: CancellationToken,
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
        cancel: CancellationToken,
    ) -> Self {
        Self {
            depth,
            meta_ctx,
            engine_ctx,
            factory,
            model: model.to_string(),
            cancel,
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
    pub async fn run(
        &self,
        task_description: &str,
        chat_history: Option<Vec<Message>>,
    ) -> Result<TPNResult, TaijiError> {
        // ── Guard against runaway recursion ──
        let max_depth = self.factory.config.runtime.max_depth;
        if self.depth > max_depth {
            return Err(TaijiError::MaxDepthExceeded { max: max_depth });
        }

        // ── Build system prompt from MetaContext (V26: 单模板，无模式分支) ──
        let system_prompt = build_system_prompt(
            &self.meta_ctx,
            &self.engine_ctx.task_dir,
            self.engine_ctx.context_dir.as_deref(),
        );

        // ── Obtain LLM client ──
        let client: Arc<deepseek::Client> = self.factory.providers.client("deepseek")?;

        // ── Build Rig agent with preamble, max_turns, max_tokens, temperature ──
        // Check if agent-specific max_turns is configured; fallback to 30 (V26:
        // Fitting max_turns 统一 30，不再复用 max_rounds)。
        let max_turns = self.factory.config.llm.agent_overrides
            .get("fitting")
            .and_then(|o| o.max_turns)
            .unwrap_or(30) as usize;
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

        // ── Register built-in composite tools ──
        // V26: 异层同构 — 任意深度注册全部工具（recursive_decompose +
        // causal_verify + 5 L1 Skills），无模式分支。permit 语义 = 并行分解
        // 节点上限：decompose 工具入口自行 acquire，spawn 闭包不持 permit，
        // 无嵌套持有 → 无死锁。
        let recursive_decompose = RecursiveDecomposeTool::new(
            self.factory.clone(),
            self.engine_ctx.clone(),
            self.depth,
            self.cancel.clone(),
            self.meta_ctx.clone(),
        );
        let causal_verify = CausalVerifyTool::new(
            self.factory.clone(),
            self.engine_ctx.clone(),
            self.meta_ctx.clone(),
        );

        // ── Register L1 Skills ──
        let skill_registry = SkillRegistry::new();
        let skill_tools: Vec<Box<dyn ToolDyn>> = skill_registry
            .tools()
            .iter()
            .map(|t| Box::new(t.clone()) as Box<dyn ToolDyn>)
            .collect();

        let agent = agent_builder
            .tool(recursive_decompose)
            .tool(causal_verify)
            .tools(skill_tools)
            .build();

        // ── Execute the prompt — always use Chat::chat for history persistence ──
        let history_path = self.engine_ctx.task_dir.join("chat_history.json");
        let (response, _history) = {
            let mut history: Vec<Message> = match chat_history {
                Some(h) => h,
                None => match crate::infra::trace::load_json_optional::<Vec<Message>>(&history_path) {
                    Ok(Some(h)) => {
                        tracing::debug!("Loaded existing chat_history from checkpoint");
                        h
                    }
                    _ => Vec::new(),
                },
            };

            // Chat::chat appends all new messages (user prompt + assistant + tool calls)
            // to `history`, so BACK_TO_TPN naturally carries forward context.
            let result = agent
                .chat(Message::user(task_description), &mut history)
                .await
                .map_err(|e| TaijiError::LLMCallFailed {
                    context: format!("FittingAgent LLM call failed: {e}"),
                });

            // Save chat_history to disk even on error (partial history is better than none).
            if let Err(e) = save_json_atomic(&history, &history_path) {
                tracing::warn!(
                    path = %history_path.display(),
                    error = %e,
                    "Failed to save chat_history"
                );
            }

            (result, history)
        };
        let response = response?;

        // ── Extract tool call info from response (basic parsing) ──
        // The LLM response may mention which tools were used; we capture
        // available tool names from the registered set as a best-effort summary.
        // V26: 全工具全层级注册，无模式分化。
        let mut registered_tool_names: Vec<String> =
            vec!["causal_verify".to_string(), "recursive_decompose".to_string()];
        registered_tool_names.extend(skill_registry.get_tool_names());

        // Check which tools appear in the response text
        let tools_used: Vec<String> = registered_tool_names
            .into_iter()
            .filter(|name| response.contains(name))
            .collect();

        // Deliverables directory exists per BCP — list files if any
        let deliverables_dir = self.engine_ctx.task_dir.join("deliverables");
        let deliverables: Vec<String> = if deliverables_dir.exists() {
            std::fs::read_dir(&deliverables_dir)
                .ok()
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.path().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(TPNResult {
            task_id: self.engine_ctx.task_id.clone(),
            content: response,
            tools_used,
            deliverables,
            depth: self.depth,
            rounds: self.engine_ctx.round + 1,
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
/// V26 起为**单一模板**（异层同构）：不区分 Orchestration / Execution，融合
/// 「拆解优先 + 执行优先」引导——可用 `recursive_decompose` 拆解复杂任务，也
/// 可直接用 L1 工具产出，由 Agent 自己判断（植物生长原则）。
fn build_system_prompt(meta_ctx: &MetaContext, task_dir: &std::path::Path, context_dir: Option<&std::path::Path>) -> String {
    // Prefer MetaAgent-composed prompt if available.
    if let Some(ref composed) = meta_ctx.fitting_system_prompt {
        return composed.clone();
    }

    // Fallback: single fused template (V26, no mode branch).
    let mut prompt = String::with_capacity(1024);

    prompt.push_str("你是概率拟合专家 (Probability Fitting Agent).\n\n");

    // Task description
    if !meta_ctx.yang_prompt.task_description.is_empty() {
        prompt.push_str("## Task\n");
        prompt.push_str(&meta_ctx.yang_prompt.task_description);
        prompt.push_str("\n\n");
    }

    // Constraint summaries
    if !meta_ctx.yang_prompt.constraint_summaries.is_empty() {
        prompt.push_str("## Constraints\n");
        for summary in &meta_ctx.yang_prompt.constraint_summaries {
            prompt.push_str(&format!("- {}\n", summary));
        }
        prompt.push('\n');
    }

    // Output directory (absolute path)
    let deliverables_dir = task_dir.join("deliverables");
    prompt.push_str("## 产出目录\n");
    prompt.push_str(&format!(
        "所有产物文件请使用**绝对路径**写入: `{}`\n",
        deliverables_dir.display()
    ));
    prompt.push_str(&format!(
        "产出文件示例: `{}/report.md`（而非相对路径 `report.md`）。\n",
        deliverables_dir.display()
    ));
    prompt.push_str("递归子任务也会有自己的同名目录，结构一致。\n\n");

    // Parent deliverables (injected from recursive parent — read-only reference)
    if !meta_ctx.yang_prompt.parent_deliverables.is_empty() {
        prompt.push_str("## 父层产物参照 (Parent Deliverables - Read Only)\n");
        prompt.push_str("以下文件由当前任务的父层产出，你可读取其内容但不可修改：\n");
        for (i, path) in meta_ctx.yang_prompt.parent_deliverables.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, path));
        }
        prompt.push('\n');
    }

    // External context (from frontend agent via MCP)
    if let Some(ctx_dir) = context_dir {
        let files_dir = ctx_dir.join("files");
        if files_dir.exists() {
            prompt.push_str("## External Context (from Frontend Agent)\n");
            prompt.push_str("以下文件由前端 agent 已读取并传递给此任务：\n");
            if let Ok(entries) = std::fs::read_dir(&files_dir) {
                for entry in entries.flatten() {
                    let index = entry.file_name().to_string_lossy().to_string();
                    prompt.push_str(&format!("- `{}/{}`\n", files_dir.display(), index));
                }
            }
            prompt.push_str("\n使用 `read` 工具检查这些文件的内容——它们包含了你推理所需的前端上下文。\n\n");
        }
    }

    // Available tools
    if !meta_ctx.matched_skills.is_empty() {
        prompt.push_str("## Available Tools\n");
        for skill in &meta_ctx.matched_skills {
            prompt.push_str(&format!(
                "- `{}` ({}): {}\n",
                skill.tool_name, skill.name, skill.id
            ));
        }
        prompt.push('\n');
    }

    // Single fused instruction set (V26 异层同构: 拆解优先 + 执行优先融合)
    prompt.push_str(
        "## Instructions\n\
         你在任务树的任意深度运行，与根任务完全同构。你的工具面包括\n\
         `recursive_decompose`（拆解）、`causal_verify`（自检）与全部 L1 工具\n\
         （read/write/bash/search/webfetch）。\n\n\
         ### 执行策略 (由你判断)\n\
         1. 🌱 拆解优先 (Decompose-First): 复杂任务先用 `recursive_decompose`\n\
            MECE 拆解为子任务，子任务完成后汇聚其结果进行综合。\n\n\
         2. 🎯 执行优先 (Execution-First): 原子任务直接用 L1 工具完成——\n\
            读文件、写代码、跑命令，把工作做完。\n\n\
         3. ⚖️ 何时拆解、何时直接执行，由你判断（植物生长原则）：\n\
            只对真正复杂、跨多独立维度的任务拆解；能直接完成就绝不拆。\n\
            过度拆解浪费轮次，宁可先直接执行再修补。\n\n\
         4. ✅ 用 `causal_verify` 检查中间结果与约束，自检通过后再收尾。\n\
            若调用了 `recursive_decompose`，全部子任务完成后提供综合摘要。\n\n\
         ### 产物路径 (Deliverable Paths)\n\
         Write all output files to the deliverables directory using their\n\
         **absolute paths**.  After execution, your deliverables will be\n\
         automatically collected from the directory.  If you used\n\
         `recursive_decompose`, your subtasks' deliverables will be available\n\
         in `parent_deliverables` for the synthesis phase.\n\n\
         Follow all constraints strictly — hard violations cause immediate failure.\n"
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
    use crate::infra::knowledge::LiluoClient;
    use crate::orchestration::constraint_engine::ConstraintEngine;
    use crate::orchestration::trigger_engine::SkillTriggerEngine;
    use crate::orchestration::worker_pool::WorkerPool;
    use crate::types::agent::{SkillRef, YangPrompt};
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
            knowledge: crate::infra::config::KnowledgeConfig::default(),
            safety: SafetyConfig::default(),
            mcp_servers: vec![],
        }
    }

    /// Build an Arc<AgentFactory> for testing.
    async fn build_factory_arc(config: TaijiConfig) -> (Arc<AgentFactory>, std::path::PathBuf) {
        let tmp_dir = std::env::temp_dir().join(format!(
            "taiji_fitting_test_{}",
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
        let providers = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        let factory = Arc::new(AgentFactory::new(
            liluo,
            providers,
            config,
            Arc::new(SafetyHook::new(&SafetyConfig::default())),
            Arc::new(WorkerPool::new(4)),
            Arc::new(ConstraintEngine::new()),
            Arc::new(SkillTriggerEngine::new()),
        ));
        (factory, tmp_dir)
    }

    fn sample_meta_context() -> MetaContext {
        MetaContext {
            constraints: vec![TruthConstraint::hard(
                "truth:no-fabrication",
                "不编造事实",
                "Don't fabricate facts",
            )],
            matched_skills: vec![SkillRef {
                id: "read".into(),
                name: "文件读取".into(),
                tool_name: "read".into(),
                match_weight: 0.9,
            }],
            yang_prompt: YangPrompt {
                task_description: "Refactor the logging module.".into(),
                constraint_summaries: vec![
                    "Hard: Do not fabricate facts".into(),
                ],
                parent_deliverables: vec![],
            },
            fitting_system_prompt: None,
            verify_system_prompt: None,
            converge_system_prompt: None,
        }
    }

    #[test]
    fn test_build_system_prompt_contains_role_header() {
        let ctx = sample_meta_context();
        let prompt = build_system_prompt(
            &ctx,
            &std::path::PathBuf::from("./test_data/tasks/prompt-test"),
            None,
        );
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("Refactor the logging module"));
        assert!(prompt.contains("recursive_decompose"));
        assert!(prompt.contains("产出目录"));
        // V26 单模板：融合拆解优先 + 执行优先
        assert!(prompt.contains("拆解优先"));
        assert!(prompt.contains("执行优先"));
    }

    #[test]
    fn test_build_system_prompt_empty_context() {
        let ctx = MetaContext::empty();
        let prompt = build_system_prompt(&ctx, &std::path::PathBuf::from("./test_data/tasks/empty-test"), None);
        // Should still have the role header and instructions.
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("Instructions"));
        assert!(prompt.contains("产出目录"));
    }

    #[tokio::test]
    async fn test_fitting_agent_builder_construction() {
        let config = make_config();
        let (factory, tmp_dir) = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();
        let engine_ctx = EngineContext {
            task_id: "test-task-1".into(),
            depth: 0,
            task_dir: std::path::PathBuf::from("./test_data/tasks/test-task-1"),
            cycle: 1,
            round: 0,
            context_dir: None,
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        let builder = factory.create_fitting_agent(0, &meta_ctx, &engine_ctx, cancel);
        assert!(builder.is_ok());
        let builder = builder.unwrap();
        assert_eq!(builder.engine_ctx().task_id, "test-task-1");

        // Cleanup temp directory
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_fitting_agent_run_integration() {
        // Integration test: runs the full Rig agent pipeline.
        // Requires a valid DEEPSEEK_API_KEY.
        let config = make_config();
        let (factory, tmp_dir) = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();
        let engine_ctx = EngineContext {
            task_id: "test-task-2".into(),
            depth: 1,
            task_dir: std::path::PathBuf::from("./test_data/tasks/test-task-2"),
            cycle: 1,
            round: 0,
            context_dir: None,
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        let builder = factory
            .create_fitting_agent(1, &meta_ctx, &engine_ctx, cancel)
            .expect("builder");

        let result = builder.run("Write a test for the logging module", None).await;
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

        // Cleanup temp directory
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_fitting_agent_depth_check() {
        let config = make_config();
        let (factory, tmp_dir) = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();

        // Depth 1, but max_depth defaults to 2 — should pass the guard.
        let engine_ctx = EngineContext {
            task_id: "depth-test".into(),
            depth: 1,
            task_dir: std::path::PathBuf::from("./test_data/tasks/depth-test"),
            cycle: 1,
            round: 0,
            context_dir: None,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let builder = factory
            .create_fitting_agent(1, &meta_ctx, &engine_ctx, cancel)
            .expect("builder");

        // run() should NOT return MaxDepthExceeded because 1 <= 2.
        // It will return LLMCallFailed because no API key is available.
        let result = builder.run("test", None).await;
        match result {
            Ok(_) => { /* valid result — requires API key */ }
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

        // Cleanup temp directory
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }
}
