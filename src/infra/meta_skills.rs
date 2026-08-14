//! V45 元层 Skill 注册表 — Rust 硬编码保底（BCP §10.1 双轨原则）。
//!
//! 阳阴元工具/元 skill 全部在此硬编码：知识库为空/损坏时，基础 Zhouyi 闭环
//! 照常运行（零资产依赖）。资产层（`skills/{cat}/{id}/skill.yaml`）是
//! 可演化覆盖层——同 id 资产优先（见 [`crate::infra::skill_catalog`]）。
//!
//! 对偶配对（元层天然成立，§10.2 dual 硬约束）：
//!
//! | 阳（执行/编排） | 阴（验证/收敛） |
//! |---|---|
//! | write | file-exists |
//! | bash | command-succeeds |
//! | search | reference-resolves |
//! | webfetch | trace-consistency |
//! | read | schema-valid |
//! | recursive-decompose | mece-check / cross-consistency / granularity-check |
//! | yin-verify（跨阴阳桥） | semantic-coherence |

use crate::types::verification::{
    CheckSeverity, CheckStats, SkillAsset, SkillCategory, SkillImpl, SkillKind,
};

// ---------------------------------------------------------------------------
// 构造辅助
// ---------------------------------------------------------------------------

fn yang(
    id: &str,
    name: &str,
    description: &str,
    kind: SkillKind,
    dual: &str,
    input_modes: &[&str],
    examples: &[&str],
    tags: &[&str],
    confidence: f64,
) -> SkillAsset {
    let impl_ = SkillImpl {
        kind,
        target: String::new(),
        params: serde_json::json!({}),
        severity: None,
        pass_condition: String::new(),
    };
    asset(
        id,
        name,
        description,
        dual,
        kind,
        vec![impl_],
        input_modes,
        examples,
        tags,
        confidence,
    )
}

fn yin(
    id: &str,
    name: &str,
    description: &str,
    kind: SkillKind,
    dual: &str,
    target: &str,
    params: serde_json::Value,
    severity: CheckSeverity,
    pass_condition: &str,
    confidence: f64,
) -> SkillAsset {
    let impl_ = SkillImpl {
        kind,
        target: target.to_string(),
        params,
        severity: Some(severity),
        pass_condition: pass_condition.to_string(),
    };
    asset(
        id,
        name,
        description,
        dual,
        kind,
        vec![impl_],
        &["text"],
        &[],
        &["general"],
        confidence,
    )
}

#[allow(clippy::too_many_arguments)]
fn asset(
    id: &str,
    name: &str,
    description: &str,
    dual: &str,
    _kind: SkillKind,
    implementations: Vec<SkillImpl>,
    input_modes: &[&str],
    examples: &[&str],
    tags: &[&str],
    confidence: f64,
) -> SkillAsset {
    SkillAsset {
        id: id.to_string(),
        name: name.to_string(),
        summary: String::new(),
        description: description.to_string(),
        detail: None,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        examples: examples.iter().map(|s| s.to_string()).collect(),
        input_modes: input_modes.iter().map(|s| s.to_string()).collect(),
        output_modes: vec!["text".to_string()],
        category: None,
        dual: dual.to_string(),
        implementations,
        agent_target: String::new(),
        confidence,
        version: 0,
        status: "active".to_string(),
        stats: CheckStats::default(),
        env_tags: Vec::new(),
        parent_id: None,
        variant_of: None,
        safe_for_exploration: false,
    }
}

// ---------------------------------------------------------------------------
// 元层查询接口
// ---------------------------------------------------------------------------

/// 全部元层 Skill（阳 7 + 阴 8），按类别过滤用 [`meta_skills`]。
pub fn all_meta_skills() -> Vec<SkillAsset> {
    let mut v = yang_meta_skills();
    v.extend(yin_meta_skills());
    v
}

/// 按类别返回元层 Skill（`Orch`/`Exec` → 阳；`Verify`/`Converge` → 阴）。
pub fn meta_skills(category: SkillCategory) -> Vec<SkillAsset> {
    let all = all_meta_skills();
    all.into_iter()
        .filter(|s| s.effective_category() == Some(category))
        .collect()
}

/// 按 id 查元层 Skill（合并视图域查询入口）。
pub fn meta_skill(id: &str) -> Option<SkillAsset> {
    all_meta_skills().into_iter().find(|s| s.id == id)
}

// ---------------------------------------------------------------------------
// 阳·元工具（执行体 = Rust builtin；recursive_decompose / yin_verify 为
// 独立 rig Tool 注册的 Zhouyi 机械节点）
// ---------------------------------------------------------------------------

/// 阳 7 元工具元数据。
pub fn yang_meta_skills() -> Vec<SkillAsset> {
    vec![
        yang(
            "write",
            "文件写入",
            "原子写入文件（覆盖目标）。JSON 模式参数：{\"path\": \"相对路径\", \"content\": \"文件内容\"}。必须传 path 与 content 两个键。",
            SkillKind::Write,
            "file-exists",
            &["json", "text"],
            &["将统计结果写入 deliverables/report.md"],
            &["exec", "write", "file"],
            0.9,
        ),
        yang(
            "bash",
            "Shell 命令",
            "执行 shell 命令并返回 stdout/stderr/退出码。text 模式：直接传命令字符串（如 \"ls -la\"）。",
            SkillKind::Bash,
            "command-succeeds",
            &["text"],
            &["ls -la deliverables/"],
            &["exec", "bash", "shell"],
            0.9,
        ),
        yang(
            "read",
            "文件读取",
            "读取文件内容（支持 offset/limit）。text 模式：直接传文件路径。",
            SkillKind::Read,
            "schema-valid",
            &["text"],
            &["读取 deliverables/report.md 核验内容"],
            &["exec", "read", "file"],
            0.9,
        ),
        yang(
            "search",
            "代码搜索",
            "在项目中搜索关键词/模式。text 模式：直接传搜索词。",
            SkillKind::Search,
            "reference-resolves",
            &["text"],
            &["搜索 fn main 的定义位置"],
            &["exec", "search"],
            0.8,
        ),
        yang(
            "webfetch",
            "网页抓取",
            "抓取 URL 内容（联网核实）。text 模式：直接传 URL。",
            SkillKind::Webfetch,
            "trace-consistency",
            &["text"],
            &["抓取 https://example.com 核实事实"],
            &["exec", "webfetch", "web"],
            0.7,
        ),
        yang(
            "recursive-decompose",
            "递归分解",
            "把复杂任务分解为子任务并行执行（每个子任务跑完整 Zhouyi 循环）。JSON 模式参数：{\"subtasks\": [{\"description\": ..., \"verification_spec\": ..., \"mode\": \"Orchestration\"|\"Execution\"}]}。仅编排模式可用。",
            SkillKind::RecursiveDecompose,
            "mece-check",
            &["json"],
            &["将『分析代码 + 写报告 + 跑测试』拆为 3 个子任务"],
            &["orch", "decompose"],
            0.8,
        ),
        yang(
            "yin-verify",
            "触发验证",
            "任务产出完成后触发因果验证（ConstraintEngine + SkillEngine 机械检查 + LLM 裁决）。JSON 模式参数：{\"task_output\": \"完整任务产出\"}。",
            SkillKind::LlmJudgement,
            "semantic-coherence",
            &["json"],
            &["完成产出后调用以触发验证闭环"],
            &["bridge", "verify"],
            0.8,
        )
        .with_category(SkillCategory::Exec),
    ]
}

// ---------------------------------------------------------------------------
// 阴·元 skill（机械判据 / LLM 裁决模板——判据从 V33/V43 种子资产提取）
// ---------------------------------------------------------------------------

/// 阴 8 元 skill 判据模板。
pub fn yin_meta_skills() -> Vec<SkillAsset> {
    vec![
        yin(
            "file-exists",
            "交付物存在性",
            "deliverables/ 必须至少有一个产物文件（执行事实是唯一记忆）。",
            SkillKind::FileExists,
            "write",
            "deliverables/*",
            serde_json::json!({}),
            CheckSeverity::Hard,
            "deliverables/ 下至少存在一个文件（任意扩展名）",
            0.8,
        ),
        yin(
            "schema-valid",
            "任务册合法",
            "meta.json 必须存在且字段完整（runner 写入，保底防线——任意任务的验证前提）。",
            SkillKind::SchemaValid,
            "read",
            "meta.json",
            serde_json::json!({
                "format": "json",
                "required_fields": ["id", "description", "depth", "status"],
            }),
            CheckSeverity::Hard,
            "meta.json 必须可解析且包含 id/description/depth/status 字段",
            0.8,
        ),
        yin(
            "command-succeeds",
            "命令执行成功",
            "白名单安全命令（编译/测试/静态检查）执行成功（30s 超时）。资产层覆盖 params.command 指定具体命令。",
            SkillKind::CommandSucceeds,
            "bash",
            "",
            serde_json::json!({ "command": "" }),
            CheckSeverity::Soft,
            "指定命令执行退出码为 0（元层默认无命令，判据由资产层覆盖）",
            0.6,
        ),
        yin(
            "reference-resolves",
            "交接引用完整",
            "deliverables/handoff.md 的 output_refs 数组内每个路径必须真实存在于任务目录。",
            SkillKind::ReferenceResolves,
            "search",
            "deliverables/handoff.md",
            serde_json::json!({ "field": "output_refs" }),
            CheckSeverity::Soft,
            "output_refs 内每个路径均存在（相对路径按任务目录解析）",
            0.7,
        ),
        yin(
            "trace-consistency",
            "断言证据链",
            "产出中的 [证据: 工具名] 引用必须对应真实工具调用记录；(推测) 标记计数作为质量信号。",
            SkillKind::TraceConsistency,
            "webfetch",
            "deliverables/*.md",
            serde_json::json!({
                "evidence_pattern": "[证据: {tool}]",
                "speculation_marker": "(推测)",
                "allowed_tools": ["webfetch", "search", "read", "bash"],
                "trace_glob": "trace.jsonl",
            }),
            CheckSeverity::Soft,
            "产出中每个 [证据: 工具名] 引用必须在 trace.jsonl 的 tool_call::* 记录中存在；推测标记不失败但计数注入 detail（质量信号）",
            0.7,
        ),
        yin(
            "semantic-coherence",
            "语义一致性裁决",
            "综合产出与 deliverables 文件内容必须一致，不得虚构产物（LLM 裁决项，L2）。",
            SkillKind::LlmJudgement,
            "yin-verify",
            "deliverables",
            serde_json::json!({}),
            CheckSeverity::Hard,
            "综合报告/任务输出必须与 deliverables/ 文件内容一致；引用的每个产物必须真实存在；不得虚构或美化未完成的产出",
            0.7,
        )
        .with_category(SkillCategory::Verify),
        yin(
            "mece-check",
            "MECE 完备性检查",
            "验证编排拆解满足 MECE：子任务互斥（无重叠）、汇总完备（覆盖全部需求维度）。",
            SkillKind::LlmJudgement,
            "recursive-decompose",
            "../*/deliverables/",
            serde_json::json!({}),
            CheckSeverity::Soft,
            "逐文件检查子任务 deliverables 后判定：1. 各子任务产出互不重叠；2. 全部子任务产出汇总覆盖父任务要求的全部维度；3. 无明显遗漏维度",
            0.7,
        ),
        yin(
            "cross-consistency",
            "跨子任务一致性",
            "子任务结果跨一致性：共同数据一致、隐含假设不互相否定、聚合结论无矛盾。",
            SkillKind::LlmJudgement,
            "recursive-decompose",
            "../*/deliverables/",
            serde_json::json!({}),
            CheckSeverity::Soft,
            "逐文件检查各子任务 deliverables 后判定：1. 共同数据/文件内容一致；2. 子任务结论之间无直接矛盾；3. 聚合综合逻辑自洽",
            0.7,
        ),
        yin(
            "granularity-check",
            "粒度适中性检查",
            "子任务粒度适中：无过度拆解（≤4）、无欠拆解（聚焦单一维度）、同级均匀。",
            SkillKind::LlmJudgement,
            "recursive-decompose",
            "../*/deliverables/",
            serde_json::json!({}),
            CheckSeverity::Soft,
            "逐文件检查各子任务 deliverables 后判定：1. 子任务数量适中（≤4）；2. 每个子任务聚焦单一维度；3. 同级子任务产出规模大致相近",
            0.65,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 元层对偶完整性：每个阳 skill 的 dual 必须存在于阴元层，反之亦然。
    #[test]
    fn test_meta_dual_pairing_complete() {
        let yang = yang_meta_skills();
        let yin = yin_meta_skills();
        for s in &yang {
            let target = yin.iter().find(|y| y.id == s.dual).unwrap_or_else(|| {
                panic!("阳 {} 的 dual '{}' 不在阴元层", s.id, s.dual)
            });
            // 类别互补：exec↔verify、orch↔converge（或 bridge）
            let pair_ok = matches!(
                (s.effective_category(), target.effective_category()),
                (Some(SkillCategory::Exec), Some(SkillCategory::Verify))
                    | (Some(SkillCategory::Orch), Some(SkillCategory::Converge))
                    | (Some(SkillCategory::Orch), Some(SkillCategory::Verify))
            );
            assert!(pair_ok, "阳 {} ↔ 阴 {} 类别不互补", s.id, target.id);
        }
        for s in &yin {
            assert!(
                yang.iter().any(|y| y.id == s.dual),
                "阴 {} 的 dual '{}' 不在阳元层",
                s.id,
                s.dual
            );
        }
    }

    /// 每个元 skill 至少一个 implementation。
    #[test]
    fn test_meta_skills_have_implementations() {
        for s in all_meta_skills() {
            assert!(
                !s.implementations.is_empty(),
                "元 skill {} 缺少 implementation",
                s.id
            );
        }
    }

    /// 类别过滤。
    #[test]
    fn test_meta_skills_by_category() {
        assert_eq!(meta_skills(SkillCategory::Exec).len(), 6); // 5 L1 + yin-verify 桥
        assert_eq!(meta_skills(SkillCategory::Orch).len(), 1);
        assert_eq!(meta_skills(SkillCategory::Verify).len(), 6);
        assert_eq!(meta_skills(SkillCategory::Converge).len(), 3);
    }
}
