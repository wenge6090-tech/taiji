use serde::{Deserialize, Serialize};

use crate::types::verification::ConvergenceStatus;

/// A recursive work unit in the TPN-DMN engine.
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
/// V26 起无 `mode` 字段（异层同构：子任务与根任务完全同构，无模式分化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskSpec {
    pub description: String,
    pub verification_spec: String,
    #[serde(default)]
    pub context: serde_json::Value,
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
/// parent FittingAgent can make informed re-run decisions.
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
    /// Populated by `recursive_decompose` tool from child TPNResults.
    pub deliverables: Vec<String>,
    /// How many TPN rounds the child agent needed before passing verify().
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

/// Final result of a TPN execution cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TPNResult {
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
// Checkpoint — TpnCycle phase tracking (durable, atomic-write to checkpoint.json)
// ---------------------------------------------------------------------------

/// Which phase of the TPN cycle has been durably persisted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CyclePhase {
    /// `MetaAgent.run()` completed; MetaContext is in `meta.json`.
    MetaDone,
    /// `FittingAgent.run()` completed; conversation is in `chat_history.json`.
    FittingDone,
    /// `CausalAgent.verify()` completed; state is in `verify_state.json`.
    VerifyDone,
}

/// Snapshot of TpnCycle progress, atomically written to `task_dir/checkpoint.json`.
///
/// On TpnCycle startup, if this file exists and the task is not fully complete,
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
