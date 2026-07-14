use serde::{Deserialize, Serialize};

/// Structured task specification (YAML frontmatter + markdown body).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub title: String,
    pub description: String,
    pub verification_spec: String,
    #[serde(default)]
    pub task_type_tags: Vec<String>,
    #[serde(default)]
    pub milestones: Vec<Milestone>,
    #[serde(default)]
    pub context: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub name: String,
    pub description: String,
    pub verification: String,
}
