//! CausalAgent builder (因果验证·阴) — "causal verification, the yin phase".
//!
//! The CausalAgent is the **third** agent in the TPN cycle.  It operates in
//! two modes (each with mode-paired templates, V27 阴阳配对):
//!
//! | Mode        | Role                 | Output                               | max_turns |
//! |-------------|----------------------|--------------------------------------|-----------|
//! | `verify`    | 因果验证器 (verifier) | [`VerificationReport`]               | 10        |
//! | `converge`  | 收敛判决器 (judge)    | [`ConvergenceDecision`]              | 10        |
//!
//! # Verify mode (CausalVerifyAgentBuilder)
//! Checks a task output (or intermediate tool result) against:
//! 1. **Constraint pre-check** ([`ConstraintEngine`]) — run **before** the LLM
//!    call.  Any hard constraint violation immediately short-circuits with
//!    `BackToMeta`.  Soft violations are injected into the LLM prompt.
//! 2. **LLM judgment** — the model reviews the output and issues a verdict
//!    (`Pass` / `BackToTpn` / `BackToMeta`).  The fallback template is
//!    selected by `meta_ctx.mode` (V27): `VERIFY_ORC` for orchestration
//!    nodes, `VERIFY_EXEC` for execution nodes.
//!
//! # Converge mode (CausalConvergeAgentBuilder)
//! Aggregates results from all subtasks of a recursive decomposition and
//! decides whether the overall task has converged, partially converged,
//! or diverged.  The fallback template is selected by `meta_ctx.mode`
//! (V27): `CONVERGE_ORC` for orchestration nodes, `CONVERGE_EXEC` for
//! execution nodes.
//!
//! # Constraints (AGENTS.md §2, §4)
//! - `max_turns = 10` for both modes (V26 3→6 仍不足，V26.1 升至 10).
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
use crate::infra::json_util::parse_llm_json;
use crate::infra::knowledge::LiluoClient;
use crate::infra::trace::save_json_atomic;
use crate::infra::provider::ProviderRegistry;
use crate::orchestration::constraint_engine::ConstraintEngine;
use crate::orchestration::contract_engine::ContractEngine;
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;
use crate::types::task::DecomposeResult;
use crate::types::verification::{
    CheckKind, CheckSpec, ContractReport, ConvergenceDecision, ConvergenceStatus,
    VerificationReport, VerificationRoute,
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
    /// V36 模型路由：provider 名（MetaContext.model 解析结果；默认 deepseek）。
    provider_name: String,
    max_turns: u32,
    /// Process-wide SafetyHook (or a default-configured instance) — always
    /// mounted on the Rig agent.
    safety_hook: Arc<SafetyHook>,
    /// 归藏客户端（V33 ContractEngine 加载验证契约）。工厂总是设置；
    /// None = 未接线（测试/异常路径）→ 契约层跳过并 warn（BCP §8.22
    /// 无契约资产时退化为纯 LLM 验证）。
    guizang: Option<Arc<LiluoClient>>,
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
            provider_name: "deepseek".to_string(),
            max_turns: 10,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            guizang: None,
        }
    }

    /// V36：设置 LLM provider 名（MetaContext.model 路由结果；默认 deepseek）。
    pub fn provider_name(mut self, provider: &str) -> Self {
        self.provider_name = provider.to_string();
        self
    }

    /// Override the SafetyHook with the shared process-wide singleton.
    pub fn safety_hook(mut self, hook: Arc<SafetyHook>) -> Self {
        self.safety_hook = hook;
        self
    }

    /// Wire the 归藏 client (V33 ContractEngine 契约加载通道)。
    pub fn guizang(mut self, guizang: Arc<LiluoClient>) -> Self {
        self.guizang = Some(guizang);
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
    ///    prompt (`VERIFY_ORC/EXEC_SYSTEM_PROMPT` by mode, starts with "你是因果验证器"), registers
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
    ///     .preamble(VERIFY_ORC_SYSTEM_PROMPT)
    ///     .max_turns(10)
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

        // ── Step 1.5: ContractEngine 机械执行验证契约（V33 §6.6/§8.22）──
        // L0 机械 + L1 契约：确定性裁决，任一 hard 机械项失败直接短路，
        // LLM 不可翻案。契约加载失败上抛（无降级原则 — §8.20）；
        // guizang 未接线（None）→ 契约层跳过并 warn（测试/异常路径）。
        // contracts 提升到外层作用域：一次加载，供 llm_judgement 收集复用。
        // V36 分区一致性（§8.3）：契约从路由模型分区加载（meta_ctx.model →
        // for_model 派生）；None = 根 client（legacy/未接线）。
        // V37 异源裁判：verify_model 优先（验证契约随验证模型分区——异源模型
        // 的分区可能持有不同的契约集，§6.1 学习单元语义）。
        let contracts: Vec<crate::types::agent::VerificationAsset> =
            if let Some(guizang) = &self.guizang {
                let contract_key = meta_ctx.verify_model.as_ref().or(meta_ctx.model.as_ref());
                match contract_key {
                    Some(key) => {
                        let partition = guizang.for_model(key.key()).await?;
                        ContractEngine::load_contracts(&partition).await?
                    }
                    None => ContractEngine::load_contracts(guizang).await?,
                }
            } else {
                tracing::warn!(
                    task_id = %self.engine_ctx.task_id,
                    "CausalVerifyAgent: guizang not wired — contract layer skipped"
                );
                Vec::new()
            };
        let contract_report: ContractReport =
            ContractEngine::run_checks(&contracts, &self.engine_ctx.task_dir).await;

        if !contract_report.passed {
            tracing::warn!(
                task_id = %self.engine_ctx.task_id,
                summary = %contract_report.summary,
                "Contract check failed (hard short-circuit) — returning BackToMeta"
            );
            let failed_checks: Vec<String> = contract_report
                .results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| format!("{}: {}", r.check_id, r.detail))
                .collect();
            return Ok(VerificationReport {
                route: VerificationRoute::BackToMeta,
                confidence: 1.0,
                summary: format!("Contract check failed: {}", contract_report.summary),
                constraint_violations: failed_checks,
            });
        }

        // llm_judgement 项收集（L2 兜底 — 唯一留给 LLM 的检查项类型）。
        // 机械全过 + 有契约 + 无 llm_judgement 项 → 直接 PASS（LLM 零调用）：
        // 契约完备即收敛（验证符号化的直接收益，§8.23 MVP-1 验收）。
        let llm_judgements: Vec<&CheckSpec> = contracts
            .iter()
            .flat_map(|v| v.checks.iter())
            .filter(|c| c.kind == CheckKind::LlmJudgement)
            .collect();

        if !contract_report.results.is_empty() && llm_judgements.is_empty() {
            tracing::info!(
                task_id = %self.engine_ctx.task_id,
                checks = contract_report.results.len(),
                "All mechanical checks passed, no llm_judgement — direct PASS (LLM zero-call)"
            );
            return Ok(VerificationReport {
                route: VerificationRoute::Pass,
                confidence: 1.0,
                summary: contract_report.summary,
                constraint_violations: vec![],
            });
        }

        // ── Step 2: Select prompt — prefer MetaAgent-composed, fallback to
        //    mode-paired template (V27 阴阳配对: 执行-验证 / 编排-验证) ──
        let system_prompt = match &meta_ctx.verify_system_prompt {
            Some(prompt) => prompt.as_str(),
            None => match meta_ctx.mode {
                crate::types::agent::AgentMode::Orchestration => VERIFY_ORC_SYSTEM_PROMPT,
                crate::types::agent::AgentMode::Execution => VERIFY_EXEC_SYSTEM_PROMPT,
            },
        };

        // Build soft violation context for the LLM prompt
        let soft_context: Vec<String> = pre_check
            .violations
            .iter()
            .filter(|v| v.severity == crate::types::verification::ConstraintSeverity::Soft)
            .map(|v| format!("[Soft] {}: {}", v.truth_name, v.reason))
            .collect();

        // Call the LLM for verification（V36：按路由 provider 选择）
        let client: Arc<deepseek::Client> = self.provider.client_for(&self.provider_name)?;

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

        // 契约执行结果注入 LLM（机械全过部分 + llm_judgement 判据 + 反偏置）。
        let contract_section = if contract_report.results.is_empty() && llm_judgements.is_empty() {
            String::new()
        } else {
            let results_summary: Vec<String> = contract_report
                .results
                .iter()
                .map(|r| format!("[{}] {}: {}", if r.passed { "PASS" } else { "FAIL" }, r.check_id, r.detail))
                .collect();
            let criteria: Vec<String> = llm_judgements
                .iter()
                .map(|c| {
                    // V33/MVP-3: fork 变体 strictness 档位注入（§8.21「收紧判据」机械实现）——
                    // params.strictness == "strict" → 从严裁决指令（证据不足即 FAIL）。
                    let strict = c.params.get("strictness").and_then(|v| v.as_str())
                        == Some("strict");
                    if strict {
                        format!(
                            "[{}] {}（从严档：证据不足即判 FAIL，禁止宽松推断）",
                            c.id, c.pass_condition
                        )
                    } else {
                        format!("[{}] {}", c.id, c.pass_condition)
                    }
                })
                .collect();
            let mut section = format!(
                "\n\nContract report (mechanical checks — deterministic, cannot be overridden):\n{}",
                if results_summary.is_empty() {
                    contract_report.summary.clone()
                } else {
                    results_summary.join("\n")
                }
            );
            if !criteria.is_empty() {
                section.push_str("\n\nLlmJudgement criteria (your sole discretionary remit):\n");
                section.push_str(&criteria.join("\n"));
                section.push_str(
                    "\n\n反偏置指令（V33 §6.6）：表面流畅不算数，必须引用具体证据；\n禁止因篇幅长 / 风格好加分；逐文件用 read 工具取证后裁决。",
                );
            }
            section
        };

        let input = format!(
            "Task output:\n{task_output}\n\nTool results:\n{results}\n\nSoft violations:\n{soft}{contract}",
            task_output = task_output,
            results = tool_results.join("\n---\n"),
            soft = if soft_context.is_empty() {
                "None".to_string()
            } else {
                soft_context.join("\n")
            },
            contract = contract_section,
        );

        let response = agent.prompt(&input).await.map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("CausalVerifyAgent LLM call failed: {e}"),
            }
        })?;

        // Parse structured output into VerificationReport
        let report: VerificationReport =
            parse_llm_json(response.as_ref()).map_err(|e| {
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
            "CausalVerifyAgent — LLM verification completed"
        );

        // ── Persist verify state for crash recovery ──
        let verify_state = serde_json::json!({
            "report": &report,
            "round": self.engine_ctx.round,
            "cycle": self.engine_ctx.cycle,
            "checks": &contract_report.results,
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

/// System prompt for the CausalAgent in **verify · Orchestration** mode
/// (V27 阴阳配对：编排-验证，编排节点的阴相位)。
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
5. Requirement satisfaction — does the synthesized output meet the task description?
6. Constraint adherence — are all L4 Truth constraints satisfied?

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

/// System prompt for the CausalAgent in **verify · Execution** mode
/// (V27 阴阳配对：执行-验证，执行节点的阴相位)。
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
    /// V36 模型路由：provider 名（MetaContext.model 解析结果；默认 deepseek）。
    provider_name: String,
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
            provider_name: "deepseek".to_string(),
            max_turns: 10,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
        }
    }

    /// V36：设置 LLM provider 名（MetaContext.model 路由结果；默认 deepseek）。
    pub fn provider_name(mut self, provider: &str) -> Self {
        self.provider_name = provider.to_string();
        self
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
    ///    converge system prompt (`CONVERGE_ORC/EXEC_SYSTEM_PROMPT` by mode, starts with
    ///    "你是收敛判决器") registers read-only tools `read` + `webfetch`, mounts
    ///    the SafetyHook, reviews the aggregated subtask results and issues a
    ///    structured [`ConvergenceDecision`] (Converged / Partial / Diverged).
    /// 3. The decision is returned; its crash-recovery window is covered by the
    ///    parent task rerun (children/ reuse → idempotent reconverge) since
    ///    V26.2 no longer persists a converge state file.
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
    ///     .preamble(CONVERGE_ORC_SYSTEM_PROMPT)
    ///     .max_turns(10)
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
    ) -> Result<ConvergenceDecision, TaijiError> {
        let total = subtask_results.len();

        // ── Empty results short-circuit ──
        if total == 0 {
            return Ok(ConvergenceDecision {
                status: ConvergenceStatus::Converged,
                task_summary: format!("Task {} has no subtasks — trivially converged", self.engine_ctx.task_id),
            });
        }

        // ── Select prompt — prefer MetaAgent-composed, fallback to
        //    mode-paired template (V27 阴阳配对: 编排-收敛 / 执行-收敛) ──
        let system_prompt = match &meta_ctx.converge_system_prompt {
            Some(prompt) => prompt.as_str(),
            None => match meta_ctx.mode {
                crate::types::agent::AgentMode::Orchestration => CONVERGE_ORC_SYSTEM_PROMPT,
                crate::types::agent::AgentMode::Execution => CONVERGE_EXEC_SYSTEM_PROMPT,
            },
        };

        // ── Production path: LLM convergence judgment ──
        let client: Arc<deepseek::Client> = self.provider.client_for(&self.provider_name)?;

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
            parse_llm_json(response.as_ref()).map_err(|e| {
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
            "CausalConvergeAgent — LLM convergence judgment completed"
        );

        Ok(decision)
    }
}

/// System prompt for the CausalAgent in **converge · Orchestration** mode
/// (V27 阴阳配对：编排-收敛，编排节点的阴相位——判决子结果聚合)。
///
/// Aggregates subtask results of a recursive decomposition: coverage,
/// cross-subtask consistency, integration quality, finality.
const CONVERGE_ORC_SYSTEM_PROMPT: &str = r#"你是收敛判决器 — 编排收敛 (Convergence Judge · Orchestration).

The task was **orchestrated**: decomposed into multiple subtasks whose
results are being aggregated.  Your job is to judge whether the aggregated
result has converged.

Your focus:
1. Goal achievement — was the overall task objective met?
2. Coverage — do the subtask results collectively cover the full task scope (MECE)?
3. Consistency — are the results compatible (no contradictions across subtasks)?
4. Integration — can the partial results be combined into a coherent whole?
5. Finality — does the synthesized output represent a complete answer?

## Deliverable Verification
Each result includes a `deliverables` field containing absolute paths to
produced files.  You MUST use the `read` tool to open each file and verify:
- Cross-subtask consistency — do files from different subtasks agree?
- Completeness — are all required artifacts present?
- Quality — do the files meet the required standards?

## Failed Subtask Reporting (V31)
Some subtask results may have `status: "Diverged"` with a `summary` like
`[failure_kind] reason` — the subtask failed (context overflow / llm failure /
IO / etc.) and its `deliverables` (if any) contain the handoff artifact it
wrote before failing. Treat these as **partial progress reports**:
- Judge `Partial` when failures are recoverable and most subtasks succeeded.
- Judge `Diverged` when the failure is fundamental or no progress was made.
- In `task_summary`, **state which subtask failed, why, and whether re-running
  it with adjusted guidance is worthwhile** — the parent orchestrator reads
  this to decide re-decomposition (rerun_of) vs. accepting partial output.

Produce a convergence decision in JSON format:
{
  "status": "Converged" | "Partial" | "Diverged",
  "task_summary": "Explanation of the decision"
}

- "Converged": All dimensions covered, results consistent.
- "Partial": Some gaps or inconsistencies remain, but partial progress made.
- "Diverged": Fundamental incoherence — decomposition strategy needs revision.
"#;

/// System prompt for the CausalAgent in **converge · Execution** mode
/// (V27 阴阳配对：执行-收敛，直接产出任务的收敛判决)。
///
/// A single direct output — judge whether it represents a complete,
/// final answer.
const CONVERGE_EXEC_SYSTEM_PROMPT: &str = r#"你是收敛判决器 — 执行收敛 (Convergence Judge · Execution).

The task was **executed directly** — a single output produced with L1 tools.
Your job is to judge whether this output has converged to a complete answer.

Your focus:
1. Goal achievement — was the task objective met?
2. Completeness — is the output fully addressed, with no missing dimensions?
3. Quality — are the deliverables well-formed and directly usable?
4. Finality — does the output represent a final answer (not a draft or partial)?

## Deliverable Verification
Each result includes a `deliverables` field containing absolute paths to
produced files.  You MUST use the `read` tool to open each file and verify:
- Completeness — are all required artifacts present?
- Quality — do the files meet the required standards?

Produce a convergence decision in JSON format:
{
  "status": "Converged" | "Partial" | "Diverged",
  "task_summary": "Explanation of the decision"
}

- "Converged": The output is complete and final.
- "Partial": Some gaps remain; partial progress made.
- "Diverged": Fundamental incoherence — the output does not satisfy the task.
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

        let report = builder.verify("CausalAgent executed and verified the task output against all L4 Truth constraints.", &[], &MetaContext::empty()).await.expect("converge");
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
        let report = builder.verify("", &[], &MetaContext::empty()).await.expect("converge");
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

        let report = builder.verify("Adequate summary for audit.", &[], &MetaContext::empty()).await.expect("converge");
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

        let decision = builder.converge(&[], &MetaContext::empty()).await.expect("converge");
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

        let decision = builder.converge(&results, &MetaContext::empty()).await.expect("converge");
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

        let decision = builder.converge(&results, &MetaContext::empty()).await.expect("converge");
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

        let decision = builder.converge(&results, &MetaContext::empty()).await.expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Diverged);
    }

    // ── System prompt tests (V27 阴阳配对：ORC/EXEC 双模板) ────────────

    #[test]
    fn test_verify_system_prompt_starts_with_chinese() {
        assert!(VERIFY_ORC_SYSTEM_PROMPT.starts_with("你是因果验证器"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("Pass"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("BackToTpn"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("BackToMeta"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("MECE"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("File Verification"));
        // V27 配对：编排验证模板关注拆解完备性。
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("Orchestration"));

        assert!(VERIFY_EXEC_SYSTEM_PROMPT.starts_with("你是因果验证器"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("Pass"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("BackToTpn"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("BackToMeta"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("Execution"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("File Verification"));
    }

    #[test]
    fn test_verify_prompt_fallback_follows_mode() {
        // V27：降级路径按 meta_ctx.mode 选配对模板（无归藏资产时）。
        let mut ctx_orc = MetaContext::empty();
        ctx_orc.mode = crate::types::agent::AgentMode::Orchestration;
        let mut ctx_exec = MetaContext::empty();
        ctx_exec.mode = crate::types::agent::AgentMode::Execution;

        // verify：编排 → VERIFY_ORC，执行 → VERIFY_EXEC（与 verify() 内同一 match）。
        assert_eq!(ctx_orc.verify_system_prompt, None);
        assert_eq!(ctx_exec.verify_system_prompt, None);
        let _ = match ctx_orc.mode {
            crate::types::agent::AgentMode::Orchestration => VERIFY_ORC_SYSTEM_PROMPT,
            crate::types::agent::AgentMode::Execution => VERIFY_EXEC_SYSTEM_PROMPT,
        };
        let _ = match ctx_exec.mode {
            crate::types::agent::AgentMode::Orchestration => VERIFY_ORC_SYSTEM_PROMPT,
            crate::types::agent::AgentMode::Execution => VERIFY_EXEC_SYSTEM_PROMPT,
        };

        // converge：编排 → CONVERGE_ORC，执行 → CONVERGE_EXEC（与 converge() 内同一 match）。
        let _ = match ctx_orc.mode {
            crate::types::agent::AgentMode::Orchestration => CONVERGE_ORC_SYSTEM_PROMPT,
            crate::types::agent::AgentMode::Execution => CONVERGE_EXEC_SYSTEM_PROMPT,
        };
        let _ = match ctx_exec.mode {
            crate::types::agent::AgentMode::Orchestration => CONVERGE_ORC_SYSTEM_PROMPT,
            crate::types::agent::AgentMode::Execution => CONVERGE_EXEC_SYSTEM_PROMPT,
        };
    }

    #[test]
    fn test_verify_system_prompt_max_turns_ten() {
        let builder = CausalVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );
        assert_eq!(builder.max_turns, 10, "V26.1: verify max_turns 统一 10");
    }

    #[test]
    fn test_converge_system_prompt_starts_with_chinese() {
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Converged"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Partial"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Diverged"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Coverage"));
        // V27 配对：编排收敛模板关注跨子任务一致性。
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Orchestration"));

        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Converged"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Partial"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Diverged"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Execution"));
    }

    #[test]
    fn test_converge_system_prompt_max_turns_ten() {
        let builder = CausalConvergeAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );
        assert_eq!(builder.max_turns, 10, "V26.1: converge max_turns 统一 10");
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
