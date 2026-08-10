//! ConstraintEngine — L4 Truth runtime enforcement.
//! Two integration points:
//!   1. `load_truths()`  — inject L4 Truths into MetaAgent context
//!   2. `check_constraints()` / `check_causal_output()` — pre-check before CausalAgent LLM call
//!
//! See AGENTS.md §4 for detailed rules.

use crate::infra::knowledge::LiluoClient;
use crate::types::agent::MetaContext;
use crate::types::verification::{
    ConstraintResult, ConstraintSeverity, ConstraintViolation, TruthConstraint, TruthStatus,
};

/// Engine for loading and enforcing L4 Truth constraints.
///
/// Constraint checking happens **before** the CausalAgent LLM call.
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
    pub fn load_truths(task_type_tags: &[String]) -> Vec<TruthConstraint> {
        let mut truths = Vec::with_capacity(4);

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

        tracing::debug!(
            count = truths.len(),
            tags = ?task_type_tags,
            "Loaded L4 Truths"
        );

        truths
    }

    /// Load L4 Truths from 归藏 Guizang, filtering by `TruthStatus::Active`.
    ///
    /// Returns only truths whose `status == Active`. Retracted or stale truths
    /// are excluded from runtime enforcement.
    ///
    /// **Fallback:** When Guizang has no active truths for the given tags,
    /// returns an empty Vec — the system runs with no active constraints.
    /// Callers that always need baseline constraints should chain with
    /// [`load_truths`] as a fallback layer.
    pub async fn load_truths_from_guizang(guizang: &LiluoClient) -> Vec<TruthConstraint> {
        let assets = match guizang.load_active_truths().await {
            Ok(assets) => assets,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to load active truths from Guizang — running without L4 constraints"
                );
                return Vec::new();
            }
        };

        if assets.is_empty() {
            tracing::debug!(
                "No active truths found in Guizang — running without L4 constraints"
            );
            return Vec::new();
        }

        let truths: Vec<TruthConstraint> = assets
            .iter()
            .map(|a| TruthConstraint {
                id: a.header.id.clone(),
                name: a.header.name.clone(),
                description: a.header.description.clone(),
                severity: match a.severity.as_str() {
                    "Hard" => ConstraintSeverity::Hard,
                    _ => ConstraintSeverity::Soft,
                },
                justification: a.justification.clone(),
                status: match a.status.as_str() {
                    "active" => TruthStatus::Active,
                    "retracted" => TruthStatus::Retracted,
                    _ => TruthStatus::Stale,
                },
            })
            .collect();

        tracing::debug!(
            count = truths.len(),
            "Loaded active L4 Truths from Guizang"
        );

        truths
    }

    /// Check `MetaContext` output against a set of constraints.
    ///
    /// For each constraint a domain-specific check is applied:
    ///   - `truth:no-fabrication`  → constraints + skills must not both be empty
    ///   - `truth:evidence-based`  → at least one L4 constraint present
    ///   - `truth:auditable`       → task description should be non-empty (soft)
    ///   - `truth:code-safety`     → dangerous tools require constraint guardrails
    ///
    /// Any **Hard** violation immediately returns `passed: false`
    /// with a single violation entry (short-circuit).  Soft violations
    /// are accumulated and returned without short-circuiting.
    pub fn check_constraints(
        output: &MetaContext,
        constraints: &[TruthConstraint],
    ) -> ConstraintResult {
        if constraints.is_empty() {
            return ConstraintResult {
                passed: true,
                violations: Vec::new(),
            };
        }

        let mut violations: Vec<ConstraintViolation> = Vec::new();

        for constraint in constraints {
            let maybe_violation = Self::check_single_constraint(output, constraint);

            if let Some(violation) = maybe_violation {
                if violation.severity == ConstraintSeverity::Hard {
                    tracing::debug!(
                        count = violations.len(),
                        hard_truth_id = %violation.truth_id,
                        hard_truth_name = %violation.truth_name,
                        reason = %violation.reason,
                        "Hard constraint violation — returning all accumulated violations"
                    );
                    violations.push(violation);
                    return ConstraintResult {
                        passed: false,
                        violations,  // Include soft violations accumulated before the hard one
                    };
                }
                violations.push(violation);
            }
        }

        let passed = violations.is_empty();
        ConstraintResult { passed, violations }
    }

    /// Check CausalAgent textual output (summary + violation list) against
    /// a set of constraints.
    ///
    /// This mirrors `check_constraints` but operates on the string-level
    /// outputs produced by CausalAgent.verify() / .converge().
    ///
    /// Any **Hard** violation short-circuits immediately.
    pub fn check_causal_output(
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
                            reason: "CausalAgent summary is empty; possible missing analysis"
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
                            reason: "Code safety violations present in CausalAgent output".into(),
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
                        "Hard constraint violation in causal output — returning all violations"
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

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Check a single constraint against the MetaContext.
    /// Returns `Some(violation)` if the constraint is not satisfied.
    fn check_single_constraint(
        output: &MetaContext,
        constraint: &TruthConstraint,
    ) -> Option<ConstraintViolation> {
        match constraint.id.as_str() {
            "truth:no-fabrication" => {
                if output.constraints.is_empty() && output.matched_skills.is_empty() {
                    Some(ConstraintViolation {
                        truth_id: constraint.id.clone(),
                        truth_name: constraint.name.clone(),
                        reason: "No constraints or skills in MetaContext — possible fabrication risk"
                            .into(),
                        severity: constraint.severity.clone(),
                    })
                } else {
                    None
                }
            }
            "truth:evidence-based" => {
                if output.constraints.is_empty() {
                    Some(ConstraintViolation {
                        truth_id: constraint.id.clone(),
                        truth_name: constraint.name.clone(),
                        reason: "No L4 constraints in MetaContext — missing evidence"
                            .into(),
                        severity: constraint.severity.clone(),
                    })
                } else {
                    None
                }
            }
            "truth:auditable" => {
                if output.yang_prompt.task_description.trim().is_empty() {
                    Some(ConstraintViolation {
                        truth_id: constraint.id.clone(),
                        truth_name: constraint.name.clone(),
                        reason: "Task context is empty — reduces auditability".into(),
                        severity: constraint.severity.clone(),
                    })
                } else {
                    None
                }
            }
            "truth:code-safety" => {
                let has_dangerous_tool = output.matched_skills.iter().any(|s| {
                    matches!(s.tool_name.as_str(), "bash" | "exec" | "shell")
                });
                if has_dangerous_tool && output.constraints.is_empty() {
                    Some(ConstraintViolation {
                        truth_id: constraint.id.clone(),
                        truth_name: constraint.name.clone(),
                        reason: "Dangerous tools matched but no constraints guide safe usage"
                            .into(),
                        severity: constraint.severity.clone(),
                    })
                } else {
                    None
                }
            }
            _ => None,
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
    use crate::types::agent::{SkillRef, YangPrompt};
    use crate::types::verification::ConstraintSeverity;

    fn make_meta_context(
        constraints: Vec<TruthConstraint>,
        matched_skills: Vec<SkillRef>,
        task_description: &str,
    ) -> MetaContext {
        MetaContext {
            constraints,
            matched_skills,
            yang_prompt: YangPrompt {
                task_description: task_description.into(),
                constraint_summaries: Vec::new(),
                parent_deliverables: vec![],
                sibling_deliverables: vec![],
            },
            mode: crate::types::agent::AgentMode::Orchestration,
            fitting_system_prompt: None,
            verify_system_prompt: None,
            converge_system_prompt: None,
        }
    }

    fn hard_truth(id: &str, name: &str, desc: &str) -> TruthConstraint {
        TruthConstraint::hard(id, name, desc)
    }

    fn soft_truth(id: &str, name: &str, desc: &str) -> TruthConstraint {
        TruthConstraint::soft(id, name, desc)
    }

    #[test]
    fn test_load_truths_default() {
        let tags: Vec<String> = Vec::new();
        let truths = ConstraintEngine::load_truths(&tags);
        assert_eq!(truths.len(), 3);
        assert!(truths.iter().any(|t| t.id == "truth:no-fabrication"));
        assert!(truths.iter().any(|t| t.id == "truth:evidence-based"));
        assert!(truths.iter().any(|t| t.id == "truth:auditable"));
    }

    #[test]
    fn test_load_truths_with_code_tag() {
        let tags = vec!["code".into()];
        let truths = ConstraintEngine::load_truths(&tags);
        assert_eq!(truths.len(), 4);
        assert!(truths.iter().any(|t| t.id == "truth:code-safety"));
    }

    #[test]
    fn test_load_truths_code_tag_case_insensitive() {
        let tags = vec!["CODE".into()];
        let truths = ConstraintEngine::load_truths(&tags);
        assert_eq!(truths.len(), 4);
    }

    #[test]
    fn test_check_constraints_empty_list_passes() {
        let ctx = make_meta_context(Vec::new(), Vec::new(), "");
        let result = ConstraintEngine::check_constraints(&ctx, &[]);
        assert!(result.passed);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn test_check_constraints_no_fabrication_pass() {
        let ctx = make_meta_context(
            vec![hard_truth("truth:evidence-based", "有依据推理", "evidence")],
            Vec::new(),
            "test task",
        );
        let constraints = vec![hard_truth(
            "truth:no-fabrication",
            "不编造事实",
            "no fabrications",
        )];
        let result = ConstraintEngine::check_constraints(&ctx, &constraints);
        assert!(result.passed);
    }

    #[test]
    fn test_check_constraints_no_fabrication_fail_hard() {
        let ctx = make_meta_context(Vec::new(), Vec::new(), "test task");
        let constraints = vec![hard_truth(
            "truth:no-fabrication",
            "不编造事实",
            "no fabrications",
        )];
        let result = ConstraintEngine::check_constraints(&ctx, &constraints);
        assert!(!result.passed);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].truth_id, "truth:no-fabrication");
        assert_eq!(result.violations[0].severity, ConstraintSeverity::Hard);
    }

    #[test]
    fn test_check_constraints_soft_does_not_short_circuit() {
        let ctx = make_meta_context(Vec::new(), Vec::new(), "");
        let constraints = vec![soft_truth("truth:auditable", "透明可审计", "auditability")];
        let result = ConstraintEngine::check_constraints(&ctx, &constraints);
        // Soft violations are still reported, but passed = false when any exists
        assert!(!result.passed);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].severity, ConstraintSeverity::Soft);
    }

    #[test]
    fn test_check_constraints_hard_short_circuits_before_soft() {
        // Hard constraint comes first, so soft is never checked
        let ctx = make_meta_context(Vec::new(), Vec::new(), "");
        let constraints = vec![
            hard_truth("truth:no-fabrication", "不编造事实", ""),
            soft_truth("truth:auditable", "透明可审计", ""),
        ];
        let result = ConstraintEngine::check_constraints(&ctx, &constraints);
        assert!(!result.passed);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].truth_id, "truth:no-fabrication");
    }

    #[test]
    fn test_check_constraints_evidence_based_pass() {
        let ctx = make_meta_context(
            vec![hard_truth("truth:evidence-based", "有依据推理", "evidence")],
            Vec::new(),
            "test",
        );
        let constraints = vec![hard_truth(
            "truth:evidence-based",
            "有依据推理",
            "evidence",
        )];
        let result = ConstraintEngine::check_constraints(&ctx, &constraints);
        assert!(result.passed);
    }

    #[test]
    fn test_check_constraints_evidence_based_fail() {
        let ctx = make_meta_context(Vec::new(), Vec::new(), "test");
        let constraints = vec![hard_truth(
            "truth:evidence-based",
            "有依据推理",
            "evidence",
        )];
        let result = ConstraintEngine::check_constraints(&ctx, &constraints);
        assert!(!result.passed);
    }

    #[test]
    fn test_check_causal_output_empty_summary_fails_hard() {
        let constraints = vec![hard_truth(
            "truth:no-fabrication",
            "不编造事实",
            "no fab",
        )];
        let result =
            ConstraintEngine::check_causal_output("", &[], &constraints);
        assert!(!result.passed);
    }

    #[test]
    fn test_check_causal_output_non_empty_summary_passes() {
        let constraints = vec![hard_truth(
            "truth:no-fabrication",
            "不编造事实",
            "no fab",
        )];
        let result = ConstraintEngine::check_causal_output(
            "Analysis complete: all constraints satisfied.",
            &[],
            &constraints,
        );
        assert!(result.passed);
    }

    #[test]
    fn test_check_causal_output_code_safety_detected() {
        let constraints = vec![hard_truth(
            "truth:code-safety",
            "代码安全",
            "code safety",
        )];
        let violations = vec!["Unsafe code detected in module X".into()];
        let result = ConstraintEngine::check_causal_output(
            "Analysis complete.",
            &violations,
            &constraints,
        );
        assert!(!result.passed);
        assert_eq!(result.violations[0].truth_id, "truth:code-safety");
    }

    #[test]
    fn test_empty_constraints_passes_immediately() {
        let result = ConstraintEngine::check_causal_output("", &[], &[]);
        assert!(result.passed);
    }
}
