//! LLM 结构化输出解析工具
//!
//! LLM 常在 JSON 前后输出叙述性文本（验证报告、推理过程），或把 JSON
//! 包在 ` ```json ... ``` ` 围栏中。直接 `serde_json::from_str` 会因
//! 首字符不是 `{` 而失败（`StructuredOutputParseFailed`）。
//!
//! 本模块提供容忍性解析：直接解析 → 围栏内大括号切片 → 全文首尾大括号切片，
//! 三级降级后仍失败才返回原始解析错误。

use serde::de::DeserializeOwned;

/// 定位 ` ```json ` 围栏并返回其内容（不含围栏本身）。
/// 语言标签大小写不敏感（json/JSON/Json）、允许 ` ``` ` 与 json 间空格。
fn find_json_fence(raw: &str) -> Option<&str> {
    let mut search_from = 0usize;
    while let Some(rel) = raw[search_from..].find("```") {
        let fence_start = search_from + rel;
        let after_fence = fence_start + 3;
        let rest = &raw[after_fence..];
        let lang_end = rest.find('\n').unwrap_or(rest.len());
        let lang = rest[..lang_end].trim();
        if lang.eq_ignore_ascii_case("json") {
            let content_start = after_fence + lang_end;
            return raw[content_start..]
                .find("```")
                .map(|end| &raw[content_start..content_start + end]);
        }
        search_from = after_fence;
    }
    None
}

/// 解析 LLM 结构化输出为 `T`。
///
/// 解析策略（逐级降级）：
/// 1. 直接解析完整文本（LLM 严格输出纯 JSON 的最快路径）；
/// 2. 先从 ` ```json ... ``` ` 围栏提取内容，再在内容内做首尾大括号切片
///    （围栏外还有无关大括号文本时的精确路径）；
/// 3. 提取全文首个 `{` 到最后一个 `}` 之间的子串再解析（覆盖叙述前缀/后缀）；
/// 4. 全部失败：返回对原始完整文本的解析错误（调用方补充上下文后包装为
///    `TaijiError::StructuredOutputParseFailed`）。
pub fn parse_llm_json<T: DeserializeOwned>(raw: &str) -> Result<T, serde_json::Error> {
    // 1) 直接解析完整文本
    if let Ok(v) = serde_json::from_str::<T>(raw) {
        return Ok(v);
    }

    // 2) ```json 围栏内容内的大括号切片（优先于全文中括号切片，更精确）
    //    语言标签大小写不敏感、允许 ``` 与 json 间空格（覆盖 ```JSON / ``` json）。
    let fence_slice = find_json_fence(raw).and_then(|content| {
        content
            .find('{')
            .zip(content.rfind('}'))
            .filter(|(s, e)| e > s)
            .map(|(s, e)| &content[s..=e])
    });

    if let Some(slice) = fence_slice {
        if let Ok(v) = serde_json::from_str::<T>(slice) {
            return Ok(v);
        }
    }

    // 3) 全文首个 `{` → 最后一个 `}`
    let brace_slice = raw
        .find('{')
        .zip(raw.rfind('}'))
        .filter(|(s, e)| e > s)
        .map(|(s, e)| &raw[s..=e]);

    if let Some(slice) = brace_slice {
        if let Ok(v) = serde_json::from_str::<T>(slice) {
            return Ok(v);
        }
    }

    // 4) 兜底：返回原始解析错误
    serde_json::from_str::<T>(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, PartialEq, Debug)]
    struct TestStruct {
        route: String,
        confidence: f64,
    }

    #[test]
    fn parse_pure_json() {
        let raw = r#"{"route":"Pass","confidence":0.95}"#;
        let v: TestStruct = parse_llm_json(raw).expect("pure json should parse");
        assert_eq!(v.route, "Pass");
        assert_eq!(v.confidence, 0.95);
    }

    /// 冒烟实测复现场景：叙述文本 + ```json 围栏 + 尾部文本
    #[test]
    fn parse_with_prose_and_fence() {
        let raw = "I have now read the actual file and can compare it.\n\n```json\n{\"route\": \"Pass\", \"confidence\": 0.95}\n```\n\nNo violations found.";
        let v: TestStruct = parse_llm_json(raw).expect("prose + fence should parse");
        assert_eq!(v.route, "Pass");
    }

    #[test]
    fn parse_with_prose_inside_fence() {
        let raw = "```json\nHere is my verification:\n{\"route\":\"Fail\",\"confidence\":0.3}\n```";
        let v: TestStruct = parse_llm_json(raw).expect("fence with inner prose should parse");
        assert_eq!(v.route, "Fail");
    }

    #[test]
    fn parse_with_bare_prose_suffix() {
        let raw = r#"My analysis follows: {"route":"Pass","confidence":0.8} End of report."#;
        let v: TestStruct = parse_llm_json(raw).expect("prose prefix/suffix should parse");
        assert_eq!(v.confidence, 0.8);
    }

    #[test]
    fn parse_non_json_returns_err() {
        let raw = "This is not JSON at all, no braces here.";
        assert!(parse_llm_json::<TestStruct>(raw).is_err());
    }

    #[test]
    fn parse_fence_case_insensitive_and_spaced() {
        let raw = "```JSON\n{\"route\":\"Pass\",\"confidence\":0.5}\n```";
        let v: TestStruct = parse_llm_json(raw).expect("uppercase fence should parse");
        assert_eq!(v.route, "Pass");

        let raw2 = "``` json\n{\"route\":\"Fail\",\"confidence\":0.2}\n```";
        let v2: TestStruct = parse_llm_json(raw2).expect("spaced fence should parse");
        assert_eq!(v2.route, "Fail");
    }
}
