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
    /// frontend agent (e.g. pi_agent_rust via MCP).
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
