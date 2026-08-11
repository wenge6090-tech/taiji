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
    /// V33/MVP-3 DMN 演化配置（§6.3/§6.4：回报权重 / UCB 采样门槛 / 演化激活门槛 / 主动学习）。
    #[serde(default)]
    pub dmn: DmnConfig,
    /// V37 多级模型路由配置（§8.8 相位级异源裁判）。
    #[serde(default)]
    pub model_routing: ModelRoutingConfig,
}

/// V37 多级模型路由配置（BCP §8.8 相位级）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelRoutingConfig {
    /// 异源裁判开关（默认 false）。true 且路由候选 ≥2 时，Causal 验证/收敛
    /// 相位用独立验证模型（裁判 ≠ 运动员——「概率系统不验证概率系统」的
    /// 又一缓解，§1.3 self-preference 偏置）。
    #[serde(default)]
    pub heterogeneous_verifier: bool,
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
            dmn: DmnConfig::default(),
            model_routing: ModelRoutingConfig::default(),
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

/// V33/MVP-3 DMN 演化配置（§6.3 UCB / §6.4 回报与主动学习 / §8.12 激活门槛）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmnConfig {
    /// 回报函数权重（§6.4 默认 0.5/0.3/0.2/0.1）。
    #[serde(default)]
    pub reward_weights: crate::types::verification::RewardWeights,
    /// UCB 利用排序最小采样门槛（§6.3：n < min_samples 不参与利用排序）。
    #[serde(default)]
    pub min_samples: u64,
    /// 演化激活门槛：每层最少资产数（§8.12）。
    #[serde(default)]
    pub activation_min_assets: usize,
    /// 演化激活门槛：累积采样数（§8.12 50+ 轨迹）。
    #[serde(default)]
    pub activation_min_samples: u64,
    /// 主动学习开关（§6.4 空闲窗口；默认关闭，MVP-3 探索闭环默认不激活）。
    #[serde(default)]
    pub active_learning_enabled: bool,
    /// 每窗口探索任务限量（§6.4 护栏：每窗口限量 + 不递归）。
    #[serde(default)]
    pub active_learning_max_per_window: u32,
    /// V33/MVP-3.5 贝叶斯后验开关（§6.4.1）：true → 演化决策用后验均值/σ_beta；
    /// false → 回退频率路径（MVP-3 行为）。
    #[serde(default = "DmnConfig::default_bayesian_enabled")]
    pub bayesian_enabled: bool,
    /// 先验强度 k（§6.4.1）：α = 1 + k·confidence，β = 1 + k·(1−confidence)。
    /// k 大 → 低采样结果更贴先验（人工种子权威性高）。
    #[serde(default)]
    pub prior_strength: f64,
    /// V34/MVP-4 随机审计率（§8.22 P2 预留）：概率触发深度复查（webfetch 重放
    /// 来源 URL + LLM 语义复核）。默认 0 = 不审计；激活条件后置（MVP-4 只落字段）。
    #[serde(default)]
    pub audit_rate: f64,
}

impl DmnConfig {
    fn default_bayesian_enabled() -> bool {
        true
    }
}

impl Default for DmnConfig {
    fn default() -> Self {
        Self {
            reward_weights: crate::types::verification::RewardWeights::default(),
            min_samples: 3,
            activation_min_assets: 5,
            activation_min_samples: 50,
            active_learning_enabled: false,
            active_learning_max_per_window: 1,
            bayesian_enabled: true,
            audit_rate: 0.0,
            prior_strength: 10.0,
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
