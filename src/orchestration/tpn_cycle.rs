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
use crate::types::agent::{AgentMode, MetaContext};
use crate::types::execution::EngineContext;
use crate::types::frontend::{TpnPhase, YinIntervention};
use crate::types::task::{Checkpoint, CyclePhase, DecomposeResult, TPNResult};
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
    /// - `mode` — Orchestration or Execution mode.
    /// - `resume_history` — if `Some`, skip MetaAgent + checkpoint, use history
    ///   directly (parent-initiated subtask re-run).  `None` for fresh execution.
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
        mode: AgentMode,
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

        let (mut resume_phase, mut is_crash_recovery) = if resume_history_is_some {
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
            let meta_agent = self
                .factory
                .create_meta_agent(&engine_ctx.task_id)?
                .task_dir(engine_ctx.task_dir.clone());
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
                // Crash recovery: FittingAgent already ran, load last result from trace.
                // If we can't reconstruct, re-run.
                match construct_tpn_result_from_trace(&engine_ctx.task_dir) {
                    Ok(Some(result)) => result,
                    _ => {
                        tracing::warn!(
                            task_id = %engine_ctx.task_id,
                            "Could not reconstruct FittingAgent result from trace — re-running"
                        );
                        let fitting_agent = self
                            .factory
                            .create_fitting_agent(
                                engine_ctx.depth,
                                mode,
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
                        mode,
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
                            .verify(&fitting_result.content, &tool_results, &meta_ctx, mode)
                            .await?
                    }
                }
            } else {
                let verify_agent = self.factory.create_causal_verify_agent(engine_ctx)?;
                let tool_results = collect_tool_results(&engine_ctx.task_dir);
                let report = verify_agent
                    .verify(&fitting_result.content, &tool_results, &meta_ctx, mode)
                    .await?;

                // Write checkpoint after Verify.
                write_checkpoint(&checkpoint_path, CyclePhase::VerifyDone, engine_ctx, &self.cancel);

                report
            };

            // Reset resume_phase so subsequent iterations run all phases normally.
            resume_phase = None;
            is_crash_recovery = false;

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
                    let meta_agent = self
                        .factory
                        .create_meta_agent(&engine_ctx.task_id)?
                        .task_dir(engine_ctx.task_dir.clone());
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

/// Try to reconstruct a TPNResult from the trace file.
fn construct_tpn_result_from_trace(task_dir: &Path) -> Result<Option<TPNResult>, TaijiError> {
    use crate::infra::trace::TraceRecord;

    let trace_path = task_dir.join("trace.jsonl");
    let content = match std::fs::read_to_string(&trace_path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    // Read last record that is a task output (not a tool call).
    for line in content.lines().rev() {
        if let Ok(record) = serde_json::from_str::<TraceRecord>(line) {
            if record.phase == "output" || record.phase == "result" {
                let content = match &record.output {
                    serde_json::Value::String(s) => s.to_string(),
                    other => other.to_string(),
                };
                return Ok(Some(TPNResult {
                    task_id: record.task_id,
                    content,
                    tools_used: vec![],
                    deliverables: vec![],
                    depth: record.depth,
                    rounds: 0,
                }));
            }
        }
    }
    Ok(None)
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
