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
        let mut hi = truncated.len().min(max_bytes);
        while !truncated.is_char_boundary(hi) {
            hi -= 1;
        }
        truncated.truncate(hi);
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
            // Walk ancestors to find the longest existing prefix
            let mut parent = path.parent();
            let mut ancestor_exists = PathBuf::new();

            while let Some(p) = parent {
                if p.exists() {
                    match p.canonicalize() {
                        Ok(c) => {
                            ancestor_exists = c;
                            break;
                        }
                        Err(_) => {
                            parent = p.parent();
                            continue;
                        }
                    }
                }
                parent = p.parent();
            }

            if ancestor_exists.as_os_str().is_empty() {
                return path.to_path_buf();
            }

            // Append the remaining non-existing suffix
            let suffix: PathBuf = path
                .strip_prefix(&ancestor_exists)
                .unwrap_or(path)
                .components()
                .collect();
            ancestor_exists.join(suffix)
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
