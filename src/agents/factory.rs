//! AgentFactory — central hub for creating transient Rig Agents.
//!
//! Holds all shared infrastructure references (NSKG client, provider registry,
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

use crate::agents::causal::{CausalConvergeAgentBuilder, CausalVerifyAgentBuilder};
use crate::agents::fitting::FittingAgentBuilder;
use crate::agents::meta::MetaAgentBuilder;
use crate::hooks::safety::SafetyHook;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::infra::provider::ProviderRegistry;
use crate::infra::qdrant::NskgClient;
use crate::orchestration::constraint_engine::ConstraintEngine;
use crate::orchestration::trigger_engine::SkillTriggerEngine;
use crate::orchestration::worker_pool::WorkerPool;
use crate::types::agent::MetaContext;
use crate::types::execution::EngineContext;

/// Central hub for creating transient Rig agents.
///
/// Each factory method returns a builder (e.g. [`MetaAgentBuilder`],
/// [`FittingAgentBuilder`]) that encapsulates the agent configuration without
/// exposing Rig internals.  The caller invokes `.run()` on the builder to
/// instantiate and execute the Rig agent.
pub struct AgentFactory {
    /// NSKG client for traversing the knowledge graph.
    pub nskg: Arc<NskgClient>,
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
        nskg: Arc<NskgClient>,
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
            nskg,
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

    /// Create a [`MetaAgentBuilder`] (权重更新·元) for the given task ID.
    ///
    /// The MetaAgent traverses the NSKG via dynamic context injection to
    /// extract reasoning paths that bias downstream agents.  It is always
    /// limited to `max_turns = 1` (single-shot structured extraction).
    ///
    /// **LLM config**: resolved from `agent_overrides["meta"]`, falling back
    /// to the default provider + model.
    pub fn create_meta_agent(&self, task_id: &str) -> Result<MetaAgentBuilder, TaijiError> {
        let (_provider, model) = self.agent_llm_config("meta");
        tracing::debug!(
            task_id,
            model = %model,
            "Creating MetaAgent"
        );
        Ok(MetaAgentBuilder::new(
            task_id,
            self.nskg.clone(),
            self.providers.clone(),
            &model,
        ))
    }

    /// Create a [`FittingAgentBuilder`] (概率拟合·阳) seeded with a
    /// [`MetaContext`] reasoning bias.
    ///
    /// The FittingAgent is configured with the task's `depth` and engine
    /// context.  Its Rig agent receives:
    /// - tools matched by [`SkillTriggerEngine`]
    /// - built-in `recursive_decompose` and `causal_verify` tools
    /// - [`SafetyHook`] and [`TraceHook`] registered as prompt hooks
    ///
    /// **Note**: this method takes `self: &Arc<Self>` because the returned
    /// builder retains a clone of the factory for spawning sub-agents during
    /// recursive decomposition.
    pub fn create_fitting_agent(
        self: &Arc<Self>,
        depth: u32,
        meta_ctx: &MetaContext,
        engine_ctx: &EngineContext,
    ) -> Result<FittingAgentBuilder, TaijiError> {
        let (_provider, model) = self.agent_llm_config("fitting");
        tracing::debug!(
            task_id = %engine_ctx.task_id,
            depth,
            model = %model,
            "Creating FittingAgent"
        );
        Ok(FittingAgentBuilder::new(
            depth,
            meta_ctx.clone(),
            engine_ctx.clone(),
            self.clone(),
            &model,
        ))
    }

    /// Create a [`CausalVerifyAgentBuilder`] (因果验证·阴, verify mode).
    ///
    /// The CausalAgent in verify mode checks task outputs against L4 Truth
    /// constraints loaded by [`ConstraintEngine`] and produces a
    /// [`VerificationReport`].  Constraint pre-checks run **before** the
    /// LLM call (see AGENTS.md §4).
    ///
    /// **LLM config**: resolved from `agent_overrides["causal"]` (verify and
    /// converge share the same config key).
    pub fn create_causal_verify_agent(
        &self,
        engine_ctx: &EngineContext,
    ) -> Result<CausalVerifyAgentBuilder, TaijiError> {
        let (_provider, model) = self.agent_llm_config("causal");
        tracing::debug!(
            task_id = %engine_ctx.task_id,
            model = %model,
            "Creating CausalVerifyAgent"
        );
        Ok(CausalVerifyAgentBuilder::new(
            engine_ctx.clone(),
            self.constraint_engine.clone(),
            self.providers.clone(),
            &model,
        ))
    }

    /// Create a [`CausalConvergeAgentBuilder`] (收敛判定, converge mode).
    ///
    /// The CausalAgent in converge mode aggregates subtask results from
    /// recursive decomposition and decides whether the overall task has
    /// converged, partially converged, or diverged.
    ///
    /// **LLM config**: resolved from `agent_overrides["causal"]` (same key
    /// as verify mode).
    pub fn create_causal_converge_agent(
        &self,
        engine_ctx: &EngineContext,
    ) -> Result<CausalConvergeAgentBuilder, TaijiError> {
        let (_provider, model) = self.agent_llm_config("causal");
        tracing::debug!(
            task_id = %engine_ctx.task_id,
            model = %model,
            "Creating CausalConvergeAgent"
        );
        Ok(CausalConvergeAgentBuilder::new(
            engine_ctx.clone(),
            self.providers.clone(),
            &model,
        ))
    }

    // ── Configuration helpers ────────────────────────────────────────

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
    pub fn agent_llm_config(&self, agent_type: &str) -> (String, String) {
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

        if let Some(override_cfg) = self.config.llm.agent_overrides.get(agent_type) {
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
    use crate::infra::config::{AgentLlmConfig, LlmConfig, SafetyConfig};
    use crate::infra::provider::ProviderRegistry;

    /// Minimal config fixture for tests that don't connect to Qdrant.
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
            qdrant: crate::infra::config::QdrantConfig::default(),
            safety: SafetyConfig {
                enabled: false,
                trusted_mcp_servers: vec![],
            },
            mcp_servers: vec![],
        }
    }

    /// Build every transient dependency needed by [`AgentFactory::new`].
    /// This connects to Qdrant — tests that call this helper require
    /// Qdrant to be running on `localhost:6334`.
    fn build_factory(config: TaijiConfig) -> AgentFactory {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let nskg = rt
            .block_on(NskgClient::new(&config.qdrant))
            .expect("Qdrant must be running on localhost:6334 for this test");
        let providers =
            ProviderRegistry::new(&config).expect("ProviderRegistry should build");

        AgentFactory::new(
            Arc::new(nskg),
            Arc::new(providers),
            config,
            Arc::new(SafetyHook::new(&SafetyConfig::default())),
            Arc::new(WorkerPool::new(4)),
            Arc::new(ConstraintEngine::new()),
            Arc::new(SkillTriggerEngine::new()),
        )
    }

    #[test]
    #[ignore = "requires Qdrant on localhost:6334"]
    fn test_create_meta_agent() {
        let config = make_config();
        let factory = build_factory(config);
        let builder = factory
            .create_meta_agent("test-task-1")
            .expect("MetaAgentBuilder creation");
        // Verify the builder is properly initialised by checking internal
        // fields through its public API (run returns a MetaContext).
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let ctx = rt.block_on(builder.run()).expect("MetaAgent run");
        assert_eq!(ctx.reasoning_paths.len(), 0);
    }

    #[test]
    #[ignore = "requires Qdrant on localhost:6334"]
    fn test_agent_llm_config_returns_defaults() {
        // We can read the config directly through the factory without Qdrant
        // by constructing a factory manually.
        let config = make_config();
        let data_root = if config.data_root.is_empty() {
            PathBuf::from("./data")
        } else {
            PathBuf::from(&config.data_root)
        };

        // ProviderRegistry::new doesn't connect — it just creates HTTP clients.
        let providers = ProviderRegistry::new(&config).expect("ProviderRegistry");
        // NskgClient needs a stub for the factory; we use a disconnected one.
        // Since we only call agent_llm_config (no Qdrant ops), this is safe.
        // We construct NskgClient directly with a fake client — the fields
        // are private so we use the proper constructor with a tokio runtime.
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let nskg = rt
            .block_on(NskgClient::new(&config.qdrant))
            .expect("Qdrant must be running");

        let factory = AgentFactory {
            nskg: Arc::new(nskg),
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
    }

    #[test]
    #[ignore = "requires Qdrant on localhost:6334"]
    fn test_agent_llm_config_with_override() {
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

        let factory = build_factory(config);
        let (provider, model) = factory.agent_llm_config("meta");
        assert_eq!(provider, "deepseek");
        assert_eq!(model, "deepseek-reasoner");
    }

    #[test]
    #[ignore = "requires Qdrant on localhost:6334"]
    fn test_task_dir_construction() {
        let config = make_config();
        let data_root = if config.data_root.is_empty() {
            PathBuf::from("./data")
        } else {
            PathBuf::from(&config.data_root)
        };

        // Build a minimal factory with a disconnected NskgClient just to
        // test the path logic (no Qdrant ops are performed).
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let nskg = rt
            .block_on(NskgClient::new(&config.qdrant))
            .expect("Qdrant must be running");

        let factory = AgentFactory {
            nskg: Arc::new(nskg),
            providers: Arc::new(
                ProviderRegistry::new(&config).expect("ProviderRegistry"),
            ),
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
    }

    #[test]
    #[ignore = "requires Qdrant on localhost:6334"]
    fn test_agent_llm_config_unknown_agent_returns_defaults() {
        // Unknown agent types should get defaults.
        let config = make_config();
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let nskg = rt
            .block_on(NskgClient::new(&config.qdrant))
            .expect("Qdrant must be running");

        let factory = AgentFactory {
            nskg: Arc::new(nskg),
            providers: Arc::new(
                ProviderRegistry::new(&config).expect("ProviderRegistry"),
            ),
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
    }
}
