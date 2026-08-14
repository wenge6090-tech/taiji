use serde::{Deserialize, Serialize};

use crate::types::verification::CheckStats;

/// 资产引用（V35/MVP-6：Lianshan 回传依据——编排所选资产列表，§8.21 数据流断点修复）。
/// `partition`（模型分区 model_key）后置：分区激活时扩展，serde default 兼容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetRef {
    /// 资产类型："prompt" | "verification" | "workflow" | "truth"。
    pub asset_type: String,
    /// 资产 id（文件 stem）。
    pub id: String,
}

impl AssetRef {
    pub fn new(asset_type: &str, id: &str) -> Self {
        Self {
            asset_type: asset_type.to_string(),
            id: id.to_string(),
        }
    }
}

/// V36：模型分区键——`{provider}-{model}` slug（如 `deepseek-deepseek-chat`）。
///
/// 不透明字符串（不做 `-` 拆解——模型名可含连字符）；provider/model 的解析
/// 由 [`ProviderRegistry::resolve_model`] 按候选表匹配（确定性，无歧义）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelKey(pub String);

impl ModelKey {
    /// 由 provider + model 构造 `{provider}-{model}` slug（分区目录名）。
    pub fn from_parts(provider: &str, model: &str) -> Self {
        Self(format!("{provider}-{model}"))
    }

    /// 分区目录名（`{provider}-{model}`）。
    pub fn key(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// V36：模型级 MCTS 统计行（BCP §6.4 元权重表，`model_key → StatsRow`）。
/// serde default 零迁移：缺失字段按 0 处理。Lianshan 回传更新（n++ 等），
/// ModelRouter 读取（§8.8 第 1 步）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelStatsRow {
    /// 采样次数（Zhouyi 任务数）。
    #[serde(default)]
    pub n: u64,
    /// 通过次数。
    #[serde(default)]
    pub pass_count: u64,
    /// token 成本累计（checks[].cost_tokens 首项摊派——同任务摊派值一致）。
    #[serde(default)]
    pub cost_sum: u64,
    /// 质量累计（任务级 passed 映射：PASS=1.0；pending 仅 PASS 入队 → 恒 1.0，
    /// 字段保留供未来 FAIL 入队扩展）。
    #[serde(default)]
    pub quality_sum: f64,
    /// 验证轮数累计（checks[].verify_rounds 首项摊派）。
    #[serde(default)]
    pub rounds_sum: u64,
}

impl ModelStatsRow {
    /// 通过率（n=0 → 0.0）。
    pub fn pass_rate(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.pass_count as f64 / self.n as f64
        }
    }

    /// 平均质量分（n=0 → 0.0）。
    pub fn avg_quality(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.quality_sum / self.n as f64
        }
    }

    /// 平均 token 成本（n=0 → 0.0）。
    pub fn avg_cost(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.cost_sum as f64 / self.n as f64
        }
    }

    /// 平均验证轮数（n=0 → 0.0）。
    pub fn avg_rounds(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.rounds_sum as f64 / self.n as f64
        }
    }
}

/// Specifies whether a task node's YangAgent (概率拟合·阳) operates in
/// **Orchestration** or **Execution** mode (V27 阴阳配对模式恢复).
///
/// Decided by the MetaAgent's weight update based on recursion depth rules
/// and task difficulty (BCP §8.8):
///
/// | 配对 | 阳 Agent | 阴 Agent |
/// |------|----------|----------|
/// | Orchestration（编排） | 编排模板：recursive_decompose 拆解 + 综合 | 收敛模板：子结果聚合判决（converge） |
/// | Execution（执行） | 执行模板：L1 工具直接产出 | 验证模板：直接产出核验（verify） |
///
/// Mode is **not** derived from depth alone: the MetaAgent LLM weighs depth
/// rules (leaf `depth+1 >= max_depth` is hard-forced to Execution by
/// `RecursiveDecomposeTool`) plus task difficulty. Children receive their
/// mode via `SubtaskSpec.mode` (parent LLM difficulty judgment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMode {
    /// Agent acts as a task decomposer/synthesizer — breaks complex tasks into
    /// subtasks via `recursive_decompose`, delegates, then integrates results.
    Orchestration,
    /// Agent acts as a focused executor — uses L1 skills to directly produce
    /// output; `recursive_decompose` is not registered (LLM cannot see it).
    Execution,
}

impl Default for AgentMode {
    fn default() -> Self {
        AgentMode::Orchestration
    }
}

/// 元 (Meta) 的产出——半 LLM 半符号认知节点的两个出口（BCP §8.8 V46）。
///
/// - `Context`：行动类任务（产出改变世界状态）→ 完整三相循环（阳→阴→路由）。
/// - `Answer`：应答类任务（产出不改变世界，如查询/问答/讨论）→ 短路，跳过阳阴
///   直接 PASS。验证规则：符号校验保底（引用真实性）+ 交互判断兜底（父节点/用户
///   裁定），阴不做语义验证（同源概率回路 §1.3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetaOutcome {
    Context(MetaContext),
    Answer(String),
}

/// Context produced by MetaAgent (权重更新·元), injected as reasoning bias
/// into YangAgent and YinAgent.
///
/// MetaAgent queries the 归藏 (cognitive warehouse) and LLM-decides:
/// - Cognitive context (constraints, skills)
/// - The optimal [`AgentMode`] for the task (depth rules + difficulty)
/// - Composed system prompts for downstream agents (mode-paired)
///
/// # Fallback
/// When 归藏 has no matching prompt assets, `yang_system_prompt`,
/// `verify_system_prompt`, and `converge_system_prompt` are `None`, `mode`
/// defaults to [`AgentMode::Orchestration`], and downstream agents fall back
/// to their built-in mode-paired hardcoded templates.
///
/// V27 起携带 `mode`（serde default=Orchestration，旧 meta_ctx.json 零迁移兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaContext {
    pub constraints: Vec<crate::types::verification::TruthConstraint>,
    pub matched_skills: Vec<SkillRef>,
    pub yang_prompt: YangPrompt,

    /// AgentMode decided by MetaAgent's weight update (recursion depth rules
    /// + task difficulty). Defaults to [`AgentMode::Orchestration`] when
    /// unset (degraded path).
    #[serde(default)]
    pub mode: AgentMode,

    /// V32：编排降级原因（LLM 重试 3 次仍失败时填充）。serde default 零迁移；
    /// 空 = 编排成功或未尝试。审计可见——「编排失败」与「无资产降级」不再混淆。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,

    /// V35/MVP-6：本次编排选用的资产引用列表（prompts + verifications，
    /// UCB 序消费）——Lianshan 回传依据（§8.21 数据流断点修复）。
    /// serde default：旧 meta_ctx.json 零迁移；空 = 未接线（checks-only 路径）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets_used: Vec<AssetRef>,

    /// V36：元权重模型路由结果（BCP §8.8 第 1 步，纯符号层先于分区检索）。
    /// 唯一分区载体——Yang/Yin 按此模型执行 + 按此分区加载资产（§8.3
    /// 分区一致性）；None = 路由异常/未接线 → 配置默认。serde default：旧
    /// meta_ctx.json 零迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelKey>,

    /// V37：验证相位（Yin）专用模型——异源裁判（BCP §8.8 相位级）。
    /// Some = YinAgent verify/converge 用此模型（及对应分区加载契约）执行，
    /// 与执行模型（`model`）异源——裁判 ≠ 运动员（§1.3 self-preference 偏置
    /// 的对抗）；None = 继承 `model`（主模型）。serde default：旧 meta_ctx.json
    /// 零迁移；默认关闭（`runtime.model_routing.heterogeneous_verifier`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_model: Option<ModelKey>,

    /// Full system prompt for YangAgent (概率拟合·阳), LLM-composed by
    /// MetaAgent.  When `None`, YangAgent uses its mode-paired built-in
    /// template (编排模板 / 执行模板).
    pub yang_system_prompt: Option<String>,

    /// Full system prompt for YinAgent.verify() (因果验证·阴).
    /// When `None`, YinAgent falls back to VERIFY_ORC/EXEC_SYSTEM_PROMPT
    /// by `mode`.
    pub verify_system_prompt: Option<String>,

    /// Full system prompt for YinAgent.converge() (收敛判决).
    /// When `None`, YinAgent falls back to CONVERGE_ORC/EXEC_SYSTEM_PROMPT
    /// by `mode`.
    pub converge_system_prompt: Option<String>,
}

impl MetaContext {
    /// Create an empty/degraded `MetaContext` with no cognitive context.
    ///
    /// All optional prompt fields are `None`, causing downstream agents to
    /// fall back to their built-in mode-paired templates.  Mode defaults to
    /// [`AgentMode::Orchestration`] (safe for root task).
    pub fn empty() -> Self {
        Self {
            constraints: vec![],
            matched_skills: vec![],
            yang_prompt: YangPrompt {
                task_description: String::new(),
                constraint_summaries: vec![],
                parent_deliverables: vec![],
                sibling_deliverables: vec![],
            },
            mode: AgentMode::Orchestration,
            degraded: None,
            assets_used: vec![],
            model: None,
            verify_model: None,
            yang_system_prompt: None,
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
    /// 层 0：一句话摘要（披露进 tool 列表，BCP §10.2 渐进披露；空 = 回退 id—name）。
    #[serde(default)]
    pub summary: String,
}

/// The prompt context passed to YangAgent (概率拟合·阳).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YangPrompt {
    pub task_description: String,
    pub constraint_summaries: Vec<String>,
    /// Absolute paths of parent deliverables, injected by `recursive_decompose`.
    /// Read-only reference for the child YangAgent — the child can read but
    /// cannot write to parent directories.
    #[serde(default)]
    pub parent_deliverables: Vec<String>,
    /// V30 会盟：sibling 贡品索引——同级子任务（兄弟）deliverables/ 的绝对路径
    /// 清单，由 `recursive_decompose` 分封时注入（BTreeMap 有序扫描，排除自身，
    /// spawn 时点快照）。贡品跨兄弟公开可发现可读（§8.20）；中间记忆仍隔离。
    #[serde(default)]
    pub sibling_deliverables: Vec<String>,
}

// ---------------------------------------------------------------------------
// ExternalContext — context from the calling frontend agent
// ---------------------------------------------------------------------------

/// Context passed from the calling frontend agent through MCP.
///
/// When a frontend agent (any MCP client) calls `taiji_run`, it can provide
/// files it has read, tools it has executed, and a summary of the conversation.
/// This context is injected into the Zhouyi cycle so YangAgent can reason over
/// data that the frontend already collected — avoiding redundant tool calls.
///
/// All fields are optional.  When `None`/empty, the Zhouyi cycle runs normally
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
// PromptAsset — 归藏 prompt template asset
// ---------------------------------------------------------------------------

/// A prompt template asset stored in the 归藏 cognitive warehouse under
/// `prompts/`.  MetaAgent searches these by task-type tags, ranks by
/// confidence, and LLM-composes them into the final system prompts carried
/// in [`MetaContext`].
///
/// # Directory layout
/// ```text
/// {data_dir}/prompts/
/// ├── orchestration_yang.yaml
/// ├── execution_yang.yaml
/// ├── orchestration_verify.yaml
/// └── ...
/// ```
///
/// V26 起 `agent_mode` 字段已删除（serde 默认宽容，旧归藏 YAML 中的
/// `agent_mode` 键自动忽略，零迁移兼容）。V27 模式配对恢复后，资产仍不含
/// `agent_mode` 字段——MetaAgent 按资产 name/tags/description 选择与所选
/// 模式匹配的资产（assets 命名 orchestration_*/execution_*，tags 含
/// orchestration/execution）。
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
    /// `serde(default)` — 手写资产 YAML 常省略；缺失时按空串处理。
    #[serde(default)]
    pub description: String,
    /// Tags for search (task_type, agent_role, mode, ...).
    pub tags: Vec<String>,
    /// Confidence score [0.0, 1.0] — MetaAgent uses this for ranking.
    pub confidence: f64,
    /// Version counter (auto-incremented by GuizangClient).
    pub version: u32,

    /// The prompt template body.
    pub content: String,

    /// Which agent this prompt targets: `"YangAgent"` or `"YinAgent"`.
    /// `serde(default)` — 手写/外部生成的资产 YAML 可能省略此字段（V27 资产
    /// 实测缺此字段导致整条资产加载失败 → MetaAgent 静默降级，系统性 bug）。
    #[serde(default)]
    pub agent_target: String,

    /// Usage statistics (updated by Lianshan evolver).
    #[serde(default)]
    pub usage_count: u32,
    /// Historical success rate [0.0, 1.0].
    #[serde(default)]
    pub success_rate: f64,

    /// V35/MVP-6 任务级 MCTS 统计（BCP §6.2 契约补齐——与 verifications 共用
    /// CheckStats 四维；backprop_prompts 写入，UCB 检索 n 数据源）。
    #[serde(default)]
    pub stats: CheckStats,
    /// 同源变体组 id（fork 树分组，MCTS 演化用——四算子对称，§8.21 V35）。
    #[serde(default)]
    pub variant_of: Option<String>,
    /// fork 来源（None = 根资产）。
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub env_tags: Vec<String>,
    /// 演化状态（"active" | "pruned"，对齐 verifications §6.5 先例；
    /// V35/MVP-6 prompts 四算子写入；load_all_prompts 过滤非 active）。
    #[serde(default)]
    pub status: String,
}

impl PromptAsset {
    /// 是否为 pruned（演化淘汰，保留文件供审计）。
    pub fn is_pruned(&self) -> bool {
        self.status == "pruned"
    }

    /// 标记为合并淘汰（merge 吸收统计后）。
    pub fn status_mark_merged(&mut self) {
        self.status = "pruned".into();
    }

    /// 标记为剪枝淘汰。
    pub fn status_mark_pruned(&mut self) {
        self.status = "pruned".into();
    }
}

impl PromptAsset {
    /// Create a new `PromptAsset` with default metadata.
    pub fn new(
        id: &str,
        name: &str,
        description: &str,
        content: &str,
        agent_target: &str,
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
            usage_count: 0,
            success_rate: 0.0,
            stats: CheckStats::default(),
            variant_of: None,
            parent_id: None,
            env_tags: Vec::new(),
            status: "active".into(),
        }
    }
}

/// 阴轨验证契约资产（V33 归藏本体论重构 — §6.0/§8.22）。
///
/// `content` 为契约语义描述（人读），`checks` 为结构化检查项（机器执行）。
/// 平铺字段风格对齐 [`PromptAsset`]（`asset_type` 经 `CognitiveAsset` tag 传达，
/// serde skip）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationAsset {
    /// Type discriminator — always `"verification"`.
    #[serde(skip)]
    pub asset_type: String,
    /// Cognitive layer — 0 占位（BCP §6.2 未定义 verifications 层号，
    /// V32 资产层统一修订时定义；不参与运行时行为）。
    pub layer: u32,
    /// Unique identifier (file stem).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of when this contract applies.
    #[serde(default)]
    pub description: String,
    /// Tags for search (task_type, agent_role, ...).
    pub tags: Vec<String>,
    /// Confidence score [0.0, 1.0] — initial prior（人工种子）。
    pub confidence: f64,
    /// Version counter (auto-incremented by GuizangClient).
    pub version: u32,
    /// 契约语义描述（人读）。
    #[serde(default)]
    pub content: String,
    /// 结构化检查项（V33 — §4 CheckSpec；机器执行）。
    #[serde(default)]
    pub checks: Vec<crate::types::verification::CheckSpec>,
    /// 消费方 Agent："YinAgent" 为主。
    #[serde(default)]
    pub agent_target: String,
    /// Usage statistics (updated by Lianshan evolver).
    #[serde(default)]
    pub usage_count: u32,
    /// Historical success rate [0.0, 1.0].
    #[serde(default)]
    pub success_rate: f64,
    /// 契约生命周期状态（MVP-3 演化 — 对齐 truths 的 active/retracted 先例 §6.5）：
    /// `"active"` 参与加载/回传；`"pruned"` 被 δ-prune 淘汰（不再加载/回传，保留审计）。
    #[serde(default = "VerificationAsset::default_status")]
    pub status: String,
    /// MCTS fork 树链接（§8.21）：指向被 fork 的源资产 id（根资产为 None）。
    /// 变体与源资产构成演化组，组内统计对比驱动 merge/prune。
    #[serde(default)]
    pub variant_of: Option<String>,
    /// 环境维度（V50 §6.3.1）：空 = 环境无关；模型类（flash/strong）隔离变体。
    #[serde(default)]
    pub env_tags: Vec<String>,
    /// V50 危险隔离：只有 true 的资产允许进入主动学习探索候选（默认 false）。
    #[serde(default)]
    pub safe_for_exploration: bool,
}

impl VerificationAsset {
    fn default_status() -> String {
        "active".to_string()
    }

    /// Create a new `VerificationAsset` with default metadata.
    pub fn new(
        id: &str,
        name: &str,
        description: &str,
        content: &str,
        checks: Vec<crate::types::verification::CheckSpec>,
        tags: Vec<String>,
    ) -> Self {
        Self {
            asset_type: "verification".into(),
            layer: 0,
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            tags,
            confidence: 0.5,
            version: 1,
            content: content.to_string(),
            checks,
            agent_target: "YinAgent".into(),
            usage_count: 0,
            success_rate: 0.0,
            status: Self::default_status(),
            variant_of: None,
            env_tags: Vec::new(),
            safe_for_exploration: false,
        }
    }
}
