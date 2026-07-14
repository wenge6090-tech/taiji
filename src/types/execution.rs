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
