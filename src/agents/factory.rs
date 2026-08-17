//! AgentFactory — central hub for creating transient Rig Agents.
//!
//! Holds all shared infrastructure references (归藏 GuizangClient, provider registry,
//! config, safety hook, worker pool, constraint engine, trigger engine) and
//! provides factory methods that create fresh agent builders per cycle.
//!
//! Each factory method resolves the agent-specific LLM configuration (model,
//! provider) from [`TaijiConfig::llm`] → `agent_overrides[agent_type]` before
//! instantiating the builder.  Callers invoke `.run()` on the returned builder
//! to execute the agent (the builder encapsulates all execution context that
//! was live at creation time).
//!
//! # Agent lifecycle
//! 1. `Factory` resolves provider + model from config.
//! 2. Factory creates the builder (config only, no Rig types leaked).
//! 3. Caller (e.g. `RecursiveRunner`) feeds the builder into the `WorkerPool`.
//! 4. Builder's `.run()` constructs the transient Rig agent, executes it, and
//!    returns the typed output.
//!
//! # Thread safety
//! `AgentFactory` is `Send + Sync`; all fields are cheaply clonable `Arc`s or
//! immutable config data.

use std::path::PathBuf;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::agents::yin::YinJudge;
use crate::agents::chat::ChatAgentBuilder;

/// V45 工具集路由画像（AGENTS.md §9 双通道——弱模型最小集）。
///
/// 按模型 key 字符串启发式判断：含 "flash" / "lite" / "mini" / "small"
/// → [`ToolProfile::Minimal`]（仅隐藏 webfetch 高代价联网；recursive-decompose 保留——拆解正是弱模型小上下文规避超限的核心手段）；
/// 其余 [`ToolProfile::Full`]。弱模型基础执行与验证闭环仍可用（元层判据保底）。
pub fn profile_for_model(model: &crate::types::agent::ModelKey) -> crate::infra::skill_catalog::ToolProfile {
    if model_class(model) == "flash" {
        crate::infra::skill_catalog::ToolProfile::Minimal
    } else {
        crate::infra::skill_catalog::ToolProfile::Full
    }
}

/// V50 环境维度轴（§5.4）：模型类指纹——key 含 flash/lite/mini/small → "flash"，
/// 其余 → "strong"。与 `profile_for_model` 同一检测源（零新判定逻辑），
/// 产出 `current_env_tags` 的源：检索/演化/主动学习按此维度隔离资产变体。
pub fn model_class(model: &crate::types::agent::ModelKey) -> &'static str {
    model_class_from_str(&model.0)
}

/// `model_class` 的字符串版——供只持有 `Option<&str>` 模型键的连山演化层调用
/// （不构造 `ModelKey`）。
pub fn model_class_from_str(key: &str) -> &'static str {
    let k = key.to_lowercase();
    if k.contains("flash")
        || k.contains("lite")
        || k.contains("mini")
        || k.contains("small")
    {
        "flash"
    } else {
        "strong"
    }
}
use crate::agents::yang::YangAgentBuilder;
use crate::agents::meta::MetaAgentBuilder;
use crate::agents::plan::PlanBuilder;
use crate::hooks::safety::SafetyHook;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::infra::knowledge::GuizangClient;
use crate::infra::provider::ProviderRegistry;
use crate::orchestration::constraint_engine::ConstraintEngine;
use crate::orchestration::trigger_engine::SkillTriggerEngine;
use crate::orchestration::worker_pool::WorkerPool;
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;

/// Central hub for creating transient Rig agents.
///
/// Each factory method returns a builder (e.g. [`MetaAgentBuilder`],
/// [`YangAgentBuilder`]) that encapsulates the agent configuration without
/// exposing Rig internals.  The caller invokes `.run()` on the builder to
/// instantiate and execute the Rig agent.
pub struct AgentFactory {
    /// 归藏 GuizangClient for traversing the cognitive knowledge warehouse.
    pub guizang: Arc<GuizangClient>,
    pub providers: Arc<ProviderRegistry>,
    pub config: TaijiConfig,
    pub safety_hook: Arc<SafetyHook>,
    pub worker_pool: Arc<WorkerPool>,
    pub constraint_engine: Arc<ConstraintEngine>,
    pub trigger_engine: Arc<SkillTriggerEngine>,
    /// Root directory for task data (default: `./data`).
    pub data_root: PathBuf,
}

impl AgentFactory {
    /// Create a new `AgentFactory` from shared infrastructure components.
    ///
    /// `data_root` is initialised from `config.data_root` (defaulting to
    /// `"./data"` when the config value is empty).
    pub fn new(
        guizang: Arc<GuizangClient>,
        providers: Arc<ProviderRegistry>,
        config: TaijiConfig,
        safety_hook: Arc<SafetyHook>,
        worker_pool: Arc<WorkerPool>,
        constraint_engine: Arc<ConstraintEngine>,
        trigger_engine: Arc<SkillTriggerEngine>,
    ) -> Self {
        let data_root = if config.data_root.is_empty() {
            PathBuf::from("./data")
        } else {
            PathBuf::from(&config.data_root)
        };

        tracing::debug!(
            ?data_root,
            "AgentFactory initialized"
        );

        Self {
            guizang,
            providers,
            config,
            safety_hook,
            worker_pool,
            constraint_engine,
            trigger_engine,
            data_root,
        }
    }

    // ── Factory methods ──────────────────────────────────────────────

    /// 返回一个新 factory，替换 config（其他共享字段 clone）——批19 P2 修复：
    /// max_depth override 需要同步 factory.config（否则 RecursiveDecomposeTool
    /// 读旧值，与 ZhouyiCycle 用副本的 override 不一致）。
    pub fn with_config(&self, config: TaijiConfig) -> Arc<AgentFactory> {
        Arc::new(AgentFactory {
            guizang: self.guizang.clone(),
            providers: self.providers.clone(),
            config,
            safety_hook: self.safety_hook.clone(),
            worker_pool: self.worker_pool.clone(),
            constraint_engine: self.constraint_engine.clone(),
            trigger_engine: self.trigger_engine.clone(),
            data_root: self.data_root.clone(),
        })
    }

    /// Create a [`MetaAgentBuilder`] (权重更新·元) for the given task ID.
    ///
    /// The MetaAgent traverses the 归藏 via dynamic context injection to
    /// extract reasoning paths that bias downstream agents.  Its Rig agent
    /// registers read-only collection tools (`read` / `search` / `webfetch`)
    /// and is limited to `max_turns = 6` (collect → extract); the shared
    /// process-wide [`SafetyHook`] is mounted — "带工具必有安全钩子" (蓝图 V25).
    /// `depth` / `max_depth` are injected into the mode-decision prompt
    /// (递归层数规则, V27).
    ///
    /// **LLM config**: resolved from `agent_overrides["meta"]`, falling back
    /// to the default provider + model.
    pub fn create_meta_agent(
        &self,
        task_id: &str,
        depth: u32,
        max_depth: u32,
    ) -> Result<MetaAgentBuilder, TaijiError> {
        let (provider, model) = self.agent_llm_config("meta");
        tracing::debug!(
            task_id,
            depth,
            max_depth,
            model = %model,
            "Creating MetaAgent"
        );
        Ok(MetaAgentBuilder::new(
            task_id,
            self.guizang.clone(),
            self.providers.clone(),
            &model,
        )
        .provider_name(&provider)
        .max_turns(6)
        .depth(depth)
        .max_depth(max_depth)
        // V37 异源裁判开关（Blueprint §4.3 相位级）：从 runtime 配置注入——true 且
        // 路由候选 ≥2 时决策 MetaContext.verify_model（Yin 专用验证模型）。
        .heterogeneous_verifier(self.config.runtime.model_routing.heterogeneous_verifier)
        .safety_hook(self.safety_hook.clone()))
    }

    /// Create a [`PlanBuilder`] (预演编排) for a given task ID.
    ///
    /// The PlanBuilder runs the MetaAgent to obtain cognitive context, then
    /// calls the LLM to compose a structured [`PlanSummary`] **without**
    /// entering the Zhouyi loop.  This is a read-only planning operation.
    ///
    /// **LLM config**: resolved from `agent_overrides["plan"]`, falling back
    /// to the default provider + model.
    pub fn create_plan_agent(&self, task_id: &str) -> Result<PlanBuilder, TaijiError> {
        let (provider, model) = self.agent_llm_config("plan");
        let (meta_provider, meta_model) = self.agent_llm_config("meta");
        tracing::debug!(
            task_id,
            model = %model,
            meta_model = %meta_model,
            "Creating PlanBuilder"
        );
        Ok(PlanBuilder::new(
            task_id,
            self.guizang.clone(),
            self.providers.clone(),
            &model,
        )
        .provider_name(&provider)
        .meta_llm(&meta_provider, &meta_model))
    }

    /// Create a [`YangAgentBuilder`] (概率拟合·阳) seeded with a
    /// [`MetaContext`] reasoning bias.
    ///
    /// The YangAgent is configured with the task's `depth` and engine
    /// context.  Its Rig agent receives:
    /// - tools matched by [`SkillTriggerEngine`]
    /// - built-in `recursive_decompose`（仅编排模式注册）and `yin_verify` tools
    /// - [`SafetyHook`] and [`TraceHook`] registered as prompt hooks
    ///
    /// **V27 阴阳配对模式**：`mode` 取自 `meta_ctx.mode`（由 MetaAgent 权重更新
    /// 按深度规则 + 难度决策），决定阳 Agent 的模板（编排/执行）与
    /// recursive_decompose 注册面。
    ///
    /// **Note**: this method takes `self: &Arc<Self>` because the returned
    /// builder retains a clone of the factory for spawning sub-agents during
    /// recursive decomposition.
    pub fn create_yang_agent(
        self: &Arc<Self>,
        depth: u32,
        meta_ctx: &MetaContext,
        engine_ctx: &EngineContext,
        cancel: CancellationToken,
    ) -> Result<YangAgentBuilder, TaijiError> {
        let (provider, model) = self.agent_llm_config_with("yang", meta_ctx.model.as_ref());
        tracing::debug!(
            task_id = %engine_ctx.task_id,
            depth,
            mode = ?meta_ctx.mode,
            provider = %provider,
            model = %model,
            "Creating YangAgent"
        );
        Ok(YangAgentBuilder::new(
            depth,
            meta_ctx.mode,
            meta_ctx.clone(),
            engine_ctx.clone(),
            self.clone(),
            &model,
            cancel,
        )
        .provider_name(&provider))
    }

    /// Create a [`YinJudge`]（阴判断节点，V57：半符号半 LLM，非 Agent）。
    ///
    /// 阴不持有 skill/工具/system prompt——符号层读归藏因果对碰，LLM 层语义
    /// 兑底（无工具）。verify 与 converge 是同一节点的两个方法。
    ///
    /// **LLM config**: resolved from `agent_overrides["yin"]`（verify 和
    /// converge 共享同一 config key）。
    pub fn create_yin_judge(
        &self,
        engine_ctx: &EngineContext,
        meta_ctx: &MetaContext,
    ) -> Result<YinJudge, TaijiError> {
        // V37 异源裁判（Blueprint §4.3 相位级）：verify_model 优先（Yin 用独立
        // 验证模型，裁判 ≠ 运动员）；None = 继承执行模型（主模型）。
        let yin_key = meta_ctx.verify_model.as_ref().or(meta_ctx.model.as_ref());
        let (provider, model) = self.agent_llm_config_with("yin", yin_key);
        tracing::debug!(
            task_id = %engine_ctx.task_id,
            provider = %provider,
            model = %model,
            "Creating YinJudge"
        );
        Ok(YinJudge::new(
            engine_ctx.clone(),
            self.providers.clone(),
            &provider,
            &model,
            self.guizang.clone(),
            self.config.runtime.context_limits,
        ))
    }

    // ── Configuration helpers ────────────────────────────────────────

    /// Create a [`ChatAgentBuilder`] (对话 agent, long-lived session).
    ///
    /// The ChatAgent is a full conversational Rig agent: 5 built-in L1 Skills
    /// + SafetyHook + streaming multi-turn output + session history
    /// persistence. It lives outside the Zhouyi cycle.
    ///
    /// **LLM config**: resolved from `agent_overrides["chat"]` (or defaults).
    pub fn create_chat_agent(
        &self,
        session_id: String,
        context_task_id: Option<String>,
        model: Option<String>,
        provider_name: Option<String>,
    ) -> Result<ChatAgentBuilder, TaijiError> {
        let (provider, default_model) = self.agent_llm_config("chat");
        let model = model.unwrap_or(default_model);
        let provider = provider_name.unwrap_or(provider);
        tracing::debug!(
            session = %session_id,
            model = %model,
            provider = %provider,
            "Creating ChatAgent"
        );
        Ok(ChatAgentBuilder::new(
            session_id,
            context_task_id,
            self.providers.clone(),
            self.safety_hook.clone(),
            self.guizang.clone(),
            self.config.clone(),
            self.data_root.clone(),
            &model,
            &provider,
        ))
    }

    /// Resolve the LLM provider name and model name for a given agent type.
    ///
    /// Checks `config.llm.agent_overrides[agent_type]` for an agent-specific
    /// entry.  Falls back to the global default provider and model when no
    /// override exists.
    ///
    /// Returns `(provider_name, model_name)`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let (provider, model) = factory.agent_llm_config("meta");
    /// // → ("deepseek", "deepseek-reasoner")   if overridden
    /// // → ("deepseek", "deepseek-chat")       if using defaults
    /// ```
    /// Resolve an agent-specific LLM override.
    fn agent_override<'a>(
        &'a self,
        agent_type: &str,
    ) -> Option<&'a crate::infra::config::AgentLlmConfig> {
        self.config.llm.agent_overrides.get(agent_type)
    }

    pub fn agent_llm_config(&self, agent_type: &str) -> (String, String) {
        self.agent_llm_config_with(agent_type, None)
    }

    /// V36：按 MetaContext.model 路由结果解析 LLM 配置（Blueprint §4.3 下游消费）。
    ///
    /// `model_key = Some` 且可在候选表解析 → 返回路由的 (provider, model)，
    /// **覆盖** agent_overrides（路由是元权重决策，优先级高于静态配置）；
    /// 未命中候选表（模型已从配置移除）→ warn + 回退静态配置。
    /// `model_key = None` → 既有静态配置逻辑。
    ///
    /// 注意：fallback 必须内联静态逻辑——不得调 `agent_llm_config`（它会转发
    /// 回本方法，无限递归）。
    pub fn agent_llm_config_with(
        &self,
        agent_type: &str,
        model_key: Option<&crate::types::agent::ModelKey>,
    ) -> (String, String) {
        if let Some(key) = model_key {
            if let Some((provider, model)) = self.providers.resolve_model(key) {
                tracing::debug!(
                    agent_type,
                    model_key = %key,
                    provider = %provider,
                    model = %model,
                    "routed model resolved for agent"
                );
                return (provider, model);
            }
            tracing::warn!(
                agent_type,
                model_key = %key,
                "routed model not in candidate table — falling back to config default"
            );
        }

        let default_provider = if self.config.llm.default_provider.is_empty() {
            "deepseek"
        } else {
            &self.config.llm.default_provider
        };

        let default_model = if self.config.llm.default_model.is_empty() {
            "deepseek-chat"
        } else {
            &self.config.llm.default_model
        };

        if let Some(override_cfg) = self.agent_override(agent_type) {
            let provider = override_cfg
                .provider
                .clone()
                .unwrap_or_else(|| default_provider.to_string());
            let model = override_cfg
                .model
                .clone()
                .unwrap_or_else(|| default_model.to_string());
            (provider, model)
        } else {
            (default_provider.to_string(), default_model.to_string())
        }
    }

    /// Return the task directory path for a given task ID.
    ///
    /// Tasks are stored under `{data_root}/tasks/{task_id}/`.
    /// The caller is responsible for ensuring the directory exists.
    pub fn task_dir(&self, task_id: &str) -> PathBuf {
        self.data_root.join("tasks").join(task_id)
    }
}

// ---------------------------------------------------------------------------
// Debug trait (manual — avoids requiring Debug on all infrastructure types)
// ---------------------------------------------------------------------------

impl std::fmt::Debug for AgentFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentFactory")
            .field("data_root", &self.data_root)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config::{AgentLlmConfig, KnowledgeConfig, LlmConfig, SafetyConfig};
    use crate::infra::provider::ProviderRegistry;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create a unique temporary directory for test isolation.
    async fn test_knowledge_dir() -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("taiji_factory_test_{ts}"))
    }

    /// Minimal config fixture for tests.
    fn make_config() -> TaijiConfig {
        TaijiConfig {
            version: "0.1.0".into(),
            workspace: "default".into(),
            data_root: "./test_data".into(),
            llm: LlmConfig {
                default_provider: "deepseek".into(),
                default_model: "deepseek-chat".into(),
                api_key: "test-key-not-used".into(),
                base_url: None,
                agent_overrides: std::collections::HashMap::new(),
                ..Default::default()
            },
            runtime: crate::infra::config::RuntimeConfig::default(),
            knowledge: KnowledgeConfig::default(),
            safety: SafetyConfig {
                enabled: false,
                trusted_mcp_servers: vec![],
            },
            mcp_servers: vec![],
        }
    }

    /// Build every transient dependency needed by [`AgentFactory::new`].
    async fn build_factory(config: TaijiConfig) -> (AgentFactory, PathBuf) {
        let tmp_dir = test_knowledge_dir().await;
        let guizang = Arc::new(
            GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let providers =
            ProviderRegistry::new(&config).expect("ProviderRegistry should build");

        let factory = AgentFactory::new(
            guizang,
            Arc::new(providers),
            config,
            Arc::new(SafetyHook::new(&SafetyConfig::default())),
            Arc::new(WorkerPool::new(4)),
            Arc::new(ConstraintEngine::new()),
            Arc::new(SkillTriggerEngine::new()),
        );
        (factory, tmp_dir)
    }

    #[tokio::test]
    async fn test_create_meta_agent() {
        let config = make_config();
        let (factory, tmp_dir) = build_factory(config).await;
        let builder = factory
            .create_meta_agent("test-task-1", 0, 2)
            .expect("MetaAgentBuilder creation");
        // Verify the builder is properly initialised by checking internal
        // fields through its public API (run returns a MetaContext).
        let outcome = builder
            .run("test task", &[], None)
            .await
            .expect("MetaAgent run");
        match outcome {
            crate::types::agent::MetaOutcome::Context(ctx) => {
                assert!(ctx.constraints.is_empty());
            }
            crate::types::agent::MetaOutcome::Answer(_) => {
                panic!("empty tags must not short-circuit");
            }
        }
        // Cleanup
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_agent_llm_config_returns_defaults() {
        let config = make_config();
        let tmp_dir = test_knowledge_dir().await;
        let guizang = Arc::new(
            GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let providers =
            ProviderRegistry::new(&config).expect("ProviderRegistry");
        let data_root = if config.data_root.is_empty() {
            PathBuf::from("./data")
        } else {
            PathBuf::from(&config.data_root)
        };

        let factory = AgentFactory {
            guizang,
            providers: Arc::new(providers),
            config,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            worker_pool: Arc::new(WorkerPool::new(4)),
            constraint_engine: Arc::new(ConstraintEngine::new()),
            trigger_engine: Arc::new(SkillTriggerEngine::new()),
            data_root,
        };

        let (provider, model) = factory.agent_llm_config("meta");
        assert_eq!(provider, "deepseek");
        assert_eq!(model, "deepseek-chat");

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_agent_llm_config_with_override() {
        let mut config = make_config();
        config.llm.agent_overrides.insert(
            "meta".into(),
            AgentLlmConfig {
                provider: Some("deepseek".into()),
                model: Some("deepseek-reasoner".into()),
                max_turns: Some(1),
                ..Default::default()
            },
        );

        let (factory, tmp_dir) = build_factory(config).await;
        let (provider, model) = factory.agent_llm_config("meta");
        assert_eq!(provider, "deepseek");
        assert_eq!(model, "deepseek-reasoner");

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_yin_llm_config_prefers_verify_model() {
        // V37 异源裁判：verify_model 优先于 model（Yin 用独立验证模型）。
        use crate::infra::config::ProviderEntry;
        use crate::types::agent::{MetaContext, ModelKey};
        let mut config = make_config();
        config.llm.providers.push(ProviderEntry {
            name: "deepseek".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: "deepseek-reasoner".into(),
        });
        let (factory, tmp_dir) = build_factory(config).await;

        let meta_ctx = MetaContext {
            model: Some(ModelKey::from_parts("deepseek", "deepseek-chat")),
            verify_model: Some(ModelKey::from_parts("deepseek", "deepseek-reasoner")),
            ..MetaContext::empty()
        };
        // 与 factory Yin 构造同式：verify_model 优先，None 继承 model。
        let yin_key = meta_ctx.verify_model.as_ref().or(meta_ctx.model.as_ref());
        let (_provider, model) =
            factory.agent_llm_config_with("yin", yin_key);
        assert_eq!(model, "deepseek-reasoner");

        // None → 继承主模型。
        let no_vm = MetaContext {
            verify_model: None,
            ..meta_ctx
        };
        let yin_key2 = no_vm.verify_model.as_ref().or(no_vm.model.as_ref());
        let (_provider, model2) =
            factory.agent_llm_config_with("yin", yin_key2);
        assert_eq!(model2, "deepseek-chat");

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_task_dir_construction() {
        let config = make_config();
        let tmp_dir = test_knowledge_dir().await;
        let guizang = Arc::new(
            GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let providers =
            ProviderRegistry::new(&config).expect("ProviderRegistry");
        let data_root = if config.data_root.is_empty() {
            PathBuf::from("./data")
        } else {
            PathBuf::from(&config.data_root)
        };

        let factory = AgentFactory {
            guizang,
            providers: Arc::new(providers),
            config,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            worker_pool: Arc::new(WorkerPool::new(4)),
            constraint_engine: Arc::new(ConstraintEngine::new()),
            trigger_engine: Arc::new(SkillTriggerEngine::new()),
            data_root,
        };

        let dir = factory.task_dir("task-001");
        let expected: PathBuf = [".", "test_data", "tasks", "task-001"].iter().collect();
        assert_eq!(dir, expected);

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_agent_llm_config_unknown_agent_returns_defaults() {
        let config = make_config();
        let tmp_dir = test_knowledge_dir().await;
        let guizang = Arc::new(
            GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let providers =
            ProviderRegistry::new(&config).expect("ProviderRegistry");

        let factory = AgentFactory {
            guizang,
            providers: Arc::new(providers),
            config,
            safety_hook: Arc::new(SafetyHook::new(&SafetyConfig::default())),
            worker_pool: Arc::new(WorkerPool::new(4)),
            constraint_engine: Arc::new(ConstraintEngine::new()),
            trigger_engine: Arc::new(SkillTriggerEngine::new()),
            data_root: PathBuf::from("./data"),
        };

        let (provider, model) = factory.agent_llm_config("nonexistent-agent");
        assert_eq!(provider, "deepseek");
        assert_eq!(model, "deepseek-chat");

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[test]
    fn model_class_detects_flash_vs_strong() {
        for k in ["deepseek-deepseek-v4-flash", "qwen-lite", "mini-x", "small-m"] {
            assert_eq!(model_class_from_str(k), "flash", "{k} → flash");
        }
        for k in ["deepseek-deepseek-chat", "gpt-4o", "claude-sonnet"] {
            assert_eq!(model_class_from_str(k), "strong", "{k} → strong");
        }
        let mk = crate::types::agent::ModelKey("deepseek-deepseek-v4-flash".into());
        assert_eq!(model_class(&mk), "flash", "ModelKey → flash");
    }

    #[tokio::test]
    async fn with_config_replaces_config_keeps_shared_fields() {
        // 批19 P2：max_depth override 需同步 factory.config；with_config 重建
        // factory 替换 config，其他共享字段（guizang/providers/safety_hook）保持同一 Arc。
        let config = make_config();
        let (factory, tmp_dir) = build_factory(config).await;
        let original_max_depth = factory.config.runtime.max_depth;

        let mut new_config = factory.config.clone();
        new_config.runtime.max_depth = original_max_depth + 7;
        let f2 = factory.with_config(new_config);

        assert_eq!(f2.config.runtime.max_depth, original_max_depth + 7);
        // 原 factory 不受影响（不可变共享语义）
        assert_eq!(factory.config.runtime.max_depth, original_max_depth);
        // 共享基础设施字段仍指向同一 Arc
        assert!(Arc::ptr_eq(&f2.guizang, &factory.guizang));
        assert!(Arc::ptr_eq(&f2.providers, &factory.providers));
        assert!(Arc::ptr_eq(&f2.safety_hook, &factory.safety_hook));

        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }
}
