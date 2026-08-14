//! GitBackend — 归藏 git 版本控制（快照式，接口语义对齐 git）。
//!
//! 归藏 = git 版本控制的库（BCP §10.0）。本模块提供 `commit` / `log` /
//! `rollback` / `diff` 四个原语：每次归藏写入 = 一次 commit（全量快照到
//! `{data_dir}/.history/{commit_id}/tree/`），rollback = 从快照恢复，
//! diff = 对比两快照。
//!
//! 自实现（非 libgit2）：核心需求是可回滚 + 可审计 + 可 diff，不需要真 git
//! 的分支合并——fork/merge 的业务语义由资产的 `parent_id` / `variant_of`
//! 字段承载（BCP §10.1）。接口语义对齐 git，未来可无痛替换为 libgit2 后端。

use crate::infra::error::TaijiError;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tokio::fs;

const HISTORY_DIR: &str = ".history";

#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub id: String,
    pub msg: String,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub path: String,
    pub kind: DiffKind,
}

fn err(ctx: impl Into<String>) -> TaijiError {
    TaijiError::KnowledgeStoreUnavailable {
        context: ctx.into(),
    }
}

fn short_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Recursively collect all file paths under `dir` (absolute), skipping the
/// subtree rooted at `exclude` (if any).
async fn walk_files(dir: &Path, exclude: Option<&Path>) -> Result<Vec<PathBuf>, TaijiError> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut rd = fs::read_dir(dir).await.map_err(|e| {
        err(format!("git: failed to read_dir {:?}: {e}", dir))
    })?;
    while let Some(entry) = rd.next_entry().await.map_err(|e| {
        err(format!("git: read_dir entry failed in {:?}: {e}", dir))
    })? {
        let path = entry.path();
        if let Some(ex) = exclude {
            if path == ex {
                continue;
            }
        }
        if path.is_dir() {
            out.extend(Box::pin(walk_files(&path, exclude)).await?);
        } else {
            out.push(path);
        }
    }
    Ok(out)
}

/// Copy all files under `src` into `dst`, skipping the `exclude` subtree.
async fn copy_tree(
    src: &Path,
    dst: &Path,
    exclude: Option<&Path>,
) -> Result<(), TaijiError> {
    let files = walk_files(src, exclude).await?;
    for f in files {
        let rel = f.strip_prefix(src).map_err(|e| {
            err(format!("git: strip_prefix failed for {:?}: {e}", f))
        })?;
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                err(format!("git: create_dir_all {:?}: {e}", parent))
            })?;
        }
        fs::copy(&f, &target).await.map_err(|e| {
            err(format!("git: copy {:?} -> {:?}: {e}", f, target))
        })?;
    }
    Ok(())
}

/// Remove all entries under `dir`, skipping the `exclude` subtree.
async fn clear_tree(dir: &Path, exclude: Option<&Path>) -> Result<(), TaijiError> {
    if !dir.exists() {
        return Ok(());
    }
    let mut rd = fs::read_dir(dir).await.map_err(|e| {
        err(format!("git: failed to read_dir {:?}: {e}", dir))
    })?;
    while let Some(entry) = rd.next_entry().await.map_err(|e| {
        err(format!("git: read_dir entry failed in {:?}: {e}", dir))
    })? {
        let path = entry.path();
        if let Some(ex) = exclude {
            if path == ex {
                continue;
            }
        }
        if path.is_dir() {
            fs::remove_dir_all(&path).await.map_err(|e| {
                err(format!("git: remove_dir_all {:?}: {e}", path))
            })?;
        } else {
            fs::remove_file(&path).await.map_err(|e| {
                err(format!("git: remove_file {:?}: {e}", path))
            })?;
        }
    }
    Ok(())
}

/// Snapshot-backed git-like version control for the 归藏 knowledge tree.
#[derive(Debug)]
pub struct GitBackend {
    /// `{data_dir}/.history`
    history_dir: PathBuf,
}

impl GitBackend {
    /// Initialise the version-control backend under `{data_dir}/.history`.
    pub async fn init(data_dir: &Path) -> Result<Self, TaijiError> {
        let history_dir = data_dir.join(HISTORY_DIR);
        fs::create_dir_all(&history_dir).await.map_err(|e| {
            err(format!(
                "git: failed to create history dir {:?}: {e}",
                history_dir
            ))
        })?;
        Ok(Self { history_dir })
    }

    /// The knowledge root directory (parent of `.history`).
    fn data_dir(&self) -> &Path {
        self.history_dir
            .parent()
            .expect("history dir always has a parent")
    }

    fn commit_dir(&self, id: &str) -> PathBuf {
        self.history_dir.join(id)
    }

    /// Snapshot the current knowledge tree as a new commit.
    ///
    /// Commit id = `{ts_millis:x}-{hash:x}`（单调 + 唯一）。返回 commit id。
    pub async fn commit(&self, msg: &str) -> Result<String, TaijiError> {
        let ts = now_millis();
        let id = format!("{:x}-{:x}", ts, short_hash(msg));
        let dir = self.commit_dir(&id);
        let tree = dir.join("tree");
        fs::create_dir_all(&tree).await.map_err(|e| {
            err(format!("git: create_dir_all {:?}: {e}", tree))
        })?;

        copy_tree(self.data_dir(), &tree, Some(&self.history_dir)).await?;

        let meta = serde_json::json!({ "id": id, "msg": msg, "ts": ts });
        let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(|e| {
            err(format!("git: serialize meta: {e}"))
        })?;
        fs::write(dir.join("meta.json"), meta_bytes).await.map_err(|e| {
            err(format!("git: write meta.json for {id}: {e}"))
        })?;
        Ok(id)
    }

    /// List all commits, oldest first.
    pub async fn log(&self) -> Result<Vec<CommitEntry>, TaijiError> {
        let mut entries = Vec::new();
        if !self.history_dir.exists() {
            return Ok(entries);
        }
        let mut rd = fs::read_dir(&self.history_dir).await.map_err(|e| {
            err(format!("git: read_dir {:?}: {e}", self.history_dir))
        })?;
        while let Some(entry) = rd.next_entry().await.map_err(|e| {
            err(format!("git: read_dir entry failed: {e}"))
        })? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let meta_path = path.join("meta.json");
            if let Ok(content) = fs::read_to_string(&meta_path).await {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let msg = v["msg"].as_str().unwrap_or("").to_string();
                    let ts = v["ts"].as_i64().unwrap_or(0);
                    if !id.is_empty() {
                        entries.push(CommitEntry { id, msg, ts });
                    }
                }
            }
        }
        entries.sort_by_key(|e| e.ts);
        Ok(entries)
    }

    /// Restore the knowledge tree to the state of `commit_id`.
    ///
    /// Clears the current tree (preserving `.history`) then copies the
    /// snapshot back. The rollback itself is not auto-committed — the caller
    /// decides whether to commit a `rollback: revert <id>` entry.
    pub async fn rollback(&self, commit_id: &str) -> Result<(), TaijiError> {
        let tree = self.commit_dir(commit_id).join("tree");
        if !tree.exists() {
            return Err(err(format!(
                "git: commit '{commit_id}' snapshot missing"
            )));
        }
        clear_tree(self.data_dir(), Some(&self.history_dir)).await?;
        copy_tree(&tree, self.data_dir(), None).await?;
        Ok(())
    }

    /// Diff two commits (by id). Paths are relative to the knowledge root.
    pub async fn diff(&self, a: &str, b: &str) -> Result<Vec<DiffEntry>, TaijiError> {
        let ta = self.commit_dir(a).join("tree");
        let tb = self.commit_dir(b).join("tree");
        let files_a = walk_files(&ta, None).await?;
        let files_b = walk_files(&tb, None).await?;

        let rel = |p: &Path, base: &Path| -> String {
            p.strip_prefix(base)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|_| p.to_string_lossy().into_owned())
        };

        let set_a: HashSet<String> = files_a.iter().map(|f| rel(f, &ta)).collect();
        let set_b: HashSet<String> = files_b.iter().map(|f| rel(f, &tb)).collect();

        let mut entries = Vec::new();
        for p in set_b.difference(&set_a) {
            entries.push(DiffEntry {
                path: p.clone(),
                kind: DiffKind::Added,
            });
        }
        for p in set_a.difference(&set_b) {
            entries.push(DiffEntry {
                path: p.clone(),
                kind: DiffKind::Removed,
            });
        }
        for p in set_a.intersection(&set_b) {
            let ca = fs::read(ta.join(p)).await.map_err(|e| {
                err(format!("git: read {:?}: {e}", ta.join(p)))
            })?;
            let cb = fs::read(tb.join(p)).await.map_err(|e| {
                err(format!("git: read {:?}: {e}", tb.join(p)))
            })?;
            if ca != cb {
                entries.push(DiffEntry {
                    path: p.clone(),
                    kind: DiffKind::Modified,
                });
            }
        }
        entries.sort_by(|x, y| x.path.cmp(&y.path));
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "taiji_git_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    #[tokio::test]
    async fn commit_log_and_rollback_roundtrip() {
        let dir = tmp_dir("roundtrip").await;
        let git = GitBackend::init(&dir).await.unwrap();

        // 初始写文件 → commit
        fs::write(dir.join("a.yaml"), "v1").await.unwrap();
        let c1 = git.commit("first").await.unwrap();
        assert_eq!(git.log().await.unwrap().len(), 1);

        // 改文件 → commit
        fs::write(dir.join("a.yaml"), "v2").await.unwrap();
        fs::write(dir.join("b.yaml"), "new").await.unwrap();
        let c2 = git.commit("second").await.unwrap();
        assert_eq!(git.log().await.unwrap().len(), 2);
        assert_ne!(c1, c2);

        // rollback 到 c1：a.yaml 回 v1，b.yaml 消失
        git.rollback(&c1).await.unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.yaml")).await.unwrap(), "v1");
        assert!(!dir.join("b.yaml").exists());

        // rollback 到 c2：a.yaml 回 v2，b.yaml 回来
        git.rollback(&c2).await.unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.yaml")).await.unwrap(), "v2");
        assert!(dir.join("b.yaml").exists());

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn rollback_preserves_history() {
        let dir = tmp_dir("preserve").await;
        let git = GitBackend::init(&dir).await.unwrap();
        fs::write(dir.join("x.yaml"), "1").await.unwrap();
        let c1 = git.commit("c1").await.unwrap();
        fs::write(dir.join("x.yaml"), "2").await.unwrap();
        let _c2 = git.commit("c2").await.unwrap();

        git.rollback(&c1).await.unwrap();
        // .history 仍在（回滚后仍可再回滚到 c2）
        assert!(dir.join(".history").exists());
        let entries = git.log().await.unwrap();
        assert_eq!(entries.len(), 2);

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn diff_detects_added_modified_removed() {
        let dir = tmp_dir("diff").await;
        let git = GitBackend::init(&dir).await.unwrap();

        fs::write(dir.join("same.yaml"), "same").await.unwrap();
        fs::write(dir.join("changed.yaml"), "old").await.unwrap();
        fs::write(dir.join("removed.yaml"), "bye").await.unwrap();
        let c1 = git.commit("c1").await.unwrap();

        fs::write(dir.join("changed.yaml"), "new").await.unwrap();
        fs::write(dir.join("added.yaml"), "hi").await.unwrap();
        fs::remove_file(dir.join("removed.yaml")).await.unwrap();
        let c2 = git.commit("c2").await.unwrap();

        let d = git.diff(&c1, &c2).await.unwrap();
        let kinds: Vec<(String, DiffKind)> = d
            .into_iter()
            .map(|e| (e.path, e.kind))
            .collect();
        assert!(kinds.contains(&("added.yaml".into(), DiffKind::Added)));
        assert!(kinds.contains(&("changed.yaml".into(), DiffKind::Modified)));
        assert!(kinds.contains(&("removed.yaml".into(), DiffKind::Removed)));
        assert!(!kinds.iter().any(|(p, _)| p == "same.yaml"));

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn rollback_missing_commit_errors() {
        let dir = tmp_dir("missing").await;
        let git = GitBackend::init(&dir).await.unwrap();
        let e = git.rollback("nonexistent").await;
        assert!(e.is_err());
        let _ = fs::remove_dir_all(&dir).await;
    }
}
