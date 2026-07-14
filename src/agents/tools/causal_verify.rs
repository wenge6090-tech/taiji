//! causal_verify — Verification tool for FittingAgent (概率拟合·阳).
//!
//! Delegates to `CausalAgent.verify()` (因果验证·阴) and returns the
//! `VerificationReport` to the calling LLM.  The report drives routing:
//!
//! - [`VerificationRoute::Pass`] → DMN reflection.
//! - [`VerificationRoute::BackToTpn`] → retry FittingAgent.
//! - [`VerificationRoute::BackToMeta`] → retry MetaAgent.

use std::sync::Arc;

use crate::agents::factory::AgentFactory;
use crate::infra::error::TaijiError;
use crate::types::execution::EngineContext;
use crate::types::verification::VerificationReport;

/// Tool that invokes CausalAgent verification on a task output.
pub struct CausalVerifyTool {
    factory: Arc<AgentFactory>,
    engine_ctx: EngineContext,
}

impl CausalVerifyTool {
    /// Create a new `CausalVerifyTool`.
    ///
    /// - `factory` — shared `AgentFactory` used to create the CausalAgent builder.
    /// - `engine_ctx` — execution context for traceability.
    pub fn new(factory: Arc<AgentFactory>, engine_ctx: EngineContext) -> Self {
        Self {
            factory,
            engine_ctx,
        }
    }

    /// Run causal verification.
    ///
    /// 1. Creates a `CausalVerifyAgentBuilder` via the factory.
    /// 2. Calls `builder.verify(task_output, tool_results)`.
    /// 3. Returns the resulting `VerificationReport`.
    ///
    /// - `task_output` — the text output produced by the FittingAgent.
    /// - `tool_results` — raw result strings from any L1 skill tools that were called.
    pub async fn execute(
        &self,
        task_output: &str,
        tool_results: &[String],
    ) -> Result<VerificationReport, TaijiError> {
        let builder = self.factory.create_causal_verify_agent(&self.engine_ctx)?;
        let report = builder.verify(task_output, tool_results).await?;
        Ok(report)
    }
}
