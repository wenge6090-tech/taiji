//! Task tree builder (L4) — scans `data/tasks/{root}/children/` recursively
//! and produces a [`TaskTreeSnapshot`] for the frontend spindle tree.
//!
//! The snapshot is derived entirely from the on-disk task layout (meta.json,
//! checkpoint.json, trace.jsonl, deliverables/, children/), so the frontend
//! can render the full recursion tree without engine cooperation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::infra::error::TaijiError;
use crate::types::frontend::{
    DmnActivity, EvolutionSummary, NodeStatus, SpindleEdge, SpindleNode, TaskTreeSnapshot, TpnPhase,
};
use crate::types::task::{Checkpoint, CyclePhase, Task, TaskStatus};

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Build a full tree snapshot for `root_task_id` under `data_root`.
///
/// # Errors
///
/// Returns `TaijiError::Other` if the root task directory does not exist or
/// its `meta.json` cannot be parsed.
pub fn build_task_tree(data_root: &Path, root_task_id: &str) -> Result<TaskTreeSnapshot, TaijiError> {
    let root_dir = data_root.join("tasks").join(root_task_id);
    if !root_dir.is_dir() {
        return Err(TaijiError::Other(format!(
            "task directory not found: {}",
            root_dir.display()
        )));
    }

    let root_task = read_task(&root_dir)?;
    let root_description = root_task.description.clone();

    let mut nodes: Vec<SpindleNode> = Vec::new();
    let mut edges: Vec<SpindleEdge> = Vec::new();

    // BFS over the on-disk tree (dedup via visited set — AGENTS.md §5).
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut queue: Vec<(PathBuf, Option<String>)> = vec![(root_dir.clone(), None)];

    while let Some((dir, parent_id)) = queue.pop() {
        if !visited.insert(dir.clone()) {
            continue;
        }

        let task = match read_task(&dir) {
            Ok(t) => t,
            Err(_) => continue, // skip malformed directories
        };
        let task_id = task.id.clone();
        let task_dir = dir.clone();

        // Children: `children/<idx>/` directories, ordered by numeric index.
        let children_dir = dir.join("children");
        let mut child_dirs: BTreeMap<u32, PathBuf> = BTreeMap::new();
        if children_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&children_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let idx = path
                    .file_name()
                    .and_then(|n| n.to_string_lossy().parse::<u32>().ok());
                if let Some(idx) = idx {
                    child_dirs.insert(idx, path);
                }
            }
        }

        // Per-node derived data.
        let checkpoint = load_checkpoint(&task_dir);
        let (phase, round, cycle) = derive_tpn_state(&task, &checkpoint);
        let status = derive_node_status(&task, &checkpoint);
        let deliverables_count = count_files(&task_dir.join("deliverables"));
        let tools_used = collect_tools_used(&task_dir);
        let total_siblings = if parent_id.is_some() {
            // Count siblings by re-scanning the parent's children dir.
            let parent_dir = &task_dir.parent().unwrap_or(&task_dir).join("children");
            count_dirs(parent_dir).max(1)
        } else {
            1
        };
        let sibling_index = if let Some(parent) = &parent_id {
            find_sibling_index(&task_dir, parent, data_root).unwrap_or(0)
        } else {
            0
        };

        nodes.push(SpindleNode {
            task_id: task_id.clone(),
            description: task.description.clone(),
            depth: task.depth,
            sibling_index,
            total_siblings,
            status,
            phase,
            round,
            cycle,
            parent_id: parent_id.clone(),
            children_count: child_dirs.len() as u32,
            deliverables_count,
            tools_used,
        });

        if let Some(parent) = &parent_id {
            edges.push(SpindleEdge {
                source: parent.clone(),
                target: task_id.clone(),
                status,
            });
        }

        // Enqueue children (BFS order; index keying gives stable order).
        for (_, child_dir) in child_dirs {
            queue.push((child_dir, Some(task_id.clone())));
        }
    }

    Ok(TaskTreeSnapshot {
        root_task_id: root_task_id.to_string(),
        root_description,
        nodes,
        edges,
        dmn_activity: None, // populated by the WS handler layer
    })
}

// ---------------------------------------------------------------------------
// Per-node derivation helpers
// ---------------------------------------------------------------------------

/// Read and parse `meta.json` in a task directory.
fn read_task(task_dir: &Path) -> Result<Task, TaijiError> {
    let content = std::fs::read_to_string(task_dir.join("meta.json"))
        .map_err(|e| TaijiError::IO(e))?;
    serde_json::from_str(&content).map_err(TaijiError::Serde)
}

/// Load `checkpoint.json` if present (crash recovery state).
fn load_checkpoint(task_dir: &Path) -> Option<Checkpoint> {
    let path = task_dir.join("checkpoint.json");
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Checkpoint>(&s).ok())
}

/// Map on-disk task status + checkpoint to a frontend [`NodeStatus`].
fn derive_node_status(task: &Task, checkpoint: &Option<Checkpoint>) -> NodeStatus {
    match task.status {
        TaskStatus::Completed => NodeStatus::Converged,
        TaskStatus::Failed => NodeStatus::Failed,
        TaskStatus::Cancelled => NodeStatus::Cancelled,
        TaskStatus::Pending => NodeStatus::Pending,
        TaskStatus::Running => {
            // Running + checkpoint present → actively cycling.
            if checkpoint.is_some() {
                NodeStatus::Running
            } else {
                NodeStatus::Pending
            }
        }
    }
}

/// Derive the current TPN phase from checkpoint + status.
fn derive_tpn_state(task: &Task, checkpoint: &Option<Checkpoint>) -> (TpnPhase, u32, u32) {
    let (round, cycle) = checkpoint
        .as_ref()
        .map(|c| (c.round, c.cycle))
        .unwrap_or((0, 0));

    if matches!(task.status, TaskStatus::Completed) {
        return (TpnPhase::Converged, round, cycle);
    }

    let phase = match checkpoint.as_ref().map(|c| &c.phase) {
        Some(CyclePhase::MetaDone) => TpnPhase::Meta,
        Some(CyclePhase::FittingDone) => TpnPhase::Fitting,
        Some(CyclePhase::VerifyDone) => TpnPhase::Causal,
        None => TpnPhase::Idle,
    };

    (phase, round, cycle)
}

/// Count files in a directory (non-recursive).
fn count_files(dir: &Path) -> u32 {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().filter(|e| e.path().is_file()).count() as u32)
        .unwrap_or(0)
}

/// Count immediate subdirectories of a directory.
fn count_dirs(dir: &Path) -> u32 {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().filter(|e| e.path().is_dir()).count() as u32)
        .unwrap_or(0)
}

/// Extract unique tool names from `trace.jsonl` (`tool_call::<tool>` phases).
fn collect_tools_used(task_dir: &Path) -> Vec<String> {
    let trace_path = task_dir.join("trace.jsonl");
    let Ok(content) = std::fs::read_to_string(&trace_path) else {
        return Vec::new();
    };

    #[derive(Deserialize)]
    struct MiniRecord {
        phase: String,
    }

    let mut tools: Vec<String> = Vec::new();
    for line in content.lines() {
        let Ok(record) = serde_json::from_str::<MiniRecord>(line) else {
            continue;
        };
        if let Some(tool) = record.phase.strip_prefix("tool_call::") {
            if !tools.iter().any(|t: &String| t == tool) {
                tools.push(tool.to_string());
            }
        }
    }
    tools
}

/// Find this node's sibling index: `task_dir` is `.../children/<idx>`, so the
/// numeric directory name is the index directly.
fn find_sibling_index(task_dir: &Path, _parent_id: &str, _data_root: &Path) -> Option<u32> {
    task_dir.file_name()?.to_string_lossy().parse::<u32>().ok()
}

// ---------------------------------------------------------------------------
// DMN activity
// ---------------------------------------------------------------------------

/// Build a [`DmnActivity`] summary from a list of evolution summaries.
pub fn dmn_activity(evolutions: Vec<EvolutionSummary>) -> Option<DmnActivity> {
    if evolutions.is_empty() {
        None
    } else {
        Some(DmnActivity {
            active_nodes: evolutions.len() as u32,
            recent_evolutions: evolutions,
        })
    }
}

/// Read the most recent evolution summaries from the DMN log file.
pub fn read_dmn_activity(data_root: &Path, max: usize) -> Option<DmnActivity> {
    #[derive(Serialize, Deserialize)]
    struct DmnLogEntry {
        layer: u32,
        asset_id: String,
        delta: String,
        timestamp: String,
    }

    let log_path = data_root.join("dmn_evolution.log");
    if !log_path.is_file() {
        return None;
    }
    let Ok(content) = std::fs::read_to_string(&log_path) else {
        return None;
    };

    let mut entries: Vec<DmnLogEntry> = Vec::new();
    for line in content.lines().rev() {
        if entries.len() >= max {
            break;
        }
        if let Ok(entry) = serde_json::from_str::<DmnLogEntry>(line) {
            entries.push(entry);
        }
    }
    entries.reverse();

    if entries.is_empty() {
        None
    } else {
        Some(DmnActivity {
            active_nodes: entries.len() as u32,
            recent_evolutions: entries
                .into_iter()
                .map(|e| EvolutionSummary {
                    layer: e.layer,
                    asset_id: e.asset_id,
                    delta: e.delta,
                    timestamp: e.timestamp,
                })
                .collect(),
        })
    }
}
