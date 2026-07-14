//! CognitionEvolver — DMN cognitive evolution (δ₀-δ₃).
//! Called by DMN Consumer background task.
//! See AGENTS.md §6 for detailed rules.
//!
//! Operations:
//! - δ₀: Prune low-confidence nodes (confidence < threshold).
//! - δ₁: L1 skill tuning (update success/fail counts).
//! - δ₂: L2 Bayesian confidence update.
//! - δ₃: L3 grid rewiring (relation weight adjustments).
//! - evolve(): Run δ₀→δ₃ in sequence, producing an EvolutionReport.

use crate::infra::error::TaijiError;
use crate::infra::qdrant::NskgClient;
use crate::infra::trace::TraceRecord;
use qdrant_client::qdrant::{PointStruct, UpsertPointsBuilder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Adjustment to a single relation during grid rewiring (δ₃).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationAdjustment {
    /// Target cognitive asset ID.
    pub target_id: String,
    /// Type of the relation being adjusted (e.g. "causes", "inhibits").
    pub relation_type: String,
    /// Signed delta to apply to the relation weight, in range [-1.0, 1.0].
    pub delta: f64,
}

/// Aggregate report produced by a full evolution cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionReport {
    /// Number of nodes pruned below the confidence threshold.
    pub pruned: u64,
    /// Number of skills tuned.
    pub skills_tuned: u64,
    /// Number of Bayesian model updates performed.
    pub models_updated: u64,
    /// Number of grid rewiring operations applied.
    pub grids_rewired: u64,
    /// Aggregate confidence delta across all updated models.
    pub confidence_delta: f64,
}

/// DMN Cognitive Evolution Engine.
///
/// Drives the four evolution operators (δ₀–δ₃) over the NSKG.
/// Qdrant writes are logged rather than executed so the evolver
/// remains functional even when the vector store is unavailable.
pub struct CognitionEvolver {
    nskg: Arc<NskgClient>,
}

impl CognitionEvolver {
    /// Create a new evolver with a reference to the NSKG client.
    pub fn new(nskg: Arc<NskgClient>) -> Self {
        Self { nskg }
    }

    /// δ₀: Prune low-confidence cognitive assets.
    ///
    /// Logs which nodes would be removed (confidence < `threshold`)
    /// and returns the count of hypothetical pruned nodes.
    pub async fn prune_low_confidence(&self, threshold: f64) -> Result<u64, TaijiError> {
        let collection = self.nskg.collection_name();
        tracing::info!(
            collection = %collection,
            threshold = threshold,
            "[δ₀] prune_low_confidence: would remove nodes with confidence < {threshold}",
        );

        // In a production implementation this would query Qdrant for
        // points whose `confidence` payload field is below threshold,
        // then delete them in batches. Here we log and return 0.
        Ok(0)
    }

    /// δ₁: L1 skill tuning — update success/fail statistics.
    ///
    /// Logs the tuning event. Returns `Ok(())` on success.
    pub async fn tune_skill(&self, skill_id: &str, success: bool) -> Result<(), TaijiError> {
        let collection = self.nskg.collection_name();
        tracing::info!(
            collection = %collection,
            skill_id = %skill_id,
            success = success,
            "[δ₁] tune_skill: skill={skill_id} success={success}",
        );
        Ok(())
    }

    /// δ₂: L2 Bayesian confidence update.
    ///
    /// Computes `new_confidence = (1.0 + success_count) / (2.0 + success_count + fail_count)`
    /// using a Beta(1,1) prior (Laplace smoothing). Logs the result and returns it.
    pub async fn bayesian_update(
        &self,
        model_id: &str,
        success_count: u64,
        fail_count: u64,
    ) -> Result<f64, TaijiError> {
        let total = success_count as f64 + fail_count as f64;
        let new_confidence =
            (1.0 + success_count as f64) / (2.0 + total);

        let collection = self.nskg.collection_name();
        tracing::info!(
            collection = %collection,
            model_id = %model_id,
            success_count = success_count,
            fail_count = fail_count,
            new_confidence = new_confidence,
            "[δ₂] bayesian_update: model={model_id} success={success_count} fail={fail_count} → confidence={new_confidence:.4}",
        );
        Ok(new_confidence)
    }

    /// δ₃: L3 grid rewiring — adjust relation weights.
    ///
    /// Applies the given signed deltas to relations of the specified grid node.
    /// Each delta is clamped to [-1.0, 1.0] before logging.
    pub async fn grid_rewire(
        &self,
        grid_id: &str,
        relation_adjustments: &[RelationAdjustment],
    ) -> Result<(), TaijiError> {
        let collection = self.nskg.collection_name();

        for adj in relation_adjustments {
            let clamped_delta = adj.delta.clamp(-1.0, 1.0);
            tracing::info!(
                collection = %collection,
                grid_id = %grid_id,
                target_id = %adj.target_id,
                relation_type = %adj.relation_type,
                delta = clamped_delta,
                "[δ₃] grid_rewire: grid={grid_id} target={} type={} delta={clamped_delta:.4}",
                adj.target_id,
                adj.relation_type,
            );
        }

        Ok(())
    }

    /// Run a full evolution cycle: δ₀ → δ₁ → δ₂ → δ₃.
    ///
    /// * δ₀ — prunes low-confidence nodes (threshold 0.1 per AGENTS.md §6).
    /// * δ₁ — replays trace records to tune skills (no-op when `trace_records` is empty).
    /// * δ₂ — placeholder for Bayesian model updates driven by trace data.
    /// * δ₃ — placeholder for grid rewiring driven by trace data.
    ///
    /// Returns an `EvolutionReport` summarising the cycle.
    pub async fn evolve(
        &self,
        task_id: &str,
        trace_records: &[TraceRecord],
    ) -> Result<EvolutionReport, TaijiError> {
        let collection = self.nskg.collection_name();
        tracing::info!(
            collection = %collection,
            task_id = %task_id,
            trace_count = trace_records.len(),
            "[evolve] starting evolution cycle for task={task_id} with {} trace records",
            trace_records.len(),
        );

        // δ₀: Prune low-confidence nodes (threshold 0.1 per AGENTS.md §6).
        let pruned = self.prune_low_confidence(0.1).await?;

        // δ₁: Tune skills from trace records.
        // Each trace record with phase containing "工具调用" or "tool" is treated
        // as a skill invocation. In a production system the successful/failed
        // outcome would be determined by the trace's output or degraded flag.
        let mut skills_tuned = 0u64;
        for record in trace_records {
            if record.phase.contains("工具调用") || record.phase.contains("tool") {
                // Determine success from the degraded flag (false = success).
                self.tune_skill(&record.task_id, !record.degraded).await?;
                skills_tuned += 1;
            }
        }

        // δ₂: Bayesian updates from trace records (placeholder logic).
        // In production, trace records would be grouped by model_id for batch updates.
        let mut models_updated = 0u64;
        let mut confidence_delta = 0.0;
        for record in trace_records {
            if record.phase.contains("概率拟合") || record.phase.contains("fitting") {
                // Simple heuristic: treat each fitting record as one success + one fail
                // to keep the model evolving. Production code would use real counters.
                let new_conf = self
                    .bayesian_update(&record.task_id, 1, 1)
                    .await?;
                models_updated += 1;
                confidence_delta += new_conf - 0.5; // diff from uniform prior
            }
        }

        // δ₃: Grid rewiring (placeholder — no trace-driven adjustments yet).
        self.grid_rewire(task_id, &[]).await?;
        let grids_rewired = 0u64;

        let report = EvolutionReport {
            pruned,
            skills_tuned,
            models_updated,
            grids_rewired,
            confidence_delta,
        };

        tracing::info!(
            collection = %collection,
            task_id = %task_id,
            pruned = report.pruned,
            skills_tuned = report.skills_tuned,
            models_updated = report.models_updated,
            grids_rewired = report.grids_rewired,
            confidence_delta = report.confidence_delta,
            "[evolve] evolution complete: pruned={} tuned={} updated={} rewired={} Δ={:.4}",
            report.pruned,
            report.skills_tuned,
            report.models_updated,
            report.grids_rewired,
            report.confidence_delta,
        );

        // ── Write evolution result to Qdrant ──
        self.write_evolution(task_id, &report).await?;

        Ok(report)
    }

    /// Write an evolution record as a Qdrant point (version++ pattern).
    pub async fn write_evolution(
        &self,
        task_id: &str,
        report: &EvolutionReport,
    ) -> Result<(), TaijiError> {
        let payload: serde_json::Map<String, serde_json::Value> = serde_json::to_value(report)
            .map_err(|e| TaijiError::Other(e.to_string()))?
            .as_object()
            .cloned()
            .unwrap_or_default();
        let point = PointStruct::new(
            format!("evolution::{task_id}"),
            vec![0.0f32; 1536], // placeholder vector
            payload,
        );

        let upsert = UpsertPointsBuilder::new(
            self.nskg.collection_name(),
            vec![point],
        );

        let qdrant = match self.nskg.client() {
            Some(c) => c,
            None => {
                tracing::warn!(
                    "Qdrant unavailable (degraded mode), skipping evolution write for {task_id}"
                );
                return Ok(());
            }
        };

        qdrant
            .upsert_points(upsert)
            .await
            .map_err(|e| TaijiError::QdrantUnavailable {
                context: format!("failed to write evolution: {e}"),
            })?;

        tracing::info!(task_id, evolution = ?report, "Evolution written to Qdrant");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config::QdrantConfig;
    use std::sync::Arc;

    /// Helper to build a CognitionEvolver backed by a real (or mock) NskgClient.
    /// Uses a dedicated collection name to avoid colliding with production data.
    async fn test_evolver() -> CognitionEvolver {
        let config = QdrantConfig {
            url: "http://localhost:6334".to_string(),
            collection_name: "test_nskg_evolver".to_string(),
        };
        // If Qdrant is not running, NskgClient::new will fail. We wrap in an
        // option so tests can gracefully skip. For unit-test purposes we
        // tolerate the failure by constructing a minimal fallback.
        let client = NskgClient::new(&config).await.unwrap_or_else(|_| {
            // Minimal stub — NskgClient is required but not called for real I/O.
            // In a real test environment, start Qdrant or mock the client.
            panic!("Qdrant must be running on localhost:6334 for evolver tests");
        });
        CognitionEvolver::new(Arc::new(client))
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_prune_low_confidence() {
        let evolver = test_evolver().await;
        let count = evolver.prune_low_confidence(0.1).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_tune_skill() {
        let evolver = test_evolver().await;
        evolver.tune_skill("skill_test_001", true).await.unwrap();
        evolver.tune_skill("skill_test_002", false).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_bayesian_update() {
        let evolver = test_evolver().await;
        // 5 successes, 1 failure → (1+5)/(2+5+1) = 6/8 = 0.75
        let conf = evolver.bayesian_update("model_a", 5, 1).await.unwrap();
        assert!((conf - 0.75).abs() < 1e-10);

        // 0 successes, 0 failures → (1+0)/(2+0+0) = 0.5
        let conf = evolver.bayesian_update("model_b", 0, 0).await.unwrap();
        assert!((conf - 0.5).abs() < 1e-10);

        // 10 successes, 0 failures → (1+10)/(2+10+0) = 11/12 ≈ 0.9167
        let conf = evolver.bayesian_update("model_c", 10, 0).await.unwrap();
        assert!((conf - 11.0 / 12.0).abs() < 1e-10);
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_grid_rewire() {
        let evolver = test_evolver().await;
        let adjustments = vec![
            RelationAdjustment {
                target_id: "node_a".to_string(),
                relation_type: "causes".to_string(),
                delta: 0.3,
            },
            RelationAdjustment {
                target_id: "node_b".to_string(),
                relation_type: "inhibits".to_string(),
                delta: -0.2,
            },
        ];
        evolver.grid_rewire("grid_001", &adjustments).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_evolve_empty_traces() {
        let evolver = test_evolver().await;
        let report = evolver.evolve("task_empty", &[]).await.unwrap();
        assert_eq!(report.pruned, 0);
        assert_eq!(report.skills_tuned, 0);
        assert_eq!(report.models_updated, 0);
        assert_eq!(report.grids_rewired, 0);
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_evolve_with_traces() {
        let evolver = test_evolver().await;
        let traces = vec![
            TraceRecord {
                ts: "2026-01-01T00:00:00Z".to_string(),
                cycle: 1,
                depth: 0,
                task_id: "task_001".to_string(),
                phase: "工具调用".to_string(),
                provider_model: "test".to_string(),
                duration_ms: 100,
                input: serde_json::json!({}),
                output: serde_json::json!({}),
                degraded: false,
                reasoning_path_ids: None,
                constraint_violations: None,
            },
            TraceRecord {
                ts: "2026-01-01T00:00:01Z".to_string(),
                cycle: 1,
                depth: 0,
                task_id: "task_001".to_string(),
                phase: "概率拟合".to_string(),
                provider_model: "test".to_string(),
                duration_ms: 200,
                input: serde_json::json!({}),
                output: serde_json::json!({}),
                degraded: false,
                reasoning_path_ids: None,
                constraint_violations: None,
            },
        ];
        let report = evolver.evolve("task_traced", &traces).await.unwrap();
        assert_eq!(report.skills_tuned, 1);
        assert_eq!(report.models_updated, 1);
    }

    #[test]
    fn test_evolution_report_serde() {
        let report = EvolutionReport {
            pruned: 3,
            skills_tuned: 1,
            models_updated: 2,
            grids_rewired: 0,
            confidence_delta: 0.42,
        };
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: EvolutionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.pruned, 3);
        assert_eq!(deserialized.confidence_delta, 0.42);
    }

    #[test]
    fn test_relation_adjustment_serde() {
        let adj = RelationAdjustment {
            target_id: "n_001".to_string(),
            relation_type: "causes".to_string(),
            delta: 0.5,
        };
        let json = serde_json::to_string(&adj).unwrap();
        let deserialized: RelationAdjustment = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.target_id, "n_001");
        assert!((deserialized.delta - 0.5).abs() < 1e-10);
    }
}
