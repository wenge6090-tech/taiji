//! TpnCycle — Reusable TPN three-phase cycle (元·阳·阴 → loop).
//!
//! Extracted from [`RecursiveRunner`] so that both root tasks and recursive
//! child tasks share the same TPN execution logic, satisfying the **isomorphic
//! recursion** principle (§1.1 of BCP).
//!
//! # Architecture (BCP §5)
//!
//! ```text
//! MetaAgent (权重更新·元)   ─── once at entrance (or from parent MetaContext)
//!     ↓
//! TPN loop (max_cycles × max_rounds):
//!     FittingAgent (概率拟合·阳)   →  LLM exploration + tools + recursion
//!     CausalAgent  (因果验证·阴)   →  constraint check + LLM verdict
//!     ├─ PASS        → return TPNResult
//!     ├─ BACK_TO_TPN → round++ (retry FittingAgent only)
//!     └─ BACK_TO_META → cycle++, round=0 (re-run MetaAgent)
//! ```

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::types::agent::{AgentMode, MetaContext};
use crate::types::execution::EngineContext;
use crate::types::task::TPNResult;
use crate::types::verification::VerificationRoute;

/// Reusable TPN cycle that executes the three-phase loop for a task at any
/// recursion depth.
///
/// # Usage
///
/// - **Root task**: execute with `initial_meta_ctx = None`.
///   The cycle runs MetaAgent automatically to extract reasoning paths.
///
/// - **Child task**: execute with `initial_meta_ctx = Some(parent_meta_ctx)`.
///   The cycle skips the initial MetaAgent and uses the parent's context as
///   the reasoning bias.  MetaAgent is still re-run on `BACK_TO_META`.
pub struct TpnCycle {
    factory: Arc<AgentFactory>,
    config: TaijiConfig,
    /// Cancellation token propagated to all sub-agents.
    /// Root token created in [`RecursiveRunner`]; child tokens linked via
    /// `CancellationToken::child_token()` in `RecursiveDecomposeTool`.
    cancel: CancellationToken,
}

impl TpnCycle {
    /// Create a new `TpnCycle` with a cancellation token.
    pub fn new(
        factory: Arc<AgentFactory>,
        config: TaijiConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self { factory, config, cancel }
    }

    /// Execute the full TPN cycle.
    ///
    /// # Parameters
    ///
    /// - `description` — task description passed to FittingAgent.
    /// - `initial_meta_ctx` — if `Some`, skip the initial MetaAgent run.
    /// - `engine_ctx` — mutable engine context; `round`/`cycle` counters are
    ///   updated in-place for retry tracking.
    ///
    /// # Returns
    ///
    /// `Ok(TPNResult)` on PASS, else `Err(TaijiError)` on exhaustion or
    /// unrecoverable failure.
    pub async fn execute(
        &self,
        description: &str,
        initial_meta_ctx: Option<MetaContext>,
        engine_ctx: &mut EngineContext,
        mode: AgentMode,
    ) -> Result<TPNResult, TaijiError> {
        // ── Phase 1: MetaAgent (权重更新·元) ──────────────────────────
        let mut meta_ctx = match initial_meta_ctx {
            Some(ctx) => {
                tracing::debug!(
                    task_id = %engine_ctx.task_id,
                    "Using provided MetaContext — skipping MetaAgent"
                );
                ctx
            }
            None => {
                tracing::debug!(
                    task_id = %engine_ctx.task_id,
                    "Running MetaAgent for initial reasoning-path extraction"
                );
                let meta_agent = self.factory.create_meta_agent(&engine_ctx.task_id)?;
                meta_agent.run(description, &[]).await?
            }
        };

        // ── Phases 2-4: TPN loop ──────────────────────────────────────
        loop {
            // Check cancellation before each iteration
            if self.cancel.is_cancelled() {
                return Err(TaijiError::Cancelled {
                    context: format!("TPN cycle cancelled for task {}", engine_ctx.task_id),
                });
            }

            // Phase 2: FittingAgent (概率拟合·阳)
            let fitting_agent = self
                .factory
                .create_fitting_agent(engine_ctx.depth, mode, &meta_ctx, engine_ctx, self.cancel.clone())?;
            let result = fitting_agent.run(description).await?;

            // Phase 3: CausalVerify (因果验证·阴)
            let verify_agent = self.factory.create_causal_verify_agent(engine_ctx)?;
            let report = verify_agent.verify(&result.content, &[], &meta_ctx, mode).await?;

            // Phase 4: Route decision
            match report.route {
                VerificationRoute::Pass => {
                    tracing::info!(
                        task_id = %engine_ctx.task_id,
                        round = engine_ctx.round,
                        cycle = engine_ctx.cycle,
                        "TPN cycle — PASS"
                    );
                    return Ok(result);
                }
                VerificationRoute::BackToTpn => {
                    engine_ctx.round += 1;
                    if engine_ctx.round > self.config.runtime.max_rounds {
                        return Err(TaijiError::MaxRoundsExceeded {
                            max: self.config.runtime.max_rounds,
                        });
                    }
                    tracing::warn!(
                        round = engine_ctx.round,
                        task_id = %engine_ctx.task_id,
                        "BACK_TO_TPN — retrying FittingAgent (MetaAgent skipped)"
                    );
                    // Continue FittingAgent only — MetaAgent is NOT re-run.
                    continue;
                }
                VerificationRoute::BackToMeta => {
                    engine_ctx.cycle += 1;
                    engine_ctx.round = 0;
                    if engine_ctx.cycle > self.config.runtime.max_cycles {
                        return Err(TaijiError::MaxCyclesExceeded {
                            max: self.config.runtime.max_cycles,
                        });
                    }
                    tracing::warn!(
                        cycle = engine_ctx.cycle,
                        task_id = %engine_ctx.task_id,
                        "BACK_TO_META — re-running MetaAgent for fresh reasoning paths"
                    );
                    // Re-run MetaAgent to obtain a new reasoning bias.
                    let meta_agent = self.factory.create_meta_agent(&engine_ctx.task_id)?;
                    meta_ctx = meta_agent.run(description, &[]).await?;
                    continue;
                }
            }
        }
    }
}
