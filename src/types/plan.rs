//! Plan — execution plan types for the **taiji_plan** MCP tool.
//!
//! [`PlanSummary`] is produced by [`PlanBuilder`](crate::agents::plan::PlanBuilder)
//! which runs MetaAgent (权重更新·元) to obtain cognitive context, then asks
//! an LLM to compose a structured execution plan **without** entering the Zhouyi
//! loop (no YangAgent / YinAgent).

use serde::{Deserialize, Serialize};

/// Pre-execution plan summary: MetaAgent + LLM-composed plan.
///
/// Returned by `taiji_plan` MCP tool.  Contains the task analysis, estimated
/// subtasks, recommended skills, expected deliverables and complexity estimate.
/// This is a **speculative** output — actual execution may differ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSummary {
    /// Brief (1-2 sentence) analysis of what the task entails.
    pub task_analysis: String,
    /// Estimated subtasks the task may decompose into.
    #[serde(default)]
    pub estimated_subtasks: Vec<SubtaskPlan>,
    /// Names of skills likely needed (e.g. "read", "write", "bash").
    #[serde(default)]
    pub recommended_skills: Vec<String>,
    /// Human-readable descriptions of expected deliverables.
    #[serde(default)]
    pub expected_deliverables: Vec<String>,
    /// Complexity assessment: "simple" | "moderate" | "complex".
    pub estimated_complexity: String,
    /// Summary of prompts matched from 归藏.
    #[serde(default)]
    pub matched_prompts_summary: String,
    /// Relevant truth constraints applicable to this task.
    #[serde(default)]
    pub relevant_constraints: Vec<String>,
}

/// A single estimated subtask within a [`PlanSummary`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskPlan {
    /// What this subtask should accomplish.
    pub description: String,
    /// How to verify the subtask's output (verification approach).
    pub verification_approach: String,
    /// Skill names likely needed for this subtask.
    #[serde(default)]
    pub required_skills: Vec<String>,
}
