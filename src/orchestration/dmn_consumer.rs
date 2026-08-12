//! DMN Consumer — background task polling pending/ queue.
//! tokio::spawn with CancellationToken, exponential backoff.
//! See AGENTS.md §6 for detailed rules.
//!
//! The consumer periodically scans `{data_root}/pending/` for `.json` task files,
//! feeds them to the CognitionEvolver, and moves processed/dead files accordingly.
//! Backoff starts at 1 s and caps at 60 s (1, 2, 4, 8, 16, 32, 60, 60, …).

use crate::infra::config::DmnConfig;
use crate::infra::error::TaijiError;
use crate::orchestration::cognition_evolver::CognitionEvolver;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio_util::sync::CancellationToken;

/// Exponential backoff parameters.
const BACKOFF_INITIAL_MS: u64 = 1_000;   // 1 s
const BACKOFF_CAP_MS: u64 = 60_000;      // 60 s

/// DMN Consumer — background task that polls the pending/ queue.
pub struct DmnConsumer {
    /// The cognition evolver that processes each pending task.
    evolver: Arc<CognitionEvolver>,
    /// Cancellation token to signal graceful shutdown.
    cancel: CancellationToken,
    /// Root directory under which `pending/` and `pending/dead/` live.
    data_root: PathBuf,
    /// V33/MVP-3: DMN 演化配置（回报权重 / 门槛 / 主动学习开关）。
    dmn_config: DmnConfig,
}

impl DmnConsumer {
    /// Create a new DMN Consumer.
    ///
    /// * `evolver` — shared cognition evolver for δ₀–δ₃ operations.
    /// * `cancel` — cancellation token; the loop exits when this is signalled.
    /// * `data_root` — root directory containing the `pending/` subdirectory.
    /// * `dmn_config` — V33/MVP-3 演化配置（§6.3/§6.4/§8.12）。
    pub fn new(
        evolver: Arc<CognitionEvolver>,
        cancel: CancellationToken,
        data_root: &Path,
        dmn_config: DmnConfig,
    ) -> Self {
        Self {
            evolver,
            cancel,
            data_root: data_root.to_path_buf(),
            dmn_config,
        }
    }

    /// Spawn the consumer loop as a background tokio task.
    ///
    /// Returns a `JoinHandle` that can be awaited or detached.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.run())
    }

    /// Main consumer loop.
    ///
    /// 1. Check cancellation — return immediately if signalled.
    /// 2. Scan `{data_root}/pending/` for `.json` files.
    /// 3. If no files found: exponential backoff sleep (capped at 60 s).
    /// 4. If files found: reset backoff; process each file:
    ///    - Read & parse as JSON.
    ///    - Call `evolver.evolve(task_id, &[])`.
    ///    - On success: delete the file.
    ///    - On error: move the file to `pending/dead/` (creating the dir as needed).
    /// 5. Loop back to step 1.
    async fn run(self) {
        let pending_dir = self.data_root.join("pending");
        let dead_dir = pending_dir.join("dead");

        let mut backoff_ms = BACKOFF_INITIAL_MS;

        loop {
            // ── Check cancellation ──────────────────────────────────────
            if self.cancel.is_cancelled() {
                tracing::info!(
                    data_root = %self.data_root.display(),
                    "[DMN Consumer] received cancellation signal, exiting",
                );
                return;
            }

            // ── Scan pending directory ──────────────────────────────────
            let entries = match fs::read_dir(&pending_dir).await {
                Ok(rd) => rd,
                Err(e) => {
                    tracing::error!(
                        data_root = %self.data_root.display(),
                        error = %e,
                        "[DMN Consumer] failed to read pending directory: {e}",
                    );
                    Self::backoff_sleep(&self.cancel, backoff_ms).await;
                    backoff_ms = (backoff_ms * 2).min(BACKOFF_CAP_MS);
                    continue;
                }
            };

            let mut files: Vec<PathBuf> = Vec::new();

            // Collect entries, handling readdir errors per-entry.
            {
                let mut entries = entries;
                while let Some(entry) = entries.next_entry().await.transpose() {
                    match entry {
                        Ok(e) => {
                            let path = e.path();
                            if path.extension().is_some_and(|ext| ext == "json") {
                                files.push(path);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "[DMN Consumer] error reading directory entry: {e}",
                            );
                        }
                    }
                }
            }

            // ── No files → backoff ──────────────────────────────────────
            if files.is_empty() {
                tracing::debug!(
                    "[DMN Consumer] no pending files, sleeping {} ms",
                    backoff_ms,
                );
                Self::backoff_sleep(&self.cancel, backoff_ms).await;
                backoff_ms = (backoff_ms * 2).min(BACKOFF_CAP_MS);
                continue;
            }

            // ── Files found → reset backoff, process each ───────────────
            backoff_ms = BACKOFF_INITIAL_MS;
            tracing::info!(
                count = files.len(),
                "[DMN Consumer] processing {} pending file(s)",
                files.len(),
            );

            for file_path in &files {
                // Exit early if cancelled.
                if self.cancel.is_cancelled() {
                    tracing::info!(
                        "[DMN Consumer] cancellation during file processing, stopping",
                    );
                    return;
                }

                let file_name = file_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                tracing::info!(
                    file = %file_name,
                    "[DMN Consumer] processing file: {file_name}",
                );

                // Read and parse the file.
                let content = match fs::read_to_string(file_path).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(
                            file = %file_name,
                            error = %e,
                            "[DMN Consumer] failed to read {file_name}: {e}",
                        );
                        move_to_dead(file_path, &dead_dir, &file_name, &e.to_string()).await;
                        continue;
                    }
                };

                let value: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(
                            file = %file_name,
                            error = %e,
                            "[DMN Consumer] failed to parse {file_name}: {e}",
                        );
                        move_to_dead(file_path, &dead_dir, &file_name, &e.to_string()).await;
                        continue;
                    }
                };

                // Extract task_id from the JSON payload.
                let task_id = value
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&file_name);

                // ── V33/MVP-2 分发：pending 携带 checks → backprop 检查项统计；
                //    无 checks → 既有 evolve（δ₀-δ₂ 占位路径，MVP-3 统一重构）──
                // backprop 单次执行（不重试）：统计累加不可重复，失败进死信供人工诊断。
                // V35/MVP-6: assets_used → backprop_prompts（任务级信号，§8.21）——
                // 与 checks 同 pending 负载，同一单次不重试语义（统计不可重复累加）。
                let is_backprop = value.get("checks").is_some();
                let mut evolve_result: Result<
                    crate::orchestration::cognition_evolver::EvolutionReport,
                    TaijiError,
                > = if is_backprop {
                    match serde_json::from_value::<Vec<crate::types::verification::CheckResult>>(
                        value["checks"].clone(),
                    ) {
                        Ok(checks) => {
                            // V44 去分区化（§10.1）：pending 携带 model_key 仅作统计键
                            // ——回传与演化统一落根级资产树；None = 未指定模型。
                            let model_key = value.get("model_key").and_then(|v| v.as_str());
                            // V35/MVP-6: prompts 任务级回传（assets_used + passed；
                            // 失败仅 warn 不阻断 checks 路径——prompts 回传是增强层）
                            let assets_used: Vec<crate::types::agent::AssetRef> = value
                                .get("assets_used")
                                .and_then(|v| serde_json::from_value(v.clone()).ok())
                                .unwrap_or_default();
                            let passed = value
                                .get("passed")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(true);
                            if !assets_used.is_empty() {
                                match self
                                    .evolver
                                    .backprop_prompts(
                                        &task_id,
                                        &assets_used,
                                        passed,
                                        &checks,
                                        &self.dmn_config,
                                        model_key,
                                    )
                                    .await
                                {
                                    Ok(_) => {}
                                    Err(e) => {
                                        tracing::warn!(
                                            task_id = %task_id,
                                            error = %e,
                                            "[dmn_consumer] backprop_prompts failed — checks path continues"
                                        );
                                    }
                                }
                            }
                            match self
                                .evolver
                                .backprop_checks(&task_id, &checks, &self.dmn_config, model_key)
                                .await
                            {
                                Ok(_updated) => {
                                    // ── V36: model_stats 元权重表回传（BCP §6.4——路由数据源；
                                    // V44：model_key 仅作统计键，表存根级）。
                                    // checks 首项聚合（同任务摊派值一致）；失败仅 warn（增强层，
                                    // 不阻断 backprop 主流程——频率统计已持久化）。
                                    if let Some(key) = model_key {
                                        let first = checks.first();
                                        let signal =
                                            crate::orchestration::model_router::ModelStatsSignal {
                                                passed,
                                                cost_tokens: first.map(|c| c.cost_tokens).unwrap_or(0),
                                                verify_rounds: first.map(|c| c.verify_rounds).unwrap_or(0),
                                                quality: first.map(|c| c.quality).unwrap_or(0.0),
                                            };
                                        if let Err(e) = crate::orchestration::model_router::update_model_stats(
                                            &self.evolver.liluo(),
                                            &crate::types::agent::ModelKey(key.to_string()),
                                            &signal,
                                        )
                                        .await
                                        {
                                            tracing::warn!(
                                                task_id = %task_id,
                                                model_key = %key,
                                                error = %e,
                                                "[dmn_consumer] model_stats update failed — checks path continues"
                                            );
                                        }
                                    }
                                    // ── V33/MVP-3: backprop 后尝试契约演化（单次、激活门槛内）──
                                    // 演化失败（I/O）→ 错误上抛 → pending 进死信：backprop 已成功
                                    // 统计已回传，死信移动防止重复 backprop（幂等性保持）。
                                    self.evolver
                                        .evolve_contracts(&self.dmn_config, model_key)
                                        .await
                                }
                                Err(e) => Err(e),
                            }
                        }
                        Err(e) => Err(TaijiError::Other(format!(
                            "pending checks parse failed: {e}"
                        ))),
                    }
                } else {
                    Err(TaijiError::Other("not started".into()))
                };

                // Evolve with retry (3 attempts for transient errors) —
                // backprop 路径不重试（统计不可重复累加）。
                const MAX_EVOLVE_RETRIES: u32 = 3;
                for attempt in 1..=MAX_EVOLVE_RETRIES {
                    if evolve_result.is_ok() || is_backprop {
                        break;
                    }
                    if attempt > 1 {
                        let delay_ms = 500 * attempt; // 1s, 1.5s
                        Self::backoff_sleep(&self.cancel, delay_ms as u64).await;
                    }
                    evolve_result = self.evolver.evolve(&task_id, &[]).await;
                    if evolve_result.is_ok() {
                        break;
                    }
                    tracing::warn!(
                        file = %file_name,
                        task_id = %task_id,
                        attempt,
                        error = ?evolve_result.as_ref().unwrap_err(),
                        "[DMN Consumer] evolve attempt {attempt}/{MAX_EVOLVE_RETRIES} failed",
                    );
                }

                match evolve_result {
                    Ok(report) => {
                        tracing::info!(
                            file = %file_name,
                            task_id = %task_id,
                            pruned = report.pruned,
                            skills_tuned = report.skills_tuned,
                            models_updated = report.models_updated,
                            grids_rewired = report.grids_rewired,
                            confidence_delta = report.confidence_delta,
                            "[DMN Consumer] successfully evolved task={task_id}: \
                             pruned={} tuned={} updated={} rewired={} Δ={:.4}",
                            report.pruned,
                            report.skills_tuned,
                            report.models_updated,
                            report.grids_rewired,
                            report.confidence_delta,
                        );

                        // Remove the processed file.
                        if let Err(e) = fs::remove_file(file_path).await {
                            tracing::error!(
                                file = %file_name,
                                error = %e,
                                "[DMN Consumer] failed to remove processed file {file_name}: {e}",
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            file = %file_name,
                            task_id = %task_id,
                            error = %e,
                            "[DMN Consumer] evolution failed for task={task_id} after {MAX_EVOLVE_RETRIES} attempts: {e}",
                        );
                        move_to_dead(file_path, &dead_dir, &file_name, &e.to_string()).await;
                    }
                }
            }
        }
    }

    /// Sleep for `duration_ms` milliseconds, waking early if cancelled.
    async fn backoff_sleep(cancel: &CancellationToken, duration_ms: u64) {
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(duration_ms)) => {}
            _ = cancel.cancelled() => {}
        }
    }
}

/// Move a failed file to the dead-letter directory.
///
/// Creates the dead directory if it does not exist.
/// Appends a `.error` extension with a sanitised error fragment
/// so operators can diagnose the failure at a glance.
async fn move_to_dead(
    file_path: &Path,
    dead_dir: &Path,
    file_name: &str,
    error: &str,
) {
    // Ensure dead directory exists.
    if let Err(e) = fs::create_dir_all(dead_dir).await {
        tracing::error!(
            dead_dir = %dead_dir.display(),
            error = %e,
            "[DMN Consumer] failed to create dead-letter directory: {e}",
        );
        return;
    }

    // Build a dead-letter path: original name with an error suffix.
    let error_slug = error
        .chars()
        .take(48)
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>();
    let dead_name = if error_slug.is_empty() {
        format!("{file_name}.error")
    } else {
        format!("{file_name}.{error_slug}.error")
    };
    let dead_path = dead_dir.join(&dead_name);

    match fs::rename(file_path, &dead_path).await {
        Ok(_) => {
            tracing::info!(
                file = %file_name,
                dead_path = %dead_path.display(),
                "[DMN Consumer] moved failed file to dead-letter: {}",
                dead_path.display(),
            );
        }
        Err(e) => {
            tracing::error!(
                file = %file_name,
                dead_path = %dead_path.display(),
                error = %e,
                "[DMN Consumer] failed to move file to dead-letter: {e}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::knowledge::LiluoClient;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create a unique temporary directory for test isolation.
    async fn create_test_root(name: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("taiji_dmn_test_{name}_{ts}"));
        fs::create_dir_all(root.join("pending")).await.unwrap();
        root
    }

    /// Build a DmnConsumer backed by a file-system LiluoClient.
    async fn test_consumer(data_root: &Path) -> (DmnConsumer, PathBuf) {
        let knowledge_dir = std::env::temp_dir().join(format!(
            "taiji_dmn_knowledge_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let client = Arc::new(
            LiluoClient::new(&knowledge_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let evolver = Arc::new(CognitionEvolver::new(client));
        let consumer = DmnConsumer::new(evolver, CancellationToken::new(), data_root, DmnConfig::default());
        (consumer, knowledge_dir)
    }

    /// Clean up a test root directory.
    async fn cleanup(root: &Path) {
        let _ = fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn test_spawn_and_cancel() {
        let data_root = create_test_root("spawn_cancel").await;
        let (consumer, knowledge_dir) = test_consumer(&data_root).await;
        let handle = consumer.spawn();

        // Give the loop time to spin once, then cancel.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // The consumer was created with its own CancellationToken.
        // We create a new token to cancel it from outside.
        // Actually, we need to drop the consumer or signal its token.
        // For this test we just drop the handle after a timeout.
        // The consumer loops forever so we use timeout.
        let result = tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            handle,
        ).await;

        // The handle should still be running (timeout), or done if cancelled.
        // Since we didn't cancel the internal token, it runs forever.
        // We just verify it started without panicking.
        assert!(result.is_err(), "consumer should run indefinitely until cancelled");

        // Manually join to clean up
        // The consumer will run until the process exits; we just clean up the dirs.
        cleanup(&data_root).await;
        cleanup(&knowledge_dir).await;
    }

    #[tokio::test]
    async fn test_spawn_with_cancellation() {
        let data_root = create_test_root("spawn_cancel2").await;
        let cancel = CancellationToken::new();
        let (consumer, knowledge_dir) = test_consumer(&data_root).await;

        // Recreate consumer with the external cancellation token.
        let evolver = consumer.evolver; // reuse the evolver
        let consumer = DmnConsumer::new(evolver, cancel.clone(), &data_root, DmnConfig::default());
        let handle = consumer.spawn();

        // Give the loop time to spin once.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        cancel.cancel();

        // The task should finish promptly.
        tokio::time::timeout(tokio::time::Duration::from_secs(5), handle)
            .await
            .expect("consumer did not shut down within 5 s")
            .expect("consumer task panicked");

        cleanup(&data_root).await;
        cleanup(&knowledge_dir).await;
    }

    #[tokio::test]
    async fn test_process_valid_file() {
        let data_root = create_test_root("valid_file").await;
        let pending = data_root.join("pending");
        let cancel = CancellationToken::new();

        // Write a valid JSON task file.
        let task_file = pending.join("task_abc123.json");
        let payload = serde_json::json!({
            "task_id": "task_abc123",
            "description": "Test DMN evolution",
        });
        fs::write(&task_file, serde_json::to_string_pretty(&payload).unwrap())
            .await
            .unwrap();

        let knowledge_dir = std::env::temp_dir().join(format!(
            "taiji_dmn_knowledge_vf_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let client = Arc::new(
            LiluoClient::new(&knowledge_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let evolver = Arc::new(CognitionEvolver::new(client));
        let consumer = DmnConsumer::new(evolver, cancel.clone(), &data_root, DmnConfig::default());
        let handle = consumer.spawn();

        // Allow one processing cycle.
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
        cancel.cancel();
        tokio::time::timeout(tokio::time::Duration::from_secs(5), handle)
            .await
            .expect("consumer did not shut down")
            .expect("consumer panicked");

        // The file should have been deleted (processed successfully).
        assert!(!task_file.exists(), "expected processed file to be deleted");

        cleanup(&data_root).await;
        cleanup(&knowledge_dir).await;
    }

    #[tokio::test]
    async fn test_backprop_updates_model_stats_in_root() {
        // V36 元权重表回传（BCP §6.4）：pending 带 model_key + checks →
        // backprop 后根级 model_stats.yaml 出现该 model_key 行（首项四维聚合）。
        let data_root = create_test_root("model_stats_backprop").await;
        let pending = data_root.join("pending");
        let cancel = CancellationToken::new();

        let task_file = pending.join("task_stats.json");
        let checks = serde_json::json!([{
            "check_id": "c1", "kind": "file_exists", "passed": true,
            "detail": "ok", "duration_ms": 1, "cost_tokens": 100,
            "verify_rounds": 2, "quality": 1.0
        }]);
        let payload = serde_json::json!({
            "task_id": "task_stats",
            "source": "tpn",
            "checks": checks,
            "assets_used": [],
            "passed": true,
            "model_key": "deepseek-deepseek-v4-flash",
        });
        fs::write(&task_file, serde_json::to_string(&payload).unwrap())
            .await
            .unwrap();

        let knowledge_dir = std::env::temp_dir().join(format!(
            "taiji_dmn_knowledge_ms_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let client = Arc::new(
            LiluoClient::new(&knowledge_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let evolver = Arc::new(CognitionEvolver::new(client.clone()));
        let consumer =
            DmnConsumer::new(evolver, cancel.clone(), &data_root, DmnConfig::default());
        let handle = consumer.spawn();

        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
        cancel.cancel();
        tokio::time::timeout(tokio::time::Duration::from_secs(5), handle)
            .await
            .expect("consumer did not shut down")
            .expect("consumer panicked");

        // model_stats.yaml 根级生成，含路由 model_key 行
        assert!(!task_file.exists(), "expected processed file to be deleted");
        let stats = client.load_model_stats().await.unwrap();
        let row = stats.get("deepseek-deepseek-v4-flash").expect("model stats row");
        assert_eq!(row.n, 1);
        assert_eq!(row.pass_count, 1);
        assert_eq!(row.cost_sum, 100);
        assert_eq!(row.rounds_sum, 2);

        cleanup(&data_root).await;
        cleanup(&knowledge_dir).await;
    }

    #[tokio::test]
    async fn test_process_invalid_file_moves_to_dead() {
        let data_root = create_test_root("invalid_file").await;
        let pending = data_root.join("pending");
        let cancel = CancellationToken::new();

        // Write a corrupt JSON file.
        let task_file = pending.join("corrupt.json");
        fs::write(&task_file, b"not valid json").await.unwrap();

        let knowledge_dir = std::env::temp_dir().join(format!(
            "taiji_dmn_knowledge_if_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let client = Arc::new(
            LiluoClient::new(&knowledge_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let evolver = Arc::new(CognitionEvolver::new(client));
        let consumer = DmnConsumer::new(evolver, cancel.clone(), &data_root, DmnConfig::default());
        let handle = consumer.spawn();

        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
        cancel.cancel();
        tokio::time::timeout(tokio::time::Duration::from_secs(5), handle)
            .await
            .expect("consumer did not shut down")
            .expect("consumer panicked");

        // The original file should no longer be in pending/.
        assert!(!task_file.exists(), "expected corrupt file to be moved");

        // A dead-letter file should exist under pending/dead/.
        let dead_dir = data_root.join("pending").join("dead");
        let mut dead_entries = fs::read_dir(&dead_dir).await.unwrap();
        let dead_file = dead_entries.next_entry().await.unwrap();
        assert!(dead_file.is_some(), "expected a dead-letter file");
        let path = dead_file.unwrap().path();
        assert!(
            path.to_string_lossy().contains("corrupt"),
            "dead-letter filename should reference original file"
        );

        cleanup(&data_root).await;
        cleanup(&knowledge_dir).await;
    }

    // ── V33/MVP-2: checks 格式 pending 处理（backprop 闭环）──

    #[tokio::test]
    async fn test_process_checks_pending_backprops_stats() {
        use crate::types::agent::VerificationAsset;
        use crate::types::verification::{
            CheckKind, CheckSeverity, CheckSpec,
        };

        let data_root = create_test_root("checks_backprop").await;
        let pending = data_root.join("pending");

        // 知识库种一棵契约资产（与 consumer 共享同一 client）
        let knowledge_dir = std::env::temp_dir().join(format!(
            "taiji_dmn_knowledge_cb_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let client = Arc::new(
            LiluoClient::new(&knowledge_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let mut v = VerificationAsset::new(
            "v-closed-loop",
            "闭环契约",
            "test",
            "contract",
            vec![CheckSpec {
                id: "check-loop".into(),
                kind: CheckKind::FileExists,
                target: "deliverables/out.md".into(),
                params: serde_json::json!({}),
                severity: CheckSeverity::Hard,
                pass_condition: "p".into(),
                stats: Default::default(),
            }],
            vec!["general".into()],
        );
        client.save_verification(&mut v).await.unwrap();

        let evolver = Arc::new(CognitionEvolver::new(client.clone()));
        let cancel = CancellationToken::new();
        let consumer = DmnConsumer::new(evolver, cancel.clone(), &data_root, DmnConfig::default());
        let handle = consumer.spawn();

        // 写 checks 格式 pending（模拟 TPN PASS 入队）
        let task_file = pending.join("task_loop_1.json");
        let payload = serde_json::json!({
            "task_id": "task_loop_1",
            "source": "tpn",
            "checks": [
                {
                    "check_id": "check-loop",
                    "kind": "file_exists",
                    "passed": true,
                    "detail": "ok",
                    "duration_ms": 1,
                }
            ],
        });
        fs::write(&task_file, serde_json::to_string(&payload).unwrap())
            .await
            .unwrap();

        // 等待消费者处理（1s 首扫 + 处理）
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("consumer did not shut down")
            .expect("consumer panicked");

        // 断言：pending 已删除（处理成功）+ 归藏统计更新
        assert!(!task_file.exists(), "expected processed file to be deleted");
        let loaded = client
            .load_verification("v-closed-loop")
            .await
            .unwrap()
            .expect("asset should exist");
        let check = loaded
            .checks
            .iter()
            .find(|c| c.id == "check-loop")
            .unwrap();
        assert_eq!(check.stats.n, 1, "check stats must be backpropagated");
        assert_eq!(check.stats.pass_count, 1);

        cleanup(&data_root).await;
        cleanup(&knowledge_dir).await;
    }

    #[tokio::test]
    async fn test_process_checks_pending_parse_failure_moves_to_dead() {
        let data_root = create_test_root("checks_bad").await;
        let pending = data_root.join("pending");

        let knowledge_dir = std::env::temp_dir().join(format!(
            "taiji_dmn_knowledge_cbd_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let client = Arc::new(
            LiluoClient::new(&knowledge_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let evolver = Arc::new(CognitionEvolver::new(client));
        let cancel = CancellationToken::new();
        let consumer = DmnConsumer::new(evolver, cancel.clone(), &data_root, DmnConfig::default());
        let handle = consumer.spawn();

        // checks 字段格式错误（非数组）→ 解析失败 → 死信
        let task_file = pending.join("task_bad_checks.json");
        let payload = serde_json::json!({
            "task_id": "task_bad_checks",
            "checks": "not-an-array",
        });
        fs::write(&task_file, serde_json::to_string(&payload).unwrap())
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("consumer did not shut down")
            .expect("consumer panicked");

        // 原文件不残留（处理或死信）；死信目录应有文件
        assert!(!task_file.exists(), "bad checks file must be moved");
        let dead_dir = pending.join("dead");
        let dead_count = if dead_dir.exists() {
            fs::read_dir(&dead_dir).await.unwrap().next_entry().await.unwrap().is_some()
        } else {
            false
        };
        assert!(dead_count, "bad checks file should land in dead-letter");

        cleanup(&data_root).await;
        cleanup(&knowledge_dir).await;
    }
}
