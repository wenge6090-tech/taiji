//! recursive_decompose — Core recursion tool for FittingAgent (概率拟合·阳).
//!
//! Spawns child FittingAgents per subtask, collects `TPNResult`s, calls
//! `CausalAgent.converge()` to produce a `ConvergenceDecision`, and returns
//! a `DecomposeResult` to the parent LLM.
//!
//! Concurrency: subtasks run in parallel via `tokio::spawn`. Results are
//! collected eagerly; any subtask failure short-circuits the entire tool.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::infra::error::TaijiError;
use crate::types::agent::{MetaContext, YangPrompt};
use crate::types::execution::EngineContext;
use crate::types::task::{DecomposeResult, SubtaskSpec, TPNResult};
use crate::types::verification::ConvergenceStatus;

/// Tool that recursively decomposes a parent task into subtasks.
///
/// Each subtask is executed by a fresh child FittingAgent. Once all children
/// complete, a CausalAgent (converge mode) merges the partial results into a
/// single `ConvergenceDecision`.
pub struct RecursiveDecomposeTool {
    factory: Arc<AgentFactory>,
    engine_ctx: EngineContext,
    depth: u32,
    /// Cancellation token propagated to all subtasks.
    /// See AGENTS.md §1 (TPN loop rules) and §9 (concurrency rules).
    cancel: CancellationToken,
}

impl RecursiveDecomposeTool {
    /// Create a new `RecursiveDecomposeTool`.
    ///
    /// - `factory` — shared `AgentFactory` used to spawn child agents.
    /// - `engine_ctx` — execution context of the parent task.
    /// - `depth` — current recursion depth (root = 0).
    /// - `cancel` — cancellation token checked before/during subtask spawning.
    pub fn new(
        factory: Arc<AgentFactory>,
        engine_ctx: EngineContext,
        depth: u32,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            factory,
            engine_ctx,
            depth,
            cancel,
        }
    }

    /// Execute the recursive decomposition.
    ///
    /// 1. Validates `depth` against `MAX_DEPTH` (hardcoded to 3; will be
    ///    read from `TaijiConfig` once the factory exposes it).
    /// 2. Spawns one child `FittingAgent` per subtask (parallel).
    /// 3. Collects all `TPNResult`s.
    /// 4. Converges via `CausalAgent.converge()`.
    /// 5. Returns a `DecomposeResult`.
    pub async fn execute(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError> {
        // --- Depth guard ---------------------------------------------------
        const MAX_DEPTH: u32 = 3;
        if self.depth >= MAX_DEPTH {
            return Err(TaijiError::MaxDepthExceeded { max: MAX_DEPTH });
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
            });
        }

        // --- Spawn child FittingAgents with WorkerPool concurrency limiting -
        let mut handles = Vec::with_capacity(subtasks.len());

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

            // ── Create child directory (同构布局) ──────────────────────
            let child_dir = self.engine_ctx.task_dir.join("children").join(i.to_string());
            std::fs::create_dir_all(&child_dir).map_err(TaijiError::IO)?;
            std::fs::create_dir_all(child_dir.join("deliverables")).map_err(TaijiError::IO)?;

            let factory = Arc::clone(&self.factory);
            let engine_ctx = self.engine_ctx.clone();
            let cancel = self.cancel.clone();

            handles.push(tokio::spawn(async move {
                // Check cancellation again once the task runs.
                if cancel.is_cancelled() {
                    return Err(TaijiError::Other(
                        "Subtask cancelled before execution".into(),
                    ));
                }

                let child_ctx = EngineContext {
                    task_dir: child_dir,
                    round: 0,
                    cycle: 0,
                    depth: engine_ctx.depth + 1,
                    ..engine_ctx
                };

                let default_meta_ctx = MetaContext {
                    reasoning_paths: vec![],
                    constraints: vec![],
                    matched_skills: vec![],
                    yang_prompt: YangPrompt {
                        task_description: subtask.description.clone(),
                        reasoning_path_summaries: vec![],
                        constraint_summaries: vec![],
                    },
                };

                let agent = factory.create_fitting_agent(
                    child_ctx.depth,
                    &default_meta_ctx,
                    &child_ctx,
                )?;

                let result = agent.run(&subtask.description).await;

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
                status: crate::types::verification::ConvergenceStatus::Converged,
                subtask_count: 0,
            })
            .collect();
        let decision = converge_agent.converge(&decompose_results).await?;

        let summary = format!(
            "Decomposed {} subtask(s) — status: {:?}",
            tpn_results.len(),
            decision.status,
        );

        Ok(DecomposeResult {
            summary,
            status: decision.status,
            subtask_count: tpn_results.len() as u32,
        })
    }

    /// Alias for [`execute`](Self::execute) — satisfies the Rig `Tool` trait
    /// convention where the entry-point is named `run`.
    pub async fn run(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError> {
        self.execute(subtasks).await
    }
}
