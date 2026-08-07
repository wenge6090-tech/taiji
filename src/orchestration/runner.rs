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
use crate::infra::trace::load_json_optional;
use crate::orchestration::event_bus;
use crate::orchestration::tpn_cycle::{write_task_status, TpnCycle};
use crate::types::agent::ExternalContext;
use crate::types::execution::EngineContext;
use crate::types::frontend::NodeStatus;
use crate::types::task::{Task, TaskStatus, TPNResult};
use crate::ws::types::TaskEvent;

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

    /// Execute a task description end-to-end with optional external context
    /// from a frontend agent (e.g. any MCP-compatible frontend agent).
    ///
    /// Same as [`execute`](Self::execute) but also materialises the external
    /// context (files, tool results, session summary) into `task_dir/context/`
    /// and sets `engine_ctx.context_dir` so FittingAgent can reference it.
    ///
    /// # Resume (V26)
    /// `resume_task_id: Some(id)` reuses the existing task directory instead of
    /// creating a new one: the task_id is kept, `depth` is read back from
    /// `meta.json`, and `TpnCycle` walks the standard recovery chain
    /// (resume_history > decompose_result.json > checkpoint.json).  Root and
    /// child tasks therefore share the exact same recovery code path.
    pub async fn execute_with_context(
        &self,
        description: &str,
        external_ctx: Option<ExternalContext>,
        resume_task_id: Option<String>,
    ) -> Result<TPNResult, TaijiError> {
        // ── 1. Resolve task identity: resume reuses, fresh generates ──
        let (task_id, task_dir, resume_depth) = match &resume_task_id {
            Some(id) => {
                let dir = self.factory.task_dir(id);
                let depth = load_json_optional::<Task>(&dir.join("meta.json"))
                    .ok()
                    .flatten()
                    .map(|t| t.depth)
                    .unwrap_or(0);
                tracing::info!(task_id = %id, depth, "Resuming task execution");
                (id.clone(), dir, depth)
            }
            None => {
                // V26.6: human-readable id `{slug}-{YYYYMMDD-HHMMSS}`;
                // ensure_unique appends `-2/-3` when the tasks/ dir collides
                // (same second + same slug).
                let id = crate::infra::task_id::generate_task_id(description);
                let id = crate::infra::task_id::ensure_unique(id, |candidate| {
                    self.factory.task_dir(candidate).exists()
                });
                let dir = self.factory.task_dir(&id);
                tracing::info!(task_id = %id, "Starting task execution with context");
                (id, dir, 0)
            }
        };

        // ── 1.1 Create directory structure ─────────────────────────────
        std::fs::create_dir_all(&task_dir).map_err(TaijiError::IO)?;
        std::fs::create_dir_all(task_dir.join("deliverables")).map_err(TaijiError::IO)?;

        // ── 2. Materialise external context (if provided) ──────────────
        let context_dir = if let Some(ref ctx) = external_ctx {
            let ctx_dir = task_dir.join("context");
            let files_dir = ctx_dir.join("files");
            std::fs::create_dir_all(&files_dir).map_err(TaijiError::IO)?;

            // Write each external file as an indexed blob
            for (i, file) in ctx.files.iter().enumerate() {
                let file_path = files_dir.join(i.to_string());
                std::fs::write(&file_path, &file.content).map_err(TaijiError::IO)?;
            }

            // Write context metadata
            let meta_path = ctx_dir.join("meta.json");
            std::fs::write(&meta_path, serde_json::to_string_pretty(ctx)?)
                .map_err(TaijiError::IO)?;

            Some(ctx_dir)
        } else {
            None
        };

        // ── 3. Write initial task metadata ─────────────────────────────
        let meta_path = task_dir.join("meta.json");
        if load_json_optional::<Task>(&meta_path)
            .ok()
            .flatten()
            .is_none()
        {
            let task = Task {
                id: task_id.clone(),
                description: description.to_string(),
                depth: 0,
                status: TaskStatus::Running,
                parent_id: None,
                subtask_ids: vec![],
            };
            std::fs::write(&meta_path, serde_json::to_string_pretty(&task)?)
                .map_err(TaijiError::IO)?;
        }

        // ── 3.1 Broadcast task creation (frontend auto-popup) ─────────
        // Skipped on resume: re-broadcasting would replace the whole root tree.
        if resume_task_id.is_none() {
            event_bus::emit_event(TaskEvent::TaskCreated {
                task_id: task_id.clone(),
                description: description.to_string(),
                parent_id: None,
                depth: 0,
            });
        }

        // ── 4. EngineContext ───────────────────────────────────────────
        let mut engine_ctx = EngineContext {
            task_id: task_id.clone(),
            depth: resume_depth,
            task_dir: task_dir.clone(),
            cycle: 0,
            round: 0,
            context_dir,
        };

        // ── 5. CancellationToken for the entire execution tree ────────
        let cancel = CancellationToken::new();

        // ── 6. TPN cycle via TpnCycle ─────────────────────────────────
        let tpn_cycle = TpnCycle::new(self.factory.clone(), self.config.clone(), cancel.clone());
        let timeout_secs = self.config.runtime.exec_timeout;
        let result = match timeout(
            Duration::from_secs(timeout_secs),
            tpn_cycle.execute(description, None, &mut engine_ctx, None),
        )
        .await
        {
            Ok(inner) => inner,
            Err(_) => {
                // ── Timeout: cancel the execution tree + persist Failed.
                //    TpnCycle is dropped with the timeout, so status is
                //    written here (V26 统一状态管理).
                cancel.cancel();
                tracing::error!(task_id = %task_id, "Task execution timed out");
                // V26.5-P3-F1: the timeout drops the TpnCycle future, which
                // drops the RecursiveDecomposeTool JoinSet → tokio aborts all
                // in-flight subtasks WITHOUT running their status-writing
                // paths, so `children/<idx>/meta.json` would stay `Running`
                // forever. Flip any still-Running children to Failed here
                // (best-effort; warn-only on failure).
                crate::agents::tools::recursive_decompose::mark_aborted_children_failed(
                    &task_dir.join("children"),
                );
                let _ = write_task_status(
                    &task_dir,
                    &task_id,
                    description,
                    resume_depth,
                    TaskStatus::Failed,
                );
                event_bus::emit_event(TaskEvent::TaskStatusChanged {
                    task_id: task_id.clone(),
                    old_status: NodeStatus::Running,
                    new_status: NodeStatus::Failed,
                });
                return Err(TaijiError::Other(
                    "Task execution timed out; status persisted as Failed".into(),
                ));
            }
        }?;

        // ── 7. Broadcast completion (frontend turns node green) ──────
        event_bus::emit_event(TaskEvent::TaskStatusChanged {
            task_id: task_id.clone(),
            old_status: NodeStatus::Running,
            new_status: NodeStatus::Converged,
        });

        tracing::info!(task_id, "Task completed successfully");
        Ok(result)
    }

    /// Execute a task description end-to-end (no external context).
    ///
    /// Delegates to [`execute_with_context`](Self::execute_with_context) with
    /// `external_ctx = None` and `resume_task_id = None`.
    pub async fn execute(&self, description: &str) -> Result<TPNResult, TaijiError> {
        self.execute_with_context(description, None, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::factory::AgentFactory;
    use crate::hooks::safety::SafetyHook;
    use crate::infra::config::{LlmConfig, RuntimeConfig, SafetyConfig, TaijiConfig};
    use crate::infra::knowledge::LiluoClient;
    use crate::infra::provider::ProviderRegistry;
    use crate::infra::trace::save_json_atomic;
    use crate::orchestration::constraint_engine::ConstraintEngine;
    use crate::orchestration::trigger_engine::SkillTriggerEngine;
    use crate::orchestration::worker_pool::WorkerPool;
    use crate::types::task::{Checkpoint, CyclePhase, TaskStatus, TPNResult};

    fn make_config(tmp_root: &std::path::Path) -> TaijiConfig {
        TaijiConfig {
            version: "0.1.0".into(),
            workspace: "default".into(),
            data_root: tmp_root.to_string_lossy().into_owned(),
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

    async fn build_factory(config: &TaijiConfig) -> Arc<AgentFactory> {
        let knowledge_dir = std::env::temp_dir().join(format!(
            "taiji_runner_knowledge_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&knowledge_dir);
        let liluo = Arc::new(
            LiluoClient::new(&knowledge_dir).await.expect("LiluoClient should initialise"),
        );
        let providers = ProviderRegistry::new(config).expect("ProviderRegistry");
        Arc::new(AgentFactory {
            liluo,
            providers: Arc::new(providers),
            config: config.clone(),
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            worker_pool: Arc::new(WorkerPool::new(4)),
            constraint_engine: Arc::new(ConstraintEngine::new()),
            trigger_engine: Arc::new(SkillTriggerEngine::new()),
            data_root: std::path::PathBuf::from(&config.data_root),
        })
    }

    fn write_meta(dir: &std::path::Path, id: &str, depth: u32, status: TaskStatus) {
        let task = Task {
            id: id.into(),
            description: "original description".into(),
            depth,
            status,
            parent_id: None,
            subtask_ids: vec![],
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&task).unwrap())
            .expect("write meta.json");
    }

    #[tokio::test]
    async fn test_resume_reuses_task_id_depth_and_recovery_chain() {
        // V26 根任务恢复入口：resume_task_id=Some 时复用 task_id（不生成新
        // UUID）、从 meta.json 读 depth、走标准恢复链（checkpoint →
        // decompose_result.json 缓存，不触发 LLM），最终 Completed 落盘。
        let tmp_root = std::env::temp_dir().join(format!("taiji_runner_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_root);
        std::fs::create_dir_all(&tmp_root).expect("create tmp root");

        let config = make_config(&tmp_root);
        let factory = build_factory(&config).await;

        // 预置失败任务：meta.json（depth=2）+ checkpoint + decompose_result
        // （模拟中断后未删除的完成标记 —— 走缓存恢复路径）。
        let task_id = "resume-me".to_string();
        let task_dir = factory.task_dir(&task_id);
        std::fs::create_dir_all(&task_dir).expect("create task dir");
        write_meta(&task_dir, &task_id, 2, TaskStatus::Failed);

        let checkpoint = Checkpoint {
            phase: CyclePhase::VerifyDone,
            round: 0,
            cycle: 0,
        };
        save_json_atomic(&checkpoint, &task_dir.join("checkpoint.json")).expect("checkpoint");

        let cached = TPNResult {
            task_id: task_id.clone(),
            content: "cached output".into(),
            tools_used: vec![],
            deliverables: vec![],
            depth: 2,
            rounds: 1,
        };
        save_json_atomic(&cached, &task_dir.join("decompose_result.json")).expect("decompose");

        // ── 执行 resume ──
        let runner = RecursiveRunner::new(factory, config);
        let result = runner
            .execute_with_context("new description", None, Some(task_id.clone()))
            .await;

        // task_id 复用（非新 UUID）
        assert!(result.is_ok(), "resume failed: {:?}", result.err());
        assert_eq!(result.unwrap().task_id, task_id);

        // meta.json：depth 保留（来自恢复读回），status 落盘 Completed
        let task: Task = load_json_optional(&task_dir.join("meta.json"))
            .expect("load meta")
            .expect("meta exists");
        assert_eq!(task.id, task_id);
        assert_eq!(task.depth, 2, "depth 应从 meta.json 恢复");
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.description, "original description", "meta.json 不应被重建");

        let _ = std::fs::remove_dir_all(&tmp_root);
    }
}
