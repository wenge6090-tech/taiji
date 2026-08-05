//! CausalAgent builder (因果验证·阴) — "causal verification, the yin phase".
//!
//! The CausalAgent is the **third** agent in the TPN cycle.  It operates in
//! two modes:
//!
//! | Mode        | Role                 | Output                               | max_turns |
//! |-------------|----------------------|--------------------------------------|-----------|
//! | `verify`    | 因果验证器 (verifier) | [`VerificationReport`]               | 3         |
//! | `converge`  | 收敛判决器 (judge)    | [`ConvergenceDecision`]              | 3         |
//!
//! # Verify mode (CausalVerifyAgentBuilder)
//! Checks a task output (or intermediate tool result) against:
//! 1. **Constraint pre-check** ([`ConstraintEngine`]) — run **before** the LLM
//!    call.  Any hard constraint violation immediately short-circuits with
//!    `BackToMeta`.  Soft violations are injected into the LLM prompt.
//! 2. **LLM judgment** — the model reviews the output and issues a verdict
//!    (`Pass` / `BackToTpn` / `BackToMeta`).
//!
//! # Converge mode (CausalConvergeAgentBuilder)
//! Aggregates results from all subtasks of a recursive decomposition and
//! decides whether the overall task has converged, partially converged,
//! or diverged.
//!
//! # Constraints (AGENTS.md §2, §4)
//! - `max_turns = 3` for both modes.
//! - Verify system prompt starts with `"你是因果验证器"`.
//! - Converge system prompt starts with `"你是收敛判决器"`.

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::deepseek;

use crate::agents::tools::skills::SkillRegistry;
use crate::hooks::safety::SafetyHook;
use crate::infra::config::SafetyConfig;
use crate::infra::error::TaijiError;
use crate::infra::trace::save_json_atomic;
use crate::infra::provider::ProviderRegistry;
use crate::orchestration::constraint_engine::ConstraintEngine;
use crate::types::agent::{AgentMode, MetaContext};
use crate::types::execution::EngineContext;
use crate::types::task::DecomposeResult;
use crate::types::verification::{
    ConvergenceDecision, ConvergenceStatus, VerificationReport, VerificationRoute,
};

// ---------------------------------------------------------------------------
// Verify mode
// ---------------------------------------------------------------------------

/// Builder for the CausalAgent in **verify** mode (因果验证·阴).
///
/// Checks whether a task output (or tool result) satisfies L4 Truth
/// constraints and passes LLM-based verification.
///
/// The LLM registers read-only verification tools (`read` + `webfetch`) so it
/// can open each referenced file and cross-check external facts before
/// issuing the route verdict; the [`SafetyHook`] is **always** mounted
/// (defaults to `SafetyConfig::default()` when no shared singleton is
/// injected) — "带工具必有安全钩子" is a type-level guarantee (蓝图 V25 §8.5).
///
/// Created by [`AgentFactory::create_causal_verify_agent`].
pub struct CausalVerifyAgentBuilder {
    engine_ctx: EngineContext,
    model: String,
    provider: Arc<ProviderRegistry>,
    max_turns: u32,
    /// Process-wide SafetyHook (or a default-configured instance) — always
    /// mounted on the Rig agent.
    safety_hook: Arc<SafetyHook>,
}

impl CausalVerifyAgentBuilder {
    /// Create a new `CausalVerifyAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_causal_verify_agent`] —
    /// external callers should use the factory rather than constructing this
    /// directly.
    pub fn new(
        engine_ctx: EngineContext,
        provider: Arc<ProviderRegistry>,
        model: &str,
    ) -> Self {
        Self {
            engine_ctx,
            model: model.to_string(),
            provider,
            max_turns: 3,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
        }
    }

    /// Override the SafetyHook with the shared process-wide singleton.
    pub fn safety_hook(mut self, hook: Arc<SafetyHook>) -> Self {
        self.safety_hook = hook;
        self
    }

    /// Run verification: check the task output and tool results against L4
    /// Truth constraints and an LLM judgment.
    ///
    /// # Logic
    /// 1. **Constraint pre-check**: runs `ConstraintEngine::check_causal_output`
    ///    on the concatenated input.  Any hard violation short-circuits with
    ///    `BackToMeta` immediately (no LLM call).
    /// 2. **LLM verification**: constructs a Rig agent with the verify system
    ///    prompt (`VERIFY_SYSTEM_PROMPT`, starts with "你是因果验证器"), registers
    ///    read-only tools `read` + `webfetch` (逐文件核验 + 联网核实), mounts the
    ///    SafetyHook, calls the LLM, and parses the structured output into a
    ///    [`VerificationReport`].
    /// 3. **Return**: the final report.
    ///
    /// # Production wiring (pinned for Rig API verification)
    ///
    /// ```ignore
    /// use rig::providers::deepseek;
    /// use rig_core::agent::AgentBuilder;
    ///
    /// // Step 1: load constraints relevant to the task type tags
    /// let constraints = ConstraintEngine::load_truths(&[]);
    ///
    /// // Step 1a: run pre-check (hard violations short-circuit)
    /// let pre_check = ConstraintEngine::check_causal_output(
    ///     task_output,
    ///     tool_results,
    ///     &constraints,
    /// );
    ///
    /// if !pre_check.passed {
    ///     let has_hard = pre_check.violations.iter().any(|v| {
    ///         v.severity == ConstraintSeverity::Hard
    ///     });
    ///     if has_hard {
    ///         return Ok(VerificationReport {
    ///             route: VerificationRoute::BackToMeta,
    ///             confidence: 1.0,
    ///             summary: "Hard constraint violation".into(),
    ///             constraint_violations: pre_check.violations.iter().map(|v| v.reason.clone()).collect(),
    ///         });
    ///     }
    /// }
    ///
    /// // Step 2: inject soft violations into LLM prompt
    /// let soft_context: Vec<String> = pre_check.violations.iter()
    ///     .filter(|v| v.severity == ConstraintSeverity::Soft)
    ///     .map(|v| format!("[Soft] {}: {}", v.truth_name, v.reason))
    ///     .collect();
    ///
    /// let client = self.provider.client("deepseek")?;
    /// let agent = client
    ///     .agent(&self.model)
    ///     .preamble(VERIFY_SYSTEM_PROMPT)
    ///     .max_turns(3)
    ///     .build();
    ///
    /// let input = format!(
    ///     "Task output:\n{}\n\nTool results:\n{}\n\nSoft violations:\n{}",
    ///     task_output,
    ///     tool_results.join("\n---\n"),
    ///     soft_context.join("\n"),
    /// );
    ///
    /// let response = agent.prompt(&input).await
    ///     .map_err(|e| TaijiError::LLMCallFailed { ... })?;
    ///
    /// let report: VerificationReport = serde_json::from_str(response.as_ref())
    ///     .map_err(|e| TaijiError::StructuredOutputParseFailed { ... })?;
    ///
    /// Ok(report)
    /// ```
    ///
    /// # Current behaviour (production path)
    /// Runs the **LLM verification path**: constraint pre-check first (hard
    /// violations short-circuit to `BackToMeta`), then the LLM reviews the task
    /// output with `read` / `webfetch` tools for file-level and web fact
    /// verification and issues a structured [`VerificationReport`].  LLM
    /// failures surface as `TaijiError::LLMCallFailed`.
    pub async fn verify(
        &self,
        task_output: &str,
        tool_results: &[String],
        meta_ctx: &MetaContext,
        mode: AgentMode,
    ) -> Result<VerificationReport, TaijiError> {
        // ── Load constraints relevant to this task ──
        // In production, pass actual task_type_tags from the task spec.
        let constraints = ConstraintEngine::load_truths(&[]);

        // ── Step 1: Constraint pre-check ──
        let pre_check = ConstraintEngine::check_causal_output(task_output, tool_results, &constraints);

        if !pre_check.passed {
            let has_hard = pre_check
                .violations
                .iter()
                .any(|v| v.severity == crate::types::verification::ConstraintSeverity::Hard);

            if has_hard {
                tracing::warn!(
                    task_id = %self.engine_ctx.task_id,
                    violations = ?pre_check.violations,
                    "Hard constraint violation — returning BackToMeta"
                );
                return Ok(VerificationReport {
                    route: VerificationRoute::BackToMeta,
                    confidence: 1.0,
                    summary: format!(
                        "Hard constraint violation(s): {}",
                        pre_check
                            .violations
                            .iter()
                            .map(|v| v.reason.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    constraint_violations: pre_check
                        .violations
                        .iter()
                        .map(|v| v.reason.clone())
                        .collect(),
                });
            }

            // Soft violations only — we'll still run the LLM with them as
            // additional context.
            tracing::debug!(
                task_id = %self.engine_ctx.task_id,
                soft_violations = pre_check.violations.len(),
                "Soft constraint violation(s) — will pass to LLM"
            );
        }

        // ── Step 2: Select prompt — prefer MetaAgent-composed, fallback to mode template ──
        let system_prompt = match &meta_ctx.verify_system_prompt {
            Some(prompt) => prompt.as_str(),
            None => match mode {
                AgentMode::Orchestration => VERIFY_ORC_SYSTEM_PROMPT,
                AgentMode::Execution => VERIFY_EXEC_SYSTEM_PROMPT,
            },
        };

        // Build soft violation context for the LLM prompt
        let soft_context: Vec<String> = pre_check
            .violations
            .iter()
            .filter(|v| v.severity == crate::types::verification::ConstraintSeverity::Soft)
            .map(|v| format!("[Soft] {}: {}", v.truth_name, v.reason))
            .collect();

        // Call the LLM for verification
        let client: Arc<deepseek::Client> = self.provider.client("deepseek")?;

        // ── 收集工具（只读）：read + webfetch — 逐文件核验 deliverables、
        //    联网核实外部事实（V25 权限分工：收集工具三相共有）。
        //    带工具必有安全钩子（§8.5 硬约束，类型级保证）：无条件挂载 SafetyHook ──
        let skill_tools: Vec<Box<dyn rig::tool::ToolDyn>> = SkillRegistry::new()
            .tools()
            .iter()
            .filter(|t| matches!(t.name(), "read" | "webfetch"))
            .map(|t| Box::new(t.clone()) as Box<dyn rig::tool::ToolDyn>)
            .collect();
        let agent = client
            .agent(&self.model)
            .preamble(system_prompt)
            .max_tokens(1024u64)
            .default_max_turns(self.max_turns as usize)
            .hook(self.safety_hook.as_ref().clone())
            .tools(skill_tools)
            .build();

        let input = format!(
            "Task output:\n{task_output}\n\nTool results:\n{results}\n\nSoft violations:\n{soft}",
            task_output = task_output,
            results = tool_results.join("\n---\n"),
            soft = if soft_context.is_empty() {
                "None".to_string()
            } else {
                soft_context.join("\n")
            },
        );

        let response = agent.prompt(&input).await.map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("CausalVerifyAgent LLM call failed: {e}"),
            }
        })?;

        // Parse structured output into VerificationReport
        let report: VerificationReport =
            serde_json::from_str(response.as_ref()).map_err(|e| {
                TaijiError::StructuredOutputParseFailed {
                    context: format!(
                        "Failed to parse VerificationReport from LLM response: {e}. Raw: {response}"
                    ),
                }
            })?;

        tracing::info!(
            task_id = %self.engine_ctx.task_id,
            route = ?report.route,
            confidence = report.confidence,
            mode = ?mode,
            "CausalVerifyAgent — LLM verification completed"
        );

        // ── Persist verify state for crash recovery ──
        let verify_state = serde_json::json!({
            "report": &report,
            "round": self.engine_ctx.round,
            "cycle": self.engine_ctx.cycle,
        });
        let verify_path = self.engine_ctx.task_dir.join("verify_state.json");
        if let Err(e) = save_json_atomic(&verify_state, &verify_path) {
            tracing::warn!(
                path = %verify_path.display(),
                error = %e,
                "Failed to save verify_state"
            );
        }

        Ok(report)
    }
}

/// System prompt for the CausalAgent in **verify · Orchestration** mode.
///
/// Focuses on MECE completeness, dependency correctness, and decomposition
/// granularity. Route preference: BACK_TO_META for decomposition issues.
const VERIFY_ORC_SYSTEM_PROMPT: &str = r#"你是因果验证器 — 编排验证 (Causal Verifier · Orchestration).

You are verifying an **orchestration** task that decomposed a parent task into
subtasks and synthesized their results.

Your focus:
1. MECE completeness — did the decomposition cover all required dimensions?
2. Dependency correctness — are subtask dependencies properly ordered?
3. Granularity — were subtasks split at the right level (not too coarse, not too fine)?
4. Synthesis quality — does the integrated result make sense as a whole?

## File Verification
The task output may reference deliverable files by absolute path.  To verify
content quality, you MUST use the `read` tool (or equivalent) to open each
referenced file and inspect its contents.  Do NOT rely solely on the summary
text — read the actual files to confirm compliance.

Provide a structured verification report in JSON format:
{
  "route": "Pass" | "BackToTpn" | "BackToMeta",
  "confidence": 0.0..1.0,
  "summary": "Brief justification for the decision",
  "constraint_violations": ["description of each violation"]
}

Routing guidance:
- "Pass":        Good decomposition + synthesis. Proceed.
- "BackToTpn":   Minor issues — retry probability fitting with same strategy.
- "BackToMeta":  Fundamental decomposition problem — need new reasoning paths.
  Prefer BACK_TO_META when the decomposition strategy itself is flawed.
"#;

/// System prompt for the CausalAgent in **verify · Execution** mode.
///
/// Focuses on requirement satisfaction, artifact quality, and constraint
/// adherence. Route preference: BACK_TO_TPN for execution quality issues.
const VERIFY_EXEC_SYSTEM_PROMPT: &str = r#"你是因果验证器 — 执行验证 (Causal Verifier · Execution).

You are verifying an **execution** task that directly produced output using
available tools.

Your focus:
1. Requirement satisfaction — does the output meet the task description?
2. Artifact quality — are deliverables well-formed and usable?
3. Constraint adherence — are all L4 Truth constraints satisfied?
4. Completeness — was the task fully addressed?

## File Verification
The task output may reference deliverable files by absolute path.  You MUST
use the `read` tool (or equivalent) to open each referenced file and inspect
its contents.  Do NOT rely solely on the summary text — read the actual files
to confirm compliance.

Provide a structured verification report in JSON format:
{
  "route": "Pass" | "BackToTpn" | "BackToMeta",
  "confidence": 0.0..1.0,
  "summary": "Brief justification for the decision",
  "constraint_violations": ["description of each violation"]
}

Routing guidance:
- "Pass":        Output satisfies requirements. Proceed.
- "BackToTpn":   Minor quality issues — retry execution with improvements.
  Prefer BACK_TO_TPN when execution quality needs improvement.
- "BackToMeta":  Fundamental issues — task specification or approach is wrong.
  Only use BACK_TO_META when the execution strategy itself is invalid.
"#;

// ---------------------------------------------------------------------------
// Converge mode
// ---------------------------------------------------------------------------

/// Builder for the CausalAgent in **converge** mode (收敛判决).
///
/// Aggregates results from all subtasks of a recursive decomposition and
/// decides whether the overall task has converged.
///
/// Created by [`AgentFactory::create_causal_converge_agent`].
///
/// The LLM registers read-only verification tools (`read` + `webfetch`) so it
/// can open each referenced deliverable and cross-check external facts before
/// issuing the convergence verdict; the [`SafetyHook`] is **always** mounted
/// (defaults to `SafetyConfig::default()` when no shared singleton is
/// injected) — "带工具必有安全钩子" is a type-level guarantee (蓝图 V25 §8.5).
pub struct CausalConvergeAgentBuilder {
    engine_ctx: EngineContext,
    model: String,
    provider: Arc<ProviderRegistry>,
    max_turns: u32,
    /// Process-wide SafetyHook (or a default-configured instance) — always
    /// mounted on the Rig agent.
    safety_hook: Arc<SafetyHook>,
}

impl CausalConvergeAgentBuilder {
    /// Create a new `CausalConvergeAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_causal_converge_agent`] —
    /// external callers should use the factory rather than constructing this
    /// directly.
    pub fn new(
        engine_ctx: EngineContext,
        provider: Arc<ProviderRegistry>,
        model: &str,
    ) -> Self {
        Self {
            engine_ctx,
            model: model.to_string(),
            provider,
            max_turns: 3,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
        }
    }

    /// Override the SafetyHook with the shared process-wide singleton.
    pub fn safety_hook(mut self, hook: Arc<SafetyHook>) -> Self {
        self.safety_hook = hook;
        self
    }

    /// Run convergence: aggregate subtask results and decide convergence.
    ///
    /// # Logic
    /// 1. Empty subtask results short-circuit to `Converged` (trivially).
    /// 2. Otherwise the **LLM convergence judgment** runs: a Rig agent with the
    ///    converge system prompt (`CONVERGE_SYSTEM_PROMPT`, starts with
    ///    "你是收敛判决器") registers read-only tools `read` + `webfetch`, mounts
    ///    the SafetyHook, reviews the aggregated subtask results and issues a
    ///    structured [`ConvergenceDecision`] (Converged / Partial / Diverged).
    /// 3. The decision is persisted to `converge_state.json` (crash recovery)
    ///    and returned.
    ///
    /// # Production wiring (pinned for Rig API verification)
    ///
    /// ```ignore
    /// use rig::providers::deepseek;
    /// use rig_core::agent::AgentBuilder;
    ///
    /// let client = self.provider.client("deepseek")?;
    /// let agent = client
    ///     .agent(&self.model)
    ///     .preamble(CONVERGE_SYSTEM_PROMPT)
    ///     .max_turns(3)
    ///     .build();
    ///
    /// let input = serde_json::to_string_pretty(subtask_results)?;
    /// let response = agent.prompt(&input).await
    ///     .map_err(|e| TaijiError::LLMCallFailed { ... })?;
    ///
    /// let decision: ConvergenceDecision = serde_json::from_str(response.as_ref())
    ///     .map_err(|e| TaijiError::StructuredOutputParseFailed { ... })?;
    ///
    /// Ok(decision)
    /// ```
    ///
    /// # Current behaviour (production path)
    /// Runs the **LLM convergence judgment**: the LLM reviews the aggregated
    /// subtask results with `read` / `webfetch` tools for file-level and web
    /// fact verification, then issues a structured [`ConvergenceDecision`].
    /// Empty results short-circuit to `Converged`; LLM failures surface as
    /// `TaijiError::LLMCallFailed`.
    pub async fn converge(
        &self,
        subtask_results: &[DecomposeResult],
        meta_ctx: &MetaContext,
        mode: AgentMode,
    ) -> Result<ConvergenceDecision, TaijiError> {
        let total = subtask_results.len();

        // ── Empty results short-circuit ──
        if total == 0 {
            return Ok(ConvergenceDecision {
                status: ConvergenceStatus::Converged,
                task_summary: format!("Task {} has no subtasks — trivially converged", self.engine_ctx.task_id),
            });
        }

        // ── Select prompt — prefer MetaAgent-composed, fallback to mode template ──
        let system_prompt = match &meta_ctx.converge_system_prompt {
            Some(prompt) => prompt.as_str(),
            None => match mode {
                AgentMode::Orchestration => CONVERGE_ORC_SYSTEM_PROMPT,
                AgentMode::Execution => CONVERGE_EXEC_SYSTEM_PROMPT,
            },
        };

        // ── Production path: LLM convergence judgment ──
        let client: Arc<deepseek::Client> = self.provider.client("deepseek")?;

        // ── 收集工具（只读）：read + webfetch — 逐文件核验 deliverables、
        //    联网核实外部事实（V25 权限分工：收集工具三相共有）。
        //    带工具必有安全钩子（§8.5 硬约束，类型级保证）：无条件挂载 SafetyHook ──
        let skill_tools: Vec<Box<dyn rig::tool::ToolDyn>> = SkillRegistry::new()
            .tools()
            .iter()
            .filter(|t| matches!(t.name(), "read" | "webfetch"))
            .map(|t| Box::new(t.clone()) as Box<dyn rig::tool::ToolDyn>)
            .collect();
        let agent = client
            .agent(&self.model)
            .preamble(system_prompt)
            .max_tokens(1024u64)
            .default_max_turns(self.max_turns as usize)
            .hook(self.safety_hook.as_ref().clone())
            .tools(skill_tools)
            .build();

        let input = serde_json::to_string_pretty(subtask_results).map_err(|e| {
            TaijiError::Serde(e)
        })?;

        let response = agent.prompt(&input).await.map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("CausalConvergeAgent LLM call failed: {e}"),
            }
        })?;

        let decision: ConvergenceDecision =
            serde_json::from_str(response.as_ref()).map_err(|e| {
                TaijiError::StructuredOutputParseFailed {
                    context: format!(
                        "Failed to parse ConvergenceDecision from LLM response: {e}. Raw: {response}"
                    ),
                }
            })?;

        tracing::info!(
            task_id = %self.engine_ctx.task_id,
            subtasks = total,
            status = ?decision.status,
            mode = ?mode,
            "CausalConvergeAgent — LLM convergence judgment completed"
        );

        // ── Persist converge state for crash recovery ──
        let converge_state = serde_json::json!({
            "decision": decision,
            "round": self.engine_ctx.round,
            "cycle": self.engine_ctx.cycle,
        });
        let converge_path = self.engine_ctx.task_dir.join("converge_state.json");
        if let Err(e) = save_json_atomic(&converge_state, &converge_path) {
            tracing::warn!(
                path = %converge_path.display(),
                error = %e,
                "Failed to save converge_state"
            );
        }

        Ok(decision)
    }
}

/// System prompt for the CausalAgent in **converge · Orchestration** mode.
///
/// Aggregates multi-subtask results — focus on coverage and consistency.
const CONVERGE_ORC_SYSTEM_PROMPT: &str = r#"你是收敛判决器 — 编排收敛 (Convergence Judge · Orchestration).

You are aggregating results from multiple subtasks of a decomposed task.
The agent operated in **Orchestration** mode — it split work across subtasks
and is now integrating their outputs.

Your focus:
1. Coverage — do the subtask results collectively cover the full task scope?
2. Consistency — are the results compatible (no contradictions across subtasks)?
3. Integration — can the partial results be combined into a coherent whole?

## Deliverable Verification
Each subtask result includes a `deliverables` field containing absolute paths
to produced files.  You MUST use the `read` tool to open each file and verify:
- Cross-subtask consistency — do files from different subtasks agree?
- Completeness — are all required artifacts present?
- Quality — do the files meet the required standards?

Produce a convergence decision in JSON format:
{
  "status": "Converged" | "Partial" | "Diverged",
  "task_summary": "Explanation of the decision"
}

- "Converged": All dimensions covered, results consistent.
- "Partial": Some gaps or inconsistencies remain, but partial progress made.
- "Diverged": Fundamental incoherence — decomposition strategy needs revision.
"#;

/// System prompt for the CausalAgent in **converge · Execution** mode.
///
/// Judging whether a single execution task met its goal.
const CONVERGE_EXEC_SYSTEM_PROMPT: &str = r#"你是收敛判决器 — 执行收敛 (Convergence Judge · Execution).

You are evaluating whether a single execution task met its objective.
The agent operated in **Execution** mode — it directly produced output.

Your focus:
1. Goal achievement — was the single task objective met?
2. Self-contained quality — is the output usable on its own?
3. Finality — does the output represent a complete answer?

## Deliverable Verification
The subtask result includes a `deliverables` field containing absolute paths
to produced files.  You MUST use the `read` tool to open each file and verify
that the contents match the claimed results.

Produce a convergence decision in JSON format:
{
  "status": "Converged" | "Partial" | "Diverged",
  "task_summary": "Explanation of the decision"
}

- "Converged": Goal met, output is complete.
- "Partial": Partial progress, some aspects still outstanding.
- "Diverged": Execution failed to meet the objective.
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config::{LlmConfig, SafetyConfig, TaijiConfig};
    use crate::types::task::DecomposeResult;

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

    fn make_engine_ctx(task_id: &str) -> EngineContext {
        EngineContext {
            task_id: task_id.into(),
            depth: 0,
            task_dir: std::path::PathBuf::from(format!("./test_data/tasks/{task_id}")),
            cycle: 1,
            round: 0,
            context_dir: None,
        }
    }

    // ── CausalVerifyAgentBuilder tests ──────────────────────────────────

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_verify_returns_default_pass() {
        let builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let report = builder.verify("CausalAgent executed and verified the task output against all L4 Truth constraints.", &[], &MetaContext::empty(), AgentMode::Orchestration).await.expect("verify");
        assert_eq!(report.route, VerificationRoute::Pass);
    }

    #[tokio::test]
    async fn test_verify_empty_summary_triggers_back_to_meta() {
        // Empty summary should trigger the constraint pre-check for
        // truth:no-fabrication (hard).
        let builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        // An empty task output violates truth:no-fabrication (hard),
        // which should return BackToMeta.
        let report = builder.verify("", &[], &MetaContext::empty(), AgentMode::Execution).await.expect("verify");
        assert_eq!(report.route, VerificationRoute::BackToMeta);
        assert!(!report.constraint_violations.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_verify_with_soft_violations_passes() {
        // A short summary (< 10 chars) triggers the soft auditability
        // constraint, but since it's soft the verify should still pass.
        let builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let report = builder.verify("Adequate summary for audit.", &[], &MetaContext::empty(), AgentMode::Execution).await.expect("verify");
        assert_eq!(report.route, VerificationRoute::Pass);
    }

    // ── CausalConvergeAgentBuilder tests ────────────────────────────────

    #[tokio::test]
    async fn test_converge_empty_results_converged() {
        let builder = CausalConvergeAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let decision = builder.converge(&[], &MetaContext::empty(), AgentMode::Orchestration).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Converged);
    }

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_converge_all_ok_converged() {
        let builder = CausalConvergeAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let results = vec![
            DecomposeResult {
                task_id: "sub-1".into(),
                summary: "Done".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
                deliverables: vec![],
                rounds: 1,
                tools_used: vec!["write".into()],
                child_results: vec![],
            },
            DecomposeResult {
                task_id: "sub-2".into(),
                summary: "Done too".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
                deliverables: vec![],
                rounds: 1,
                tools_used: vec![],
                child_results: vec![],
            },
        ];

        let decision = builder.converge(&results, &MetaContext::empty(), AgentMode::Execution).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Converged);
    }

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_converge_some_partial() {
        let builder = CausalConvergeAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let results = vec![
            DecomposeResult {
                task_id: "sub-1".into(),
                summary: "Done".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
                deliverables: vec![],
                rounds: 1,
                tools_used: vec![],
                child_results: vec![],
            },
            DecomposeResult {
                task_id: "sub-2".into(),
                summary: "Partial".into(),
                status: ConvergenceStatus::Partial,
                subtask_count: 0,
                deliverables: vec![],
                rounds: 3,
                tools_used: vec![],
                child_results: vec![],
            },
        ];

        let decision = builder.converge(&results, &MetaContext::empty(), AgentMode::Orchestration).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Partial);
    }

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_converge_all_diverged() {
        let builder = CausalConvergeAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let results = vec![
            DecomposeResult {
                task_id: "sub-1".into(),
                summary: "Failed".into(),
                status: ConvergenceStatus::Diverged,
                subtask_count: 0,
                deliverables: vec![],
                rounds: 5,
                tools_used: vec![],
                child_results: vec![],
            },
            DecomposeResult {
                task_id: "sub-2".into(),
                summary: "Also failed".into(),
                status: ConvergenceStatus::Diverged,
                subtask_count: 0,
                deliverables: vec![],
                rounds: 4,
                tools_used: vec![],
                child_results: vec![],
            },
        ];

        let decision = builder.converge(&results, &MetaContext::empty(), AgentMode::Execution).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Diverged);
    }

    // ── System prompt tests ─────────────────────────────────────────────

    #[test]
    fn test_verify_orc_system_prompt_starts_with_chinese() {
        assert!(VERIFY_ORC_SYSTEM_PROMPT.starts_with("你是因果验证器"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("Pass"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("BackToTpn"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("BackToMeta"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("编排"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("Orchestration"));
    }

    #[test]
    fn test_verify_exec_system_prompt_starts_with_chinese() {
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.starts_with("你是因果验证器"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("Pass"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("BackToTpn"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("BackToMeta"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("Execution"));
    }

    #[test]
    fn test_converge_orc_system_prompt_starts_with_chinese() {
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Converged"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Partial"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Diverged"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Orchestration"));
    }

    #[test]
    fn test_converge_exec_system_prompt_starts_with_chinese() {
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Converged"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Partial"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Diverged"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Execution"));
    }

    #[test]
    fn test_causal_builders_safety_hook_setters() {
        // 蓝图 V25 §8.5：Causal 相位带收集工具（read+webfetch）→ 必有安全钩子
        // （类型级保证，字段非 Option）；注入进程级单例后指针一致。
        let hook = Arc::new(SafetyHook::new(&SafetyConfig {
            enabled: false,
            trusted_mcp_servers: vec![],
        }));

        let verify_builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        )
        .safety_hook(hook.clone());
        assert!(Arc::ptr_eq(&verify_builder.safety_hook, &hook));

        let converge_builder = CausalConvergeAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        )
        .safety_hook(hook.clone());
        assert!(Arc::ptr_eq(&converge_builder.safety_hook, &hook));
    }
}
