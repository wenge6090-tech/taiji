//! recursive_decompose — Core recursion tool for YangAgent (概率拟合·阳).
//!
//! Spawns child Zhouyi cycles per subtask, collects `ZhouyiResult`s, calls
//! `YinAgent.converge()` to produce a `ConvergenceDecision`, and returns
//! a `DecomposeResult` to the parent LLM.
//!
//! Each child executes a **full Zhouyi cycle** (元·阳·阴 → loop) via
//! [`ZhouyiCycle`], matching the isomorphic recursion principle (BCP §1.1).
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
use crate::orchestration::zhouyi::{write_task_status, ZhouyiCycle};
use crate::types::agent::{AgentMode, MetaContext};
use crate::types::execution::EngineContext;
use crate::types::frontend::NodeStatus;
use crate::types::task::{ChildResultSummary, DecomposeResult, SubtaskSpec, Task, TaskStatus, ZhouyiResult};
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
/// Each subtask is executed by a fresh [`ZhouyiCycle`] (full MetaAgent →
/// YangAgent → YinAgent loop).  Once all children complete, a
/// YinAgent (converge mode) merges the partial results into a single
/// `ConvergenceDecision`.
pub struct RecursiveDecomposeTool {
    factory: Arc<AgentFactory>,
    engine_ctx: EngineContext,
    depth: u32,
    /// 当前节点的阴阳配对模式（V27）：仅 Orchestration 模式注册本工具，
    /// 内部 mode guard 兑底（Execution 模式调用直接拒绝）。
    mode: AgentMode,
    /// Cancellation token propagated to all subtasks.
    /// See AGENTS.md §1 (Zhouyi loop rules) and §9 (concurrency rules).
    cancel: CancellationToken,
    /// Reasoning bias inherited from the parent's MetaAgent run.
    /// Passed to child Zhouyi cycles as `initial_meta_ctx` (BCP §8.2).
    parent_meta_ctx: MetaContext,
}

impl RecursiveDecomposeTool {
    /// Create a new `RecursiveDecomposeTool`.
    ///
    /// - `factory` — shared `AgentFactory` used to spawn child agents.
    /// - `engine_ctx` — execution context of the parent task.
    /// - `depth` — current recursion depth (root = 0).
    /// - `mode` — the parent node's 阴阳配对模式 (V27). Execution-mode agents
    ///   never see this tool (not registered); the guard here is a
    ///   belt-and-suspenders second line.
    /// - `cancel` — cancellation token checked before/during subtask spawning.
    /// - `parent_meta_ctx` — reasoning bias from the parent's MetaAgent run.
    pub fn new(
        factory: Arc<AgentFactory>,
        engine_ctx: EngineContext,
        depth: u32,
        mode: AgentMode,
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
            mode,
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
    /// 3. Spawns one child `ZhouyiCycle` per subtask (parallel, full Zhouyi).
    /// 4. Collects all `ZhouyiResult`s.
    /// 5. Converges via `YinAgent.converge()`.
    /// 6. Returns a `DecomposeResult`.
    pub async fn execute(&self, subtasks: Vec<SubtaskSpec>) -> Result<DecomposeResult, TaijiError> {
        // ── Mode guard (V27 配对模式兑底) ──
        // 工具仅编排模式注册；若因 bug 或未来路径在 Execution 模式被调用，直接拒绝。
        if self.mode == AgentMode::Execution {
            return Err(TaijiError::Other(
                "recursive_decompose is not available in Execution mode (V27 阴阳配对)".into(),
            ));
        }

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
                            // Try DecomposeResult first (new format), then ZhouyiResult (legacy).
                            if let Ok(Some(result)) =
                                load_json_optional::<DecomposeResult>(&result_path)
                            {
                                prior_results.insert(idx, result);
                            } else if let Ok(Some(zhouyi)) =
                                load_json_optional::<ZhouyiResult>(&result_path)
                            {
                                // Legacy format: map ZhouyiResult → DecomposeResult.
                                prior_results.insert(idx, map_zhouyi_to_decompose(&zhouyi));
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
            mode: AgentMode,
            /// V37 子任务级路由：SubtaskSpec.model（None = 继承父）。
            model: Option<crate::types::agent::ModelKey>,
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

                    // Delete old checkpoint to prevent ZhouyiCycle from mis-reading it.
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

            // V27 阴阳配对：子任务模式由父 LLM 在 SubtaskSpec.mode 中按难度
            // 分配（编排模板教学）；深度规则在下方子任务 MetaContext 注入处
            // 兑底（depth+1 >= max_depth 强制 Execution）。
            let enriched_description = assemble_child_description(
                &subtask.description,
                &subtask.verification_spec,
                &subtask.context,
            );

            // ── 子模式决策（V27）：SubtaskSpec.mode（父 LLM 难度判断）＋
            //    深度规则兑底（叶节点无法再拆解 → 强制 Execution）──
            let child_depth = self.depth + 1;
            let actual_mode = if child_depth >= self.factory.config.runtime.max_depth {
                AgentMode::Execution
            } else {
                subtask.mode
            };

            subtask_metas.push(SubtaskMeta {
                index: child_index,
                child_dir,
                child_deliverables: parent_deliverables.clone(),
                description: enriched_description,
                mode: actual_mode,
                // V37 子任务级路由：子模型覆盖（父 LLM 按难度/领域分配）。
                model: subtask.model,
                resume_history,
            });
        }

        // --- Spawn child Zhouyi cycles (V26: 子任务运行不持 permit — permit 已由
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
            let child_mode = meta.mode;
            let child_model = meta.model;

            // ── V30 会盟：收集兄弟贡品陈列室（分封时快照目录，排除自身）──
        // 无降级原则（BCP §8.20）：扫描失败 → Err 上抛，中止 decompose。
        let sibling_deliverables =
            collect_sibling_deliverables(&children_root, child_index)?;

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
                // ── V30 会盟：兄弟贡品陈列室目录（只读，贡品公开陈列语义 §8.20）──
                // 注入目录而非文件快照：同批并行兄弟的贡品在分封时点尚未产出，
                // 子任务执行中可经 read 工具随时发现陆续陈列的兄弟贡品。
                child_meta_ctx.yang_prompt.sibling_deliverables = sibling_deliverables;
                // ── Inject child 配对模式 (V27)：SubtaskSpec.mode（父 LLM 难度
                //    判断）或深度规则兑底后的 Execution。子 ZhouyiCycle 的阳 Agent
                //    据此选模板与工具注册面 ──
                child_meta_ctx.mode = child_mode;
                // ── V37 子任务级路由（BCP §8.8）：SubtaskSpec.model 覆盖子模型
                //    （父 LLM 按难度/领域分配）；None = 继承父模型（默认）。
                //    验证相位（verify_model）随子模型继承父的异源配置——子任务
                //    的裁判语义与父一致（异源方向不逐层重决策，MVP 边界）。
                apply_subtask_model(&mut child_meta_ctx, child_model.as_ref());

                // ── Create CancellationToken child linked to parent ──
                let child_cancel = cancel.child_token();

                // Create a ZhouyiCycle for the child subtask and execute
                // the full Zhouyi cycle with the parent's MetaContext.
                let zhouyi =
                    ZhouyiCycle::new(factory.clone(), factory.config.clone(), child_cancel);

                let result = zhouyi
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
        let mut result_map: BTreeMap<usize, ZhouyiResult> = BTreeMap::new();
        // V31 失败汇报：失败子任务索引 → failure_kind 分类（child_results 映射用）。
        let mut failure_kinds: BTreeMap<usize, String> = BTreeMap::new();
        let mut success_count = 0usize;
        let total = join_set.len();

        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((idx, Ok(zhouyi_result))) => {
                    // Broadcast child completion to frontend.
                    event_bus::emit_event(TaskEvent::ChildCompleted {
                        child_task_id: zhouyi_result.task_id.clone(),
                        status: NodeStatus::Converged,
                        deliverables: zhouyi_result.deliverables.clone(),
                        rounds: zhouyi_result.rounds,
                    });
                    result_map.insert(idx, zhouyi_result);
                    success_count += 1;
                }
                Ok((idx, Err(e))) => {
                    // V31 失败汇报（BCP §8.18）：取消 → 硬中止（收敛树整体放弃）；
                    // 任务级失败 → Diverged 失败条目进 prior_results，**不整体上抛**——
                    // 成功兄弟继续收集（不 abort_all），converge 收到完整汇报。
                    if self.cancel.is_cancelled() {
                        join_set.abort_all();
                        mark_aborted_children_failed(&children_root);
                        return Err(e);
                    }
                    let child_dir = children_root.join(idx.to_string());
                    failure_kinds.insert(idx, classify_failure(&e));
                    prior_results.insert(idx, build_failure_entry(&child_dir, &e));
                }
                Err(join_err) => {
                    // 进程级异常（panic）仍硬中止——不吞。
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
            prior_results.insert(idx, map_zhouyi_to_decompose(&result));
        }

        // ── Converge via YinAgent (with ALL results, old + new) ──────
        // V44：converge 在根级资产树执行（parent_meta_ctx.model 仅作路由）。
        let converge_agent =
            self.factory
                .create_yin_converge_agent(&self.engine_ctx, &self.parent_meta_ctx)?;
        let all_decompose_results: Vec<DecomposeResult> =
            prior_results.values().cloned().collect();
        let decision = converge_agent
            .converge(&all_decompose_results, &self.parent_meta_ctx)
            .await?;

        // Build child_results summary for parent LLM.
        // V31：Diverged 条目带 failure_reason/failure_kind（父阳再指导依据）。
        let child_results: Vec<ChildResultSummary> = prior_results
            .iter()
            .map(|(idx, r)| ChildResultSummary {
                task_id: r.task_id.clone(),
                summary: r.summary.clone(),
                status: r.status.clone(),
                rounds: r.rounds,
                tools_used: r.tools_used.clone(),
                deliverables: r.deliverables.clone(),
                failure_reason: failure_kinds
                    .get(idx)
                    .map(|_| r.summary.clone()),
                failure_kind: failure_kinds.get(idx).cloned(),
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

/// V30 会盟（BCP §8.20）：收集兄弟子任务的**贡品陈列室**目录（绝对路径）。
///
/// BTreeMap 有序扫描 `children/` 下各数字子任务目录，收集存在 `deliverables/`
/// 的子目录路径，排除 `exclude_idx`（当前正在 spawn 的子任务，防自引用）。
///
/// **注入目录而非文件快照**：同批并行兄弟在分封时点尚无产出（冒烟实证：
/// 文件级快照恒空、会盟失效），目录路径 = 动态发现入口——子任务执行中可
/// 经 read 工具随时发现兄弟陆续陈列的贡品；跨轮/rerun 场景同样有效。
///
/// 无降级原则：目录读取失败 → `Err` 上抛（数据完整性问题必须暴露），
/// 禁止 `unwrap_or_default()` 吞错。`children/` 不存在（无兄弟）是状态分支，
/// 返回空列表，非降级。非数字目录条目（如临时目录）跳过。
fn collect_sibling_deliverables(
    children_root: &Path,
    exclude_idx: usize,
) -> Result<Vec<String>, TaijiError> {
    let mut venues: BTreeMap<usize, PathBuf> = BTreeMap::new();
    if !children_root.exists() {
        return Ok(vec![]);
    }
    for entry in std::fs::read_dir(children_root).map_err(TaijiError::IO)? {
        let entry = entry.map_err(TaijiError::IO)?;
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(idx) = name.parse::<usize>() else {
            continue; // 非数字目录（临时/杂项）不是兄弟任务
        };
        if idx == exclude_idx {
            continue;
        }
        let del_dir = entry.path().join("deliverables");
        if del_dir.exists() {
            venues.insert(idx, del_dir);
        }
    }
    Ok(venues.into_values().map(|p| p.to_string_lossy().to_string()).collect())
}

/// V31 失败汇报（BCP §8.18）：TaijiError 变体 → failure_kind 分类
///（§8.18 词汇表扩展）。纯函数，可单测。
fn classify_failure(e: &TaijiError) -> String {
    match e {
        TaijiError::ContextOverflow { .. } => "context_overflow",
        TaijiError::HardCutoff { .. } => "hard_cutoff",
        TaijiError::LLMCallFailed { .. } => "llm_failed",
        TaijiError::StructuredOutputParseFailed { .. } => "cognitive",
        TaijiError::MaxDepthExceeded { .. } => "cognitive",
        TaijiError::MaxRoundsExceeded { .. } => "cognitive",
        TaijiError::MaxCyclesExceeded { .. } => "cognitive",
        TaijiError::MaxSubtasksExceeded { .. } => "cognitive",
        TaijiError::ConstraintViolation { .. } => "constraint_violation",
        TaijiError::SafetyViolation { .. } => "constraint_violation",
        TaijiError::IO(_) | TaijiError::Serde(_) => "io",
        TaijiError::Config { .. } => "config",
        TaijiError::KnowledgeStoreUnavailable { .. } => "io",
        TaijiError::WorkerPoolUnavailable { .. } => "io",
        TaijiError::Cancelled { .. } => "cancelled",
        TaijiError::Other(_) => "other",
    }
    .to_string()
}

/// V31 失败汇报（BCP §8.18）：任务级失败子任务 → Diverged 条目。
///
/// summary = `[{kind}] {reason}`（converge LLM 可读）；deliverables = 子任务
/// deliverables/ 现存文件（含 handoff.md 交接产物——V28 失败一律先写）。
/// 交接产物收集失败仅 warn（**有意例外**：原始失败原因必须优先传播，
/// 叠加 IO 错误会掩盖根因，BCP §8.18 声明）。
fn build_failure_entry(child_dir: &Path, e: &TaijiError) -> DecomposeResult {
    let del_dir = child_dir.join("deliverables");
    let mut deliverables = Vec::new();
    match std::fs::read_dir(&del_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                deliverables.push(entry.path().to_string_lossy().to_string());
            }
        }
        Err(err) => {
            tracing::warn!(
                child_dir = %child_dir.display(),
                error = %err,
                "failure entry: 无法读取子任务 deliverables/（交接产物收集失败，仅告警）"
            );
        }
    }
    let kind = classify_failure(e);
    let reason = e.to_string();
    let task_id = crate::infra::trace::load_json_optional::<Task>(&child_dir.join("meta.json"))
        .ok()
        .flatten()
        .map(|t| t.id)
        .unwrap_or_else(|| {
            child_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "child".to_string())
        });
    DecomposeResult {
        task_id,
        summary: format!("[{kind}] {reason}"),
        status: ConvergenceStatus::Diverged,
        subtask_count: 0,
        deliverables,
        rounds: 0,
        tools_used: vec![],
        child_results: vec![],
    }
}

/// Best-effort mark of every aborted child whose `meta.json` still says
/// `Running` as `Failed` (V26.3, E1).
///
/// `tokio::task::JoinSet::abort_all()` kills the child futures without running
/// their cancellation/status-writing paths, so without this step children
/// would stay `Running` forever even though they were terminated mid-flight.
/// Iterates the numeric `children/<idx>/` directories only; skips non-directory
/// entries and unreadable `meta.json` (warn only). Write failures warn and are
/// swallowed — this helper must never block error propagation in the caller.
pub(crate) fn mark_aborted_children_failed(children_root: &Path) {
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

/// Map a ZhouyiResult into a DecomposeResult for convergence analysis.
/// V37 子任务级路由（BCP §8.8）：应用 `SubtaskSpec.model` 覆盖到子 MetaContext。
/// None = 继承父模型（不变）；Some = 覆盖（父 LLM 按难度/领域分配）。
/// 纯函数——可单测，spawn 闭包内调用。
fn apply_subtask_model(child: &mut MetaContext, model: Option<&crate::types::agent::ModelKey>) {
    if let Some(m) = model {
        child.model = Some(m.clone());
    }
}

fn map_zhouyi_to_decompose(result: &ZhouyiResult) -> DecomposeResult {
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
/// This keeps the ZhouyiCycle signature unchanged while giving child agents
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
            description: "Recursively decompose a task into subtasks. Each subtask runs a full Zhouyi cycle (MetaAgent → YangAgent → YinAgent). Returns a JSON-serialized DecomposeResult.".to_string(),
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
                                "mode": {
                                    "type": "string",
                                    "enum": ["Orchestration", "Execution"],
                                    "description": "Whether the subtask runs in Orchestration (further decomposition) or Execution (direct work) mode. Assign by subtask difficulty: atomic/single-step → Execution; complex/multi-dimension → Orchestration. Leaf-depth subtasks are forced to Execution automatically."
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
        serde_json::to_string(&result).map_err(TaijiError::Serde)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::agent::ModelKey;

    // V30 测试临时目录唯一性（AGENTS.md §16）：pid 基路径不唯一，需静态计数器。
    static SIBLING_TEST_COUNTER: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn test_apply_subtask_model_override_and_inherit() {
        // V37 子任务级路由：Some 覆盖子模型；None 保持父模型（继承）。
        let parent_key = ModelKey::from_parts("deepseek", "deepseek-chat");
        let mut child = MetaContext { model: Some(parent_key.clone()), ..MetaContext::empty() };

        // None → 不变（继承父）。
        apply_subtask_model(&mut child, None);
        assert_eq!(child.model.as_ref().map(|k| k.key()), Some("deepseek-deepseek-chat"));

        // Some → 覆盖。
        let override_key = ModelKey::from_parts("deepseek", "deepseek-reasoner");
        apply_subtask_model(&mut child, Some(&override_key));
        assert_eq!(
            child.model.as_ref().map(|k| k.key()),
            Some("deepseek-deepseek-reasoner")
        );
        // verify_model 不被触碰（随父继承语义）。
        assert!(child.verify_model.is_none());
    }

    #[test]
    fn test_collect_sibling_deliverables_basic() {
        // V30 会盟：收集兄弟贡品陈列室目录，排除自身，BTreeMap 有序。
        let tmp = std::env::temp_dir().join(format!(
            "decompose_sibling_{}_{}",
            std::process::id(),
            SIBLING_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("0/deliverables")).unwrap();
        std::fs::create_dir_all(tmp.join("1/deliverables")).unwrap();
        std::fs::create_dir_all(tmp.join("2/deliverables")).unwrap();
        std::fs::write(tmp.join("1/deliverables/b.md"), "b").unwrap();
        std::fs::write(tmp.join("2/deliverables/c.md"), "c").unwrap();

        // 排除自身（idx=1）：应看到 0 与 2 的陈列室
        let siblings = collect_sibling_deliverables(&tmp, 1).unwrap();
        assert_eq!(siblings.len(), 2, "应含兄弟 0 与 2 的陈列室: {siblings:?}");
        assert!(
            siblings[0].ends_with("0/deliverables"),
            "路径错误: {}",
            siblings[0]
        );
        assert!(
            siblings[1].ends_with("2/deliverables"),
            "路径错误: {}",
            siblings[1]
        );

        // 不排除自身：按目录索引有序出现
        let all = collect_sibling_deliverables(&tmp, 99).unwrap();
        assert_eq!(all.len(), 3, "应含 0/1/2 的陈列室: {all:?}");
        let joined = all.join("\n");
        let pos_0 = joined.find("0/deliverables").unwrap();
        let pos_1 = joined.find("1/deliverables").unwrap();
        let pos_2 = joined.find("2/deliverables").unwrap();
        assert!(pos_0 < pos_1 && pos_1 < pos_2, "目录应按索引有序: {joined}");

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_collect_sibling_deliverables_empty_and_missing_root() {
        // 无兄弟 / children/ 不存在 → 空列表（状态分支，非降级）。
        let tmp = std::env::temp_dir().join(format!(
            "decompose_sibling_empty_{}_{}",
            std::process::id(),
            SIBLING_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(
            collect_sibling_deliverables(&tmp, 0).unwrap(),
            Vec::<String>::new()
        );

        std::fs::create_dir_all(tmp.join("0")).unwrap(); // 无 deliverables 目录
        assert_eq!(
            collect_sibling_deliverables(&tmp, 0).unwrap(),
            Vec::<String>::new()
        );

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_collect_sibling_deliverables_skips_non_numeric_entries() {
        // 非数字目录（临时/杂项）不是兄弟任务，跳过。
        let tmp = std::env::temp_dir().join(format!(
            "decompose_sibling_skip_{}_{}",
            std::process::id(),
            SIBLING_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("0/deliverables")).unwrap();
        std::fs::create_dir_all(tmp.join("_tmp/deliverables")).unwrap();
        std::fs::write(tmp.join("0/deliverables/x.md"), "x").unwrap();
        std::fs::write(tmp.join("_tmp/deliverables/y.md"), "y").unwrap();

        let siblings = collect_sibling_deliverables(&tmp, 99).unwrap();
        assert_eq!(siblings.len(), 1, "非数字目录应被跳过: {siblings:?}");
        assert!(siblings[0].contains("0/deliverables"));

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_classify_failure_mapping() {
        // V31：TaijiError 变体 → failure_kind 分类（§8.18 词汇表）。
        assert_eq!(classify_failure(&TaijiError::ContextOverflow { threshold: 1 }), "context_overflow");
        assert_eq!(classify_failure(&TaijiError::HardCutoff { threshold: 1 }), "hard_cutoff");
        assert_eq!(classify_failure(&TaijiError::LLMCallFailed { context: "x".into() }), "llm_failed");
        assert_eq!(classify_failure(&TaijiError::StructuredOutputParseFailed { context: "x".into() }), "cognitive");
        assert_eq!(classify_failure(&TaijiError::MaxRoundsExceeded { max: 3 }), "cognitive");
        assert_eq!(classify_failure(&TaijiError::ConstraintViolation { context: "x".into() }), "constraint_violation");
        assert_eq!(classify_failure(&TaijiError::IO(std::io::Error::new(std::io::ErrorKind::Other, "e"))), "io");
        assert_eq!(classify_failure(&TaijiError::Cancelled { context: "x".into() }), "cancelled");
        assert_eq!(classify_failure(&TaijiError::Other("x".into())), "other");
    }

    #[test]
    fn test_build_failure_entry_with_handoff() {
        // V31：失败条目 = Diverged + failure kind 前缀 + handoff 交接产物路径。
        let tmp = std::env::temp_dir().join(format!(
            "decompose_failure_entry_{}_{}",
            std::process::id(),
            SIBLING_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("deliverables")).unwrap();
        std::fs::write(tmp.join("deliverables/handoff.md"), "# 交接").unwrap();
        std::fs::write(
            tmp.join("meta.json"),
            serde_json::to_string(&Task {
                id: "child-9".into(),
                description: "子任务".into(),
                depth: 1,
                status: crate::types::task::TaskStatus::Failed,
                parent_id: Some("root".into()),
                subtask_ids: vec![],
            })
            .unwrap(),
        )
        .unwrap();

        let err = TaijiError::ContextOverflow { threshold: 250_000 };
        let entry = build_failure_entry(&tmp, &err);
        assert_eq!(entry.status, ConvergenceStatus::Diverged);
        assert!(entry.summary.contains("[context_overflow]"), "summary: {}", entry.summary);
        assert_eq!(entry.task_id, "child-9", "task_id 应从身份册读取");
        assert_eq!(entry.deliverables.len(), 1, "应含 handoff 交接产物");
        assert!(entry.deliverables[0].ends_with("handoff.md"));

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_build_failure_entry_missing_deliverables_warns_and_continues() {
        // 无 deliverables 目录（异常路径）→ 条目仍构造（deliverables 空，仅 warn）。
        let tmp = std::env::temp_dir().join(format!(
            "decompose_failure_entry_empty_{}_{}",
            std::process::id(),
            SIBLING_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let entry = build_failure_entry(&tmp, &TaijiError::Other("boom".into()));
        assert_eq!(entry.status, ConvergenceStatus::Diverged);
        assert!(entry.summary.contains("[other] boom"));
        assert!(entry.deliverables.is_empty());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_child_result_summary_failure_fields_serde_compat() {
        // V31：旧 decompose_result.json（无 failure 字段）反序列化 → None。
        let legacy = serde_json::json!({
            "task_id": "t1",
            "summary": "s",
            "status": "Converged",
            "rounds": 2,
            "tools_used": ["read"],
            "deliverables": []
        });
        let parsed: ChildResultSummary = serde_json::from_value(legacy).expect("legacy parse");
        assert_eq!(parsed.failure_reason, None);
        assert_eq!(parsed.failure_kind, None);
    }

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
