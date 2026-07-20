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

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::infra::error::TaijiError;
use crate::orchestration::tpn_cycle::TpnCycle;
use crate::types::agent::{AgentMode, MetaContext};
use crate::types::execution::EngineContext;
use crate::types::task::{DecomposeResult, SubtaskSpec, TPNResult};
use crate::types::verification::ConvergenceStatus;

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
    /// The mode of the parent FittingAgent — used for converge and passed to children.
    mode: AgentMode,
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
        mode: AgentMode,
    ) -> Self {
        Self {
            factory,
            engine_ctx,
            depth,
            cancel,
            parent_meta_ctx,
            mode,
        }
    }

    /// Execute the recursive decomposition.
    ///
    /// 1. Validates `depth` against `max_depth` from config.
    /// 2. Spawns one child `TpnCycle` per subtask (parallel, full TPN).
    /// 3. Collects all `TPNResult`s.
    /// 4. Converges via `CausalAgent.converge()`.
    /// 5. Returns a `DecomposeResult`.
    pub async fn execute(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError> {
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
                summary: "No subtasks provided; decomposition is trivially converged.".into(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
                deliverables: vec![],
            });
        }

        // --- Spawn child TPN cycles with WorkerPool concurrency limiting ----
        let mut handles = Vec::with_capacity(subtasks.len());

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

        for (i, subtask) in subtasks.into_iter().enumerate() {
            // Re-check cancellation before each spawn (AGENTS.md §1).
            if self.cancel.is_cancelled() {
                return Err(TaijiError::Other(
                    "Task cancelled during decomposition".into(),
                ));
            }

            // Acquire a semaphore permit from the shared WorkerPool to
            // ensure global concurrency limits (AGENTS.md §9).
            let permit = self.factory.worker_pool.acquire().await;

            // Determine child mode:
            // - Leaf nodes (depth+1 >= max_depth) are FORCED to Execution
            // - Intermediate nodes use the parent LLM's SubtaskSpec.mode
            let child_depth = self.engine_ctx.depth + 1;
            let actual_mode = if child_depth >= max_depth {
                AgentMode::Execution
            } else {
                subtask.mode
            };

            // ── Create child directory (同构布局) ──────────────────────
            let child_dir = self.engine_ctx.task_dir.join("children").join(i.to_string());
            std::fs::create_dir_all(&child_dir).map_err(TaijiError::IO)?;
            std::fs::create_dir_all(child_dir.join("deliverables")).map_err(TaijiError::IO)?;

            // Clone values needed inside the spawn closure.
            let factory = Arc::clone(&self.factory);
            let engine_ctx = self.engine_ctx.clone();
            let parent_meta_ctx = self.parent_meta_ctx.clone();
            let cancel = self.cancel.clone();
            let child_mode = actual_mode;
            let child_deliverables = parent_deliverables.clone();

            handles.push(tokio::spawn(async move {
                // Check cancellation again once the task runs.
                if cancel.is_cancelled() {
                    return Err(TaijiError::Other(
                        "Subtask cancelled before execution".into(),
                    ));
                }

                // ── Generate independent UUID for child (嵌套 task_id) ──
                let child_task_id = uuid::Uuid::new_v4().to_string();

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
                // Children need to know which files the parent already produced
                // so they can reference/extend them (BCP §8.9).
                let mut child_meta_ctx = parent_meta_ctx;
                child_meta_ctx.yang_prompt.parent_deliverables = child_deliverables;

                // ── Create CancellationToken child linked to parent ──
                let child_cancel = cancel.child_token();

                // Create a TpnCycle for the child subtask and execute
                // the full TPN cycle with the parent's MetaContext.
                let tpn_cycle = TpnCycle::new(factory.clone(), factory.config.clone(), child_cancel);

                let result = tpn_cycle
                    .execute(
                        &subtask.description,
                        Some(child_meta_ctx),
                        &mut child_ctx,
                        child_mode,
                    )
                    .await;

                // Hold the permit until the subtask completes, then
                // release it back to the WorkerPool semaphore.
                drop(permit);

                result
            }));
        }

        // --- Collect results (short-circuit on first error) ----------------
        let mut tpn_results: Vec<TPNResult> = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok(Ok(result)) => tpn_results.push(result),
                Ok(Err(e)) => return Err(e),
                Err(join_err) => {
                    return Err(TaijiError::Other(format!(
                        "Child agent task panicked: {join_err}"
                    )));
                }
            }
        }

        // --- Converge via CausalAgent --------------------------------------
        let converge_agent = self.factory.create_causal_converge_agent(&self.engine_ctx)?;
        // Map TPNResults to DecomposeResults for convergence
        let decompose_results: Vec<DecomposeResult> = tpn_results
            .iter()
            .map(|r| DecomposeResult {
                summary: r.content.clone(),
                status: ConvergenceStatus::Converged,
                subtask_count: 0,
                deliverables: r.deliverables.clone(),
            })
            .collect();
        let decision = converge_agent.converge(&decompose_results, &self.parent_meta_ctx, self.mode).await?;

        let summary = format!(
            "Decomposed {} subtask(s) — status: {:?}",
            tpn_results.len(),
            decision.status,
        );

        Ok(DecomposeResult {
            summary,
            status: decision.status,
            subtask_count: tpn_results.len() as u32,
            // Aggregate all child deliverables upward.
            deliverables: tpn_results
                .iter()
                .flat_map(|r| r.deliverables.clone())
                .collect(),
        })
    }

    /// Alias for [`execute`](Self::execute) — satisfies the Rig `Tool` trait
    /// convention where the entry-point is named `run`.
    pub async fn run(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError> {
        self.execute(subtasks).await
    }
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
                                "mode": {
                                    "type": "string",
                                    "enum": ["Orchestration", "Execution"],
                                    "description": "Whether the subtask runs in Orchestration (further decomposition) or Execution (direct work) mode. Leaf-depth subtasks are forced to Execution automatically."
                                }
                            },
                            "required": ["description", "verification_spec", "mode"]
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
        serde_json::to_string(&result).map_err(|e| TaijiError::Serde(e))
    }
}
