//! `guizang_query` tool — 归藏只读检索（chat agent 第 6 个工具）。
//!
//! 通过 `search_prompts` + UCB 排序从归藏认知资产库检索已沉淀的 prompt 经验
//! 模板，返回 top-N 摘要（名称/描述/置信度）。**零 LLM**、零副作用——chat LLM
//! 可在对话中自主决定何时查询归藏（「有没有相关经验」类问题）。
//!
//! 与 MetaAgent 的关系：MetaAgent 的「检索」和「编排」解耦后，本工具只取
//! 「检索」部分（纯函数），不含 mode 决策 / prompts 组合（那些是阳阴执行链的
//! 编排模板，对对话角色语义错配）。

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolError};
use serde_json::{json, Value};

use crate::infra::knowledge::{rank_prompts_by_ucb, GuizangClient};

/// 归藏只读检索工具。
pub struct GuizangQueryTool {
    guizang: Arc<GuizangClient>,
}

impl GuizangQueryTool {
    pub fn new(guizang: Arc<GuizangClient>) -> Self {
        Self { guizang }
    }
}

impl Tool for GuizangQueryTool {
    const NAME: &'static str = "guizang_query";

    type Error = ToolError;
    type Args = Value;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "guizang_query".into(),
            description: "查询归藏认知资产库：按标签/关键词检索已沉淀的 prompt 经验模板，\
                          返回 top 匹配摘要（名称/描述/置信度）。零副作用只读，不执行任何动作。"
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "查询意图描述或关键词"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "可选显式标签（精确匹配）"
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Value) -> Result<String, ToolError> {
        // 标签来源：显式 tags ∪ classify_task_tags(query) 提取的类型标签。
        let mut tags: Vec<String> = Vec::new();
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(arr) = args.get("tags").and_then(|v| v.as_array()) {
            tags.extend(arr.iter().filter_map(|v| v.as_str().map(String::from)));
        }
        if !query.is_empty() {
            tags.extend(crate::agents::meta::classify_task_tags(query));
        }
        tags.sort();
        tags.dedup();
        if tags.is_empty() {
            tags.push("general".to_string());
        }

        let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
        let prompts = self
            .guizang
            .search_prompts(&tag_refs)
            .await
            .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

        if prompts.is_empty() {
            return Ok("归藏中无匹配的认知资产（可尝试其他关键词或标签）".to_string());
        }

        // UCB 排序（纯函数，零 LLM），取 top 5。
        let ranked = rank_prompts_by_ucb(&prompts, &[], 1.414, 10.0, &[]);
        let mut out = String::new();
        for (i, idx) in ranked.iter().take(5).enumerate() {
            let p = &prompts[*idx];
            out.push_str(&format!(
                "{}. {} (置信度 {:.2}, tags: {})\n   {}\n",
                i + 1,
                p.name,
                p.confidence,
                p.tags.join(","),
                if p.description.is_empty() {
                    "(无描述)"
                } else {
                    &p.description
                }
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::tool::Tool;

    #[tokio::test]
    async fn empty_knowledge_returns_no_match() {
        let dir = std::env::temp_dir().join(format!(
            "guizang_query_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp dir");

        let guizang = Arc::new(GuizangClient::new(&dir).await.expect("guizang"));
        let tool = GuizangQueryTool::new(guizang);

        // 空库（仅元层保底）→ 检索不到资产，返回无匹配提示。
        let out = tool
            .call(serde_json::json!({"query": "重构代码"}))
            .await
            .expect("call");
        assert!(out.contains("无匹配"), "unexpected output: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
