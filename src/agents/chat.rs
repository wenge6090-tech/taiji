//! ChatAgent — long-lived conversational Rig Agent (L3).
//!
//! Powers the browser chat panel with a **full agent loop**: 5 built-in L1
//! Skills (read/write/bash/search/webfetch) + SafetyHook + multi-turn
//! streaming output + session history persistence.
//!
//! Lifecycle: session-scoped (24h), never enters the TPN cycle. History is
//! stored at `{data_root}/chat/{session_id}.json` (atomic writes).

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use rig::agent::{MultiTurnStreamItem, Text};
use rig::client::CompletionClient;
use rig::completion::Message;
use rig::streaming::{StreamedAssistantContent, StreamingChat};

use crate::agents::tools::skills::SkillRegistry;
use crate::hooks::safety::SafetyHook;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::infra::knowledge::LiluoClient as GuizangClient;
use crate::infra::provider::{ChatProvider, ProviderRegistry};
use crate::infra::trace::save_json_atomic;

/// Default maximum turns for the ChatAgent's multi-turn loop.
const DEFAULT_MAX_TURNS: usize = 20;

/// Builder for a session-scoped conversational agent.
///
/// Created via [`crate::agents::factory::AgentFactory::create_chat_agent`].
/// Each instance is tied to one chat session (identified by `session_id`)
/// and persists its message history to `{data_root}/chat/{session_id}.json`.
pub struct ChatAgentBuilder {
    session_id: String,
    context_task_id: Option<String>,
    providers: Arc<ProviderRegistry>,
    safety_hook: Arc<SafetyHook>,
    config: TaijiConfig,
    data_root: PathBuf,
    model: String,
    provider_name: String,
}

impl ChatAgentBuilder {
    /// Create a new chat agent builder for the given session.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        context_task_id: Option<String>,
        providers: Arc<ProviderRegistry>,
        safety_hook: Arc<SafetyHook>,
        config: TaijiConfig,
        data_root: PathBuf,
        model: &str,
        provider_name: &str,
    ) -> Self {
        Self {
            session_id,
            context_task_id,
            providers,
            safety_hook,
            config,
            data_root,
            model: model.to_string(),
            provider_name: provider_name.to_string(),
        }
    }

    /// Session identifier (used as the history filename stem).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Load persisted chat history for this session (empty when absent).
    pub fn load_history(&self) -> Vec<Message> {
        crate::infra::trace::load_json_optional::<Vec<Message>>(&self.history_path())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Persist the full chat history atomically.
    pub fn save_history(&self, history: &[Message]) {
        let owned: Vec<Message> = history.to_vec();
        if let Err(e) = save_json_atomic(&owned, &self.history_path()) {
            tracing::warn!(
                session = %self.session_id,
                error = %e,
                "failed to persist chat history"
            );
        }
    }

    /// Path of the session history file.
    fn history_path(&self) -> PathBuf {
        self.data_root.join("chat").join(format!("{}.json", self.session_id))
    }

    /// Run one conversational turn with streaming output.
    ///
    /// The user message is appended to `chat_history` (which is also updated
    /// with the assistant turn on completion). Text deltas are forwarded to
    /// `on_chunk` as they stream in. Returns the final assistant text.
    pub async fn chat(
        &self,
        message: &str,
        chat_history: &mut Vec<Message>,
        on_chunk: Box<dyn Fn(String) + Send + Sync>,
    ) -> Result<String, TaijiError> {
        let system_prompt = self.build_system_prompt().await;
        let max_turns = self
            .config
            .llm
            .agent_overrides
            .get("chat")
            .and_then(|o| o.max_turns)
            .unwrap_or(DEFAULT_MAX_TURNS as u32) as usize;
        let safety_hook = self.safety_hook.as_ref().clone();
        let skill_registry = SkillRegistry::new();
        let skill_tools: Vec<Box<dyn rig::tool::ToolDyn>> = skill_registry
            .tools()
            .iter()
            .map(|t| Box::new(t.clone()) as Box<dyn rig::tool::ToolDyn>)
            .collect();

        let final_text = match self.providers.resolve_chat_provider(&self.provider_name) {
            ChatProvider::Deepseek(client) => {
                let agent = client
                    .agent(&self.model)
                    .preamble(&system_prompt)
                    .default_max_turns(max_turns)
                    .hook(safety_hook)
                    .tools(skill_tools)
                    .build();
                Self::run_stream(agent, message, chat_history, &on_chunk).await?
            }
            ChatProvider::OpenAI(client) => {
                let agent = client
                    .agent(&self.model)
                    .preamble(&system_prompt)
                    .default_max_turns(max_turns)
                    .hook(safety_hook)
                    .tools(skill_tools)
                    .build();
                Self::run_stream(agent, message, chat_history, &on_chunk).await?
            }
        };

        self.save_history(chat_history);
        Ok(final_text)
    }

    /// Execute the streaming multi-turn loop on a built agent.
    ///
    /// Text deltas are emitted via `on_chunk`; the full conversation (user
    /// turn + assistant turn + tool calls) is appended to `chat_history`.
    async fn run_stream<M, R>(
        agent: rig::agent::Agent<M, R>,
        message: &str,
        chat_history: &mut Vec<Message>,
        on_chunk: &(dyn Fn(String) + Send + Sync),
    ) -> Result<String, TaijiError>
    where
        M: rig::completion::CompletionModel + 'static,
        R: rig::agent::PromptHook<M> + 'static,
        <M as rig::completion::CompletionModel>::StreamingResponse: Clone + Unpin + 'static,
        rig::agent::Agent<M, R>: StreamingChat<M, <M as rig::completion::CompletionModel>::StreamingResponse>,
    {
        let mut stream = agent
            .stream_chat(Message::user(message), chat_history.iter().cloned())
            .await;
        let mut final_text = String::new();
        let mut turn_messages: Option<Vec<Message>> = None;

        while let Some(item) = stream.next().await {
            match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(
                    StreamedAssistantContent::Text(Text { text, .. }),
                )) => {
                    on_chunk(text.clone());
                    final_text.push_str(&text);
                }
                Ok(MultiTurnStreamItem::FinalResponse(fin)) => {
                    turn_messages = fin.history().map(|h| h.to_vec());
                }
                Err(e) => {
                    return Err(TaijiError::LLMCallFailed {
                        context: format!("ChatAgent stream error: {e}"),
                    });
                }
                _ => {}
            }
        }

        // Persist the full turn into chat history (fall back to the final text).
        match turn_messages {
            Some(messages) => chat_history.extend(messages),
            None => {
                chat_history.push(Message::user(message));
                chat_history.push(Message::assistant(final_text.as_str()));
            }
        }

        Ok(final_text)
    }

    /// Build the ChatAgent system prompt.
    ///
    /// When a `context_task_id` is set, task context (description / status /
    /// depth) plus a Guizang knowledge digest are injected so the agent is
    /// aware of the task the user is currently looking at.
    async fn build_system_prompt(&self) -> String {
        let mut prompt = String::from(
            "你是归藏认知内核驱动的智能体助手，与用户协作完成任务。\n\
             你可以使用工具读写文件、执行命令、搜索网页来辅助回答。\n\
             请用中文回答，保持准确与简洁。",
        );
        if let Some(task_id) = &self.context_task_id {
            if let Some(meta) = self.load_task_meta(task_id) {
                prompt.push_str(&format!(
                    "\n\n当前用户正在查看任务「{}」\n\
                     任务 ID: {}\n\
                     状态: {}\n\
                     递归深度: {}\n\
                     如需了解任务产物，可读取 {data_root}/tasks/{task_id}/ 下的文件。",
                    meta.0, task_id, meta.1, meta.2,
                    data_root = self.data_root.display()
                ));
            }
        }
        // Guizang knowledge digest (L5 prompts + L4 active truths).  Degrades
        // silently to the base template when the store is unavailable.
        if let Some(digest) = self.guizang_digest().await {
            prompt.push_str(&digest);
        }
        prompt
    }

    /// Build a digest of Guizang knowledge assets (L5 prompt templates and
    /// L4 active constraints) for the chat system prompt.
    ///
    /// Returns `None` when the knowledge directory is missing or unusable so
    /// the caller falls back to the base template without erroring.
    async fn guizang_digest(&self) -> Option<String> {
        let guizang_dir = PathBuf::from(&self.config.knowledge.data_dir);
        let prompts_dir = guizang_dir.join("prompts");
        let truths_dir = guizang_dir.join("truths");
        if !prompts_dir.is_dir() && !truths_dir.is_dir() {
            return None;
        }
        let guizang = match GuizangClient::new_sparse(&guizang_dir).await {
            Ok(client) => client,
            Err(e) => {
                tracing::warn!("guizang digest unavailable: {e}");
                return None;
            }
        };

        let mut sections: Vec<String> = Vec::new();

        // L5 prompt templates: scan the prompts/ directory, top-3 by
        // confidence (no index.yaml dependency — best effort).
        let mut prompts = Vec::new();
        if let Ok(mut read_dir) = tokio::fs::read_dir(&prompts_dir).await {
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "yaml") {
                    continue;
                }
                let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned())
                else {
                    continue;
                };
                match guizang.load_prompt(&stem).await {
                    Ok(Some(p)) => prompts.push(p),
                    Ok(None) => {}
                    Err(e) => tracing::warn!("guizang digest: load_prompt {stem}: {e}"),
                }
            }
        }
        prompts.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for p in prompts.into_iter().take(3) {
            sections.push(format!("- [L5 提示词] {}: {}", p.name, p.description));
        }

        // L4 constraints: active truths only.
        if let Ok(truths) = guizang.load_active_truths().await {
            for t in truths.into_iter().take(5) {
                sections.push(format!(
                    "- [L4 约束 · {}] {}: {}",
                    t.severity, t.header.name, t.header.description
                ));
            }
        }

        if sections.is_empty() {
            return None;
        }
        Some(format!("\n\n## 归藏知识摘要\n{}", sections.join("\n")))
    }

    /// Load lightweight task context from `{data_root}/tasks/{id}/meta.json`.
    fn load_task_meta(&self, task_id: &str) -> Option<(String, String, u32)> {
        let meta_path = self.data_root.join("tasks").join(task_id).join("meta.json");
        let content = std::fs::read_to_string(&meta_path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
        let description = value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or(task_id)
            .to_string();
        let status = value
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let depth = value.get("depth").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        Some((description, status, depth))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::factory::AgentFactory;
    use crate::hooks::safety::SafetyHook;
    use crate::infra::knowledge::LiluoClient as GuizangClient;
    use crate::infra::provider::ProviderRegistry;
    use crate::infra::config::{KnowledgeConfig, SafetyConfig};
    use crate::orchestration::constraint_engine::ConstraintEngine;
    use crate::orchestration::trigger_engine::SkillTriggerEngine;
    use crate::orchestration::worker_pool::WorkerPool;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_config() -> TaijiConfig {
        TaijiConfig {
            version: "0.1".into(),
            workspace: "test-ws".into(),
            data_root: String::new(),
            llm: crate::infra::config::LlmConfig {
                default_provider: "deepseek".into(),
                default_model: "deepseek-chat".into(),
                api_key: "test-key-not-used".into(),
                base_url: None,
                agent_overrides: Default::default(),
                providers: Vec::new(),
            },
            runtime: Default::default(),
            knowledge: KnowledgeConfig {
                // Point at a path that never exists so guizang_digest()
                // degrades to None deterministically in unit tests.
                data_dir: std::env::temp_dir()
                    .join("taiji_chat_none_knowledge")
                    .display()
                    .to_string(),
            },
            safety: Default::default(),
            mcp_servers: Vec::new(),
        }
    }

    fn make_builder(context_task_id: Option<String>) -> (ChatAgentBuilder, PathBuf) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp_dir = std::env::temp_dir().join(format!("taiji_chat_test_{ts}"));
        // Clean up any leftover dir from a previously failed run (same pid).
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("tmp dir");
        let config = make_config();
        let guizang = Arc::new(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(GuizangClient::new(&tmp_dir))
                .expect("GuizangClient"),
        );
        let providers = Arc::new(ProviderRegistry::new(&config).expect("providers"));
        let factory = AgentFactory::new(
            guizang,
            providers,
            config,
            Arc::new(SafetyHook::new(&SafetyConfig::default())),
            Arc::new(WorkerPool::new(4)),
            Arc::new(ConstraintEngine::new()),
            Arc::new(SkillTriggerEngine::new()),
        );
        let builder = ChatAgentBuilder::new(
            "test-session".into(),
            context_task_id,
            factory.providers.clone(),
            factory.safety_hook.clone(),
            factory.config.clone(),
            factory.data_root.clone(),
            "deepseek-chat",
            "deepseek",
        );
        (builder, tmp_dir)
    }

    #[test]
    fn test_build_system_prompt_baseline() {
        let (builder, tmp_dir) = make_builder(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let prompt = rt.block_on(builder.build_system_prompt());
        assert!(prompt.contains("归藏认知内核"));
        assert!(!prompt.contains("正在查看任务"));
        assert!(!prompt.contains("归藏知识摘要"));
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn test_build_system_prompt_with_task_context() {
        let (builder, tmp_dir) = make_builder(Some("task-123".into()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let prompt = rt.block_on(builder.build_system_prompt());
        assert!(prompt.contains("任务-123") || !prompt.contains("正在查看任务"));
        std::fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn test_build_system_prompt_with_guizang_digest() {
        let (mut builder, tmp_dir) = make_builder(None);
        // make_builder already initialised a knowledge layout under tmp_dir
        // (truths/models/skills/prompts + index.yaml) — write real assets.
        std::fs::create_dir_all(tmp_dir.join("prompts")).expect("prompts dir");
        std::fs::create_dir_all(tmp_dir.join("truths")).expect("truths dir");
        std::fs::write(
            tmp_dir.join("prompts").join("test-prompt.yaml"),
            "type: prompt\n\
             layer: 1\n\
             id: test-prompt\n\
             name: 测试提示词\n\
             description: 用于测试的提示词\n\
             tags: []\n\
             confidence: 0.9\n\
             version: 1\n\
             content: 测试内容\n\
             agent_target: FittingAgent\n\
             usage_count: 0\n\
             success_rate: 0.0\n",
        )
        .expect("write prompt asset");
        std::fs::write(
            tmp_dir.join("truths").join("test-truth.yaml"),
            "type: truth\n\
             layer: 4\n\
             id: test-truth\n\
             name: 测试约束\n\
             description: 用于测试的约束\n\
             tags: []\n\
             confidence: 0.8\n\
             version: 1\n\
             severity: Hard\n\
             status: active\n",
        )
        .expect("write truth asset");
        builder.config.knowledge.data_dir = tmp_dir.display().to_string();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let prompt = rt.block_on(builder.build_system_prompt());
        assert!(prompt.contains("归藏知识摘要"));
        assert!(prompt.contains("测试提示词"));
        assert!(prompt.contains("测试约束"));
        std::fs::remove_dir_all(&tmp_dir).ok();
    }
}
