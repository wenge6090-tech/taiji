use serde::{Deserialize, Serialize};

/// Specifies whether a FittingAgent (概率拟合·阳) operates in **Orchestration**
/// or **Execution** mode.
///
/// | depth | mode |谁决定 |
/// |-------|------|--------|
/// | `0` (root) | `Orchestration` | Runner 固定 |
/// | `1..max_depth-1` | 子任务定 | 父 LLM 在 `SubtaskSpec.mode` 中指定 |
/// | `max_depth` (leaf) | `Execution` | `RecursiveDecomposeTool` 强制覆盖 |
///
/// The `from_depth` helper is intentionally *absent* — mode is never derived
/// from depth alone except at root (hardcoded) and leaf (tool-enforced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMode {
    /// Agent acts as a task decomposer/synthesizer — breaks complex tasks into
    /// subtasks via `recursive_decompose`, delegates, then integrates results.
    Orchestration,
    /// Agent acts as a focused executor — uses L1 skills to directly produce
    /// output, only decomposing when genuinely unable to execute directly.
    Execution,
}

/// Context produced by MetaAgent (权重更新·元), injected as reasoning bias
/// into FittingAgent and CausalAgent.
///
/// MetaAgent queries the 归藏 (cognitive warehouse) and LLM-decides:
/// - Cognitive context (constraints, skills)
/// - The optimal [`AgentMode`] for the task
/// - Composed system prompts for downstream agents
///
/// # Fallback
/// When 归藏 has no matching prompt assets, `fitting_system_prompt`,
/// `verify_system_prompt`, and `converge_system_prompt` are `None`, and
/// downstream agents fall back to their built-in hardcoded templates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaContext {
    pub constraints: Vec<crate::types::verification::TruthConstraint>,
    pub matched_skills: Vec<SkillRef>,
    pub yang_prompt: YangPrompt,

    /// AgentMode decided by MetaAgent based on task type + 归藏 assets.
    /// Defaults to [`AgentMode::Orchestration`] when unset (root task).
    pub mode: AgentMode,

    /// Full system prompt for FittingAgent (概率拟合·阳), LLM-composed by
    /// MetaAgent.  When `None`, FittingAgent uses its built-in template.
    pub fitting_system_prompt: Option<String>,

    /// Full system prompt for CausalAgent.verify() (因果验证·阴).
    /// When `None`, CausalAgent falls back to VERIFY_{ORC,EXEC}_SYSTEM_PROMPT.
    pub verify_system_prompt: Option<String>,

    /// Full system prompt for CausalAgent.converge() (收敛判决).
    /// When `None`, CausalAgent falls back to CONVERGE_{ORC,EXEC}_SYSTEM_PROMPT.
    pub converge_system_prompt: Option<String>,
}

impl MetaContext {
    /// Create an empty/degraded `MetaContext` with no cognitive context.
    ///
    /// All optional prompt fields are `None`, causing downstream agents to
    /// fall back to their built-in hardcoded templates.  Mode defaults to
    /// [`AgentMode::Orchestration`] (safe for root task).
    pub fn empty() -> Self {
        Self {
            constraints: vec![],
            matched_skills: vec![],
            yang_prompt: YangPrompt {
                task_description: String::new(),
                constraint_summaries: vec![],
                parent_deliverables: vec![],
            },
            mode: AgentMode::Orchestration,
            fitting_system_prompt: None,
            verify_system_prompt: None,
            converge_system_prompt: None,
        }
    }
}

/// Reference to an L1 Skill matched by SkillTriggerEngine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRef {
    pub id: String,
    pub name: String,
    pub tool_name: String,
    pub match_weight: f64,
}

/// The prompt context passed to FittingAgent (概率拟合·阳).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YangPrompt {
    pub task_description: String,
    pub constraint_summaries: Vec<String>,
    /// Absolute paths of parent deliverables, injected by `recursive_decompose`.
    /// Read-only reference for the child FittingAgent — the child can read but
    /// cannot write to parent directories.
    #[serde(default)]
    pub parent_deliverables: Vec<String>,
}

// ---------------------------------------------------------------------------
// ExternalContext — context from the calling frontend agent
// ---------------------------------------------------------------------------

/// Context passed from the calling frontend agent through MCP.
///
/// When a frontend agent (any MCP client) calls `taiji_run`, it can provide
/// files it has read, tools it has executed, and a summary of the conversation.
/// This context is injected into the TPN cycle so FittingAgent can reason over
/// data that the frontend already collected — avoiding redundant tool calls.
///
/// All fields are optional.  When `None`/empty, the TPN cycle runs normally
/// with no external context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalContext {
    /// Files the frontend agent has already read.
    #[serde(default)]
    pub files: Vec<ExternalFile>,
    /// Tool results the frontend agent has already collected.
    #[serde(default)]
    pub tool_results: Vec<ExternalToolResult>,
    /// Summary of the conversation or session history.
    pub session_summary: Option<String>,
}

/// A file that the frontend agent read using its `read` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalFile {
    /// Absolute or relative path as seen by the frontend agent.
    pub path: String,
    /// Full text content of the file.
    pub content: String,
}

/// The result of a tool call made by the frontend agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolResult {
    /// Name of the tool that was executed (e.g. "bash", "grep", "find").
    pub tool: String,
    /// Human-readable output or summary of the result.
    pub output: String,
}

// ---------------------------------------------------------------------------
// PromptAsset — 理络 prompt template asset
// ---------------------------------------------------------------------------

/// A prompt template asset stored in the 归藏 cognitive warehouse under
/// `prompts/`.  MetaAgent searches these by task-type tags, ranks by
/// confidence, and LLM-composes them into the final system prompts carried
/// in [`MetaContext`].
///
/// # Directory layout
/// ```text
/// {data_dir}/prompts/
/// ├── orchestration_fitting.yaml
/// ├── execution_fitting.yaml
/// ├── orchestration_verify.yaml
/// └── ...
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptAsset {
    /// Type discriminator — always `"prompt"`.
    /// Skipped in serde because [`CognitiveAsset`]`::#[serde(tag = "type")]`
    /// already provides the `type` key.
    #[serde(skip)]
    pub asset_type: String,
    /// Cognitive layer (1 = Skill, matching L1).
    pub layer: u32,
    /// Unique identifier (file stem).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of when this prompt template is appropriate.
    pub description: String,
    /// Tags for search (task_type, agent_role, mode, ...).
    pub tags: Vec<String>,
    /// Confidence score [0.0, 1.0] — MetaAgent uses this for ranking.
    pub confidence: f64,
    /// Version counter (auto-incremented by LiluoClient).
    pub version: u32,

    /// The prompt template body.
    pub content: String,

    /// Which agent this prompt targets: `"FittingAgent"` or `"CausalAgent"`.
    pub agent_target: String,
    /// Which mode this prompt is designed for.
    pub agent_mode: AgentMode,

    /// Usage statistics (updated by DMN evolver).
    pub usage_count: u32,
    /// Historical success rate [0.0, 1.0].
    pub success_rate: f64,
}

impl PromptAsset {
    /// Create a new `PromptAsset` with default metadata.
    pub fn new(
        id: &str,
        name: &str,
        description: &str,
        content: &str,
        agent_target: &str,
        agent_mode: AgentMode,
        tags: Vec<String>,
    ) -> Self {
        Self {
            asset_type: "prompt".into(),
            layer: 1,
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            tags,
            confidence: 0.5,
            version: 1,
            content: content.to_string(),
            agent_target: agent_target.to_string(),
            agent_mode,
            usage_count: 0,
            success_rate: 0.0,
        }
    }
}
