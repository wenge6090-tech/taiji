//! L1 Skill tools — built-in real implementations with optional augmentation
//! via MCP ExternalContext from a frontend agent.
//!
//! Each [`SkillRef`](crate::types::agent::SkillRef) from the MetaContext is
//! wrapped in a [`SkillTool`].  The [`SkillRegistry`] manages the full set
//! and provides tool names suitable for Rig agent registration.
//!
//! The five built-in skills (`read`, `write`, `bash`, `search`, `webfetch`)
//! have real implementations adapted from pi_agent_rust's tool algorithms.

pub mod bash;
pub mod common;
pub mod read;
pub mod search;
pub mod webfetch;
pub mod write;

use std::sync::Arc;

use async_trait::async_trait;
use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolError};
use serde::{Deserialize, Serialize};
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
// Built-in skill implementations (thin wrappers around real modules)
// ---------------------------------------------------------------------------

/// Built-in `read` tool — delegates to [`read::ReadTool`].
#[derive(Debug, Clone, Default)]
pub struct ReadTool;

#[async_trait]
impl BuiltinSkill for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        read::ReadTool.call(args).await
    }
}

/// Built-in `write` tool — delegates to [`write::WriteTool`].
#[derive(Debug, Clone, Default)]
pub struct WriteTool;

#[async_trait]
impl BuiltinSkill for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        write::WriteTool.call(args).await
    }
}

/// Built-in `bash` tool — delegates to [`bash::BashTool`].
#[derive(Debug, Clone, Default)]
pub struct BashTool;

#[async_trait]
impl BuiltinSkill for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        bash::BashTool.call(args).await
    }
}

/// Built-in `search` tool — delegates to [`search::SearchTool`].
#[derive(Debug, Clone, Default)]
pub struct SearchTool;

#[async_trait]
impl BuiltinSkill for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        search::SearchTool.call(args).await
    }
}

/// Built-in `webfetch` tool — delegates to [`webfetch::WebfetchTool`].
#[derive(Debug, Clone, Default)]
pub struct WebfetchTool;

#[async_trait]
impl BuiltinSkill for WebfetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    async fn call(&self, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        webfetch::WebfetchTool.call(args).await
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
            "read" => Some(Arc::new(ReadTool)),
            "write" => Some(Arc::new(WriteTool)),
            "bash" => Some(Arc::new(BashTool)),
            "search" => Some(Arc::new(SearchTool)),
            "webfetch" => Some(Arc::new(WebfetchTool)),
            _ => None,
        }
    }
}

// ── Rig Tool implementation ────────────────────────────────────────────

/// Arguments for a `SkillTool` call.
#[derive(Debug, Deserialize)]
pub struct SkillToolArgs {
    /// Raw input arguments for the skill.
    ///
    /// Two forms are accepted (see the tool definition description for
    /// per-skill usage examples):
    /// - A **plain string** (e.g. `"ls -la"` for bash) — passed through to the
    ///   skill as-is (mapped to the skill's primary parameter, e.g. `command`).
    /// - A **JSON object string** (e.g. `'{"command": "ls -la"}'`) — parsed
    ///   into the skill's parameter keys before the skill is invoked.
    input: Option<String>,
}

/// Serialized output wrapper for tool results.
#[derive(Debug, Serialize)]
pub struct SkillToolOutput(String);

impl Tool for SkillTool {
    const NAME: &'static str = "skill_tool";

    type Error = ToolError;
    type Args = SkillToolArgs;
    type Output = SkillToolOutput;

    /// Dynamic name — returns the skill's tool_name.
    fn name(&self) -> String {
        self.skill.tool_name.clone()
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let desc = input_desc(&self.skill.name);
        ToolDefinition {
            name: self.skill.tool_name.clone(),
            description: format!("L1 Skill: {} — {}. Args: {}", self.skill.id, self.skill.name, desc),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": desc
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Two-form input contract (V26.3 E2):
        // - JSON object string  → parsed into the skill's parameter keys.
        // - Plain string        → passed through as `{"input": "<string>"}` so
        //   single-parameter skills (bash/read/search/webfetch) can consume it
        //   directly via the `input` key.
        let input: JsonValue = match args.input {
            Some(s) if !s.trim().is_empty() => {
                serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({ "input": s }))
            }
            _ => JsonValue::Null,
        };

        self.execute(&input)
            .await
            .map(|v| SkillToolOutput(v.to_string()))
            .map_err(|e| ToolError::ToolCallError(Box::new(e)))
    }
}

/// Per-skill usage description for the `input` argument (exposed in the tool
/// definition so the LLM does not have to guess the argument contract).
fn input_desc(skill_name: &str) -> &'static str {
    match skill_name {
        "bash" => "Shell command. 可传纯字符串命令（如 \"ls -la\"），或 JSON 字符串对象（如 '{\"command\": \"ls -la\"}'，可附 timeout/workdir）。",
        "read" => "File path. 可传纯字符串路径（如 \"src/lib.rs\"），或 JSON 字符串对象（如 '{\"path\": \"src/lib.rs\"}'，可附 offset/limit）。",
        "write" => "必须传 JSON 字符串对象（如 '{\"path\": \"out.md\", \"content\": \"hello\"}'），包含 path 与 content 两个键。",
        "search" => "Search query. 可传纯字符串（如 \"fn main\"），或 JSON 字符串对象（如 '{\"query\": \"fn main\"}'，可附 path/limit）。",
        "webfetch" => "URL. 可传纯字符串 URL（如 \"https://example.com\"），或 JSON 字符串对象（如 '{\"url\": \"https://example.com\"}'）。",
        _ => "Raw input arguments for the skill.",
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
    async fn test_read_tool_returns_result() {
        let tool = SkillTool::new(SkillRef {
            id: "builtin::read".into(),
            name: "read".into(),
            tool_name: "read".into(),
            match_weight: 1.0,
        });
        let args = serde_json::json!({"path": "Cargo.toml"});
        let result = tool.execute(&args).await.unwrap();
        assert_eq!(result["tool"], "read");
        assert_eq!(result["status"], "ok");
        assert!(result["path"].as_str().unwrap_or("").contains("Cargo.toml"));
        assert!(result["content"].as_str().unwrap_or("").len() > 0);
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
        // Provide valid args for each known built-in skill.
        // webfetch is excluded — it requires network; see SSRF tests in webfetch module.
        for tool in reg.tools() {
            let args = match tool.name() {
                "read" => serde_json::json!({"path": "Cargo.toml"}),
                "write" => serde_json::json!({"path": "target/taiji_test_write.txt", "content": "test"}),
                "bash" => serde_json::json!({"command": "echo hello"}),
                "search" => serde_json::json!({"query": "fn main", "path": ".", "limit": 3}),
                _ => continue, // skip webfetch and unknown skills
            };
            let result = tool.execute(&args).await;
            assert!(result.is_ok(), "Skill '{}' failed: {:?}", tool.name(), result.err());
        }
    }

    // ── V26.3 E2: input contract — plain string / JSON object string ──────

    fn bash_tool() -> SkillTool {
        SkillTool::new(SkillRef {
            id: "builtin::bash".into(),
            name: "bash".into(),
            tool_name: "bash".into(),
            match_weight: 1.0,
        })
    }

    #[tokio::test]
    async fn test_skill_tool_accepts_plain_string_input() {
        use rig::tool::Tool;
        let tool = bash_tool();
        // `{"input": "echo hi"}` — plain-string passthrough.
        let out = tool
            .call(SkillToolArgs {
                input: Some("echo hi".into()),
            })
            .await
            .expect("plain string input should work");
        assert!(out.0.contains("hello") || out.0.contains("hi"), "output: {}", out.0);
    }

    #[tokio::test]
    async fn test_skill_tool_accepts_json_string_input() {
        use rig::tool::Tool;
        let tool = bash_tool();
        // `{"input": "{\"command\": \"echo hi\"}"}` — JSON-string-in-input form.
        let out = tool
            .call(SkillToolArgs {
                input: Some(r#"{"command": "echo hi"}"#.into()),
            })
            .await
            .expect("JSON string input should work");
        assert!(out.0.contains("hello") || out.0.contains("hi"), "output: {}", out.0);
    }

    #[tokio::test]
    async fn test_skill_tool_definition_describes_usage() {
        use rig::tool::Tool;
        for name in ["read", "write", "bash", "search", "webfetch"] {
            let tool = SkillTool::new(SkillRef {
                id: format!("builtin::{name}"),
                name: name.into(),
                tool_name: name.into(),
                match_weight: 1.0,
            });
            let def = tool.definition("".into()).await;
            let desc = def.parameters["properties"]["input"]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                desc.contains("input") || desc.contains("JSON") || desc.contains("纯字符串"),
                "tool '{name}' input description should explain usage, got: {desc}"
            );
            assert!(!desc.contains("Raw input arguments for the skill"), "tool '{name}' should have a per-skill usage description, got: {desc}");
        }
    }
}
