//! Frontend-facing types (L0) — consumed by the taiji-web frontend.
//!
//! These types are serialized to JSON and pushed over the WebSocket bridge
//! (`src/ws/`) to the React UI. They are the cross-language contract between
//! the Rust engine and the TypeScript frontend — keep them in sync with
//! `taiji-web/src/types/index.ts`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Node status & Zhouyi phase
// ---------------------------------------------------------------------------

/// Visual status of a task node on the spindle tree.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum NodeStatus {
    /// Created but not yet started (yellow).
    Pending,
    /// Actively executing a Zhouyi phase (yellow + pulse).
    Running,
    /// Verified & converged — PASS (green).
    Converged,
    /// Diverged — routed BACK_TO_ZHOUYI / BACK_TO_META (red).
    Diverged,
    /// Task failed after exhausting rounds/cycles (red).
    Failed,
    /// Task cancelled (gray).
    Cancelled,
    /// Blocked waiting for human review in the Zhouyi popup (orange).
    AwaitingHumanReview,
}

/// Current Zhouyi phase of a task node.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ZhouyiPhase {
    Idle,
    Meta,
    Yang,
    Yin,
    Converged,
}

// ---------------------------------------------------------------------------
// Spindle tree snapshot
// ---------------------------------------------------------------------------

/// One node in the spindle-shaped recursive task tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpindleNode {
    pub task_id: String,
    pub description: String,
    pub depth: u32,
    pub sibling_index: u32,
    pub total_siblings: u32,
    pub status: NodeStatus,
    pub phase: ZhouyiPhase,
    pub round: u32,
    pub cycle: u32,
    pub parent_id: Option<String>,
    pub children_count: u32,
    pub deliverables_count: u32,
    pub tools_used: Vec<String>,
}

/// Edge connecting a parent node to a child node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpindleEdge {
    pub source: String,
    pub target: String,
    pub status: NodeStatus,
}

/// Single Lianshan evolution entry (δ₁ skill tuning, δ₂ bayesian, δ₃ grid rewire).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvolutionSummary {
    pub layer: u32,
    pub asset_id: String,
    pub delta: String,
    pub timestamp: String,
}

/// Lianshan background activity summary for the right-hand panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LianshanActivity {
    pub active_nodes: u32,
    pub recent_evolutions: Vec<EvolutionSummary>,
}

/// Full snapshot of one root task's recursive tree, as rendered by the
/// frontend spindle layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskTreeSnapshot {
    pub root_task_id: String,
    pub root_description: String,
    pub nodes: Vec<SpindleNode>,
    pub edges: Vec<SpindleEdge>,
    pub lianshan_activity: Option<LianshanActivity>,
}

// ---------------------------------------------------------------------------
// Zhouyi popup state
// ---------------------------------------------------------------------------

/// One trace record previewed in the Zhouyi popup detail panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceRecordPreview {
    pub ts: String,
    pub phase: String,
    pub cycle: u32,
    pub round: u32,
    pub tool: Option<String>,
    pub summary: String,
}

/// Yin verification verdict shown in the popup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YinVerdict {
    pub route: String,
    pub confidence: f64,
    pub summary: String,
    pub violations: Vec<String>,
}

/// State of one node's Zhouyi tri-phase flow, pushed to the popup on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZhouyiPhaseState {
    pub task_id: String,
    pub current_phase: ZhouyiPhase,
    pub meta_summary: Option<String>,
    pub yang_summary: Option<String>,
    pub yin_verdict: Option<YinVerdict>,
    pub deliverables: Vec<String>,
    pub trace_preview: Vec<TraceRecordPreview>,
}

// ---------------------------------------------------------------------------
// Human intervention (yin approval)
// ---------------------------------------------------------------------------

/// Action the human takes in the Zhouyi popup's yin intervention area.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InterventionAction {
    /// Approve convergence — route PASS.
    Approve,
    /// Reject and retry the Zhouyi loop — route BACK_TO_ZHOUYI.
    RejectRetry,
    /// Reject and reroute back to MetaAgent — route BACK_TO_META.
    RejectReroute,
}

/// Human suggestion + action submitted from the frontend yin intervention box.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YinIntervention {
    pub task_id: String,
    pub action: InterventionAction,
    pub suggestion: String,
}
