//! BashTool — execute shell commands with timeout and process isolation.
//!
//! Adapted from pi_agent_rust's BashTool for taiji's tokio runtime.

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use super::common::{self, BASH_DEFAULT_TIMEOUT_SECS};
use super::BuiltinSkill;
use crate::infra::error::TaijiError;

/// Built-in `bash` skill.
#[derive(Debug, Clone, Default)]
pub struct BashTool;

#[async_trait]
impl BuiltinSkill for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                TaijiError::Other("bash: missing required 'command' argument".into())
            })?;

        let timeout_secs = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(BASH_DEFAULT_TIMEOUT_SECS);

        let workdir: Option<String> = args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());

        // Validate workdir if provided
        let cwd = std::env::current_dir().map_err(|e| {
            TaijiError::Other(format!("bash: cannot get current directory: {e}"))
        })?;

        let effective_workdir = if let Some(ref wd) = workdir {
            let wd_path = std::path::Path::new(wd);
            super::common::enforce_cwd_scope(wd_path, &cwd, "execute in")?;
            wd_path.to_path_buf()
        } else {
            cwd.clone()
        };

        // Spawn shell
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&effective_workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                TaijiError::Other(format!("bash: failed to spawn shell: {e}"))
            })?;

        // Collect output with timeout
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(TaijiError::Other(format!("bash: process error: {e}")));
            }
            Err(_) => {
                // Timeout — the child is killed automatically via kill_on_drop(true)
                return Ok(serde_json::json!({
                    "tool": "bash",
                    "status": "timeout",
                    "command": command,
                    "stdout": "[Command timed out]",
                    "stderr": "",
                    "exit_code": null,
                    "timeout": timeout_secs,
                }));
            }
        };

        // Decode stdout/stderr
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Truncate output (tail mode to capture errors at the end)
        let stdout_trunc = super::common::truncate_tail(
            &stdout,
            common::DEFAULT_MAX_LINES,
            common::DEFAULT_MAX_BYTES,
        );
        let stderr_trunc = super::common::truncate_tail(
            &stderr,
            common::DEFAULT_MAX_LINES,
            common::DEFAULT_MAX_BYTES,
        );

        let exit_code = output.status.code();

        let mut result = serde_json::json!({
            "tool": "bash",
            "status": if output.status.success() { "ok" } else { "error" },
            "command": command,
            "stdout": stdout_trunc.content,
            "stderr": stderr_trunc.content,
            "exit_code": exit_code,
        });

        if stdout_trunc.truncated {
            result["stdout_truncated"] = serde_json::Value::Bool(true);
        }
        if stderr_trunc.truncated {
            result["stderr_truncated"] = serde_json::Value::Bool(true);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bash_tool_echo() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.call(&args).await.unwrap();
        assert_eq!(result["status"], "ok");
        assert!(result["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_tool_missing_command() {
        let tool = BashTool;
        let args = serde_json::json!({});
        let result = tool.call(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_bash_tool_nonzero_exit() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "exit 42"});
        let result = tool.call(&args).await.unwrap();
        assert_eq!(result["status"], "error");
        assert_eq!(result["exit_code"], serde_json::json!(42));
    }

    #[tokio::test]
    async fn test_bash_tool_stderr() {
        let tool = BashTool;
        let args = serde_json::json!({"command": "echo error >&2; echo output"});
        let result = tool.call(&args).await.unwrap();
        assert_eq!(result["status"], "ok");
        assert!(result["stdout"].as_str().unwrap().contains("output"));
        assert!(result["stderr"].as_str().unwrap().contains("error"));
    }
}
