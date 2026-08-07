//! recursive_decompose — Core recursion tool for FittingAgent (概率拟合·阳).
//!
//! Spawns child TPN cycles per subtask, collects `TPNResult`s, calls
//! `CausalAgent.converge()` to produce a `ConvergenceDecision`, and returns
//! a `DecomposeResult` to the parent LLM.
//!
//! Each child executes a **full TPN cycle** (元·阳·阴 → loop) via
//! [`TpnCycle`], matching the isomorphic recursion principle (BCP §1.1).
//! The parent's [`MetaContext`] is passed as the initial reasoning bias so
//! that children inherit the same reasoning paths and constraints (BCP §8.2).
//!
//! Concurrency: subtasks run in parallel via `tokio::spawn`. Results are
//! collected eagerly; any subtask failure short-circuits the entire tool.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rig::completion::Message;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::infra::error::TaijiError;
use crate::infra::trace::load_json_optional;
use crate::orchestration::event_bus;
use crate::orchestration::tpn_cycle::{write_task_status, TpnCycle};
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;
use crate::types::frontend::NodeStatus;
use crate::types::task::{ChildResultSummary, DecomposeResult, SubtaskSpec, Task, TaskStatus, TPNResult};
use crate::types::verification::ConvergenceStatus;
use crate::ws::types::TaskEvent;

/// Arguments for the recursive_decompose tool.
#[derive(Debug, Deserialize)]
pub struct RecursiveDecomposeArgs {
    /// Subtasks to spawn (LLM provides these from task decomposition).
    pub subtasks: Vec<SubtaskSpec>,
}

/// Tool that recursively decomposes a parent task into subtasks.
///
/// Each subtask is executed by a fresh [`TpnCycle`] (full MetaAgent →
/// FittingAgent → CausalAgent loop).  Once all children complete, a
/// CausalAgent (converge mode) merges the partial results into a single
/// `ConvergenceDecision`.
pub struct RecursiveDecomposeTool {
    factory: Arc<AgentFactory>,
    engine_ctx: EngineContext,
    depth: u32,
    /// Cancellation token propagated to all subtasks.
    /// See AGENTS.md §1 (TPN loop rules) and §9 (concurrency rules).
    cancel: CancellationToken,
    /// Reasoning bias inherited from the parent's MetaAgent run.
    /// Passed to child TPN cycles as `initial_meta_ctx` (BCP §8.2).
    parent_meta_ctx: MetaContext,
}

impl RecursiveDecomposeTool {
    /// Create a new `RecursiveDecomposeTool`.
    ///
    /// - `factory` — shared `AgentFactory` used to spawn child agents.
    /// - `engine_ctx` — execution context of the parent task.
    /// - `depth` — current recursion depth (root = 0).
    /// - `cancel` — cancellation token checked before/during subtask spawning.
    /// - `parent_meta_ctx` — reasoning bias from the parent's MetaAgent run.
    pub fn new(
        factory: Arc<AgentFactory>,
        engine_ctx: EngineContext,
        depth: u32,
        cancel: CancellationToken,
        parent_meta_ctx: MetaContext,
    ) -> Self {
        // Guard: depth and engine_ctx.depth must agree (single source of truth).
        debug_assert_eq!(
            depth, engine_ctx.depth,
            "RecursiveDecomposeTool: depth ({depth}) != engine_ctx.depth ({}) — \
             caller must keep these in sync",
            engine_ctx.depth
        );
        Self {
            factory,
            engine_ctx,
            depth,
            cancel,
            parent_meta_ctx,
        }
    }

    /// Execute the recursive decomposition.
    ///
    /// 1. Acquires 1 WorkerPool permit (并行分解节点上限 — V26 语义：permit 在
    ///    工具入口 acquire，join 完成后释放；子任务运行不持 permit，任意深度
    ///    decompose 在各自入口 acquire，无嵌套持有 → 无死锁)。
    /// 2. Validates `depth` against `max_depth` from config.
    /// 3. Spawns one child `TpnCycle` per subtask (parallel, full TPN).
    /// 4. Collects all `TPNResult`s.
    /// 5. Converges via `CausalAgent.converge()`.
    /// 6. Returns a `DecomposeResult`.
    pub async fn execute(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError> {
        // ── Permit acquisition: tool entry, held until join completes ──
        // V26 permit 语义：permit = 并行分解节点上限。入口 acquire 1 个并持有
        // 到函数返回，spawn 闭包不再捕获 permit。持 permit 者只等待子任务 join
        // （子任务运行不持 permit），无嵌套持有 → 无死锁路径。
        let _permit = self.factory.worker_pool.acquire().await.map_err(|e| {
            TaijiError::WorkerPoolUnavailable {
                context: format!(
                    "recursive_decompose: failed to acquire worker permit for task {}: {e}",
                    self.engine_ctx.task_id
                ),
            }
        })?;

        // --- Depth guard (uses config, not hardcoded) --------------------------
        let max_depth = self.factory.config.runtime.max_depth;
        if self.depth >= max_depth {
            return Err(TaijiError::MaxDepthExceeded { max: max_depth });
        }

        // --- Max subtasks guard -------------------------------------------------
        let max_subtasks = self.factory.config.runtime.max_subtasks as usize;
        if subtasks.len() > max_subtasks {
            return Err(TaijiError::MaxSubtasksExceeded {
                max: max_subtasks,
                actual: subtasks.len(),
            });
        }

        // --- Early cancellation check --------------------------------------
        if self.cancel.is_cancelled() {
            return Err(TaijiError::Other(
                "Task cancelled before decomposition started".into(),
            ));
        }

        // --- Empty input guard ---------------------------------------------
        if subtasks.is_empty() {
            return Ok(DecomposeResult {
                task_id: self.engine_ctx.task_id.clone(),
                summary: "No subtasks provided; decomposition is trivially converged.".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
                deliverables: vec![],
                rounds: 0,
                tools_used: vec![],
                child_results: vec![],
            });
        }

        // ── Scan children/ directory for prior results ──────────────────
        let children_root = self.engine_ctx.task_dir.join("children");
        let mut prior_results: BTreeMap<usize, DecomposeResult> = BTreeMap::new();
        let mut max_existing_index: usize = 0;

        if children_root.exists() {
            if let Ok(entries) = std::fs::read_dir(&children_root) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name();
                    if let Some(idx_str) = dir_name.to_str() {
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            max_existing_index = max_existing_index.max(idx);
                            let result_path = entry.path().join("decompose_result.json");
                            // Try DecomposeResult first (new format), then TPNResult (legacy).
                            if let Ok(Some(result)) =
                                load_json_optional::<DecomposeResult>(&result_path)
                            {
                                prior_results.insert(idx, result);
                            } else if let Ok(Some(tpn)) =
                                load_json_optional::<TPNResult>(&result_path)
                            {
                                // Legacy format: map TPNResult → DecomposeResult.
                                prior_results.insert(idx, map_tpn_to_decompose(&tpn));
                            }
                        }
                    }
                }
            }
        }

        tracing::debug!(
            task_id = %self.engine_ctx.task_id,
            prior_count = prior_results.len(),
            max_idx = max_existing_index,
            "Scanned children/ for prior results"
        );

        // ── Scan parent deliverables directory for child injection ──
        let parent_deliverables: Vec<String> = {
            let dir = self.engine_ctx.task_dir.join("deliverables");
            if dir.exists() {
                std::fs::read_dir(&dir)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .map(|e| e.path().to_string_lossy().to_string())
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                vec![]
            }
        };

        // ── Compute child indices and prepare subtask metadata ──────────
        struct SubtaskMeta {
            index: usize,
            child_dir: PathBuf,
            child_deliverables: Vec<String>,
            description: String,
            resume_history: Option<Vec<Message>>,
        }

        let mut subtask_metas: Vec<SubtaskMeta> = Vec::with_capacity(subtasks.len());
        // Track which old indices have been claimed by re-runs.
        let mut claimed_rerun_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();

        for (i, subtask) in subtasks.into_iter().enumerate() {
            let (child_index, child_dir, resume_history) =
                if let Some(old_idx) = subtask.rerun_of {
                    // Re-run of an existing child.
                    if claimed_rerun_indices.contains(&old_idx) {
                        return Err(TaijiError::Other(format!(
                            "Duplicate rerun_of index {old_idx} — each child can only be re-run once"
                        )));
                    }
                    claimed_rerun_indices.insert(old_idx);

                    let dir = children_root.join(old_idx.to_string());

                    // Load old chat_history for context continuity.
                    let history: Option<Vec<Message>> = {
                        let chat_path = dir.join("chat_history.json");
                        load_json_optional::<Vec<Message>>(&chat_path)
                            .ok()
                            .flatten()
                    };

                    // Delete old checkpoint to prevent TpnCycle from mis-reading it.
                    let checkpoint_path = dir.join("checkpoint.json");
                    let _ = std::fs::remove_file(&checkpoint_path);

                    // Ensure the child directory still exists.
                    std::fs::create_dir_all(&dir).map_err(TaijiError::IO)?;

                    (old_idx, dir, history)
                } else {
                    // New child: assign index = max_existing_index + 1 + i
                    let new_idx = max_existing_index + 1 + i;
                    let dir = children_root.join(new_idx.to_string());
                    std::fs::create_dir_all(&dir).map_err(TaijiError::IO)?;
                    std::fs::create_dir_all(dir.join("deliverables")).map_err(TaijiError::IO)?;
                    (new_idx, dir, None)
                };

            // V26 起子任务与父任务完全同构：无 mode 分化（异层同构，BCP §1.1）。
            let enriched_description = assemble_child_description(
                &subtask.description,
                &subtask.verification_spec,
                &subtask.context,
            );

            subtask_metas.push(SubtaskMeta {
                index: child_index,
                child_dir,
                child_deliverables: parent_deliverables.clone(),
                description: enriched_description,
                resume_history,
            });
        }

        // --- Spawn child TPN cycles (V26: 子任务运行不持 permit — permit 已由
        // 本工具入口 acquire，见 execute() 开头) ----
        let mut join_set = tokio::task::JoinSet::new();

        for meta in subtask_metas {
            // Re-check cancellation before each spawn (AGENTS.md §1).
            if self.cancel.is_cancelled() {
                return Err(TaijiError::Other(
                    "Task cancelled during decomposition".into(),
                ));
            }

            // Clone values needed inside the spawn closure.
            let factory = Arc::clone(&self.factory);
            let engine_ctx = self.engine_ctx.clone();
            let parent_meta_ctx = self.parent_meta_ctx.clone();
            let cancel = self.cancel.clone();
            let child_deliverables = meta.child_deliverables;
            let child_dir = meta.child_dir;
            let resume_history = meta.resume_history;
            let child_index = meta.index;
            let child_description = meta.description;

            // ── Generate readable nested task_id: {slug}-{timestamp}-{index} ──
            // index is unique within this parent, so parallel children spawned
            // in the same second cannot collide (V26.6).
            let child_task_id = format!(
                "{}-{}",
                crate::infra::task_id::generate_task_id(&child_description),
                child_index
            );

            // Broadcast child spawn to frontend (frontend tree sync).
            event_bus::emit_event(TaskEvent::ChildSpawned {
                parent_task_id: self.engine_ctx.task_id.clone(),
                child_task_id: child_task_id.clone(),
                description: child_description.clone(),
                depth: engine_ctx.depth + 1,
            });

            join_set.spawn(async move {
                // Check cancellation again once the task runs.
                if cancel.is_cancelled() {
                    return (child_index, Err(TaijiError::Other(
                        "Subtask cancelled before execution".into(),
                    )));
                }

                // Build child EngineContext (同构: same fields, depth+1).
                let mut child_ctx = EngineContext {
                    task_id: child_task_id,
                    task_dir: child_dir,
                    round: 0,
                    cycle: 0,
                    depth: engine_ctx.depth + 1,
                    context_dir: None,
                };

                // ── Inject parent deliverables into child MetaContext ──
                let mut child_meta_ctx = parent_meta_ctx;
                child_meta_ctx.yang_prompt.parent_deliverables = child_deliverables;

                // ── Create CancellationToken child linked to parent ──
                let child_cancel = cancel.child_token();

                // Create a TpnCycle for the child subtask and execute
                // the full TPN cycle with the parent's MetaContext.
                let tpn_cycle =
                    TpnCycle::new(factory.clone(), factory.config.clone(), child_cancel);

                let result = tpn_cycle
                    .execute(
                        &child_description,
                        Some(child_meta_ctx),
                        &mut child_ctx,
                        resume_history,
                    )
                    .await;

                (child_index, result)
            });
        }

        // --- Collect results (streaming — fastest-first, short-circuit on error) ---
        let mut result_map: BTreeMap<usize, TPNResult> = BTreeMap::new();
        let mut success_count = 0usize;
        let total = join_set.len();

        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((idx, Ok(tpn_result))) => {
                    // Broadcast child completion to frontend.
                    event_bus::emit_event(TaskEvent::ChildCompleted {
                        child_task_id: tpn_result.task_id.clone(),
                        status: NodeStatus::Converged,
                        deliverables: tpn_result.deliverables.clone(),
                        rounds: tpn_result.rounds,
                    });
                    result_map.insert(idx, tpn_result);
                    success_count += 1;
                }
                Ok((_idx, Err(e))) => {
                    join_set.abort_all();
                    mark_aborted_children_failed(&children_root);
                    return Err(e);
                }
                Err(join_err) => {
                    join_set.abort_all();
                    mark_aborted_children_failed(&children_root);
                    return Err(TaijiError::Other(format!(
                        "Child agent task panicked: {join_err}"
                    )));
                }
            }
        }

        // ── Merge new results into prior_results ───────────────────────
        // For re-run tasks, replace the old result with the new one.
        // For new tasks, insert the new result.
        for (idx, result) in result_map {
            prior_results.insert(idx, map_tpn_to_decompose(&result));
        }

        // ── Converge via CausalAgent (with ALL results, old + new) ──────
        let converge_agent = self.factory.create_causal_converge_agent(&self.engine_ctx)?;
        let all_decompose_results: Vec<DecomposeResult> =
            prior_results.values().cloned().collect();
        let decision = converge_agent
            .converge(&all_decompose_results, &self.parent_meta_ctx)
            .await?;

        // Build child_results summary for parent LLM.
        let child_results: Vec<ChildResultSummary> = all_decompose_results
            .iter()
            .map(|r| ChildResultSummary {
                task_id: r.task_id.clone(),
                summary: r.summary.clone(),
                status: r.status.clone(),
                rounds: r.rounds,
                tools_used: r.tools_used.clone(),
                deliverables: r.deliverables.clone(),
            })
            .collect();

        let summary = format!(
            "Decomposed {total} subtask(s) ({success_count} succeeded) — converge status: {:?}",
            decision.status,
        );

        Ok(DecomposeResult {
            task_id: self.engine_ctx.task_id.clone(),
            summary,
            status: decision.status,
            subtask_count: total as u32,
            deliverables: all_decompose_results
                .iter()
                .flat_map(|r| r.deliverables.clone())
                .collect(),
            rounds: 0,
            tools_used: vec![],
            child_results,
        })

    }

    /// Alias for [`execute`](Self::execute) — satisfies the Rig `Tool` trait
    /// convention where the entry-point is named `run`.
    pub async fn run(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError> {
        self.execute(subtasks).await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Best-effort mark of every aborted child whose `meta.json` still says
/// `Running` as `Failed` (V26.3, E1).
///
/// `tokio::task::JoinSet::abort_all()` kills the child futures without running
/// their cancellation/status-writing paths, so without this step children
/// would stay `Running` forever even though they were terminated mid-flight.
/// Iterates the numeric `children/<idx>/` directories only; skips non-directory
/// entries and unreadable `meta.json` (warn only). Write failures warn and are
/// swallowed — this helper must never block error propagation in the caller.
fn mark_aborted_children_failed(children_root: &Path) {
    let entries = match std::fs::read_dir(children_root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(
                path = %children_root.display(),
                error = %e,
                "Failed to list children/ while marking aborted subtasks Failed"
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(idx_str) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if idx_str.parse::<usize>().is_err() {
            continue;
        }

        let Some(task) = load_json_optional::<Task>(&dir.join("meta.json"))
            .ok()
            .flatten()
        else {
            tracing::warn!(
                path = %dir.display(),
                "Skipping child with missing/unreadable meta.json during abort marking"
            );
            continue;
        };

        if task.status != TaskStatus::Running {
            continue;
        }

        if let Err(e) = write_task_status(
            &dir,
            &task.id,
            &task.description,
            task.depth,
            TaskStatus::Failed,
        ) {
            tracing::warn!(
                path = %dir.display(),
                error = %e,
                "Failed to mark aborted child subtask as Failed"
            );
        }
    }
}

/// Map a TPNResult into a DecomposeResult for convergence analysis.
fn map_tpn_to_decompose(result: &TPNResult) -> DecomposeResult {
    DecomposeResult {
        task_id: result.task_id.clone(),
        summary: result.content.clone(),
        status: ConvergenceStatus::Converged,
        subtask_count: 0,
        deliverables: result.deliverables.clone(),
        rounds: result.rounds,
        tools_used: result.tools_used.clone(),
        child_results: vec![],
    }
}

/// Assemble a child task description that includes the parent LLM's
/// verification specification and per-subtask context.
///
/// This keeps the TpnCycle signature unchanged while giving child agents
/// full visibility into what the parent expected.
fn assemble_child_description(
    description: &str,
    verification_spec: &str,
    context: &serde_json::Value,
) -> String {
    let mut parts = vec![format!("{description}")];

    if !verification_spec.is_empty() {
        parts.push(format!("\n## Verification Criteria\n{verification_spec}"));
    }

    if !context.is_null() {
        let ctx_str = serde_json::to_string_pretty(context).unwrap_or_default();
        if !ctx_str.is_empty() && ctx_str != "null" && ctx_str != "{}" {
            parts.push(format!("\n## Additional Context\n{ctx_str}"));
        }
    }

    parts.join("\n")
}

// ── Rig Tool implementation ─────────────────────────────────────────────

impl Tool for RecursiveDecomposeTool {
    const NAME: &'static str = "recursive_decompose";

    type Error = TaijiError;
    type Args = RecursiveDecomposeArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Recursively decompose a task into subtasks. Each subtask runs a full TPN cycle (MetaAgent → FittingAgent → CausalAgent). Returns a JSON-serialized DecomposeResult.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "subtasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "description": {
                                    "type": "string",
                                    "description": "Description of the subtask"
                                },
                                "verification_spec": {
                                    "type": "string",
                                    "description": "Specification for verifying the subtask result"
                                },
                                "context": {
                                    "type": "object",
                                    "description": "Additional context for the subtask"
                                },
                                "rerun_of": {
                                    "type": "integer",
                                    "description": "Optional: index of an existing child subtask to re-run. When set, the child reuses its existing directory, loads prior chat history for continuity, and the old checkpoint is deleted before re-execution. Use this when a previous sub-decompose needs retrying with adjusted parameters. Leave unset for new subtasks."
                                }
                            },
                            "required": ["description", "verification_spec"]
                        },
                        "description": "List of subtasks to execute"
                    }
                },
                "required": ["subtasks"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let result = self.execute(args.subtasks).await?;
        serde_json::to_string(&result).map_err(TaijiError::Serde)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assemble_child_description_full() {
        let desc = "Write an add function";
        let vspec = "Check overflow, zero, and negative cases";
        let ctx = serde_json::json!({"target_file": "src/lib.rs"});

        let result = assemble_child_description(desc, vspec, &ctx);
        assert!(result.contains("Write an add function"));
        assert!(result.contains("Check overflow, zero, and negative cases"));
        assert!(result.contains("src/lib.rs"));
    }

    #[test]
    fn test_assemble_child_description_empty_spec() {
        let result = assemble_child_description("Do task", "", &serde_json::Value::Null);
        assert_eq!(result, "Do task");
    }

    #[test]
    fn test_assemble_child_description_empty_context() {
        let result = assemble_child_description("Do task", "Verify", &serde_json::json!({}));
        assert!(result.contains("Do task"));
        assert!(result.contains("Verify"));
        assert!(!result.contains("Additional Context"));
    }

    // ── mark_aborted_children_failed (V26.3 E1) ─────────────────────────

    fn make_child_meta(dir: &Path, status: TaskStatus) {
        let task = Task {
            id: format!("child-{}", dir.file_name().unwrap().to_string_lossy()),
            description: "child task".into(),
            depth: 1,
            status,
            parent_id: Some("parent".into()),
            subtask_ids: vec![],
        };
        std::fs::write(
            dir.join("meta.json"),
            serde_json::to_string_pretty(&task).expect("serialize task"),
        )
        .expect("write meta.json");
    }

    #[test]
    fn test_mark_aborted_children_failed_flips_running_only() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "recursive_decompose_abort_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let children_root = tmp_dir.join("children");

        let running_dir = children_root.join("1");
        let completed_dir = children_root.join("2");
        let missing_meta_dir = children_root.join("3");
        std::fs::create_dir_all(&running_dir).expect("create children/1");
        std::fs::create_dir_all(&completed_dir).expect("create children/2");
        std::fs::create_dir_all(&missing_meta_dir).expect("create children/3");
        std::fs::create_dir_all(children_root.join("not-a-number")).expect("create non-numeric dir");
        std::fs::write(children_root.join("stray-file"), b"x").expect("stray file");

        make_child_meta(&running_dir, TaskStatus::Running);
        make_child_meta(&completed_dir, TaskStatus::Completed);

        mark_aborted_children_failed(&children_root);

        let running_meta: Task =
            serde_json::from_str(&std::fs::read_to_string(running_dir.join("meta.json")).unwrap())
                .expect("read running meta");
        assert_eq!(running_meta.status, TaskStatus::Failed);

        let completed_meta: Task =
            serde_json::from_str(&std::fs::read_to_string(completed_dir.join("meta.json")).unwrap())
                .expect("read completed meta");
        assert_eq!(completed_meta.status, TaskStatus::Completed, "non-Running untouched");

        // Missing meta.json must not panic; non-numeric dirs/files skipped.
        assert!(!children_root.join("3").join("meta.json").exists());

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_mark_aborted_children_failed_missing_root_is_noop() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "recursive_decompose_abort_missing_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let children_root = tmp_dir.join("children");

        mark_aborted_children_failed(&children_root);

        assert!(!children_root.exists());
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
