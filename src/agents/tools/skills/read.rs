//! ReadTool — read file contents with optional offset/limit windowing.
//!
//! Adapted from pi_agent_rust's ReadTool for taiji's tokio runtime.
//!
//! # Argument contract (V26.3 E2)
//! The path may arrive in any of three forms:
//! - `{"path": "src/lib.rs"}` — canonical keyed form.
//! - `{"input": "src/lib.rs"}` — plain-string passthrough from `SkillTool::call`.
//! - `{"input": "{\"path\": \"src/lib.rs\"}"}` — JSON-string-in-input form.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use std::path::Path;

use super::common::{self, enforce_read_scope, truncate_head, READ_TOOL_MAX_BYTES};
use super::BuiltinSkill;
use crate::infra::error::TaijiError;

/// Built-in `read` skill.
#[derive(Debug, Clone, Default)]
pub struct ReadTool;

#[async_trait]
impl BuiltinSkill for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    async fn call(&self, _task_dir: &Path, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let path_str = args
            .get("path")
            .and_then(JsonValue::as_str)
            .or_else(|| args.get("input").and_then(JsonValue::as_str))
            .ok_or_else(|| {
                TaijiError::Other("read: missing required 'path' argument".into())
            })?;

        let offset: Option<usize> = args.get("offset").and_then(|v| v.as_i64()).map(|v| {
            if v < 0 { 0_usize } else { v as usize }
        });
        let limit: Option<usize> = args.get("limit").and_then(|v| v.as_i64()).map(|v| {
            if v < 0 { 0_usize } else { v as usize }
        });

        let path = common::expand_tilde(path_str);
        // Use CWD = current directory for read scope (data_root for broader access)
        let cwd = std::env::current_dir().map_err(|e| {
            TaijiError::Other(format!("read: cannot get current directory: {e}"))
        })?;
        let canonical = enforce_read_scope(&path, &cwd, None)?;

        // 批10 P2 修复：先查元数据大小（避免超大文件整体读入内存），
        // 再读文件；读后仍保留 len 检查兜底（TOCTOU 防御）。
        let metadata = tokio::fs::metadata(&canonical).await.map_err(|e| {
            TaijiError::Other(format!("read: failed to stat '{}': {e}", canonical.display()))
        })?;
        if metadata.len() > READ_TOOL_MAX_BYTES as u64 {
            return Err(TaijiError::Other(format!(
                "read: file '{}' exceeds maximum size of {} bytes",
                canonical.display(),
                READ_TOOL_MAX_BYTES,
            )));
        }

        // Detect binary files by reading first 8 KB
        let file = tokio::fs::read(&canonical).await.map_err(|e| {
            TaijiError::Other(format!("read: failed to read '{}': {e}", canonical.display()))
        })?;

        if file.len() > READ_TOOL_MAX_BYTES as usize {
            return Err(TaijiError::Other(format!(
                "read: file '{}' exceeds maximum size of {} bytes",
                canonical.display(),
                READ_TOOL_MAX_BYTES,
            )));
        }

        // Check for null bytes in first 8KB to detect binary files
        let check_len = file.len().min(8192);
        if file[..check_len].contains(&0u8) {
            return Err(TaijiError::Other(format!(
                "read: '{}' appears to be a binary file (null byte detected)",
                canonical.display(),
            )));
        }

        let content = String::from_utf8(file).map_err(|_| {
            TaijiError::Other(format!(
                "read: '{}' is not valid UTF-8 text",
                canonical.display(),
            ))
        })?;

        // Apply offset/limit window (1-indexed lines)
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let start = offset.unwrap_or(1).max(1) - 1; // 0-indexed
        let end = match limit {
            Some(n) => start + n,
            None => total_lines,
        }
        .min(total_lines);

        let windowed: String = if start == 0 && end == total_lines {
            content
        } else {
            lines[start..end].join("\n")
        };

        // Apply truncation (max 2000 lines / 1 MB)
        let truncated = truncate_head(&windowed, common::DEFAULT_MAX_LINES, common::DEFAULT_MAX_BYTES);

        let mut result = serde_json::json!({
            "tool": "read",
            "status": "ok",
            "path": canonical.to_string_lossy(),
            "content": truncated.content,
            "total_lines": total_lines,
        });

        if truncated.truncated {
            result["truncated"] = serde_json::Value::Bool(true);
            result["output_lines"] = serde_json::Value::Number(serde_json::Number::from(truncated.output_lines));
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_tool_missing_path() {
        let tool = ReadTool;
        let args = serde_json::json!({});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing required 'path'"));
    }

    #[tokio::test]
    async fn test_read_tool_accepts_input_key_plain_string() {
        let tool = ReadTool;
        // V26.3 E2: SkillTool plain-string passthrough (`{"input": "Cargo.toml"}`).
        let args = serde_json::json!({"input": "Cargo.toml"});
        let result = tool.call(std::path::Path::new("."), &args).await.unwrap();
        assert_eq!(result["status"], "ok");
        assert!(result["content"].as_str().unwrap_or("").len() > 0);
    }

    #[tokio::test]
    async fn test_read_tool_binary_detection() {
        let tool = ReadTool;
        // Write a small binary file inside CWD for testing
        let bin_path = std::path::PathBuf::from("target/test_binary.bin");
        let bin_content: Vec<u8> = vec![0x00, 0x01, 0x02, 0xFF]; // Contains null byte
        std::fs::write(&bin_path, &bin_content).ok();

        let args = serde_json::json!({"path": bin_path.to_string_lossy().to_string()});
        let result = tool.call(std::path::Path::new("."), &args).await;
        // Clean up
        std::fs::remove_file(&bin_path).ok();

        assert!(result.is_err(), "Expected error for binary file");
        assert!(result.unwrap_err().to_string().contains("binary file"));
    }

    #[tokio::test]
    async fn test_read_tool_nonexistent_file() {
        let tool = ReadTool;
        let args = serde_json::json!({"path": "/tmp/__nonexistent_file_12345__"});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
    }
}
