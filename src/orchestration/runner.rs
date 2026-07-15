//! RecursiveRunner — thin wrapper: task dir init + TpnCycle delegation.
//!
//! The actual TPN loop (MetaAgent → FittingAgent → CausalAgent → route) is
//! owned by [`TpnCycle`].  This runner is responsible for:
//!
//! 1. Creating the task directory and initial `meta.json`.
//! 2. Delegating to [`TpnCycle::execute`] inside a `tokio::timeout`.
//! 3. Updating task status on completion.

use std::sync::Arc;

use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::orchestration::tpn_cycle::TpnCycle;
use crate::types::agent::AgentMode;
use crate::types::execution::EngineContext;
use crate::types::task::{Task, TaskStatus, TPNResult};

/// Thin wrapper around the root-level TPN execution loop.
///
/// Responsibilities:
/// - Create task directory and initial metadata
/// - Bootstrap an [`EngineContext`] at depth 0
/// - Delegate to [`TpnCycle`] for the actual TPN loop
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
    /// 5. Delegate to [`TpnCycle::execute`] with `initial_meta_ctx = None`
    ///    (the cycle runs MetaAgent internally to obtain reasoning paths)
    /// 6. Mark task as `Completed` in `meta.json`
    /// 7. Return the [`TPNResult`]
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

        // ── 4. CancellationToken for the entire execution tree ────────
        let cancel = CancellationToken::new();

        // ── 5. TPN cycle via TpnCycle ─────────────────────────────────
        let tpn_cycle = TpnCycle::new(self.factory.clone(), self.config.clone(), cancel);
        let timeout_secs = self.config.runtime.exec_timeout;
        let result = timeout(
            Duration::from_secs(timeout_secs),
            tpn_cycle.execute(description, None, &mut engine_ctx, AgentMode::Orchestration),
        )
        .await
        .map_err(|_| TaijiError::Other("Task execution timed out".into()))??;

        // ── 5. Mark completed ──────────────────────────────────────────
        task.status = TaskStatus::Completed;
        std::fs::write(&meta_path, serde_json::to_string_pretty(&task)?)
            .map_err(TaijiError::IO)?;

        tracing::info!(task_id, "Task completed successfully");
        Ok(result)
    }
}
