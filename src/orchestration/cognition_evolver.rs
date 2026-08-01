//! CognitionEvolver — DMN cognitive evolution (δ₀-δ₂).
//! Called by DMN Consumer background task.
//! See AGENTS.md §6 for detailed rules.
//!
//! Operations:
//! - δ₀: Prune low-confidence nodes (confidence < threshold).
//! - δ₁: L1 skill tuning (update success/fail counts).
//! - δ₂: L2 Bayesian confidence update (预留).
//! - evolve(): Run δ₀→δ₂ in sequence, producing an EvolutionReport.
//!
//! # 归藏 integration
//! Evolution results are written back to the 归藏 knowledge store as
//! metadata placeholders (V22: grids/ removed — no asset is persisted).

use crate::infra::error::TaijiError;
use crate::infra::knowledge::LiluoClient;
use crate::infra::trace::TraceRecord;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Aggregate report produced by a full evolution cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionReport {
    /// Number of nodes pruned below the confidence threshold.
    pub pruned: u64,
    /// Number of skills tuned.
    pub skills_tuned: u64,
    /// Number of Bayesian model updates performed.
    pub models_updated: u64,
    /// Number of grid rewiring operations applied (V22: always 0).
    pub grids_rewired: u64,
    /// Aggregate confidence delta across all updated models.
    pub confidence_delta: f64,
}

/// DMN Cognitive Evolution Engine.
///
/// Drives the evolution operators (δ₀–δ₂) over the 归藏 knowledge store.
pub struct CognitionEvolver {
    liluo: Arc<LiluoClient>,
}

impl CognitionEvolver {
    /// Create a new evolver with a reference to the 归藏 client.
    pub fn new(liluo: Arc<LiluoClient>) -> Self {
        Self { liluo }
    }

    /// δ₀: Prune low-confidence cognitive assets.
    ///
    /// Logs which nodes would be removed (confidence < `threshold`)
    /// and returns the count of hypothetical pruned nodes.
    pub async fn prune_low_confidence(&self, threshold: f64) -> Result<u64, TaijiError> {
        tracing::info!(
            knowledge_dir = %self.liluo.knowledge_dir().display(),
            threshold = threshold,
            "[δ₀] prune_low_confidence: would remove nodes with confidence < {threshold}",
        );

        // In a production implementation this would query the 归藏 via
        // `search_by_tags()` or directory scan, then delete assets whose
        // `confidence` is below threshold. Here we log and return 0.
        Ok(0)
    }

    /// δ₁: L1 skill tuning — update success/fail statistics.
    ///
    /// Logs the tuning event. Returns `Ok(())` on success.
    pub async fn tune_skill(&self, skill_id: &str, success: bool) -> Result<(), TaijiError> {
        tracing::info!(
            knowledge_dir = %self.liluo.knowledge_dir().display(),
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

        tracing::info!(
            knowledge_dir = %self.liluo.knowledge_dir().display(),
            model_id = %model_id,
            success_count = success_count,
            fail_count = fail_count,
            new_confidence = new_confidence,
            "[δ₂] bayesian_update: model={model_id} success={success_count} fail={fail_count} → confidence={new_confidence:.4}",
        );
        Ok(new_confidence)
    }

    /// Run a full evolution cycle: δ₀ → δ₁ → δ₂.
    ///
    /// * δ₀ — prunes low-confidence nodes (threshold 0.1 per AGENTS.md §6).
    /// * δ₁ — replays trace records to tune skills (no-op when `trace_records` is empty).
    /// * δ₂ — placeholder for Bayesian model updates driven by trace data.
    ///
    /// Returns an `EvolutionReport` summarising the cycle.
    pub async fn evolve(
        &self,
        task_id: &str,
        trace_records: &[TraceRecord],
    ) -> Result<EvolutionReport, TaijiError> {
        tracing::info!(
            knowledge_dir = %self.liluo.knowledge_dir().display(),
            task_id = %task_id,
            trace_count = trace_records.len(),
            "[evolve] starting evolution cycle for task={task_id} with {} trace records",
            trace_records.len(),
        );

        // δ₀: Prune low-confidence nodes (threshold 0.1 per AGENTS.md §6).
        let pruned = self.prune_low_confidence(0.1).await?;

        // δ₁: Tune skills from trace records.
        let mut skills_tuned = 0u64;
        for record in trace_records {
            if record.phase.contains("工具调用") || record.phase.contains("tool") {
                self.tune_skill(&record.task_id, !record.degraded).await?;
                skills_tuned += 1;
            }
        }

        // δ₂: Bayesian updates from trace records (placeholder logic).
        let mut models_updated = 0u64;
        let mut confidence_delta = 0.0;
        for record in trace_records {
            if record.phase.contains("概率拟合") || record.phase.contains("fitting") {
                let new_conf = self
                    .bayesian_update(&record.task_id, 1, 1)
                    .await?;
                models_updated += 1;
                confidence_delta += new_conf - 0.5;
            }
        }

        // δ₃ removed (V22): grid rewiring deleted — grids_rewired always 0.
        let grids_rewired = 0u64;

        let report = EvolutionReport {
            pruned,
            skills_tuned,
            models_updated,
            grids_rewired,
            confidence_delta,
        };

        tracing::info!(
            knowledge_dir = %self.liluo.knowledge_dir().display(),
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

        // ── Write evolution result to 理络 ──
        self.write_evolution(task_id, &report).await?;

        Ok(report)
    }

    /// Write an evolution record back to the 归藏 knowledge store.
    ///
    /// V22: the `grids/` layer and GridAsset were removed — evolution results
    /// are no longer persisted as cognitive assets. This is a log-only
    /// placeholder (the report is already emitted via tracing in `evolve`).
    /// Future layers (e.g. `prompts/` metadata) may consume this hook.
    pub async fn write_evolution(
        &self,
        task_id: &str,
        report: &EvolutionReport,
    ) -> Result<(), TaijiError> {
        tracing::info!(
            task_id,
            pruned = report.pruned,
            skills_tuned = report.skills_tuned,
            models_updated = report.models_updated,
            grids_rewired = report.grids_rewired,
            confidence_delta = report.confidence_delta,
            "Evolution record for task={task_id} (V22: no asset persisted — grids/ removed)",
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use crate::infra::knowledge::LiluoClient;

    /// Create a unique temporary directory for test isolation.
    async fn test_knowledge_dir() -> std::path::PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("taiji_evolver_test_{ts}"))
    }

    /// Helper to build a CognitionEvolver backed by a file-system LiluoClient.
    async fn test_evolver() -> (CognitionEvolver, std::path::PathBuf) {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(
            LiluoClient::new(&dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let evolver = CognitionEvolver::new(client);
        (evolver, dir)
    }

    #[tokio::test]
    async fn test_prune_low_confidence() {
        let (evolver, dir) = test_evolver().await;
        let count = evolver.prune_low_confidence(0.1).await.unwrap();
        assert_eq!(count, 0);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_tune_skill() {
        let (evolver, dir) = test_evolver().await;
        evolver.tune_skill("skill_test_001", true).await.unwrap();
        evolver.tune_skill("skill_test_002", false).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_bayesian_update() {
        let (evolver, dir) = test_evolver().await;
        // 5 successes, 1 failure → (1+5)/(2+5+1) = 6/8 = 0.75
        let conf = evolver.bayesian_update("model_a", 5, 1).await.unwrap();
        assert!((conf - 0.75).abs() < 1e-10);

        // 0 successes, 0 failures → (1+0)/(2+0+0) = 0.5
        let conf = evolver.bayesian_update("model_b", 0, 0).await.unwrap();
        assert!((conf - 0.5).abs() < 1e-10);

        // 10 successes, 0 failures → (1+10)/(2+10+0) = 11/12 ≈ 0.9167
        let conf = evolver.bayesian_update("model_c", 10, 0).await.unwrap();
        assert!((conf - 11.0 / 12.0).abs() < 1e-10);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_evolve_empty_traces() {
        let (evolver, dir) = test_evolver().await;
        let report = evolver.evolve("task_empty", &[]).await.unwrap();
        assert_eq!(report.pruned, 0);
        assert_eq!(report.skills_tuned, 0);
        assert_eq!(report.models_updated, 0);
        assert_eq!(report.grids_rewired, 0);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_evolve_with_traces() {
        let (evolver, dir) = test_evolver().await;
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
                constraint_violations: None,
            },
        ];
        let report = evolver.evolve("task_traced", &traces).await.unwrap();
        assert_eq!(report.skills_tuned, 1);
        assert_eq!(report.models_updated, 1);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_evolve_no_persisted_asset() {
        let (evolver, dir) = test_evolver().await;
        let _report = evolver.evolve("write_test", &[]).await.unwrap();

        // V22: grids/ removed — evolution must NOT persist any asset.
        let mut dir_exists = tokio::fs::read_dir(&dir).await.unwrap();
        let mut assets: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = dir_exists.next_entry().await {
            assets.push(entry.file_name().to_string_lossy().to_string());
        }
        assert!(
            assets.iter().all(|n| {
                n == "truths"
                    || n == "models"
                    || n == "skills"
                    || n == "prompts"
                    || n == "index.yaml"
            }),
            "unexpected files written during evolve: {assets:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
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
}
