use thiserror::Error;

/// Unified error type for the taiji engine.
#[derive(Error, Debug)]
pub enum TaijiError {
    #[error("config error: {context}")]
    Config { context: String },

    #[error("归藏 knowledge store unavailable: {context}")]
    KnowledgeStoreUnavailable { context: String },

    #[error("knowledge asset not found: {id}")]
    /// 资产文件不存在（区别于读权限/解析错误——调用方据此区分「不存在→降级」
    /// 与「真实 I/O 错误→上抛」，替代旧的文案字符串匹配反模式）。
    KnowledgeAssetNotFound { id: String },

    #[error("LLM call failed: {context}")]
    LLMCallFailed { context: String },

    #[error("context overflow: threshold={threshold}")]
    /// V29 上下文预算超限（BCP §8.19）：单次窗口占用 input_tokens >= handoff。
    /// V48：改为单次窗口占用语义（不再跨轮累计）。
    /// 语义：任务粒度错误信号 → BACK_TO_ZHOUYI → 阳基于产出递归分解。
    ContextOverflow { threshold: u64 },

    #[error("hard cutoff: threshold={threshold}")]
    /// V29 硬截止（BCP §8.19）：单次窗口占用 input_tokens >= hard_cutoff。
    /// V48：改为单次窗口占用语义（不再跨轮累计）。
    /// 语义：预算保护 → 直接上报 FAIL，不进 BACK_TO_* 循环。
    HardCutoff { threshold: u64 },

    #[error("structured output parse failed: {context}")]
    StructuredOutputParseFailed { context: String },

    #[error("max depth ({max}) exceeded")]
    MaxDepthExceeded { max: u32 },

    #[error("max rounds ({max}) exceeded")]
    MaxRoundsExceeded { max: u32 },

    #[error("max cycles ({max}) exceeded")]
    MaxCyclesExceeded { max: u32 },

    #[error("max subtasks exceeded: max={max}, actual={actual}")]
    MaxSubtasksExceeded { max: usize, actual: usize },

    #[error("safety violation: {reason}")]
    SafetyViolation { reason: String },

    #[error("constraint violation: {context}")]
    ConstraintViolation { context: String },

    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("cancelled: {context}")]
    Cancelled { context: String },

    #[error("worker pool unavailable: {context}")]
    WorkerPoolUnavailable { context: String },

    #[error("{0}")]
    Other(String),
}
