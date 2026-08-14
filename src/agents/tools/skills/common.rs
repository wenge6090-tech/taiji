//! Shared utility functions for L1 Skill implementations.
//!
//! Provides path resolution & sandboxing, output truncation, and artifact
//! spill-over, adapted from pi_agent_rust's tool infrastructure for
//! taiji's tokio runtime.

use std::path::{Path, PathBuf};

use crate::infra::error::TaijiError;

// ---------------------------------------------------------------------------
// Constants (matches pi_agent DEFAULT_MAX_LINES / DEFAULT_MAX_BYTES)
// ---------------------------------------------------------------------------

/// Default maximum number of lines in tool output before truncation.
pub const DEFAULT_MAX_LINES: usize = 2000;
/// Default maximum number of bytes in tool output before truncation.
pub const DEFAULT_MAX_BYTES: usize = 1_000_000; // 1 MB

/// Maximum size for an individual read file (100 MB).
pub const READ_TOOL_MAX_BYTES: u64 = 100 * 1024 * 1024;
/// Maximum size for an individual write content (100 MB).
pub const WRITE_TOOL_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// Hard limit on grep search results.
pub const GREP_DEFAULT_LIMIT: usize = 50;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Default bash timeout in seconds.
pub const BASH_DEFAULT_TIMEOUT_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// TruncationResult
// ---------------------------------------------------------------------------

/// Outcome of a truncation operation.
#[derive(Debug, Clone)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub total_lines: usize,
    pub output_lines: usize,
}

/// Truncate to the *first* `max_lines` / `max_bytes`.
pub fn truncate_head(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let total_lines = content.lines().count();

    if content.len() <= max_bytes && total_lines <= max_lines {
        return TruncationResult {
            content: content.to_owned(),
            truncated: false,
            total_lines,
            output_lines: total_lines,
        };
    }

    // Lines-first truncation: keep only first `max_lines` lines
    let lines: Vec<&str> = content.lines().take(max_lines).collect();
    let mut truncated = lines.join("\n");

    // If we accidentally blew the byte budget, cut further
    if truncated.len() > max_bytes {
        // Binary search for a safe byte boundary
        let mut hi = truncated.len().min(max_bytes);
        while !truncated.is_char_boundary(hi) {
            hi -= 1;
        }
        truncated.truncate(hi);
    }

    // Add trailing newline if original had one and we removed it
    if content.ends_with('\n') && !truncated.ends_with('\n') {
        truncated.push('\n');
    }

    let output_lines = truncated.lines().count();
    TruncationResult {
        content: truncated,
        truncated: true,
        total_lines,
        output_lines,
    }
}

/// Truncate to the *last* `max_lines` / `max_bytes`.
pub fn truncate_tail(content: &str, max_lines: usize, max_bytes: usize) -> TruncationResult {
    let total_lines = content.lines().count();

    if content.len() <= max_bytes && total_lines <= max_lines {
        return TruncationResult {
            content: content.to_owned(),
            truncated: false,
            total_lines,
            output_lines: total_lines,
        };
    }

    // Keep only last `max_lines` lines
    let lines: Vec<&str> = content.lines().collect();
    let tail_lines: Vec<&str> = lines.iter().copied().rev().take(max_lines).rev().collect();
    let mut truncated = tail_lines.join("\n");

    if truncated.len() > max_bytes {
        // 批10 P2 修复：tail 语义应保留**末尾** max_bytes 字节（错误信息
        // 通常在尾部），而非从头截断丢末尾（原 truncate(hi) 保开头）。
        let mut start = truncated.len() - max_bytes;
        while start < truncated.len() && !truncated.is_char_boundary(start) {
            start += 1;
        }
        truncated = truncated[start..].to_string();
    }

    if content.ends_with('\n') && !truncated.ends_with('\n') {
        truncated.push('\n');
    }

    let output_lines = truncated.lines().count();
    TruncationResult {
        content: truncated,
        truncated: true,
        total_lines,
        output_lines,
    }
}

// ---------------------------------------------------------------------------
// Path sandboxing
// ---------------------------------------------------------------------------

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with('~')
        && let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(path.replacen('~', &home, 1));
        }
    PathBuf::from(path)
}

/// Canonicalize a path, falling back to component-wise resolution when the
/// path doesn't exist yet (e.g. for write targets).
fn safe_canonicalize(path: &Path) -> PathBuf {
    match path.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            // Walk ancestors to find the longest existing prefix, canonicalize
            // that prefix (resolving symlinks), then re-append the non-existing
            // suffix. 批10 P2 修复：suffix 必须用**原始祖先**（词法）截取，而非
            // 已解析祖先——否则含 symlink 的祖先段会导致 strip_prefix 失败、
            // 退化为词法路径，enforce_cwd_scope 的 starts_with 退化为词法比较，
            // symlink 逃逸漏检。
            let mut parent = path.parent();
            while let Some(p) = parent {
                if p.exists() {
                    if let Ok(c) = p.canonicalize() {
                        let suffix = path.strip_prefix(p).unwrap_or(path);
                        return c.join(suffix);
                    }
                }
                parent = p.parent();
            }
            // 无存在祖先（如纯相对新路径）→ 原样返回
            path.to_path_buf()
        }
    }
}

/// Validate that `path` is within the working directory `cwd`.
///
/// Returns a canonicalised absolute path on success, or a `TaijiError` if the
/// path escapes the sandbox.
pub fn enforce_cwd_scope(path: &Path, cwd: &Path, action: &str) -> Result<PathBuf, TaijiError> {
    let expanded = expand_tilde(&path.to_string_lossy());
    let canonical_path = safe_canonicalize(&expanded);
    let canonical_cwd = safe_canonicalize(cwd);

    if !canonical_path.starts_with(&canonical_cwd) {
        return Err(TaijiError::Other(format!(
            "Cannot {} outside the working directory.\n  path: {}\n  cwd:  {}",
            action,
            canonical_path.display(),
            canonical_cwd.display(),
        )));
    }

    Ok(canonical_path)
}

/// A broader scope for `read` — allows both the CWD and the taiji data root.
pub fn enforce_read_scope(
    path: &Path,
    cwd: &Path,
    data_root: Option<&Path>,
) -> Result<PathBuf, TaijiError> {
    let expanded = expand_tilde(&path.to_string_lossy());
    let canonical_path = safe_canonicalize(&expanded);
    let canonical_cwd = safe_canonicalize(cwd);

    if canonical_path.starts_with(&canonical_cwd) {
        return Ok(canonical_path);
    }

    // Also allow the data root (e.g. for reading knowledge assets)
    if let Some(root) = data_root {
        let canonical_root = safe_canonicalize(root);
        if canonical_path.starts_with(&canonical_root) {
            return Ok(canonical_path);
        }
    }

    Err(TaijiError::Other(format!(
        "Cannot read outside the working directory or data root.\n  path: {}\n  cwd:  {}",
        canonical_path.display(),
        canonical_cwd.display(),
    )))
}

/// Spill content to an artifact file when it exceeds a threshold.
///
/// Returns `(truncated_content, artifact_path_option)`.
pub fn spill_to_artifact(
    content: &str,
    task_dir: &Path,
    tool_name: &str,
    threshold: usize,
) -> (String, Option<String>) {
    if content.len() <= threshold {
        return (content.to_owned(), None);
    }

    // Write to task_dir/artifacts/{tool_name}_{timestamp}.txt
    let artifacts_dir = task_dir.join("artifacts");
    let _ = std::fs::create_dir_all(&artifacts_dir);

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let artifact_path = artifacts_dir.join(format!("{}_{}.txt", tool_name, ts));

    // Best-effort write
    let _ = std::fs::write(&artifact_path, content);

    let preview = truncate_head(content, 50, 10_240);
    let artifact_str = artifact_path.to_string_lossy().to_string();

    (
        format!(
            "{}[Full tool output artifact: {} ({} bytes, {} lines)]\n",
            preview.content,
            artifact_str,
            content.len(),
            preview.total_lines,
        ),
        Some(artifact_str),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_tail_keeps_tail_not_head() {
        // 批10 P2 回归：超 max_bytes 时应保留**末尾**字节（错误信息在尾部），
        // 而非从头截断丢末尾（原 truncate(hi) 保开头）。
        let content = "HEAD\n".repeat(50) + &"E".repeat(100); // 350 字节
        let r = truncate_tail(&content, 1000, 64);
        assert!(r.truncated);
        assert_eq!(r.content.len(), 64);
        assert!(
            r.content.chars().all(|c| c == 'E'),
            "tail 语义应保留末尾内容，实际: {:?}",
            r.content
        );
    }

    #[cfg(unix)]
    #[test]
    fn enforce_cwd_scope_blocks_symlink_escape() {
        // 批10 P2 回归：cwd 内 symlink 指向外部目录、目标不存在时，
        // safe_canonicalize 必须解析 symlink 祖先——否则 starts_with 退化为词法
        // 比较，write 新文件可经 symlink 逃逸出沙箱。
        let tmp = std::env::temp_dir().join(format!(
            "taiji_symlink_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let task = tmp.join("task");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, task.join("link")).unwrap();

        let target = task.join("link").join("newfile"); // newfile 不存在
        let r = enforce_cwd_scope(&target, &task, "write to");
        assert!(r.is_err(), "symlink 逃逸应被拦截，实际: {:?}", r);

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn enforce_cwd_scope_allows_legit_new_file_in_cwd() {
        // 正常 cwd 内新文件（write 目标不存在）应放行。
        let tmp = std::env::temp_dir().join(format!(
            "taiji_legit_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let r = enforce_cwd_scope(&tmp.join("deliverables/x.md"), &tmp, "write to");
        assert!(r.is_ok());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
