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
    /// Additional named providers (OpenAI-compatible or DeepSeek) available
    /// to the ChatAgent. Empty list means only the default provider is used.
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_provider: "deepseek".into(),
            default_model: "deepseek-chat".into(),
            api_key: String::new(),
            base_url: None,
            agent_overrides: std::collections::HashMap::new(),
            providers: Vec::new(),
        }
    }
}

/// A named extra LLM provider entry (OpenAI-compatible or DeepSeek).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProviderEntry {
    /// Provider name used to reference this entry (e.g. "openai", "local").
    pub name: String,
    /// Base URL of the OpenAI-compatible / DeepSeek endpoint.
    pub base_url: String,
    /// API key for this provider (may be empty for local endpoints).
    pub api_key: String,
    /// Model identifier used by default for this provider.
    pub model: String,
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
#[derive(Default)]
pub struct AgentLlmConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
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
    /// V29 上下文窗口预算（BCP §8.19）：精准 token 计数替换 max_turns 轮次。
    #[serde(default)]
    pub context_limits: ContextLimits,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 4,
            max_depth: 2,
            max_rounds: 10,
            max_cycles: 3,
            max_subtasks: 4,
            // 蓝图 §8.6: 默认 600s（10 分钟），允许复杂任务完整执行
            exec_timeout: 600,
            context_limits: ContextLimits::default(),
        }
    }
}

/// V29 上下文窗口预算（BCP §8.19）— 精准 token 统计替换 max_turns 轮次。
///
/// 统计源：`CompletionResponse.usage.input_tokens`（provider 报告的真实请求
/// token 数，含历史重放与工具结果），经 ContextLimiter hook 累计。
/// 轮次计数器（max_rounds / max_cycles）降级为循环防护，不再承担上下文管理。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::derivable_impls)]
pub struct ContextLimits {
    /// 超限阈值：累计 `usage.input_tokens >= handoff_tokens` → 必须写交接文件
    /// （failure_reason=context_overflow → BACK_TO_TPN → 阳基于产出递归分解）。
    pub handoff_tokens: u64,
    /// 硬截止阈值：`>= hard_cutoff_tokens` → 写交接文件后直接上报 FAIL
    /// （预算保护，不进 BACK_TO_* 循环）。
    pub hard_cutoff_tokens: u64,
    /// 收尾压缩输入截断上限（§8.18 LLM 压缩收尾）：序列化对话截断到此量
    /// （首部 2k 保留任务目标 + 尾部最新状态），防超限路径再花一次大调用。
    pub compress_input_tokens: u64,
}

impl Default for ContextLimits {
    fn default() -> Self {
        Self {
            // BCP §8.19 默认值：250k 交接 / 300k 硬截止，50k 余量即「收尾写交接」预算
            handoff_tokens: 250_000,
            hard_cutoff_tokens: 300_000,
            // BCP §8.18：收尾压缩输入截断上限（字符近似，1 字符 ≤ 1 token 保守上界）
            compress_input_tokens: 20_000,
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
