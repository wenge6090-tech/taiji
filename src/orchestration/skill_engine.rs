//! SkillEngine — 验证 Skill 机械执行器（V43 归藏 Skills 子树对齐 BCP §10.1-10.2）。
//!
//! 验证三权分立（BCP §6.6/§8.22）的 L0 机械验证 + L1 Skill 验证层：
//!
//! - **L0 机械验证**：file_exists / schema_valid / reference_resolves /
//!   command_succeeds 类检查项——确定性执行，零 LLM。
//! - **L1 Skill 验证**：加载 `yin/skills/verify/` + `yin/skills/converge/` 结构化 Skill，
//!   逐条机械执行 checks，产出 [`SkillReport`]。
//!
//! **确定性保证**：同一 Skill + 同一产出 → 同一结果（与 LLM 无关）。
//! **裁决优先级**：任一 hard 机械项失败 → `passed = false`，YinAgent
//! 直接短路，LLM 不可翻案（LLM 的 PASS 不能覆盖机械 FAIL）。
//!
//! SkillEngine 是 Rust 内部函数（非 LLM 工具）——LLM 不可调用、不可绕过。
//!
//! # 契约命令安全面（BCP §8.22 预埋）
//!
//! `command_succeeds` 类检查项仅允许白名单安全命令（编译/测试/静态检查），
//! 禁止任意 shell 命令进契约——防契约资产被污染后变成任意代码执行面。
//! 白名单与 SafetyHook 同源审批精神；命令经 `split_whitespace` 直接解析，
//! 不经过 shell（无元字符解释面）。

use crate::infra::error::TaijiError;
use crate::infra::knowledge::GuizangClient;
use crate::types::agent::VerificationAsset;
use crate::types::verification::{
    CheckKind, CheckResult, CheckSeverity, CheckSpec, SkillReport,
};
use std::path::Path;
use tokio::fs;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// 契约命令白名单前缀（MVP-1：仅编译/测试/静态检查类安全命令）。
/// 与 BCP §8.22「MVP-1 仅允许白名单安全命令」一致。
const COMMAND_ALLOWLIST: &[&str] = &[
    "cargo check",
    "cargo test --no-run",
    "rustc --emit=metadata",
];

/// 命令执行超时（30s）。
const COMMAND_TIMEOUT_SECS: u64 = 30;

/// 单检查项输出截断上限（2KB）。
const OUTPUT_TRUNCATE: usize = 2048;

/// V43 SkillEngine — 验证 Skill 机械执行器（unit struct，风格对齐
/// [`crate::orchestration::constraint_engine::ConstraintEngine`]）。
pub struct SkillEngine;

impl SkillEngine {
    /// 加载 `yin/skills/verify/` 全部 Skill 资产（V43 BCP §10.1）。
    ///
    /// MVP-1 直接全量加载（种子 Skill <10 条，无性能问题）。
    ///
    /// # Errors
    /// 目录级 I/O 失败上抛（无降级原则 — §8.20：归藏不可用是系统错误，
    /// 不静默吞掉）；单个契约文件损坏仅 warn 跳过（不影响其他契约执行）。
    pub async fn load_skills(
        guizang: &GuizangClient,
    ) -> Result<Vec<VerificationAsset>, TaijiError> {
        guizang.load_all_verifications().await
    }

    /// V43: 按 SkillCategory 加载全部 active Skill（BCP §10.1）。
    /// 委托给 [`GuizangClient::load_skills_by_category`]。
    pub async fn load_skills_by_category(
        guizang: &GuizangClient,
        category: crate::types::verification::SkillCategory,
    ) -> Result<Vec<VerificationAsset>, TaijiError> {
        guizang.load_skills_by_category(category).await
    }

    /// V45: 从合并视图加载统一 Skill 资产（元层 ∪ 资产层，同 id 优先）。
    /// 阴面类别使用；阳面类别返回空（阳面不做机械执行）。
    pub async fn load_skill_catalog(
        guizang: &GuizangClient,
        category: crate::types::verification::SkillCategory,
        profile: crate::infra::skill_catalog::ToolProfile,
    ) -> Result<Vec<crate::types::verification::SkillAsset>, TaijiError> {
        crate::infra::skill_catalog::load_skill_catalog(guizang, category, profile).await
    }

    /// 机械执行全部 Skill 的检查项，产出 [`SkillReport`]。
    ///
    /// - 仅执行机械类检查项（FileExists / SchemaValid / ReferenceResolves /
    ///   CommandSucceeds）；`LlmJudgement` 项跳过（由调用方 YinAgent
    ///   收集注入 LLM 裁决 — §6.6 L2）。
    /// - 任一 hard 机械项失败 → `passed = false`。
    /// - 串行执行（MVP-1 Skill 数量少）；单检查项内部失败（如文件读失败）
    ///   记为 failed 的 CheckResult，不整体报错——机械判定失败是结果而非
    ///   系统错误。
    pub async fn run_checks(
        skills: &[VerificationAsset],
        task_dir: &Path,
    ) -> SkillReport {
        let mut results = Vec::new();
        let mut hard_failed = false;

        for skill in skills {
            for check in &skill.checks {
                if check.kind == CheckKind::LlmJudgement {
                    // L2 项不参与机械裁决 — 由 YinAgent 收集（§6.6）
                    continue;
                }
                let result = Self::run_check(check, task_dir).await;
                if !result.passed && check.severity == CheckSeverity::Hard {
                    hard_failed = true;
                }
                results.push(result);
            }
        }

        let passed = !hard_failed;
        let failed_count = results.iter().filter(|r| !r.passed).count();
        let summary = if results.is_empty() {
            "no mechanical checks".to_string()
        } else if passed {
            format!("all {} mechanical checks passed", results.len())
        } else {
            format!(
                "{failed_count}/{} mechanical checks failed (hard short-circuit)",
                results.len()
            )
        };

        SkillReport {
            passed,
            results,
            summary,
        }
    }

    /// V45: 机械执行 SkillAsset 的阴面 implementations（FileExists/SchemaValid/
    /// ReferenceResolves/CommandSucceeds/TraceConsistency）；LlmJudgement 跳过
    /// （由 YinAgent 收集注入 LLM 裁决—§6.6 L2）；阳面 kind 跳过（非机械）。
    ///
    /// 等价旧 [`Self::run_checks`]（吃 VerificationAsset）的新接口——统一 SkillAsset
    /// 后的调用点入口；旧接口保留供过渡期单测。
    pub async fn run_checks_assets(
        skills: &[crate::types::verification::SkillAsset],
        task_dir: &Path,
    ) -> SkillReport {
        use crate::types::verification::SkillKind;
        let mut results = Vec::new();
        let mut hard_failed = false;

        for skill in skills {
            for (idx, impl_) in skill.implementations.iter().enumerate() {
                if !impl_.kind.is_yin() || impl_.kind == SkillKind::LlmJudgement {
                    // L2 项 + 阳面执行体不参与机械裁决。
                    continue;
                }
                // 元层 command-succeeds 默认 command 为空——跳过（避免 soft-fail 噪声）。
                // 资产层覆盖 params.command 后才会机械执行。
                if impl_.kind == SkillKind::CommandSucceeds {
                    let cmd = impl_
                        .params
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    if cmd.is_empty() {
                        continue;
                    }
                }
                let spec = impl_to_check_spec(&skill.id, idx, impl_);
                let result = Self::run_check(&spec, task_dir).await;
                if !result.passed && spec.severity == CheckSeverity::Hard {
                    hard_failed = true;
                }
                results.push(result);
            }
        }

        let passed = !hard_failed;
        let failed_count = results.iter().filter(|r| !r.passed).count();
        let summary = if results.is_empty() {
            "no mechanical checks".to_string()
        } else if passed {
            format!("all {} mechanical checks passed", results.len())
        } else {
            format!(
                "{failed_count}/{} mechanical checks failed (hard short-circuit)",
                results.len()
            )
        };

        SkillReport {
            passed,
            results,
            summary,
        }
    }
    /// 执行单个检查项（可单测）。
    pub async fn run_check(spec: &CheckSpec, task_dir: &Path) -> CheckResult {
        let start = std::time::Instant::now();
        let (passed, detail) = match spec.kind {
            CheckKind::FileExists => Self::check_file_exists(spec, task_dir).await,
            CheckKind::SchemaValid => Self::check_schema_valid(spec, task_dir).await,
            CheckKind::ReferenceResolves => Self::check_reference_resolves(spec, task_dir).await,
            CheckKind::CommandSucceeds => Self::check_command_succeeds(spec, task_dir).await,
            CheckKind::LlmJudgement => {
                // 防御性：run_checks 已跳过；直接调用时按未执行处理。
                (true, "llm_judgement — deferred to LLM (L2)".to_string())
            }
            CheckKind::TraceConsistency => {
                Self::check_trace_consistency(spec, task_dir).await
            }
        };
        CheckResult {
            check_id: spec.id.clone(),
            kind: spec.kind.clone(),
            passed,
            detail,
            duration_ms: start.elapsed().as_millis() as u64,
            // 机械检查零 token 成本；任务级信号（cost/rounds/quality）由 Zhouyi 入队层摊派
            cost_tokens: 0,
            verify_rounds: 0,
            quality: 0.0,
        }
    }

    /// 命令是否通过白名单（MVP-1：前缀匹配 + 禁止 shell 元字符）。
    ///
    /// 白名单命中 = 命令以前缀开头（去首尾空白后）；命令含 shell 元字符
    /// （`&&` / `||` / `;` / `` ` `` / `$(` / `>` / `<` / `|`）一律拒绝——
    /// 命令经 `split_whitespace` 直接执行，不经过 shell。
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

    // ── 检查项实现（L0 机械验证） ─────────────────────────────────────

    /// file_exists：target 存在性；支持最后一段 `*` 通配（MVP-1 简化，
    /// 仅单段前缀/后缀匹配，不递归）。
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
            // 通配段：扫描直接子项匹配（前缀/后缀）
            let dir_path = task_dir.join(dir_part);
            let mut entries = match fs::read_dir(&dir_path).await {
                Ok(e) => e,
                Err(e) => {
                    return (
                        false,
                        format!("cannot read directory {:?}: {e}", dir_path),
                    );
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
                        return (
                            false,
                            format!("error reading directory entry: {e}"),
                        );
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

    /// schema_valid：params = {format: "json"|"yaml", required_fields: ["a.b"]}。
    /// 文件可被解析 + 点分字段路径全部存在（MVP-1 内联断言，自包含）。
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

    /// reference_resolves：解析 target 的 YAML front matter，取 params.field
    /// （字符串数组，默认为 "output_refs"），数组内每个路径必须真实存在于
    /// task_dir（相对路径按 task_dir 解析，绝对路径按原样）。
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

        // 解析 YAML front matter（`---` 围栏）或纯 YAML。
        let yaml_block = extract_front_matter(&content).unwrap_or(content.as_str());
        let value: serde_yaml::Value = match serde_yaml::from_str(yaml_block) {
            Ok(v) => v,
            Err(e) => return (false, format!("front matter YAML parse failed: {e}")),
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
            return (
                false,
                format!("field '{field}' missing or empty in {:?}", path),
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
            (
                false,
                format!("unresolved references: {}", missing.join(", ")),
            )
        }
    }

    /// command_succeeds：params = {command: "..."}。白名单前缀匹配 +
    /// 30s 超时 + task_dir 为 cwd；退出码 0 = 通过。输出截断 2KB。
    /// V34/MVP-4 断言证据链（§8.22）：产出 `[证据: 工具名]` 引用 → 任务
    /// trace.jsonl `tool_call::*` 记录存在性校验——引用完整性（reference_resolves
    /// 从文件推广到 trace 记录）。纯机械零 LLM；**只对精确格式引用做存在性判定**，
    /// 无匹配视为推测处理（宁漏勿误，零误报优先）。
    ///
    /// params 键（复用 `params: Value`，零 schema 变更）：
    ///   evidence_pattern   默认 "[证据: {tool}]"（{tool} 占位）
    ///   speculation_marker 默认 "(推测)"（计数注入 detail，质量信号）
    ///   allowed_tools      默认 ["webfetch","search","read","bash"]
    ///   trace_glob         默认 "trace.jsonl"
    ///
    /// 失败 = 证据引用在 trace 中不存在（编造证据）→ hard 短路语义由 severity 决定
    /// （种子契约 soft 起步，§8.23 MVP-4）。
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
                        continue; // 损坏行跳过（宁漏勿误）
                    };
                    let Some(phase) = record.get("phase").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let Some(tool) = phase.strip_prefix("tool_call::") else {
                        continue;
                    };
                    if !allowed.iter().any(|t| t == tool) {
                        continue; // 非证据型工具不计入索引
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
        let mut evidence_refs: Vec<(String, String)> = Vec::new(); // (工具名, 引用原文)
        let mut speculation_count: u64 = 0;
        let mut scanned_files: u64 = 0;
        if deliverables_dir.is_dir() {
            let mut entries = match tokio::fs::read_dir(&deliverables_dir).await {
                Ok(e) => e,
                Err(_) => {
                    return (
                        false,
                        "failed to read deliverables dir".to_string(),
                    );
                }
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string())
                else {
                    continue;
                };
                let matched = if let Some((dir_part, pattern)) = split_glob_last_segment(target) {
                    // 仅支持 deliverables 直接子项 glob（与 check_file_exists 语义一致）
                    if !dir_part.is_empty() && dir_part != "deliverables" {
                        continue;
                    }
                    basename_glob_match(&name, &pattern)
                } else {
                    // 无 glob：匹配文件名（含 deliverables/ 前缀形式）
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
                // `[证据: 工具名]` 精确格式提取
                for line in content.lines() {
                    let mut rest = line;
                    while let Some(start) = rest.find("[证据:") {
                        let after = &rest[start..];
                        let Some(close) = after.find(']') else {
                            break;
                        };
                        let inner = after[..close].trim_start_matches("[证据:");
                        let tool = inner.trim().trim_end_matches(']').trim();
                        if !tool.is_empty() && tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                            evidence_refs.push((tool.to_string(), after[..=close].to_string()));
                        }
                        rest = &after[close.saturating_add(1).min(after.len())..];
                    }
                }
            }
        }

        // 3. 校验：每个证据引用必须在 trace 索引中（存在性 + 类型匹配）
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
        if !Self::is_command_allowed(&command) {
            return (
                false,
                format!("command not in allowlist (BCP §8.22): {command}"),
            );
        }

        // split_whitespace 直接解析 — 不经过 shell，无元字符解释面。
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
}

// ── 内部工具函数 ───────────────────────────────────────────────────────

/// 检查 target 是否含路径穿越（`..` 段）——契约 target 禁止离开 task_dir。
fn contains_path_traversal(target: &str) -> bool {
    target.split('/').any(|seg| seg == "..")
}

/// 若 target 最后一段含 `*`，返回 (父路径, 通配段)；否则返回 None。
/// 文件名 glob 匹配（单段 `*`：前缀/后缀，与 check_file_exists 通配语义一致）。
/// 共享实现——trace_consistency 产出扫描复用，防公式漂移。
fn basename_glob_match(name: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            name.starts_with(prefix) && name.ends_with(suffix) && name.len() >= prefix.len() + suffix.len()
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

/// 从内容中提取 YAML front matter（`---` 围栏之间的块）；无围栏返回 None。
fn extract_front_matter(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    Some(rest[..end].trim())
}

/// 点分字段路径取值（如 "a.b.c"）；不存在返回 None。
fn field_path_get<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

/// 截断字符串到最大长度（保留头部）。
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("…[truncated]");
        out
    }
}

// ---------------------------------------------------------------------------
// V45 SkillImpl → CheckSpec 转换（机械执行辅助）
// ---------------------------------------------------------------------------

/// SkillImpl 转 CheckSpec 以复用现有 run_check 机械执行体。
fn impl_to_check_spec(
    skill_id: &str,
    idx: usize,
    impl_: &crate::types::verification::SkillImpl,
) -> CheckSpec {
    use crate::types::verification::SkillKind;
    let kind = match impl_.kind {
        SkillKind::FileExists => CheckKind::FileExists,
        SkillKind::SchemaValid => CheckKind::SchemaValid,
        SkillKind::ReferenceResolves => CheckKind::ReferenceResolves,
        SkillKind::CommandSucceeds => CheckKind::CommandSucceeds,
        SkillKind::TraceConsistency => CheckKind::TraceConsistency,
        SkillKind::LlmJudgement => CheckKind::LlmJudgement,
        // 阳面 kind 不会走到这里（run_checks_assets 已过滤）。
        _ => CheckKind::FileExists,
    };
    let stats = crate::types::verification::CheckStats::default();
    CheckSpec {
        id: format!("{skill_id}#{idx}"),
        kind,
        target: impl_.target.clone(),
        params: impl_.params.clone(),
        severity: impl_.severity.clone().unwrap_or_default(),
        pass_condition: impl_.pass_condition.clone(),
        stats,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::verification::{CheckKind, CheckSeverity, CheckSpec};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::fs;

    /// 每次调用唯一（AGENTS.md §16：并行测试共享 pid 基路径会导致互删）。
    static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    async fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("taiji_skill_engine_{tag}_{}_{n}", std::process::id()))
    }

    fn check(id: &str, kind: CheckKind, target: &str, params: serde_json::Value, severity: CheckSeverity) -> CheckSpec {
        CheckSpec {
            id: id.to_string(),
            kind,
            target: target.to_string(),
            params,
            severity,
            pass_condition: "test".to_string(),
            stats: Default::default(),
        }
    }

    #[tokio::test]
    async fn file_exists_pass_and_fail() {
        let dir = unique_tmp_dir("file_exists").await;
        fs::create_dir_all(dir.join("deliverables")).await.unwrap();
        fs::write(dir.join("deliverables").join("report.md"), "# report").await.unwrap();

        let spec = check("c1", CheckKind::FileExists, "deliverables/report.md", json!({}), CheckSeverity::Hard);
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(r.passed, "{}", r.detail);

        let spec = check("c2", CheckKind::FileExists, "deliverables/missing.md", json!({}), CheckSeverity::Hard);
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(!r.passed);

        // 路径穿越拒绝
        let spec = check("c3", CheckKind::FileExists, "../etc/passwd", json!({}), CheckSeverity::Hard);
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(!r.passed);
        assert!(r.detail.contains("traversal"));

        fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn file_exists_glob_single_segment() {
        let dir = unique_tmp_dir("file_glob").await;
        fs::create_dir_all(dir.join("deliverables")).await.unwrap();
        fs::write(dir.join("deliverables").join("handoff.md"), "# h").await.unwrap();

        let spec = check("c1", CheckKind::FileExists, "deliverables/*.md", json!({}), CheckSeverity::Hard);
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(r.passed, "{}", r.detail);

        let spec = check("c2", CheckKind::FileExists, "deliverables/*.json", json!({}), CheckSeverity::Hard);
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(!r.passed);

        fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn schema_valid_json_and_yaml() {
        let dir = unique_tmp_dir("schema").await;
        fs::create_dir_all(&dir).await.unwrap();
        fs::write(dir.join("meta.json"), r#"{"id":"t1","description":"d"}"#).await.unwrap();
        fs::write(dir.join("cfg.yaml"), "a:\n  b: 1\n").await.unwrap();

        // JSON 解析 + required_fields
        let spec = check(
            "c1",
            CheckKind::SchemaValid,
            "meta.json",
            json!({"format": "json", "required_fields": ["id", "description"]}),
            CheckSeverity::Hard,
        );
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(r.passed, "{}", r.detail);

        // 缺失字段
        let spec = check(
            "c2",
            CheckKind::SchemaValid,
            "meta.json",
            json!({"format": "json", "required_fields": ["status"]}),
            CheckSeverity::Hard,
        );
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(!r.passed);
        assert!(r.detail.contains("status"));

        // YAML 点分路径
        let spec = check(
            "c3",
            CheckKind::SchemaValid,
            "cfg.yaml",
            json!({"format": "yaml", "required_fields": ["a.b"]}),
            CheckSeverity::Hard,
        );
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(r.passed, "{}", r.detail);

        // 非法 JSON
        fs::write(dir.join("bad.json"), "not json {").await.unwrap();
        let spec = check("c4", CheckKind::SchemaValid, "bad.json", json!({"format": "json"}), CheckSeverity::Hard);
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(!r.passed);

        fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn reference_resolves_front_matter() {
        let dir = unique_tmp_dir("refs").await;
        fs::create_dir_all(dir.join("deliverables")).await.unwrap();
        fs::write(dir.join("deliverables").join("out.md"), "content").await.unwrap();
        fs::write(
            dir.join("deliverables").join("handoff.md"),
            "---\nphase: yang\noutput_refs:\n  - deliverables/out.md\n---\nbody\n",
        )
        .await
        .unwrap();

        let spec = check(
            "c1",
            CheckKind::ReferenceResolves,
            "deliverables/handoff.md",
            json!({"field": "output_refs"}),
            CheckSeverity::Hard,
        );
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(r.passed, "{}", r.detail);

        // 引用不存在的文件
        fs::write(
            dir.join("deliverables").join("handoff2.md"),
            "---\noutput_refs:\n  - deliverables/ghost.md\n---\n",
        )
        .await
        .unwrap();
        let spec = check(
            "c2",
            CheckKind::ReferenceResolves,
            "deliverables/handoff2.md",
            json!({"field": "output_refs"}),
            CheckSeverity::Hard,
        );
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(!r.passed);
        assert!(r.detail.contains("ghost.md"));

        fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn command_allowlist_and_execution() {
        // 白名单判定（纯函数）
        assert!(SkillEngine::is_command_allowed("cargo check"));
        assert!(SkillEngine::is_command_allowed("cargo test --no-run"));
        assert!(SkillEngine::is_command_allowed("  rustc --emit=metadata  "));
        assert!(!SkillEngine::is_command_allowed("rm -rf /"));
        assert!(!SkillEngine::is_command_allowed("cargo check && rm -rf /"));
        assert!(!SkillEngine::is_command_allowed("cargo check; echo x"));
        assert!(!SkillEngine::is_command_allowed(""));
        assert!(!SkillEngine::is_command_allowed("python3 -c 'import os'"));

        // 白名单外的命令执行被拒（不实际执行）
        let dir = unique_tmp_dir("cmd").await;
        fs::create_dir_all(&dir).await.unwrap();
        let spec = check(
            "c1",
            CheckKind::CommandSucceeds,
            "",
            json!({"command": "rm -rf /tmp/evil"}),
            CheckSeverity::Hard,
        );
        let r = SkillEngine::run_check(&spec, &dir).await;
        assert!(!r.passed);
        assert!(r.detail.contains("allowlist"));

        fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn run_checks_hard_short_circuit_and_llm_skip() {
        let dir = unique_tmp_dir("run_checks").await;
        fs::create_dir_all(dir.join("deliverables")).await.unwrap();
        fs::write(dir.join("deliverables").join("out.md"), "x").await.unwrap();

        let v = VerificationAsset::new(
            "v:general",
            "通用收敛契约",
            "test",
            "contract",
            vec![
                check("c1", CheckKind::FileExists, "deliverables/out.md", json!({}), CheckSeverity::Hard),
                check("c2", CheckKind::FileExists, "deliverables/ghost.md", json!({}), CheckSeverity::Hard),
                check("c3", CheckKind::LlmJudgement, "deliverables/out.md", json!({}), CheckSeverity::Hard),
            ],
            vec!["general".into()],
        );

        let report = SkillEngine::run_checks(&[v], &dir).await;
        assert!(!report.passed, "hard fail must short-circuit");
        // llm_judgement 项不进入机械结果
        assert_eq!(report.results.len(), 2);
        assert!(report.results.iter().all(|r| r.kind != CheckKind::LlmJudgement));
        assert!(report.summary.contains("short-circuit"));

        // soft 失败不短路
        let v2 = VerificationAsset::new(
            "v:soft",
            "soft 契约",
            "test",
            "contract",
            vec![check("s1", CheckKind::FileExists, "deliverables/ghost.md", json!({}), CheckSeverity::Soft)],
            vec!["general".into()],
        );
        let report2 = SkillEngine::run_checks(&[v2], &dir).await;
        assert!(report2.passed, "soft failures must not short-circuit: {}", report2.summary);

        // 空契约：passed=true，summary 注明无机械检查
        let report3 = SkillEngine::run_checks(&[], &dir).await;
        assert!(report3.passed);
        assert!(report3.summary.contains("no mechanical checks"));

        fs::remove_dir_all(&dir).await.ok();
    }
}

#[cfg(test)]
mod trace_consistency_tests {
    use super::*;
    use crate::types::verification::{CheckKind, CheckSeverity, CheckSpec};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TC_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 唯一临时任务目录（AGENTS.md §16）。
    async fn make_task_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "taiji_trace_consistency_{}_{}",
            std::process::id(),
            TC_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        tokio::fs::create_dir_all(dir.join("deliverables")).await.unwrap();
        dir
    }

    fn spec(target: &str) -> CheckSpec {
        CheckSpec {
            id: "assertion-evidence-trace".into(),
            kind: CheckKind::TraceConsistency,
            target: target.into(),
            params: json!({}),
            severity: CheckSeverity::Soft,
            pass_condition: "evidence refs must resolve to trace tool calls".into(),
            stats: crate::types::verification::CheckStats::default(),
        }
    }

    async fn write_trace(dir: &std::path::Path, tools: &[&str]) {
        let mut lines = Vec::new();
        for tool in tools {
            lines.push(format!(
                r#"{{"ts":"2026-08-07T00:00:00","cycle":1,"depth":0,"task_id":"t","phase":"tool_call::{tool}","provider_model":"m","duration_ms":1,"input":{{}},"output":{{}},"degraded":false}}"#
            ));
        }
        tokio::fs::write(dir.join("trace.jsonl"), lines.join("\n")).await.unwrap();
    }

    /// V34/MVP-4 编造场景：产出引用 [证据: webfetch] 但 trace 无 webfetch 记录 → FAIL。
    #[tokio::test]
    async fn fabricated_evidence_fails() {
        let dir = make_task_dir().await;
        write_trace(&dir, &["read"]).await;
        tokio::fs::write(
            dir.join("deliverables").join("report.md"),
            "调研了 5 个竞品 [证据: webfetch]，趋势分析见上。",
        )
        .await
        .unwrap();
        let (passed, detail) = SkillEngine::check_trace_consistency(&spec("deliverables/*.md"), &dir).await;
        assert!(!passed, "fabricated webfetch evidence must fail");
        assert!(detail.contains("webfetch"), "detail must name the missing tool: {detail}");
        assert!(detail.contains("UNVERIFIED"), "detail must mark unverified");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V34/MVP-4 真实证据场景：引用工具在 trace 中存在 → PASS。
    #[tokio::test]
    async fn real_evidence_passes() {
        let dir = make_task_dir().await;
        write_trace(&dir, &["webfetch"]).await;
        tokio::fs::write(
            dir.join("deliverables").join("report.md"),
            "调研了 5 个竞品 [证据: webfetch]，数据见附录。",
        )
        .await
        .unwrap();
        let (passed, detail) = SkillEngine::check_trace_consistency(&spec("deliverables/*.md"), &dir).await;
        assert!(passed, "real evidence must pass: {detail}");
        assert!(detail.contains("evidence refs: 1"), "detail must count refs: {detail}");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V34/MVP-4 推测场景：`(推测)` 标记 → PASS + 计数注入 detail（质量信号）。
    #[tokio::test]
    async fn speculation_marked_passes_with_count() {
        let dir = make_task_dir().await;
        write_trace(&dir, &[]).await;
        tokio::fs::write(
            dir.join("deliverables").join("report.md"),
            "该趋势预计持续(推测)，另外两个假设也成立(推测)。",
        )
        .await
        .unwrap();
        let (passed, detail) = SkillEngine::check_trace_consistency(&spec("deliverables/*.md"), &dir).await;
        assert!(passed, "speculation markers are legal: {detail}");
        assert!(detail.contains("speculation markers: 2"), "count in detail: {detail}");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V34/MVP-4 零误报：产出无任何标记 → PASS（宁漏勿误，§8.22）。
    #[tokio::test]
    async fn no_markers_passes_zero_false_positive() {
        let dir = make_task_dir().await;
        write_trace(&dir, &[]).await;
        tokio::fs::write(
            dir.join("deliverables").join("report.md"),
            "普通叙述文本，无断言标记。",
        )
        .await
        .unwrap();
        let (passed, detail) = SkillEngine::check_trace_consistency(&spec("deliverables/*.md"), &dir).await;
        assert!(passed, "no markers must pass: {detail}");
        assert!(detail.contains("evidence refs: 0"));
        // 无产出文件也通过（目录为空 = 无断言可查）
        let dir2 = make_task_dir().await;
        write_trace(&dir2, &[]).await;
        let (passed2, _) = SkillEngine::check_trace_consistency(&spec("deliverables/*.md"), &dir2).await;
        assert!(passed2, "empty deliverables must pass");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let _ = tokio::fs::remove_dir_all(&dir2).await;
    }

    /// V34/MVP-4：CheckKind 序列化往返（snake_case: trace_consistency）。
    #[test]
    fn check_kind_serde_roundtrip() {
        let v = serde_json::to_value(CheckKind::TraceConsistency).unwrap();
        assert_eq!(v, json!("trace_consistency"));
        let back: CheckKind = serde_json::from_value(v).unwrap();
        assert_eq!(back, CheckKind::TraceConsistency);
    }

    /// V34/MVP-4 种子契约加载：v-assertion-evidence 经 run_checks 真实执行
    /// （soft 失败不短路 passed；编造证据时 passed=false 但 hard_failed=false）。
    #[tokio::test]
    async fn seed_contract_executes_via_run_checks() {
        use crate::infra::knowledge::GuizangClient;
        let dir = make_task_dir().await;
        let client = GuizangClient::new(dir.join("knowledge").as_path()).await.unwrap();
        // 直接构造等价资产（避免依赖 .taiji 目录——测试隔离）
        let mut v = crate::types::agent::VerificationAsset::new(
            "v-assertion-evidence", "断言证据链", "t", "契约语义",
            vec![spec("deliverables/*.md")],
            vec!["general".into()],
        );
        v.confidence = 0.7;
        client.save_verification(&mut v).await.unwrap();

        // 编造场景：soft 失败 → run_checks passed=false（结果记录）但无 hard 短路
        write_trace(&dir, &["read"]).await;
        tokio::fs::write(
            dir.join("deliverables").join("r.md"),
            "声称 [证据: webfetch]",
        )
        .await
        .unwrap();
        let report = SkillEngine::run_checks(&[v.clone()], &dir).await;
        // soft 失败不短路（§6.6：soft 注入 LLM prompt 供参考）——passed 仍 true，
        // 但检查项结果记录失败（供 verify prompt 与 Lianshan 回传消费）
        assert!(report.passed, "soft failure does not short-circuit");
        let tc = report
            .results
            .iter()
            .find(|r| r.kind == CheckKind::TraceConsistency)
            .expect("trace_consistency result present");
        assert!(!tc.passed, "fabricated evidence recorded as failed");

        // 真实证据场景：通过
        write_trace(&dir, &["webfetch"]).await;
        let report = SkillEngine::run_checks(&[v.clone()], &dir).await;
        assert!(report.passed);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
