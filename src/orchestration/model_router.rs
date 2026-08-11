//! ModelRouter — 元权重模型路由（V36 落地，BCP §8.8 第 1 步）。
//!
//! 纯符号层 bandit/UCB：读根级 model_stats 元权重表 → 候选 UCB 决策 model_key
//! → MetaAgent 按路由结果分区检索（§6.1 分区一致性）。**无 LLM 调用**——路由
//! 先于分区检索（V32 plan.md 阻塞点 #1 修正：分区检索依赖路由结果，而路由是
//! 读 model_stats 的符号决策，不需要 LLM）。
//!
//! 候选 = 配置 providers × models（`ProviderRegistry::model_keys`，default 在前）。
//! score = avg_reward + C·√(ln N_total / (n+1))：
//! - avg_reward = w_pass·pass_rate + w_quality·avg_quality − w_cost·avg_cost_norm
//!   − w_rounds·avg_rounds（成本组内归一化——绝对 token 数跨模型不可比）
//! - (n+1) 平滑：n=0 冷启动候选有有限探索分（无统计 → μ=0，探索分 = C·√ln N_total）
//! - **全部无统计（N_total=0）→ 配置默认模型**（探索由首次采样开启）
//! - tie → 候选声明顺序（default 在前，确定性）
//!
//! 模型级 quality/rounds 维度：pending 仅 PASS 入队 → quality 恒 1.0、rounds 取
//! checks 首项摊派（BCP §6.4 元权重表注记）。

use std::collections::BTreeMap;

use crate::infra::error::TaijiError;
use crate::infra::knowledge::LiluoClient;
use crate::infra::provider::ProviderRegistry;
use crate::types::agent::{ModelKey, ModelStatsRow};
use crate::types::verification::RewardWeights;

/// UCB 探索常数（与 §6.3 UCB 检索同值）。
pub const ROUTER_UCB_C: f64 = 1.414;

/// 模型路由决策器（纯符号层，无 IO 状态——stats 由调用方加载）。
pub struct ModelRouter {
    /// model_key → StatsRow（根级 model_stats）。
    stats: BTreeMap<String, ModelStatsRow>,
    /// 候选 ModelKey（default 在前）。
    candidates: Vec<ModelKey>,
    /// 配置默认模型键。
    default_key: ModelKey,
    /// 回报权重（§6.4 默认 0.5/0.3/0.2/0.1）。
    weights: RewardWeights,
    /// UCB 探索常数。
    ucb_c: f64,
}

impl ModelRouter {
    /// 构建路由决策器。`stats` 来自 `LiluoClient::load_model_stats`（根级）。
    pub fn new(providers: &ProviderRegistry, stats: BTreeMap<String, ModelStatsRow>) -> Self {
        let candidates = providers.model_keys();
        let default_key = providers.default_model_key();
        Self {
            stats,
            candidates,
            default_key,
            weights: RewardWeights::default(),
            ucb_c: ROUTER_UCB_C,
        }
    }

    /// 决策 model_key（纯符号层）。
    ///
    /// 全部无统计 → 配置默认；否则 UCB 最大化 avg_reward + 探索项。
    pub fn route(&self) -> ModelKey {
        let n_total: u64 = self.stats.values().map(|r| r.n).sum();
        if n_total == 0 || self.candidates.is_empty() {
            return self.default_key.clone();
        }

        // 组内最大平均成本（归一化基准——绝对 token 数跨模型不可比）。
        let max_cost = self
            .stats
            .values()
            .map(|r| r.avg_cost())
            .fold(0.0_f64, f64::max);

        let mut best: Option<(f64, &ModelKey)> = None;
        for key in &self.candidates {
            let row = self.stats.get(key.key()).cloned().unwrap_or_default();
            let n = row.n;
            let cost_norm = if max_cost > 0.0 {
                row.avg_cost() / max_cost
            } else {
                0.0
            };
            let avg_reward = self.weights.pass * row.pass_rate()
                + self.weights.quality * row.avg_quality()
                - self.weights.cost * cost_norm
                - self.weights.rounds * row.avg_rounds();
            // (n+1) 平滑：n=0 冷启动候选有有限探索分（UCB1，§6.3 同公式）。
            let explore = self.ucb_c * ((n_total as f64).ln() / (n as f64 + 1.0)).sqrt();
            let score = avg_reward + explore;
            if best.as_ref().is_none_or(|(bs, _)| score > *bs) {
                best = Some((score, key));
            }
        }

        best.map(|(_, k)| k.clone())
            .unwrap_or_else(|| self.default_key.clone())
    }

    /// V37 异源裁判决策（BCP §8.8 相位级，MVP 边界：复用任务级 stats，
    /// (model_key × tag × phase) 相位维度后置）。
    ///
    /// 从**非主模型候选**中按 UCB 同公式（avg_reward + C·√(ln N_total/(n+1))）
    /// 选验证模型——裁判 ≠ 运动员（§1.3 self-preference / position 偏置对抗）。
    /// 候选数 < 2 → `None`（无源可异，调用方 warn 降级 = 继承主模型）。
    /// 异源候选全无统计 → 冷启动探索分最大的非主候选（有界）。
    pub fn route_verifier(&self, exec_key: &ModelKey) -> Option<ModelKey> {
        let others: Vec<&ModelKey> = self
            .candidates
            .iter()
            .filter(|k| k.key() != exec_key.key())
            .collect();
        if others.is_empty() {
            return None;
        }
        let n_total: u64 = self.stats.values().map(|r| r.n).sum();
        if n_total == 0 {
            // 全冷启动：异源按候选声明顺序（确定性）取第一个非主候选。
            return others.first().map(|k| (*k).clone());
        }
        let max_cost = self
            .stats
            .values()
            .map(|r| r.avg_cost())
            .fold(0.0_f64, f64::max);
        let mut best: Option<(f64, &ModelKey)> = None;
        for key in others {
            let row = self.stats.get(key.key()).cloned().unwrap_or_default();
            let n = row.n;
            let cost_norm = if max_cost > 0.0 {
                row.avg_cost() / max_cost
            } else {
                0.0
            };
            let avg_reward = self.weights.pass * row.pass_rate()
                + self.weights.quality * row.avg_quality()
                - self.weights.cost * cost_norm
                - self.weights.rounds * row.avg_rounds();
            let explore =
                self.ucb_c * ((n_total as f64).ln() / (n as f64 + 1.0)).sqrt();
            let score = avg_reward + explore;
            if best.as_ref().is_none_or(|(bs, _)| score > *bs) {
                best = Some((score, key));
            }
        }
        best.map(|(_, k)| k.clone())
    }
}

/// 模型级回传信号（BCP §6.4 元权重表）——来自 pending 负载聚合。
#[derive(Debug, Clone, Copy)]
pub struct ModelStatsSignal {
    /// 任务 PASS（pending 仅 PASS 入队 → 恒 true；字段保留扩展）。
    pub passed: bool,
    /// token 成本（checks 首项 cost_tokens——同任务摊派值一致）。
    pub cost_tokens: u64,
    /// 验证轮数（checks 首项 verify_rounds）。
    pub verify_rounds: u32,
    /// 质量分（任务级 passed 映射：PASS=1.0）。
    pub quality: f64,
}

/// 累加一次任务级信号到根级 model_stats.yaml（n++ 等），原子写。
///
/// DMN 单写者调用（§8.3）；失败上抛（调用方决定 warn 或进死信——回传是
/// 增强层，不阻断 backprop 主流程）。
pub async fn update_model_stats(
    liluo: &LiluoClient,
    model_key: &ModelKey,
    signal: &ModelStatsSignal,
) -> Result<(), TaijiError> {
    let mut stats = liluo.load_model_stats().await?;
    let row = stats.entry(model_key.key().to_string()).or_default();
    row.n += 1;
    row.pass_count += u64::from(signal.passed);
    row.cost_sum += signal.cost_tokens;
    row.rounds_sum += u64::from(signal.verify_rounds);
    row.quality_sum += signal.quality;
    liluo.save_model_stats(&stats).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config::{LlmConfig, ProviderEntry, TaijiConfig};

    fn make_providers(providers: Vec<ProviderEntry>) -> ProviderRegistry {
        let config = TaijiConfig {
            version: "0.1.0".into(),
            workspace: "default".into(),
            data_root: "./test_data".into(),
            llm: LlmConfig {
                default_provider: "deepseek".into(),
                default_model: "deepseek-chat".into(),
                api_key: "test-key-not-used".into(),
                providers,
                ..Default::default()
            },
            runtime: crate::infra::config::RuntimeConfig::default(),
            knowledge: crate::infra::config::KnowledgeConfig::default(),
            safety: crate::infra::config::SafetyConfig::default(),
            mcp_servers: vec![],
        };
        ProviderRegistry::new(&config).expect("registry")
    }

    fn row(n: u64, pass: u64, cost: u64, quality: f64, rounds: u64) -> ModelStatsRow {
        ModelStatsRow {
            n,
            pass_count: pass,
            cost_sum: cost,
            quality_sum: quality,
            rounds_sum: rounds,
        }
    }

    #[test]
    fn no_stats_returns_default() {
        let providers = make_providers(vec![]);
        let router = ModelRouter::new(&providers, BTreeMap::new());
        assert_eq!(router.route().key(), "deepseek-deepseek-chat");
    }

    #[test]
    fn high_pass_rate_wins() {
        // 候选：deepseek-chat（默认）与 reasoner。chat 统计好（9/10 通过、低成本），
        // reasoner 无统计（探索分 C·√ln(10/1) ≈ 2.1——被 chat 的 0.45 利用分压过？）
        // 不：无统计候选 explore≈2.1 > chat avg_reward≈0.45 → reasoner 会被选（探索）。
        // 要让 chat 赢需要它自己 n 大（探索项衰减）。n=1000：explore=1.414·√(ln1010/1001)≈0.004。
        let mut stats = BTreeMap::new();
        stats.insert(
            "deepseek-deepseek-chat".to_string(),
            row(1000, 900, 1_000_000, 900.0, 100),
        );
        let providers = make_providers(vec![]);
        let router = ModelRouter::new(&providers, stats);
        assert_eq!(router.route().key(), "deepseek-deepseek-chat");
    }

    #[test]
    fn cost_penalty_favors_cheap_model() {
        // 两模型同等通过率，但 A 贵 10 倍 → B（便宜）赢。
        let mut stats = BTreeMap::new();
        stats.insert(
            "deepseek-deepseek-chat".to_string(),
            row(100, 90, 10_000_000, 90.0, 5),
        );
        stats.insert(
            "deepseek-deepseek-reasoner".to_string(),
            row(100, 90, 1_000_000, 90.0, 5),
        );
        let providers = make_providers(vec![ProviderEntry {
            name: "deepseek".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: "deepseek-reasoner".into(),
        }]);
        let router = ModelRouter::new(&providers, stats);
        assert_eq!(router.route().key(), "deepseek-deepseek-reasoner");
    }

    #[test]
    fn zero_n_candidate_explores_when_n_total_small() {
        // N_total=1 时 explore = 1.414·√(ln1/1) = 0 → 无统计候选得分 0，
        // chat 有样本（1/1 通过）μ=0.6 赢（利用优先）。
        // N_total=3 时 reasoner explore = 1.414·√(ln3/1) ≈ 1.48 > chat
        // (0.6 + 1.414·√(ln3/4) ≈ 1.34) → 无统计候选被探索（bandit 探索语义）。
        let mut stats = BTreeMap::new();
        stats.insert(
            "deepseek-deepseek-chat".to_string(),
            row(3, 3, 300, 3.0, 0),
        );
        let providers = make_providers(vec![ProviderEntry {
            name: "deepseek".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: "deepseek-reasoner".into(),
        }]);
        let router = ModelRouter::new(&providers, stats);
        assert_eq!(router.route().key(), "deepseek-deepseek-reasoner");
    }

    /// 构造带 reasoner 附加候选的 providers（V37 异源测试复用）。
    fn make_providers_with_reasoner() -> ProviderRegistry {
        make_providers(vec![ProviderEntry {
            name: "deepseek".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: "deepseek-reasoner".into(),
        }])
    }

    #[test]
    fn route_verifier_single_candidate_returns_none() {
        // 候选仅默认（无附加 provider）→ 无源可异 → None（继承主模型）。
        let providers = make_providers(vec![]);
        let router = ModelRouter::new(&providers, BTreeMap::new());
        let exec = ModelKey::from_parts("deepseek", "deepseek-chat");
        assert!(router.route_verifier(&exec).is_none());
    }

    #[test]
    fn route_verifier_cold_start_picks_first_other() {
        // 全冷启动（无统计）：异源 = 候选声明顺序第一个非主候选（确定性）。
        let providers = make_providers_with_reasoner();
        let router = ModelRouter::new(&providers, BTreeMap::new());
        let exec = ModelKey::from_parts("deepseek", "deepseek-chat");
        let v = router.route_verifier(&exec).expect("verifier");
        assert_eq!(v.key(), "deepseek-deepseek-reasoner");
    }

    #[test]
    fn route_verifier_never_returns_exec_model() {
        // 异源硬约束：验证模型必须 ≠ 执行模型（裁判 ≠ 运动员）。
        let providers = make_providers_with_reasoner();
        let router = ModelRouter::new(&providers, BTreeMap::new());
        let exec = ModelKey::from_parts("deepseek", "deepseek-chat");
        let v = router.route_verifier(&exec).expect("verifier");
        assert_ne!(v.key(), exec.key());
    }

    #[test]
    fn route_verifier_ucb_picks_best_other() {
        // 有统计：reasoner 通过率高 → 异源选 reasoner（利用主导）。
        let mut stats = BTreeMap::new();
        stats.insert(
            "deepseek-deepseek-chat".to_string(),
            row(100, 80, 1_000_000, 80.0, 5),
        );
        stats.insert(
            "deepseek-deepseek-reasoner".to_string(),
            row(100, 95, 2_000_000, 95.0, 3),
        );
        let providers = make_providers_with_reasoner();
        let router = ModelRouter::new(&providers, stats);
        let exec = ModelKey::from_parts("deepseek", "deepseek-chat");
        let v = router.route_verifier(&exec).expect("verifier");
        assert_eq!(v.key(), "deepseek-deepseek-reasoner");
    }

    #[test]
    fn route_verifier_ignores_exec_stats_scope() {
        // 异源候选自身无统计但其他候选有 → 异源冷启动探索（选非主候选），
        // 即使执行模型利用分高——异源选择只看非主候选集。
        let mut stats = BTreeMap::new();
        stats.insert(
            "deepseek-deepseek-chat".to_string(),
            row(1000, 900, 1_000_000, 900.0, 5),
        );
        let providers = make_providers_with_reasoner();
        let router = ModelRouter::new(&providers, stats);
        let exec = ModelKey::from_parts("deepseek", "deepseek-chat");
        let v = router.route_verifier(&exec).expect("verifier");
        assert_eq!(v.key(), "deepseek-deepseek-reasoner");
    }

    #[tokio::test]
    async fn update_model_stats_accumulates_and_persists() {
        let dir = std::env::temp_dir().join(format!(
            "taiji_model_router_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let client = LiluoClient::new(&dir).await.unwrap();
        let key = ModelKey::from_parts("deepseek", "deepseek-chat");

        update_model_stats(
            &client,
            &key,
            &ModelStatsSignal {
                passed: true,
                cost_tokens: 1234,
                verify_rounds: 2,
                quality: 1.0,
            },
        )
        .await
        .unwrap();

        let stats = client.load_model_stats().await.unwrap();
        let row = stats.get("deepseek-deepseek-chat").expect("row exists");
        assert_eq!(row.n, 1);
        assert_eq!(row.pass_count, 1);
        assert_eq!(row.cost_sum, 1234);
        assert_eq!(row.rounds_sum, 2);
        assert!((row.quality_sum - 1.0).abs() < 1e-9);

        // 第二次回传累加。
        update_model_stats(
            &client,
            &key,
            &ModelStatsSignal {
                passed: false,
                cost_tokens: 500,
                verify_rounds: 1,
                quality: 0.4,
            },
        )
        .await
        .unwrap();
        let stats = client.load_model_stats().await.unwrap();
        let row = stats.get("deepseek-deepseek-chat").expect("row exists");
        assert_eq!(row.n, 2);
        assert_eq!(row.pass_count, 1);
        assert_eq!(row.cost_sum, 1734);
        assert!((row.quality_sum - 1.4).abs() < 1e-9);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
