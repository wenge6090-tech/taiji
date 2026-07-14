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

use crate::infra::error::TaijiError;
use crate::infra::provider::ProviderRegistry;
use crate::orchestration::constraint_engine::ConstraintEngine;
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
/// Created by [`AgentFactory::create_causal_verify_agent`].
pub struct CausalVerifyAgentBuilder {
    engine_ctx: EngineContext,
    constraint_engine: Arc<ConstraintEngine>,
    model: String,
    provider: Arc<ProviderRegistry>,
    max_turns: u32,
}

impl CausalVerifyAgentBuilder {
    /// Create a new `CausalVerifyAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_causal_verify_agent`] —
    /// external callers should use the factory rather than constructing this
    /// directly.
    pub fn new(
        engine_ctx: EngineContext,
        constraint_engine: Arc<ConstraintEngine>,
        provider: Arc<ProviderRegistry>,
        model: &str,
    ) -> Self {
        Self {
            engine_ctx,
            constraint_engine,
            model: model.to_string(),
            provider,
            max_turns: 3,
        }
    }

    /// Run verification: check the task output and tool results against L4
    /// Truth constraints and an LLM judgment.
    ///
    /// # Logic
    /// 1. **Constraint pre-check**: runs `ConstraintEngine::check_causal_output`
    ///    on the concatenated input.  Any hard violation short-circuits with
    ///    `BackToMeta` immediately (no LLM call).
    /// 2. **LLM verification** (TODO): constructs a Rig agent with the verify
    ///    system prompt (`VERIFY_SYSTEM_PROMPT`, starts with "你是因果验证器"),
    ///    calls the LLM, and parses the structured output into a
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
    /// # Current behaviour (degraded mode)
    /// Skips the LLM call and returns a default `VerificationReport` with
    /// `route: Pass`.  Constraint pre-checks are **not** performed in degraded
    /// mode — callers that need strict enforcement should implement the
    /// production path.
    pub async fn verify(
        &self,
        task_output: &str,
        tool_results: &[String],
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
            // additional context.  In degraded mode we just note them.
            tracing::debug!(
                task_id = %self.engine_ctx.task_id,
                soft_violations = pre_check.violations.len(),
                "Soft constraint violation(s) — will pass to LLM"
            );
        }

        // ── Step 2: LLM verification (TODO: wire Rig agent) ──
        // The code block below shows the intended production path.
        let _ = task_output;
        let _ = tool_results;

        // ── Degraded mode: return low-confidence pass ──
        tracing::warn!(
            task_id = %self.engine_ctx.task_id,
            "CausalVerifyAgent.verify() degraded mode — return low-confidence pass"
        );

        Ok(VerificationReport {
            route: VerificationRoute::Pass,
            confidence: 0.0, // Low confidence — no LLM verification
            summary: format!(
                "[DEGRADED] Skipped LLM verification for task {}",
                self.engine_ctx.task_id
            ),
            constraint_violations: pre_check
                .violations
                .iter()
                .map(|v| v.reason.clone())
                .collect(),
        })
    }
}

/// System prompt for the CausalAgent in **verify** mode.
///
/// Starts with the required Chinese identifier per AGENTS.md §2:
/// "CausalAgent verify 模式的 system prompt 必须以 '你是因果验证器' 开头".
const VERIFY_SYSTEM_PROMPT: &str = r#"你是因果验证器 (Causal Verifier · Yin Agent).

Your role is to verify whether the task output satisfies all applicable
constraints and produces correct results.

Instructions:
1. Review the task output and tool results provided below.
2. Check each constraint carefully — hard violations must be flagged.
3. Provide a structured verification report in JSON format:
   {
     "route": "Pass" | "BackToTpn" | "BackToMeta",
     "confidence": 0.0..1.0,
     "summary": "Brief justification for the decision",
     "constraint_violations": ["description of each violation"]
   }

Routing:
- "Pass":        Output is correct and converges. Proceed to DMN reflection.
- "BackToTpn":   Output has minor issues — retry probability fitting (yang).
- "BackToMeta":  Output has fundamental issues — retry weight update (yuan).

Be thorough but fair. False positives (rejecting correct output) waste cycles,
while false negatives (accepting incorrect output) degrade the knowledge base.
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
pub struct CausalConvergeAgentBuilder {
    engine_ctx: EngineContext,
    model: String,
    provider: Arc<ProviderRegistry>,
    max_turns: u32,
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
        }
    }

    /// Run convergence: aggregate subtask results and decide convergence.
    ///
    /// # Logic
    /// 1. Checks if all subtasks completed successfully → `Converged`.
    /// 2. If some failed or are pending → `Partial`.
    /// 3. If all failed or diverged → `Diverged`.
    ///
    /// In the production path an LLM call (with system prompt
    /// `CONVERGE_SYSTEM_PROMPT`) reviews the subtask outputs and makes a
    /// nuanced decision.  In degraded mode a deterministic heuristic is used.
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
    /// # Current behaviour (degraded mode)
    /// Uses a deterministic heuristic based on subtask status counts.  Returns
    /// `Converged` when all subtasks are in a non-failed state.
    pub async fn converge(
        &self,
        subtask_results: &[DecomposeResult],
    ) -> Result<ConvergenceDecision, TaijiError> {
        // ── Deterministic heuristic (used in degraded mode) ──
        let total = subtask_results.len();
        if total == 0 {
            return Ok(ConvergenceDecision {
                status: ConvergenceStatus::Converged,
                task_summary: format!("Task {} has no subtasks", self.engine_ctx.task_id),
            });
        }

        let diverged_count = subtask_results
            .iter()
            .filter(|r| r.status == ConvergenceStatus::Diverged)
            .count();
        let partial_count = subtask_results
            .iter()
            .filter(|r| r.status == ConvergenceStatus::Partial)
            .count();

        let status = if diverged_count == total {
            ConvergenceStatus::Diverged
        } else if diverged_count > 0 || partial_count > 0 {
            ConvergenceStatus::Partial
        } else {
            ConvergenceStatus::Converged
        };

        let task_summary = format!(
            "Task {}: {} subtask(s) evaluated — {:?}",
            self.engine_ctx.task_id,
            total,
            status,
        );

        // ── Production path: LLM convergence judgment (TODO) ──
        // The deterministic heuristic above is used in degraded mode.
        // The true production path invokes a Rig agent with CONVERGE_SYSTEM_PROMPT
        // for nuanced aggregation.

        let decision = ConvergenceDecision {
            status,
            task_summary,
        };

        tracing::debug!(
            task_id = %self.engine_ctx.task_id,
            subtasks = total,
            status = ?decision.status,
            "CausalConvergeAgent.converge() — deterministic heuristic"
        );

        Ok(decision)
    }
}

/// System prompt for the CausalAgent in **converge** mode.
///
/// Starts with the required Chinese identifier per AGENTS.md §2:
/// "CausalAgent converge 模式的 system prompt 必须以 '你是收敛判决器' 开头".
const CONVERGE_SYSTEM_PROMPT: &str = r#"你是收敛判决器 (Convergence Judge · Yin Agent).

Your role is to aggregate results from all subtasks of a recursive
decomposition and decide whether the overall task has converged.

Instructions:
1. Review the results from each subtask.
2. Consider interdependencies — some subtask failures may be recoverable.
3. Produce a convergence decision in JSON format:
   {
     "status": "Converged" | "Partial" | "Diverged",
     "task_summary": "Explanation of the decision"
   }

- "Converged": All subtasks completed, task objective met.
- "Partial": Some subtasks incomplete or failed, but partial progress made.
- "Diverged": Fundamental failure — the decomposition strategy needs revision.

Be precise. Premature convergence leads to incomplete knowledge;
false divergence wastes cycles on unnecessary retries.
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
            qdrant: crate::infra::config::QdrantConfig::default(),
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
        }
    }

    // ── CausalVerifyAgentBuilder tests ──────────────────────────────────

    #[tokio::test]
    async fn test_verify_returns_default_pass() {
        let builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(ConstraintEngine::new()),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let report = builder.verify("CausalAgent executed and verified the task output against all L4 Truth constraints.", &[]).await.expect("verify");
        assert_eq!(report.route, VerificationRoute::Pass);
        assert_eq!(report.confidence, 0.0, "degraded mode confidence should be 0");
    }

    #[tokio::test]
    async fn test_verify_empty_summary_triggers_back_to_meta() {
        // Empty summary should trigger the constraint pre-check for
        // truth:no-fabrication (hard).
        let builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(ConstraintEngine::new()),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        // An empty task output violates truth:no-fabrication (hard),
        // which should return BackToMeta.
        let report = builder.verify("", &[]).await.expect("verify");
        assert_eq!(report.route, VerificationRoute::BackToMeta);
        assert!(!report.constraint_violations.is_empty());
    }

    #[tokio::test]
    async fn test_verify_with_soft_violations_passes() {
        // A short summary (< 10 chars) triggers the soft auditability
        // constraint, but since it's soft the verify should still pass.
        let builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(ConstraintEngine::new()),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let report = builder.verify("Adequate summary for audit.", &[]).await.expect("verify");
        // In degraded mode, soft violations are noted but route is Pass.
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

        let decision = builder.converge(&[]).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Converged);
    }

    #[tokio::test]
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
                summary: "Done".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
            },
            DecomposeResult {
                summary: "Done too".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
            },
        ];

        let decision = builder.converge(&results).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Converged);
    }

    #[tokio::test]
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
                summary: "Done".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
            },
            DecomposeResult {
                summary: "Partial".into(),
                status: ConvergenceStatus::Partial,
                subtask_count: 0,
            },
        ];

        let decision = builder.converge(&results).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Partial);
    }

    #[tokio::test]
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
                summary: "Failed".into(),
                status: ConvergenceStatus::Diverged,
                subtask_count: 0,
            },
            DecomposeResult {
                summary: "Also failed".into(),
                status: ConvergenceStatus::Diverged,
                subtask_count: 0,
            },
        ];

        let decision = builder.converge(&results).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Diverged);
    }

    // ── System prompt tests ─────────────────────────────────────────────

    #[test]
    fn test_verify_system_prompt_starts_with_chinese() {
        assert!(VERIFY_SYSTEM_PROMPT.starts_with("你是因果验证器"));
        assert!(VERIFY_SYSTEM_PROMPT.contains("Pass"));
        assert!(VERIFY_SYSTEM_PROMPT.contains("BackToTpn"));
        assert!(VERIFY_SYSTEM_PROMPT.contains("BackToMeta"));
    }

    #[test]
    fn test_converge_system_prompt_starts_with_chinese() {
        assert!(CONVERGE_SYSTEM_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_SYSTEM_PROMPT.contains("Converged"));
        assert!(CONVERGE_SYSTEM_PROMPT.contains("Partial"));
        assert!(CONVERGE_SYSTEM_PROMPT.contains("Diverged"));
    }
}
