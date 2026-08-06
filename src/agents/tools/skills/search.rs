//! SearchTool — search file contents using ripgrep (rg) with JSON output.
//!
//! Adapted from pi_agent_rust's GrepTool for taiji's tokio runtime.

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use super::common::{GREP_DEFAULT_LIMIT, GREP_MAX_LINE_LENGTH};
use super::BuiltinSkill;
use crate::infra::error::TaijiError;

/// A single match result from the search.
#[derive(Debug, Clone, serde::Serialize)]
struct MatchResult {
    file: String,
    line: usize,
    column: usize,
    text: String,
}

/// Built-in `search` skill (uses `rg` / `grep` underneath).
#[derive(Debug, Clone, Default)]
pub struct SearchTool;

impl SearchTool {
    /// Try to spawn ripgrep; returns `(rg_binary, use_json)`.
    fn detect_rg() -> Option<&'static str> {
        ["rg", "ripgrep"].iter().find(|&candidate| std::process::Command::new(candidate)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok()).map(|v| v as _)
    }

    /// Parse a single JSON line from ripgrep's `--json` output.
    fn parse_rg_json_line(line: &str) -> Option<MatchResult> {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value["type"] != "match" {
            return None;
        }
        let data = &value["data"];
        let file = data["path"]["text"].as_str()?.to_owned();
        let line_num = data["line_number"].as_u64()? as usize;
        let column = data.get("absolute_offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let text = data["lines"]["text"].as_str()?.to_owned();
        Some(MatchResult { file, line: line_num, column, text })
    }

    /// Fallback: use `grep -rn` when ripgrep is not available.
    async fn grep_fallback(query: &str, path: &str, limit: usize) -> Result<Vec<MatchResult>, TaijiError> {
        let child = tokio::process::Command::new("grep")
            .args(["-rn", "--", query, path])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                TaijiError::Other(format!("search: failed to spawn grep: {e}"))
            })?;

        let output = child.wait_with_output().await.map_err(|e| {
            TaijiError::Other(format!("search: grep execution failed: {e}"))
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut matches = Vec::new();

        for line_str in stdout.lines().take(limit) {
            // Parse "file:line:content"
            if let Some((file, rest)) = line_str.split_once(':')
                && let Some((line_num_str, text)) = rest.split_once(':')
                    && let Ok(line_num) = line_num_str.parse::<usize>() {
                        let text_trunc: String = text.chars().take(GREP_MAX_LINE_LENGTH).collect();
                        matches.push(MatchResult {
                            file: file.to_owned(),
                            line: line_num,
                            column: 0,
                            text: text_trunc,
                        });
                    }
        }

        Ok(matches)
    }
}

#[async_trait]
impl BuiltinSkill for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let query = args
            .get("query")
            .and_then(JsonValue::as_str)
            .or_else(|| args.get("input").and_then(JsonValue::as_str))
            .ok_or_else(|| {
                TaijiError::Other("search: missing required 'query' argument".into())
            })?;

        let search_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(GREP_DEFAULT_LIMIT as u64) as usize;

        // Validate scope
        let cwd = std::env::current_dir().map_err(|e| {
            TaijiError::Other(format!("search: cannot get current directory: {e}"))
        })?;
        let search_path_buf = std::path::Path::new(search_path);
        let canonical = super::common::enforce_cwd_scope(search_path_buf, &cwd, "search in")?;

        let matches = if let Some(rg) = Self::detect_rg() {
            // Use ripgrep with JSON output
            let child = tokio::process::Command::new(rg)
                .args([
                    "--json", "--line-number", "--color", "never",
                    "--hidden", "--max-columns", "10000",
                    "--max-count", &limit.to_string(),
                    "--", query,
                ])
                .arg(canonical.to_string_lossy().as_ref())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .stdin(std::process::Stdio::null())
                .spawn()
                .map_err(|e| {
                    TaijiError::Other(format!("search: failed to spawn {}: {e}", rg))
                })?;

            let output = child.wait_with_output().await.map_err(|e| {
                TaijiError::Other(format!("search: {} execution failed: {e}", rg))
            })?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut matches: Vec<MatchResult> = stdout
                .lines()
                .filter_map(Self::parse_rg_json_line)
                .take(limit)
                .collect();

            // Deduplicate by (file, line) — rg can produce duplicates with context
            matches.dedup_by(|a, b| a.file == b.file && a.line == b.line);

            matches
        } else {
            // Fallback to grep
            Self::grep_fallback(query, &canonical.to_string_lossy(), limit).await?
        };

        let total = matches.len();
        let truncated = total > limit;

        let match_values: Vec<JsonValue> = matches
            .into_iter()
            .take(limit)
            .map(|m| {
                serde_json::json!({
                    "file": m.file,
                    "line": m.line,
                    "column": m.column,
                    "text": m.text,
                })
            })
            .collect();

        let mut result = serde_json::json!({
            "tool": "search",
            "status": "ok",
            "query": query,
            "path": canonical.to_string_lossy(),
            "matches": match_values,
            "total": total,
        });

        if truncated {
            result["truncated"] = serde_json::Value::Bool(true);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_search_tool_missing_query() {
        let tool = SearchTool;
        let args = serde_json::json!({});
        let result = tool.call(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore] // requires ripgrep or grep
    async fn test_search_tool_self_search() {
        let tool = SearchTool;
        // Search for "SearchTool" in the current source directory
        let args = serde_json::json!({
            "query": "SearchTool",
            "path": "src/agents/tools/skills/search.rs",
            "limit": 5,
        });
        let result = tool.call(&args).await.unwrap();
        assert_eq!(result["status"], "ok");
        assert!(result["total"].as_u64().unwrap_or(0) > 0);
    }
}
