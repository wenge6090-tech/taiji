use serde::{Deserialize, Serialize};

/// Routing decision emitted by CausalAgent (因果验证·阴).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VerificationRoute {
    /// Task passed verification — convergence, proceed to DMN reflection.
    Pass,
    /// Execution deviation — retry 概率拟合 (阳).
    BackToTpn,
    /// Cognitive deviation — retry 权重更新 (元).
    BackToMeta,
}

/// Convergence status for subtask aggregation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConvergenceStatus {
    Converged,
    Partial,
    Diverged,
}

/// Output of CausalAgent.verify().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub route: VerificationRoute,
    pub confidence: f64,
    pub summary: String,
    pub constraint_violations: Vec<String>,
}

/// Output of CausalAgent.converge().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceDecision {
    pub status: ConvergenceStatus,
    pub task_summary: String,
}

/// L4 Truth 的状态（TMS 真值维护）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TruthStatus {
    /// 活跃中，ConstraintEngine 正常加载。
    Active,
    /// 被 RETRACT，不再参与约束检查。
    Retracted,
    /// 上游依赖断裂，等待重新验证或移除。
    Stale,
}

impl Default for TruthStatus {
    fn default() -> Self {
        Self::Active
    }
}

/// An L4 Truth constraint (runtime enforcement).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthConstraint {
    pub id: String,
    pub name: String,
    pub description: String,
    pub severity: ConstraintSeverity,
    // ── TMS 字段（V18 新增） ──
    /// 为什么这个约束成立？（TMS justification 审计）
    /// None = 未初始化（旧资产兼容）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
    /// 真值状态。默认 Active。
    #[serde(default)]
    pub status: TruthStatus,
}

impl TruthConstraint {
    /// Shorthand for constructing a Hard truth (backward-compatible helper).
    pub fn hard(id: &str, name: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            severity: ConstraintSeverity::Hard,
            justification: None,
            status: TruthStatus::Active,
        }
    }

    /// Shorthand for constructing a Soft truth.
    pub fn soft(id: &str, name: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            severity: ConstraintSeverity::Soft,
            justification: None,
            status: TruthStatus::Active,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConstraintSeverity {
    Hard,
    Soft,
}

/// Result of constraint checking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintResult {
    pub passed: bool,
    pub violations: Vec<ConstraintViolation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintViolation {
    pub truth_id: String,
    pub truth_name: String,
    pub reason: String,
    pub severity: ConstraintSeverity,
}

// ---------------------------------------------------------------------------
// V33 验证契约类型（归藏本体论重构 — §6.0/§6.6/§8.22）
// ---------------------------------------------------------------------------

/// 验证契约检查项类型（CheckSpec.kind）。
///
/// 前四种为**机械可判定断言**（L0/L1，ContractEngine 执行，LLM 不可翻案）；
/// `LlmJudgement` 是唯一留给 LLM 的检查项类型（L2 兜底，§6.6）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// 文件/目录存在性（target 支持单段 `*` 通配）。
    FileExists,
    /// 结构校验：params = {format: "json"|"yaml", required_fields: ["a.b"]}（点分路径）。
    SchemaValid,
    /// 引用解析：解析 target 的 YAML front matter，params = {field: "output_refs"}，
    /// 数组内路径必须真实存在于 task_dir。
    ReferenceResolves,
    /// 命令执行成功：params = {command: "cargo check"}——白名单前缀 + 30s 超时。
    CommandSucceeds,
    /// LLM 裁决：不机械执行，由 CausalAgent 收集注入 verify prompt（§6.6 L2）。
    LlmJudgement,
    /// V34/MVP-4 断言证据链：产出中 `[证据: 工具名]` 引用 → 任务 trace.jsonl
    /// `tool_call::*` 记录存在性校验（引用完整性，reference_resolves 推广，§8.22）。
    /// 纯机械零 LLM；只对精确格式引用做存在性判定，无匹配视为推测（宁漏勿误）。
    TraceConsistency,
}

impl Default for CheckKind {
    fn default() -> Self {
        Self::FileExists // serde 容错：YAML 缺 kind 时按最常见的机械检查处理
    }
}

/// 检查项严重度。Hard 失败 = 验证失败（LLM 不可翻案）；Soft 失败注入 LLM 参考。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CheckSeverity {
    Hard,
    Soft,
}

impl Default for CheckSeverity {
    fn default() -> Self {
        Self::Hard // 安全默认：无显式声明时按 Hard 处理
    }
}

/// 验证契约的最小单元（本体论规则/公理，§6.0 TBox）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckSpec {
    pub id: String,
    #[serde(default)]
    pub kind: CheckKind,
    /// 相对 task_dir 的路径或单段 `*` 通配。
    pub target: String,
    /// kind 相关参数（JSON 对象，MVP-1 内联自包含）。
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub severity: CheckSeverity,
    /// 人读判据（llm_judgement 类注入 LLM prompt）。
    pub pass_condition: String,
    /// 检查项级统计（MVP-2 DMN backprop 回传 — BCP §6.4 V33 统计粒度）。
    /// serde default：旧契约 YAML 零迁移。
    #[serde(default)]
    pub stats: CheckStats,
}

/// 检查项级统计块（MVP-2 通过率一维 → MVP-3 四维回报，BCP §6.4）。
/// 四维：pass_rate（pass_count/n）/ avg_quality / avg_cost / avg_verify_rounds——
/// 全部来自既有数据（CheckResult 携带，零新增持久化文件）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct CheckStats {
    /// 采样次数（DMN backprop 累加）。
    #[serde(default)]
    pub n: u64,
    /// 通过次数。
    #[serde(default)]
    pub pass_count: u64,
    /// token 成本累计（trace usage.input_tokens 求和摊派）。
    #[serde(default)]
    pub cost_sum: u64,
    /// 验证轮数累计（BACK_TO_TPN 次数，收敛速度倒数）。
    #[serde(default)]
    pub rounds_sum: u64,
    /// 质量分累计（派生：route 映射 × VerificationReport.confidence，不落 VerificationReport schema）。
    #[serde(default)]
    pub quality_sum: f64,
}

impl CheckStats {
    /// 通过率 [0.0, 1.0]；无采样返回 0.0。
    pub fn pass_rate(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.pass_count as f64 / self.n as f64
        }
    }

    /// 平均 token 成本；无采样返回 0.0。
    pub fn avg_cost(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.cost_sum as f64 / self.n as f64
        }
    }

    /// 平均验证轮数；无采样返回 0.0。
    pub fn avg_rounds(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.rounds_sum as f64 / self.n as f64
        }
    }

    /// 平均质量分；无采样返回 0.0。
    pub fn avg_quality(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.quality_sum / self.n as f64
        }
    }

    /// 回报函数（BCP §6.4 写死，config `runtime.dmn.reward_weights` 可覆盖）：
    /// `reward = w_pass·pass_rate + w_quality·avg_quality − w_cost·avg_cost − w_rounds·avg_verify_rounds`
    pub fn reward(&self, w: &RewardWeights) -> f64 {
        w.pass * self.pass_rate()
            + w.quality * self.avg_quality()
            - w.cost * self.avg_cost()
            - w.rounds * self.avg_rounds()
    }
}

/// 回报函数权重（§6.4 默认值，config `runtime.dmn.reward_weights` 可覆盖）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RewardWeights {
    pub pass: f64,
    pub quality: f64,
    pub cost: f64,
    pub rounds: f64,
}

impl Default for RewardWeights {
    fn default() -> Self {
        Self {
            pass: 0.5,
            quality: 0.3,
            cost: 0.2,
            rounds: 0.1,
        }
    }
}

/// 契约执行记录（随 verify_state.json 持久化，零新增文件 — §6.6）。
/// MVP-3 扩展：任务级信号（cost/rounds/quality）摊派给同任务所有检查项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub check_id: String,
    pub kind: CheckKind,
    pub passed: bool,
    /// 失败原因 / 截断输出（≤2KB）。
    pub detail: String,
    pub duration_ms: u64,
    /// token 成本（trace usage.input_tokens 求和；serde default 零迁移）。
    #[serde(default)]
    pub cost_tokens: u64,
    /// 验证轮数（verify_state.round；BACK_TO_TPN 次数）。
    #[serde(default)]
    pub verify_rounds: u32,
    /// 质量分（route 映射 × confidence 派生，§6.4）。
    #[serde(default)]
    pub quality: f64,
}

/// ContractEngine 输出（L0 机械 + L1 契约，§8.22）。
/// 仅含机械检查项结果；llm_judgement 项由调用方（CausalAgent）收集。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractReport {
    /// 任一 hard 机械项失败 → false。
    pub passed: bool,
    pub results: Vec<CheckResult>,
    pub summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V33/MVP-2：旧契约 YAML（无 stats 键）零迁移兼容。
    #[test]
    fn check_spec_without_stats_defaults_to_zero() {
        let yaml = r#"
id: old-check
kind: file_exists
target: deliverables/out.md
params: {}
severity: hard
pass_condition: 产出必须存在
"#;
        let spec: CheckSpec = serde_yaml::from_str(yaml).expect("old check must deserialize");
        assert_eq!(spec.stats.n, 0);
        assert_eq!(spec.stats.pass_count, 0);
        assert_eq!(spec.stats.pass_rate(), 0.0);
        assert_eq!(spec.kind, CheckKind::FileExists);
        assert_eq!(spec.severity, CheckSeverity::Hard);
    }

    /// V33/MVP-2：新格式带 stats 正常反序列化 + pass_rate 计算。
    #[test]
    fn check_spec_with_stats_roundtrip() {
        let spec = CheckSpec {
            id: "c1".into(),
            kind: CheckKind::SchemaValid,
            target: "meta.json".into(),
            params: serde_json::json!({}),
            severity: CheckSeverity::Soft,
            pass_condition: "p".into(),
            stats: CheckStats {
                n: 4,
                pass_count: 3,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: CheckSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.stats.n, 4);
        assert_eq!(back.stats.pass_count, 3);
        assert!((back.stats.pass_rate() - 0.75).abs() < 1e-9);
    }

    /// V33/MVP-3：四维统计 avg 计算 + 回报函数（§6.4 默认权重）。
    #[test]
    fn check_stats_four_dimension_reward() {
        let stats = CheckStats {
            n: 4,
            pass_count: 3,
            cost_sum: 8000,
            rounds_sum: 4,
            quality_sum: 2.8,
        };
        assert!((stats.pass_rate() - 0.75).abs() < 1e-9);
        assert!((stats.avg_cost() - 2000.0).abs() < 1e-9);
        assert!((stats.avg_rounds() - 1.0).abs() < 1e-9);
        assert!((stats.avg_quality() - 0.7).abs() < 1e-9);
        let w = RewardWeights::default();
        assert_eq!(w.pass, 0.5);
        assert_eq!(w.quality, 0.3);
        assert_eq!(w.cost, 0.2);
        assert_eq!(w.rounds, 0.1);
        // reward = 0.5·0.75 + 0.3·0.7 − 0.2·2000 − 0.1·1.0
        let expected = 0.5 * 0.75 + 0.3 * 0.7 - 0.2 * 2000.0 - 0.1 * 1.0;
        assert!((stats.reward(&w) - expected).abs() < 1e-9);
        // 无采样：avg 全部 0.0，reward 0.0
        let empty = CheckStats::default();
        assert_eq!(empty.avg_cost(), 0.0);
        assert_eq!(empty.reward(&w), 0.0);
    }

    /// V33/MVP-3：四维 stats 完整 serde roundtrip（含 quality_sum 浮点）。
    #[test]
    fn check_stats_four_dimension_roundtrip() {
        let stats = CheckStats {
            n: 10,
            pass_count: 7,
            cost_sum: 12345,
            rounds_sum: 6,
            quality_sum: 6.5,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let back: CheckStats = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cost_sum, 12345);
        assert_eq!(back.rounds_sum, 6);
        assert!((back.quality_sum - 6.5).abs() < 1e-9);
    }

    /// V33/MVP-3：MVP-2 旧格式（仅 n/pass_count）零迁移反序列化。
    #[test]
    fn check_stats_old_two_dimension_zero_migration() {
        let json = r#"{"n":5,"pass_count":4}"#;
        let stats: CheckStats = serde_json::from_str(json).unwrap();
        assert_eq!(stats.n, 5);
        assert_eq!(stats.pass_count, 4);
        assert_eq!(stats.cost_sum, 0);
        assert_eq!(stats.rounds_sum, 0);
        assert_eq!(stats.quality_sum, 0.0);
    }
}
