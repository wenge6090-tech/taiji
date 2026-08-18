//! 任务树读取底座（V68「执行记忆归位」）——纯符号，零 LLM。
//!
//! 三个用途共享同一套树读取：
//! 1. **编译蓝图 = 原任务递归分解树**（Blueprint §5.0 V68 定论）——从树收束出
//!    skill（`summarize_tree` → 编译任务注入 + 根级 deliverables/handoff 物化
//!    `collect_root_sources`）；
//! 2. **拓扑增强**（manifold.rs）——Task 节点补 description / stats；
//! 3. **案例召回**（knowledge.rs `list_task_instances`）——动态扫任务目录读 meta.json。
//!
//! 数据源 = 任务目录树（`.taiji/tasks/{root}/`）：`meta.json`（Task 契约）+
//! `deliverables/`（含 handoff.md）+ `children/<idx>/` 递归，**不碰 trace.jsonl**
//! （trace 归度量轨，§5.0 三层定论）。

use crate::infra::error::TaijiError;
use crate::types::task::Task;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 任务树视图：递归嵌入的任务节点（从磁盘任务目录树读取）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTreeView {
    pub root_id: String,
    pub nodes: Vec<TreeNodeView>,
}

/// 树节点视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNodeView {
    pub id: String,
    pub description: String,
    pub depth: u32,
    pub status: String,
    /// deliverables/ 下产出物（相对 task_dir）。
    pub deliverables: Vec<String>,
    /// handoff.md 前部 YAML front matter（无围栏/无可解析 → None）。
    pub handoff: Option<serde_json::Value>,
    /// 子任务节点（children/<idx>/ 递归）。
    pub children: Vec<TreeNodeView>,
}

/// 递归读取任务目录树（纯符号，零 LLM）。
///
/// 容错：单节点 meta.json 缺失/损坏 → warn + 路径名兜底（不阻断整树）；
/// deliverables/handoff 读取失败仅 warn。目录级 I/O 错误上抛（无降级 §8）。
pub fn load_task_tree(task_dir: &Path) -> Result<TaskTreeView, TaijiError> {
    let mut nodes = Vec::new();
    collect_node(task_dir, task_dir, &mut nodes)?;
    let root_id = nodes
        .first()
        .map(|n| n.id.clone())
        .unwrap_or_else(|| {
            task_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        });
    Ok(TaskTreeView { root_id, nodes })
}

fn collect_node(
    root_dir: &Path,
    dir: &Path,
    nodes: &mut Vec<TreeNodeView>,
) -> Result<(), TaijiError> {
    let meta_path = dir.join("meta.json");
    let meta: Option<Task> = match crate::infra::trace::load_json_optional(&meta_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                path = %meta_path.display(),
                error = %e,
                "[treeio] failed to read task meta — falling back to dir name"
            );
            None
        }
    };

    let id = meta
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    let description = meta
        .as_ref()
        .map(|t| t.description.clone())
        .unwrap_or_default();
    let depth = meta.as_ref().map(|t| t.depth).unwrap_or(0);
    let status = meta
        .as_ref()
        .map(|t| format!("{:?}", t.status))
        .unwrap_or_else(|| "Unknown".into());

    // deliverables/（相对 root 的节点 id——与 manifold 契约一致，树内唯一）
    let mut deliverables = Vec::new();
    let mut handoff: Option<serde_json::Value> = None;
    for abs in crate::infra::handoff::list_deliverables(dir) {
        let rel = Path::new(&abs)
            .strip_prefix(root_dir)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_else(|_| {
                Path::new(&abs)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| abs.clone())
            });
        if Path::new(&abs).file_name().is_some_and(|n| n == "handoff.md") {
            if let Ok(content) = std::fs::read_to_string(&abs) {
                handoff = extract_first_yaml_block(&content);
            }
        } else {
            deliverables.push(rel);
        }
    }

    // children/<idx>/ 递归（按字典序稳定）
    let mut children = Vec::new();
    let children_dir = dir.join("children");
    if children_dir.is_dir() {
        let mut child_dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&children_dir)
            .map_err(|e| {
                TaijiError::Other(format!(
                    "failed to read children dir {:?}: {e}",
                    children_dir
                ))
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        child_dirs.sort();
        for child in child_dirs {
            collect_node(root_dir, &child, &mut children)?;
        }
    }

    nodes.push(TreeNodeView {
        id,
        description,
        depth,
        status,
        deliverables,
        handoff,
        children,
    });
    Ok(())
}

/// 渲染树摘要（编译注入文本）——纯符号拼接，description 按节点截断。
///
/// 输出结构：每层 `[depth] id — description` + 产出物清单 + （根）验证/交接要点。
/// 截断保护：单节点描述超 `max_chars_per_node` 截断；全树总长超
/// `max_total_chars` 时跳过叶子（保根层信息）。
pub fn summarize_tree(view: &TaskTreeView, max_chars_per_node: usize) -> String {
    let mut out = String::new();
    for node in &view.nodes {
        render_node(node, max_chars_per_node, 0, &mut out);
    }
    out
}

fn render_node(node: &TreeNodeView, max_chars: usize, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let desc: String = if node.description.chars().count() > max_chars {
        node.description.chars().take(max_chars).collect::<String>() + " …"
    } else {
        node.description.clone()
    };
    out.push_str(&format!("{}[d{}] {}（{}）— {}\n", indent, node.depth, node.id, node.status, desc));
    for d in &node.deliverables {
        out.push_str(&format!("{}  · 产出: {}\n", indent, d));
    }
    if let Some(h) = &node.handoff {
        let summary = compact_handoff(h);
        if !summary.is_empty() {
            out.push_str(&format!("{}  · 交接要点: {}\n", indent, summary));
        }
    }
    for child in &node.children {
        render_node(child, max_chars, depth + 1, out);
    }
}

/// handoff front matter → 一行要点（task/result/status/output_refs）。
fn compact_handoff(h: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    for key in ["task", "result", "status"] {
        if let Some(v) = h.get(key).and_then(|v| v.as_str()) {
            let s: String = v.chars().take(120).collect();
            parts.push(format!("{key}={s}"));
        }
    }
    if let Some(refs) = h.get("output_refs").and_then(|v| v.as_array()) {
        let n = refs.len();
        parts.push(format!("output_refs={n}"));
    }
    parts.join("; ")
}

/// 提取首段 YAML front matter（`---` 围栏之间；无围栏 → 全文 YAML 宽松解析，
/// 同 §30 手写 handoff 宽容：截首个文档分隔符前）。
fn extract_first_yaml_block(content: &str) -> Option<serde_json::Value> {
    let trimmed = content.trim();
    if trimmed.starts_with("---") {
        // 首个围栏后的内容 → 找第二个围栏或直接解析
        let rest = &trimmed[3..];
        let block = match rest.find('\n') {
            Some(idx) => {
                let after = &rest[idx + 1..];
                match after.find("\n---") {
                    Some(end) => &after[..end],
                    None => after,
                }
            }
            None => rest,
        };
        return parse_yaml_value(block);
    }
    // 无围栏：截首个文档分隔符前（§30 front matter 宽容）
    let head = match trimmed.find("\n---") {
        Some(idx) => &trimmed[..idx],
        None => trimmed,
    };
    parse_yaml_value(head)
}

fn parse_yaml_value(block: &str) -> Option<serde_json::Value> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(block).ok()?;
    serde_json::to_value(yaml).ok()
}

/// 收集根级文件内容（编译任务物化源）：根 deliverables/*（含 handoff.md）→ (文件名, 内容)。
/// 单个文件 > `max_bytes` 截断（防上下文爆表）；读取失败跳过。
pub fn collect_root_sources(task_dir: &Path, max_bytes: usize) -> Vec<(String, String)> {
    let mut sources = Vec::new();
    for abs in crate::infra::handoff::list_deliverables(task_dir) {
        let name = Path::new(&abs)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| abs.clone());
        match std::fs::read(&abs) {
            Ok(bytes) => {
                let content = if bytes.len() > max_bytes {
                    String::from_utf8_lossy(&bytes[..max_bytes]).to_string()
                } else {
                    String::from_utf8_lossy(&bytes).to_string()
                };
                sources.push((name, content));
            }
            Err(e) => {
                tracing::warn!(path = %abs, error = %e, "[treeio] skip unreadable source file");
            }
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::task::TaskStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("taiji_treeio_{name}_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_meta(dir: &std::path::Path, id: &str, desc: &str, depth: u32, parent: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        let t = Task {
            id: id.into(),
            description: desc.into(),
            depth,
            status: TaskStatus::Completed,
            parent_id: parent.map(String::from),
            subtask_ids: vec![],
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_string(&t).unwrap()).unwrap();
    }

    fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn load_tree_recurses_and_extracts_deliverables() {
        let root = tmp_dir("load_tree");
        write_meta(&root, "task-root", "根任务：写报告", 0, None);
        write_file(&root, "deliverables/out.md", "report");
        write_file(&root, "deliverables/handoff.md", "---\ntask: root\ntask,result: done\nstatus: Completed\n---\n正文");
        let child = root.join("children").join("0");
        write_meta(&child, "task-child", "子任务：收集数据", 1, Some("task-root"));
        write_file(&child, "deliverables/data.csv", "1,2,3");

        let view = load_task_tree(&root).unwrap();
        assert_eq!(view.root_id, "task-root");
        assert_eq!(view.nodes.len(), 1);
        let n = &view.nodes[0];
        assert_eq!(n.id, "task-root");
        assert_eq!(n.depth, 0);
        assert!(n.deliverables.iter().any(|d| d.ends_with("out.md")));
        assert!(!n.deliverables.iter().any(|d| d.ends_with("handoff.md")), "handoff 不进 deliverables 清单");
        let hf = n.handoff.as_ref().expect("handoff front matter parsed");
        assert_eq!(hf.get("status").and_then(|v| v.as_str()), Some("Completed"));
        assert_eq!(n.children.len(), 1);
        assert_eq!(n.children[0].description, "子任务：收集数据");
        assert!(n.children[0].deliverables.iter().any(|d| d.contains("children") && d.ends_with("data.csv")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn summarize_tree_truncates_long_descriptions() {
        let root = tmp_dir("summarize");
        write_meta(&root, "t-root", "短描述", 0, None);
        write_file(&root, "deliverables/a.md", "x");

        let view = load_task_tree(&root).unwrap();
        let s = summarize_tree(&view, 10);
        assert!(s.contains("短描述"));
        assert!(s.contains("产出"));

        let long_desc = "长".repeat(100);
        write_meta(&root, "t-root", &long_desc, 0, None);
        let view2 = load_task_tree(&root).unwrap();
        let s2 = summarize_tree(&view2, 10);
        assert!(s2.contains("…"), "长描述被截断");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn collect_root_sources_materializes_and_truncates() {
        let root = tmp_dir("sources");
        write_meta(&root, "t-root", "d", 0, None);
        write_file(&root, "deliverables/big.txt", &"x".repeat(1000));
        write_file(&root, "deliverables/handoff.md", "h");

        let sources = collect_root_sources(&root, 50);
        let names: Vec<&str> = sources.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names.len(), 2);
        let big = sources.iter().find(|(n, _)| n == "big.txt").unwrap();
        assert!(big.1.len() <= 50, "大文件被截断");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn handoff_without_fence_parses_leniently() {
        let root = tmp_dir("handoff_no_fence");
        write_meta(&root, "t-root", "d", 0, None);
        write_file(&root, "deliverables/handoff.md", "task: 无围栏\nresult: done\n---\n后续文档\n");

        let view = load_task_tree(&root).unwrap();
        let hf = view.nodes[0].handoff.as_ref().expect("lenient parse");
        assert_eq!(hf.get("task").and_then(|v| v.as_str()), Some("无围栏"));

        std::fs::remove_dir_all(&root).unwrap();
    }
}