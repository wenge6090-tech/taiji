//! WriteTool — atomic file writing via tempfile.
//!
//! Adapted from pi_agent_rust's WriteTool for taiji's tokio runtime.

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use super::common::{self, enforce_cwd_scope, WRITE_TOOL_MAX_BYTES};
use super::BuiltinSkill;
use crate::infra::error::TaijiError;

/// Built-in `write` skill.
#[derive(Debug, Clone, Default)]
pub struct WriteTool;

#[async_trait]
impl BuiltinSkill for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                TaijiError::Other("write: missing required 'path' argument".into())
            })?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                TaijiError::Other("write: missing required 'content' argument".into())
            })?;

        // Size check
        if content.len() as u64 > WRITE_TOOL_MAX_BYTES {
            return Err(TaijiError::Other(format!(
                "write: content exceeds maximum size of {} bytes",
                WRITE_TOOL_MAX_BYTES,
            )));
        }

        let path = common::expand_tilde(path_str);
        let cwd = std::env::current_dir().map_err(|e| {
            TaijiError::Other(format!("write: cannot get current directory: {e}"))
        })?;
        let canonical = enforce_cwd_scope(&path, &cwd, "write to")?;

        // Create parent directories
        if let Some(parent) = canonical.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                TaijiError::Other(format!(
                    "write: cannot create parent directories for '{}': {e}",
                    canonical.display(),
                ))
            })?;
        }

        // Atomic write: create temp file in the SAME directory as the target,
        // so the atomic rename works within the same filesystem.
        let tmp_dir = canonical.parent().unwrap_or(std::path::Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(tmp_dir).map_err(|e| {
            TaijiError::Other(format!("write: cannot create temp file: {e}"))
        })?;

        use std::io::Write;
        tmp.write_all(content.as_bytes()).map_err(|e| {
            TaijiError::Other(format!("write: failed to write temp file: {e}"))
        })?;

        tmp.persist(&canonical).map_err(|e| {
            TaijiError::Other(format!(
                "write: failed to atomically write '{}': {e}",
                canonical.display(),
            ))
        })?;

        Ok(serde_json::json!({
            "tool": "write",
            "status": "ok",
            "path": canonical.to_string_lossy(),
            "bytes_written": content.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_write_tool_missing_path() {
        let tool = WriteTool;
        let args = serde_json::json!({"content": "hello"});
        let result = tool.call(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_tool_missing_content() {
        let tool = WriteTool;
        let args = serde_json::json!({"path": "/tmp/test.txt"});
        let result = tool.call(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore] // requires filesystem
    async fn test_write_tool_atomic_write() {
        let tmp_dir = std::env::current_dir().unwrap().join("target/taiji_write_test");
        let _ = std::fs::remove_dir_all(&tmp_dir);

        let tool = WriteTool;
        let test_path = tmp_dir.join("test.txt");
        let args = serde_json::json!({
            "path": test_path.to_string_lossy(),
            "content": "hello world"
        });
        let result = tool.call(&args).await;
        assert!(result.is_ok());

        let val = result.unwrap();
        assert_eq!(val["status"], "ok");
        assert_eq!(val["bytes_written"], 11);

        // Verify content
        let read_back = std::fs::read_to_string(&test_path).unwrap();
        assert_eq!(read_back, "hello world");

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
