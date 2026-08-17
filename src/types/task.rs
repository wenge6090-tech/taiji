use serde::{Deserialize, Serialize};

use crate::types::verification::ConvergenceStatus;

/// A recursive work unit in the Zhouyi-Lianshan engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub depth: u32,
    pub status: TaskStatus,
    pub parent_id: Option<String>,
    pub subtask_ids: Vec<String>,
}

/// Subtask specification emitted by LLM during recursive decomposition.
///
/// V27 阴阳配对：子任务模式由父 LLM 按难度分配（`mode` 字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskSpec {
    pub description: String,
    pub verification_spec: String,
    #[serde(default)]
    pub context: serde_json::Value,
    /// Whether this subtask executes in Orchestration (decompose further) or
    /// Execution (focus on direct output) mode (V27 阴阳配对模式).
    ///
    /// The parent LLM sets this value by difficulty judgment (guided by the
    /// orchestration prompt's depth rules). `RecursiveDecomposeTool` may
    /// override it to `Execution` when `depth + 1 >= max_depth` (leaf
    /// enforcement). Defaults to Orchestration for robustness.
    #[serde(default)]
    pub mode: crate::types::agent::AgentMode,
    /// V37：子任务模型覆盖（Blueprint §4.3 子任务级路由）。父 LLM 拆解时可按
    /// 子任务难度/领域分配不同模型；None = 继承父模型（子 ZhouyiCycle 注入父
    /// MetaContext，`model` 默认继承）。serde default：旧 decompose_result
    /// 零迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::types::agent::ModelKey>,
    /// Explicit re-run marker: `Some(old_child_index)` means "re-run the
    /// subtask previously at `children/<old_child_index>/`". The `description`
    /// field carries the re-run reason and new requirements.
    #[serde(default)]
    pub rerun_of: Option<usize>,
}

/// Per-child execution summary for converge LLM and parent Yang.
///
/// Carried inside [`DecomposeResult.child_results`] so the converge LLM has
/// full visibility into each child's quality signals (rounds, tools) and the
/// parent YangAgent can make informed re-run decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildResultSummary {
    pub task_id: String,
    pub summary: String,
    pub status: ConvergenceStatus,
    #[serde(default)]
    pub rounds: u32,
    #[serde(default)]
    pub tools_used: Vec<String>,
    #[serde(default)]
    pub deliverables: Vec<String>,
    /// V31 失败汇报（Blueprint §1.5）：任务级失败的子任务原因文本（供父阳再指导）。
    /// 成功子任务为 None。失败子任务的 deliverables 含 handoff.md 交接产物路径
    /// （残缺产出，父阳可读后精准 rerun_of + 修正指导）。
    #[serde(default)]
    pub failure_reason: Option<String>,
    /// V31 失败分类（§1.5 词汇表扩展）：context_overflow / hard_cutoff /
    /// llm_failed / io / cognitive / constraint_violation / other。
    #[serde(default)]
    pub failure_kind: Option<String>,
}

/// Result returned by the recursive_decompose tool.
///
/// Carries per-child execution metadata so the converge (阴) LLM can assess
/// not just output text but also execution quality signals: how many rounds
/// each child needed, and what tools were used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeResult {
    /// Child task ID for traceability.
    pub task_id: String,
    pub summary: String,
    pub status: ConvergenceStatus,
    pub subtask_count: u32,
    /// Absolute paths of all child deliverables aggregated upward.
    /// Populated by `recursive_decompose` tool from child ZhouyiResults.
    pub deliverables: Vec<String>,
    /// How many Zhouyi rounds the child agent needed before passing verify().
    /// Low rounds = easy task, high rounds = struggled task — key signal for converge.
    #[serde(default)]
    pub rounds: u32,
    /// Tools the child agent used during execution (e.g. "read", "write", "bash").
    #[serde(default)]
    pub tools_used: Vec<String>,
    /// Per-child detailed results for converge LLM and parent Yang.
    /// The parent LLM reads these to decide which children need re-running.
    #[serde(default)]
    pub child_results: Vec<ChildResultSummary>,
}

/// Final result of a Zhouyi execution cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZhouyiResult {
    pub task_id: String,
    pub content: String,
    pub tools_used: Vec<String>,
    pub deliverables: Vec<String>,
    pub depth: u32,
    pub rounds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Checkpoint — ZhouyiCycle phase tracking (durable, atomic-write to checkpoint.json)
// ---------------------------------------------------------------------------

/// Which phase of the Zhouyi cycle has been durably persisted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CyclePhase {
    /// `MetaAgent.run()` completed; MetaContext is in `meta.json`.
    MetaDone,
    /// `YangAgent.run()` completed; conversation is in `chat_history.json`.
    YangDone,
    /// `YinAgent.verify()` completed; state is in `verify_state.json`.
    YinDone,
}

/// Snapshot of ZhouyiCycle progress, atomically written to `task_dir/checkpoint.json`.
///
/// On ZhouyiCycle startup, if this file exists and the task is not fully complete,
/// the cycle can resume from the last completed phase rather than restarting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The last completed phase.
    pub phase: CyclePhase,
    /// Round counter value at the time of checkpoint.
    pub round: u32,
    /// Cycle counter value at the time of checkpoint.
    pub cycle: u32,
}
