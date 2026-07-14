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

/// An L4 Truth constraint (runtime enforcement).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthConstraint {
    pub id: String,
    pub name: String,
    pub description: String,
    pub severity: ConstraintSeverity,
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
