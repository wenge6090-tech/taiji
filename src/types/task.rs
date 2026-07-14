use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskSpec {
    pub description: String,
    pub verification_spec: String,
    #[serde(default)]
    pub context: serde_json::Value,
}

/// Result returned by the recursive_decompose tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposeResult {
    pub summary: String,
    pub status: crate::types::verification::ConvergenceStatus,
    pub subtask_count: u32,
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
    Decomposed,
}
