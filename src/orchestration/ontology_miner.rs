//! V50 §6.6 本体挖掘（OntologyMiner）——连山纯符号挖掘（零 LLM）。
//!
//! 三个挖掘态射（纯函数，可单测）：
//! - `accumulate_cooccur` + `merge_cooccur`：任务级资产共现 → 持久累积（零新采集，
//!   数据源 = 既有 pending `assets_used × passed`）。
//! - `abstract_to_types`：id 级共现 → **类型抽象**（id → SemanticType id，无映射跳过）。
//! - `mine_dependencies`：type→type 共现 → 依赖边（联合通过率 ≥ 阈值 + 样本达标）。
//! - `mine_constraints`：失败 × env_tags 分组 → type-level 约束规则。
//!
//! 红线（§6.6）：连山纯符号（本模块零 LLM）；互斥边不挖（Forbid 留 SafetyHook/人工）；
//! 先验≠后验（产出边/规则是「先验智能」，进候选池仍走 UCB）。

use crate::types::agent::AssetRef;
use crate::types::ontology::{
    CooccurPair, FailureGroup, OntologyEdge, OntologyEdgeKind, OntologyRule, RuleCondition,
};
use crate::types::verification::{CheckKind, CheckResult, CheckSeverity};
use std::collections::HashMap;

/// 挖掘样本门槛（§6.6：≥ 50，防稀疏共现噪声）。
/// 测试用纯函数可传自定义阈值；生产 hook 用此常量。
pub(crate) const ONTOLOGY_MIN_SAMPLES: u64 = 50;
/// 依赖边联合通过率阈值（MVP：≥ 0.8）。
pub(crate) const ONTOLOGY_LIFT_THRESHOLD: f64 = 0.8;

/// 从一次任务的 `assets_used` 生成共现增量（所有无序对，co=1）。
/// 零 LLM；数据源 = 既有 pending 负载（阻塞点：MVP-1 只含 prompt，skill 级延后）。
pub fn accumulate_cooccur(assets_used: &[AssetRef], passed: bool) -> Vec<CooccurPair> {
    let ids: Vec<&str> = assets_used.iter().map(|a| a.id.as_str()).collect();
    let mut pairs = Vec::new();
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            let (a, b) = if ids[i] <= ids[j] {
                (ids[i].to_string(), ids[j].to_string())
            } else {
                (ids[j].to_string(), ids[i].to_string())
            };
            pairs.push(CooccurPair {
                a,
                b,
                co: 1,
                pass: if passed { 1 } else { 0 },
            });
        }
    }
    pairs
}

/// 合并共现累积（existing 存量 + delta 增量，按 (a,b) 键聚合 co/pass）。
pub fn merge_cooccur(existing: &[CooccurPair], deltas: &[CooccurPair]) -> Vec<CooccurPair> {
    let mut acc: HashMap<(String, String), (u64, u64)> = HashMap::new();
    for p in existing.iter().chain(deltas) {
        let e = acc.entry((p.a.clone(), p.b.clone())).or_insert((0, 0));
        e.0 += p.co;
        e.1 += p.pass;
    }
    let mut out: Vec<CooccurPair> = acc
        .into_iter()
        .map(|((a, b), (co, pass))| CooccurPair { a, b, co, pass })
        .collect();
    out.sort_by(|x, y| (x.a.as_str(), x.b.as_str()).cmp(&(y.a.as_str(), y.b.as_str())));
    out
}

/// **类型抽象**（§6.6 关键定论）：把 id 级共现对映射为 type 级共现对。
///
/// 资产 id 无类型映射 → 跳过（状态分支，非错误）；同 type 对聚合 co/pass。
pub fn abstract_to_types(
    pairs: &[CooccurPair],
    asset_types: &HashMap<String, String>,
) -> Vec<CooccurPair> {
    let mut acc: HashMap<(String, String), (u64, u64, Vec<String>)> = HashMap::new();
    for p in pairs {
        let (Some(ta), Some(tb)) = (asset_types.get(&p.a), asset_types.get(&p.b)) else {
            continue; // 无映射跳过
        };
        if ta == tb {
            continue; // 同类型内共现无跨类型意义
        }
        let (a, b) = if ta <= tb {
            (ta.clone(), tb.clone())
        } else {
            (tb.clone(), ta.clone())
        };
        let e = acc.entry((a, b)).or_insert((0, 0, Vec::new()));
        e.0 += p.co;
        e.1 += p.pass;
        e.2.push(p.a.clone());
        e.2.push(p.b.clone());
    }
    let mut out: Vec<CooccurPair> = acc
        .into_iter()
        .map(|((a, b), (co, pass, _ev))| CooccurPair { a, b, co, pass })
        .collect();
    out.sort_by(|x, y| (x.a.as_str(), x.b.as_str()).cmp(&(y.a.as_str(), y.b.as_str())));
    out
}

/// 共现 → 依赖边（type→type）。
///
/// 产出条件：样本 `co ≥ min_samples` 且联合通过率 `pass/co ≥ lift_threshold`。
/// strength = 联合通过率（MVP；lift = P(pass|a∧b) − P(pass|a) 的个体基线扩展后续）。
pub fn mine_dependencies(
    type_pairs: &[CooccurPair],
    min_samples: u64,
    lift_threshold: f64,
) -> Vec<OntologyEdge> {
    type_pairs
        .iter()
        .filter(|p| p.co >= min_samples && p.co > 0)
        .filter_map(|p| {
            let strength = p.pass as f64 / p.co as f64;
            if strength < lift_threshold {
                return None;
            }
            Some(OntologyEdge {
                from: p.a.clone(),
                to: p.b.clone(),
                kind: OntologyEdgeKind::WeakDependency,
                strength,
                samples: p.co,
                evidence: vec![], // MVP：id 级 evidence 由 abstract_to_types 侧记录（后续）
            })
        })
        .collect()
}

/// 失败 × env_tags → 约束规则（type-level GuardClause）。
///
/// 产出条件：某 check kind 在某 env 下失败率 = 1.0 且样本 ≥ min_samples。
/// `require` 约定为 `"check:{kind}"`——即「必须有一个能满足该检查项的资产」
/// （消费端 `validate_logic` 按此约定匹配资产 tags）。
pub fn mine_constraints(failures: &[FailureGroup], min_samples: u64) -> Vec<OntologyRule> {
    failures
        .iter()
        .filter(|f| f.total >= min_samples && f.total > 0 && f.fails == f.total)
        .map(|f| OntologyRule {
            id: format!("guard-{}-{}", f.check_kind, f.env_tags.join("-")),
            when: RuleCondition {
                domain: None,
                env: f.env_tags.first().cloned(),
                action: None,
            },
            require: vec![format!("check:{}", f.check_kind)],
            forbid: vec![],
            severity: CheckSeverity::Hard,
        })
        .collect()
}

/// 合并失败分组累积（existing 存量 + delta 增量，按 (env_tags, check_kind) 键聚合）。
pub fn merge_failures(existing: &[FailureGroup], deltas: &[FailureGroup]) -> Vec<FailureGroup> {
    let mut acc: HashMap<(Vec<String>, String), (u64, u64)> = HashMap::new();
    for f in existing.iter().chain(deltas) {
        let e = acc
            .entry((f.env_tags.clone(), f.check_kind.clone()))
            .or_insert((0, 0));
        e.0 += f.fails;
        e.1 += f.total;
    }
    acc.into_iter()
        .map(|((env_tags, check_kind), (fails, total))| FailureGroup {
            env_tags,
            check_kind,
            fails,
            total,
        })
        .collect()
}

/// CheckKind → serde snake_case 字符串（`file_exists` / `command_succeeds`）。
fn check_kind_name(kind: CheckKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// 连山本体挖掘入口（零 LLM；增强层——失败上抛由调用方 warn，§6.6）。
///
/// 数据源全部复用既有 pending 负载：`assets_used × passed × checks × model_key`。
/// 流程：共现累积 → 类型抽象 → 依赖边 → relations.yaml；
///      失败 × model_class 分组累积 → 约束规则 → rules.yaml。
pub async fn run_ontology_mining(
    guizang: &crate::infra::knowledge::GuizangClient,
    assets_used: &[AssetRef],
    passed: bool,
    checks: &[CheckResult],
    model_key: Option<&str>,
) -> Result<(), crate::infra::error::TaijiError> {
    // ── 1. 共现 → 依赖边 → relations.yaml ──
    if assets_used.len() >= 2 {
        let existing = guizang.load_cooccur().await?;
        let delta = accumulate_cooccur(assets_used, passed);
        let merged = merge_cooccur(&existing, &delta);
        guizang.save_cooccur(&merged).await?;

        let asset_types = guizang.asset_type_map().await?;
        if !asset_types.is_empty() {
            let type_pairs = abstract_to_types(&merged, &asset_types);
            let edges = mine_dependencies(&type_pairs, ONTOLOGY_MIN_SAMPLES, ONTOLOGY_LIFT_THRESHOLD);
            if !edges.is_empty() {
                guizang.save_relations(&edges).await?;
                tracing::info!(
                    edges = edges.len(),
                    "[ontology_miner] mined dependency edges → relations.yaml"
                );
            }
        }
    }

    // ── 2. 失败 × model_class → 约束规则 → rules.yaml ──
    if !checks.is_empty() {
        let env_tag =
            model_key.map(|k| crate::agents::factory::model_class_from_str(k).to_string());
        let delta: Vec<FailureGroup> = checks
            .iter()
            .filter_map(|c| {
                let kind = check_kind_name(c.kind);
                if kind.is_empty() {
                    return None;
                }
                Some(FailureGroup {
                    env_tags: env_tag.clone().into_iter().collect(),
                    check_kind: kind,
                    fails: if c.passed { 0 } else { 1 },
                    total: 1,
                })
            })
            .collect();
        if !delta.is_empty() {
            let existing = guizang.load_failures().await?;
            let merged = merge_failures(&existing, &delta);
            guizang.save_failures(&merged).await?;
            let rules = mine_constraints(&merged, ONTOLOGY_MIN_SAMPLES);
            if !rules.is_empty() {
                guizang.save_rules(&rules).await?;
                tracing::info!(
                    rules = rules.len(),
                    "[ontology_miner] mined constraint rules → rules.yaml"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &str, b: &str, co: u64, pass: u64) -> CooccurPair {
        CooccurPair { a: a.into(), b: b.into(), co, pass }
    }

    #[test]
    fn accumulate_pairs_all_unordered() {
        let assets = vec![
            AssetRef::new("prompt", "a"),
            AssetRef::new("prompt", "b"),
            AssetRef::new("prompt", "c"),
        ];
        let pairs = accumulate_cooccur(&assets, true);
        assert_eq!(pairs.len(), 3); // ab/ac/bc
        assert!(pairs.iter().all(|p| p.co == 1 && p.pass == 1));
    }

    #[test]
    fn merge_aggregates_by_key() {
        let existing = vec![p("a", "b", 3, 2)];
        let deltas = vec![p("a", "b", 1, 1), p("b", "c", 1, 0)];
        let merged = merge_cooccur(&existing, &deltas);
        assert_eq!(merged.len(), 2);
        let ab = merged.iter().find(|p| p.a == "a" && p.b == "b").unwrap();
        assert_eq!(ab.co, 4);
        assert_eq!(ab.pass, 3);
    }

    #[test]
    fn abstract_to_types_maps_and_skips() {
        let pairs = vec![p("d1", "s1", 5, 4), p("d2", "s1", 5, 4), p("d3", "unmapped", 5, 4)];
        let mut types = HashMap::new();
        types.insert("d1".to_string(), "deploy".to_string());
        types.insert("d2".to_string(), "deploy".to_string());
        types.insert("s1".to_string(), "security".to_string());
        let out = abstract_to_types(&pairs, &types);
        // d1+s1 与 d2+s1 聚合为 deploy+security；d3+unmapped 跳过
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].a, "deploy");
        assert_eq!(out[0].b, "security");
        assert_eq!(out[0].co, 10);
        assert_eq!(out[0].pass, 8);
    }

    #[test]
    fn mine_dependencies_threshold() {
        let pairs = vec![
            p("deploy", "security", 50, 48),   // 0.96 ≥ 0.8 → 产出
            p("deploy", "data", 50, 20),       // 0.4 < 0.8 → 不产出
            p("x", "y", 2, 2),                 // 样本不足 → 不产出
        ];
        let edges = mine_dependencies(&pairs, 10, 0.8);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "deploy");
        assert_eq!(edges[0].to, "security");
        assert!((edges[0].strength - 0.96).abs() < 1e-9);
    }

    #[test]
    fn mine_constraints_requires_full_failure() {
        let failures = vec![
            FailureGroup { env_tags: vec!["prod".into()], check_kind: "command-succeeds".into(), fails: 50, total: 50 },
            FailureGroup { env_tags: vec!["prod".into()], check_kind: "file-exists".into(), fails: 40, total: 50 },
        ];
        let rules = mine_constraints(&failures, 50);
        assert_eq!(rules.len(), 1, "只有 100% 失败产出规则");
        assert_eq!(rules[0].require, vec!["check:command-succeeds"]);
        assert_eq!(rules[0].when.env.as_deref(), Some("prod"));
    }
}
