use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Execution context common to all agents.
///
/// Each layer (root or recursive child) has its own `task_dir` so that
/// every node in the recursion tree writes deliverables, traces, and
/// metadata into its own directory with the same layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineContext {
    pub task_id: String,
    pub depth: u32,
    /// Filesystem root for this layer's task directory.
    /// `{task_dir}/deliverables/` is the LLM's output sandbox.
    /// Child subtask dirs live under `{task_dir}/children/{i}/`.
    pub task_dir: PathBuf,
    pub cycle: u32,
    pub round: u32,
    /// Optional directory containing context materialized from the calling
    /// frontend agent (e.g. any MCP-compatible frontend agent).
    ///
    /// Layout:
    ///   `{context_dir}/files/` — one file per ExternalFile
    ///   `{context_dir}/meta.json` — ExternalContext (session_summary, tool_results)
    ///
    /// When `Some`, FittingAgent's system prompt references this directory so
    /// the LLM can use the `read` tool to inspect pre-collected context.
    /// Children inherit this reference as read-only; they do not overwrite it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_dir: Option<PathBuf>,
}

/// Record of a single tool call during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub result: serde_json::Value,
    pub duration_ms: u64,
    pub safety_check_passed: bool,
}

// ---------------------------------------------------------------------------
// ExplainReport — post-execution reasoning tree summary
// ---------------------------------------------------------------------------

/// Post-execution explanation report produced by the **taiji_explain** MCP tool.
///
/// Reads `meta.json` + recursive `trace.jsonl` + `deliverables/` to reconstruct
/// a human-readable reasoning tree summary with phase timeline, route decisions,
/// and final deliverables.
///
/// This is a **data-processing** report — no LLM calls are involved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainReport {
    pub task_id: String,
    pub description: String,
    /// Task status: "completed" | "failed" | "cancelled" | "running"
    pub status: String,
    pub total_cycles: u32,
    pub total_rounds: u32,
    pub total_depth: u32,
    pub total_duration_ms: u64,
    /// Chronologically sorted phase records.
    #[serde(default)]
    pub timeline: Vec<PhaseSummary>,
    /// TPN route decisions extracted from verification phases.
    #[serde(default)]
    pub decisions: Vec<DecisionSummary>,
    /// Final deliverable absolute paths.
    #[serde(default)]
    pub final_deliverables: Vec<String>,
    /// Human-readable Chinese summary (synthesised from data, not LLM).
    pub summary: String,
}

/// A single phase in the execution timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSummary {
    /// Phase name: "权重更新" | "概率拟合" | "因果验证" | "收敛判定"
    pub phase: String,
    pub cycle: u32,
    pub round: u32,
    pub depth: u32,
    pub duration_ms: u64,
    /// Tool names used during this phase (for "概率拟合" phases).
    #[serde(default)]
    pub tools_used: Vec<String>,
    /// Truncated key output or description of this phase.
    pub key_output: String,
}

/// A TPN route decision recorded during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionSummary {
    pub cycle: u32,
    pub round: u32,
    /// Verdict: "PASS" | "BACK_TO_TPN" | "BACK_TO_META"
    pub verdict: String,
    /// Human-readable reason for the decision.
    #[serde(default)]
    pub reason: String,
    /// Any constraint violations that contributed to this decision.
    #[serde(default)]
    pub constraint_violations: Vec<String>,
}
