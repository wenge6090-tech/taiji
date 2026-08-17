//! 交接文件（deliverables/handoff.md）读写 — V28 产出即交接（Blueprint §1.4 / §1.5）。
//!
//! 交接物是产出物之一，不设独立交接文件：置于 `deliverables/` 内保证可发现性——
//! 父层（parent_deliverables 注入）、同任务其他 agent（verify/converge 逐文件
//! 核验）、元校准（BACK_TO_META 读产出）、恢复链全部经既有路径发现。
//!
//! 本模块是交接文件读写的唯一实现；`failure_reason` 路由信号由 Yang 错误路径
//! 运行时捕获（Blueprint §1.5：路由不依赖解析交接文件），front matter 仅作审计与
//! LLM 消费。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 交接文件 front matter 结构化字段（Blueprint §1.5）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffInfo {
    /// 产出相位（当前恒为 yang）。
    pub phase: String,
    /// 失败原因（context_overflow / hard_cutoff / llm_failed / …）。
    pub failure_reason: String,
    /// 是否 LLM 降级运行。
    pub degraded: bool,
    /// deliverables/ 现有产出物引用（绝对路径）。
    pub output_refs: Vec<String>,
}

/// 交接文件路径：deliverables/handoff.md（产出物之一）。
pub fn handoff_path(task_dir: &Path) -> std::path::PathBuf {
    task_dir.join("deliverables").join("handoff.md")
}

/// 写交接文件 — V28 确定性收尾为基线，V29+ LLM 压缩收尾（Blueprint §1.5）为增强。
///
/// 输出 YAML-ish front matter + 正文。`body`：
/// - `Some(text)` — LLM 压缩收尾产出的结构化正文（## 进度 / ## 剩余工作 / …）
/// - `None` — 降级静态正文（v1 确定性收尾，LLM 压缩失败 / 超时 / 不可用时）
///
/// 写失败返回 Err，由调用方决定是否阻断（惯例：仅 warn 不阻断错误传播）。
pub fn write_handoff(
    task_dir: &Path,
    info: &HandoffInfo,
    body: Option<&str>,
) -> Result<(), std::io::Error> {
    let path = handoff_path(task_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut content = String::new();
    content.push_str("---\n");
    content.push_str(&format!("phase: {}\n", info.phase));
    content.push_str(&format!("failure_reason: {}\n", info.failure_reason));
    content.push_str(&format!("degraded: {}\n", info.degraded));
    content.push_str("output_refs:\n");
    for r in &info.output_refs {
        content.push_str(&format!("  - {}\n", r));
    }
    content.push_str("---\n");
    match body {
        // LLM 压缩收尾正文（Blueprint §1.5）：只含环境事实，不含对话过程。
        Some(b) if !b.trim().is_empty() => {
            content.push_str(b.trim());
            content.push('\n');
        }
        // 降级静态正文（v1 确定性收尾）。
        _ => {
            content.push_str("# 交接信息（环境信息）\n");
            content.push_str("> 前一瞬态 agent 的产出继承载体：本文件列出产出物引用与失败原因。\n");
            content.push_str("> 继续执行 / 递归拆解时优先读取 deliverables/ 下全部产出物；\n");
            content.push_str("> 本文件不含对话过程（执行事实是唯一记忆，中间记忆不跨层）。\n");
        }
    }
    std::fs::write(&path, content)?;
    Ok(())
}

/// 读取交接文件全文（None = 不存在 / 不可读——调用方按「无交接」处理）。
pub fn read_handoff(task_dir: &Path) -> Option<String> {
    std::fs::read_to_string(handoff_path(task_dir)).ok()
}

/// 列出 deliverables/ 下现有产出物（绝对路径；目录不存在 / 读失败 → 空列表）。
pub fn list_deliverables(task_dir: &Path) -> Vec<String> {
    let deliverables_dir = task_dir.join("deliverables");
    std::fs::read_dir(&deliverables_dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 构造「基于前一瞬态产出」的任务描述（V28 产出继承）。
///
/// 用于：BACK_TO_ZHOUYI 时取代「原 description + chat_history 重放」（Blueprint §1.5）；
/// 也供 BACK_TO_META 注入 MetaAgent 作产出校准。列出 deliverables/ 全部产出物
/// 与 handoff.md 内容，让 LLM 基于产出继续 / 拆解。
pub fn build_handoff_description(task_dir: &Path) -> String {
    let mut desc = String::from("\n\n## 前一瞬态产出（V28 产出继承）\n");
    let files = list_deliverables(task_dir);
    if !files.is_empty() {
        desc.push_str("### 产出文件\n");
        for f in &files {
            desc.push_str(&format!("- {}\n", f));
        }
    } else {
        desc.push_str("（deliverables/ 为空 — 无产出物）\n");
    }
    if let Some(content) = read_handoff(task_dir) {
        desc.push_str("\n### 交接文件（handoff.md）\n");
        desc.push_str(&content);
    }
    desc
}

// ================================================================
// V29+ LLM 压缩收尾（Blueprint §1.5「交接 = 压缩产物」）
// ================================================================
//
// 压缩输入构建均为纯函数（本层无 provider 依赖）；LLM 调用由调用方（agents 层）
// 执行：序列化 → 截断 → build_compress_prompt → 一次聚焦瞬态调用 → 结构化正文
// → 失败降级 write_handoff(None)。

/// 序列化对话历史为压缩输入（Prime Agent serializeConversation 同款格式）：
/// `[User]: …` / `[Assistant]: …` / `[Tool call]: name(args)` / `[Tool result]: …`。
/// 工具结果截断 2000 字符（防止 tool 结果独占压缩输入）。
pub fn serialize_history(history: &[rig::completion::Message]) -> String {
    use rig::completion::message::{AssistantContent, Message, ToolResultContent, UserContent};
    const TOOL_RESULT_LIMIT: usize = 2000;

    let mut out = String::with_capacity(history.len() * 256);
    for m in history {
        match m {
            Message::System { content } => {
                out.push_str(&format!("[System]: {}\n", content));
            }
            Message::User { content } => {
                for item in content.iter() {
                    match item {
                        UserContent::Text(t) => {
                            out.push_str(&format!("[User]: {}\n", t.text));
                        }
                        UserContent::ToolResult(tr) => {
                            let mut text = String::new();
                            for c in tr.content.iter() {
                                if let ToolResultContent::Text(t) = c {
                                    text.push_str(&t.text);
                                }
                            }
                            if text.chars().count() > TOOL_RESULT_LIMIT {
                                let truncated: String =
                                    text.chars().take(TOOL_RESULT_LIMIT).collect();
                                out.push_str(&format!(
                                    "[Tool result]: {}… [截断 {} 字符]\n",
                                    truncated,
                                    text.chars().count() - TOOL_RESULT_LIMIT
                                ));
                            } else {
                                out.push_str(&format!("[Tool result]: {}\n", text));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Assistant { content, .. } => {
                for item in content.iter() {
                    match item {
                        AssistantContent::Text(t) => {
                            out.push_str(&format!("[Assistant]: {}\n", t.text));
                        }
                        AssistantContent::ToolCall(tc) => {
                            out.push_str(&format!(
                                "[Tool call]: {}({})\n",
                                tc.function.name, tc.function.arguments
                            ));
                        }
                        AssistantContent::Reasoning(_) => {
                            // 推理内容仅作线索，不展开（防换皮中间记忆）。
                            out.push_str("[Assistant reasoning]: (见对话轨迹)\n");
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

/// 截断压缩输入：保留首部 `HEAD_CHARS`（任务目标/约束）+ 尾部 `limit - HEAD_CHARS`
/// （最新工作状态），中间以省略标记代替。limit ≤ HEAD_CHARS 时仅保留首部。
/// 字符近似（1 字符 ≤ 1 token 的保守上界，中文准确、英文偏保守安全）。
pub fn truncate_compress_input(text: &str, limit: usize) -> String {
    const HEAD_CHARS: usize = 2000;
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    let head_len = HEAD_CHARS.min(limit.saturating_sub(32)).min(total);
    let head: String = text.chars().take(head_len).collect();
    // 剩余额度全部给尾部（最新状态），尾部可能为空（limit 极小/首部占满）。
    let tail_budget = limit.saturating_sub(head_len);
    let tail_start = total - tail_budget;
    let tail: String = text.chars().skip(tail_start).collect();
    let mut out = String::with_capacity(limit + 32);
    out.push_str(&head);
    out.push_str(&format!(
        "\n… [中间 {} 字符已省略] …\n",
        total - head_len - tail.chars().count()
    ));
    out.push_str(&tail);
    out
}

/// 构建收尾压缩提示词（输出 = 交接文件正文，结构化环境事实）。
pub fn build_compress_prompt(serialized: &str) -> String {
    format!(
        "以下是一次失败任务对话的序列化记录（可能是对话的最后部分，工具结果可能被截断）。\n\n\
         {}\n\n\
         ===\n\n\
         请把它压缩为交接文件正文（Markdown），供下一个瞬态 agent 恢复执行。\n\
         结构（每节标题必须保留）：\n\
         ## 进度\n已完成 / 进行中的工作（按可证实的执行事实）\n\
         ## 剩余工作\n未完成的事项，含下一步所需的关键信息\n\
         ## 决策\n已做的关键决策与理由\n\
         ## 约束状态\n任务约束的满足情况 / 违规记录\n\
         ## 已产出文件\n对话中出现的产出物路径\n\
         规则：\
         - 只写从对话中可证实的执行事实，不推断、不补全；\n\
         - 简洁，全部内容控制在 800 字内；\n\
         - 不要复述对话过程本身，只提取环境事实。",
        serialized
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每次调用唯一临时目录（AGENTS.md §16：并行测试不得共享 pid 基路径）。
    fn tmp_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("taiji_handoff_test_{name}_{ts}_{n}"))
    }

    #[test]
    fn test_write_read_roundtrip() {
        let dir = tmp_dir("roundtrip");
        let info = HandoffInfo {
            phase: "yang".into(),
            failure_reason: "context_overflow".into(),
            degraded: false,
            output_refs: vec![
                dir.join("deliverables/part1.md").to_string_lossy().to_string(),
            ],
        };
        write_handoff(&dir, &info, None).unwrap();

        let content = read_handoff(&dir).expect("handoff should exist");
        assert!(content.contains("failure_reason: context_overflow"));
        assert!(content.contains("output_refs"));
        assert!(content.contains("part1.md"));
        assert!(content.contains("# 交接信息")); // 降级静态正文

        // 交接文件确实在 deliverables/ 内（可发现性）
        assert!(handoff_path(&dir).starts_with(dir.join("deliverables")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_handoff_with_compressed_body() {
        // V29+ LLM 压缩收尾：body 覆盖静态正文（交接 = 压缩产物）。
        let dir = tmp_dir("body");
        let info = HandoffInfo {
            phase: "yang".into(),
            failure_reason: "context_overflow".into(),
            degraded: false,
            output_refs: vec![],
        };
        let body = "## 进度\n已完成 A\n\n## 剩余工作\n- B\n";
        write_handoff(&dir, &info, Some(body)).unwrap();
        let content = read_handoff(&dir).expect("handoff should exist");
        assert!(content.contains("## 进度"), "压缩正文必须写入: {content}");
        assert!(content.contains("已完成 A"));
        assert!(!content.contains("# 交接信息"), "压缩正文不得混入静态说明");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_handoff_empty_body_falls_back() {
        let dir = tmp_dir("empty_body");
        let info = HandoffInfo {
            phase: "yang".into(),
            failure_reason: "llm_failed".into(),
            degraded: true,
            output_refs: vec![],
        };
        // 空 body → 降级静态正文（压缩失败路径）。
        write_handoff(&dir, &info, Some("   ")).unwrap();
        assert!(read_handoff(&dir).unwrap().contains("# 交接信息"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_missing_returns_none() {
        let dir = tmp_dir("missing");
        assert!(read_handoff(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_deliverables() {
        let dir = tmp_dir("list");
        let d = dir.join("deliverables");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.md"), "a").unwrap();
        std::fs::write(d.join("b.md"), "b").unwrap();

        let files = list_deliverables(&dir);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.ends_with("a.md")));
        assert!(files.iter().any(|f| f.ends_with("b.md")));

        // 不存在目录 → 空列表
        assert!(list_deliverables(&tmp_dir("none")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_handoff_description_includes_handoff() {
        let dir = tmp_dir("desc");
        let info = HandoffInfo {
            phase: "yang".into(),
            failure_reason: "context_overflow".into(),
            degraded: false,
            output_refs: vec![],
        };
        write_handoff(&dir, &info, None).unwrap();
        std::fs::create_dir_all(dir.join("deliverables")).unwrap();
        std::fs::write(dir.join("deliverables/part.md"), "x").unwrap();

        let desc = build_handoff_description(&dir);
        assert!(desc.contains("前一瞬态产出"));
        assert!(desc.contains("part.md"));
        assert!(desc.contains("failure_reason: context_overflow"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_serialize_history_formats_roles_and_truncates_tool_results() {
        use rig::completion::message::ToolResultContent;
        use rig::completion::Message;

        let long_tool_result: String = "x".repeat(3000);
        let history = vec![
            Message::System {
                content: "system-instr".into(),
            },
            Message::user("任务目标"),
            Message::assistant("部分完成"),
            Message::User {
                content: rig::OneOrMany::one(rig::completion::message::UserContent::ToolResult(
                    rig::completion::message::ToolResult {
                        id: "t1".into(),
                        call_id: None,
                        content: rig::OneOrMany::one(ToolResultContent::text(long_tool_result)),
                    },
                )),
            },
        ];

        let s = serialize_history(&history);
        assert!(s.contains("[System]: system-instr"));
        assert!(s.contains("[User]: 任务目标"));
        assert!(s.contains("[Assistant]: 部分完成"));
        assert!(s.contains("[Tool result]:"));
        assert!(s.contains("[截断"), "超长工具结果必须截断: {}", s.len());
        assert!(
            !s.contains(&"x".repeat(2001)),
            "截断后不得包含超过 2000 字符的连续工具结果"
        );
    }

    #[test]
    fn test_truncate_compress_input_keeps_head_and_tail() {
        let text: String = (0..100).map(|i| format!("line-{i:03}\n")).collect();
        let truncated = truncate_compress_input(&text, 60);
        assert!(truncated.contains("line-000"), "首部必须保留（任务目标）");
        assert!(truncated.contains("line-099"), "尾部必须保留（最新状态）");
        assert!(truncated.contains("已省略"), "中间省略标记");
        assert!(truncated.chars().count() <= 60 + 64, "长度控制在 limit 附近");

        // 不超限 → 原样返回
        assert_eq!(truncate_compress_input(&text, 10_000), text);
    }

    #[test]
    fn test_build_compress_prompt_has_sections() {
        let p = build_compress_prompt("[User]: hi");
        for section in ["## 进度", "## 剩余工作", "## 决策", "## 约束状态", "## 已产出文件"] {
            assert!(p.contains(section), "缺少章节 {section}");
        }
        assert!(p.contains("可证实的执行事实"));
        assert!(p.contains("[User]: hi"));
    }
}
