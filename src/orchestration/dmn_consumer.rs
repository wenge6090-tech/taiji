//! DMN Consumer — background task polling pending/ queue.
//! tokio::spawn with CancellationToken, exponential backoff.
//! See AGENTS.md §6 for detailed rules.
//!
//! The consumer periodically scans `{data_root}/pending/` for `.json` task files,
//! feeds them to the CognitionEvolver, and moves processed/dead files accordingly.
//! Backoff starts at 1 s and caps at 60 s (1, 2, 4, 8, 16, 32, 60, 60, …).

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
}

impl DmnConsumer {
    /// Create a new DMN Consumer.
    ///
    /// * `evolver` — shared cognition evolver for δ₀–δ₃ operations.
    /// * `cancel` — cancellation token; the loop exits when this is signalled.
    /// * `data_root` — root directory containing the `pending/` subdirectory.
    pub fn new(
        evolver: Arc<CognitionEvolver>,
        cancel: CancellationToken,
        data_root: &Path,
    ) -> Self {
        Self {
            evolver,
            cancel,
            data_root: data_root.to_path_buf(),
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
                            if path.extension().map_or(false, |ext| ext == "json") {
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

                // Evolve with retry (3 attempts for transient Qdrant errors).
                const MAX_EVOLVE_RETRIES: u32 = 3;
                let mut evolve_result = Err(TaijiError::Other("not started".into()));
                for attempt in 1..=MAX_EVOLVE_RETRIES {
                    if attempt > 1 {
                        let delay_ms = 500 * attempt; // 1s, 1.5s
                        Self::backoff_sleep(&self.cancel, delay_ms as u64).await;
                    }
                    evolve_result = self.evolver.evolve(task_id, &[]).await;
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
    use crate::infra::config::QdrantConfig;
    use crate::infra::qdrant::NskgClient;
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

    /// Build a DmnConsumer backed by a real evolver (requires Qdrant).
    async fn test_consumer(data_root: &Path) -> DmnConsumer {
        let config = QdrantConfig {
            url: "http://localhost:6334".to_string(),
            collection_name: "test_nskg_dmn".to_string(),
        };
        let client = Arc::new(
            NskgClient::new(&config)
                .await
                .expect("Qdrant must be running on localhost:6334 for DMN consumer tests"),
        );
        let evolver = Arc::new(CognitionEvolver::new(client));
        DmnConsumer::new(evolver, CancellationToken::new(), data_root)
    }

    /// Clean up a test root directory.
    async fn cleanup(root: &Path) {
        let _ = fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_spawn_and_cancel() {
        let data_root = create_test_root("spawn_cancel").await;
        let cancel = CancellationToken::new();
        let consumer = test_consumer(&data_root).await;
        let handle = consumer.spawn();

        // Give the loop time to spin once, then cancel.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        cancel.cancel();

        // The task should finish promptly.
        tokio::time::timeout(tokio::time::Duration::from_secs(5), handle)
            .await
            .expect("consumer did not shut down within 5 s")
            .expect("consumer task panicked");

        cleanup(&data_root).await;
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_process_valid_file() {
        let data_root = create_test_root("valid_file").await;
        let pending = data_root.join("pending");

        // Write a valid JSON task file.
        let task_file = pending.join("task_abc123.json");
        let payload = serde_json::json!({
            "task_id": "task_abc123",
            "description": "Test DMN evolution",
        });
        fs::write(&task_file, serde_json::to_string_pretty(&payload).unwrap())
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        let consumer = test_consumer(&data_root).await;
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
    }

    #[tokio::test]
    #[ignore = "requires Qdrant on localhost:6334"]
    async fn test_process_invalid_file_moves_to_dead() {
        let data_root = create_test_root("invalid_file").await;
        let pending = data_root.join("pending");

        // Write a corrupt JSON file.
        let task_file = pending.join("corrupt.json");
        fs::write(&task_file, b"not valid json").await.unwrap();

        let cancel = CancellationToken::new();
        let consumer = test_consumer(&data_root).await;
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
    }
}
