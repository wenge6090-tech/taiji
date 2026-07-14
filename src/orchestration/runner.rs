//! RecursiveRunner — thin wrapper: task dir init + spawn root FittingAgent.
//! The actual recursion is owned by FittingAgent's recursive_decompose tool.

use std::sync::Arc;

use crate::agents::factory::AgentFactory;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::types::execution::EngineContext;
use crate::types::task::{Task, TaskStatus, TPNResult};
use crate::types::verification::VerificationRoute;
use tokio::time::{timeout, Duration};

/// Thin wrapper around the root-level TPN execution loop.
///
/// Responsibilities:
/// - Create task directory and initial metadata
/// - Bootstrap an [`EngineContext`] at depth 0
/// - Delegate to MetaAgent (权重更新·元) for reasoning-path extraction
/// - Spawn root FittingAgent (概率拟合·阳) with the resulting [`MetaContext`]
/// - Persist final task status on completion
///
/// Recursive decomposition is handled **within** FittingAgent via the
/// `recursive_decompose` tool — this runner only owns the root invocation.
#[derive(Debug)]
pub struct RecursiveRunner {
    factory: Arc<AgentFactory>,
    config: TaijiConfig,
}

impl RecursiveRunner {
    /// Create a new runner bound to a specific factory and configuration.
    pub fn new(factory: Arc<AgentFactory>, config: TaijiConfig) -> Self {
        Self { factory, config }
    }

    /// Execute a task description end-to-end.
    ///
    /// 1. Allocate a UUID v4 task ID
    /// 2. Create `{data_root}/tasks/{id}/` with a `deliverables/` subdirectory
    /// 3. Persist initial `meta.json` (status = `Running`)
    /// 4. Build an [`EngineContext`] at depth 0, cycle 0, round 0
    /// 5. Run MetaAgent to obtain [`MetaContext`] (weighted reasoning paths)
    /// 6. Run FittingAgent with the description and MetaContext
    /// 7. Mark task as `Completed` in `meta.json`
    /// 8. Return the [`TPNResult`]
    pub async fn execute(&self, description: &str) -> Result<TPNResult, TaijiError> {
        let task_id = uuid::Uuid::new_v4().to_string();
        tracing::info!(task_id, "Starting task execution");

        let task_dir = self.factory.task_dir(&task_id);

        // ── 1. Create directory structure ──────────────────────────────
        std::fs::create_dir_all(&task_dir).map_err(TaijiError::IO)?;
        std::fs::create_dir_all(task_dir.join("deliverables")).map_err(TaijiError::IO)?;

        // ── 2. Write initial task metadata ─────────────────────────────
        let mut task = Task {
            id: task_id.clone(),
            description: description.to_string(),
            depth: 0,
            status: TaskStatus::Running,
            parent_id: None,
            subtask_ids: vec![],
        };
        let meta_path = task_dir.join("meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&task)?)
            .map_err(TaijiError::IO)?;

        // ── 3. EngineContext at root ───────────────────────────────────
        let mut engine_ctx = EngineContext {
            task_id: task_id.clone(),
            depth: 0,
            task_dir: task_dir.clone(),
            cycle: 0,
            round: 0,
        };

        // ── 4-6. TPN loop: MetaAgent → FittingAgent → CausalVerify ────
        let timeout_secs = self.config.runtime.exec_timeout;
        timeout(Duration::from_secs(timeout_secs), async {
            loop {
                // Update meta.json status to Running at each iteration
                task.status = TaskStatus::Running;
                std::fs::write(&meta_path, serde_json::to_string_pretty(&task)?)
                    .map_err(TaijiError::IO)?;

                // Phase 4: MetaAgent (权重更新·元)
                let meta_agent = self.factory.create_meta_agent(&task_id)?;
                let meta_ctx = meta_agent.run().await?;

                // Phase 5: FittingAgent (概率拟合·阳)
                let fitting_agent =
                    self.factory
                        .create_fitting_agent(0, &meta_ctx, &engine_ctx)?;
                let result = fitting_agent.run(description).await?;

                // Phase 6: CausalVerify (因果验证·阴)
                let verify_agent = self.factory.create_causal_verify_agent(&engine_ctx)?;
                let report = verify_agent.verify(&result.content, &[]).await?;

                match report.route {
                    VerificationRoute::Pass => {
                        task.status = TaskStatus::Completed;
                        std::fs::write(&meta_path, serde_json::to_string_pretty(&task)?)
                            .map_err(TaijiError::IO)?;
                        tracing::info!(task_id, "TPN cycle passed");
                        return Ok(result);
                    }
                    VerificationRoute::BackToTpn => {
                        engine_ctx.round += 1;
                        if engine_ctx.round > self.config.runtime.max_rounds {
                            return Err(TaijiError::MaxRoundsExceeded {
                                max: self.config.runtime.max_rounds,
                            });
                        }
                        tracing::warn!(
                            round = engine_ctx.round,
                            "BACK_TO_TPN — retrying FittingAgent"
                        );
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
                        tracing::warn!(
                            cycle = engine_ctx.cycle,
                            "BACK_TO_META — retrying MetaAgent"
                        );
                        continue;
                    }
                }
            }
        })
        .await
        .map_err(|_| TaijiError::Other("Task execution timed out".into()))?
    }
}
