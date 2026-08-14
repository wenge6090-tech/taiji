//! 迹拓扑类型（BCP §6.0 蓝图文件契约）——连山拓扑压缩的产物类型。
//!
//! 蓝图文件 = 执行迹的离散拓扑（状态转移图），非连续流型（§6.0 三层定论：
//! 高维流型到周易递归文件夹时已离散为马尔可夫链，拓扑离散对象确定性可做）。
//! 纯数据类型，仅依赖 `CheckStats`，零业务依赖。

use crate::types::verification::CheckStats;
use serde::{Deserialize, Serialize};

/// 迹拓扑（`knowledge/manifold/{root_task}.yaml`）——节点 + 边状态转移图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifoldTopology {
    /// 根任务 id。
    pub root_task: String,
    /// 压缩时间戳（unix ms）。
    pub generated_at: u64,
    /// 拓扑节点。
    pub nodes: Vec<TopologyNode>,
    /// 拓扑边。
    pub edges: Vec<TopologyEdge>,
}

/// 拓扑节点：task / asset / deliverable / handoff。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyNode {
    /// task_id | asset_id | deliverable 相对 root_task_dir 的路径。
    pub id: String,
    /// 节点类别。
    pub kind: TopologyNodeKind,
    /// 仅 Task 节点有效（递归深度）。
    #[serde(default)]
    pub depth: u32,
    /// 复用四维统计（serde default 零迁移）。
    #[serde(default)]
    pub stats: CheckStats,
}

/// 拓扑节点类别。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TopologyNodeKind {
    /// 任务节点（meta.json）。
    Task,
    /// 资产节点（assets_used）。
    Asset,
    /// 产出物节点（deliverables/ 下文件）。
    Deliverable,
    /// 交接节点（deliverables/handoff.md）。
    Handoff,
}

/// 拓扑边：状态转移。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyEdge {
    /// 源节点 id。
    pub from: String,
    /// 目标节点 id。
    pub to: String,
    /// 边类别。
    pub kind: TopologyEdgeKind,
}

/// 拓扑边类别。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TopologyEdgeKind {
    /// parent → child（meta.json.subtask_ids，精确）。
    Decompose,
    /// task → asset（assets_used）。
    Invoke,
    /// task → deliverable（产出物）。
    Dataflow,
    /// task → handoff.md（交接）。
    Handoff,
    /// task → check（checks[].check_id）。
    Verify,
}
