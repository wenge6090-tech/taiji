//! 阴判断节点（V57）— "yin judgment, the yin phase"。
//!
//! V57 定论：阴不是 Agent——不持有 skill、不注册工具、不跑 SkillEngine、
//! 不持有 system prompt（资产层）。阴是**半符号半 LLM 的判断节点**，与元
//! 对称、顺序相反：
//!
//! - 元 = 半 LLM 半符号（入口，先语义后符号）
//! - 阴 = 半符号半 LLM（出口，先符号后语义）
//!
//! # 符号层（优先·恒在，LLM 不可翻案）
//! 1. **判断依据·逻辑层**：读归藏 `rules.yaml` type-level 规则（required/forbid
//!    清单，二值存在即生效——晶体归藏，无概率加权）→
//!    [`ConstraintEngine::load_truths`] + `check_yin_output`
//!    机械对碰阳的产出，hard 违反 → `BackToMeta`（认知偏差）。
//! 2. **判断依据·因果层**：读归藏 `relations.yaml` type→type 边 →
//!    [`ConstraintEngine::match_relations`] 产出因果依赖先验，注入 LLM 兜底
//!    prompt（MVP：实体到产出的机械链接延后，因果先验注入而非机械裁决）。
//! 3. **运行保障**：[`check_atomics`] 无条件恒在的 Rust 原子判据
//!    （file-exists/schema-valid/reference-resolves/trace-consistency），
//!    保证系统 invariant（产出真实、任务册合法、引用解析、证据可追溯），
//!    hard 失败 → `BackToZhouyi`（执行偏差）。
//!
//! # LLM 层（兜底，唯一 LLM 介入点）
//! 只在符号层无法表达的语义判断处介入（「产出是否真满足任务意图」）。
//! **不注册工具**（read/webfetch 移除）——文件级核验由符号层原子判据已覆盖，
//! 语义裁决是纯文本判断。预算由 [`ContextLimiter`] 承担（§14 阴预算对称）。
//!
//! # 约束
//! - `max_turns = 200`（防死循环兜底，预算由 ContextLimiter 承担）。
//! - 归藏必接（`Arc<GuizangClient>`）——阴读因果是判断依据，归藏不可用是系统错误。

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::deepseek;

use crate::hooks::context_limiter::{ContextLimiter, LimitKind};
use crate::infra::config::ContextLimits;
use crate::infra::error::TaijiError;
use crate::infra::json_util::parse_llm_json;
use crate::infra::knowledge::GuizangClient;
use crate::infra::provider::ProviderRegistry;
use crate::infra::trace::save_json_atomic;
use crate::orchestration::constraint_engine::{check_atomics, ConstraintEngine};
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;
use crate::types::task::DecomposeResult;
use crate::types::verification::{
    CheckResult, ConstraintSeverity, ConvergenceDecision, ConvergenceStatus,
    VerificationReport, VerificationRoute,
};

// ---------------------------------------------------------------------------
// YinJudge — 阴判断节点（半符号半 LLM）
// ---------------------------------------------------------------------------

/// 阴判断节点（V57）：半符号半 LLM 的判断节点，非 Agent。
///
/// 符号层（归藏因果 rules/relations + Rust 原子判据）优先恒在；LLM 层只在
/// 符号层无法表达的语义判断处兜底（无工具、不做概率采样执行）。隔离由结构
/// 保证（无工具注册面），不靠工具注册面隔离。
///
/// Created by [`AgentFactory::create_yin_judge`].
pub struct YinJudge {
    engine_ctx: EngineContext,
    model: String,
    provider: Arc<ProviderRegistry>,
    /// V36 模型路由：provider 名（MetaContext.model 解析结果；默认 deepseek）。
    provider_name: String,
    /// V49 阴预算（AGENTS.md §14）：窗口阈值来源（30% 交接 / 35% 硬截止）。
    context_limits: ContextLimits,
    /// 归藏客户端（读因果——判断依据）。必接（V57 无降级：归藏不可用是系统错误）。
    guizang: Arc<GuizangClient>,
    max_turns: u32,
}

impl YinJudge {
    /// Create a new [`YinJudge`].
    ///
    /// Normally called from [`AgentFactory::create_yin_judge`] — external
    /// callers should use the factory rather than constructing this directly.
    pub fn new(
        engine_ctx: EngineContext,
        provider: Arc<ProviderRegistry>,
        provider_name: &str,
        model: &str,
        guizang: Arc<GuizangClient>,
        context_limits: ContextLimits,
    ) -> Self {
        Self {
            engine_ctx,
            model: model.to_string(),
            provider,
            provider_name: provider_name.to_string(),
            context_limits,
            guizang,
            max_turns: 200,
        }
    }

    /// 因果验证（阴判断节点·verify）：半符号半 LLM。
    ///
    /// # Logic
    /// 1. **符号层·逻辑层**：`load_truths`（只 Rust 宪法，V62）+ `check_yin_output`
    ///    约束预检。hard 违反 → `BackToMeta`（零 LLM）。
    ///    挖掘规则/人工条文已降经验层（只进 LLM 兑底措辞，不机械对碰）。
    /// 2. **符号层·运行保障**：`check_atomics` 原子判据（无条件恒在）。
    ///    hard 失败 → `BackToZhouyi`（零 LLM）。
    /// 3. **LLM 层**：符号层通过 → 语义裁决兜底（无工具，纯文本）。
    pub async fn verify(
        &self,
        task_output: &str,
        tool_results: &[String],
        meta_ctx: &MetaContext,
    ) -> Result<VerificationReport, TaijiError> {
        // ── 符号层·逻辑层：约束预检（只 Rust 宪法，V62 分层）──
        let constraints = ConstraintEngine::load_truths(&meta_ctx.task_type_tags);
        let pre_check = ConstraintEngine::check_yin_output(task_output, tool_results, &constraints);

        if !pre_check.passed {
            let has_hard = pre_check
                .violations
                .iter()
                .any(|v| v.severity == ConstraintSeverity::Hard);

            if has_hard {
                tracing::warn!(
                    task_id = %self.engine_ctx.task_id,
                    violations = ?pre_check.violations,
                    "YinJudge — hard constraint violation → BackToMeta"
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
                    hodge: None,
                });
            }
        }

        // ── 符号层·运行保障：原子判据（无条件恒在）──
        let (atomic_results, hard_failed) = check_atomics(&self.engine_ctx.task_dir).await;
        if hard_failed {
            let failed: Vec<String> = atomic_results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| format!("{}: {}", r.check_id, r.detail))
                .collect();
            tracing::warn!(
                task_id = %self.engine_ctx.task_id,
                "YinJudge — atomic invariant failed → BackToZhouyi"
            );
            return Ok(VerificationReport {
                route: VerificationRoute::BackToZhouyi,
                confidence: 1.0,
                summary: format!("Atomic invariant check failed: {}", failed.join("; ")),
                constraint_violations: failed,
                hodge: None,
            });
        }

        // ── LLM 层：语义裁决兜底 ──
        let mut report = self
            .llm_verify(task_output, tool_results, meta_ctx, &pre_check, &atomic_results)
            .await?;

        // ── V65 Hodge 三模态病理诊断（纯符号零 LLM，软信号）──
        match self.hodge_assets_text().await {
            Ok(assets_text) => {
                report.hodge = Some(crate::orchestration::constraint_engine::hodge_diagnose(
                    &self.engine_ctx.task_dir,
                    task_output,
                    &assets_text,
                ).await);
            }
            Err(e) => {
                tracing::warn!(error = %e, "YinJudge — hodge assets load failed, diagnosis skipped");
            }
        }
        Ok(report)
    }

    /// V65：归藏资产文本（调和分量的相似度基准）——prompt 描述 + skill 摘要。
    async fn hodge_assets_text(&self) -> Result<Vec<String>, TaijiError> {
        let mut texts = Vec::new();
        let prompts = self.guizang.load_all_prompts().await?;
        for p in prompts {
            texts.push(format!("{} {}", p.name, p.description));
        }
        let mut seen_skill_ids = std::collections::HashSet::new();
        for category in [
            crate::types::verification::SkillCategory::Exec,
            crate::types::verification::SkillCategory::Orch,
            crate::types::verification::SkillCategory::Verify,
            crate::types::verification::SkillCategory::Converge,
        ] {
            match crate::infra::skill_catalog::load_skill_catalog(
                &self.guizang,
                category,
                crate::infra::skill_catalog::ToolProfile::Full,
            )
            .await
            {
                Ok(skills) => {
                    for s in skills {
                        if seen_skill_ids.insert(s.id.clone()) {
                            texts.push(format!("{} {}", s.name, s.summary));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "YinJudge — hodge skill catalog load failed for {category:?}");
                }
            }
        }
        Ok(texts)
    }

    /// LLM 语义裁决兜底（唯一 LLM 介入点，无工具）。
    async fn llm_verify(
        &self,
        task_output: &str,
        tool_results: &[String],
        meta_ctx: &MetaContext,
        pre_check: &crate::types::verification::ConstraintResult,
        atomic_results: &[CheckResult],
    ) -> Result<VerificationReport, TaijiError> {
        // 符号层·因果层：relations 因果先验（注入 prompt，不机械裁决）
        let relations = self.guizang.load_relations().await?;
        let causal_hints = ConstraintEngine::match_relations(&meta_ctx.ontology_objects, &relations);

        // V62 经验层：rules.yaml 条文（挖掘/人工）只进兑底措辞，不机械对碰
        let rules = self.guizang.load_rules().await?;
        let empirical_rules: Vec<String> = rules
            .iter()
            .map(|r| {
                let mut s = format!("[经验条文] {}", r.id);
                if !r.require.is_empty() {
                    s.push_str(&format!(" 通常需要: {}", r.require.join(",")));
                }
                if !r.forbid.is_empty() {
                    s.push_str(&format!(" 通常避免: {}", r.forbid.join(",")));
                }
                s
            })
            .collect();

        // Soft violations 注入
        let soft_context: Vec<String> = pre_check
            .violations
            .iter()
            .filter(|v| v.severity == ConstraintSeverity::Soft)
            .map(|v| format!("[Soft] {}: {}", v.truth_name, v.reason))
            .collect();

        // 原子判据结果注入（机械已通过，供语义裁决参考）
        let atomic_summary: Vec<String> = atomic_results
            .iter()
            .map(|r| {
                format!(
                    "[{}] {}: {}",
                    if r.passed { "PASS" } else { "FAIL" },
                    r.check_id,
                    r.detail
                )
            })
            .collect();

        let client: Arc<deepseek::Client> = self.provider.client_for(&self.provider_name)?;

        // V49 阴预算（AGENTS.md §14）：仅 ContextLimiter（无工具 → 无需 SafetyHook）。
        let limiter = ContextLimiter::new(
            self.context_limits.effective_handoff(),
            self.context_limits.effective_hard_cutoff(),
        );
        let agent = client
            .agent(&self.model)
            .preamble(VERIFY_FALLBACK_PROMPT)
            .max_tokens(1024u64)
            .default_max_turns(self.max_turns as usize)
            // V51 四象温度：阴 verify 0.2（低温稳定验证）。
            .temperature(0.2)
            .hook(limiter.clone())
            .build();

        let input = format!(
            "Task output:\n{task_output}\n\nTool results:\n{results}\n\nSoft violations:\n{soft}\n\nCausal priors (归藏因果):\n{causal}\n\nEmpirical rules (经验条文, advisory):\n{emp}\n\nAtomic checks (mechanical, cannot override):\n{atomic}",
            task_output = task_output,
            results = tool_results.join("\n---\n"),
            soft = if soft_context.is_empty() {
                "None".to_string()
            } else {
                soft_context.join("\n")
            },
            causal = if causal_hints.is_empty() {
                "None".to_string()
            } else {
                causal_hints.join("\n")
            },
            emp = if empirical_rules.is_empty() {
                "None".to_string()
            } else {
                empirical_rules.join("\n")
            },
            atomic = if atomic_summary.is_empty() {
                "None".to_string()
            } else {
                atomic_summary.join("\n")
            },
        );

        let response_result = agent.prompt(&input).await;

        // V49 阴溢出：先于结果处理检查 limiter。
        if let Some(kind) = limiter.triggered() {
            match kind {
                LimitKind::Handoff => {
                    tracing::warn!(
                        task_id = %self.engine_ctx.task_id,
                        "YinJudge — context overflow → conservative BackToZhouyi verdict"
                    );
                    return Ok(VerificationReport {
                        route: VerificationRoute::BackToZhouyi,
                        confidence: 0.0,
                        summary: "verify context_overflow".into(),
                        constraint_violations: vec![],
                        hodge: None,
                    });
                }
                LimitKind::HardCutoff => {
                    return Err(TaijiError::HardCutoff {
                        threshold: self.context_limits.effective_hard_cutoff(),
                    });
                }
            }
        }

        let response = response_result.map_err(|e| TaijiError::LLMCallFailed {
            context: format!("YinJudge LLM call failed: {e}"),
        })?;

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
            "YinJudge — LLM semantic verdict completed"
        );

        // ── Persist verify state for crash recovery ──
        let verify_state = serde_json::json!({
            "report": &report,
            "round": self.engine_ctx.round,
            "cycle": self.engine_ctx.cycle,
            "checks": atomic_results,
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

    /// 收敛判定（阴判断节点·converge）：半符号半 LLM。
    ///
    /// # Logic
    /// 1. 空子任务结果 → `Converged`（机械短路）。
    /// 2. 全部子任务 `Diverged` → `Diverged`（机械短路）。
    /// 3. 其他 → LLM 语义兜底。
    pub async fn converge(
        &self,
        subtask_results: &[DecomposeResult],
        meta_ctx: &MetaContext,
    ) -> Result<ConvergenceDecision, TaijiError> {
        let total = subtask_results.len();

        // ── 符号层：空结果短路 ──
        if total == 0 {
            return Ok(ConvergenceDecision {
                status: ConvergenceStatus::Converged,
                task_summary: format!(
                    "Task {} has no subtasks — trivially converged",
                    self.engine_ctx.task_id
                ),
            });
        }

        // ── 符号层：全 Diverged 短路 ──
        if subtask_results
            .iter()
            .all(|r| r.status == ConvergenceStatus::Diverged)
        {
            tracing::warn!(
                task_id = %self.engine_ctx.task_id,
                "YinJudge — all subtasks diverged → Diverged (mechanical)"
            );
            return Ok(ConvergenceDecision {
                status: ConvergenceStatus::Diverged,
                task_summary: format!("all {total} subtasks diverged — decomposition strategy failed"),
            });
        }

        // ── LLM 层：语义兜底 ──
        self.llm_converge(subtask_results, meta_ctx).await
    }

    /// LLM 收敛语义兜底（无工具）。
    async fn llm_converge(
        &self,
        subtask_results: &[DecomposeResult],
        _meta_ctx: &MetaContext,
    ) -> Result<ConvergenceDecision, TaijiError> {
        let client: Arc<deepseek::Client> = self.provider.client_for(&self.provider_name)?;

        let limiter = ContextLimiter::new(
            self.context_limits.effective_handoff(),
            self.context_limits.effective_hard_cutoff(),
        );
        let agent = client
            .agent(&self.model)
            .preamble(CONVERGE_FALLBACK_PROMPT)
            .max_tokens(1024u64)
            .default_max_turns(self.max_turns as usize)
            .temperature(0.2)
            .hook(limiter.clone())
            .build();

        let input = serde_json::to_string_pretty(subtask_results).map_err(TaijiError::Serde)?;

        let response_result = agent.prompt(&input).await;

        if let Some(kind) = limiter.triggered() {
            match kind {
                LimitKind::Handoff => {
                    tracing::warn!(
                        task_id = %self.engine_ctx.task_id,
                        "YinJudge converge — context overflow → conservative Partial verdict"
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

        let response = response_result.map_err(|e| TaijiError::LLMCallFailed {
            context: format!("YinJudge converge LLM call failed: {e}"),
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
            subtasks = subtask_results.len(),
            status = ?decision.status,
            "YinJudge — LLM convergence verdict completed"
        );

        Ok(decision)
    }
}

/// LLM 语义裁决兜底 prompt（V57：阴不持有资产层 system prompt——硬编码宪法，
/// 不参与主动学习，与元层宪法同构）。
const VERIFY_FALLBACK_PROMPT: &str = r#"你是因果验证器 — 语义裁决兜底（阴判断节点）。

符号层（归藏因果 rules/relations + Rust 原子判据）已完成机械对碰且通过。
你的职责是符号层无法表达的语义判断——产出是否真正满足任务意图、覆盖完整、
内容可用。这是你唯一的裁决空间，符号层结果不可翻案。

## 裁决维度
1. 需求满足——产出是否完整覆盖任务描述？
2. 产物质量——交付物是否格式正确、内容可用？
3. 完整性——任务是否被完整处理，无遗漏维度？

## 反偏置
表面流畅不算数，必须引用具体证据；禁止因篇幅长 / 风格好加分。

## 输出格式（严格 JSON）
{
  "route": "Pass" | "BackToZhouyi" | "BackToMeta",
  "confidence": 0.0..1.0,
  "summary": "判定依据简述",
  "constraint_violations": ["违规项描述"]
}

- "Pass": 产出满足需求，可交付。
- "BackToZhouyi": 执行偏差——产出存在但质量/完整性不足，需重试执行。
- "BackToMeta": 认知偏差——任务规格或方法本身有问题，需重新权重更新。
"#;

/// LLM 收敛语义兜底 prompt（V57 硬编码）。
const CONVERGE_FALLBACK_PROMPT: &str = r#"你是收敛判决器 — 语义裁决兜底（阴判断节点）。

符号层已完成机械判定（空结果 → Converged；全失败 → Diverged）。你的职责是
符号层无法表达的语义判断：子任务结果聚合是否收敛为完整答案。

## 判决维度
1. 目标达成——整体任务目标是否达成？
2. 覆盖——子任务结果是否集体覆盖全部任务范围？
3. 一致性——跨子任务结果是否相容（无矛盾）？
4. 终局性——综合产出是否代表完整答案？

## 失败子任务处理
子任务结果可能含 status: "Diverged" 的失败条目。多数成功 → Partial；
根本性失败且无进展 → Diverged。在 task_summary 中写出失败原因与 rerun 建议。

## 输出格式（严格 JSON）
{
  "status": "Converged" | "Partial" | "Diverged",
  "task_summary": "判决说明"
}
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config::{LlmConfig, SafetyConfig, TaijiConfig};

    static DIR_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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

    /// 测试辅助：创建稀疏归藏（唯一临时目录，末尾清理）。
    async fn make_judge(task_id: &str) -> (YinJudge, std::path::PathBuf) {
        let n = DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("taiji-yin-test-{}-{n}", std::process::id()));
        let guizang = Arc::new(GuizangClient::new_sparse(&dir).await.expect("guizang"));
        let config = make_config();
        let judge = YinJudge::new(
            make_engine_ctx(task_id),
            Arc::new(ProviderRegistry::new(&config).expect("ProviderRegistry")),
            "deepseek",
            "deepseek-chat",
            guizang,
            ContextLimits::default(),
        );
        (judge, dir)
    }

    // ── YinJudge verify tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_verify_empty_summary_triggers_back_to_meta() {
        // Empty summary violates truth:no-fabrication (hard) → BackToMeta（零 LLM）。
        let (judge, dir) = make_judge("test-task").await;
        let report = judge
            .verify("", &[], &MetaContext::empty())
            .await
            .expect("verify");
        assert_eq!(report.route, VerificationRoute::BackToMeta);
        assert!(!report.constraint_violations.is_empty());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_verify_atomic_hard_failure_short_circuits() {
        // 原子判据（file-exists）hard 失败（无 deliverables）→ BackToZhouyi（零 LLM）。
        let (judge, dir) = make_judge("test-task-no-deliverables").await;
        let report = judge
            .verify(
                "Adequate summary for audit purposes.",
                &[],
                &MetaContext::empty(),
            )
            .await
            .expect("verify");
        assert_eq!(report.route, VerificationRoute::BackToZhouyi);
        assert!(report.summary.contains("Atomic invariant"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ── YinJudge converge tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_converge_empty_results_converged() {
        let (judge, dir) = make_judge("test-task").await;
        let decision = judge
            .converge(&[], &MetaContext::empty())
            .await
            .expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Converged);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_converge_all_diverged_short_circuits() {
        // 全 Diverged → Diverged（机械短路，零 LLM）。
        let (judge, dir) = make_judge("test-task").await;
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
        let decision = judge
            .converge(&results, &MetaContext::empty())
            .await
            .expect("converge");
        assert_eq!(decision.status, ConvergenceStatus::Diverged);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ── Prompt tests ────────────────────────────────────────────────

    #[test]
    fn test_fallback_prompts_start_with_chinese() {
        assert!(VERIFY_FALLBACK_PROMPT.starts_with("你是因果验证器"));
        assert!(VERIFY_FALLBACK_PROMPT.contains("Pass"));
        assert!(VERIFY_FALLBACK_PROMPT.contains("BackToZhouyi"));
        assert!(VERIFY_FALLBACK_PROMPT.contains("BackToMeta"));
        assert!(CONVERGE_FALLBACK_PROMPT.starts_with("你是收敛判决器"));
        assert!(CONVERGE_FALLBACK_PROMPT.contains("Converged"));
        assert!(CONVERGE_FALLBACK_PROMPT.contains("Diverged"));
    }

    #[tokio::test]
    async fn test_yin_judge_max_turns_defensive_200() {
        // V49：max_turns 降级为防死循环兜底 200。
        let (judge, dir) = make_judge("test-task").await;
        assert_eq!(judge.max_turns, 200);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
