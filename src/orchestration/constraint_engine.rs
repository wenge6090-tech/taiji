//! ConstraintEngine — L4 硬约束运行时执行（V38 起内置化，不再读归藏 truths/）。
//! 两个集成点：
//!   1. `load_truths()`  — 硬编码基线约束（不编造事实/有依据推理/可审计 + code-safety）
//!   2. `check_constraints()` / `check_yin_output()` — YinAgent LLM 调用前的 L0 检查
//!
//! V38：truths 资产层已移除——约束不再资产化、不参与 Lianshan 演化；
//! 硬约束 = 本引擎内置的硬编码检查（summary 非空/有依据/可审计 + code-safety）。

use crate::types::ontology::{OntologyEdge, OntologyEdgeKind, OntologyRule};
use crate::types::verification::{
    CheckKind, CheckResult, CheckSeverity, CheckSpec, CheckStats, ConstraintResult,
    ConstraintSeverity, ConstraintViolation, TruthConstraint, TruthStatus,
};
use std::path::Path;
use tokio::fs;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

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
    /// V62 分层：只产 Rust 宪法（元层 4 truth）。挖掘规则与人工条文已降经验层
    /// （走 LLM 兑底措辞与先验注入），永不进此机械对碰路径。
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

    /// 因果对碰（V57 判断依据·因果层，纯符号，MVP）：任务实体（MetaContext
    /// .ontology_objects）命中的 type→type 边 → 因果依赖清单，注入阴 LLM 兜底
    /// prompt 作为判断依据。
    ///
    /// 实体到「阳产出」的机械链接延后（需产出实体链接，MVP 以因果先验注入
    /// LLM 兜底而非机械裁决——防误杀）；机械裁决由 rules（load_truths）+ 原子
    /// 判据（check_atomics）承担。
    pub fn match_relations(entities: &[String], edges: &[OntologyEdge]) -> Vec<String> {
        let mut hints = Vec::new();
        for obj in entities {
            for e in edges {
                if e.from == *obj {
                    hints.push(match e.kind {
                        OntologyEdgeKind::WeakDependency => format!("{} 依赖 {}", e.from, e.to),
                        OntologyEdgeKind::Sequence => format!("{} 先于 {}", e.from, e.to),
                    });
                } else if e.to == *obj {
                    hints.push(match e.kind {
                        OntologyEdgeKind::WeakDependency => format!("{} 是 {} 的依赖", obj, e.from),
                        OntologyEdgeKind::Sequence => format!("{} 晚于 {}", obj, e.from),
                    });
                }
            }
        }
        hints
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

// ---------------------------------------------------------------------------
// V57 系统运行保障：无条件恒在的 Rust 原子判据（非 skill，独立于 ontology 因果）
// ---------------------------------------------------------------------------
//
// 从 SkillEngine 迁入（V57：阴不跑 SkillEngine，原子判据落约束引擎）。
// 这些判据保证系统 invariant（产出真实、任务册合法、引用可解析、证据可追溯），
// 与 ontology 因果（判断依据）正交——即使 rules/relations 为空也执行。

/// 契约命令白名单前缀（MVP-1：仅编译/测试/静态检查类安全命令）。
const COMMAND_ALLOWLIST: &[&str] = &[
    "cargo check",
    "cargo test --no-run",
    "rustc --emit=metadata",
];

/// 命令执行超时（30s）。
const COMMAND_TIMEOUT_SECS: u64 = 30;

/// 单检查项输出截断上限（2KB）。
const OUTPUT_TRUNCATE: usize = 2048;

/// 系统运行保障：无条件恒在的 Rust 原子判据（V57）。
///
/// 判据 = 元层种子默认配置（file-exists/schema-valid/reference-resolves/
/// trace-consistency；command-succeeds 默认空 command 不执行）。这些判据保证
/// 系统 invariant，独立于 ontology 因果——即使 rules/relations 为空也执行
/// （防「垃圾进垃圾出」）。Hard 失败由调用方（YinJudge）短路。
///
/// 返回（结果清单, 是否有 hard 失败）。
pub async fn check_atomics(task_dir: &Path) -> (Vec<CheckResult>, bool) {
    let specs = vec![
        CheckSpec {
            id: "file-exists#0".into(),
            kind: CheckKind::FileExists,
            target: "deliverables/*".into(),
            params: serde_json::json!({}),
            severity: CheckSeverity::Hard,
            pass_condition: "deliverables/ 下至少存在一个文件".into(),
            stats: CheckStats::default(),
        },
        CheckSpec {
            id: "schema-valid#0".into(),
            kind: CheckKind::SchemaValid,
            target: "meta.json".into(),
            params: serde_json::json!({"format": "json", "required_fields": ["id","description","depth","status"]}),
            severity: CheckSeverity::Hard,
            pass_condition: "meta.json 可解析且含 id/description/depth/status".into(),
            stats: CheckStats::default(),
        },
        CheckSpec {
            id: "reference-resolves#0".into(),
            kind: CheckKind::ReferenceResolves,
            target: "deliverables/handoff.md".into(),
            params: serde_json::json!({"field": "output_refs"}),
            severity: CheckSeverity::Soft,
            pass_condition: "output_refs 内每个路径均存在".into(),
            stats: CheckStats::default(),
        },
        CheckSpec {
            id: "trace-consistency#0".into(),
            kind: CheckKind::TraceConsistency,
            target: "deliverables/*.md".into(),
            params: serde_json::json!({
                "evidence_pattern": "[证据: {tool}]",
                "speculation_marker": "(推测)",
                "allowed_tools": ["webfetch","search","read","bash","write"],
                "trace_glob": "trace.jsonl",
            }),
            severity: CheckSeverity::Soft,
            pass_condition: "产出中 [证据: 工具名] 引用必须在 trace.jsonl 中存在".into(),
            stats: CheckStats::default(),
        },
    ];
    let mut results = Vec::with_capacity(specs.len());
    let mut hard_failed = false;
    for spec in &specs {
        let result = run_check(spec, task_dir).await;
        if !result.passed && spec.severity == CheckSeverity::Hard {
            hard_failed = true;
        }
        results.push(result);
    }
    (results, hard_failed)
}

/// 执行单个检查项（V57 迁入，原 SkillEngine::run_check）。
pub async fn run_check(spec: &CheckSpec, task_dir: &Path) -> CheckResult {
    let start = std::time::Instant::now();
    let (passed, detail) = match spec.kind {
        CheckKind::FileExists => check_file_exists(spec, task_dir).await,
        CheckKind::SchemaValid => check_schema_valid(spec, task_dir).await,
        CheckKind::ReferenceResolves => check_reference_resolves(spec, task_dir).await,
        CheckKind::CommandSucceeds => check_command_succeeds(spec, task_dir).await,
        CheckKind::LlmJudgement => {
            (true, "llm_judgement — deferred to LLM (L2)".to_string())
        }
        CheckKind::TraceConsistency => check_trace_consistency(spec, task_dir).await,
        CheckKind::Python => {
            (true, "python — executed via python_engine (not run_check)".to_string())
        }
    };
    CheckResult {
        check_id: spec.id.clone(),
        kind: spec.kind,
        passed,
        detail,
        duration_ms: start.elapsed().as_millis() as u64,
        cost_tokens: 0,
        verify_rounds: 0,
        quality: 0.0,
    }
}

/// 命令是否通过白名单（MVP-1：前缀匹配 + 禁止 shell 元字符）。
pub fn is_command_allowed(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return false;
    }
    if ["&&", "||", ";", "`", ">", "<", "|", "$("]
        .iter()
        .any(|meta| trimmed.contains(meta))
    {
        return false;
    }
    COMMAND_ALLOWLIST
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

// ── 原子判据实现（L0 机械验证，V57 迁入） ──────────────────────────────

async fn check_file_exists(spec: &CheckSpec, task_dir: &Path) -> (bool, String) {
    let target = spec.target.trim();
    if target.is_empty() {
        return (false, "empty target".to_string());
    }

    // 路径穿越防护（契约 target 禁止离开 task_dir）
    if contains_path_traversal(target) {
        return (false, format!("path traversal in target: {target}"));
    }

    if let Some((dir_part, pattern)) = split_glob_last_segment(target) {
        let dir_path = task_dir.join(dir_part);
        let mut entries = match fs::read_dir(&dir_path).await {
            Ok(e) => e,
            Err(e) => {
                return (false, format!("cannot read directory {:?}: {e}", dir_path));
            }
        };
        let (prefix, suffix) = pattern
            .split_once('*')
            .map(|(p, s)| (p.to_string(), s.to_string()))
            .unwrap_or((String::new(), pattern.to_string()));
        let mut matched = false;
        while let Some(entry) = entries.next_entry().await.transpose() {
            match entry {
                Ok(e) => {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with(&prefix) && name.ends_with(&suffix) {
                        matched = true;
                        break;
                    }
                }
                Err(e) => {
                    return (false, format!("error reading directory entry: {e}"));
                }
            }
        }
        if matched {
            (true, format!("matched '{target}'"))
        } else {
            (false, format!("no file matching '{target}'"))
        }
    } else {
        let path = task_dir.join(target);
        match fs::try_exists(&path).await {
            Ok(true) => (true, format!("exists: {}", path.display())),
            Ok(false) => (false, format!("not found: {}", path.display())),
            Err(e) => (false, format!("cannot stat {:?}: {e}", path)),
        }
    }
}

async fn check_schema_valid(spec: &CheckSpec, task_dir: &Path) -> (bool, String) {
    let target = spec.target.trim();
    if contains_path_traversal(target) {
        return (false, format!("path traversal in target: {target}"));
    }
    let path = task_dir.join(target);

    let format = spec
        .params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");
    let required_fields: Vec<String> = spec
        .params
        .get("required_fields")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let content = match fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return (false, format!("cannot read {:?}: {e}", path)),
    };

    let value: serde_json::Value = match format {
        "yaml" => match serde_yaml::from_str::<serde_json::Value>(&content) {
            Ok(v) => v,
            Err(e) => return (false, format!("YAML parse failed: {e}")),
        },
        _ => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(v) => v,
            Err(e) => return (false, format!("JSON parse failed: {e}")),
        },
    };

    for field in &required_fields {
        if field_path_get(&value, field).is_none() {
            return (false, format!("missing required field '{field}'"));
        }
    }

    (true, format!("schema valid ({format})"))
}

async fn check_reference_resolves(spec: &CheckSpec, task_dir: &Path) -> (bool, String) {
    let target = spec.target.trim();
    if contains_path_traversal(target) {
        return (false, format!("path traversal in target: {target}"));
    }
    let path = task_dir.join(target);

    let field = spec
        .params
        .get("field")
        .and_then(|v| v.as_str())
        .unwrap_or("output_refs");

    let content = match fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return (false, format!("cannot read {:?}: {e}", path)),
    };

    // 解析 YAML front matter（`---` 围栏）；无围栏时取首个文档分隔符前的块（手写
    // handoff 场景）。提取失败 → 判据不适用（普通任务成功路径无 output_refs 数据源），
    // 跳过而非 FAIL——照单 FAIL 会把「产出正确但 handoff 非标准格式」的任务推进
    // BackToZhouyi 死循环（实测：LLM 手写 handoff → reference-resolves Soft-FAIL
    // → 语义裁决必 BackToZhouyi → 每轮重写同格式 → 永不过）。
    let yaml_block = extract_first_yaml_block(&content).unwrap_or(content.as_str());
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml_block) {
        Ok(v) => v,
        Err(e) => {
            return (
                true,
                format!("front matter not parseable — check skipped ({e})"),
            )
        }
    };

    let refs = value
        .get(field)
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if refs.is_empty() {
        // 无 output_refs：判据不适用（普通任务成功路径程序不写权威 handoff，
        // output_refs 仅编译任务/失败恢复路径存在）——跳过而非 FAIL。
        return (
            true,
            "no output_refs in front matter — check skipped".into(),
        );
    }

    let mut missing = Vec::new();
    for reference in &refs {
        let ref_path = if Path::new(reference).is_absolute() {
            Path::new(reference).to_path_buf()
        } else {
            task_dir.join(reference)
        };
        match fs::try_exists(&ref_path).await {
            Ok(true) => {}
            _ => missing.push(reference.clone()),
        }
    }

    if missing.is_empty() {
        (true, format!("all {} references resolve", refs.len()))
    } else {
        (false, format!("unresolved references: {}", missing.join(", ")))
    }
}

// ────────────────────────────────────────────────────────────────
// Hodge 三模态病理诊断（V65，信息几何第 34 章 Hodge 分解）
// ────────────────────────────────────────────────────────────────

/// 旋度判打转阈值（V65 定论：极端值才判打转，防误杀正常详述）。
pub const HODGE_CURL_SPIN_THRESHOLD: f64 = 0.8;
/// 调和判新意阈值：与归藏资产重叠低于此值 = 跨域新意。
pub const HODGE_NOVEL_OVERLAP_THRESHOLD: f64 = 0.5;
/// 调和联合判定的梯度门槛：只有逻辑自洽（梯度高）才认顿悟。
pub const HODGE_NOVEL_GRADIENT_THRESHOLD: f64 = 0.8;

/// Hodge 三模态病理诊断（V65）——纯符号，零 LLM。
///
/// - **梯度**：引用图连通率 [0,1]——复用 reference-resolves 的 output_refs 解析，
///   引用文件存在比例。无引用可验（handoff 缺失/无 output_refs）= 1.0（无断裂证据，
///   §30 定论：无引用可验 = 状态分支跳过，不是 FAIL）。
/// - **旋度**：句子 n-gram Jaccard 重叠均值 [0,1]——重复论证同一前提 = 打转。
/// - **调和**：跨域新意 = 1 − 产出与归藏资产的 n-gram 重叠。**必须梯度联合**：
///   低相似 + 高梯度 = 顿悟（novel）；低相似 + 低梯度 = 跑题（novel=false）。
///
/// 诊断永远不进机械对碰（律令层 Rust 宪法专属，V58 晶体二值不动）——
/// 三维软信号只指导路由（zhouyi）与统计。
pub async fn hodge_diagnose(
    task_dir: &Path,
    output: &str,
    assets_text: &[String],
) -> crate::types::verification::HodgeDiagnosis {
    let gradient = reference_connectivity(task_dir).await;
    let curl = self_similarity(output);
    let overlap = cross_overlap(output, assets_text);
    let harmonic = 1.0 - overlap.min(1.0);
    let novel = harmonic > HODGE_NOVEL_OVERLAP_THRESHOLD && gradient > HODGE_NOVEL_GRADIENT_THRESHOLD;
    crate::types::verification::HodgeDiagnosis {
        gradient,
        curl,
        harmonic,
        novel,
    }
}

/// 梯度分量：引用图连通率（复用 reference-resolves 解析：handoff.md output_refs）。
async fn reference_connectivity(task_dir: &Path) -> f64 {
    let handoff = task_dir.join("deliverables").join("handoff.md");
    let content = match fs::read_to_string(&handoff).await {
        Ok(c) => c,
        Err(_) => return 1.0, // 无 handoff = 无引用可验 = 无断裂证据
    };
    let yaml_block = extract_first_yaml_block(&content).unwrap_or(content.as_str());
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml_block) {
        Ok(v) => v,
        Err(_) => return 1.0, // front matter 不可解析 = 跳过
    };
    let refs: Vec<String> = value
        .get("output_refs")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if refs.is_empty() {
        return 1.0; // 无 output_refs = 状态分支跳过（§30）
    }
    let mut resolved = 0usize;
    for r in &refs {
        let path = if Path::new(r).is_absolute() {
            Path::new(r).to_path_buf()
        } else {
            task_dir.join(r)
        };
        if fs::try_exists(&path).await.unwrap_or(false) {
            resolved += 1;
        }
    }
    resolved as f64 / refs.len() as f64
}

/// 句子切分（换行 / 中文标点 / 英文句末）。
fn split_sentences(text: &str) -> Vec<&str> {
    text.split(|c: char| {
        matches!(c, '\n' | '\r' | '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';')
    })
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect()
}

/// 字符 n-gram 集合（n=2，中文/英文通用，零分词依赖）。
fn char_ngrams(text: &str, n: usize) -> std::collections::HashSet<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < n {
        return [text.to_string()].into_iter().collect();
    }
    chars
        .windows(n)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

/// Jaccard 相似度（空集/交集空 → 0）。
fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// 旋度分量：句子级自相似度（两两 Jaccard 平均；单句/空文 → 0）。
fn self_similarity(output: &str) -> f64 {
    let sentences = split_sentences(output);
    if sentences.len() < 2 {
        return 0.0;
    }
    let grams: Vec<_> = sentences.iter().map(|s| char_ngrams(s, 2)).collect();
    let mut total = 0.0;
    let mut pairs = 0usize;
    for i in 0..grams.len() {
        for j in (i + 1)..grams.len() {
            total += jaccard(&grams[i], &grams[j]);
            pairs += 1;
        }
    }
    if pairs == 0 {
        0.0
    } else {
        total / pairs as f64
    }
}

/// 调和分量：产出与资产文本的 n-gram 重叠（资产为空 → 0 重叠 = 全盘新意）。
fn cross_overlap(output: &str, assets_text: &[String]) -> f64 {
    if assets_text.is_empty() {
        return 0.0;
    }
    let out_grams = char_ngrams(output, 2);
    let mut inter = 0usize;
    let mut union = out_grams.len();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for asset in assets_text {
        for g in char_ngrams(asset, 2) {
            if seen.insert(g.clone()) {
                if out_grams.contains(&g) {
                    inter += 1;
                } else {
                    union += 1;
                }
            }
        }
    }
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

async fn check_trace_consistency(spec: &CheckSpec, task_dir: &Path) -> (bool, String) {
    let trace_glob = spec
        .params
        .get("trace_glob")
        .and_then(|v| v.as_str())
        .unwrap_or("trace.jsonl");
    let trace_path = task_dir.join(trace_glob);
    let allowed: Vec<String> = spec
        .params
        .get("allowed_tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_else(|| {
            ["webfetch", "search", "read", "bash"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    let speculation_marker = spec
        .params
        .get("speculation_marker")
        .and_then(|v| v.as_str())
        .unwrap_or("(推测)");

    // 1. trace 工具索引（tool_call::* 事件，去重）
    let mut tool_index: Vec<String> = Vec::new();
    if trace_path.is_file() {
        if let Ok(content) = tokio::fs::read_to_string(&trace_path).await {
            for line in content.lines() {
                let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let Some(phase) = record.get("phase").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(tool) = phase.strip_prefix("tool_call::") else {
                    continue;
                };
                if !allowed.iter().any(|t| t == tool) {
                    continue;
                }
                if !tool_index.iter().any(|t| t == tool) {
                    tool_index.push(tool.to_string());
                }
            }
        }
    }

    // 2. 产出断言扫描（deliverables 目录，target glob）
    let target = spec.target.trim();
    if target.is_empty() {
        return (false, "empty target".to_string());
    }
    if contains_path_traversal(target) {
        return (false, format!("path traversal in target: {target}"));
    }
    let deliverables_dir = task_dir.join("deliverables");
    let mut evidence_refs: Vec<(String, String)> = Vec::new();
    let mut speculation_count: u64 = 0;
    let mut scanned_files: u64 = 0;
    if deliverables_dir.is_dir() {
        let mut entries = match tokio::fs::read_dir(&deliverables_dir).await {
            Ok(e) => e,
            Err(_) => {
                return (false, "failed to read deliverables dir".to_string());
            }
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            let matched = if let Some((dir_part, pattern)) = split_glob_last_segment(target) {
                if !dir_part.is_empty() && dir_part != "deliverables" {
                    continue;
                }
                basename_glob_match(&name, &pattern)
            } else {
                name == target || format!("deliverables/{name}") == target
            };
            if !matched {
                continue;
            }
            scanned_files += 1;
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            speculation_count += content.matches(speculation_marker).count() as u64;
            for line in content.lines() {
                let mut rest = line;
                while let Some(start) = rest.find("[证据:") {
                    let after = &rest[start..];
                    let Some(close) = after.find(']') else {
                        break;
                    };
                    let inner = after[..close].trim_start_matches("[证据:");
                    let tool = inner.trim().trim_end_matches(']').trim();
                    if !tool.is_empty()
                        && tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        evidence_refs.push((tool.to_string(), after[..=close].to_string()));
                    }
                    rest = &after[close.saturating_add(1).min(after.len())..];
                }
            }
        }
    }

    // 3. 校验：每个证据引用必须在 trace 索引中
    let missing: Vec<String> = evidence_refs
        .iter()
        .filter(|(tool, _)| !tool_index.iter().any(|t| t == tool))
        .map(|(tool, raw)| format!("{tool} ({raw})"))
        .collect();

    let mut detail = format!(
        "evidence refs: {}, speculation markers: {}, scanned files: {}",
        evidence_refs.len(),
        speculation_count,
        scanned_files
    );
    if !missing.is_empty() {
        detail = format!(
            "{detail}; UNVERIFIED evidence — tool call not found in trace: [{}]",
            missing.join(", ")
        );
        return (false, detail);
    }
    (true, detail)
}

async fn check_command_succeeds(spec: &CheckSpec, task_dir: &Path) -> (bool, String) {
    let command = spec
        .params
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if command.is_empty() {
        return (false, "params.command missing".to_string());
    }
    if !is_command_allowed(&command) {
        return (false, format!("command not in allowlist (AGENTS.md): {command}"));
    }

    let mut parts = command.split_whitespace();
    let program = match parts.next() {
        Some(p) => p.to_string(),
        None => return (false, "empty command".to_string()),
    };
    let args: Vec<String> = parts.map(String::from).collect();

    let mut cmd = Command::new(&program);
    cmd.args(&args).current_dir(task_dir);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = match timeout(Duration::from_secs(COMMAND_TIMEOUT_SECS), cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return (false, format!("failed to spawn {program}: {e}")),
        Err(_) => return (false, format!("command timed out after {COMMAND_TIMEOUT_SECS}s")),
    };

    if output.status.success() {
        (true, format!("{command} exited 0"))
    } else {
        let mut detail = String::new();
        if !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            detail.push_str(&truncate(&stdout, OUTPUT_TRUNCATE));
        }
        if !output.stderr.is_empty() {
            if !detail.is_empty() {
                detail.push_str("\n--- stderr ---\n");
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            detail.push_str(&truncate(&stderr, OUTPUT_TRUNCATE));
        }
        if detail.is_empty() {
            detail = format!("{command} exited {}", output.status.code().unwrap_or(-1));
        }
        (false, detail)
    }
}

// ── 内部工具函数（V57 迁入） ───────────────────────────────────────────

fn contains_path_traversal(target: &str) -> bool {
    target.split('/').any(|seg| seg == "..")
}

fn basename_glob_match(name: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            name.starts_with(prefix)
                && name.ends_with(suffix)
                && name.len() >= prefix.len() + suffix.len()
        }
        None => name == pattern,
    }
}

fn split_glob_last_segment(target: &str) -> Option<(String, String)> {
    let (dir_part, last) = match target.rsplit_once('/') {
        Some((d, l)) => (d, l),
        None => ("", target),
    };
    if last.contains('*') {
        Some((dir_part.to_string(), last.to_string()))
    } else {
        None
    }
}

fn extract_front_matter(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    Some(rest[..end].trim())
}

/// front matter 缺失时回退到「首个 YAML 文档分隔符前」的块。
/// 手写 handoff（无 `---` 围栏）若含 `---` 水平线，直接全文解析会触发
/// serde_yaml「multiple documents」误导性报错——先截到第一处分隔符。
fn extract_first_yaml_block(content: &str) -> Option<&str> {
    if let Some(fm) = extract_front_matter(content) {
        return Some(fm);
    }
    let trimmed = content.trim_start();
    let pos = trimmed.find("\n---")?;
    Some(trimmed[..pos].trim())
}

fn field_path_get<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("…[truncated]");
        out
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

    /// V62 分层：load_truths 永不消费挖掘规则/人工条文（只 Rust 宪法）。
    #[test]
    fn test_load_truths_never_consumes_ontology_rules() {
        let tags: Vec<String> = vec![];
        let truths = ConstraintEngine::load_truths(&tags);
        assert_eq!(truths.len(), 3); // 只 3 元层 truth，无 ontology: 前缀
        assert!(!truths.iter().any(|t| t.id.starts_with("ontology:")));
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

    // ── V65 Hodge 三模态诊断 ──

    #[tokio::test]
    async fn hodge_gradient_detects_broken_references() {
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let dl = dir.path().join("deliverables");
        fs::create_dir_all(&dl).unwrap();
        // 引用 2 个文件，只存在 1 个 → gradient = 0.5
        fs::write(dl.join("exists.md"), "x").unwrap();
        fs::write(
            dl.join("handoff.md"),
            "---\noutput_refs:\n  - deliverables/exists.md\n  - deliverables/missing.md\n---\n",
        )
        .unwrap();
        let d = hodge_diagnose(dir.path(), "output", &[]).await;
        assert!((d.gradient - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn hodge_gradient_skips_without_handoff() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let d = hodge_diagnose(dir.path(), "output", &[]).await;
        assert_eq!(d.gradient, 1.0, "无 handoff = 无断裂证据 = 跳过");
    }

    #[test]
    fn hodge_curl_detects_repetition() {
        // 同一句话重复多遍 → 自相似度极高
        let repetitive = "写一个文件。\n写一个文件。\n写一个文件。\n写一个文件。";
        let diverse = "分析需求。\n实现功能。\n运行测试。\n修复缺陷。";
        let d_rep = hodge_diagnose_parts(repetitive, &[]);
        let d_div = hodge_diagnose_parts(diverse, &[]);
        assert!(d_rep.curl > 0.8, "重复文本 curl={}", d_rep.curl);
        assert!(d_rep.curl > d_div.curl, "重复 > 多样");
    }

    #[test]
    fn hodge_novel_joint_condition() {
        // 产出与资产完全不重叠 + 梯度高（无引用可验 → 1.0）→ novel
        let d = hodge_diagnose_parts("全新的产出内容", &["部署安全审查清单".to_string()]);
        assert!(d.harmonic > HODGE_NOVEL_OVERLAP_THRESHOLD);
        assert!(d.novel, "低相似 + 高梯度 = 顿悟");

        // 产出与资产高度重叠 → 非 novel
        let d2 = hodge_diagnose_parts(
            "部署安全审查清单部署安全审查",
            &["部署安全审查清单".to_string()],
        );
        assert!(!d2.novel, "高重叠 = 常规产出");
    }

    /// 同步测试辅助：梯度按「无 handoff = 1.0」固定，只测旋度/调和。
    fn hodge_diagnose_parts(
        output: &str,
        assets_text: &[String],
    ) -> crate::types::verification::HodgeDiagnosis {
        let curl = self_similarity(output);
        let overlap = cross_overlap(output, assets_text);
        let harmonic = 1.0 - overlap.min(1.0);
        let novel =
            harmonic > HODGE_NOVEL_OVERLAP_THRESHOLD && 1.0 > HODGE_NOVEL_GRADIENT_THRESHOLD;
        crate::types::verification::HodgeDiagnosis {
            gradient: 1.0,
            curl,
            harmonic,
            novel,
        }
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

    /// V57 因果对碰：任务实体命中 type→type 边 → 因果依赖清单。
    #[test]
    fn test_match_relations_causal_hints() {
        use crate::types::ontology::{OntologyEdge, OntologyEdgeKind};
        let edges = vec![OntologyEdge {
            from: "deploy-action".into(),
            to: "security-check".into(),
            kind: OntologyEdgeKind::WeakDependency,
            strength: 0.9,
            samples: 60,
            evidence: vec![],
        }];
        let entities = vec!["deploy-action".to_string()];
        let hints = ConstraintEngine::match_relations(&entities, &edges);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].contains("依赖"));

        // 空实体 → 无命中
        assert!(ConstraintEngine::match_relations(&[], &edges).is_empty());
    }

    /// V57 运行保障：原子判据无条件恒在，空 deliverables → file-exists hard 失败。
    #[tokio::test]
    async fn test_check_atomics_hard_failure_on_empty_deliverables() {
        let dir = std::env::temp_dir().join(format!(
            "taiji-atomic-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let (results, hard_failed) = check_atomics(&dir).await;
        assert!(hard_failed, "空 task_dir 应触发 file-exists hard 失败");
        assert!(results.iter().any(|r| r.check_id == "file-exists#0" && !r.passed));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V57 死循环修复 1：trace-consistency 白名单必须含 write——
    /// `tool_call::write`（builtin skill 执行路径）是核心证据，白名单遗漏导致
    /// 手写 [证据: write] 永远不可验证 → soft-FAIL → BackToZhouyi 死循环。
    #[tokio::test]
    async fn test_trace_consistency_verified_with_write_tool() {
        let dir = std::env::temp_dir().join(format!(
            "taiji-trace-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let deliverables = dir.join("deliverables");
        tokio::fs::create_dir_all(&deliverables).await.unwrap();
        // 产物必须是 .md（spec target 是 deliverables/*.md）才会被证据扫描覆盖。
        tokio::fs::write(
            deliverables.join("handoff.md"),
            "# 交接\n[证据: write]",
        )
        .await
        .unwrap();
        // trace 含 write 工具调用（真实 hook 产生的 phase 命名）。
        tokio::fs::write(
            dir.join("trace.jsonl"),
            serde_json::json!({"phase": "tool_call::write", "input": {"path": "deliverables/hello.txt"}, "output": {"status": "ok"}})
                .to_string()
                + "\n",
        )
        .await
        .unwrap();
        let spec = CheckSpec {
            id: "trace-consistency#0".into(),
            kind: CheckKind::TraceConsistency,
            target: "deliverables/*.md".into(),
            params: serde_json::json!({
                "evidence_pattern": "[证据: {tool}]",
                "speculation_marker": "(推测)",
                "artifact_glob": "deliverables/*",
                "allowed_tools": ["webfetch","search","read","bash","write"],
                "trace_glob": "trace.jsonl",
            }),
            severity: CheckSeverity::Soft,
            pass_condition: "".into(),
            stats: CheckStats::default(),
        };
        let result = run_check(&spec, &dir).await;
        assert!(
            result.passed,
            "write 证据应可验证，实际: {}",
            result.detail
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V57 死循环修复 2：reference-resolves 对「无 output_refs」状态分支跳过而非 FAIL——
    /// 普通任务成功路径无权威 handoff（output_refs 仅编译/恢复路径存在），照单 FAIL = 死循环。
    #[tokio::test]
    async fn test_reference_resolves_skips_without_output_refs() {
        let dir = std::env::temp_dir().join(format!(
            "taiji-ref-skip-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir.join("deliverables")).await.unwrap();
        // 手写 handoff：front matter 存在但无 output_refs。
        tokio::fs::write(
            dir.join("deliverables/handoff.md"),
            "---\ntask: 写一个文件\nstatus: completed\n---\n# 正文\n",
        )
        .await
        .unwrap();
        let spec = CheckSpec {
            id: "reference-resolves#0".into(),
            kind: CheckKind::ReferenceResolves,
            target: "deliverables/handoff.md".into(),
            params: serde_json::json!({"field": "output_refs"}),
            severity: CheckSeverity::Soft,
            pass_condition: "".into(),
            stats: CheckStats::default(),
        };
        let result = run_check(&spec, &dir).await;
        assert!(
            result.passed,
            "无 output_refs 应跳过而非 FAIL，实际: {}",
            result.detail
        );
        assert!(result.detail.contains("skip"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// reference-resolves 有 output_refs 时仍严格验证（编译任务契约 §26 保持）：
    /// 引用存在 → passed；引用缺失 → FAIL。
    #[tokio::test]
    async fn test_reference_resolves_still_validates_output_refs() {
        let dir = std::env::temp_dir().join(format!(
            "taiji-ref-valid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir.join("deliverables")).await.unwrap();
        tokio::fs::write(dir.join("deliverables/skill.yaml"), "x").await.unwrap();
        // 有 output_refs：一个存在一个缺失。
        tokio::fs::write(
            dir.join("deliverables/handoff.md"),
            "---\noutput_refs:\n  - deliverables/skill.yaml\n  - deliverables/ghost.txt\n---\n",
        )
        .await
        .unwrap();
        let spec = CheckSpec {
            id: "reference-resolves#0".into(),
            kind: CheckKind::ReferenceResolves,
            target: "deliverables/handoff.md".into(),
            params: serde_json::json!({"field": "output_refs"}),
            severity: CheckSeverity::Soft,
            pass_condition: "".into(),
            stats: CheckStats::default(),
        };
        let result = run_check(&spec, &dir).await;
        assert!(
            !result.passed,
            "output_refs 含缺失引用应 FAIL，实际: {}",
            result.detail
        );
        assert!(result.detail.contains("ghost.txt"));
        // 补齐缺失引用 → passed。
        tokio::fs::write(dir.join("deliverables/ghost.txt"), "x").await.unwrap();
        let result = run_check(&spec, &dir).await;
        assert!(result.passed, "引用全部存在应通过: {}", result.detail);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
