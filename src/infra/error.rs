use thiserror::Error;

/// Unified error type for the taiji engine.
#[derive(Error, Debug)]
pub enum TaijiError {
    #[error("config error: {context}")]
    Config { context: String },

    #[error("Qdrant unavailable: {context}")]
    QdrantUnavailable { context: String },

    #[error("LLM call failed: {context}")]
    LLMCallFailed { context: String },

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

    #[error("{0}")]
    Other(String),
}
