//! L1 Skill tools — dynamically loaded from NSKG and matched by
//! `SkillTriggerEngine`.
//!
//! Each [`SkillRef`](crate::types::agent::SkillRef) from the MetaContext is
//! wrapped in a [`SkillTool`].  The [`SkillRegistry`] manages the full set
//! and provides tool names suitable for Rig agent registration.
//!
//! A handful of built-in skills (`read`, `write`, `bash`, `search`,
//! `webfetch`) serve as placeholders.  In production they will be replaced
//! or augmented by Qdrant-hosted L1 skill definitions.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::infra::error::TaijiError;
use crate::types::agent::SkillRef;

// ---------------------------------------------------------------------------
// BuiltinSkill trait
// ---------------------------------------------------------------------------

/// Trait implemented by every built-in skill tool.
///
/// Using `#[async_trait]` so that implementors can write natural `async fn
/// call(&self, …)`.
#[async_trait]
pub trait BuiltinSkill: Send + Sync {
    /// Human-readable skill name (e.g. `"read"`, `"bash"`).
    fn name(&self) -> &str;

    /// Execute the skill with the given JSON arguments.
    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError>;
}

// ---------------------------------------------------------------------------
// Built-in skill implementations
// ---------------------------------------------------------------------------

/// Placeholder `read` tool — emulates file reading.
#[derive(Debug, Clone, Default)]
pub struct ReadTool;

#[async_trait]
impl BuiltinSkill for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        Ok(serde_json::json!({
            "tool": "read",
            "status": "ok",
            "path": path,
            "content": format!("[placeholder] contents of {}", path),
        }))
    }
}

/// Placeholder `write` tool — emulates file writing.
#[derive(Debug, Clone, Default)]
pub struct WriteTool;

#[async_trait]
impl BuiltinSkill for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        Ok(serde_json::json!({
            "tool": "write",
            "status": "ok",
            "path": path,
            "bytes_written": 0,
            "note": "[placeholder] write simulated",
        }))
    }
}

/// Placeholder `bash` tool — emulates command execution.
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
            .unwrap_or("<empty>");
        Ok(serde_json::json!({
            "tool": "bash",
            "status": "ok",
            "command": command,
            "stdout": format!("[placeholder] output of: {command}"),
            "stderr": "",
            "exit_code": 0,
        }))
    }
}

/// Placeholder `search` tool — emulates web / knowledge-base search.
#[derive(Debug, Clone, Default)]
pub struct SearchTool;

#[async_trait]
impl BuiltinSkill for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("<empty>");
        Ok(serde_json::json!({
            "tool": "search",
            "status": "ok",
            "query": query,
            "results": [
                {"title": "[placeholder]", "url": "https://example.com/1", "snippet": "…"},
                {"title": "[placeholder]", "url": "https://example.com/2", "snippet": "…"},
            ],
        }))
    }
}

/// Placeholder `webfetch` tool — emulates URL fetching.
#[derive(Debug, Clone, Default)]
pub struct WebfetchTool;

#[async_trait]
impl BuiltinSkill for WebfetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("<empty>");
        Ok(serde_json::json!({
            "tool": "webfetch",
            "status": "ok",
            "url": url,
            "content": format!("[placeholder] fetched content from {url}"),
        }))
    }
}

// ---------------------------------------------------------------------------
// SkillTool — wraps a SkillRef with an optional BuiltinSkill
// ---------------------------------------------------------------------------

/// A fully-qualified skill tool ready for Rig agent registration.
///
/// If the `SkillRef` corresponds to a known built-in skill the `runner`
/// field will carry its implementation; otherwise the tool returns a
/// placeholder result so the LLM can still observe the skill was "called".
pub struct SkillTool {
    /// Original `SkillRef` from the MetaContext / NSKG.
    pub skill: SkillRef,
    /// Optional built-in runner for this skill.
    runner: Option<Arc<dyn BuiltinSkill>>,
}

impl std::fmt::Debug for SkillTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillTool")
            .field("skill", &self.skill)
            .field("has_runner", &self.runner.is_some())
            .finish()
    }
}

impl Clone for SkillTool {
    fn clone(&self) -> Self {
        Self {
            skill: self.skill.clone(),
            runner: self.runner.clone(),
        }
    }
}

impl SkillTool {
    /// Wrap a `SkillRef` into a `SkillTool`.
    ///
    /// The constructor checks the skill name against known built-ins so that
    /// the LLM can exercise common operations immediately.
    pub fn new(skill: SkillRef) -> Self {
        let runner = Self::lookup_builtin(&skill.name);
        Self { skill, runner }
    }

    /// Execute the skill with the given JSON arguments.
    ///
    /// If a built-in runner is available it is delegated to; otherwise a
    /// placeholder result is returned so that the LLM pipeline remains
    /// unblocked.
    pub async fn execute(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        match &self.runner {
            Some(runner) => runner.call(args).await,
            None => Ok(serde_json::json!({
                "tool": self.skill.tool_name,
                "status": "placeholder",
                "note": format!(
                    "Skill '{}' (tool_name='{}') — no built-in runner; call recorded.",
                    self.skill.name, self.skill.tool_name,
                ),
            })),
        }
    }

    /// Name of the skill (used for registration).
    pub fn name(&self) -> &str {
        &self.skill.name
    }

    /// Tool name of the skill (used for LLM tool calling).
    pub fn tool_name(&self) -> &str {
        &self.skill.tool_name
    }

    // ---- internal helpers -------------------------------------------------

    /// Map a skill name to an optional built-in implementation.
    fn lookup_builtin(name: &str) -> Option<Arc<dyn BuiltinSkill>> {
        match name {
            "read" => Some(Arc::new(ReadTool::default())),
            "write" => Some(Arc::new(WriteTool::default())),
            "bash" => Some(Arc::new(BashTool::default())),
            "search" => Some(Arc::new(SearchTool::default())),
            "webfetch" => Some(Arc::new(WebfetchTool::default())),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// SkillRegistry — manages the full tool set
// ---------------------------------------------------------------------------

/// Registry of all L1 skill tools available to a FittingAgent.
///
/// Skills can be loaded from a list of [`SkillRef`]s (produced by the
/// `SkillTriggerEngine`).  The registry also pre-populates the five built-in
/// skills by default.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    tools: Vec<SkillTool>,
}

impl SkillRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        // Pre-populate the five canonical built-in skills.
        let builtins = vec![
            SkillTool::new(SkillRef {
                id: "builtin::read".into(),
                name: "read".into(),
                tool_name: "read".into(),
                match_weight: 1.0,
            }),
            SkillTool::new(SkillRef {
                id: "builtin::write".into(),
                name: "write".into(),
                tool_name: "write".into(),
                match_weight: 1.0,
            }),
            SkillTool::new(SkillRef {
                id: "builtin::bash".into(),
                name: "bash".into(),
                tool_name: "bash".into(),
                match_weight: 1.0,
            }),
            SkillTool::new(SkillRef {
                id: "builtin::search".into(),
                name: "search".into(),
                tool_name: "search".into(),
                match_weight: 1.0,
            }),
            SkillTool::new(SkillRef {
                id: "builtin::webfetch".into(),
                name: "webfetch".into(),
                tool_name: "webfetch".into(),
                match_weight: 1.0,
            }),
        ];

        Self { tools: builtins }
    }

    /// Replace the current tool set with skills parsed from the given
    /// `SkillRef` list.  Built-in skills whose names overlap with loaded
    /// skills are kept but can be overridden.
    pub fn load_from_skills(&mut self, skills: Vec<SkillRef>) {
        // Keep built-in entries that are *not* shadowed by an incoming ref.
        let builtin_names: std::collections::HashSet<String> =
            skills.iter().map(|s| s.name.clone()).collect();

        self.tools.retain(|t| !builtin_names.contains(t.name()));

        for skill_ref in skills {
            self.tools.push(SkillTool::new(skill_ref));
        }
    }

    /// Return the list of tool names (`.tool_name`) for Rig agent
    /// registration.
    pub fn get_tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.tool_name().to_owned()).collect()
    }

    /// Immutable reference to the full tool list.
    pub fn tools(&self) -> &[SkillTool] {
        &self.tools
    }

    /// Mutable reference to the full tool list.
    pub fn tools_mut(&mut self) -> &mut Vec<SkillTool> {
        &mut self.tools
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::agent::SkillRef;

    #[tokio::test]
    async fn test_read_tool_returns_placeholder() {
        let tool = SkillTool::new(SkillRef {
            id: "builtin::read".into(),
            name: "read".into(),
            tool_name: "read".into(),
            match_weight: 1.0,
        });
        let args = serde_json::json!({"path": "/tmp/foo.txt"});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result["tool"], "read");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["path"], "/tmp/foo.txt");
    }

    #[tokio::test]
    async fn test_unknown_skill_returns_placeholder() {
        let tool = SkillTool::new(SkillRef {
            id: "custom::foo".into(),
            name: "foo".into(),
            tool_name: "foo".into(),
            match_weight: 0.5,
        });
        let args = serde_json::json!({"input": "bar"});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result["status"], "placeholder");
    }

    #[tokio::test]
    async fn test_skill_registry_has_builtins() {
        let reg = SkillRegistry::new();
        let names = reg.get_tool_names();
        assert!(names.contains(&"read".into()));
        assert!(names.contains(&"write".into()));
        assert!(names.contains(&"bash".into()));
        assert!(names.contains(&"search".into()));
        assert!(names.contains(&"webfetch".into()));
    }

    #[tokio::test]
    async fn test_load_from_skills_replaces_builtins() {
        let mut reg = SkillRegistry::new();
        let custom = vec![SkillRef {
            id: "custom::read".into(),
            name: "read".into(),
            tool_name: "my_read".into(),
            match_weight: 0.9,
        }];
        reg.load_from_skills(custom);

        let names = reg.get_tool_names();
        // "read" was replaced by "my_read"
        assert!(!names.contains(&"read".into()));
        assert!(names.contains(&"my_read".into()));
        // other builtins remain
        assert!(names.contains(&"write".into()));
    }

    #[tokio::test]
    async fn test_builtin_skills_all_executable() {
        let reg = SkillRegistry::new();
        for tool in reg.tools() {
            let args = serde_json::json!({});
            let result = tool.execute(&args).await;
            assert!(result.is_ok(), "Skill '{}' failed", tool.name());
        }
    }
}
