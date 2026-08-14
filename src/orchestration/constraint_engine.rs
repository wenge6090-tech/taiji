//! ConstraintEngine — L4 硬约束运行时执行（V38 起内置化，不再读归藏 truths/）。
//! 两个集成点：
//!   1. `load_truths()`  — 硬编码基线约束（不编造事实/有依据推理/可审计 + code-safety）
//!   2. `check_constraints()` / `check_yin_output()` — YinAgent LLM 调用前的 L0 检查
//!
//! V38：truths 资产层已移除——约束不再资产化、不参与 Lianshan 演化；
//! 硬约束 = 本引擎内置的硬编码检查（summary 非空/有依据/可审计 + code-safety）。

use crate::types::ontology::OntologyRule;
use crate::types::verification::{
    CheckSeverity, ConstraintResult, ConstraintSeverity, ConstraintViolation, TruthConstraint,
    TruthStatus,
};

/// Engine for loading and enforcing L4 Truth constraints.
///
/// Constraint checking happens **before** the YinAgent LLM call.
/// Hard violations short-circuit immediately without invoking the model.
/// Soft violations are injected as additional context for the LLM to adjudicate.
#[derive(Debug, Clone)]
pub struct ConstraintEngine;

impl ConstraintEngine {
    /// Create a new ConstraintEngine instance.
    pub fn new() -> Self {
        Self
    }

    /// Load built-in L4 Truths based on the given task type tags.
    ///
    /// Always loads the three core truths:
    ///   - `truth:no-fabrication`  (Hard) — no fabricating facts
    ///   - `truth:evidence-based`  (Hard) — reasoning must trace to evidence
    ///   - `truth:auditable`       (Soft) — process transparency
    ///
    /// If tags contain `"code"`, additionally loads:
    ///   - `truth:code-safety`     (Hard) — no security regressions
    ///
    /// V50 §6.6：`rules` 为连山本体挖掘的 type-level 约束规则（`rules.yaml`），
    /// 映射为 TruthConstraint（元层 ∪ 挖掘规则；挖掘规则 id 前缀 `ontology:`）。
    pub fn load_truths(task_type_tags: &[String], rules: &[OntologyRule]) -> Vec<TruthConstraint> {
        let mut truths = Vec::with_capacity(4 + rules.len());

        truths.push(TruthConstraint::hard(
            "truth:no-fabrication",
            "不编造事实",
            "Don't fabricate facts or make unsubstantiated claims",
        ));

        truths.push(TruthConstraint::hard(
            "truth:evidence-based",
            "有依据推理",
            "All reasoning must be grounded in evidence",
        ));

        truths.push(TruthConstraint::soft(
            "truth:auditable",
            "透明可审计",
            "Process should be transparent and auditable",
        ));

        if task_type_tags.iter().any(|t| t.eq_ignore_ascii_case("code")) {
            truths.push(TruthConstraint::hard(
                "truth:code-safety",
                "代码安全",
                "Code changes must not introduce security vulnerabilities",
            ));
        }

        // V50 §6.6：挖掘规则 → TruthConstraint（require/forbid 清单，阴机械执行）。
        for r in rules {
            let severity = match r.severity {
                CheckSeverity::Hard => ConstraintSeverity::Hard,
                CheckSeverity::Soft => ConstraintSeverity::Soft,
            };
            let mut desc = format!("when={:?}", r.when);
            if !r.require.is_empty() {
                desc.push_str(&format!(" require=[{}]", r.require.join(",")));
            }
            if !r.forbid.is_empty() {
                desc.push_str(&format!(" forbid=[{}]", r.forbid.join(",")));
            }
            truths.push(TruthConstraint {
                id: format!("ontology:{}", r.id),
                name: r.id.clone(),
                description: desc,
                severity,
                justification: Some("连山本体挖掘（§6.6）".into()),
                status: TruthStatus::Active,
            });
        }

        tracing::debug!(
            count = truths.len(),
            tags = ?task_type_tags,
            "Loaded L4 Truths"
        );

        truths
    }

    /// Check YinAgent textual output (summary + violation list) against
    /// a set of constraints.
    ///
    /// This mirrors `check_constraints` but operates on the string-level
    /// outputs produced by YinAgent.verify() / .converge().
    ///
    /// Any **Hard** violation short-circuits immediately.
    pub fn check_yin_output(
        summary: &str,
        violations: &[String],
        constraints: &[TruthConstraint],
    ) -> ConstraintResult {
        if constraints.is_empty() {
            return ConstraintResult {
                passed: true,
                violations: Vec::new(),
            };
        }

        let mut result_violations: Vec<ConstraintViolation> = Vec::new();

        for constraint in constraints {
            let maybe_violation = match constraint.id.as_str() {
                "truth:no-fabrication" => {
                    if summary.trim().is_empty() {
                        Some(ConstraintViolation {
                            truth_id: constraint.id.clone(),
                            truth_name: constraint.name.clone(),
                            reason: "YinAgent summary is empty; possible missing analysis"
                                .into(),
                            severity: constraint.severity.clone(),
                        })
                    } else {
                        None
                    }
                }
                "truth:evidence-based" => {
                    if violations.is_empty() && summary.trim().len() < 20 {
                        Some(ConstraintViolation {
                            truth_id: constraint.id.clone(),
                            truth_name: constraint.name.clone(),
                            reason: "Summary is too short to contain meaningful evidence".into(),
                            severity: constraint.severity.clone(),
                        })
                    } else {
                        None
                    }
                }
                "truth:auditable" => {
                    // Soft: just warn if summary seems terse
                    if summary.trim().len() < 10 {
                        Some(ConstraintViolation {
                            truth_id: constraint.id.clone(),
                            truth_name: constraint.name.clone(),
                            reason: "Summary is very terse, reducing auditability".into(),
                            severity: constraint.severity.clone(),
                        })
                    } else {
                        None
                    }
                }
                "truth:code-safety" => {
                    let has_code_violation = violations.iter().any(|v| {
                        let lower = v.to_lowercase();
                        lower.contains("code") || lower.contains("security") || lower.contains("unsafe")
                    });
                    if has_code_violation {
                        Some(ConstraintViolation {
                            truth_id: constraint.id.clone(),
                            truth_name: constraint.name.clone(),
                            reason: "Code safety violations present in YinAgent output".into(),
                            severity: constraint.severity.clone(),
                        })
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(violation) = maybe_violation {
                if violation.severity == ConstraintSeverity::Hard {
                    tracing::debug!(
                        count = result_violations.len(),
                        hard_truth_id = %violation.truth_id,
                        hard_truth_name = %violation.truth_name,
                        reason = %violation.reason,
                        "Hard constraint violation in yin output — returning all violations"
                    );
                    result_violations.push(violation);
                    return ConstraintResult {
                        passed: false,
                        violations: result_violations,  // Include soft violations accumulated before
                    };
                }
                result_violations.push(violation);
            }
        }

        let passed = result_violations.is_empty();
        ConstraintResult {
            passed,
            violations: result_violations,
        }
    }

}

impl Default for ConstraintEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::verification::ConstraintSeverity;

    fn hard_truth(id: &str, name: &str, desc: &str) -> TruthConstraint {
        TruthConstraint::hard(id, name, desc)
    }

    #[test]
    fn test_load_truths_default() {
        let tags: Vec<String> = Vec::new();
        let truths = ConstraintEngine::load_truths(&tags, &[]);
        assert_eq!(truths.len(), 3);
        assert!(truths.iter().any(|t| t.id == "truth:no-fabrication"));
        assert!(truths.iter().any(|t| t.id == "truth:evidence-based"));
        assert!(truths.iter().any(|t| t.id == "truth:auditable"));
    }

    #[test]
    fn test_load_truths_with_code_tag() {
        let tags = vec!["code".into()];
        let truths = ConstraintEngine::load_truths(&tags, &[]);
        assert_eq!(truths.len(), 4);
        assert!(truths.iter().any(|t| t.id == "truth:code-safety"));
    }

    #[test]
    fn test_load_truths_code_tag_case_insensitive() {
        let tags = vec!["CODE".into()];
        let truths = ConstraintEngine::load_truths(&tags, &[]);
        assert_eq!(truths.len(), 4);
    }

    /// V50 §6.6：挖掘规则 → TruthConstraint（元层 ∪ rules）。
    #[test]
    fn test_load_truths_with_ontology_rules() {
        use crate::types::ontology::{OntologyRule, RuleCondition};
        use crate::types::verification::CheckSeverity;
        let tags: Vec<String> = vec![];
        let rules = vec![OntologyRule {
            id: "guard-command-succeeds-prod".into(),
            when: RuleCondition { domain: None, env: Some("prod".into()), action: None },
            require: vec!["check:command_succeeds".into()],
            forbid: vec![],
            severity: CheckSeverity::Hard,
        }];
        let truths = ConstraintEngine::load_truths(&tags, &rules);
        assert_eq!(truths.len(), 4); // 3 元层 + 1 挖掘规则
        assert!(truths.iter().any(|t| t.id == "ontology:guard-command-succeeds-prod"));
    }

    #[test]
    fn test_check_yin_output_empty_summary_fails_hard() {
        let constraints = vec![hard_truth(
            "truth:no-fabrication",
            "不编造事实",
            "no fab",
        )];
        let result =
            ConstraintEngine::check_yin_output("", &[], &constraints);
        assert!(!result.passed);
    }

    #[test]
    fn test_check_yin_output_non_empty_summary_passes() {
        let constraints = vec![hard_truth(
            "truth:no-fabrication",
            "不编造事实",
            "no fab",
        )];
        let result = ConstraintEngine::check_yin_output(
            "Analysis complete: all constraints satisfied.",
            &[],
            &constraints,
        );
        assert!(result.passed);
    }

    #[test]
    fn test_check_yin_output_code_safety_detected() {
        let constraints = vec![hard_truth(
            "truth:code-safety",
            "代码安全",
            "code safety",
        )];
        let violations = vec!["Unsafe code detected in module X".into()];
        let result = ConstraintEngine::check_yin_output(
            "Analysis complete.",
            &violations,
            &constraints,
        );
        assert!(!result.passed);
        assert_eq!(result.violations[0].truth_id, "truth:code-safety");
    }

    #[test]
    fn test_empty_constraints_passes_immediately() {
        let result = ConstraintEngine::check_yin_output("", &[], &[]);
        assert!(result.passed);
    }
}
