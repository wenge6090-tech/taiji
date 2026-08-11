//! causal_verify — Verification tool for FittingAgent (概率拟合·阳).
//!
//! Delegates to `CausalAgent.verify()` (因果验证·阴) and returns the
//! `VerificationReport` to the calling LLM.  The report drives routing:
//!
//! - [`VerificationRoute::Pass`] → DMN reflection.
//! - [`VerificationRoute::BackToTpn`] → retry FittingAgent.
//! - [`VerificationRoute::BackToMeta`] → retry MetaAgent.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::agents::factory::AgentFactory;
use crate::infra::error::TaijiError;
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;

/// Arguments for the causal_verify tool.
#[derive(Debug, Deserialize)]
pub struct CausalVerifyArgs {
    /// The task output text produced by the FittingAgent.
    pub task_output: String,
    /// Raw result strings from any L1 skill tools that were called.
    #[serde(default)]
    pub tool_results: Vec<String>,
}

/// Tool that invokes CausalAgent verification on a task output.
pub struct CausalVerifyTool {
    factory: Arc<AgentFactory>,
    engine_ctx: EngineContext,
    /// Reasoning bias from MetaAgent, used for prompt selection.
    meta_ctx: MetaContext,
}

impl CausalVerifyTool {
    /// Create a new `CausalVerifyTool`.
    ///
    /// - `factory` — shared `AgentFactory` used to create the CausalAgent builder.
    /// - `engine_ctx` — execution context for traceability.
    /// - `meta_ctx` — reasoning bias from MetaAgent, for prompt selection.
    pub fn new(
        factory: Arc<AgentFactory>,
        engine_ctx: EngineContext,
        meta_ctx: MetaContext,
    ) -> Self {
        Self {
            factory,
            engine_ctx,
            meta_ctx,
        }
    }

    /// Run causal verification.
    ///
    /// 1. Creates a `CausalVerifyAgentBuilder` via the factory.
    /// 2. Calls `builder.verify(task_output, tool_results, meta_ctx)`.
    /// 3. Returns the resulting `VerificationReport`.
    pub async fn execute(
        &self,
        task_output: &str,
        tool_results: &[String],
    ) -> Result<String, TaijiError> {
        let builder = self.factory.create_causal_verify_agent(&self.engine_ctx, &self.meta_ctx)?;
        let report = builder.verify(task_output, tool_results, &self.meta_ctx).await?;
        serde_json::to_string(&report).map_err(TaijiError::Serde)
    }
}

// ── Rig Tool implementation ─────────────────────────────────────────────

impl Tool for CausalVerifyTool {
    const NAME: &'static str = "causal_verify";

    type Error = TaijiError;
    type Args = CausalVerifyArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Verify a task output against L4 Truth constraints and produce a VerificationReport. Returns a JSON-serialized VerificationReport.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_output": {
                        "type": "string",
                        "description": "The text output produced by the FittingAgent"
                    },
                    "tool_results": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Raw result strings from any L1 skill tools that were called"
                    }
                },
                "required": ["task_output"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.execute(&args.task_output, &args.tool_results).await
    }
}
