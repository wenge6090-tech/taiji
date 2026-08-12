//! V33/MVP-3 主动学习（BCP §6.4 空闲窗口 — 契约假设验证版）。
//!
//! DMN 在 pending 空 + `runtime.dmn.active_learning_enabled` 开时：
//! 1. 选高不确定性变体契约节点（UCB 探索项最大者，§6.3 —— 低 N / 高方差优先）；
//! 2. 生成**模板化探索任务**（静态模板，零 LLM 调用 —— 纯符号层承诺 §6.4）；
//! 3. 写 `experiments/{ts}.json` 队列；
//! 4. 执行器（`spawn_runner`，main.rs 独立 spawn）消费队列：RecursiveRunner
//!    以 Execution 最小预算执行 → 对任务产出跑变体契约（SkillEngine 机械检查，
//!    零 LLM 裁决 —— 探索裁决符号化，与三权分立 §6.6 一致）→ CheckResult 入队
//!    pending（enqueue_dmn_pending 幂等）→ 删除 experiments 文件。
//!
//! 护栏：探索任务不产生新探索任务（无递归）；默认关闭（config 开关）；每窗口限量
//! （active_learning_max_per_window，DMN Consumer 侧限制入队数）。

use crate::agents::factory::AgentFactory;
use crate::infra::error::TaijiError;
use crate::infra::knowledge::LiluoClient;
use crate::infra::trace::save_json_atomic;
use crate::orchestration::skill_engine::SkillEngine;
use crate::orchestration::runner::RecursiveRunner;
use crate::types::agent::VerificationAsset;
use crate::types::verification::RewardWeights;
use crate::infra::config::TaijiConfig;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// UCB 探索常数 C（§6.3 默认 1.414）。
pub(crate) const UCB_C: f64 = 1.414;

/// 选择探索目标：**活跃变体资产**（variant_of 存在 —— 契约假设验证对象）中
/// UCB 探索分最高者。`score = avg_reward + C·√(ln N_total / N_node)`（§6.3）；
/// N_node = 0（变体无采样）→ 最大探索分（f64::MAX）。
///
/// V33/MVP-3.5: avg_reward 的 pass 分量用**贝叶斯后验均值**（§6.4.1，
/// posterior 传入；无后验 → 频率回退）。
///
/// 根资产不参与探索（已积累统计，走利用路径）；无候选返回 None。
pub fn pick_exploration_target(
    assets: &[VerificationAsset],
    weights: &RewardWeights,
    posterior: &BTreeMap<String, f64>,
) -> Option<usize> {
    let total_n: f64 = assets
        .iter()
        .flat_map(|a| a.checks.iter())
        .map(|c| c.stats.n as f64)
        .sum();
    let n_total = total_n.max(1.0);

    assets
        .iter()
        .enumerate()
        .filter(|(_, a)| a.status == "active" && a.variant_of.is_some())
        .max_by(|(_, a), (_, b)| {
            let sa = exploration_score(a, n_total, weights, posterior);
            let sb = exploration_score(b, n_total, weights, posterior);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
}

/// UCB 探索分（§6.3）。N_node = 0 → f64::MAX（最大探索——变体尚未被验证过）。
/// reward 的 pass 分量 = 后验均值（§6.4.1；无后验 → 频率 pass_rate 回退）。
fn exploration_score(
    a: &VerificationAsset,
    n_total: f64,
    weights: &RewardWeights,
    posterior: &BTreeMap<String, f64>,
) -> f64 {
    let n_node: f64 = a.checks.iter().map(|c| c.stats.n as f64).sum();
    if n_node < 1.0 {
        return f64::MAX;
    }
    let mu = posterior
        .get(&a.id)
        .copied()
        .unwrap_or_else(|| {
            let mut n = 0u64;
            let mut pass = 0u64;
            for c in &a.checks {
                n += c.stats.n;
                pass += c.stats.pass_count;
            }
            if n == 0 {
                0.0
            } else {
                pass as f64 / n as f64
            }
        });
    let avg_quality = a.checks.iter().map(|c| c.stats.avg_quality()).sum::<f64>() / n_node;
    let avg_cost = a.checks.iter().map(|c| c.stats.avg_cost()).sum::<f64>() / n_node;
    let avg_rounds = a.checks.iter().map(|c| c.stats.avg_rounds()).sum::<f64>() / n_node;
    let avg_reward = weights.pass * mu + weights.quality * avg_quality
        - weights.cost * avg_cost
        - weights.rounds * avg_rounds;
    avg_reward + UCB_C * (n_total.ln() / n_node).sqrt()
}

/// 模板化探索任务描述（静态模板，零 LLM 调用）。
/// 注入变体契约的检查目标（target），指引 Execution 最小任务产出可验证产物。
pub fn build_exploration_task(asset: &VerificationAsset) -> String {
    let targets: Vec<String> = asset.checks.iter().map(|c| c.target.clone()).collect();
    format!(
        "[探索任务] 为验证契约「{}」（{}）执行一次最小任务：\n\
         目标：产出 {} \n\
         要求：只做最小可行版本，保持简洁，完成后简述产出。\n\
         这是自动化学习探索：不分解、不递归、控制篇幅、完成即止。",
        asset.name,
        asset.id,
        targets.join("、")
    )
}

/// 生成探索任务并写入 experiments/ 队列（DMN Consumer 空闲窗口调用，每窗口限量）。
///
/// # Returns
/// 入队的探索任务数（0 = 无候选 / 已达窗口限量 / 队列非空）。
pub async fn enqueue_exploration_task(
    liluo: &LiluoClient,
    data_root: &Path,
    weights: &RewardWeights,
    max_per_window: u32,
) -> Result<u32, TaijiError> {
    let exp_dir = data_root.join("experiments");
    tokio::fs::create_dir_all(&exp_dir).await.map_err(|e| {
        TaijiError::Other(format!("failed to create experiments dir {:?}: {e}", exp_dir))
    })?;
    // 队列非空：等执行器消化（单执行器，防堆积）
    let mut entries = tokio::fs::read_dir(&exp_dir).await.map_err(|e| {
        TaijiError::Other(format!("failed to read experiments dir {:?}: {e}", exp_dir))
    })?;
    while let Some(entry) = entries.next_entry().await.transpose() {
        match entry {
            Ok(e) if e.path().extension().is_some_and(|x| x == "json") => {
                return Ok(0); // 队列已有任务在等
            }
            _ => {}
        }
    }

    let assets = liluo.load_all_verifications().await?;
    // V33/MVP-3.5: 贝叶斯后验 map（id → 后验均值；§6.4.1 探索分升级）
    let posterior: BTreeMap<String, f64> = liluo
        .load_all_models()
        .await?
        .iter()
        .map(|m| (m.header.id.clone(), m.posterior_mean()))
        .collect();
    let mut queued = 0u32;
    for _ in 0..max_per_window {
        let Some(idx) = pick_exploration_target(&assets, weights, &posterior) else {
            break;
        };
        let asset = &assets[idx];
        let payload = serde_json::json!({
            "task_id": format!("explore-{}", asset.id),
            "asset_id": asset.id,
            "description": build_exploration_task(asset),
        });
        let path = exp_dir.join(format!("{}.json", asset.id));
        save_json_atomic(&payload, &path).map_err(|e| {
            TaijiError::Other(format!("failed to write experiment {:?}: {e}", path))
        })?;
        queued += 1;
        tracing::info!(
            asset_id = %asset.id,
            "[active_learning] exploration task queued"
        );
        // 同资产不重复入队（id 唯一文件名天然幂等）
        break;
    }
    Ok(queued)
}

/// 探索执行器入口（main.rs `--with-dmn` 时 spawn；开关关闭 → 不启动）。
pub fn spawn_runner(
    factory: Arc<AgentFactory>,
    config: TaijiConfig,
    data_root: &Path,
    cancel: CancellationToken,
    liluo: Arc<LiluoClient>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !config.runtime.dmn.active_learning_enabled {
        return None;
    }
    let data_root = data_root.to_path_buf();
    Some(tokio::spawn(async move {
        let runner = RecursiveRunner::new(factory, config.clone());
        loop {
            if cancel.is_cancelled() {
                tracing::info!("[active_learning] runner cancelled, exiting");
                return;
            }
            match run_experiment_queue(&runner, &liluo, &data_root, &cancel).await {
                Ok(processed) if processed == 0 => {
                    // 空闲等待
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                        _ = cancel.cancelled() => {}
                    }
                }
                Ok(_) => {} // 处理完立即再扫
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "[active_learning] experiment queue scan failed — retry next cycle"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                        _ = cancel.cancelled() => {}
                    }
                }
            }
        }
    }))
}

/// 消费 experiments/ 队列：执行探索任务 → 对产物跑变体契约（机械检查）→
/// CheckResult 入队 pending → 删除 experiments 文件。
///
/// 单次尝试不重试：任务失败 → 文件改名 `.failed`（人工诊断）；I/O 错误上抛。
/// # Returns
/// 本次处理的探索任务数。
async fn run_experiment_queue(
    runner: &RecursiveRunner,
    liluo: &LiluoClient,
    data_root: &Path,
    cancel: &CancellationToken,
) -> Result<u32, TaijiError> {
    let exp_dir = data_root.join("experiments");
    if !exp_dir.exists() {
        return Ok(0);
    }
    let mut entries = tokio::fs::read_dir(&exp_dir).await.map_err(|e| {
        TaijiError::Other(format!("failed to read experiments dir {:?}: {e}", exp_dir))
    })?;
    let mut processed = 0u32;
    while let Some(entry) = entries.next_entry().await.transpose() {
        if cancel.is_cancelled() {
            break;
        }
        let path = entry.map_err(|e| {
            TaijiError::Other(format!("failed to read experiments entry: {e}"))
        })?.path();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if path.extension().is_none_or(|x| x != "json") || file_name.ends_with(".failed") {
            continue;
        }
        let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
            TaijiError::Other(format!("failed to read experiment {:?}: {e}", path))
        })?;
        let value: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                // 解析失败：标记 failed，不阻断队列
                let _ = tokio::fs::rename(&path, path.with_extension("json.failed")).await;
                tracing::warn!(file = %file_name, error = %e, "[active_learning] experiment parse failed");
                continue;
            }
        };
        let Some(desc) = value.get("description").and_then(|v| v.as_str()) else {
            let _ = tokio::fs::rename(&path, path.with_extension("json.failed")).await;
            continue;
        };
        let Some(asset_id) = value.get("asset_id").and_then(|v| v.as_str()) else {
            let _ = tokio::fs::rename(&path, path.with_extension("json.failed")).await;
            continue;
        };

        // 执行探索任务（Execution 最小预算 —— runner 默认配置；不递归由描述教学层保证）
        match runner.execute_with_context(desc, None, None).await {
            Ok(result) => {
                // 对产物跑变体契约（机械检查，零 LLM 裁决 —— §6.6 L0/L1）
                let task_dir = data_root.join("tasks").join(&result.task_id);
                if let Some(asset) = liluo.load_verification(asset_id).await? {
                    let report = SkillEngine::run_checks(&[asset], &task_dir).await;
                    let checks = report.results;
                    // CheckResult 入队 pending（幂等覆盖写；同任务重复探索覆盖不重复学习）。
                    // V36：携带分区键（liluo.partition_key）——回传落到变体所在分区。
                    if let Err(e) = crate::orchestration::tpn_cycle::enqueue_dmn_pending(
                        data_root,
                        &result.task_id,
                        &checks,
                        &[],
                        true,
                        liluo.partition_key(),
                    )
                    .await
                    {
                        tracing::warn!(
                            error = %e,
                            "[active_learning] pending enqueue failed for exploration task"
                        );
                    }
                }
                tokio::fs::remove_file(&path).await.map_err(|e| {
                    TaijiError::Other(format!("failed to remove experiment {:?}: {e}", path))
                })?;
                processed += 1;
                tracing::info!(
                    task_id = %result.task_id,
                    asset_id = %asset_id,
                    "[active_learning] exploration task completed"
                );
            }
            Err(e) => {
                // 探索任务执行失败：标记 failed（保留证据），不阻塞队列
                let _ = tokio::fs::rename(&path, path.with_extension("json.failed")).await;
                tracing::warn!(
                    asset_id = %asset_id,
                    error = %e,
                    "[active_learning] exploration task failed — marked .failed"
                );
            }
        }
    }
    Ok(processed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::knowledge::LiluoClient;
    use crate::types::verification::{CheckKind, CheckSeverity, CheckSpec, CheckStats};

    fn mk_asset(id: &str, n: u64, pass_count: u64, variant_of: Option<&str>) -> VerificationAsset {
        let mut a = VerificationAsset::new(
            id,
            id,
            "test",
            "test",
            vec![CheckSpec {
                id: format!("check-{id}"),
                kind: CheckKind::FileExists,
                target: "deliverables/out.md".into(),
                params: serde_json::json!({}),
                severity: CheckSeverity::Hard,
                pass_condition: "p".into(),
                stats: CheckStats { n, pass_count, ..Default::default() },
            }],
            vec!["general".into()],
        );
        a.variant_of = variant_of.map(|v| v.to_string());
        a
    }

    #[test]
    fn pick_exploration_target_prefers_unverified_variant() {
        let assets = vec![
            mk_asset("root-a", 20, 18, None),
            mk_asset("root-a-v1", 0, 0, Some("root-a")),
            mk_asset("root-a-v2", 3, 2, Some("root-a")),
            mk_asset("root-b", 10, 9, None),
        ];
        // 变体（n=0 → f64::MAX 探索分）优先于根资产
        let idx = pick_exploration_target(&assets, &RewardWeights::default(), &BTreeMap::new()).expect("candidate");
        assert_eq!(assets[idx].id, "root-a-v1", "unverified variant has max exploration score");
    }

    #[test]
    fn pick_exploration_target_none_without_variants() {
        let assets = vec![mk_asset("root-a", 20, 18, None), mk_asset("root-b", 5, 4, None)];
        assert!(pick_exploration_target(&assets, &RewardWeights::default(), &BTreeMap::new()).is_none());
    }

    #[test]
    fn build_exploration_task_includes_targets() {
        let asset = mk_asset("root-a-v1", 0, 0, Some("root-a"));
        let task = build_exploration_task(&asset);
        assert!(task.contains("deliverables/out.md"), "target injected: {task}");
        assert!(task.contains("root-a-v1"), "asset id injected: {task}");
        assert!(!task.contains("llm"), "static template — no LLM phrasing");
    }

    #[tokio::test]
    async fn enqueue_exploration_task_writes_queue_and_dedups() {
        let dir = std::env::temp_dir().join(format!(
            "taiji_al_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let knowledge = dir.join("knowledge");
        tokio::fs::create_dir_all(&knowledge).await.unwrap();
        let liluo = LiluoClient::new(&knowledge).await.unwrap();

        // 一个低 N 变体 → 入队 1 个
        let mut root = mk_asset("x-a", 10, 9, None);
        liluo.save_verification(&mut root).await.unwrap();
        let mut v1 = mk_asset("x-a-v1", 0, 0, Some("x-a"));
        liluo.save_verification(&mut v1).await.unwrap();

        let queued = enqueue_exploration_task(&liluo, &dir, &RewardWeights::default(), 1)
            .await
            .unwrap();
        assert_eq!(queued, 1);
        assert!(dir.join("experiments").join("x-a-v1.json").exists());

        // 队列非空 → 防堆积：不再入队
        let queued2 = enqueue_exploration_task(&liluo, &dir, &RewardWeights::default(), 1)
            .await
            .unwrap();
        assert_eq!(queued2, 0, "queue busy — no stacking");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
