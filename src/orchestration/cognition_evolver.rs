//! CognitionEvolver — DMN cognitive evolution (δ₀-δ₂ + V33/MVP-3 契约演化).
//! Called by DMN Consumer background task.
//! See AGENTS.md §6 for detailed rules.
//!
//! Operations:
//! - δ₀: Prune low-confidence nodes (confidence < threshold).
//! - δ₁: L1 skill tuning (update success/fail counts).
//! - δ₂: L2 Bayesian confidence update (预留).
//! - evolve(): Run δ₀→δ₂ in sequence, producing an EvolutionReport.
//! - V33/MVP-3: `evolve_contracts()` — MCTS 四算子作用于契约空间（§8.21）：
//!   backprop（dmn_consumer 分发）→ fork（严格度参数化变体）→ merge（相似变体合并）→
//!   prune（组内 >2σ 淘汰）。纯符号层，不调 LLM。
//!
//! # 归藏 integration
//! Evolution results are written back to the 归藏 knowledge store as
//! metadata placeholders (V22: grids/ removed — no asset is persisted).

use crate::infra::config::DmnConfig;
use crate::infra::error::TaijiError;
use crate::infra::knowledge::{LiluoClient, ModelAsset};
use crate::infra::trace::TraceRecord;
use crate::types::agent::{AssetRef, PromptAsset, VerificationAsset};
use crate::types::verification::{CheckKind, CheckResult, CheckStats};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// fork 触发阈值：资产级通过率 < 0.6 且采样 ≥ min_samples → 生成严格度变体。
/// （MVP-3 定稿，沉淀 AGENTS.md；BCP §8.21「低回报资产扩展变体」的定量化）
const FORK_PASS_RATE_THRESHOLD: f64 = 0.6;
/// merge 触发阈值：组内通过率差 < 0.1 视为无显著差异 → 合并到最优。
const MERGE_PASS_RATE_DIFF: f64 = 0.1;

/// Aggregate report produced by a full evolution cycle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    /// V33/MVP-3: fork 生成的变体契约数。
    #[serde(default)]
    pub forked: u64,
    /// V33/MVP-3: merge 合并的变体契约数（次者 pruned）。
    #[serde(default)]
    pub merged: u64,
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

    /// 根归藏 client 访问器（V36：model_stats 根级回传——update_model_stats
    /// 需要 knowledge 根，分区 client 与根 client 的 root_dir 一致，传根即可）。
    pub fn liluo(&self) -> &Arc<LiluoClient> {
        &self.liluo
    }

    /// V44 去分区化：回传统一落根级资产树（model_key 仅作统计键，不派生 client）。
    async fn partition_liluo(
        &self,
        _model_key: Option<&str>,
    ) -> Result<Arc<LiluoClient>, TaijiError> {
        Ok(self.liluo.clone())
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

    /// δ₂ → V33/MVP-3.5: Beta-Bernoulli 后验更新（持久化版 — BCP §6.4.1）。
    ///
    /// 加载 `models/{asset_id}.yaml`；不存在 → 从关联 verification 的 `confidence`
    /// 映射先验初始化（α = 1 + k·confidence，β = 1 + k·(1−confidence)，k = prior_strength）；
    /// 然后 α += success、β += fail → save_model（version++ 原子写）→ 返回后验均值。
    ///
    /// **单写者约束**：仅 DMN Consumer（backprop 路径）调用——TPN 执行期归藏只读（§8.3）。
    pub async fn bayesian_update(
        &self,
        liluo: &LiluoClient,
        asset_id: &str,
        success: u64,
        fail: u64,
        prior_confidence: f64,
        prior_strength: f64,
    ) -> Result<f64, TaijiError> {
        let mut model = match liluo.load_model(asset_id).await? {
            Some(m) => m,
            None => {
                // 首次：纯先验（§6.4.1 先验映射）
                let mut m = ModelAsset::from_prior(
                    asset_id,
                    asset_id,
                    prior_confidence,
                    prior_strength,
                );
                // from_prior 内部构造已含先验伪计数；此处先持久化让版本链从 1 开始
                liluo.save_model(&mut m).await?;
                m
            }
        };
        model.alpha += success as f64;
        model.beta += fail as f64;
        liluo.save_model(&mut model).await?;
        let mean = model.posterior_mean();
        tracing::info!(
            asset_id = %asset_id,
            alpha = model.alpha,
            beta = model.beta,
            posterior_mean = mean,
            "[bayesian_update] Beta posterior updated"
        );
        Ok(mean)
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
                    .bayesian_update(&self.liluo, &record.task_id, 1, 1, 0.5, 10.0)
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
            ..Default::default()
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

    /// V33/MVP-2：检查项统计回传（backprop — BCP §6.4 V33 统计粒度 / §8.23）。
    ///
    /// 加载全部验证契约资产，按 `check_id` 匹配检查项，累加 `stats`
    /// （n++ / passed → pass_count++），写回归藏（version++）。
    ///
    /// **单写者约束**：仅 DMN Consumer（单线程后台循环）调用本方法——
    /// TPN 执行期间归藏只读（§8.3 硬约束），本方法是被动学习的唯一写路径。
    ///
    /// 未匹配到资产的 check_id 仅 warn（资产可能已删除/重构）；
    /// 归藏 I/O 失败重试语义由调用方（DMN Consumer）负责。
    ///
    /// # Returns
    /// 更新的检查项数量。
    pub async fn backprop_checks(
        &self,
        task_id: &str,
        checks: &[CheckResult],
        config: &DmnConfig,
        model_key: Option<&str>,
    ) -> Result<u64, TaijiError> {
        if checks.is_empty() {
            tracing::debug!(task_id, "[backprop_checks] empty checks — no-op");
            return Ok(0);
        }

        // V36 分区一致性（§8.3）：统计回传到路由模型分区（None → 根/legacy）。
        let liluo = self.partition_liluo(model_key).await?;
        let mut verifications = liluo.load_all_verifications().await?;
        let mut updated_total = 0u64;
        let mut matched_ids: Vec<String> = Vec::new();

        for verification in &mut verifications {
            let mut updated_any = false;
            // V33/MVP-3.5: 贝叶斯双轨——本资产聚合成败（§6.4.1）
            let mut bayes_success = 0u64;
            let mut bayes_fail = 0u64;
            for check in &mut verification.checks {
                let Some(result) = checks.iter().find(|r| r.check_id == check.id) else {
                    continue;
                };
                check.stats.n += 1;
                if result.passed {
                    check.stats.pass_count += 1;
                    bayes_success += 1;
                } else {
                    bayes_fail += 1;
                }
                // V33/MVP-3: 四维回报累加（§6.4——cost/rounds/quality 随 CheckResult 摊派）
                check.stats.cost_sum += result.cost_tokens;
                check.stats.rounds_sum += result.verify_rounds as u64;
                check.stats.quality_sum += result.quality;
                matched_ids.push(check.id.clone());
                updated_any = true;
                updated_total += 1;
            }
            if updated_any {
                liluo.save_verification(verification).await?;
                // V33/MVP-3.5: 贝叶斯后验双轨（开关关 → 跳过；失败仅 warn——
                // 频率统计是主数据已持久化，贝叶斯是可增强维度，不阻断学习）
                if config.bayesian_enabled && (bayes_success > 0 || bayes_fail > 0) {
                    if let Err(e) = self
                        .bayesian_update(
                            &liluo,
                            &verification.id,
                            bayes_success,
                            bayes_fail,
                            verification.confidence,
                            config.prior_strength,
                        )
                        .await
                    {
                        tracing::warn!(
                            task_id,
                            asset_id = %verification.id,
                            error = %e,
                            "[backprop_checks] bayesian update failed — frequency stats already persisted"
                        );
                    }
                }
            }
        }

        for result in checks {
            if !matched_ids.contains(&result.check_id) {
                tracing::warn!(
                    task_id,
                    check_id = %result.check_id,
                    "[backprop_checks] no asset contains check_id — skipping"
                );
            }
        }

        tracing::info!(
            task_id,
            updated = updated_total,
            total_checks = checks.len(),
            "[backprop_checks] check stats backpropagated"
        );
        Ok(updated_total)
    }

    // ── V33/MVP-3: MCTS 契约演化（§6.4 四算子 / §8.21 契约空间）──

    /// 演化入口：激活门槛（§8.12：资产 ≥ activation_min_assets 且总采样 ≥
    /// activation_min_samples）通过后依次 fork → merge → prune。
    /// 纯符号层操作（不调 LLM），每个算子独立 load/save（原子写）。
    ///
    /// **单次执行语义**（与 backprop 一致，不重试）：算子条件满足后状态改变，
    /// 重试不会重复操作（幂等）；中途 I/O 失败的部分应用由调用方死信处理。
    pub async fn evolve_contracts(
        &self,
        config: &DmnConfig,
        model_key: Option<&str>,
    ) -> Result<EvolutionReport, TaijiError> {
        // V36 分区一致性（§8.3）：演化作用于 pending 路由模型分区（None → 根）。
        let liluo = self.partition_liluo(model_key).await?;
        let assets = liluo.load_all_verifications().await?;
        if !Self::activation_gate(&assets, config) {
            tracing::debug!(
                assets = assets.len(),
                min_assets = config.activation_min_assets,
                min_samples = config.activation_min_samples,
                "[evolve_contracts] activation gate not met — evolution skipped"
            );
            return Ok(EvolutionReport::default());
        }

        // V33/MVP-3.5: 贝叶斯后验 map（§6.4.1）——bayesian_enabled=false → 空 map，
        // 算子回退频率路径（MVP-3 行为不变）。
        let posterior: BTreeMap<String, (f64, f64)> = if config.bayesian_enabled {
            let models = liluo.load_all_models().await?;
            Self::asset_posterior_map(&assets, &models, config)
        } else {
            BTreeMap::new()
        };

        let forked = self.fork_variants(&liluo, config, &posterior).await?;
        let merged = self.merge_variants(&liluo, config, &posterior).await?;
        let pruned = self.prune_variants(&liluo, config, &posterior).await?;

        // V35/MVP-6: prompts 对称演化（同一 evolve 调用内串行——单写者保持 §8.3）
        let p_report = self.evolve_prompts(&liluo, config).await?;

        tracing::info!(
            forked,
            merged,
            pruned,
            p_forked = p_report.forked,
            p_merged = p_report.merged,
            p_pruned = p_report.pruned,
            bayesian = config.bayesian_enabled,
            "[evolve_contracts] MCTS evolution cycle complete (verifications + prompts)"
        );
        Ok(EvolutionReport {
            pruned: pruned + p_report.pruned,
            forked: forked + p_report.forked,
            merged: merged + p_report.merged,
            ..Default::default()
        })
    }

    /// V35/MVP-6：prompts 任务级回传（§8.21）——pending 的 assets_used 分发到此。
    /// 信号粒度：任务级（passed 成败 + checks 首项四维摊派——同任务所有检查项
    /// 摊派值一致，取首项即可）；verifications 走检查项级（backprop_checks）。
    /// 贝叶斯双轨：与 backprop_checks 同构（prior = confidence 映射，β++ 降权）。
    pub async fn backprop_prompts(
        &self,
        task_id: &str,
        assets_used: &[AssetRef],
        passed: bool,
        checks: &[CheckResult],
        config: &DmnConfig,
        model_key: Option<&str>,
    ) -> Result<u64, TaijiError> {
        let prompt_refs: Vec<&str> = assets_used
            .iter()
            .filter(|a| a.asset_type == "prompt")
            .map(|a| a.id.as_str())
            .collect();
        if prompt_refs.is_empty() {
            tracing::debug!(task_id, "[backprop_prompts] no prompt assets used — no-op");
            return Ok(0);
        }

        // V36 分区一致性（§8.3）：prompts 回传到路由模型分区（None → 根/legacy）。
        let liluo = self.partition_liluo(model_key).await?;

        // 任务级四维信号（§6.4——同任务摊派值一致，取首项；空 checks → 0）
        let cost = checks.first().map(|c| c.cost_tokens).unwrap_or(0);
        let rounds = checks.first().map(|c| c.verify_rounds as u64).unwrap_or(0);
        let quality = checks.first().map(|c| c.quality).unwrap_or(0.0);

        let mut updated = 0u64;
        for pid in prompt_refs {
            let Some(mut p) = liluo.load_prompt(pid).await? else {
                tracing::warn!(task_id, prompt = pid, "[backprop_prompts] prompt asset not found — skipping");
                continue;
            };
            p.stats.n += 1;
            if passed {
                p.stats.pass_count += 1;
            }
            p.stats.cost_sum += cost;
            p.stats.rounds_sum += rounds;
            p.stats.quality_sum += quality;
            // 旧字段同步（兼容既有消费方——§6.2 usage_count/success_rate）
            p.usage_count += 1;
            if p.stats.n > 0 {
                p.success_rate = Self::stats_pass_rate(&p.stats);
            }
            liluo.save_prompt(&mut p).await?;
            // 贝叶斯双轨（开关关 → 跳过；失败仅 warn——频率是主数据）
            if config.bayesian_enabled {
                let (s, f) = if passed { (1, 0) } else { (0, 1) };
                if let Err(e) = self
                    .bayesian_update(&liluo, &p.id, s, f, p.confidence, config.prior_strength)
                    .await
                {
                    tracing::warn!(
                        prompt = %p.id,
                        error = %e,
                        "[backprop_prompts] bayesian update failed — frequency already saved"
                    );
                }
            }
            updated += 1;
            tracing::debug!(
                task_id,
                prompt = %p.id,
                passed,
                n = p.stats.n,
                "[backprop_prompts] prompt stats updated"
            );
        }
        Ok(updated)
    }

    /// V35/MVP-6：prompts 四算子对称演化（§8.21）——与 verifications 同一
    /// reward/阈值/贝叶斯框架（共享 stats_pass_rate / 决策值 / 后验 map）。
    /// 激活门槛独立判定（prompts 层自身资产数 + 采样数）。
    pub async fn evolve_prompts(
        &self,
        liluo: &LiluoClient,
        config: &DmnConfig,
    ) -> Result<EvolutionReport, TaijiError> {
        let prompts = liluo.load_all_prompts().await?;
        if !Self::activation_gate_prompts(&prompts, config) {
            tracing::debug!(
                prompts = prompts.len(),
                "[evolve_prompts] activation gate not met — evolution skipped"
            );
            return Ok(EvolutionReport::default());
        }
        // 同一后验 map 机制（§6.4.1）
        let posterior: BTreeMap<String, (f64, f64)> = if config.bayesian_enabled {
            let models = liluo.load_all_models().await?;
            Self::prompt_posterior_map(&prompts, &models, config)
        } else {
            BTreeMap::new()
        };

        let forked = self.fork_prompts(liluo, config, &posterior).await?;
        let merged = self.merge_prompts(liluo, config, &posterior).await?;
        let pruned = self.prune_prompts(liluo, config, &posterior).await?;
        tracing::info!(
            forked,
            merged,
            pruned,
            "[evolve_prompts] prompt MCTS evolution cycle complete"
        );
        Ok(EvolutionReport {
            pruned,
            forked,
            merged,
            ..Default::default()
        })
    }

    /// prompt 后验 map：id → (μ, σ)——与 asset_posterior_map 同式（先验映射 §6.4.1）。
    fn prompt_posterior_map(
        prompts: &[PromptAsset],
        models: &[ModelAsset],
        config: &DmnConfig,
    ) -> BTreeMap<String, (f64, f64)> {
        prompts
            .iter()
            .map(|p| {
                let (mu, sigma) = match models.iter().find(|m| m.header.id == p.id) {
                    Some(m) => (m.posterior_mean(), m.posterior_sigma()),
                    None => {
                        let c = p.confidence.clamp(0.0, 1.0);
                        let k = config.prior_strength.max(0.0);
                        let alpha = 1.0 + k * c;
                        let beta = 1.0 + k * (1.0 - c);
                        let mu = alpha / (alpha + beta);
                        let sigma =
                            (alpha * beta / ((alpha + beta).powi(2) * (alpha + beta + 1.0)))
                                .sqrt();
                        (mu, sigma)
                    }
                };
                (p.id.clone(), (mu, sigma))
            })
            .collect()
    }

    /// δ-fork（prompts）：根资产（无 variant_of）低决策值 → 生成变体。
    /// 与 fork_variants 同构：n ≥ min_samples、决策值 < FORK_PASS_RATE_THRESHOLD、
    /// 已 fork 防重复；变体 confidence×0.8 + stats 清零 + ModelAsset 独立初始化。
    async fn fork_prompts(
        &self,
        liluo: &LiluoClient,
        config: &DmnConfig,
        posterior: &BTreeMap<String, (f64, f64)>,
    ) -> Result<u64, TaijiError> {
        let prompts = liluo.load_all_prompts().await?;
        let mut forked = 0u64;

        for p in prompts.iter().filter(|p| p.variant_of.is_none()) {
            if prompts.iter().any(|v| v.variant_of.as_deref() == Some(p.id.as_str())) {
                continue; // 已 fork 过，防每次演化循环重复生成
            }
            let total_n = p.stats.n;
            let mu = posterior
                .get(&p.id)
                .map(|m| m.0)
                .unwrap_or_else(|| Self::stats_pass_rate(&p.stats));
            if total_n < config.min_samples || mu >= FORK_PASS_RATE_THRESHOLD {
                continue;
            }

            let mut variant = p.clone();
            variant.id = format!("{}-v1", p.id);
            variant.name = format!("{}（strict 变体）", p.name);
            variant.variant_of = Some(p.id.clone());
            variant.parent_id = Some(p.id.clone());
            variant.confidence = p.confidence * 0.8;
            variant.stats = CheckStats::default();
            liluo.save_prompt(&mut variant).await?;
            if config.bayesian_enabled {
                let mut model = ModelAsset::from_prior(
                    &variant.id,
                    &variant.id,
                    variant.confidence,
                    config.prior_strength,
                );
                liluo.save_model(&mut model).await?;
            }
            forked += 1;
            tracing::info!(
                root = %p.id,
                variant = %variant.id,
                mu = mu,
                "[fork_prompts] forked variant"
            );
        }
        Ok(forked)
    }

    /// δ-merge（prompts）：同组决策值差 < MERGE_PASS_RATE_DIFF → 统计并入最优，
    /// 次者 pruned（同分根优先——read_dir 顺序确定性，与 merge_variants 同构）。
    async fn merge_prompts(
        &self,
        liluo: &LiluoClient,
        config: &DmnConfig,
        posterior: &BTreeMap<String, (f64, f64)>,
    ) -> Result<u64, TaijiError> {
        let mut prompts = liluo.load_all_prompts().await?;
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, p) in prompts.iter().enumerate() {
            let key = p.variant_of.clone().unwrap_or_else(|| p.id.clone());
            groups.entry(key).or_default().push(i);
        }
        let mut merged = 0u64;
        let mut changed: Vec<usize> = Vec::new();

        for members in groups.values() {
            let eligible: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| prompts[i].stats.n >= config.min_samples)
                .collect();
            if eligible.len() < 2 {
                continue;
            }
            let mu_of = |p: &PromptAsset| {
                posterior
                    .get(&p.id)
                    .map(|m| m.0)
                    .unwrap_or_else(|| Self::stats_pass_rate(&p.stats))
            };
            let mut sorted = eligible.clone();
            sorted.sort_by(|&a, &b| {
                mu_of(&prompts[b])
                    .partial_cmp(&mu_of(&prompts[a]))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        match (prompts[a].variant_of.is_none(), prompts[b].variant_of.is_none()) {
                            (true, false) => std::cmp::Ordering::Less,
                            (false, true) => std::cmp::Ordering::Greater,
                            _ => std::cmp::Ordering::Equal,
                        }
                    })
            });
            let best = sorted[0];
            for &candidate in &sorted[1..] {
                if (mu_of(&prompts[best]) - mu_of(&prompts[candidate])).abs() < MERGE_PASS_RATE_DIFF {
                    // 统计并入最优（任务级 stats 单块，直接累加）
                    prompts[best].stats.n += prompts[candidate].stats.n;
                    prompts[best].stats.pass_count += prompts[candidate].stats.pass_count;
                    prompts[best].stats.cost_sum += prompts[candidate].stats.cost_sum;
                    prompts[best].stats.rounds_sum += prompts[candidate].stats.rounds_sum;
                    prompts[best].stats.quality_sum += prompts[candidate].stats.quality_sum;
                    prompts[best].usage_count += prompts[candidate].usage_count;
                    // 贝叶斯后验合并：根吸收候选采样增量（去先验伪计数，§6.4.1）
                    if config.bayesian_enabled {
                        let cand_prior_alpha = 1.0
                            + config.prior_strength.max(0.0)
                                * prompts[candidate].confidence.clamp(0.0, 1.0);
                        let cand_prior_beta = 1.0
                            + config.prior_strength.max(0.0)
                                * (1.0 - prompts[candidate].confidence.clamp(0.0, 1.0));
                        if let Ok(Some(mut best_model)) =
                            liluo.load_model(&prompts[best].id).await
                        {
                            if let Ok(Some(cand_model)) =
                                liluo.load_model(&prompts[candidate].id).await
                            {
                                best_model.alpha += cand_model.alpha - cand_prior_alpha;
                                best_model.beta += cand_model.beta - cand_prior_beta;
                                if let Err(e) = liluo.save_model(&mut best_model).await {
                                    tracing::warn!(
                                        asset_id = %prompts[best].id,
                                        error = %e,
                                        "[merge_prompts] posterior merge failed — frequency merge already saved"
                                    );
                                }
                            }
                        }
                    }
                    prompts[candidate].status_mark_merged();
                    merged += 1;
                    if !changed.contains(&best) {
                        changed.push(best);
                    }
                    if !changed.contains(&candidate) {
                        changed.push(candidate);
                    }
                }
            }
        }
        for i in changed {
            liluo.save_prompt(&mut prompts[i]).await?;
        }
        Ok(merged)
    }

    /// δ-prune（prompts）：组内成员中决策值低于组内最优 − 2·σ(候选自身 Beta
    /// 后验) → status 标记 pruned（保留文件供审计，load_all_prompts 过滤）。
    /// 频率回退（bayesian_enabled=false）：组内 σ 标准差版（与 prune_variants 同构）。
    async fn prune_prompts(
        &self,
        liluo: &LiluoClient,
        config: &DmnConfig,
        posterior: &BTreeMap<String, (f64, f64)>,
    ) -> Result<u64, TaijiError> {
        let mut prompts = liluo.load_all_prompts().await?;
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, p) in prompts.iter().enumerate() {
            let key = p.variant_of.clone().unwrap_or_else(|| p.id.clone());
            groups.entry(key).or_default().push(i);
        }
        let mut pruned = 0u64;

        for members in groups.values() {
            let eligible: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| prompts[i].stats.n >= config.min_samples)
                .collect();
            if eligible.len() < 2 {
                continue;
            }
            let mu_of = |i: usize| {
                posterior
                    .get(&prompts[i].id)
                    .map(|m| m.0)
                    .unwrap_or_else(|| Self::stats_pass_rate(&prompts[i].stats))
            };
            let mus: Vec<f64> = eligible.iter().map(|&i| mu_of(i)).collect();
            let best = mus.iter().copied().fold(f64::MIN, f64::max);

            if config.bayesian_enabled {
                for (idx, &i) in eligible.iter().enumerate() {
                    let sigma_cand = posterior.get(&prompts[i].id).map(|m| m.1).unwrap_or(0.0);
                    if mus[idx] < best - 2.0 * sigma_cand {
                        prompts[i].status_mark_pruned();
                        pruned += 1;
                        tracing::info!(
                            id = %prompts[i].id,
                            mu = mus[idx],
                            best_mu = best,
                            "[prune_prompts] pruned below best−2σ_beta"
                        );
                    }
                }
            } else {
                let mean = mus.iter().sum::<f64>() / mus.len() as f64;
                let sigma = (mus
                    .iter()
                    .map(|r| (r - mean).powi(2))
                    .sum::<f64>()
                    / mus.len() as f64)
                .sqrt();
                if sigma == 0.0 {
                    continue;
                }
                for (idx, &i) in eligible.iter().enumerate() {
                    if mus[idx] < best - 2.0 * sigma {
                        prompts[i].status_mark_pruned();
                        pruned += 1;
                    }
                }
            }
        }
        if pruned > 0 {
            for p in prompts.iter_mut().filter(|p| p.is_pruned()) {
                liluo.save_prompt(p).await?;
            }
        }
        Ok(pruned)
    }

    /// 贝叶斯后验 map：active verification id → (后验均值 μ, 后验标准差 σ)。
    /// 无 ModelAsset（未采样）→ 从 confidence 映射纯先验（§6.4.1）。
    /// bayesian_enabled=false 时调用方传空 map（算子回退频率路径）。
    fn asset_posterior_map(
        assets: &[VerificationAsset],
        models: &[ModelAsset],
        config: &DmnConfig,
    ) -> BTreeMap<String, (f64, f64)> {
        let k = config.prior_strength.max(0.0);
        assets
            .iter()
            .map(|a| {
                let (mu, sigma) = match models.iter().find(|m| m.header.id == a.id) {
                    Some(m) => (m.posterior_mean(), m.posterior_sigma()),
                    None => {
                        // 纯先验（未采样）：α = 1 + k·c，β = 1 + k·(1−c)
                        let c = a.confidence.clamp(0.0, 1.0);
                        let alpha = 1.0 + k * c;
                        let beta = 1.0 + k * (1.0 - c);
                        let total = alpha + beta;
                        (
                            alpha / total,
                            (alpha * beta / (total * total * (total + 1.0))).sqrt(),
                        )
                    }
                };
                (a.id.clone(), (mu, sigma))
            })
            .collect()
    }

    /// 激活门槛（§8.12）：每层资产数 ≥ 5 且累积采样 ≥ 50。
    /// backprop 不受限（数据积累期），演化算子受此门槛保护。
    fn activation_gate(assets: &[VerificationAsset], config: &DmnConfig) -> bool {
        if assets.len() < config.activation_min_assets {
            return false;
        }
        let total_n: u64 = assets
            .iter()
            .flat_map(|a| a.checks.iter())
            .map(|c| c.stats.n)
            .sum();
        total_n >= config.activation_min_samples
    }

    /// V35/MVP-6：prompts 版激活门槛（同一数学：资产数 + 总采样数，独立判定）。
    fn activation_gate_prompts(prompts: &[PromptAsset], config: &DmnConfig) -> bool {
        if prompts.len() < config.activation_min_assets {
            return false;
        }
        let total_n: u64 = prompts.iter().map(|p| p.stats.n).sum();
        total_n >= config.activation_min_samples
    }

    /// δ-fork：低回报根资产（资产级通过率 < 0.6 且采样 ≥ min_samples）→
    /// 生成 **strict 严格度参数化变体**（§8.21「放宽/收紧判据」的机械实现）：
    /// 复制资产 + llm_judgement 项 `params.strictness = "strict"` + check id 重命名
    /// `{原id}@{变体id}`（防 backprop 撞名）+ stats 清零（变体独立采样）+ 降权
    /// （confidence × 0.8）+ `variant_of` 链接。判据文本不动（内容修订走人工通道）。
    ///
    /// 防重复：已存在变体的根资产不重复 fork；变体不 fork 变体（树不爆炸）。
    async fn fork_variants(
        &self,
        liluo: &LiluoClient,
        config: &DmnConfig,
        posterior: &BTreeMap<String, (f64, f64)>,
    ) -> Result<u64, TaijiError> {
        let assets = liluo.load_all_verifications().await?;
        let mut forked = 0u64;

        for asset in assets.iter().filter(|a| a.variant_of.is_none()) {
            // 已 fork 过（存在指向本资产的变体）→ 跳过，防每次演化循环重复生成
            if assets.iter().any(|v| v.variant_of.as_deref() == Some(asset.id.as_str())) {
                continue;
            }
            // 无 llm_judgement 项：无参数化空间，fork 无意义
            if !asset.checks.iter().any(|c| c.kind == CheckKind::LlmJudgement) {
                continue;
            }
            let total_n = Self::asset_total_n(asset);
            // V33/MVP-3.5: 决策值 = 后验均值（空 map → 频率回退）
            let mu = posterior
                .get(&asset.id)
                .map(|p| p.0)
                .unwrap_or_else(|| Self::asset_pass_rate(asset));
            if total_n < config.min_samples || mu >= FORK_PASS_RATE_THRESHOLD {
                continue;
            }

            // 生成 strict 变体（MVP-3：固定生成 strict 档——低回报 = 判据太松）
            let mut variant = asset.clone();
            variant.id = format!("{}-v1", asset.id);
            variant.name = format!("{}（strict 变体）", asset.name);
            variant.variant_of = Some(asset.id.clone());
            variant.confidence = asset.confidence * 0.8;
            for check in &mut variant.checks {
                check.stats = CheckStats::default();
                if check.kind == CheckKind::LlmJudgement {
                    check.params["strictness"] = serde_json::json!("strict");
                    // 变体 check id 重命名：`{base}@{variant}` —— 回传精确落位变体，
                    // 原资产统计零污染（Verify 加载变体时 SkillReport 天然携带全 id）
                    check.id = format!("{}@{}", check.id, variant.id);
                }
            }
            liluo.save_verification(&mut variant).await?;
            // V33/MVP-3.5: 变体后验独立初始化（降权 confidence 映射先验，stats 清零同构）
            if config.bayesian_enabled {
                let mut model = ModelAsset::from_prior(
                    &variant.id,
                    &variant.id,
                    variant.confidence,
                    config.prior_strength,
                );
                liluo.save_model(&mut model).await?;
            }
            forked += 1;
            tracing::info!(
                root = %asset.id,
                variant = %variant.id,
                mu = mu,
                "[fork_variants] forked strict variant"
            );
        }
        Ok(forked)
    }

    /// δ-merge：同组（variant_of 指向同一根）n ≥ min_samples 的成员，
    /// 通过率差 < 0.1（无显著差异）→ 统计按 check 位置并入最优者，次者 pruned。
    async fn merge_variants(
        &self,
        liluo: &LiluoClient,
        config: &DmnConfig,
        posterior: &BTreeMap<String, (f64, f64)>,
    ) -> Result<u64, TaijiError> {
        let mut assets = liluo.load_all_verifications().await?;
        let groups = Self::group_variants(&assets);
        let mut merged = 0u64;
        let mut changed: Vec<usize> = Vec::new();
        // V33/MVP-3.5: 决策值 = 后验均值（空 map → 频率回退）
        let mu_of = |a: &VerificationAsset| {
            posterior
                .get(&a.id)
                .map(|p| p.0)
                .unwrap_or_else(|| Self::asset_pass_rate(a))
        };

        for members in groups.values() {
            let eligible: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| Self::asset_total_n(&assets[i]) >= config.min_samples)
                .collect();
            if eligible.len() < 2 {
                continue;
            }
            let mut sorted = eligible.clone();
            // 降序：决策值高者优先；同分时**根资产（非变体）优先保留**
            // （read_dir 顺序不确定——同分无二级键时 best 可能落到变体上，
            //  导致根契约被误 pruned。MVP-3 实测修复）
            sorted.sort_by(|&a, &b| {
                mu_of(&assets[b])
                    .partial_cmp(&mu_of(&assets[a]))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        match (
                            assets[a].variant_of.is_none(),
                            assets[b].variant_of.is_none(),
                        ) {
                            (true, false) => std::cmp::Ordering::Less,
                            (false, true) => std::cmp::Ordering::Greater,
                            _ => std::cmp::Ordering::Equal,
                        }
                    })
            });
            let best = sorted[0];
            for &candidate in &sorted[1..] {
                let diff = (mu_of(&assets[best]) - mu_of(&assets[candidate])).abs();
                if diff < MERGE_PASS_RATE_DIFF {
                    // 统计并入最优（变体 checks 顺序与原件一致——fork 复制保证）
                    // 先克隆候选统计，避免与最优的可变借用冲突
                    let cand_stats: Vec<CheckStats> = assets[candidate]
                        .checks
                        .iter()
                        .map(|c| c.stats)
                        .collect();
                    for (best_check, cand_stats) in
                        assets[best].checks.iter_mut().zip(cand_stats.iter())
                    {
                        best_check.stats.n += cand_stats.n;
                        best_check.stats.pass_count += cand_stats.pass_count;
                        best_check.stats.cost_sum += cand_stats.cost_sum;
                        best_check.stats.rounds_sum += cand_stats.rounds_sum;
                        best_check.stats.quality_sum += cand_stats.quality_sum;
                    }
                    // V33/MVP-3.5: 贝叶斯后验合并——根吸收候选**采样增量**（去先验伪计数）
                    if config.bayesian_enabled {
                        let cand_prior_alpha =
                            1.0 + config.prior_strength.max(0.0)
                                * assets[candidate].confidence.clamp(0.0, 1.0);
                        let cand_prior_beta = 1.0
                            + config.prior_strength.max(0.0)
                                * (1.0 - assets[candidate].confidence.clamp(0.0, 1.0));
                        if let Ok(Some(mut best_model)) =
                            liluo.load_model(&assets[best].id).await
                        {
                            match liluo.load_model(&assets[candidate].id).await {
                                Ok(Some(cand_model)) => {
                                    best_model.alpha +=
                                        cand_model.alpha - cand_prior_alpha;
                                    best_model.beta += cand_model.beta - cand_prior_beta;
                                    if let Err(e) =
                                        liluo.save_model(&mut best_model).await
                                    {
                                        tracing::warn!(
                                            asset_id = %assets[best].id,
                                            error = %e,
                                            "[merge_variants] posterior merge failed — frequency merge already saved"
                                        );
                                    }
                                }
                                _ => {
                                    // 候选无后验（未采样即被合并）：仅频率合并，贝叶斯跳过
                                    tracing::debug!(
                                        asset_id = %assets[candidate].id,
                                        "[merge_variants] candidate has no posterior — frequency-only merge"
                                    );
                                }
                            }
                        }
                    }
                    assets[candidate].status = "pruned".into();
                    merged += 1;
                    if !changed.contains(&best) {
                        changed.push(best);
                    }
                    if !changed.contains(&candidate) {
                        changed.push(candidate);
                    }
                }
            }
        }
        if !changed.is_empty() {
            for i in changed {
                liluo.save_verification(&mut assets[i]).await?;
            }
        }
        if merged > 0 {
            tracing::info!(merged, "[merge_variants] merged similar variants");
        }
        Ok(merged)
    }

    /// δ-prune：组内 n ≥ min_samples 成员，通过率低于组内最优 > 2σ
    /// （σ = 组内通过率标准差）→ `status = "pruned"`（保留文件供审计，不再加载/回传）。
    async fn prune_variants(
        &self,
        liluo: &LiluoClient,
        config: &DmnConfig,
        posterior: &BTreeMap<String, (f64, f64)>,
    ) -> Result<u64, TaijiError> {
        let mut assets = liluo.load_all_verifications().await?;
        let groups = Self::group_variants(&assets);
        let mut pruned = 0u64;
        // V33/MVP-3.5: 决策值 = 后验均值（空 map → 频率回退）
        let mu_of = |a: &VerificationAsset| {
            posterior
                .get(&a.id)
                .map(|p| p.0)
                .unwrap_or_else(|| Self::asset_pass_rate(a))
        };
        let sigma_of = |a: &VerificationAsset| {
            posterior.get(&a.id).map(|p| p.1).unwrap_or(0.0)
        };

        for members in groups.values() {
            let eligible: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| Self::asset_total_n(&assets[i]) >= config.min_samples)
                .collect();
            if eligible.len() < 2 {
                continue;
            }
            let mus: Vec<f64> = eligible.iter().map(|&i| mu_of(&assets[i])).collect();
            let best = mus.iter().copied().fold(f64::MIN, f64::max);

            if config.bayesian_enabled {
                // 贝叶斯版（§6.4.1）：μ < best − 2·σ(候选自身 Beta 后验)——
                // 低采样 σ 大 → 不易误淘汰；偶然失败不触发 prune。
                for (idx, &i) in eligible.iter().enumerate() {
                    let sigma_cand = sigma_of(&assets[i]);
                    if mus[idx] < best - 2.0 * sigma_cand {
                        assets[i].status = "pruned".into();
                        pruned += 1;
                        tracing::info!(
                            id = %assets[i].id,
                            mu = mus[idx],
                            best_mu = best,
                            sigma_beta = sigma_cand,
                            "[prune_variants] pruned below best−2σ_beta"
                        );
                    }
                }
            } else {
                // 频率版（MVP-3 既有）：组内率标准差 σ，rates < best − 2σ
                let mean = mus.iter().sum::<f64>() / mus.len() as f64;
                let sigma = (mus.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
                    / mus.len() as f64)
                .sqrt();
                if sigma == 0.0 {
                    continue; // 组内无差异，无淘汰对象
                }
                for (idx, &i) in eligible.iter().enumerate() {
                    if mus[idx] < best - 2.0 * sigma {
                        assets[i].status = "pruned".into();
                        pruned += 1;
                        tracing::info!(
                            id = %assets[i].id,
                            rate = mus[idx],
                            best = best,
                            sigma = sigma,
                            "[prune_variants] pruned below 2σ"
                        );
                    }
                }
            }
        }
        if pruned > 0 {
            for asset in assets.iter_mut().filter(|a| a.status == "pruned") {
                liluo.save_verification(asset).await?;
            }
        }
        Ok(pruned)
    }

    /// 变体簇划分：root id（variant_of 指向的根，无 variant_of 资产自身为根）→ 成员索引。
    fn group_variants(assets: &[VerificationAsset]) -> BTreeMap<String, Vec<usize>> {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, a) in assets.iter().enumerate() {
            let root = a.variant_of.clone().unwrap_or_else(|| a.id.clone());
            groups.entry(root).or_default().push(i);
        }
        groups
    }

    /// 资产级通过率（checks 聚合，§8.21「资产级统计由检查项聚合」）。
    fn asset_pass_rate(a: &VerificationAsset) -> f64 {
        Self::stats_pass_rate(&Self::asset_stats(a))
    }

    /// 资产级总采样数。
    fn asset_total_n(a: &VerificationAsset) -> u64 {
        Self::asset_stats(a).n
    }

    /// 聚合 checks → 资产级 CheckStats（与 prompts 单 stats 同构，算子公式单份）。
    fn asset_stats(a: &VerificationAsset) -> CheckStats {
        let mut s = CheckStats::default();
        for c in &a.checks {
            s.n += c.stats.n;
            s.pass_count += c.stats.pass_count;
            s.cost_sum += c.stats.cost_sum;
            s.rounds_sum += c.stats.rounds_sum;
            s.quality_sum += c.stats.quality_sum;
        }
        s
    }

    /// 共享公式：通过率（V35/MVP-6——verifications 与 prompts 同一数学）。
    fn stats_pass_rate(s: &CheckStats) -> f64 {
        if s.n == 0 {
            0.0
        } else {
            s.pass_count as f64 / s.n as f64
        }
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
    async fn test_bayesian_update_persists_posterior() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        let evolver = CognitionEvolver::new(client.clone());

        // 首次：先验映射 confidence=0.8, k=10 → α=1+8=9, β=1+2=3 → μ=0.75
        let conf = evolver.bayesian_update(&client, "m-a", 0, 0, 0.8, 10.0).await.unwrap();
        assert!((conf - 0.75).abs() < 1e-9, "prior-only posterior mean");

        // 更新：+5 成功 → α=14, β=3 → μ=14/17≈0.8235
        let conf = evolver.bayesian_update(&client, "m-a", 5, 0, 0.8, 10.0).await.unwrap();
        assert!((conf - 14.0 / 17.0).abs() < 1e-9);

        // 失败 +1 → α=14, β=4 → μ=14/18≈0.7778
        let conf = evolver.bayesian_update(&client, "m-a", 0, 1, 0.8, 10.0).await.unwrap();
        assert!((conf - 14.0 / 18.0).abs() < 1e-9);

        // 持久化断言：version++ + 回读一致
        let model = client.load_model("m-a").await.unwrap().expect("model persisted");
        assert!((model.posterior_mean() - 14.0 / 18.0).abs() < 1e-9);
        assert!(model.header.version >= 3, "version increments per save: {}", model.header.version);

        // σ_beta 计算：σ = √(αβ/((α+β)²·(α+β+1)))
        let expected_sigma = (14.0_f64 * 4.0 / (18.0 * 18.0 * 19.0)).sqrt();
        assert!((model.posterior_sigma() - expected_sigma).abs() < 1e-12);

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
        // V44：根级资产树目录（yang/ + yin/ + models/ 等）由 new 创建，允许存在。
        let mut dir_exists = tokio::fs::read_dir(&dir).await.unwrap();
        let mut assets: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = dir_exists.next_entry().await {
            assets.push(entry.file_name().to_string_lossy().to_string());
        }
        assert!(
            assets.iter().all(|n| {
                // V44：不再创建 truths/ 与 index.yaml；不再有 {model_key}/ 分区
                n == "models"
                    || n == "yang"
                    || n == "yin"
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
            ..Default::default()
        };
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: EvolutionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.pruned, 3);
        assert_eq!(deserialized.confidence_delta, 0.42);
    }

    // ── V33/MVP-2: backprop_checks（检查项统计回传 — BCP §6.4/§8.23）──

    #[tokio::test]
    async fn test_backprop_checks_updates_stats_and_version() {
        use crate::types::agent::VerificationAsset;
        use crate::types::verification::{
            CheckKind, CheckResult, CheckSeverity, CheckSpec,
        };

        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());

        // 种一棵带两个检查项的契约资产
        let mut v = VerificationAsset::new(
            "v-test",
            "测试契约",
            "test",
            "contract",
            vec![
                CheckSpec {
                    id: "check-a".into(),
                    kind: CheckKind::FileExists,
                    target: "deliverables/out.md".into(),
                    params: serde_json::json!({}),
                    severity: CheckSeverity::Hard,
                    pass_condition: "p".into(),
                    stats: Default::default(),
                },
                CheckSpec {
                    id: "check-b".into(),
                    kind: CheckKind::LlmJudgement,
                    target: "deliverables".into(),
                    params: serde_json::json!({}),
                    severity: CheckSeverity::Hard,
                    pass_condition: "p".into(),
                    stats: Default::default(),
                },
            ],
            vec!["general".into()],
        );
        client.save_verification(&mut v).await.unwrap();
        let version_before = v.version;

        let evolver = CognitionEvolver::new(client.clone());

        // 任务 1：check-a 通过 + check-b 通过 + ghost（未匹配）
        let checks_task1 = vec![
            CheckResult {
                check_id: "check-a".into(),
                kind: CheckKind::FileExists,
                passed: true,
                detail: "ok".into(),
                duration_ms: 1,
                cost_tokens: 0,
                verify_rounds: 0,
                quality: 0.0,
            },
            CheckResult {
                check_id: "check-b".into(),
                kind: CheckKind::LlmJudgement,
                passed: true,
                detail: "ok".into(),
                duration_ms: 1,
                cost_tokens: 0,
                verify_rounds: 0,
                quality: 0.0,
            },
            // 未匹配：应跳过（warn）不计入 updated
            CheckResult {
                check_id: "ghost".into(),
                kind: CheckKind::FileExists,
                passed: true,
                detail: "ok".into(),
                duration_ms: 1,
                cost_tokens: 0,
                verify_rounds: 0,
                quality: 0.0,
            },
        ];
        let updated = evolver.backprop_checks("task-1", &checks_task1, &DmnConfig::default(), None).await.unwrap();
        assert_eq!(updated, 2, "ghost check must be skipped");

        // 任务 2：check-a 失败（跨任务累加）
        let checks_task2 = vec![CheckResult {
            check_id: "check-a".into(),
            kind: CheckKind::FileExists,
            passed: false,
            detail: "missing".into(),
            duration_ms: 1,
            cost_tokens: 0,
            verify_rounds: 0,
            quality: 0.0,
        }];
        let updated = evolver.backprop_checks("task-2", &checks_task2, &DmnConfig::default(), None).await.unwrap();
        assert_eq!(updated, 1);

        // 持久化断言：stats 跨任务累加 + version++
        let loaded = client
            .load_verification("v-test")
            .await
            .unwrap()
            .expect("asset should exist");
        assert!(loaded.version > version_before, "version must increment");
        let a = loaded.checks.iter().find(|c| c.id == "check-a").unwrap();
        assert_eq!(a.stats.n, 2);
        assert_eq!(a.stats.pass_count, 1);
        assert!((a.stats.pass_rate() - 0.5).abs() < 1e-9);
        let b = loaded.checks.iter().find(|c| c.id == "check-b").unwrap();
        assert_eq!(b.stats.n, 1);
        assert_eq!(b.stats.pass_count, 1);

        // 空 checks：no-op
        let updated = evolver.backprop_checks("task-2", &[], &DmnConfig::default(), None).await.unwrap();
        assert_eq!(updated, 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ── V33/MVP-3: MCTS 契约演化（四算子 — BCP §6.4/§8.21）──

    fn mk_asset(
        id: &str,
        n: u64,
        pass_count: u64,
        kind: CheckKind,
        variant_of: Option<&str>,
    ) -> VerificationAsset {
        use crate::types::verification::{CheckSeverity, CheckSpec};
        let mut a = VerificationAsset::new(
            id,
            id,
            "test",
            "test",
            vec![CheckSpec {
                id: format!("check-{id}"),
                kind,
                target: "deliverables/out.md".into(),
                params: serde_json::json!({}),
                severity: CheckSeverity::Hard,
                pass_condition: "p".into(),
                stats: CheckStats {
                    n,
                    pass_count,
                    ..Default::default()
                },
            }],
            vec!["general".into()],
        );
        a.variant_of = variant_of.map(|v| v.to_string());
        a
    }

    #[tokio::test]
    async fn test_evolve_contracts_activation_gate_blocks() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 仅 4 资产（<5）：门槛不过，零演化
        for i in 0..4 {
            let mut a = mk_asset(
                &format!("low-{i}"),
                3,
                1,
                CheckKind::LlmJudgement,
                None,
            );
            client.save_verification(&mut a).await.unwrap();
        }
        let evolver = CognitionEvolver::new(client.clone());
        let config = DmnConfig::default();
        let report = evolver.evolve_contracts(&config, None).await.unwrap();
        assert_eq!(report.forked, 0);
        assert_eq!(report.merged, 0);
        assert_eq!(report.pruned, 0);
        let loaded = client.load_all_verifications().await.unwrap();
        assert_eq!(loaded.len(), 4, "no fork without gate");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_fork_variants_creates_strict_variant() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 5 资产、总采样 50：低回报根（0.4）+ 4 陪衬
        let mut low = mk_asset("root-low", 10, 4, CheckKind::LlmJudgement, None);
        client.save_verification(&mut low).await.unwrap();
        for i in 0..4 {
            let mut a = mk_asset(&format!("peer-{i}"), 10, 9, CheckKind::FileExists, None);
            client.save_verification(&mut a).await.unwrap();
        }
        let evolver = CognitionEvolver::new(client.clone());
        let report = evolver.evolve_contracts(&DmnConfig::default(), None).await.unwrap();
        assert_eq!(report.forked, 1, "one low-reward root should fork");

        let loaded = client.load_all_verifications().await.unwrap();
        let variant = loaded.iter().find(|a| a.variant_of.is_some()).expect("variant exists");
        assert_eq!(variant.variant_of.as_deref(), Some("root-low"));
        assert_eq!(variant.id, "root-low-v1");
        // check id 重命名 + strictness 档位 + stats 清零 + 降权
        let check = variant.checks.iter().find(|c| c.kind == CheckKind::LlmJudgement).unwrap();
        assert_eq!(check.id, format!("check-root-low@{}", variant.id));
        assert_eq!(check.params["strictness"], "strict");
        assert_eq!(check.stats.n, 0, "variant stats must reset");
        assert!((variant.confidence - low.confidence * 0.8).abs() < 1e-9);

        // 幂等：再跑一轮不再 fork（已有变体）
        let report2 = evolver.evolve_contracts(&DmnConfig::default(), None).await.unwrap();
        assert_eq!(report2.forked, 0, "no duplicate fork");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_merge_variants_merges_similar_and_prunes() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 组 A：根 0.8 + v1 0.8（差 0 → 合并）+ v2 0.5（差 0.3 → 保留）；陪衬 B/C 自成组
        let mut a = mk_asset("grp-a", 10, 8, CheckKind::FileExists, None);
        client.save_verification(&mut a).await.unwrap();
        let mut v1 = mk_asset("grp-a-v1", 5, 4, CheckKind::FileExists, Some("grp-a"));
        client.save_verification(&mut v1).await.unwrap();
        let mut v2 = mk_asset("grp-a-v2", 6, 3, CheckKind::FileExists, Some("grp-a"));
        client.save_verification(&mut v2).await.unwrap();
        let mut b = mk_asset("grp-b", 15, 13, CheckKind::FileExists, None);
        client.save_verification(&mut b).await.unwrap();
        let mut c = mk_asset("grp-c", 15, 13, CheckKind::FileExists, None);
        client.save_verification(&mut c).await.unwrap();
        let evolver = CognitionEvolver::new(client.clone());
        // 频率路径（MVP-3 断言基线）：贝叶斯关闭
        let mut freq_cfg = DmnConfig::default();
        freq_cfg.bayesian_enabled = false;
        let report = evolver.evolve_contracts(&freq_cfg, None).await.unwrap();
        assert_eq!(report.merged, 1, "v1 (same rate) merged into root");

        let loaded = client.load_all_verifications().await.unwrap();
        let root = loaded.iter().find(|a| a.id == "grp-a").unwrap();
        let root_check = &root.checks[0];
        assert_eq!(root_check.stats.n, 15, "root absorbs v1 stats");
        assert_eq!(root_check.stats.pass_count, 12);
        // pruned 资产被 load_all 过滤（设计）；按 id 直读断言状态
        let v1_loaded = client.load_verification("grp-a-v1").await.unwrap().expect("v1 file retained for audit");
        assert_eq!(v1_loaded.status, "pruned");
        let v2_loaded = loaded.iter().find(|a| a.id == "grp-a-v2").unwrap();
        assert_eq!(v2_loaded.status, "active", "v2 (0.3 diff) kept");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_prune_variants_below_two_sigma() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 组 A：0.9 / 0.25（< best−2σ → prune）/ 0.8；陪衬 B/C 单成员组
        let mut a = mk_asset("sig-a", 10, 9, CheckKind::FileExists, None);
        client.save_verification(&mut a).await.unwrap();
        let mut v1 = mk_asset("sig-a-v1", 8, 2, CheckKind::FileExists, Some("sig-a"));
        client.save_verification(&mut v1).await.unwrap();
        // v2 rate 0.6：与 best(0.9) 差 0.3——远离 merge 浮点边界（0.9−0.8 的浮点差可能 <0.1 误合并）
        let mut v2 = mk_asset("sig-a-v2", 5, 3, CheckKind::FileExists, Some("sig-a"));
        client.save_verification(&mut v2).await.unwrap();
        let mut b = mk_asset("sig-b", 15, 13, CheckKind::FileExists, None);
        client.save_verification(&mut b).await.unwrap();
        let mut c = mk_asset("sig-c", 15, 13, CheckKind::FileExists, None);
        client.save_verification(&mut c).await.unwrap();
        let evolver = CognitionEvolver::new(client.clone());
        // 频率路径（MVP-3 断言基线）：贝叶斯关闭
        let mut freq_cfg = DmnConfig::default();
        freq_cfg.bayesian_enabled = false;
        let report = evolver.evolve_contracts(&freq_cfg, None).await.unwrap();
        assert_eq!(report.pruned, 1, "worst variant pruned below 2σ");

        let loaded = client.load_all_verifications().await.unwrap();
        let v1_loaded = client.load_verification("sig-a-v1").await.unwrap().expect("v1 file retained for audit");
        assert_eq!(v1_loaded.status, "pruned");
        let v2_loaded = loaded.iter().find(|a| a.id == "sig-a-v2").unwrap();
        assert_eq!(v2_loaded.status, "active");
        // pruned 资产不再被加载（load_all_verifications 过滤）
        assert_eq!(loaded.len(), 4, "pruned excluded from active load");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_backprop_variant_check_id_targets_variant_only() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 根 + 变体（check id 重命名 `{base}@{variant}`）
        let mut a = mk_asset("vp-a", 5, 4, CheckKind::FileExists, None);
        client.save_verification(&mut a).await.unwrap();
        let mut v1 = mk_asset("vp-a-v1", 0, 0, CheckKind::FileExists, Some("vp-a"));
        v1.checks[0].id = "check-vp-a@vp-a-v1".into();
        client.save_verification(&mut v1).await.unwrap();
        let evolver = CognitionEvolver::new(client.clone());

        // 变体回传：全 id 精确落位变体，原资产零污染
        let checks = vec![CheckResult {
            check_id: "check-vp-a@vp-a-v1".into(),
            kind: CheckKind::FileExists,
            passed: true,
            detail: "ok".into(),
            duration_ms: 1,
            cost_tokens: 500,
            verify_rounds: 2,
            quality: 0.9,
        }];
        let updated = evolver.backprop_checks("task-v", &checks, &DmnConfig::default(), None).await.unwrap();
        assert_eq!(updated, 1);
        let loaded = client.load_all_verifications().await.unwrap();
        let root = loaded.iter().find(|a| a.id == "vp-a").unwrap();
        assert_eq!(root.checks[0].stats.n, 5, "root stats untouched");
        let variant = loaded.iter().find(|a| a.id == "vp-a-v1").unwrap();
        assert_eq!(variant.checks[0].stats.n, 1);
        assert_eq!(variant.checks[0].stats.pass_count, 1);
        assert_eq!(variant.checks[0].stats.cost_sum, 500);
        assert_eq!(variant.checks[0].stats.rounds_sum, 2);
        assert!((variant.checks[0].stats.quality_sum - 0.9).abs() < 1e-9);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}

#[cfg(test)]
mod bayesian_tests {
    use super::*;
    use crate::infra::knowledge::LiluoClient;
    use crate::types::verification::{CheckSeverity, CheckSpec};

    /// 临时目录唯一性（§16：pid + 时间戳 + 模块前缀）。
    async fn test_knowledge_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "taiji_knowledge_bayes_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    fn mk_b(id: &str, n: u64, pass_count: u64, confidence: f64, variant_of: Option<&str>) -> VerificationAsset {
        let mut a = VerificationAsset::new(
            id, id, "test", "test",
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
        a.confidence = confidence;
        a.variant_of = variant_of.map(|v| v.to_string());
        a
    }

    /// V33/MVP-3.5：backprop 双轨——同一次回传 CheckStats（频率）与 ModelAsset（贝叶斯）同时更新；
    /// bayesian_enabled=false → 仅频率。
    #[tokio::test]
    async fn backprop_writes_frequency_and_bayesian_dual_track() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        let mut a = mk_b("dual-a", 0, 0, 0.8, None);
        client.save_verification(&mut a).await.unwrap();
        let evolver = CognitionEvolver::new(client.clone());
        let config = DmnConfig::default(); // bayesian_enabled = true

        let checks = vec![CheckResult {
            check_id: "check-dual-a".into(),
            kind: CheckKind::FileExists,
            passed: true,
            detail: "ok".into(),
            duration_ms: 1,
            cost_tokens: 100,
            verify_rounds: 1,
            quality: 0.9,
        }];
        let updated = evolver.backprop_checks("t1", &checks, &config, None).await.unwrap();
        assert_eq!(updated, 1);

        // 频率视图
        let v = client.load_verification("dual-a").await.unwrap().unwrap();
        assert_eq!(v.checks[0].stats.n, 1);
        assert_eq!(v.checks[0].stats.cost_sum, 100);
        // 贝叶斯视图：先验(0.8,k=10)→α=9,β=3；+1 成功 → α=10,β=3 → μ=10/13
        let m = client.load_model("dual-a").await.unwrap().expect("model written");
        assert!((m.posterior_mean() - 10.0 / 13.0).abs() < 1e-9);

        // 开关关：仅频率（模型不被第二次更新）
        let mut off = DmnConfig::default();
        off.bayesian_enabled = false;
        let checks2 = vec![CheckResult {
            check_id: "check-dual-a".into(),
            kind: CheckKind::FileExists,
            passed: false,
            detail: "missing".into(),
            duration_ms: 1,
            cost_tokens: 0,
            verify_rounds: 0,
            quality: 0.0,
        }];
        let _ = evolver.backprop_checks("t2", &checks2, &off, None).await.unwrap();
        let v2 = client.load_verification("dual-a").await.unwrap().unwrap();
        assert_eq!(v2.checks[0].stats.n, 2);
        let m2 = client.load_model("dual-a").await.unwrap().unwrap();
        assert!((m2.posterior_mean() - 10.0 / 13.0).abs() < 1e-9, "bayesian skipped when disabled");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// 模拟 backprop 双轨后的状态：stats（频率）+ ModelAsset（贝叶斯）同时存在。
    async fn seed_model(client: &crate::infra::knowledge::LiluoClient, id: &str, n: u64, pass: u64, confidence: f64) {
        let mut m = crate::infra::knowledge::ModelAsset::from_prior(id, id, confidence, 10.0);
        m.alpha += pass as f64;
        m.beta += (n - pass) as f64;
        client.save_model(&mut m).await.unwrap();
    }

    /// V33/MVP-3.5 价值实证：低采样偶然失败（n=3 全败 + 高先验）——\n    /// 频率版误淘汰（rate=0.0 < best−2σ_group），贝叶斯版保留（μ 收缩向先验 + σ_beta 大）。
    #[tokio::test]
    async fn bayesian_prune_keeps_low_sample_variant_frequency_prunes_it() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 组 b-root：root(30/28, c=0.5) + v1(2/0, c=0.9 高先验) + v2(8/7, c=0.5)；陪衬 ×2 凑门槛
        let mut root = mk_b("b-root", 30, 28, 0.5, None);
        client.save_verification(&mut root).await.unwrap();
        let mut v1 = mk_b("b-root-v1", 3, 0, 0.9, Some("b-root"));
        client.save_verification(&mut v1).await.unwrap();
        let mut v2 = mk_b("b-root-v2", 5, 4, 0.5, Some("b-root"));
        client.save_verification(&mut v2).await.unwrap();
        for i in 0..2 {
            let mut p = mk_b(&format!("b-peer-{i}"), 10, 9, 0.5, None);
            client.save_verification(&mut p).await.unwrap();
        }
        let evolver = CognitionEvolver::new(client.clone());

        // 频率版（bayesian_enabled=false）：v1 rate=0.0 低于 best−2σ_group → 淘汰
        let mut freq_cfg = DmnConfig::default();
        freq_cfg.bayesian_enabled = false;
        let freq_report = evolver.evolve_contracts(&freq_cfg, None).await.unwrap();
        assert_eq!(freq_report.pruned, 1, "frequency: low-sample zero-rate variant pruned");

        // 重跑知识库（v1 已被 pruned——重建）
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let client2 = Arc::new(LiluoClient::new(&dir).await.unwrap());
        let mut root2 = mk_b("b-root", 30, 28, 0.5, None);
        client2.save_verification(&mut root2).await.unwrap();
        let mut v1b = mk_b("b-root-v1", 3, 0, 0.9, Some("b-root"));
        client2.save_verification(&mut v1b).await.unwrap();
        let mut v2b = mk_b("b-root-v2", 5, 4, 0.5, Some("b-root"));
        client2.save_verification(&mut v2b).await.unwrap();
        for i in 0..2 {
            let mut p = mk_b(&format!("b-peer-{i}"), 10, 9, 0.5, None);
            client2.save_verification(&mut p).await.unwrap();
        }
        // 模拟 backprop 双轨：stats 与 ModelAsset 同步存在（真实流程保证）
        seed_model(&client2, "b-root", 30, 28, 0.5).await;
        seed_model(&client2, "b-root-v1", 3, 0, 0.9).await;
        seed_model(&client2, "b-root-v2", 5, 4, 0.5).await;
        for i in 0..2 {
            seed_model(&client2, &format!("b-peer-{i}"), 10, 9, 0.5).await;
        }
        let evolver2 = CognitionEvolver::new(client2.clone());

        // 贝叶斯版（默认开）：v1 μ=0.714（高先验收缩）> best−2·σ_beta(0.117) → 保留
        let bayes_report = evolver2.evolve_contracts(&DmnConfig::default(), None).await.unwrap();
        assert_eq!(bayes_report.pruned, 0, "bayesian: low-sample variant protected by prior+σ_beta");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}

#[cfg(test)]
mod prompt_evolution_tests {
    use super::*;
    use crate::infra::knowledge::LiluoClient;
    use crate::types::agent::{AssetRef, PromptAsset};

    async fn test_knowledge_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "taiji_knowledge_prompts_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    async fn mk_p_async(client: &LiluoClient, id: &str, confidence: f64, n: u64, pass: u64) {
        let mut p = PromptAsset::new(id, id, "t", "t", "FittingAgent", vec!["x".into()]);
        p.confidence = confidence;
        p.stats.n = n;
        p.stats.pass_count = pass;
        client.save_prompt(&mut p).await.unwrap();
    }

    /// V35/MVP-6：任务级回传——stats 四维 + usage_count 同步 + 贝叶斯 α 递增。
    #[tokio::test]
    async fn backprop_prompts_updates_task_level_stats_and_bayes() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        mk_p_async(&client, "pp-a", 0.8, 0, 0).await;
        let evolver = CognitionEvolver::new(client.clone());
        let config = DmnConfig::default(); // bayesian_enabled = true

        let checks = vec![CheckResult {
            check_id: "check-x".into(),
            kind: CheckKind::FileExists,
            passed: true,
            detail: "ok".into(),
            duration_ms: 1,
            cost_tokens: 42,
            verify_rounds: 2,
            quality: 0.9,
        }];
        let assets = vec![AssetRef::new("prompt", "pp-a")];
        let updated = evolver
            .backprop_prompts("t1", &assets, true, &checks, &config, None)
            .await
            .unwrap();
        assert_eq!(updated, 1);

        let p = client.load_prompt("pp-a").await.unwrap().unwrap();
        assert_eq!(p.stats.n, 1);
        assert_eq!(p.stats.pass_count, 1);
        assert_eq!(p.stats.cost_sum, 42);
        assert_eq!(p.stats.rounds_sum, 2);
        assert!((p.stats.quality_sum - 0.9).abs() < 1e-9);
        assert_eq!(p.usage_count, 1, "legacy field synced");
        assert!((p.success_rate - 1.0).abs() < 1e-9);
        // 贝叶斯：先验(0.8,k=10)→α=9,β=3；+1 成功 → α=10 → μ=10/13
        let m = client.load_model("pp-a").await.unwrap().expect("model written");
        assert!((m.posterior_mean() - 10.0 / 13.0).abs() < 1e-9);

        // 失败任务：pass_count 不变，β 递增
        let checks2 = vec![];
        let updated = evolver
            .backprop_prompts("t2", &assets, false, &checks2, &config, None)
            .await
            .unwrap();
        assert_eq!(updated, 1);
        let p2 = client.load_prompt("pp-a").await.unwrap().unwrap();
        assert_eq!(p2.stats.n, 2);
        assert_eq!(p2.stats.pass_count, 1);
        assert!((p2.success_rate - 0.5).abs() < 1e-9);
        let m2 = client.load_model("pp-a").await.unwrap().unwrap();
        assert!((m2.posterior_mean() - 10.0 / 14.0).abs() < 1e-9, "β incremented on fail");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V35/MVP-6：fork 对称——低决策值根资产 → 变体（confidence×0.8 + stats 清零 +
    /// ModelAsset 独立初始化）。
    #[tokio::test]
    async fn fork_prompts_creates_variant_on_low_mu() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 根：n=5 pass=1（频率 0.2 < 0.6）→ fork；陪衬根：高通过 → 不 fork
        mk_p_async(&client, "pf-low", 0.5, 5, 1).await;
        mk_p_async(&client, "pf-high", 0.5, 5, 5).await;
        let evolver = CognitionEvolver::new(client.clone());
        let config = DmnConfig::default();
        let posterior = BTreeMap::new(); // 频率回退

        let forked = evolver.fork_prompts(&client, &config, &posterior).await.unwrap();
        assert_eq!(forked, 1);

        let v = client.load_prompt("pf-low-v1").await.unwrap().expect("variant written");
        assert_eq!(v.variant_of.as_deref(), Some("pf-low"));
        assert!((v.confidence - 0.4).abs() < 1e-9, "confidence × 0.8");
        assert_eq!(v.stats.n, 0, "stats cleared");
        // 变体 ModelAsset 独立初始化（先验 confidence=0.4,k=10 → μ=(1+4)/12=5/12）
        let m = client.load_model("pf-low-v1").await.unwrap().unwrap();
        assert!((m.posterior_mean() - 5.0 / 12.0).abs() < 1e-9);
        // 防重复：再跑一次不产生新变体
        let again = evolver.fork_prompts(&client, &config, &posterior).await.unwrap();
        assert_eq!(again, 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V35/MVP-6：merge/prune 对称——相似变体统计并入根（同分根优先）、
    /// 低后验变体淘汰（保留文件供审计，load_all_prompts 过滤）。
    #[tokio::test]
    async fn merge_and_prune_prompts_symmetric() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        // 组 pv-root：root(n=10 pass=9) + v1(n=3 pass=3, 差 0.1 → 合并)
        mk_p_async(&client, "pv-root", 0.5, 10, 9).await;
        let mut v1 = PromptAsset::new("pv-root-v1", "pv-root-v1", "t", "t", "FittingAgent", vec!["x".into()]);
        v1.variant_of = Some("pv-root".into());
        v1.confidence = 0.4;
        v1.stats.n = 3;
        v1.stats.pass_count = 3;
        client.save_prompt(&mut v1).await.unwrap();
        // 组独立根（门槛陪衬）
        mk_p_async(&client, "pv-other", 0.5, 5, 4).await;
        let evolver = CognitionEvolver::new(client.clone());
        let config = DmnConfig::default();
        let posterior = BTreeMap::new(); // 频率路径（远离浮点边界：9/10=0.9 vs 1.0 差 0.1——用 < 0.1 判定，0.1 不触发）
        let _ = evolver;

        // merge：root 0.9 vs v1 1.0 → diff 0.1，非 < 0.1 → 不合并（浮点边界陷阱，
        // 测试数据远离边界：v1 改用 n=2 pass=2 → 1.0，仍差 0.1）。
        // 调整：v1 与 root 差必须 < 0.1——root n=10 pass=9=0.9，v1 n=1 pass=1=1.0 差 0.1 不触发。
        // 构造：root n=10 pass=10=1.0，v1 n=2 pass=2=1.0 → 差 0 → 合并。
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let client2 = Arc::new(LiluoClient::new(&dir).await.unwrap());
        mk_p_async(&client2, "pv-root", 0.5, 10, 10).await;
        let mut v1b = PromptAsset::new("pv-root-v1", "pv-root-v1", "t", "t", "FittingAgent", vec!["x".into()]);
        v1b.variant_of = Some("pv-root".into());
        v1b.confidence = 0.4;
        v1b.stats.n = 3;
        v1b.stats.pass_count = 3;
        client2.save_prompt(&mut v1b).await.unwrap();
        mk_p_async(&client2, "pv-other", 0.5, 5, 4).await;
        let evolver2 = CognitionEvolver::new(client2.clone());

        let merged = evolver2.merge_prompts(&client2, &config, &posterior).await.unwrap();
        assert_eq!(merged, 1, "identical-rate variant merged into root");
        let root = client2.load_prompt("pv-root").await.unwrap().unwrap();
        assert_eq!(root.stats.n, 13, "stats absorbed");
        let v1_after = client2.load_prompt("pv-root-v1").await.unwrap().unwrap();
        assert_eq!(v1_after.status, "pruned", "candidate pruned (kept on disk)");

        // prune：v2 低采样全败 + 低先验 → 贝叶斯版 μ 低于 root − 2σ → 淘汰
        let mut v2 = PromptAsset::new("pv-root-v2", "pv-root-v2", "t", "t", "FittingAgent", vec!["x".into()]);
        v2.variant_of = Some("pv-root".into());
        v2.confidence = 0.1; // 低先验：μ=(1+1)/(2+10)=2/12≈0.167
        v2.stats.n = 3;
        v2.stats.pass_count = 0;
        client2.save_prompt(&mut v2).await.unwrap();
        let posterior2 = {
            // 手工构造后验（模拟采样后）：root 高 μ，v2 低 μ
            let mut m = BTreeMap::new();
            m.insert("pv-root".to_string(), (0.9, 0.05));
            m.insert("pv-root-v2".to_string(), (0.167, 0.09));
            m
        };
        let pruned = evolver2.prune_prompts(&client2, &config, &posterior2).await.unwrap();
        assert_eq!(pruned, 1, "low-posterior variant pruned");
        let v2_after = client2.load_prompt("pv-root-v2").await.unwrap().unwrap();
        assert_eq!(v2_after.status, "pruned");

        // load_all_prompts 过滤 pruned
        let all = client2.load_all_prompts().await.unwrap();
        assert!(!all.iter().any(|p| p.id == "pv-root-v2"), "pruned excluded from loading");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V35/MVP-6：bayesian_enabled=false → 频率路径（merge 判定不变，无模型写入）。
    #[tokio::test]
    async fn backprop_prompts_bayesian_disabled_frequency_only() {
        let dir = test_knowledge_dir().await;
        let client = Arc::new(LiluoClient::new(&dir).await.unwrap());
        mk_p_async(&client, "pf-a", 0.8, 0, 0).await;
        let evolver = CognitionEvolver::new(client.clone());
        let mut config = DmnConfig::default();
        config.bayesian_enabled = false;

        let assets = vec![AssetRef::new("prompt", "pf-a")];
        let _ = evolver
            .backprop_prompts("t1", &assets, true, &[], &config, None)
            .await
            .unwrap();
        let p = client.load_prompt("pf-a").await.unwrap().unwrap();
        assert_eq!(p.stats.n, 1);
        assert!(client.load_model("pf-a").await.unwrap().is_none(), "no bayesian write when disabled");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
