//! TpnCycle — Reusable TPN three-phase cycle (元·阳·阴 → loop).
//!
//! Extracted from [`RecursiveRunner`] so that both root tasks and recursive
//! child tasks share the same TPN execution logic, satisfying the **isomorphic
//! recursion** principle (§1.1 of BCP).
//!
//! # Architecture (BCP §5)
//!
//! ```text
//! MetaAgent (权重更新·元)   ─── once at entrance (or from parent MetaContext)
//!     ↓
//! TPN loop (max_cycles × max_rounds):
//!     FittingAgent (概率拟合·阳)   →  LLM exploration + tools + recursion
//!     CausalAgent  (因果验证·阴)   →  constraint check + LLM verdict
//!     ├─ PASS        → return TPNResult
//!     ├─ BACK_TO_TPN → round++ (retry FittingAgent only)
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
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;
use crate::types::frontend::{TpnPhase, YinIntervention};
use crate::types::task::{Checkpoint, CyclePhase, DecomposeResult, Task, TaskStatus, TPNResult};
use crate::types::verification::{ConvergenceStatus, VerificationReport, VerificationRoute};
use crate::ws::types::TaskEvent;

/// Reusable TPN cycle that executes the three-phase loop for a task at any
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
///   provided history directly to FittingAgent for context-continuity.
pub struct TpnCycle {
    factory: Arc<AgentFactory>,
    config: TaijiConfig,
    /// Cancellation token propagated to all sub-agents.
    /// Root token created in [`RecursiveRunner`]; child tokens linked via
    /// `CancellationToken::child_token()` in `RecursiveDecomposeTool`.
    cancel: CancellationToken,
}

impl TpnCycle {
    /// Create a new `TpnCycle` with a cancellation token.
    pub fn new(
        factory: Arc<AgentFactory>,
        config: TaijiConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self { factory, config, cancel }
    }

    /// Execute the full TPN cycle.
    ///
    /// # Parameters
    ///
    /// - `description` — task description passed to FittingAgent.
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
    /// `Ok(TPNResult)` on PASS, else `Err(TaijiError)` on exhaustion or
    /// unrecoverable failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        description: &str,
        initial_meta_ctx: Option<MetaContext>,
        engine_ctx: &mut EngineContext,
        resume_history: Option<Vec<Message>>,
    ) -> Result<TPNResult, TaijiError> {
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

    /// Internal implementation of the TPN loop (see [`execute`]).
    async fn execute_inner(
        &self,
        description: &str,
        initial_meta_ctx: Option<MetaContext>,
        engine_ctx: &mut EngineContext,
        resume_history: Option<Vec<Message>>,
    ) -> Result<TPNResult, TaijiError> {
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
            (None, false)
        } else if let Ok(Some(checkpoint)) = load_json_optional::<Checkpoint>(&checkpoint_path) {
            // Check if decompose_result.json exists → task already completed.
            if let Ok(Some(_)) = load_json_optional::<TPNResult>(&decompose_result_path) {
                tracing::info!(
                    task_id = %engine_ctx.task_id,
                    phase = ?checkpoint.phase,
                    "Task already completed (decompose_result.json exists) — returning cached"
                );
                return load_json_optional::<TPNResult>(&decompose_result_path)
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
        //   2. Not crash recovery from FittingDone or VerifyDone
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
                phase: TpnPhase::Meta,
            });
            let meta_agent = self.factory.create_meta_agent(&engine_ctx.task_id)?;
            meta_ctx = meta_agent.run(description, &["general"]).await?;

            // Persist MetaContext for crash recovery.
            persist_meta_ctx(&meta_ctx, &engine_ctx.task_dir);

            // Write checkpoint after MetaAgent.
            write_checkpoint(&checkpoint_path, CyclePhase::MetaDone, engine_ctx, &self.cancel);
        } else if !resume_history_is_some && !is_crash_recovery {
            // Use parent-provided MetaContext.
            if let Some(ctx) = initial_meta_ctx {
                meta_ctx = ctx;
                // Persist immediately so crash recovery never loses the parent context.
                persist_meta_ctx(&meta_ctx, &engine_ctx.task_dir);
            }
        }

        // ── Phases 2-4: TPN loop ──────────────────────────────────────
        loop {
            // Check cancellation before each iteration.
            if self.cancel.is_cancelled() {
                return Err(TaijiError::Cancelled {
                    context: format!("TPN cycle cancelled for task {}", engine_ctx.task_id),
                });
            }

            // ── Phase 2: FittingAgent (概率拟合·阳) ──
            //
            // Skip if crash recovery from FittingDone or VerifyDone.
            event_bus::emit_event(TaskEvent::PhaseChanged {
                task_id: engine_ctx.task_id.clone(),
                phase: TpnPhase::Fitting,
            });
            let fitting_result = if resume_phase == Some(CyclePhase::FittingDone)
                || resume_phase == Some(CyclePhase::VerifyDone)
            {
                // Crash recovery: FittingAgent already ran, reconstruct its
                // result from persisted state (chat_history + trace + deliverables).
                // If we can't reconstruct, re-run.
                match construct_tpn_result_from_state(&engine_ctx) {
                    Ok(Some(result)) => result,
                    _ => {
                        tracing::warn!(
                            task_id = %engine_ctx.task_id,
                            "Could not reconstruct FittingAgent result from state — re-running"
                        );
                        let fitting_agent = self
                            .factory
                            .create_fitting_agent(
                                engine_ctx.depth,
                                &meta_ctx,
                                engine_ctx,
                                self.cancel.clone(),
                            )?;
                        fitting_agent
                            .run(&current_description, Some(chat_history.clone()))
                            .await?
                    }
                }
            } else {
                let fitting_agent = self
                    .factory
                    .create_fitting_agent(
                        engine_ctx.depth,
                        &meta_ctx,
                        engine_ctx,
                        self.cancel.clone(),
                    )?;
                let result = fitting_agent
                    .run(&current_description, Some(chat_history.clone()))
                    .await?;

                // Write checkpoint after FittingAgent (chat_history already saved internally).
                write_checkpoint(&checkpoint_path, CyclePhase::FittingDone, engine_ctx, &self.cancel);

                result
            };

            // ── Phase 3: CausalVerify (因果验证·阴) ──
            //
            // Skip verify if crash recovery from VerifyDone.
            event_bus::emit_event(TaskEvent::PhaseChanged {
                task_id: engine_ctx.task_id.clone(),
                phase: TpnPhase::Causal,
            });
            let report = if resume_phase == Some(CyclePhase::VerifyDone) {
                // Load verify_state.json and use cached report.
                match load_verify_report(&engine_ctx.task_dir) {
                    Some(r) => r,
                    None => {
                        tracing::warn!(
                            task_id = %engine_ctx.task_id,
                            "verify_state.json not found — re-running verify"
                        );
                        let verify_agent = self.factory.create_causal_verify_agent(engine_ctx)?;
                        let tool_results = collect_tool_results(&engine_ctx.task_dir);
                        verify_agent
                            .verify(&fitting_result.content, &tool_results, &meta_ctx)
                            .await?
                    }
                }
            } else {
                let verify_agent = self.factory.create_causal_verify_agent(engine_ctx)?;
                let tool_results = collect_tool_results(&engine_ctx.task_dir);
                let report = verify_agent
                    .verify(&fitting_result.content, &tool_results, &meta_ctx)
                    .await?;

                // Write checkpoint after Verify.
                write_checkpoint(&checkpoint_path, CyclePhase::VerifyDone, engine_ctx, &self.cancel);

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
                        "TPN cycle — PASS"
                    );

                    // Broadcast route decision + consume pending review.
                    event_bus::emit_event(TaskEvent::TpnRouteDecision {
                        task_id: engine_ctx.task_id.clone(),
                        route: "PASS".into(),
                        cycle: engine_ctx.cycle,
                        round: engine_ctx.round,
                        verdict: report.summary.clone(),
                    });
                    let _ = std::fs::remove_file(engine_ctx.task_dir.join("review.json"));

                    // Construct DecomposeResult (matching what recursive_decompose expects).
                    let decompose_result = DecomposeResult {
                        task_id: fitting_result.task_id.clone(),
                        summary: fitting_result.content.clone(),
                        status: ConvergenceStatus::Converged,
                        subtask_count: 0,
                        deliverables: fitting_result.deliverables.clone(),
                        rounds: fitting_result.rounds,
                        tools_used: fitting_result.tools_used.clone(),
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

                    return Ok(fitting_result);
                }
                VerificationRoute::BackToTpn => {
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
                    tracing::warn!(
                        round = engine_ctx.round,
                        task_id = %engine_ctx.task_id,
                        "BACK_TO_TPN — retrying FittingAgent (MetaAgent skipped)"
                    );

                    // Broadcast route decision.
                    event_bus::emit_event(TaskEvent::TpnRouteDecision {
                        task_id: engine_ctx.task_id.clone(),
                        route: "BACK_TO_TPN".into(),
                        cycle: engine_ctx.cycle,
                        round: engine_ctx.round,
                        verdict: report.summary.clone(),
                    });

                    // Inject human review suggestion (yin approval) into the
                    // retry description, then consume the review file.
                    inject_human_review(&engine_ctx.task_dir, &mut current_description);

                    // Reload chat_history from disk (fitting_agent.run() saved it).
                    chat_history = load_chat_history_or_empty(&engine_ctx.task_dir);
                    continue;
                }
                VerificationRoute::BackToMeta => {
                    engine_ctx.cycle += 1;
                    engine_ctx.round = 0;
                    if engine_ctx.cycle > self.config.runtime.max_cycles {
                        return Err(TaijiError::MaxCyclesExceeded {
                            max: self.config.runtime.max_cycles,
                        });
                    }
                    current_description = description.to_string();
                    tracing::warn!(
                        cycle = engine_ctx.cycle,
                        task_id = %engine_ctx.task_id,
                        "BACK_TO_META — re-running MetaAgent for fresh reasoning paths"
                    );

                    // Broadcast route decision.
                    event_bus::emit_event(TaskEvent::TpnRouteDecision {
                        task_id: engine_ctx.task_id.clone(),
                        route: "BACK_TO_META".into(),
                        cycle: engine_ctx.cycle,
                        round: engine_ctx.round,
                        verdict: report.summary.clone(),
                    });

                    // Inject human review suggestion into the fresh description.
                    inject_human_review(&engine_ctx.task_dir, &mut current_description);

                    if self.cancel.is_cancelled() {
                        return Err(TaijiError::Cancelled {
                            context: format!(
                                "TPN cycle cancelled for task {}",
                                engine_ctx.task_id
                            ),
                        });
                    }
                    event_bus::emit_event(TaskEvent::PhaseChanged {
                        task_id: engine_ctx.task_id.clone(),
                        phase: TpnPhase::Meta,
                    });
                    let meta_agent = self.factory.create_meta_agent(&engine_ctx.task_id)?;
                    meta_ctx = meta_agent.run(description, &[]).await?;

                    // Persist MetaContext and checkpoint for crash recovery.
                    persist_meta_ctx(&meta_ctx, &engine_ctx.task_dir);
                    write_checkpoint(&checkpoint_path, CyclePhase::MetaDone, engine_ctx, &self.cancel);

                    // Reset chat_history for fresh round.
                    chat_history = Vec::new();
                    continue;
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

/// Reconstruct a TPNResult from persisted state after a crash at/after
/// FittingDone, without re-running the FittingAgent LLM.
///
/// V26.5 (P2): the old trace-based reconstruction matched
/// `phase == "output" | "result"` records that no code ever writes
/// (TraceHook only emits `completion_call` / `completion_response` /
/// `tool_call::*`), so crash recovery always fell back to re-running Fitting
/// — wasting tokens and potentially changing the result. Sources used here:
/// 1. `content`      ← last assistant text message in `chat_history.json`
/// 2. `tools_used`   ← deduped `tool_call::*` phases in `trace.jsonl` (first-call order)
/// 3. `deliverables` ← files listed under `deliverables/`
fn construct_tpn_result_from_state(
    engine_ctx: &EngineContext,
) -> Result<Option<TPNResult>, TaijiError> {
    let task_dir = &engine_ctx.task_dir;

    // 1. Content: last assistant text in the persisted conversation.
    let history = load_chat_history_or_empty(task_dir);
    let content = assistant_text_from_history(&history);
    let Some(content) = content else {
        tracing::warn!(
            task_id = %engine_ctx.task_id,
            "chat_history has no assistant text — cannot reconstruct FittingAgent result"
        );
        return Ok(None);
    };

    Ok(Some(TPNResult {
        task_id: engine_ctx.task_id.clone(),
        content,
        tools_used: collect_tools_used_from_trace(task_dir),
        deliverables: list_deliverables(task_dir),
        depth: engine_ctx.depth,
        rounds: engine_ctx.round + 1,
    }))
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
/// Extracts tool output strings for use by CausalAgent.verify(), so the
/// verify LLM can cross-reference tool outputs against the task output.
///
/// This is a fast synchronous I/O operation (~ms) compared to the LLM
/// calls in the TPN loop (~seconds).
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
        let dir = std::env::temp_dir().join(format!("taiji_tpn_test_{tag}_{}", std::process::id()));
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
        // during LiluoClient init produced a flaky KnowledgeStoreUnavailable
        // (V26.1-3 verification round, plan blocker 1).
        static FACTORY_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = FACTORY_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp_dir = std::env::temp_dir().join(format!(
            "taiji_tpn_factory_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let liluo = Arc::new(
            crate::infra::knowledge::LiluoClient::new(&tmp_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let providers = ProviderRegistry::new(&config).expect("ProviderRegistry");
        Arc::new(AgentFactory {
            liluo,
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

    // ── TpnCycle status 终态落盘（根/子同构）─────────────────────────

    #[tokio::test]
    async fn test_execute_writes_cancelled_on_cancelled_token() {
        let config = make_config();
        let factory = build_factory(config.clone()).await;
        let task_id = "cancel-test";
        let task_dir = tmp_task_dir(task_id);
        write_task_status(&task_dir, task_id, "desc", 0, TaskStatus::Running).expect("running");

        let cancel = CancellationToken::new();
        cancel.cancel();

        let tpn = TpnCycle::new(factory, config, cancel);
        let mut ctx = make_engine_ctx(task_id, task_dir.clone());
        let result = tpn
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
            phase: CyclePhase::VerifyDone,
            round: 0,
            cycle: 0,
        };
        save_json_atomic(&checkpoint, &task_dir.join("checkpoint.json")).expect("checkpoint");

        let cached = TPNResult {
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
        let tpn = TpnCycle::new(factory, config, cancel);
        let mut ctx = make_engine_ctx(task_id, task_dir.clone());
        let result = tpn.execute("desc", None, &mut ctx, None).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().task_id, task_id);
        let task: Task = load_json_optional(&task_dir.join("meta.json"))
            .expect("load meta")
            .expect("meta exists");
        assert_eq!(task.status, TaskStatus::Completed);

        let _ = std::fs::remove_dir_all(&task_dir);
    }

    // ── V26.5 (P2): crash recovery rebuilds FittingAgent result from
    //    persisted state (chat_history + trace + deliverables) instead of
    //    matching `phase == "output"|"result"` records that are never written.

    static RECONSTRUCT_SEQ: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn construct_tpn_result_from_state_reconstructs_from_persisted_files() {
        let seq = RECONSTRUCT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "taiji_tpn_reconstruct_{}_{}",
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
            "tool_call::causal_verify",
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
        let result = construct_tpn_result_from_state(&ctx)
            .expect("no IO error")
            .expect("must reconstruct");

        assert_eq!(result.task_id, "reconstruct-test");
        assert_eq!(result.content, "final answer: the report is done");
        // Deduplicated, first-call order — not file order.
        assert_eq!(result.tools_used, vec!["read", "causal_verify", "write"]);
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
    fn construct_tpn_result_from_state_returns_none_without_chat_history() {
        let seq = RECONSTRUCT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "taiji_tpn_reconstruct_none_{}_{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::create_dir_all(&dir);

        let ctx = make_engine_ctx("reconstruct-none", dir.clone());
        let result = construct_tpn_result_from_state(&ctx).expect("no IO error");
        assert!(result.is_none(), "empty chat_history must yield None (fallback to re-run)");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
