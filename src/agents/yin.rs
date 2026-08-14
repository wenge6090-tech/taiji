//! YinAgent builder (因果验证·阴) — "yin verification, the yin phase".
//!
//! The YinAgent is the **third** agent in the Zhouyi cycle.  It operates in
//! two modes (each with mode-paired templates, V27 阴阳配对):
//!
//! | Mode        | Role                 | Output                               | max_turns |
//! |-------------|----------------------|--------------------------------------|-----------|
//! | `verify`    | 因果验证器 (verifier) | [`VerificationReport`]               | 200       |
//! | `converge`  | 收敛判决器 (judge)    | [`ConvergenceDecision`]              | 200       |
//!
//! # Verify mode (YinVerifyAgentBuilder)
//! Checks a task output (or intermediate tool result) against:
//! 1. **Constraint pre-check** ([`ConstraintEngine`]) — run **before** the LLM
//!    call.  Any hard constraint violation immediately short-circuits with
//!    `BackToMeta`.  Soft violations are injected into the LLM prompt.
//! 2. **LLM judgment** — the model reviews the output and issues a verdict
//!    (`Pass` / `BackToZhouyi` / `BackToMeta`).  The fallback template is
//!    selected by `meta_ctx.mode` (V27): `VERIFY_ORC` for orchestration
//!    nodes, `VERIFY_EXEC` for execution nodes.
//!
//! # Converge mode (YinConvergeAgentBuilder)
//! Aggregates results from all subtasks of a recursive decomposition and
//! decides whether the overall task has converged, partially converged,
//! or diverged.  The fallback template is selected by `meta_ctx.mode`
//! (V27): `CONVERGE_ORC` for orchestration nodes, `CONVERGE_EXEC` for
//! execution nodes.
//!
//! # Constraints (AGENTS.md §2, §4)
//! - `max_turns = 200` for both modes (V49 防御兜底——预算由 ContextLimiter 承担，
//!   BCP §8.19 阴预算对称；V26.1 曾升 10，V49 起降级为防死循环兜底).
//! - Verify system prompt starts with `"你是因果验证器"`.
//! - Converge system prompt starts with `"你是收敛判决器"`.

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::deepseek;

use crate::agents::tools::skills::SkillRegistry;
use crate::hooks::context_limiter::{ContextLimiter, LimitKind};
use crate::hooks::safety::SafetyHook;
use crate::hooks::yin_hook_set::YinHookSet;
use crate::infra::config::{ContextLimits, SafetyConfig};
use crate::infra::error::TaijiError;
use crate::infra::json_util::parse_llm_json;
use crate::infra::knowledge::GuizangClient;
use crate::infra::trace::save_json_atomic;
use crate::infra::provider::ProviderRegistry;
use crate::orchestration::constraint_engine::ConstraintEngine;
use crate::orchestration::skill_engine::SkillEngine;
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;
use crate::types::task::DecomposeResult;
use crate::types::verification::{
    SkillCategory, SkillReport, ConvergenceDecision, ConvergenceStatus,
    VerificationReport, VerificationRoute,
};

// ---------------------------------------------------------------------------
// Verify mode
// ---------------------------------------------------------------------------

/// Builder for the YinAgent in **verify** mode (因果验证·阴).
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
/// Created by [`AgentFactory::create_yin_verify_agent`].
pub struct YinVerifyAgentBuilder {
    engine_ctx: EngineContext,
    model: String,
    provider: Arc<ProviderRegistry>,
    /// V36 模型路由：provider 名（MetaContext.model 解析结果；默认 deepseek）。
    provider_name: String,
    max_turns: u32,
    /// Process-wide SafetyHook (or a default-configured instance) — always
    /// mounted on the Rig agent.
    safety_hook: Arc<SafetyHook>,
    /// V49 阴预算（BCP §8.19）：窗口阈值来源（30% 交接 / 35% 硬截止），工厂设置。
    context_limits: ContextLimits,
    /// 归藏客户端（V33 SkillEngine 加载验证契约）。工厂总是设置；
    /// None = 未接线（测试/异常路径）→ 契约层跳过并 warn（BCP §8.22
    /// 无契约资产时退化为纯 LLM 验证）。
    guizang: Option<Arc<GuizangClient>>,
}

impl YinVerifyAgentBuilder {
    /// Create a new `YinVerifyAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_yin_verify_agent`] —
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
            max_turns: 200,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            context_limits: ContextLimits::default(),
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

    /// Wire the 归藏 client (V33 SkillEngine 契约加载通道)。
    pub fn guizang(mut self, guizang: Arc<GuizangClient>) -> Self {
        self.guizang = Some(guizang);
        self
    }

    /// V49 阴预算（BCP §8.19）：设置窗口阈值来源（工厂接线）。
    pub fn context_limits(mut self, limits: ContextLimits) -> Self {
        self.context_limits = limits;
        self
    }

    /// Run verification: check the task output and tool results against L4
    /// Truth constraints and an LLM judgment.
    ///
    /// # Logic
    /// 1. **Constraint pre-check**: runs `ConstraintEngine::check_yin_output`
    ///    on the concatenated input.  Any hard violation short-circuits with
    ///    `BackToMeta` immediately (no LLM call).
    /// 2. **LLM verification**: constructs a Rig agent with the verify system
    ///    prompt (`VERIFY_ORC/EXEC_SYSTEM_PROMPT` by mode, starts with "你是因果验证器"), registers
    ///    read-only tools `read` + `webfetch` (逐文件核验 + 联网核实), mounts the
    ///    SafetyHook, calls the LLM, and parses the structured output into a
    ///    [`VerificationReport`].
    /// 3. **Return**: the final report.
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
        // V50 §6.6：元层基线 ∪ 连山挖掘规则（rules.yaml）；None = 未接线测试路径
        // （状态分支）；归藏 I/O 失败上抛（§8 无降级）。
        let rules = match &self.guizang {
            Some(g) => g.load_rules().await?,
            None => vec![],
        };
        let constraints = ConstraintEngine::load_truths(&meta_ctx.task_type_tags, &rules);

        // ── Step 1: Constraint pre-check ──
        let pre_check = ConstraintEngine::check_yin_output(task_output, tool_results, &constraints);

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

        // ── Step 1.5: SkillEngine 机械执行验证契约（V33 §6.6/§8.22）──
        // L0 机械 + L1 契约：确定性裁决，任一 hard 机械项失败直接短路，
        // LLM 不可翻案。契约加载失败上抛（无降级原则 — §8.20）；
        // guizang 未接线（None）→ 契约层跳过并 warn（测试/异常路径）。
        // V45 双轨：合并视图加载（元层 ∪ 资产层，同 id 资产优先——元层保底）。
        let contracts: Vec<crate::types::verification::SkillAsset> =
            if let Some(guizang) = &self.guizang {
                SkillEngine::load_skill_catalog(
                    guizang,
                    crate::types::verification::SkillCategory::Verify,
                    crate::infra::skill_catalog::ToolProfile::Full,
                )
                .await?
            } else {
                tracing::warn!(
                    task_id = %self.engine_ctx.task_id,
                    "YinVerifyAgent: guizang not wired — contract layer skipped"
                );
                Vec::new()
            };
        let skill_report: SkillReport =
            SkillEngine::run_checks_assets(&contracts, &self.engine_ctx.task_dir).await;

        if !skill_report.passed {
            tracing::warn!(
                task_id = %self.engine_ctx.task_id,
                summary = %skill_report.summary,
                "Contract check failed (hard short-circuit) — returning BackToMeta"
            );
            let failed_checks: Vec<String> = skill_report
                .results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| format!("{}: {}", r.check_id, r.detail))
                .collect();
            return Ok(VerificationReport {
                route: VerificationRoute::BackToMeta,
                confidence: 1.0,
                summary: format!("Contract check failed: {}", skill_report.summary),
                constraint_violations: failed_checks,
            });
        }

        // llm_judgement 项收集（L2 兜底 — 唯一留给 LLM 的检查项类型）。
        // 机械全过 + 有契约 + 无 llm_judgement 项 → 直接 PASS（LLM 零调用）：
        // 契约完备即收敛（验证符号化的直接收益，§8.23 MVP-1 验收）。
        let llm_judgements: Vec<(&str, &crate::types::verification::SkillImpl)> = contracts
            .iter()
            .flat_map(|v| {
                v.implementations
                    .iter()
                    .map(|i| (v.id.as_str(), i))
                    .collect::<Vec<_>>()
            })
            .filter(|(_, c)| c.kind == crate::types::verification::SkillKind::LlmJudgement)
            .collect();

        if !skill_report.results.is_empty() && llm_judgements.is_empty() {
            tracing::info!(
                task_id = %self.engine_ctx.task_id,
                checks = skill_report.results.len(),
                "All mechanical checks passed, no llm_judgement — direct PASS (LLM zero-call)"
            );
            return Ok(VerificationReport {
                route: VerificationRoute::Pass,
                confidence: 1.0,
                summary: skill_report.summary,
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
        let skill_tools: Vec<Box<dyn rig::tool::ToolDyn>> = SkillRegistry::new(&self.engine_ctx.task_dir)
            .tools()
            .iter()
            .filter(|t| matches!(t.name(), "read" | "webfetch"))
            .map(|t| Box::new(t.clone()) as Box<dyn rig::tool::ToolDyn>)
            .collect();
        // V49 阴预算（BCP §8.19）：safety → limiter 组合，一次 .hook() 挂载
        //（Rig 0.39 单槽 hook，AGENTS.md §4）。
        let limiter = ContextLimiter::new(
            self.context_limits.effective_handoff(),
            self.context_limits.effective_hard_cutoff(),
        );
        let hook_set = YinHookSet::new(self.safety_hook.as_ref().clone(), limiter.clone());
        let agent = client
            .agent(&self.model)
            .preamble(system_prompt)
            .max_tokens(1024u64)
            .default_max_turns(self.max_turns as usize)
            // V51 四象温度（AGENTS.md §3）：YinVerify 默认 0.2（低温稳定验证）。
            .temperature(0.2)
            .hook(hook_set)
            .tools(skill_tools)
            .build();

        // 契约执行结果注入 LLM（机械全过部分 + llm_judgement 判据 + 反偏置）。
        let contract_section = if skill_report.results.is_empty() && llm_judgements.is_empty() {
            String::new()
        } else {
            let results_summary: Vec<String> = skill_report
                .results
                .iter()
                .map(|r| format!("[{}] {}: {}", if r.passed { "PASS" } else { "FAIL" }, r.check_id, r.detail))
                .collect();
            let criteria: Vec<String> = llm_judgements
                .iter()
                .map(|(sid, c)| {
                    // V33/MVP-3: fork 变体 strictness 档位注入（§8.21「收紧判据」机械实现）——
                    // params.strictness == "strict" → 从严裁决指令（证据不足即 FAIL）。
                    let strict = c.params.get("strictness").and_then(|v| v.as_str())
                        == Some("strict");
                    if strict {
                        format!(
                            "[{}] {}（从严档：证据不足即判 FAIL，禁止宽松推断）",
                            sid, c.pass_condition
                        )
                    } else {
                        format!("[{}] {}", sid, c.pass_condition)
                    }
                })
                .collect();
            let mut section = format!(
                "\n\nContract report (mechanical checks — deterministic, cannot be overridden):\n{}",
                if results_summary.is_empty() {
                    skill_report.summary.clone()
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

        let response_result = agent.prompt(&input).await;

        // V49 阴溢出（BCP §8.19）：先于结果处理检查 limiter——Terminate 可能以
        // Err 或部分 Ok 浮现（与 yang.rs 同构）。
        if let Some(kind) = limiter.triggered() {
            match kind {
                LimitKind::Handoff => {
                    tracing::warn!(
                        task_id = %self.engine_ctx.task_id,
                        "YinVerifyAgent — context overflow → conservative BackToZhouyi verdict"
                    );
                    return Ok(VerificationReport {
                        route: VerificationRoute::BackToZhouyi,
                        confidence: 0.0,
                        summary: "verify context_overflow".into(),
                        constraint_violations: vec![],
                    });
                }
                LimitKind::HardCutoff => {
                    return Err(TaijiError::HardCutoff {
                        threshold: self.context_limits.effective_hard_cutoff(),
                    });
                }
            }
        }

        let response = response_result.map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("YinVerifyAgent LLM call failed: {e}"),
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
            "YinVerifyAgent — LLM verification completed"
        );

        // ── Persist verify state for crash recovery ──
        let verify_state = serde_json::json!({
            "report": &report,
            "round": self.engine_ctx.round,
            "cycle": self.engine_ctx.cycle,
            "checks": &skill_report.results,
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

/// System prompt for the YinAgent in **verify · Orchestration** mode
/// (V27 阴阳配对：编排-验证，编排节点的阴相位)。
///
/// Focuses on MECE completeness, dependency correctness, and decomposition
/// granularity. Route preference: BACK_TO_META for decomposition issues.
const VERIFY_ORC_SYSTEM_PROMPT: &str = r#"你是因果验证器 — 编排验证 (Yin Verifier · Orchestration).

你在验证一个编排节点的综合产出——该任务被拆解为子任务后汇聚结果。

## 验证维度
1. MECE 完备性——拆解是否覆盖了任务的全部维度？
2. 综合质量——汇聚结果是否连贯一致？跨子任务有无矛盾？
3. 粒度——子任务拆分粒度是否合适？
4. 需求满足——综合产出是否满足原始任务描述？

## 文件核验
必须用 `read` 工具逐文件检查 deliverables 目录下的实际产出。
不依赖摘要文本——读实际文件确认合规。

## 输出格式（严格 JSON）
{
  "route": "Pass" | "BackToZhouyi" | "BackToMeta",
  "confidence": 0.0..1.0,
  "summary": "判定依据简述",
  "constraint_violations": ["违规项描述"]
}

路由指引：
- "Pass":       综合完备、一致，可交付。
- "BackToZhouyi":  执行偏差——产出存在但质量/完整性不足，需重试拟合。
- "BackToMeta": 认知偏差——拆解策略本身有问题，需重新权重更新。
"#;

/// System prompt for the YinAgent in **verify · Execution** mode
/// (V27 阴阳配对：执行-验证，执行节点的阴相位)。
///
/// Focuses on requirement satisfaction, artifact quality, and constraint
/// adherence. Route preference: BACK_TO_ZHOUYI for execution quality issues.
const VERIFY_EXEC_SYSTEM_PROMPT: &str = r#"你是因果验证器 — 执行验证 (Yin Verifier · Execution).

你在验证一个执行节点的直接产出——任务由 L1 工具直接完成，未经拆解。

## 验证维度
1. 需求满足——产出是否完整覆盖任务描述？
2. 产物质量——交付物是否格式正确、内容可用？
3. 完整性——任务是否被完整处理，无遗漏维度？

## 文件核验
必须用 `read` 工具逐文件检查 deliverables 目录下的实际产出。
不依赖摘要文本——读实际文件确认合规。

## 输出格式（严格 JSON）
{
  "route": "Pass" | "BackToZhouyi" | "BackToMeta",
  "confidence": 0.0..1.0,
  "summary": "判定依据简述",
  "constraint_violations": ["违规项描述"]
}

路由指引：
- "Pass":       产出满足需求，可交付。
- "BackToZhouyi":  执行偏差——质量/完整性可改进，重试执行。
- "BackToMeta": 认知偏差——任务规格或方法本身有问题，需重新权重更新。
"#;

// ---------------------------------------------------------------------------
// Converge mode
// ---------------------------------------------------------------------------

/// Builder for the YinAgent in **converge** mode (收敛判决).
///
/// Aggregates results from all subtasks of a recursive decomposition and
/// decides whether the overall task has converged.
///
/// Created by [`AgentFactory::create_yin_converge_agent`].
///
/// The LLM registers read-only verification tools (`read` + `webfetch`) so it
/// can open each referenced deliverable and cross-check external facts before
/// issuing the convergence verdict; the [`SafetyHook`] is **always** mounted
/// (defaults to `SafetyConfig::default()` when no shared singleton is
/// injected) — "带工具必有安全钩子" is a type-level guarantee (蓝图 V25 §8.5).
pub struct YinConvergeAgentBuilder {
    engine_ctx: EngineContext,
    model: String,
    provider: Arc<ProviderRegistry>,
    /// V36 模型路由：provider 名（MetaContext.model 解析结果；默认 deepseek）。
    provider_name: String,
    max_turns: u32,
    /// Process-wide SafetyHook (or a default-configured instance) — always
    /// mounted on the Rig agent.
    safety_hook: Arc<SafetyHook>,
    /// V49 阴预算（BCP §8.19）：窗口阈值来源（30% 交接 / 35% 硬截止），工厂设置。
    context_limits: ContextLimits,
    /// 归藏客户端（V43 SkillEngine 加载 converge Skill）。工厂总是设置；
    /// None = 未接线（测试/异常路径）——converge Skill 层跳过。
    guizang: Option<Arc<GuizangClient>>,
}

impl YinConvergeAgentBuilder {
    /// Create a new `YinConvergeAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_yin_converge_agent`] —
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
            max_turns: 200,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            context_limits: ContextLimits::default(),
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

    /// Wire the Guizang client（V43 SkillEngine converge Skill 加载通道）。
    pub fn guizang(mut self, guizang: Arc<GuizangClient>) -> Self {
        self.guizang = Some(guizang);
        self
    }

    /// V49 阴预算（BCP §8.19）：设置窗口阈值来源（工厂接线）。
    pub fn context_limits(mut self, limits: ContextLimits) -> Self {
        self.context_limits = limits;
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

        // ── Step 1.5: SkillEngine 机械执行 converge Skill（V43 §6.6/§10.1）──
        // L0 机械 + L1 Skill：加载 yin/skills/converge/ 全部 active Skill（V45 合并视图），
        // 确定性裁决。converge Skill 的 checks 全部为 llm_judgement 类（soft）——
        // 不触发 hard 短路，仅收集注入 LLM prompt 供参考。
        // guizang 未接线（None）→ converge Skill 层跳过并 warn。
        let (converge_skills, converge_skill_report) = if let Some(guizang) = &self.guizang {
            let skills = SkillEngine::load_skill_catalog(
                guizang,
                SkillCategory::Converge,
                crate::infra::skill_catalog::ToolProfile::Full,
            )
            .await?;
            if skills.is_empty() {
                (skills, None)
            } else {
                let report =
                    SkillEngine::run_checks_assets(&skills, &self.engine_ctx.task_dir).await;
                if !report.passed {
                    tracing::warn!(
                        task_id = %self.engine_ctx.task_id,
                        summary = %report.summary,
                        "Converge Skill check failed (hard short-circuit) — returning Diverged"
                    );
                    let failed: Vec<String> = report
                        .results
                        .iter()
                        .filter(|r| !r.passed)
                        .map(|r| format!("{}: {}", r.check_id, r.detail))
                        .collect();
                    return Ok(ConvergenceDecision {
                        status: ConvergenceStatus::Diverged,
                        task_summary: format!(
                            "Converge Skill mechanical check failed: {}. Failures: [{}]",
                            report.summary,
                            failed.join("; ")
                        ),
                    });
                }
                (skills, Some(report))
            }
        } else {
            tracing::warn!(
                task_id = %self.engine_ctx.task_id,
                "YinConvergeAgent: guizang not wired — converge Skill layer skipped"
            );
            (Vec::new(), None)
        };

        // ── 收集工具（只读）：read + webfetch — 逐文件核验 deliverables、
        let skill_tools: Vec<Box<dyn rig::tool::ToolDyn>> = SkillRegistry::new(&self.engine_ctx.task_dir)
            .tools()
            .iter()
            .filter(|t| matches!(t.name(), "read" | "webfetch"))
            .map(|t| Box::new(t.clone()) as Box<dyn rig::tool::ToolDyn>)
            .collect();
        // V49 阴预算（BCP §8.19）：safety → limiter 组合，一次 .hook() 挂载。
        let limiter = ContextLimiter::new(
            self.context_limits.effective_handoff(),
            self.context_limits.effective_hard_cutoff(),
        );
        let hook_set = YinHookSet::new(self.safety_hook.as_ref().clone(), limiter.clone());
        let agent = client
            .agent(&self.model)
            .preamble(system_prompt)
            .max_tokens(1024u64)
            .default_max_turns(self.max_turns as usize)
            // V51 四象温度（AGENTS.md §3）：YinConverge 默认 0.2（低温稳定收敛）。
            .temperature(0.2)
            .hook(hook_set)
            .tools(skill_tools)
            .build();

        let mut input = serde_json::to_string_pretty(subtask_results).map_err(|e| {
            TaijiError::Serde(e)
        })?;

        // ── 注入 converge Skill 执行结果（LLM 裁决参考）──
        if let Some(ref report) = converge_skill_report {
            let results_summary: Vec<String> = report
                .results
                .iter()
                .map(|r| format!("[{}] {}: {}", if r.passed { "PASS" } else { "FAIL" }, r.check_id, r.detail))
                .collect();
            // 收集 llm_judgement 判据
            let criteria: Vec<String> = converge_skills
                .iter()
                .flat_map(|v| {
                    v.implementations
                        .iter()
                        .map(|i| (v.id.as_str(), i))
                        .collect::<Vec<_>>()
                })
                .filter(|(_, c)| c.kind == crate::types::verification::SkillKind::LlmJudgement)
                .map(|(sid, c)| format!("[{}] {}", sid, c.pass_condition))
                .collect();
            let section = if criteria.is_empty() {
                format!(
                    "\n\nSkillEngine converge report (mechanical checks — for reference):\n{}\n{}",
                    report.summary,
                    results_summary.join("\n")
                )
            } else {
                format!(
                    "\n\nSkillEngine converge report (mechanical checks — for reference):\n{}\n{}\n\nLlmJudgement criteria (your discretionary remit):\n{}\n\n反偏置指令：表面流畅不算数，必须引用子任务 deliverables 中的具体证据；禁止因篇幅长 / 风格好加分。",
                    report.summary,
                    results_summary.join("\n"),
                    criteria.join("\n")
                )
            };
            input.push_str(&section);
        }

        let response_result = agent.prompt(&input).await;

        // V49 阴溢出（BCP §8.19）：先于结果处理检查 limiter。
        if let Some(kind) = limiter.triggered() {
            match kind {
                LimitKind::Handoff => {
                    tracing::warn!(
                        task_id = %self.engine_ctx.task_id,
                        "YinConvergeAgent — context overflow → conservative Partial verdict"
                    );
                    return Ok(ConvergenceDecision {
                        status: ConvergenceStatus::Partial,
                        task_summary: "converge context_overflow".into(),
                    });
                }
                LimitKind::HardCutoff => {
                    return Err(TaijiError::HardCutoff {
                        threshold: self.context_limits.effective_hard_cutoff(),
                    });
                }
            }
        }

        let response = response_result.map_err(|e| {
            TaijiError::LLMCallFailed {
                context: format!("YinConvergeAgent LLM call failed: {e}"),
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
            "YinConvergeAgent — LLM convergence judgment completed"
        );

        Ok(decision)
    }
}

/// System prompt for the YinAgent in **converge · Orchestration** mode
/// (V27 阴阳配对：编排-收敛，编排节点的阴相位——判决子结果聚合)。
///
/// Aggregates subtask results of a recursive decomposition: coverage,
/// cross-subtask consistency, integration quality, finality.
const CONVERGE_ORC_SYSTEM_PROMPT: &str = r#"你是收敛判决器 — 编排收敛 (Convergence Judge · Orchestration).

任务以编排模式执行：拆解为多个子任务后汇聚结果。你的职责是判定
汇聚结果是否收敛。

## 判决维度
1. 目标达成——整体任务目标是否达成？
2. 覆盖——子任务结果是否集体覆盖了全部任务范围（MECE）？
3. 一致性——跨子任务结果是否相容（无矛盾）？
4. 整合——各子任务结果能否合并为连贯整体？
5. 终局性——综合产出是否代表完整答案？

## 文件核验
必须用 `read` 工具打开每个子任务的 deliverables 文件，逐文件验证：
跨子任务一致性、完整性、质量。不依赖摘要文本。

## 失败子任务处理
子任务结果可能含 `status: "Diverged"` 的失败条目及其交接产物。
- 失败可恢复且多数子任务成功 → 判 `Partial`
- 失败根本性且无进展 → 判 `Diverged`
- 在 `task_summary` 中明确写出哪个子任务失败、原因、是否值得 rerun

## 输出格式（严格 JSON）
{
  "status": "Converged" | "Partial" | "Diverged",
  "task_summary": "判决说明，含失败分析与 rerun 建议"
}

- "Converged": 全部维度覆盖，结果一致。
- "Partial":   部分缺口或矛盾，但已取得进展。
- "Diverged":  根本性不一致——拆解策略需修正。
"#;

/// System prompt for the YinAgent in **converge · Execution** mode
/// (V27 阴阳配对：执行-收敛，直接产出任务的收敛判决)。
///
/// A single direct output — judge whether it represents a complete,
/// final answer.
const CONVERGE_EXEC_SYSTEM_PROMPT: &str = r#"你是收敛判决器 — 执行收敛 (Convergence Judge · Execution).

任务以执行模式直接完成——由 L1 工具产出单一结果。你的职责是判定
该产出是否已收敛为完整答案。

## 判决维度
1. 目标达成——任务目标是否达成？
2. 完整性——产出是否完整覆盖，无遗漏维度？
3. 质量——交付物是否格式正确、可直接使用？
4. 终局性——产出是否为最终答案（非草稿或半成品）？

## 文件核验
必须用 `read` 工具打开 deliverables 文件，逐文件验证完整性与质量。
不依赖摘要文本。

## 输出格式（严格 JSON）
{
  "status": "Converged" | "Partial" | "Diverged",
  "task_summary": "判决说明"
}

- "Converged": 产出完整、最终。
- "Partial":   部分缺口，但已取得进展。
- "Diverged":  根本性不一致——产出不满足任务要求。
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

    // ── YinVerifyAgentBuilder tests ──────────────────────────────────

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_verify_returns_default_pass() {
        let builder = YinVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let report = builder.verify("YinAgent executed and verified the task output against all L4 Truth constraints.", &[], &MetaContext::empty()).await.expect("converge");
        assert_eq!(report.route, VerificationRoute::Pass);
    }

    #[tokio::test]
    async fn test_verify_empty_summary_triggers_back_to_meta() {
        // Empty summary should trigger the constraint pre-check for
        // truth:no-fabrication (hard).
        let builder = YinVerifyAgentBuilder::new(
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
        let builder = YinVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );

        let report = builder.verify("Adequate summary for audit.", &[], &MetaContext::empty()).await.expect("converge");
        assert_eq!(report.route, VerificationRoute::Pass);
    }

    // ── YinConvergeAgentBuilder tests ────────────────────────────────

    #[tokio::test]
    async fn test_converge_empty_results_converged() {
        let builder = YinConvergeAgentBuilder::new(
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
        let builder = YinConvergeAgentBuilder::new(
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
        let builder = YinConvergeAgentBuilder::new(
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
        let builder = YinConvergeAgentBuilder::new(
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
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("BackToZhouyi"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("BackToMeta"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("MECE"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("文件核验"));
        assert!(VERIFY_ORC_SYSTEM_PROMPT.contains("编排验证"));

        assert!(VERIFY_EXEC_SYSTEM_PROMPT.starts_with("你是因果验证器"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("Pass"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("BackToZhouyi"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("BackToMeta"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("执行验证"));
        assert!(VERIFY_EXEC_SYSTEM_PROMPT.contains("文件核验"));
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
    fn test_verify_system_prompt_max_turns_defensive_200() {
        let builder = YinVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );
        assert_eq!(builder.max_turns, 200, "V49: verify max_turns 降级为防御兜底 200");
    }

    #[test]
    fn test_converge_system_prompt_starts_with_chinese() {
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Converged"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Partial"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("Diverged"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("覆盖"));
        assert!(CONVERGE_ORC_SYSTEM_PROMPT.contains("编排收敛"));

        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Converged"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Partial"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("Diverged"));
        assert!(CONVERGE_EXEC_SYSTEM_PROMPT.contains("执行收敛"));
    }

    #[test]
    fn test_converge_system_prompt_max_turns_defensive_200() {
        let builder = YinConvergeAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        );
        assert_eq!(builder.max_turns, 200, "V49: converge max_turns 降级为防御兜底 200");
    }

    #[test]
    fn test_yin_builders_safety_hook_setters() {
        // 蓝图 V25 §8.5：Yin 相位带收集工具（read+webfetch）→ 必有安全钩子
        // （类型级保证，字段非 Option）；注入进程级单例后指针一致。
        let hook = Arc::new(SafetyHook::new(&SafetyConfig {
            enabled: false,
            trusted_mcp_servers: vec![],
        }));

        let verify_builder = YinVerifyAgentBuilder::new(
            make_engine_ctx("test-task"),
            Arc::new(
                ProviderRegistry::new(&make_config()).expect("ProviderRegistry"),
            ),
            "deepseek-chat",
        )
        .safety_hook(hook.clone());
        assert!(Arc::ptr_eq(&verify_builder.safety_hook, &hook));

        let converge_builder = YinConvergeAgentBuilder::new(
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
