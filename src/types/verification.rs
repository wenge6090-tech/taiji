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
