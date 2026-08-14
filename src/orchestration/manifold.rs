//! 迹拓扑压缩算子（BCP §6.0 蓝图文件契约）——纯符号，零 LLM。
//!
//! 任务目录树（meta.json + deliverables/ + handoff.md）+ pending 字段
//! （assets_used + checks）→ `ManifoldTopology` 状态转移图。
//!
//! 拓扑数据源 = 任务目录树，**不碰 trace.jsonl**（trace 归统计压缩·度量轨；
//! deliverables + handoff.md 归拓扑·结构轨，§6.0 三层定论）。

use crate::infra::error::TaijiError;
use crate::infra::handoff::list_deliverables;
use crate::infra::trace::load_json_optional;
use crate::types::agent::AssetRef;
use crate::types::manifold::{
    ManifoldTopology, TopologyEdge, TopologyEdgeKind, TopologyNode, TopologyNodeKind,
};
use crate::types::task::Task;
use crate::types::verification::CheckResult;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 压缩任务目录树为迹拓扑（纯符号，零 LLM）。
///
/// * `task_dir` — 根任务目录（`data/tasks/{root_task}/`）。
/// * `assets_used` — 根任务编排所用资产（MVP 仅根级 invoke 边）。
/// * `checks` — 根任务验证结果（MVP 仅根级 verify 边）。
pub fn compress_task_tree_to_topology(
    task_dir: &Path,
    assets_used: &[AssetRef],
    checks: &[CheckResult],
) -> Result<ManifoldTopology, TaijiError> {
    let mut nodes: Vec<TopologyNode> = Vec::new();
    let mut edges: Vec<TopologyEdge> = Vec::new();
    let mut root_task: Option<String> = None;

    collect_task_dir(
        task_dir, // root_dir
        task_dir, // dir
        &mut nodes,
        &mut edges,
        &mut root_task,
        None,
    )?;

    let root_task = root_task.unwrap_or_else(|| {
        task_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // invoke 边：根任务 → 资产（MVP 根级）
    for a in assets_used {
        let asset_id = a.id.clone();
        if !nodes
            .iter()
            .any(|n| n.id == asset_id && n.kind == TopologyNodeKind::Asset)
        {
            nodes.push(TopologyNode {
                id: asset_id.clone(),
                kind: TopologyNodeKind::Asset,
                depth: 0,
                stats: Default::default(),
            });
        }
        edges.push(TopologyEdge {
            from: root_task.clone(),
            to: asset_id,
            kind: TopologyEdgeKind::Invoke,
        });
    }

    // verify 边：根任务 → check
    for c in checks {
        edges.push(TopologyEdge {
            from: root_task.clone(),
            to: c.check_id.clone(),
            kind: TopologyEdgeKind::Verify,
        });
    }

    let generated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    Ok(ManifoldTopology {
        root_task,
        generated_at,
        nodes,
        edges,
    })
}

/// 递归遍历任务目录：meta.json → Task 节点 + decompose 边；
/// deliverables/ → Deliverable 节点 + dataflow 边；handoff.md → Handoff 节点 + handoff 边。
fn collect_task_dir(
    root_dir: &Path,
    dir: &Path,
    nodes: &mut Vec<TopologyNode>,
    edges: &mut Vec<TopologyEdge>,
    root_task: &mut Option<String>,
    parent_id: Option<String>,
) -> Result<(), TaijiError> {
    let meta_path = dir.join("meta.json");
    let meta = load_json_optional::<Task>(&meta_path)
        .map_err(|e| TaijiError::Other(format!("failed to read task meta {:?}: {e}", meta_path)))?;

    let task_id = meta.as_ref().map(|t| t.id.clone()).unwrap_or_else(|| {
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    let depth = meta.as_ref().map(|t| t.depth).unwrap_or(0);

    if root_task.is_none() {
        *root_task = Some(task_id.clone());
    }

    nodes.push(TopologyNode {
        id: task_id.clone(),
        kind: TopologyNodeKind::Task,
        depth,
        stats: Default::default(),
    });

    if let Some(p) = parent_id {
        edges.push(TopologyEdge {
            from: p,
            to: task_id.clone(),
            kind: TopologyEdgeKind::Decompose,
        });
    }

    // deliverables/（含 handoff.md）→ Deliverable/Handoff 节点 + dataflow/handoff 边
    for abs in list_deliverables(dir) {
        let is_handoff = Path::new(&abs)
            .file_name()
            .map(|n| n == "handoff.md")
            .unwrap_or(false);
        let node_id = relative_id(root_dir, &abs);
        let (kind, edge_kind) = if is_handoff {
            (TopologyNodeKind::Handoff, TopologyEdgeKind::Handoff)
        } else {
            (TopologyNodeKind::Deliverable, TopologyEdgeKind::Dataflow)
        };
        nodes.push(TopologyNode {
            id: node_id.clone(),
            kind,
            depth: 0,
            stats: Default::default(),
        });
        edges.push(TopologyEdge {
            from: task_id.clone(),
            to: node_id,
            kind: edge_kind,
        });
    }

    // children/<idx>/ 递归
    let children_dir = dir.join("children");
    if children_dir.is_dir() {
        let mut child_dirs: Vec<PathBuf> = std::fs::read_dir(&children_dir)
            .map_err(|e| {
                TaijiError::Other(format!("failed to read children dir {:?}: {e}", children_dir))
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        child_dirs.sort();
        for child in child_dirs {
            collect_task_dir(
                root_dir,
                &child,
                nodes,
                edges,
                root_task,
                Some(task_id.clone()),
            )?;
        }
    }

    Ok(())
}

/// 绝对产出物路径 → 相对 root_dir 的节点 id（跨环境稳定、树内唯一）。
fn relative_id(root_dir: &Path, abs: &str) -> String {
    let p = Path::new(abs);
    p.strip_prefix(root_dir)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| abs.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::task::TaskStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 并行测试临时目录唯一化（AGENTS.md §5：并行测试目录必须唯一）。
    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tmp_dir(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("taiji_manifold_{name}_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_task_meta(dir: &Path, id: &str, depth: u32, parent: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        let task = Task {
            id: id.to_string(),
            description: "test task".into(),
            depth,
            status: TaskStatus::Completed,
            parent_id: parent.map(|s| s.to_string()),
            subtask_ids: vec![],
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_string(&task).unwrap()).unwrap();
    }

    fn write_file(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    #[test]
    fn compress_builds_nodes_and_edges() {
        let root = tmp_dir("compress");
        write_task_meta(&root, "task-root", 0, None);
        write_file(&root, "deliverables/out.md");
        write_file(&root, "deliverables/handoff.md");
        // 子任务
        let child = root.join("children").join("0");
        write_task_meta(&child, "task-child", 1, Some("task-root"));
        write_file(&child, "deliverables/sub.md");

        let assets = vec![AssetRef::new("prompt", "exec-yang")];
        let checks = vec![CheckResult {
            check_id: "file-exists".into(),
            kind: crate::types::verification::CheckKind::FileExists,
            passed: true,
            detail: "ok".into(),
            duration_ms: 0,
            cost_tokens: 0,
            verify_rounds: 0,
            quality: 0.0,
        }];

        let topo = compress_task_tree_to_topology(&root, &assets, &checks).unwrap();

        assert_eq!(topo.root_task, "task-root");

        // 节点：2 task + 2 deliverable + 1 handoff + 1 asset = 6
        let task_nodes: Vec<_> = topo
            .nodes
            .iter()
            .filter(|n| n.kind == TopologyNodeKind::Task)
            .collect();
        assert_eq!(task_nodes.len(), 2, "root + child task nodes");
        assert!(task_nodes.iter().any(|n| n.id == "task-child" && n.depth == 1));

        let handoff_nodes: Vec<_> = topo
            .nodes
            .iter()
            .filter(|n| n.kind == TopologyNodeKind::Handoff)
            .collect();
        assert_eq!(handoff_nodes.len(), 1);
        assert!(handoff_nodes[0].id.ends_with("deliverables/handoff.md"));

        let asset_nodes: Vec<_> = topo
            .nodes
            .iter()
            .filter(|n| n.kind == TopologyNodeKind::Asset)
            .collect();
        assert_eq!(asset_nodes.len(), 1);
        assert_eq!(asset_nodes[0].id, "exec-yang");

        // 边：1 decompose + 1 invoke + 1 verify + 2 dataflow + 1 handoff = 6
        let decompose: Vec<_> = topo
            .edges
            .iter()
            .filter(|e| e.kind == TopologyEdgeKind::Decompose)
            .collect();
        assert_eq!(decompose.len(), 1);
        assert_eq!(decompose[0].from, "task-root");
        assert_eq!(decompose[0].to, "task-child");

        let invoke: Vec<_> = topo
            .edges
            .iter()
            .filter(|e| e.kind == TopologyEdgeKind::Invoke)
            .collect();
        assert_eq!(invoke.len(), 1);
        assert_eq!(invoke[0].to, "exec-yang");

        let verify: Vec<_> = topo
            .edges
            .iter()
            .filter(|e| e.kind == TopologyEdgeKind::Verify)
            .collect();
        assert_eq!(verify.len(), 1);
        assert_eq!(verify[0].to, "file-exists");

        // 产出物节点 id 相对 root，树内唯一（child 的 sub.md 带 children/0 前缀）
        let deliverable_ids: Vec<_> = topo
            .nodes
            .iter()
            .filter(|n| n.kind == TopologyNodeKind::Deliverable)
            .map(|n| n.id.clone())
            .collect();
        assert!(deliverable_ids.iter().any(|d| d.ends_with("deliverables/out.md")));
        assert!(deliverable_ids.iter().any(|d| d.contains("children") && d.ends_with("sub.md")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn compress_empty_dir_yields_root_task_only() {
        let root = tmp_dir("empty");
        write_task_meta(&root, "solo", 0, None);

        let topo = compress_task_tree_to_topology(&root, &[], &[]).unwrap();
        assert_eq!(topo.root_task, "solo");
        assert_eq!(topo.nodes.len(), 1);
        assert_eq!(topo.nodes[0].kind, TopologyNodeKind::Task);
        assert!(topo.edges.is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
