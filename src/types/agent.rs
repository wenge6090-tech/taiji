use serde::{Deserialize, Serialize};

/// A single relation edge traversed during NSKG BFS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chain {
    pub source: String,
    pub target: String,
    pub target_type: String,
    pub relation_type: String,
    pub weight: f64,
    pub interpretation: String,
}

/// A reasoning path: 1-3 hop BFS from a source grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningPath {
    pub source_grid: String,
    pub chains: Vec<Chain>,
    pub depth: u32,
    pub task_type_tags: Vec<String>,
}

/// Context produced by MetaAgent (权重更新·元), injected as reasoning bias into FittingAgent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaContext {
    pub reasoning_paths: Vec<ReasoningPath>,
    pub constraints: Vec<crate::types::verification::TruthConstraint>,
    pub matched_skills: Vec<SkillRef>,
    pub yang_prompt: YangPrompt,
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
    pub reasoning_path_summaries: Vec<String>,
    pub constraint_summaries: Vec<String>,
}
