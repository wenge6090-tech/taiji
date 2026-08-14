//! V45 曾用名数据迁移工具（一次性）。
//!
//! 全面改名后，旧任务目录中的持久化文件仍含曾用名字符串值。本工具扫描
//! `{data_root}/tasks/`（递归 `children/`），对三类文件做旧值→新值的
//! **文本替换**（JSON 值替换，不依赖反序列化——旧值无法反序列化的文件
//! 也能迁移；幂等——新值文件不会二次替换）：
//!
//! - `checkpoint.json`：`CyclePhase` 变体 `"FittingDone"`→`"YangDone"`、
//!   `"VerifyDone"`→`"YinDone"`
//! - `verify_state.json`：`VerificationRoute` 变体 `"BackToTpn"`→`"BackToZhouyi"`
//! - `meta_ctx.json`：字段键 `fitting_system_prompt`→`yang_system_prompt`
//!
//! 迁移后旧任务目录可被当前代码正常反序列化（checkpoint 恢复 / verify 缓存 /
//! meta 上下文加载）。

use std::path::{Path, PathBuf};

use crate::infra::error::TaijiError;

/// 迁移条目：文件名 + (旧值, 新值) 替换表。
const MIGRATIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "checkpoint.json",
        &[("\"FittingDone\"", "\"YangDone\""), ("\"VerifyDone\"", "\"YinDone\"")],
    ),
    (
        "verify_state.json",
        &[("\"BackToTpn\"", "\"BackToZhouyi\"")],
    ),
    (
        "meta_ctx.json",
        &[("\"fitting_system_prompt\"", "\"yang_system_prompt\"")],
    ),
];

/// 递归收集任务目录（根任务 + 所有 `children/<idx>/` 子树）。
fn collect_task_dirs(root: &Path, out: &mut Vec<PathBuf>) {
    if root.join("meta.json").is_file() {
        out.push(root.to_path_buf());
    }
    let children = root.join("children");
    if children.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&children) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    collect_task_dirs(&p, out);
                }
            }
        }
    }
}

/// 对单个文件做旧值→新值文本替换。返回是否发生了替换。
fn migrate_file(path: &Path, rules: &[(&str, &str)]) -> Result<bool, TaijiError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| TaijiError::Other(format!("读取 {} 失败: {e}", path.display())))?;
    let mut new_content = content.clone();
    for (old, new) in rules {
        new_content = new_content.replace(old, new);
    }
    if new_content == content {
        return Ok(false);
    }
    // 原子写（tmp + rename，与 save_json_atomic 同模式）。
    let tmp = path.with_extension("migrate.tmp");
    std::fs::write(&tmp, &new_content)
        .map_err(|e| TaijiError::Other(format!("写入 {} 失败: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| TaijiError::Other(format!("替换 {} 失败: {e}", path.display())))?;
    Ok(true)
}

/// 迁移一个任务目录下的所有目标文件。返回 (任务目录, 迁移文件数)。
fn migrate_task_dir(task_dir: &Path) -> Result<(PathBuf, usize), TaijiError> {
    let mut migrated = 0usize;
    for (file, rules) in MIGRATIONS {
        let path = task_dir.join(file);
        if path.is_file() {
            if migrate_file(&path, rules)? {
                migrated += 1;
            }
        }
    }
    Ok((task_dir.to_path_buf(), migrated))
}

/// 迁移 `{data_root}/tasks/` 下所有任务（含子任务）的持久化文件。
///
/// 返回迁移的任务目录总数。
pub async fn migrate_all(data_root: &Path) -> Result<usize, TaijiError> {
    let tasks_root = data_root.join("tasks");
    if !tasks_root.is_dir() {
        return Err(TaijiError::Other(format!(
            "任务目录不存在: {}",
            tasks_root.display()
        )));
    }
    let mut dirs = Vec::new();
    match std::fs::read_dir(&tasks_root) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    collect_task_dirs(&p, &mut dirs);
                }
            }
        }
        Err(e) => {
            // 批5 P2 修复：read_dir 失败 warn 而非静默返回 Ok(0)（运维工具需可见）。
            tracing::warn!(error = %e, dir = %tasks_root.display(), "migrate: read_dir failed");
        }
    }
    let mut touched = 0usize;
    for dir in &dirs {
        let (d, n) = migrate_task_dir(dir)?;
        if n > 0 {
            tracing::info!(task_dir = %d.display(), files = n, "V45 迁移: 旧值→新值");
            touched += 1;
        }
    }
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("taiji_migrate_{tag}_{}_{}", std::process::id(), n))
    }

    #[test]
    fn test_migrate_file_checkpoint() {
        let dir = tmp_dir("checkpoint");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("checkpoint.json");
        std::fs::write(&p, r#"{"phase":"FittingDone","round":3,"cycle":1}"#).unwrap();
        let rules = &[("\"FittingDone\"", "\"YangDone\""), ("\"VerifyDone\"", "\"YinDone\"")];
        assert!(migrate_file(&p, rules).unwrap());
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after, r#"{"phase":"YangDone","round":3,"cycle":1}"#);
        // 幂等：再次迁移不替换
        assert!(!migrate_file(&p, rules).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_migrate_file_verify_state() {
        let dir = tmp_dir("verify");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("verify_state.json");
        std::fs::write(&p, r#"{"report":{"route":"BackToTpn","confidence":0.8}}"#).unwrap();
        let rules = &[("\"BackToTpn\"", "\"BackToZhouyi\"")];
        assert!(migrate_file(&p, rules).unwrap());
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after, r#"{"report":{"route":"BackToZhouyi","confidence":0.8}}"#);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_migrate_file_meta_ctx() {
        let dir = tmp_dir("meta");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("meta_ctx.json");
        std::fs::write(&p, r#"{"mode":"Execution","fitting_system_prompt":"x"}"#).unwrap();
        let rules = &[("\"fitting_system_prompt\"", "\"yang_system_prompt\"")];
        assert!(migrate_file(&p, rules).unwrap());
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after, r#"{"mode":"Execution","yang_system_prompt":"x"}"#);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_collect_task_dirs_recursive() {
        let dir = tmp_dir("tree");
        // tasks/root/meta.json + children/0/meta.json + children/1/children/0/meta.json
        let root = dir.join("root");
        std::fs::create_dir_all(root.join("children/0")).unwrap();
        std::fs::create_dir_all(root.join("children/1/children/0")).unwrap();
        std::fs::write(root.join("meta.json"), "{}").unwrap();
        std::fs::write(root.join("children/0/meta.json"), "{}").unwrap();
        std::fs::write(root.join("children/1/children/0/meta.json"), "{}").unwrap();
        // 无 meta.json 的目录不应被收集
        std::fs::create_dir_all(root.join("children/2")).unwrap();
        let mut dirs = Vec::new();
        collect_task_dirs(&root, &mut dirs);
        assert_eq!(dirs.len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
