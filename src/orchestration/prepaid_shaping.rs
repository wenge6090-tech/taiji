//! 预付费塑形（V64，Blueprint §5.9 熵产最小化定理移植）。
//!
//! 空闲窗口不盲目探索——识别高熵任务族（历史失败率 + 验证轮数），
//! 给它们跑**逆过程预习任务**（dry-run）：正常任务「写代码」→ 预习
//! 「读代码并重构」。提前在归藏里凿出应对未来高熵激波的测地线沟槽。
//!
//! 与主动学习分工：主动学习探索**资产空间**（哪个变体该试），
//! 预付费塑形探索**任务空间**（哪个任务类型未来会难）。
//!
//! 护栏（同主动学习）：预习任务不产生新预习任务；最小预算不递归；
//! 不写 model_stats（塑形非路由统计）；纯符号层零 LLM。

use std::path::Path;

use crate::infra::error::TaijiError;
use crate::infra::trace::save_json_atomic;

/// 任务族熵增率门槛：n < 此值不塑形（样本不足 = 噪声）。
pub const PREPAID_MIN_SAMPLES: u64 = 3;
/// 失败率权重（熵 = w_fail·失败率 + w_rounds·轮数归一）。
pub const ENTROPY_W_FAIL: f64 = 0.6;
/// 轮数权重。
pub const ENTROPY_W_ROUNDS: f64 = 0.4;

/// 任务族统计（task_type_tags 分组聚合，纯符号）。
#[derive(Debug, Clone, Default)]
pub struct TaskFamilyStats {
    /// 任务族标签（task_type_tags 第一个标签，如 `code` / `general`）。
    pub tag: String,
    /// 样本数。
    pub n: u64,
    /// 失败数（route != Pass）。
    pub fails: u64,
    /// 验证轮数累计。
    pub rounds_sum: u64,
}

impl TaskFamilyStats {
    /// 失败率。
    pub fn fail_rate(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.fails as f64 / self.n as f64
        }
    }

    /// 平均轮数。
    pub fn avg_rounds(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.rounds_sum as f64 / self.n as f64
        }
    }
}

/// 熵增率（V64）：失败率 + 轮数（归一 [0,1]：r/(r+1)）。
/// 熵越高 = 该任务族未来的高负载越可能「临时抱佛脚」——塑形优先级越高。
pub fn entropy_rate(s: &TaskFamilyStats) -> f64 {
    let r = s.avg_rounds();
    ENTROPY_W_FAIL * s.fail_rate() + ENTROPY_W_ROUNDS * (r / (r + 1.0))
}

/// 逆过程模板（初期硬编码；后期由象语言对偶推导——write↔read 等）。
/// 返回 (预习任务描述, 说明)。None = 该任务族无逆过程模板。
pub fn inverse_template(tag: &str) -> Option<(&'static str, &'static str)> {
    match tag {
        "code" => Some((
            "阅读并重构：分析现有代码的结构与组织，识别重复代码、脆弱依赖和可维护性问题，产出一份重构方案并实际执行一处安全的小重构。",
            "写代码的逆过程——读代码并重构",
        )),
        _ => None,
    }
}

/// 扫描任务目录，按 task_type_tags 分组聚合任务族统计（纯符号零 LLM）。
///
/// 数据源：`{data_root}/tasks/*/meta_ctx.json`（task_type_tags）+
/// `verify_state.json`（report.route + round）。I/O 失败 per-entry warn 跳过。
pub async fn scan_task_families(data_root: &Path) -> Result<Vec<TaskFamilyStats>, TaijiError> {
    let tasks_dir = data_root.join("tasks");
    let mut entries = match tokio::fs::read_dir(&tasks_dir).await {
        Ok(e) => e,
        Err(e) => {
            return Err(TaijiError::Other(format!(
                "failed to read tasks dir {:?}: {e}",
                tasks_dir
            )));
        }
    };

    let mut acc: std::collections::BTreeMap<String, TaskFamilyStats> =
        std::collections::BTreeMap::new();
    while let Some(entry) = entries.next_entry().await.transpose() {
        let Ok(entry) = entry else { continue };
        let task_dir = entry.path();
        if !task_dir.is_dir() {
            continue;
        }

        // 读 meta_ctx.json → task_type_tags
        let meta_ctx_path = task_dir.join("meta_ctx.json");
        let meta_ctx: serde_json::Value = match tokio::fs::read_to_string(&meta_ctx_path).await {
            Ok(c) => match serde_json::from_str(&c) {
                Ok(v) => v,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        let tags: Vec<String> = meta_ctx
            .get("task_type_tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if tags.is_empty() {
            continue;
        }
        let tag = tags[0].clone();

        // 读 verify_state.json → route + round
        let vs_path = task_dir.join("verify_state.json");
        let vs: serde_json::Value = match tokio::fs::read_to_string(&vs_path).await {
            Ok(c) => match serde_json::from_str(&c) {
                Ok(v) => v,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        let passed = vs
            .get("report")
            .and_then(|r| r.get("route"))
            .and_then(|v| v.as_str())
            .is_some_and(|r| r == "Pass");
        let round = vs.get("round").and_then(|v| v.as_u64()).unwrap_or(0);

        let fam = acc.entry(tag.clone()).or_insert(TaskFamilyStats {
            tag,
            ..Default::default()
        });
        fam.n += 1;
        if !passed {
            fam.fails += 1;
        }
        fam.rounds_sum += round;
    }

    Ok(acc.into_values().collect())
}

/// 预付费塑形空闲窗口入口（Lianshan Consumer 空闲窗口调用）。
///
/// 选熵增率最高的任务族（n ≥ PREPAID_MIN_SAMPLES 且存在逆过程模板）
/// → 入队预习任务（幂等：同族文件已存在跳过）。返回是否入队。
pub async fn run_prepaid_window(data_root: &Path) -> Result<bool, TaijiError> {
    let families = scan_task_families(data_root).await?;
    let candidate = families
        .iter()
        .filter(|f| f.n >= PREPAID_MIN_SAMPLES)
        .filter(|f| inverse_template(&f.tag).is_some())
        .max_by(|a, b| {
            entropy_rate(a)
                .partial_cmp(&entropy_rate(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let Some(family) = candidate else {
        return Ok(false);
    };
    enqueue_prepaid_task(data_root, family).await
}

/// 写入预习任务 `experiments/prepaid-{tag}.json`（幂等：已存在跳过）。
pub async fn enqueue_prepaid_task(
    data_root: &Path,
    family: &TaskFamilyStats,
) -> Result<bool, TaijiError> {
    let Some((template, _)) = inverse_template(&family.tag) else {
        return Ok(false);
    };
    let exp_dir = data_root.join("experiments");
    tokio::fs::create_dir_all(&exp_dir).await.map_err(|e| {
        TaijiError::Other(format!("failed to create experiments dir {:?}: {e}", exp_dir))
    })?;
    let path = exp_dir.join(format!("prepaid-{}.json", family.tag));
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(false); // 幂等：已入队
    }
    let payload = serde_json::json!({
        "task_id": format!("prepaid-{}", family.tag),
        "asset_id": "prepaid-shaping",
        "description": template,
        "prepaid": true,
        "family_tag": family.tag,
        "entropy": entropy_rate(family),
    });
    save_json_atomic(&payload, &path).map_err(|e| {
        TaijiError::Other(format!("failed to write prepaid task {:?}: {e}", path))
    })?;
    tracing::info!(
        family = %family.tag,
        n = family.n,
        entropy = entropy_rate(family),
        "[prepaid_shaping] inverse-process rehearsal task queued"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_rates_high_failure_family() {
        let bad = TaskFamilyStats {
            tag: "code".into(),
            n: 10,
            fails: 6,
            rounds_sum: 20,
        };
        let good = TaskFamilyStats {
            tag: "general".into(),
            n: 10,
            fails: 1,
            rounds_sum: 5,
        };
        assert!(
            entropy_rate(&bad) > entropy_rate(&good),
            "高失败族熵高于低失败族"
        );
    }

    #[test]
    fn entropy_zero_for_empty_family() {
        let empty = TaskFamilyStats::default();
        assert_eq!(entropy_rate(&empty), 0.0);
    }

    #[test]
    fn inverse_template_covers_code() {
        assert!(inverse_template("code").is_some());
        assert!(inverse_template("general").is_none(), "无模板任务族不塑形");
    }

    #[tokio::test]
    async fn enqueue_prepaid_task_idempotent() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let family = TaskFamilyStats {
            tag: "code".into(),
            n: 10,
            fails: 2,
            rounds_sum: 10,
        };
        assert!(enqueue_prepaid_task(dir.path(), &family).await.unwrap());
        assert!(
            !enqueue_prepaid_task(dir.path(), &family).await.unwrap(),
            "重复入队幂等跳过"
        );
        let file = dir.path().join("experiments").join("prepaid-code.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["asset_id"], "prepaid-shaping");
        assert_eq!(v["prepaid"], true);
    }
}
