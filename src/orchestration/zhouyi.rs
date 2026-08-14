//! ZhouyiCycle — Reusable Zhouyi three-phase cycle (元·阳·阴 → loop).
//!
//! Extracted from [`RecursiveRunner`] so that both root tasks and recursive
//! child tasks share the same Zhouyi execution logic, satisfying the **isomorphic
//! recursion** principle (§1.1 of BCP).
//!
//! # Architecture (BCP §5)
//!
//! ```text
//! MetaAgent (权重更新·元)   ─── once at entrance (or from parent MetaContext)
//!     ↓
//! Zhouyi loop (max_cycles × max_rounds):
//!     YangAgent (概率拟合·阳)   →  LLM exploration + tools + recursion
//!     YinAgent  (因果验证·阴)   →  constraint check + LLM verdict
//!     ├─ PASS        → return ZhouyiResult
//!     ├─ BACK_TO_ZHOUYI → round++ (retry YangAgent only)
//!     └─ BACK_TO_META → cycle++, round=0 (re-run MetaAgent)
//! ```
//!
//! # Crash Recovery & Subtask Resumption
//!
//! The cycle persists progress to `checkpoint.json` after each completed phase.
//! On restart (crash recovery) the cycle skips already-passed phases.
//! When `resume_history` is provided (parent-initiated subtask re-run),
//! checkpoint is ignored and the conversation history is used directly.

use std::path::Path;
use std::sync::Arc;

use rig::completion::Message;
use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::infra::trace::{load_json_optional, save_json_atomic};
use crate::orchestration::event_bus;
use crate::types::agent::{AssetRef, MetaContext, MetaOutcome};
use crate::types::execution::EngineContext;
use crate::types::frontend::{ZhouyiPhase, YinIntervention};
use crate::types::task::{Checkpoint, CyclePhase, DecomposeResult, Task, TaskStatus, ZhouyiResult};
use crate::types::verification::{
    CheckResult, ConvergenceStatus, VerificationReport, VerificationRoute,
};
use crate::ws::types::TaskEvent;

/// Reusable Zhouyi cycle that executes the three-phase loop for a task at any
/// recursion depth.
///
/// # Usage
///
/// - **Root task**: execute with `initial_meta_ctx = None`, `resume_history = None`.
///   The cycle runs MetaAgent automatically to extract reasoning paths.
///
/// - **Child task (first run)**: execute with `initial_meta_ctx = Some(parent_meta_ctx)`.
///   The cycle skips the initial MetaAgent and uses the parent's context as
///   the reasoning bias.  MetaAgent is still re-run on `BACK_TO_META`.
///
/// - **Child task (re-run)**: execute with `resume_history = Some(history)`.
///   The cycle skips MetaAgent entirely, ignores checkpoint, and feeds the
///   provided history directly to YangAgent for context-continuity.
pub struct ZhouyiCycle {
    factory: Arc<AgentFactory>,
    config: TaijiConfig,
    /// Cancellation token propagated to all sub-agents.
    /// Root token created in [`RecursiveRunner`]; child tokens linked via
    /// `CancellationToken::child_token()` in `RecursiveDecomposeTool`.
    cancel: CancellationToken,
}

impl ZhouyiCycle {
    /// Create a new `ZhouyiCycle` with a cancellation token.
    pub fn new(
        factory: Arc<AgentFactory>,
        config: TaijiConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self { factory, config, cancel }
    }

    /// Execute the full Zhouyi cycle.
    ///
    /// # Parameters
    ///
    /// - `description` — task description passed to YangAgent.
    /// - `initial_meta_ctx` — if `Some`, skip the initial MetaAgent run.
    /// - `engine_ctx` — mutable engine context; `round`/`cycle` counters are
    ///   updated in-place for retry tracking.
    /// - `resume_history` — if `Some`, skip MetaAgent + checkpoint, use history
    ///   directly (parent-initiated subtask re-run).  `None` for fresh execution.
    ///
    /// # Status management (V26 统一)
    /// 根任务与子任务走同一路径：入口原子写 `meta.json` status=Running，每个
    /// 阶段结束随 checkpoint 更新 status，PASS 写 Completed，失败/取消写
    /// Failed/Cancelled（`save_json_atomic`）。`resume_task_id` 恢复复用同一
    /// `task_id`，恢复链（resume_history > decompose_result.json > checkpoint.json）
    /// 对根任务同样生效。
    ///
    /// # Returns
    ///
    /// `Ok(ZhouyiResult)` on PASS, else `Err(TaijiError)` on exhaustion or
    /// unrecoverable failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        description: &str,
        initial_meta_ctx: Option<MetaContext>,
        engine_ctx: &mut EngineContext,
        resume_history: Option<Vec<Message>>,
    ) -> Result<ZhouyiResult, TaijiError> {
        let result = self
            .execute_inner(description, initial_meta_ctx, engine_ctx, resume_history)
            .await;

        // ── 统一 status 终态落盘（V26）：成功路径写 Completed，取消写
        //    Cancelled，其余失败写 Failed —— 根/子任务共用，runner 纯薄壳 ──
        match &result {
            Ok(_) => {
                let _ = write_task_status(
                    &engine_ctx.task_dir,
                    &engine_ctx.task_id,
                    description,
                    engine_ctx.depth,
                    TaskStatus::Completed,
                );
            }
            Err(TaijiError::Cancelled { .. }) => {
                let _ = write_task_status(
                    &engine_ctx.task_dir,
                    &engine_ctx.task_id,
                    description,
                    engine_ctx.depth,
                    TaskStatus::Cancelled,
                );
            }
            Err(_) => {
                let _ = write_task_status(
                    &engine_ctx.task_dir,
                    &engine_ctx.task_id,
                    description,
                    engine_ctx.depth,
                    TaskStatus::Failed,
                );
            }
        }

        result
    }

    /// Internal implementation of the Zhouyi loop (see [`execute`]).
    async fn execute_inner(
        &self,
        description: &str,
        initial_meta_ctx: Option<MetaContext>,
        engine_ctx: &mut EngineContext,
        resume_history: Option<Vec<Message>>,
    ) -> Result<ZhouyiResult, TaijiError> {
        let checkpoint_path = engine_ctx.task_dir.join("checkpoint.json");
        let decompose_result_path = engine_ctx.task_dir.join("decompose_result.json");

        // ── Entry: determine start state ───────────────────────────────
        //
        // Priority: resume_history > decompose_result.json > checkpoint.json
        let mut meta_ctx: MetaContext;
        let mut current_description = description.to_string();

        // Load chat_history: either from resume_history, or from file.
        let resume_history_is_some = resume_history.is_some();
        let mut chat_history: Vec<Message> = if let Some(history) = resume_history {
            history
        } else {
            load_chat_history_or_empty(&engine_ctx.task_dir)
        };

        if !chat_history.is_empty() {
            tracing::debug!(
                task_id = %engine_ctx.task_id,
                msg_count = chat_history.len(),
                "Loaded chat_history with {} messages",
                chat_history.len()
            );
        }

        // ── Decide entry path ──
        //
        // Three entry modes:
        //   1. Parent-initiated re-run (resume_history.is_some()) → skip MetaAgent,
        //      skip checkpoint, use provided history directly.
        //   2. Crash recovery (checkpoint.json exists, no decompose_result.json)
        //      → jump to the last persisted phase.
        //   3. Fresh execution (no resume_history, no checkpoint) → full pipeline.

        let (mut resume_phase, is_crash_recovery) = if resume_history_is_some {
            // Parent-initiated re-run: history already loaded above.
            tracing::debug!(
                task_id = %engine_ctx.task_id,
                "Resume mode: parent-initiated re-run (resume_history provided)"
            );
            meta_ctx = match initial_meta_ctx {
                Some(ref ctx) => ctx.clone(),
                None => MetaContext::empty(),
            };
            // V27 深度规则兑底：parent-initiated re-run 同样适用。
            apply_leaf_depth_rule(&mut meta_ctx, engine_ctx.depth, self.config.runtime.max_depth);
            (None, false)
        } else if let Ok(Some(checkpoint)) = load_json_optional::<Checkpoint>(&checkpoint_path) {
            // Check if decompose_result.json exists → task already completed.
            if let Ok(Some(_)) = load_json_optional::<ZhouyiResult>(&decompose_result_path) {
                tracing::info!(
                    task_id = %engine_ctx.task_id,
                    phase = ?checkpoint.phase,
                    "Task already completed (decompose_result.json exists) — returning cached"
                );
                return load_json_optional::<ZhouyiResult>(&decompose_result_path)
                    .ok()
                    .flatten()
                    .ok_or_else(|| TaijiError::Other(
                        "decompose_result.json existed but failed to re-read".into(),
                    ));
            }

            // Crash recovery: jump to the appropriate phase.
            engine_ctx.round = checkpoint.round;
            engine_ctx.cycle = checkpoint.cycle;
            tracing::warn!(
                task_id = %engine_ctx.task_id,
                phase = ?checkpoint.phase,
                round = engine_ctx.round,
                cycle = engine_ctx.cycle,
                "Crash recovery — resuming from checkpoint phase"
            );

            // Load meta_ctx from meta_ctx.json (written after MetaAgent completes).
            meta_ctx = match load_json_optional::<MetaContext>(
                &engine_ctx.task_dir.join("meta_ctx.json"),
            ) {
                Ok(Some(ctx)) => ctx,
                _ => {
                    tracing::warn!(
                        task_id = %engine_ctx.task_id,
                        "meta_ctx.json not found — falling back to initial_meta_ctx or empty"
                    );
                    initial_meta_ctx.clone().unwrap_or_else(MetaContext::empty)
                }
            };
            // V27 深度规则兑底：崩溃恢复路径同样适用。
            apply_leaf_depth_rule(&mut meta_ctx, engine_ctx.depth, self.config.runtime.max_depth);

            (Some(checkpoint.phase), true)
        } else {
            // Fresh execution.
            meta_ctx = MetaContext::empty(); // placeholder; may be overwritten below
            // If initial_meta_ctx is Some, we keep it for Phase 1 skip.
            (None, false)
        };

        // ── V26 状态统一管理：入口写 Running（根/子同构；子任务目录在
        //    recursive_decompose 中创建，此处补写 meta.json）──
        if let Err(e) = write_task_status(
            &engine_ctx.task_dir,
            &engine_ctx.task_id,
            description,
            engine_ctx.depth,
            TaskStatus::Running,
        ) {
            tracing::warn!(
                task_id = %engine_ctx.task_id,
                error = %e,
                "Failed to write Running status"
            );
        }

        // ── Phase 1: MetaAgent (权重更新·元) ──
        //
        // Run MetaAgent when ALL of these are true:
        //   1. Not a parent-initiated re-run
        //   2. Not crash recovery from YangDone or YinDone
        //   3. initial_meta_ctx is None (no parent context provided)
        let needs_meta = resume_phase.is_none()
            && !is_crash_recovery
            && initial_meta_ctx.is_none()
            && !resume_history_is_some;

        if needs_meta {
            tracing::debug!(
                task_id = %engine_ctx.task_id,
                "Running MetaAgent for initial reasoning-path extraction"
            );
            event_bus::emit_event(TaskEvent::PhaseChanged {
                task_id: engine_ctx.task_id.clone(),
                phase: ZhouyiPhase::Meta,
            });
            let meta_agent = self
                .factory
                .create_meta_agent(&engine_ctx.task_id, engine_ctx.depth, self.config.runtime.max_depth)?;
            // 首次运行无前一瞬态产出（handoff=None）。
            match meta_agent.run(description, &["general"], None).await? {
                // V46 短路（BCP §8.8）：应答类任务直接产出，跳过阳阴。
                MetaOutcome::Answer(answer) => {
                    let answer_path =
                        write_short_circuit_answer(&engine_ctx.task_dir, &answer).await?;
                    tracing::info!(
                        task_id = %engine_ctx.task_id,
                        "MetaAgent answer short-circuit → PASS (跳过阳阴)"
                    );
                    write_task_status(
                        &engine_ctx.task_dir,
                        &engine_ctx.task_id,
                        description,
                        engine_ctx.depth,
                        TaskStatus::Completed,
                    )?;
                    return Ok(ZhouyiResult {
                        task_id: engine_ctx.task_id.clone(),
                        content: answer,
                        tools_used: vec![],
                        deliverables: vec![answer_path],
                        depth: engine_ctx.depth,
                        rounds: 0,
                    });
                }
                MetaOutcome::Context(ctx) => {
                    meta_ctx = ctx;

                    // V27 深度规则兑底：叶节点（depth+1 >= max_depth）强制 Execution。
                    apply_leaf_depth_rule(&mut meta_ctx, engine_ctx.depth, self.config.runtime.max_depth);

                    // Persist MetaContext for crash recovery.
                    persist_meta_ctx(&meta_ctx, &engine_ctx.task_dir);

                    // Write checkpoint after MetaAgent.
                    write_checkpoint(&checkpoint_path, CyclePhase::MetaDone, engine_ctx, &self.cancel);
                }
            }
        } else if !resume_history_is_some && !is_crash_recovery {
            // Use parent-provided MetaContext.
            if let Some(ctx) = initial_meta_ctx {
                meta_ctx = ctx;
                // V27 深度规则兑底：父层分配的 mode 在叶节点强制 Execution。
                apply_leaf_depth_rule(&mut meta_ctx, engine_ctx.depth, self.config.runtime.max_depth);
                // Persist immediately so crash recovery never loses the parent context.
                persist_meta_ctx(&meta_ctx, &engine_ctx.task_dir);
            }
        }

        // ── Phases 2-4: Zhouyi loop ──────────────────────────────────────
        loop {
            // Check cancellation before each iteration.
            if self.cancel.is_cancelled() {
                return Err(TaijiError::Cancelled {
                    context: format!("Zhouyi cycle cancelled for task {}", engine_ctx.task_id),
                });
            }

            // ── Phase 2: YangAgent (概率拟合·阳) ──
            //
            // Skip if crash recovery from YangDone or YinDone.
            event_bus::emit_event(TaskEvent::PhaseChanged {
                task_id: engine_ctx.task_id.clone(),
                phase: ZhouyiPhase::Yang,
            });
            let yang_result = if resume_phase == Some(CyclePhase::YangDone)
                || resume_phase == Some(CyclePhase::YinDone)
            {
                // Crash recovery: YangAgent already ran, reconstruct its
                // result from persisted state. V28 产出继承优先（handoff /
                // deliverables），chat_history 仅本节点兜底（§1.4 / §8.18）。
                // If we can't reconstruct, re-run.
                match construct_zhouyi_result_from_state(&engine_ctx) {
                    Ok(Some(result)) => result,
                    _ => {
                        tracing::warn!(
                            task_id = %engine_ctx.task_id,
                            "Could not reconstruct YangAgent result from state — re-running"
                        );
                        match run_yang_with_v28_routing(
                            &self.factory,
                            engine_ctx,
                            &meta_ctx,
                            self.cancel.clone(),
                            &mut chat_history,
                            &mut current_description,
                            self.config.runtime.max_rounds,
                        )
                        .await?
                        {
                            YangOutcome::Success(result) => result,
                            YangOutcome::BackToZhouyi => continue,
                            YangOutcome::BackToMeta => {
                                match rerun_meta(
                                    &self.factory,
                                    engine_ctx,
                                    &mut meta_ctx,
                                    &self.cancel,
                                    &mut chat_history,
                                    &mut current_description,
                                    description,
                                    &checkpoint_path,
                                    "上下文超限（执行模式）→ 元重判编排".to_string(),
                                )
                                .await?
                                {
                                    Some(result) => return Ok(result),
                                    None => continue,
                                }
                            }
                        }
                    }
                }
            } else {
                match run_yang_with_v28_routing(
                    &self.factory,
                    engine_ctx,
                    &meta_ctx,
                    self.cancel.clone(),
                    &mut chat_history,
                    &mut current_description,
                    self.config.runtime.max_rounds,
                )
                .await?
                {
                    YangOutcome::Success(result) => {
                        // Write checkpoint after YangAgent (chat_history already saved internally).
                        write_checkpoint(
                            &checkpoint_path,
                            CyclePhase::YangDone,
                            engine_ctx,
                            &self.cancel,
                        );
                        result
                    }
                    YangOutcome::BackToZhouyi => continue,
                    YangOutcome::BackToMeta => {
                        match rerun_meta(
                            &self.factory,
                            engine_ctx,
                            &mut meta_ctx,
                            &self.cancel,
                            &mut chat_history,
                            &mut current_description,
                            description,
                            &checkpoint_path,
                            "上下文超限（执行模式）→ 元重判编排".to_string(),
                        )
                        .await?
                        {
                            Some(result) => return Ok(result),
                            None => continue,
                        }
                    }
                }
            };

            // ── Phase 3: YinVerify (因果验证·阴) ──
            //
            // Skip verify if crash recovery from YinDone.
            event_bus::emit_event(TaskEvent::PhaseChanged {
                task_id: engine_ctx.task_id.clone(),
                phase: ZhouyiPhase::Yin,
            });
            let report = if resume_phase == Some(CyclePhase::YinDone) {
                // Load verify_state.json and use cached report.
                match load_verify_report(&engine_ctx.task_dir) {
                    Some(r) => r,
                    None => {
                        tracing::warn!(
                            task_id = %engine_ctx.task_id,
                            "verify_state.json not found — re-running verify"
                        );
                        let verify_agent =
                            self.factory.create_yin_verify_agent(engine_ctx, &meta_ctx)?;
                        let tool_results = collect_tool_results(&engine_ctx.task_dir);
                        verify_agent
                            .verify(&yang_result.content, &tool_results, &meta_ctx)
                            .await?
                    }
                }
            } else {
                let verify_agent = self.factory.create_yin_verify_agent(engine_ctx, &meta_ctx)?;
                let tool_results = collect_tool_results(&engine_ctx.task_dir);
                let report = verify_agent
                    .verify(&yang_result.content, &tool_results, &meta_ctx)
                    .await?;

                // Write checkpoint after Verify.
                write_checkpoint(&checkpoint_path, CyclePhase::YinDone, engine_ctx, &self.cancel);

                report
            };

            // Reset resume_phase so subsequent iterations run all phases normally.
            resume_phase = None;

            // ── Phase 4: Route decision ──
            match report.route {
                VerificationRoute::Pass => {
                    tracing::info!(
                        task_id = %engine_ctx.task_id,
                        round = engine_ctx.round,
                        cycle = engine_ctx.cycle,
                        "Zhouyi cycle — PASS"
                    );

                    // Broadcast route decision + consume pending review.
                    event_bus::emit_event(TaskEvent::ZhouyiRouteDecision {
                        task_id: engine_ctx.task_id.clone(),
                        route: "PASS".into(),
                        cycle: engine_ctx.cycle,
                        round: engine_ctx.round,
                        verdict: report.summary.clone(),
                    });
                    let _ = std::fs::remove_file(engine_ctx.task_dir.join("review.json"));

                    // Construct DecomposeResult (matching what recursive_decompose expects).
                    let decompose_result = DecomposeResult {
                        task_id: yang_result.task_id.clone(),
                        summary: yang_result.content.clone(),
                        status: ConvergenceStatus::Converged,
                        subtask_count: 0,
                        deliverables: yang_result.deliverables.clone(),
                        rounds: yang_result.rounds,
                        tools_used: yang_result.tools_used.clone(),
                        child_results: vec![],
                    };

                    // Write decompose_result.json as completion marker.
                    if let Err(e) = save_json_atomic(&decompose_result, &decompose_result_path) {
                        tracing::warn!(
                            path = %decompose_result_path.display(),
                            error = %e,
                            "Failed to save decompose_result"
                        );
                    }

                    // Clean up checkpoint (task is done).
                    let _ = std::fs::remove_file(&checkpoint_path);

                    // ── V33/MVP-2: enqueue Lianshan pending（被动学习 — BCP §6.4/§8.23）──
                    // 读 verify_state.json 的 checks（YinAgent 已写，MVP-1）→
                    // 原子写 pending/{task_id}.json。Zhouyi 只读归藏（§8.3 硬约束）：
                    // 入队只写 pending/，归藏 YAML 由 Lianshan Consumer 单写。
                    // I/O 失败仅 warn —— 学习是增强层，不阻断 PASS。
                    let data_root = engine_ctx
                        .task_dir
                        .parent()
                        .and_then(|p| p.parent());
                    if let Some(data_root) = data_root {
                        // ── V33/MVP-3: 四维信号摊派（BCP §6.4）──
                        // cost = trace usage.input_tokens 求和；rounds = verify_state.round；
                        // quality = route 映射（Pass=1.0/BackToZhouyi=0.4/BackToMeta=0.2）× confidence——
                        // 全部既有数据，零新增持久化文件。任务级信号摊派给同任务所有检查项。
                        let checks: Vec<CheckResult> =
                            match load_json_optional::<serde_json::Value>(
                                &engine_ctx.task_dir.join("verify_state.json"),
                            ) {
                                Ok(Some(state)) => {
                                    let rounds = state
                                        .get("round")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0) as u32;
                                    let confidence = state
                                        .get("report")
                                        .and_then(|r| r.get("confidence"))
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or(0.0);
                                    let route_mult = match state
                                        .get("report")
                                        .and_then(|r| r.get("route"))
                                        .and_then(|v| v.as_str())
                                    {
                                        Some("BackToMeta") => 0.2,
                                        Some("BackToZhouyi") => 0.4,
                                        _ => 1.0, // Pass
                                    };
                                    let quality = route_mult * confidence;
                                    let cost = sum_trace_input_tokens(&engine_ctx.task_dir);
                                    let mut checks: Vec<CheckResult> = state
                                        .get("checks")
                                        .and_then(|c| serde_json::from_value(c.clone()).ok())
                                        .unwrap_or_default();
                                    for c in &mut checks {
                                        c.cost_tokens = cost;
                                        c.verify_rounds = rounds;
                                        c.quality = quality;
                                    }
                                    checks
                                }
                                _ => vec![],
                            };
                        if let Err(e) = enqueue_lianshan_pending(
                            &data_root,
                            &engine_ctx.task_dir,
                            &engine_ctx.task_id,
                            &checks,
                            &meta_ctx.assets_used,
                            true,
                            meta_ctx.model.as_ref().map(|m| m.key()),
                        )
                        .await
                        {
                            tracing::warn!(
                                task_id = %engine_ctx.task_id,
                                error = %e,
                                "Failed to enqueue Lianshan pending — learning skipped (non-blocking)"
                            );
                        }
                    } else {
                        tracing::warn!(
                            task_dir = %engine_ctx.task_dir.display(),
                            "Cannot derive data_root from task_dir — Lianshan pending enqueue skipped"
                        );
                    }

                    return Ok(yang_result);
                }
                VerificationRoute::BackToZhouyi => {
                    engine_ctx.round += 1;
                    if engine_ctx.round > self.config.runtime.max_rounds {
                        return Err(TaijiError::MaxRoundsExceeded {
                            max: self.config.runtime.max_rounds,
                        });
                    }
                    let violations_note: String = if report.constraint_violations.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\n  - Violations: {}",
                            report.constraint_violations.join(", ")
                        )
                    };
                    current_description = format!(
                        "## Previous Attempt (Round {}) Was Rejected\n\
                         Reason: {}{}\n\n\
                         ## Original Task\n\
                         {description}",
                        engine_ctx.round - 1,
                        report.summary,
                        violations_note,
                    );
                    // V28 产出继承：注入产出文件引用（deliverables/ + handoff.md），
                    // 下一轮基于产出修正/拆解（§8.18）。
                    current_description.push_str(
                        &crate::infra::handoff::build_handoff_description(
                            &engine_ctx.task_dir,
                        ),
                    );
                    tracing::warn!(
                        round = engine_ctx.round,
                        task_id = %engine_ctx.task_id,
                        "BACK_TO_ZHOUYI — retrying YangAgent (MetaAgent skipped)"
                    );

                    // Broadcast route decision.
                    event_bus::emit_event(TaskEvent::ZhouyiRouteDecision {
                        task_id: engine_ctx.task_id.clone(),
                        route: "BACK_TO_ZHOUYI".into(),
                        cycle: engine_ctx.cycle,
                        round: engine_ctx.round,
                        verdict: report.summary.clone(),
                    });

                    // Inject human review suggestion (yin approval) into the
                    // retry description, then consume the review file.
                    inject_human_review(&engine_ctx.task_dir, &mut current_description);

                    // V28：不再重放对话历史（执行事实是唯一记忆，§1.4）——
                    // 下一轮基于验证报告 + 产出文件继续/修正。
                    chat_history = Vec::new();
                    continue;
                }
                VerificationRoute::BackToMeta => {
                    let verdict = report.summary.clone();
                    match rerun_meta(
                        &self.factory,
                        engine_ctx,
                        &mut meta_ctx,
                        &self.cancel,
                        &mut chat_history,
                        &mut current_description,
                        description,
                        &checkpoint_path,
                        verdict,
                    )
                    .await?
                    {
                        Some(result) => return Ok(result),
                        None => continue,
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Inject the human review suggestion from `review.json` (written by the
/// frontend yin approval box) into the next-round task description, then
/// consume the file so it is injected exactly once.
fn inject_human_review(task_dir: &Path, description: &mut String) {
    let review_path = task_dir.join("review.json");
    let Ok(Some(review)) = load_json_optional::<YinIntervention>(&review_path) else {
        return;
    };
    let _ = std::fs::remove_file(&review_path);
    if review.suggestion.trim().is_empty() {
        return;
    }
    description.push_str(&format!(
        "\n\n## Human Review Suggestion\n{}",
        review.suggestion
    ));
}

/// Persist MetaContext to meta_ctx.json (crash recovery support).
fn persist_meta_ctx(meta_ctx: &MetaContext, task_dir: &Path) {
    let path = task_dir.join("meta_ctx.json");
    if let Err(e) = save_json_atomic(meta_ctx, &path) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "Failed to save meta_ctx"
        );
    }
}

/// V27 深度规则兑底：叶节点（`depth + 1 >= max_depth`）无法再拆解，无论
/// MetaAgent 或父层分配了什么模式，一律强制 Execution。
///
/// 与 `RecursiveDecomposeTool` 内的子任务模式兑底互为镜像——本函数覆盖根任务
/// 与 BACK_TO_META 重跑路径，工具内覆盖子任务路径。
fn apply_leaf_depth_rule(meta_ctx: &mut MetaContext, depth: u32, max_depth: u32) {
    if depth + 1 >= max_depth && meta_ctx.mode != crate::types::agent::AgentMode::Execution {
        tracing::info!(
            depth,
            max_depth,
            "V27 leaf depth rule: forcing Execution mode (depth+1 >= max_depth)"
        );
        meta_ctx.mode = crate::types::agent::AgentMode::Execution;
    }
}

/// Write a checkpoint atomically. Skips if cancelled (state is untrusted).
fn write_checkpoint(path: &Path, phase: CyclePhase, engine_ctx: &EngineContext, cancel: &CancellationToken) {
    if cancel.is_cancelled() {
        return;
    }
    let checkpoint = Checkpoint {
        phase,
        round: engine_ctx.round,
        cycle: engine_ctx.cycle,
    };
    if let Err(e) = save_json_atomic(&checkpoint, path) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "Failed to write checkpoint"
        );
    }
}

/// Atomically write the task status into `task_dir/meta.json` (V26 统一状态管理).
///
/// Loads the existing `Task` if present and only updates `status`, preserving
/// all other fields; creates a fresh `Task` entry when the file is missing
/// (child task dirs are created by `recursive_decompose` before this runs).
/// Serialization follows the atomic-write convention (tmp + rename).
pub(crate) fn write_task_status(
    task_dir: &Path,
    task_id: &str,
    description: &str,
    depth: u32,
    status: TaskStatus,
) -> Result<(), TaijiError> {
    let meta_path = task_dir.join("meta.json");
    let mut task = match load_json_optional::<Task>(&meta_path) {
        Ok(Some(t)) => t,
        _ => Task {
            id: task_id.to_string(),
            description: description.to_string(),
            depth,
            status: TaskStatus::Pending,
            parent_id: None,
            subtask_ids: vec![],
        },
    };
    task.status = status;
    save_json_atomic(&task, &meta_path).map_err(TaijiError::IO)
}

/// V46 短路（BCP §8.8）：应答类任务把答案写为 `deliverables/answer.md`，
/// 返回绝对路径。验证规则：符号校验保底（引用真实性）+ 交互判断兜底
/// （父节点/用户读 answer.md 裁定），阴不做语义验证（同源概率回路 §1.3）。
async fn write_short_circuit_answer(task_dir: &Path, answer: &str) -> Result<String, TaijiError> {
    let dir = task_dir.join("deliverables");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| TaijiError::IO(e))?;
    let path = dir.join("answer.md");
    tokio::fs::write(&path, answer)
        .await
        .map_err(|e| TaijiError::IO(e))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Load chat_history from disk, returning empty Vec on any error.
fn load_chat_history_or_empty(task_dir: &Path) -> Vec<Message> {
    let path = task_dir.join("chat_history.json");
    match load_json_optional::<Vec<Message>>(&path) {
        Ok(Some(history)) => history,
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to load chat_history — using empty Vec"
            );
            Vec::new()
        }
    }
}

/// Reconstruct a ZhouyiResult from persisted state after a crash at/after
/// YangDone, without re-running the YangAgent LLM.
///
/// V26.5 (P2): the old trace-based reconstruction matched
/// `phase == "output" | "result"` records that no code ever writes
/// (TraceHook only emits `completion_call` / `completion_response` /
/// `tool_call::*`), so crash recovery always fell back to re-running Yang
/// — wasting tokens and potentially changing the result. Sources used here:
/// 1. `content`      ← last assistant text message in `chat_history.json`
/// 2. `tools_used`   ← deduped `tool_call::*` phases in `trace.jsonl` (first-call order)
/// 3. `deliverables` ← files listed under `deliverables/`
fn construct_zhouyi_result_from_state(
    engine_ctx: &EngineContext,
) -> Result<Option<ZhouyiResult>, TaijiError> {
    let task_dir = &engine_ctx.task_dir;

    // V28 产出继承优先：有交接文件（deliverables/handoff.md）则从产出重建——
    // 执行事实是唯一记忆（§1.4 / §8.18），chat_history 仅作本节点兜底。
    if let Some(handoff) = crate::infra::handoff::read_handoff(task_dir) {
        tracing::info!(
            task_id = %engine_ctx.task_id,
            "Crash recovery — reconstructing YangAgent result from handoff (V28 产出继承)"
        );
        return Ok(Some(ZhouyiResult {
            task_id: engine_ctx.task_id.clone(),
            content: handoff,
            tools_used: collect_tools_used_from_trace(task_dir),
            deliverables: list_deliverables(task_dir),
            depth: engine_ctx.depth,
            rounds: engine_ctx.round + 1,
        }));
    }

    // 1. Content: last assistant text in the persisted conversation.
    let history = load_chat_history_or_empty(task_dir);
    let content = assistant_text_from_history(&history);
    let Some(content) = content else {
        tracing::warn!(
            task_id = %engine_ctx.task_id,
            "chat_history has no assistant text — cannot reconstruct YangAgent result"
        );
        return Ok(None);
    };

    Ok(Some(ZhouyiResult {
        task_id: engine_ctx.task_id.clone(),
        content,
        tools_used: collect_tools_used_from_trace(task_dir),
        deliverables: list_deliverables(task_dir),
        depth: engine_ctx.depth,
        rounds: engine_ctx.round + 1,
    }))
}

/// YangAgent 执行结果（含 V47 上下文超限路由分流）。
enum YangOutcome {
    /// 成功产出（进入 Yin 验证）。
    Success(ZhouyiResult),
    /// context_overflow：编排模式或叶节点 → BACK_TO_ZHOUYI（重跑 Yang）。
    BackToZhouyi,
    /// context_overflow：执行模式且可再拆 → BACK_TO_META（元重判编排）。
    BackToMeta,
}

/// Run the YangAgent with V28/V29 error routing (BCP §8.18 / §8.19 / V47 §8.14).
///
/// - `Ok(Success(result))` — success.
/// - `Ok(BackToZhouyi)` — `ContextOverflow`（编排模式或叶节点）：已递增 round、
///   安装产出继承描述（deliverables/ + handoff.md）、emit BACK_TO_ZHOUYI；调用方
///   应 `continue`（阳基于产出递归分解 / 残缺产出兜底，不再重放 chat_history）。
/// - `Ok(BackToMeta)` — `ContextOverflow`（执行模式且 depth+1 < max_depth）：
///   粒度错误 = 认知偏差，emit BACK_TO_META；调用方应走元重判编排流程（V47）。
/// - `Err(e)` — `HardCutoff`（硬截止）及其他错误：传播为 FAIL。
async fn run_yang_with_v28_routing(
    factory: &Arc<AgentFactory>,
    engine_ctx: &mut EngineContext,
    meta_ctx: &MetaContext,
    cancel: CancellationToken,
    chat_history: &mut Vec<Message>,
    current_description: &mut String,
    max_rounds: u32,
) -> Result<YangOutcome, TaijiError> {
    let yang_agent =
        factory.create_yang_agent(engine_ctx.depth, meta_ctx, engine_ctx, cancel)?;
    match yang_agent
        .run(current_description.as_str(), Some(chat_history.clone()))
        .await
    {
        Ok(result) => Ok(YangOutcome::Success(result)),
        Err(TaijiError::ContextOverflow { threshold }) => {
            engine_ctx.round += 1;
            if engine_ctx.round > max_rounds {
                return Err(TaijiError::MaxRoundsExceeded { max: max_rounds });
            }
            // V47 模式分流（BCP §8.18/§8.14）：执行模式 + 可再拆 → 粒度错误 =
            // 认知偏差 → BACK_TO_META（元重判编排）；编排模式或叶节点 →
            // BACK_TO_ZHOUYI（阳递归分解 / 残缺产出兜底）。
            let can_decompose = engine_ctx.depth + 1 < factory.config.runtime.max_depth;
            if meta_ctx.mode == crate::types::agent::AgentMode::Execution && can_decompose {
                tracing::warn!(
                    task_id = %engine_ctx.task_id,
                    round = engine_ctx.round,
                    threshold,
                    "BACK_TO_META — context overflow in Execution mode → meta re-routing (V47)"
                );
                event_bus::emit_event(TaskEvent::ZhouyiRouteDecision {
                    task_id: engine_ctx.task_id.clone(),
                    route: "BACK_TO_META".into(),
                    cycle: engine_ctx.cycle,
                    round: engine_ctx.round,
                    verdict: format!(
                        "上下文超限（≥{threshold} tokens）→ 执行模式认知偏差 → 元重判编排"
                    ),
                });
                *chat_history = Vec::new();
                return Ok(YangOutcome::BackToMeta);
            }
            tracing::warn!(
                task_id = %engine_ctx.task_id,
                round = engine_ctx.round,
                threshold,
                "BACK_TO_ZHOUYI — context overflow: handoff-based decomposition (V28)"
            );
            event_bus::emit_event(TaskEvent::ZhouyiRouteDecision {
                task_id: engine_ctx.task_id.clone(),
                route: "BACK_TO_ZHOUYI".into(),
                cycle: engine_ctx.cycle,
                round: engine_ctx.round,
                verdict: format!("上下文超限（≥{threshold} tokens）→ 基于产出递归分解"),
            });
            // V28 产出继承：不再以原 description + chat_history 重放重跑
            *current_description =
                crate::infra::handoff::build_handoff_description(&engine_ctx.task_dir);
            *chat_history = Vec::new();
            Ok(YangOutcome::BackToZhouyi)
        }
        Err(e) => Err(e),
    }
}

/// V47：执行 BACK_TO_META 流程（cycle++、元基于 handoff 校准重判、叶深度兑底、
/// 持久化、重置 chat_history）。Phase 2（context_overflow 执行模式分流）与
/// Phase 4（VerificationRoute::BackToMeta）共用。
///
/// 返回 `Some(ZhouyiResult)` = 元短路（Answer → 直接 PASS）；`None` = 元产出
/// Context（meta_ctx 已更新，调用方应 `continue` 重新进入循环）。
async fn rerun_meta(
    factory: &Arc<AgentFactory>,
    engine_ctx: &mut EngineContext,
    meta_ctx: &mut MetaContext,
    cancel: &CancellationToken,
    chat_history: &mut Vec<Message>,
    current_description: &mut String,
    description: &str,
    checkpoint_path: &Path,
    verdict: String,
) -> Result<Option<ZhouyiResult>, TaijiError> {
    engine_ctx.cycle += 1;
    engine_ctx.round = 0;
    if engine_ctx.cycle > factory.config.runtime.max_cycles {
        return Err(TaijiError::MaxCyclesExceeded {
            max: factory.config.runtime.max_cycles,
        });
    }
    *current_description = description.to_string();
    tracing::warn!(
        cycle = engine_ctx.cycle,
        task_id = %engine_ctx.task_id,
        "BACK_TO_META — re-running MetaAgent for fresh reasoning paths"
    );
    event_bus::emit_event(TaskEvent::ZhouyiRouteDecision {
        task_id: engine_ctx.task_id.clone(),
        route: "BACK_TO_META".into(),
        cycle: engine_ctx.cycle,
        round: engine_ctx.round,
        verdict,
    });
    inject_human_review(&engine_ctx.task_dir, current_description);
    if cancel.is_cancelled() {
        return Err(TaijiError::Cancelled {
            context: format!("Zhouyi cycle cancelled for task {}", engine_ctx.task_id),
        });
    }
    event_bus::emit_event(TaskEvent::PhaseChanged {
        task_id: engine_ctx.task_id.clone(),
        phase: ZhouyiPhase::Meta,
    });
    let meta_agent = factory.create_meta_agent(
        &engine_ctx.task_id,
        engine_ctx.depth,
        factory.config.runtime.max_depth,
    )?;
    let handoff = crate::infra::handoff::read_handoff(&engine_ctx.task_dir);
    match meta_agent
        .run(description, &["general"], handoff.as_deref())
        .await?
    {
        MetaOutcome::Answer(answer) => {
            let answer_path =
                write_short_circuit_answer(&engine_ctx.task_dir, &answer).await?;
            tracing::info!(
                task_id = %engine_ctx.task_id,
                "MetaAgent answer short-circuit (BACK_TO_META) → PASS"
            );
            write_task_status(
                &engine_ctx.task_dir,
                &engine_ctx.task_id,
                description,
                engine_ctx.depth,
                TaskStatus::Completed,
            )?;
            Ok(Some(ZhouyiResult {
                task_id: engine_ctx.task_id.clone(),
                content: answer,
                tools_used: vec![],
                deliverables: vec![answer_path],
                depth: engine_ctx.depth,
                rounds: engine_ctx.round,
            }))
        }
        MetaOutcome::Context(ctx) => {
            *meta_ctx = ctx;
            apply_leaf_depth_rule(meta_ctx, engine_ctx.depth, factory.config.runtime.max_depth);
            persist_meta_ctx(meta_ctx, &engine_ctx.task_dir);
            write_checkpoint(checkpoint_path, CyclePhase::MetaDone, engine_ctx, cancel);
            *chat_history = Vec::new();
            Ok(None)
        }
    }
}

/// Extract the text of the last assistant message that contains any text
/// content (skipping tool-only assistant turns).
fn assistant_text_from_history(history: &[Message]) -> Option<String> {
    use rig::completion::AssistantContent;

    history.iter().rev().find_map(|m| match m {
        Message::Assistant { content, .. } => content
            .iter()
            .find_map(|c| match c {
                AssistantContent::Text(t) => Some(t.text.clone()),
                _ => None,
            }),
        _ => None,
    })
}

/// Deduplicated, first-call-ordered tool names from `tool_call::*` records.
fn collect_tools_used_from_trace(task_dir: &Path) -> Vec<String> {
    use crate::infra::trace::TraceRecord;

    let trace_path = task_dir.join("trace.jsonl");
    let Ok(content) = std::fs::read_to_string(&trace_path) else {
        return Vec::new();
    };

    let mut tools: Vec<String> = Vec::new();
    for line in content.lines() {
        if let Ok(record) = serde_json::from_str::<TraceRecord>(line) {
            if let Some(name) = record.phase.strip_prefix("tool_call::") {
                if !tools.iter().any(|t| t == name) {
                    tools.push(name.to_string());
                }
            }
        }
    }
    tools
}

/// List absolute paths of files under `deliverables/` (empty if none).
fn list_deliverables(task_dir: &Path) -> Vec<String> {
    let dir = task_dir.join("deliverables");
    if !dir.exists() {
        return Vec::new();
    }
    std::fs::read_dir(&dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Load the cached verification report from verify_state.json.
/// 读 trace.jsonl，累加 `completion_response` 记录的 `output.usage.input_tokens`
/// （token 成本信号，§6.4 回报函数 avg_cost_tokens 数据源）。
/// 读失败/无记录 → 0（I/O 问题不阻断学习，成本维度退化）。
fn sum_trace_input_tokens(task_dir: &Path) -> u64 {
    let Ok(content) = std::fs::read_to_string(task_dir.join("trace.jsonl")) else {
        return 0;
    };
    content
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            if v.get("phase").and_then(|p| p.as_str()) != Some("completion_response") {
                return None;
            }
            v.get("output")
                .and_then(|o| o.get("usage"))
                .and_then(|u| u.get("input_tokens"))
                .and_then(|t| t.as_u64())
        })
        .sum()
}

fn load_verify_report(task_dir: &Path) -> Option<VerificationReport> {
    let path = task_dir.join("verify_state.json");
    match load_json_optional::<serde_json::Value>(&path) {
        Ok(Some(state)) => {
            if let Some(report_val) = state.get("report") {
                serde_json::from_value(report_val.clone()).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Read tool result records from the trace file for the current task.
///
/// Extracts tool output strings for use by YinAgent.verify(), so the
/// verify LLM can cross-reference tool outputs against the task output.
///
/// This is a fast synchronous I/O operation (~ms) compared to the LLM
/// calls in the Zhouyi loop (~seconds).
fn collect_tool_results(task_dir: &Path) -> Vec<String> {
    use crate::infra::trace::TraceRecord;

    let trace_path = task_dir.join("trace.jsonl");
    let content = match std::fs::read_to_string(&trace_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Parse only recent tool result records, keeping the last few per tool.
    let mut results: Vec<String> = Vec::new();
    for line in content.lines().rev() {
        if results.len() >= 10 {
            break; // cap at 10 most recent tool results
        }
        match serde_json::from_str::<TraceRecord>(line) {
            Ok(record) if record.phase.starts_with("tool_call::") => {
                let tool_name = record
                    .phase
                    .strip_prefix("tool_call::")
                    .unwrap_or("unknown");
                let output_summary = format_tool_output(tool_name, &record.output);
                results.push(output_summary);
            }
            _ => {}
        }
    }
    results.reverse(); // restore chronological order
    results
}

/// Format a tool's JSON output into a brief readable string for the verify prompt.
fn format_tool_output(tool_name: &str, output: &serde_json::Value) -> String {
    let body = match output {
        serde_json::Value::Object(map) => {
            if let Some(s) = map.get("content").and_then(|v| v.as_str()) {
                trunc(s, 200)
            } else if let Some(s) = map.get("stdout").and_then(|v| v.as_str()) {
                trunc(s, 200)
            } else if let Some(s) = map.get("status").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(s) = map.get("note").and_then(|v| v.as_str()) {
                s.to_string()
            } else {
                trunc(&output.to_string(), 100)
            }
        }
        serde_json::Value::String(s) => trunc(s, 200),
        _ => trunc(&output.to_string(), 100),
    };
    format!("[{tool_name}] {body}")
}

fn trunc(s: &str, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        s.to_string()
    } else {
        chars[..max_len].iter().collect()
    }
}

/// V33/MVP-2：将检查项结果入队 Lianshan pending（被动学习 — BCP §6.4/§8.23）。
///
/// 写 `{data_root}/pending/{task_id}.json`，内容 = `{task_id, source, checks, assets_used, passed, model_key}`。
/// 同 task_id 覆盖写（幂等——重跑任务不产生重复学习）；原子写（save_json_atomic）。
/// 调用方为 Zhouyi PASS 分支；I/O 失败由调用方 warn（学习是增强层，不阻断 PASS）。
/// V35/MVP-6：assets_used（编排所选资产，Lianshan 回传依据 §8.21）与 passed（任务级
/// PASS 信号——prompts 任务级归因；serde default 旧 pending 零迁移）。
/// V36→V44：model_key 作为统计键随 pending 入队（§10.1 去分区化——Lianshan 回传
/// 统一落根级资产树，model_key 仅用于 model_stats 索引；serde default
/// 旧 pending 零迁移，None = 未指定模型）。
pub(crate) async fn enqueue_lianshan_pending(
    data_root: &Path,
    task_dir: &Path,
    task_id: &str,
    checks: &[CheckResult],
    assets_used: &[AssetRef],
    passed: bool,
    model_key: Option<&str>,
) -> Result<(), TaijiError> {
    let pending_dir = data_root.join("pending");
    tokio::fs::create_dir_all(&pending_dir).await.map_err(|e| {
        TaijiError::Other(format!(
            "failed to create pending dir {:?}: {e}",
            pending_dir
        ))
    })?;

    let payload = serde_json::json!({
        "task_id": task_id,
        "task_dir": task_dir.display().to_string(),
        "source": "zhouyi",
        "checks": checks,
        "assets_used": assets_used,
        "passed": passed,
        "model_key": model_key,
    });
    let path = pending_dir.join(format!("{task_id}.json"));
    save_json_atomic(&payload, &path).map_err(|e| {
        TaijiError::Other(format!(
            "failed to write pending file {:?}: {e}",
            path
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::factory::AgentFactory;
    use crate::hooks::safety::SafetyHook;
    use crate::infra::config::{LlmConfig, RuntimeConfig, SafetyConfig, TaijiConfig};
    use crate::infra::provider::ProviderRegistry;
    use crate::types::execution::EngineContext;

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
            runtime: RuntimeConfig::default(),
            knowledge: crate::infra::config::KnowledgeConfig::default(),
            safety: SafetyConfig::default(),
            mcp_servers: vec![],
        }
    }

    fn tmp_task_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("taiji_zhouyi_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tmp task dir");
        dir
    }

    fn make_engine_ctx(task_id: &str, task_dir: std::path::PathBuf) -> EngineContext {
        EngineContext {
            task_id: task_id.into(),
            depth: 0,
            task_dir,
            cycle: 1,
            round: 0,
            context_dir: None,
        }
    }

    async fn build_factory(config: TaijiConfig) -> Arc<AgentFactory> {
        // Unique dir per invocation: parallel tests each calling build_factory
        // must not share a pid-based dir — concurrent remove_dir_all + rename
        // during GuizangClient init produced a flaky KnowledgeStoreUnavailable
        // (V26.1-3 verification round, plan blocker 1).
        static FACTORY_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = FACTORY_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp_dir = std::env::temp_dir().join(format!(
            "taiji_zhouyi_factory_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let guizang = Arc::new(
            crate::infra::knowledge::GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let providers = ProviderRegistry::new(&config).expect("ProviderRegistry");
        Arc::new(AgentFactory {
            guizang,
            providers: Arc::new(providers),
            config,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            worker_pool: Arc::new(crate::orchestration::worker_pool::WorkerPool::new(4)),
            constraint_engine: Arc::new(crate::orchestration::constraint_engine::ConstraintEngine::new()),
            trigger_engine: Arc::new(crate::orchestration::trigger_engine::SkillTriggerEngine::new()),
            data_root: std::path::PathBuf::from("./data"),
        })
    }

    // ── write_task_status (V26 统一状态管理) ─────────────────────────

    #[test]
    fn test_write_task_status_creates_missing_meta() {
        let dir = tmp_task_dir("status_create");
        let result = write_task_status(&dir, "task-1", "desc", 0, TaskStatus::Running);
        assert!(result.is_ok());

        let task: Task = load_json_optional(&dir.join("meta.json"))
            .expect("load meta")
            .expect("meta exists");
        assert_eq!(task.id, "task-1");
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.depth, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_task_status_preserves_existing_fields() {
        let dir = tmp_task_dir("status_update");
        write_task_status(&dir, "task-1", "old desc", 0, TaskStatus::Running).expect("running");
        write_task_status(&dir, "task-1", "new desc", 99, TaskStatus::Failed).expect("failed");

        let task: Task = load_json_optional(&dir.join("meta.json"))
            .expect("load meta")
            .expect("meta exists");
        // 只更新 status，其余字段保留（V26 统一状态管理契约）
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.description, "old desc");
        assert_eq!(task.depth, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── ZhouyiCycle status 终态落盘（根/子同构）─────────────────────────

    #[tokio::test]
    async fn test_enqueue_lianshan_pending_writes_file_and_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "taiji_enqueue_pending_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let checks = vec![CheckResult {
            check_id: "meta-json-schema".into(),
            kind: crate::types::verification::CheckKind::SchemaValid,
            passed: true,
            detail: "schema valid (json)".into(),
            duration_ms: 1,
            cost_tokens: 0,
            verify_rounds: 0,
            quality: 0.0,
        }];

        // 首次入队：文件结构断言
        super::enqueue_lianshan_pending(&dir, &dir, "task-1", &checks, &[], true, Some("deepseek-deepseek-chat"))
            .await
            .unwrap();
        let path = dir.join("pending").join("task-1.json");
        assert!(path.exists(), "pending file should exist");
        let content: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(content["task_id"], "task-1");
        assert_eq!(content["source"], "zhouyi");
        assert_eq!(content["checks"].as_array().unwrap().len(), 1);
        assert_eq!(content["checks"][0]["check_id"], "meta-json-schema");
        assert_eq!(content["checks"][0]["passed"], true);
        // V36：model_key 随 pending 入队（Lianshan 分区回传依据）
        assert_eq!(content["model_key"], "deepseek-deepseek-chat");

        // 幂等：同 task_id 覆盖写（不产生第二文件）
        super::enqueue_lianshan_pending(&dir, &dir, "task-1", &[], &[], true, None)
            .await
            .unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.join("pending"))
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 1, "idempotent overwrite — single file");
        let content2: serde_json::Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(content2["checks"].as_array().unwrap().len(), 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_sum_trace_input_tokens_accumulates_usage() {
        let dir = std::env::temp_dir().join(format!(
            "taiji_sum_trace_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let trace = dir.join("trace.jsonl");
        // 两条 completion_response 带 usage + 一条无关记录
        tokio::fs::write(
            &trace,
            concat!(
                r#"{"ts":"t1","cycle":0,"depth":0,"task_id":"t","phase":"completion_response","provider_model":"m","duration_ms":1,"input":{},"output":{"usage":{"input_tokens":1234,"output_tokens":10,"total_tokens":1244}},"degraded":false}"#,
                "\n",
                r#"{"ts":"t2","cycle":0,"depth":0,"task_id":"t","phase":"completion_response","provider_model":"m","duration_ms":1,"input":{},"output":{"usage":{"input_tokens":666,"output_tokens":5,"total_tokens":671}},"degraded":false}"#,
                "\n",
                r#"{"ts":"t3","cycle":0,"depth":0,"task_id":"t","phase":"tool_call::read","provider_model":"m","duration_ms":1,"input":{},"output":{},"degraded":false}"#,
                "\n",
            ),
        )
        .await
        .unwrap();
        assert_eq!(super::sum_trace_input_tokens(&dir), 1234 + 666);

        // 无 trace 文件 / 无记录 → 0
        let empty = dir.join("empty");
        tokio::fs::create_dir_all(&empty).await.unwrap();
        assert_eq!(super::sum_trace_input_tokens(&empty), 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_execute_writes_cancelled_on_cancelled_token() {
        let config = make_config();
        let factory = build_factory(config.clone()).await;
        let task_id = "cancel-test";
        let task_dir = tmp_task_dir(task_id);
        write_task_status(&task_dir, task_id, "desc", 0, TaskStatus::Running).expect("running");

        let cancel = CancellationToken::new();
        cancel.cancel();

        let zhouyi = ZhouyiCycle::new(factory, config, cancel);
        let mut ctx = make_engine_ctx(task_id, task_dir.clone());
        let result = zhouyi
            .execute("desc", Some(MetaContext::empty()), &mut ctx, None)
            .await;

        assert!(matches!(result, Err(TaijiError::Cancelled { .. })));
        let task: Task = load_json_optional(&task_dir.join("meta.json"))
            .expect("load meta")
            .expect("meta exists");
        assert_eq!(task.status, TaskStatus::Cancelled);

        let _ = std::fs::remove_dir_all(&task_dir);
    }

    #[tokio::test]
    async fn test_execute_cached_result_writes_completed() {
        // checkpoint.json + decompose_result.json 存在 → 返回缓存，不触发 LLM。
        // 验证根任务恢复链（V26：根/子同一恢复路径）与 Completed 落盘。
        let config = make_config();
        let factory = build_factory(config.clone()).await;
        let task_id = "cached-task";
        let task_dir = tmp_task_dir(task_id);

        let checkpoint = Checkpoint {
            phase: CyclePhase::YinDone,
            round: 0,
            cycle: 0,
        };
        save_json_atomic(&checkpoint, &task_dir.join("checkpoint.json")).expect("checkpoint");

        let cached = ZhouyiResult {
            task_id: task_id.into(),
            content: "cached output".into(),
            tools_used: vec![],
            deliverables: vec![],
            depth: 0,
            rounds: 1,
        };
        save_json_atomic(&cached, &task_dir.join("decompose_result.json")).expect("decompose");
        write_task_status(&task_dir, task_id, "desc", 0, TaskStatus::Failed).expect("meta");

        let cancel = CancellationToken::new();
        let zhouyi = ZhouyiCycle::new(factory, config, cancel);
        let mut ctx = make_engine_ctx(task_id, task_dir.clone());
        let result = zhouyi.execute("desc", None, &mut ctx, None).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().task_id, task_id);
        let task: Task = load_json_optional(&task_dir.join("meta.json"))
            .expect("load meta")
            .expect("meta exists");
        assert_eq!(task.status, TaskStatus::Completed);

        let _ = std::fs::remove_dir_all(&task_dir);
    }

    // ── V26.5 (P2): crash recovery rebuilds YangAgent result from
    //    persisted state (chat_history + trace + deliverables) instead of
    //    matching `phase == "output"|"result"` records that are never written.

    static RECONSTRUCT_SEQ: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn construct_zhouyi_result_from_state_reconstructs_from_persisted_files() {
        let seq = RECONSTRUCT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "taiji_zhouyi_reconstruct_{}_{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::create_dir_all(&dir);

        // chat_history.json: last assistant message carries the final output.
        let history = vec![
            Message::user("task description"),
            Message::assistant("early thinking with a tool call"),
            Message::user(""),
            Message::assistant("final answer: the report is done"),
        ];
        save_json_atomic(&history, &dir.join("chat_history.json"))
            .expect("chat_history");

        // trace.jsonl: tool_call records out of first-call order.
        let trace_path = dir.join("trace.jsonl");
        let mut lines = String::new();
        for (i, phase) in [
            "tool_call::read",
            "tool_call::yin_verify",
            "tool_call::read",
            "tool_call::write",
        ]
        .iter()
        .enumerate()
        {
            let record = crate::infra::trace::TraceRecord {
                ts: format!("2026-08-07T00:00:{i:02}Z"),
                cycle: 0,
                depth: 0,
                task_id: "reconstruct-test".into(),
                phase: phase.to_string(),
                provider_model: "test-model".into(),
                duration_ms: 0,
                input: serde_json::json!({}),
                output: serde_json::json!({}),
                degraded: false,
                constraint_violations: None,
            };
            lines.push_str(&serde_json::to_string(&record).unwrap());
            lines.push('\n');
        }
        std::fs::write(&trace_path, lines).unwrap();

        // deliverables/: one file.
        std::fs::create_dir_all(dir.join("deliverables")).unwrap();
        std::fs::write(dir.join("deliverables").join("report.md"), "x").unwrap();

        let ctx = make_engine_ctx("reconstruct-test", dir.clone());
        let result = construct_zhouyi_result_from_state(&ctx)
            .expect("no IO error")
            .expect("must reconstruct");

        assert_eq!(result.task_id, "reconstruct-test");
        assert_eq!(result.content, "final answer: the report is done");
        // Deduplicated, first-call order — not file order.
        assert_eq!(result.tools_used, vec!["read", "yin_verify", "write"]);
        assert_eq!(result.depth, 0);
        assert_eq!(result.rounds, 1);
        assert!(
            result
                .deliverables
                .iter()
                .any(|p| p.ends_with("deliverables/report.md")),
            "deliverables should list the report file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn construct_zhouyi_result_from_state_returns_none_without_chat_history() {
        let seq = RECONSTRUCT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "taiji_zhouyi_reconstruct_none_{}_{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::create_dir_all(&dir);

        let ctx = make_engine_ctx("reconstruct-none", dir.clone());
        let result = construct_zhouyi_result_from_state(&ctx).expect("no IO error");
        assert!(result.is_none(), "empty chat_history must yield None (fallback to re-run)");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
