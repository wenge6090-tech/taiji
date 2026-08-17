//! V45 文本调用块解释器（AGENTS.md §9 通道 B——弱模型 Tool Calls fallback）。
//!
//! 弱模型原生 function calling 训练不足时，LLM 可能在纯文本输出中嵌入
//! 类 JSON 调用块（```json {"tool": "write", "arguments": {...}} ```）。
//! 本模块解析此类文本块为 [`ParsedToolCall`]，供 YangAgent 驱动循环
//! 注入 toolresult（P3b 挂接点验证后接线）。

use serde_json::Value;

/// 解析出的工具调用。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedToolCall {
    /// 工具名（与 SkillTool.tool_name 对齐，如 "write" / "bash"）。
    pub tool: String,
    /// 参数（JSON 对象；纯字符串形态归一为 {"input": <str>}）。
    pub arguments: Value,
}

/// 从 LLM 纯文本输出中提取工具调用块。
///
/// 识别三种形态（容错由宽到严）：
/// 1. ```` ```json { "tool": "...", "arguments": {...} } ``` ```` 围栏块（最严格）
/// 2. ```` ``` { "tool": "...", ... } ``` ```` 无 lang 标记围栏
/// 3. 裸 `{"tool": "...", "arguments": {...}}` 行内 JSON（单行优先）
///
/// 多个块全部收集（顺序保留）；无匹配返回空 Vec。
pub fn extract_tool_calls(text: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // ── 围栏块：```json ... ``` 或 ``` ... ``` ──
    for block in iter_fenced_blocks(text) {
        if let Some(call) = parse_call_object(&block) {
            calls.push(call);
        }
    }

    // ── 行内裸 JSON 对象（含 "tool" 键）—— 围栏未命中时兜底 ──
    if calls.is_empty() {
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(call) = parse_inline_call(trimmed) {
                calls.push(call);
            }
        }
    }

    calls
}

/// 迭代 ``` 围栏块内容（去 lang 标记）。
fn iter_fenced_blocks(text: &str) -> impl Iterator<Item = String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut buf = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            if in_block {
                blocks.push(std::mem::take(&mut buf));
                in_block = false;
            } else {
                in_block = true;
                buf.clear();
            }
        } else if in_block {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(line);
        }
    }
    blocks.into_iter()
}

/// 解析 `{"tool": "...", "arguments": {...}}` 为 ParsedToolCall。
fn parse_call_object(s: &str) -> Option<ParsedToolCall> {
    let v: Value = serde_json::from_str(s).ok()?;
    let obj = v.as_object()?;
    let tool = obj.get("tool")?.as_str()?.to_string();
    let arguments = obj
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    Some(ParsedToolCall { tool, arguments })
}

/// 行内兜底：从一行中抽取首个 `{...}` 子串尝试解析。
fn parse_inline_call(line: &str) -> Option<ParsedToolCall> {
    let start = line.find('{')?;
    let end = line.rfind('}')?;
    if end <= start {
        return None;
    }
    let slice = &line[start..=end];
    let call = parse_call_object(slice)?;
    // 仅当含 tool 键才算调用（避免普通 JSON 干扰）。
    Some(call)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fenced_json_block() {
        let text = "我需要写文件：\n```json\n{\"tool\": \"write\", \"arguments\": {\"path\": \"a.md\", \"content\": \"hi\"}}\n```\n完成。";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "write");
        assert_eq!(calls[0].arguments["path"], "a.md");
    }

    #[test]
    fn test_fenced_no_lang() {
        let text = "```\n{\"tool\": \"bash\", \"arguments\": {\"input\": \"ls\"}}\n```";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "bash");
    }

    #[test]
    fn test_inline_json_fallback() {
        let text = "结果：{\"tool\": \"read\", \"arguments\": {\"input\": \"src/lib.rs\"}}";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "read");
    }

    #[test]
    fn test_no_tool_key_ignored() {
        // 普通 JSON（无 tool 键）不应被识别为调用。
        let text = "配置：{\"version\": 2, \"name\": \"x\"}";
        assert!(extract_tool_calls(text).is_empty());
    }

    #[test]
    fn test_multiple_blocks() {
        let text = "```json\n{\"tool\": \"a\", \"arguments\": {}}\n```\n中间文本\n```json\n{\"tool\": \"b\", \"arguments\": {}}\n```";
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool, "a");
        assert_eq!(calls[1].tool, "b");
    }

    #[test]
    fn test_plain_text_no_calls() {
        assert!(extract_tool_calls("纯文本无调用").is_empty());
    }
}