use serde::{Deserialize, Serialize};

/// Top-level taiji configuration (mirrors Python TaijiConfig schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaijiConfig {
    pub version: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub data_root: String,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub knowledge: KnowledgeConfig,
    #[serde(default)]
    pub safety: SafetyConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub default_provider: String,
    pub default_model: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub agent_overrides: std::collections::HashMap<String, AgentLlmConfig>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_provider: "deepseek".into(),
            default_model: "deepseek-chat".into(),
            api_key: String::new(),
            base_url: None,
            agent_overrides: std::collections::HashMap::new(),
        }
    }
}

impl LlmConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.api_key.is_empty() {
            return Err("LlmConfig.api_key must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentLlmConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
}

impl Default for AgentLlmConfig {
    fn default() -> Self {
        Self {
            provider: None,
            model: None,
            max_turns: None,
            max_tokens: None,
            temperature: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub max_concurrent_agents: usize,
    pub max_depth: u32,
    pub max_rounds: u32,
    pub max_cycles: u32,
    pub max_subtasks: u32,
    #[serde(default)]
    pub exec_timeout: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 4,
            max_depth: 2,
            max_rounds: 10,
            max_cycles: 3,
            max_subtasks: 4,
            exec_timeout: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeConfig {
    pub data_dir: String,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            data_dir: ".taiji/knowledge".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    pub enabled: bool,
    pub trusted_mcp_servers: Vec<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trusted_mcp_servers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub trusted: bool,
    #[serde(default)]
    pub timeout: u64,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}
