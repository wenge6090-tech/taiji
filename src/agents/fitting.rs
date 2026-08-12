//! FittingAgent builder (概率拟合·阳) — "probability fitting, the yang phase".
//!
//! The FittingAgent is the **second** agent in the TPN cycle.  It receives the
//! [`MetaContext`] produced by the MetaAgent (权重更新·元) as a reasoning bias
//! and executes along the extracted reasoning paths, either solving the task
//! directly or recursively decomposing it into subtasks.
//!
//! # Toolset
//! The FittingAgent's transient Rig agent is wired with:
//! - **`recursive_decompose`**: spawns child FittingAgents for subtasks.
//! - **`causal_verify`**: invokes CausalAgent.verify() on intermediate outputs.
//! - **5 L1 Skills**: `read`, `write`, `bash`, `search`, `webfetch`.
//! - **`SafetyHook`** and **`TraceHook`**: registered as Rig `PromptHook`s.
//!
//! L1 Skills are real built-in implementations.  A frontend agent may also
//! inject additional context via MCP ExternalContext.
//!
//! # Constraints (AGENTS.md §2)
//! - `max_turns = config.runtime.max_rounds` (default 30).
//! - System prompt is built from `meta_ctx.yang_prompt`.
//!
//! # Lifecycle
//! 1. [`AgentFactory::create_fitting_agent`] creates this builder.
//! 2. The runner calls [`FittingAgentBuilder::run`] with the task description.
//! 3. The internal Rig agent executes, calling tools and possibly recursing.
//! 4. Returns a [`TPNResult`] with the final content and tool usage summary.

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::{Chat, Message};
use rig::providers::deepseek;
use rig::tool::ToolDyn;
use tokio_util::sync::CancellationToken;

use crate::agents::factory::AgentFactory;
use crate::agents::tools::causal_verify::CausalVerifyTool;
use crate::agents::tools::recursive_decompose::RecursiveDecomposeTool;
use crate::agents::tools::skills::SkillRegistry;
use crate::hooks::trace::TraceHook;
use crate::infra::error::TaijiError;
use crate::infra::trace::save_json_atomic;
use crate::types::agent::{AgentMode, MetaContext};
use crate::types::execution::EngineContext;
use crate::types::task::{Task, TPNResult};

/// Builder for the FittingAgent (概率拟合·阳).
///
/// Created by [`AgentFactory::create_fitting_agent`].  Encapsulates the
/// reasoning bias ([`MetaContext`]), engine context (depth, cycle, round),
/// cancellation token, and a handle to the factory for spawning sub-agents
/// during recursion.
///
/// V27 起按阴阳配对模式分化（用户框架要求，V26 单模式融合已撤销）：
/// - Orchestration：编排模板（recursive_decompose 拆解 + 综合），注册拆解工具
/// - Execution：执行模板（L1 工具直接产出），不注册 recursive_decompose
pub struct FittingAgentBuilder {
    depth: u32,
    /// 阴阳配对模式（Orchestration | Execution），由 MetaAgent 权重更新决策，
    /// 经 MetaContext.mode 传递（V27）。决定 system prompt 模板与工具注册面。
    mode: AgentMode,
    meta_ctx: MetaContext,
    engine_ctx: EngineContext,
    factory: Arc<AgentFactory>,
    model: String,
    /// V36 模型路由：provider 名（MetaContext.model 解析结果；默认 deepseek）。
    provider_name: String,
    /// Cancellation token propagated from the runner.
    /// Used by [`RecursiveDecomposeTool`] to signal cancellation to subtasks.
    cancel: CancellationToken,
}

impl FittingAgentBuilder {
    /// Create a new `FittingAgentBuilder`.
    ///
    /// Normally called from [`AgentFactory::create_fitting_agent`] — external
    /// callers should use the factory rather than constructing this directly.
    pub fn new(
        depth: u32,
        mode: AgentMode,
        meta_ctx: MetaContext,
        engine_ctx: EngineContext,
        factory: Arc<AgentFactory>,
        model: &str,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            depth,
            mode,
            meta_ctx,
            engine_ctx,
            factory,
            model: model.to_string(),
            provider_name: "deepseek".to_string(),
            cancel,
        }
    }

    /// V36：设置 LLM provider 名（MetaContext.model 路由结果；默认 deepseek）。
    pub fn provider_name(mut self, provider: &str) -> Self {
        self.provider_name = provider.to_string();
        self
    }

    /// Run the FittingAgent: execute the task along reasoning paths.
    ///
    /// This method constructs a transient Rig agent equipped with:
    /// - `System prompt`: composed from [`MetaContext::yang_prompt`], which
    ///   includes the task description, reasoning path summaries, and
    ///   constraint summaries.
    /// - `max_turns`: set to `factory.config.runtime.max_rounds`.
    /// - `Tools`: L1 skills matched by [`SkillTriggerEngine`], plus
    ///   `recursive_decompose` and `causal_verify`.
    /// - `Hooks`: [`SafetyHook`] and [`TraceHook`] for security and tracing.
    ///
    /// # Production wiring (pinned for Rig API verification)
    ///
    /// ```ignore
    /// use rig::providers::deepseek;
    /// use rig_core::agent::AgentBuilder;
    /// use crate::hooks::safety::SafetyHook;
    /// use crate::hooks::trace::TraceHook;
    /// use crate::agents::tools::recursive_decompose::RecursiveDecompose;
    /// use crate::agents::tools::causal_verify::CausalVerifyTool;
    ///
    /// let client = self.factory.providers.client("deepseek")?;
    ///
    /// let system_prompt = build_system_prompt(&self.meta_ctx);
    ///
    /// let mut agent = client
    ///     .agent(&self.model)
    ///     .preamble(&system_prompt)
    ///     .max_turns(self.factory.config.runtime.max_rounds);
    ///
    /// // Register matched L1 skills as tools
    /// for skill in &self.meta_ctx.matched_skills {
    ///     agent = agent.tool(skill.tool_name.clone());
    /// }
    ///
    /// // Register built-in tools
    /// let task_dir = self.factory.task_dir(&self.engine_ctx.task_id);
    /// let trace_hook = TraceHook::new(&task_dir, &self.engine_ctx, &self.model);
    /// let safety_hook = self.factory.safety_hook.clone();
    ///
    /// // Wire hooks via one .hook() call — Rig 0.39 AgentBuilder::hook() is a
    /// // SINGLE slot, so multiple hooks must be composed via FittingHookSet
    /// // (safety → trace → snapshot) instead of chaining .hook().hook().
    /// // agent = agent.hook(FittingHookSet::new(safety_hook, trace_hook, snapshot_hook));
    ///
    /// let agent = agent.build();
    ///
    /// let response = agent
    ///     .prompt(task_description)
    ///     .await
    ///     .map_err(|e| TaijiError::LLMCallFailed {
    ///         context: format!("FittingAgent LLM call failed: {e}"),
    ///     })?;
    ///
    /// Ok(TPNResult {
    ///     task_id: self.engine_ctx.task_id.clone(),
    ///     content: response.as_ref().to_string(),
    ///     tools_used: vec![],
    ///     deliverables: vec![],
    ///     depth: self.depth,
    ///     rounds: 1,
    /// })
    /// ```
    ///
    /// # Current state (TODO)
    /// The actual Rig agent construction and LLM invocation is stubbed with
    /// [`todo!`] here.  The struct, constructor, and method signatures are
    /// complete and correct.  The LLM wiring will be filled in once the
    /// Rig 0.39 API surface is finalised.
    ///
    /// # Returns
    /// - `Ok(TPNResult)` on successful execution.
    /// - `Err(TaijiError::LLMCallFailed)` if the underlying LLM call fails
    ///   (after retries).
    pub async fn run(
        &self,
        task_description: &str,
        chat_history: Option<Vec<Message>>,
    ) -> Result<TPNResult, TaijiError> {
        // ── Guard against runaway recursion ──
        let max_depth = self.factory.config.runtime.max_depth;
        if self.depth > max_depth {
            return Err(TaijiError::MaxDepthExceeded { max: max_depth });
        }

        // ── Build system prompt from MetaContext (V27: 按配对模式选模板) ──
        let mut system_prompt = build_system_prompt(
            &self.meta_ctx,
            &self.engine_ctx.task_dir,
            self.engine_ctx.context_dir.as_deref(),
            self.mode,
        );
        // V29 预算纪律（高效语义，BCP §8.19）：上下文窗口是单次拟合的采样空间，
        // 不是可自由消耗的仓库——上限是保险丝，不是配额。LLM 必须感知硬约束
        // 并主动收敛（对归藏资产与 Base 模板两条路径统一生效）。
        system_prompt.push_str(&build_budget_discipline(
            self.factory.config.runtime.context_limits,
        ));
        // V30 分封制（BCP §8.20）：任务自我认知——身份与地位段。
        // 无降级：读册失败 → Err 上抛（数据损坏必须暴露，不用默认值掩盖）。
        system_prompt.push_str(&build_identity_section(
            &self.engine_ctx,
            &self.meta_ctx,
            max_depth,
        )?);
        // V34/MVP-4 断言分级教学（BCP §8.22）：证据断言必须附 [证据: 工具名]（引用
        // 真实工具调用）、推测必须标 (推测)、禁止编造证据引用——与 SkillEngine
        // TraceConsistency 检查构成双保险：教学层降低违规频率，检查层独立判定。
        system_prompt.push_str(&build_assertion_discipline_prompt());

        // ── Obtain LLM client（V36：按路由 provider 选择——MetaContext.model
        //    经 factory.agent_llm_config_with 解析为 provider_name）──
        let client: Arc<deepseek::Client> =
            self.factory.providers.client_for(&self.provider_name)?;

        // ── Build Rig agent with preamble, max_turns, max_tokens, temperature ──
        // V29 上下文预算（BCP §8.19）：max_turns 不再承担上下文管理——轮次与
        // token 消耗不对应（一次工具调用可返回 10k tokens 结果）。此值仅作
        // 防工具死循环的防御性兜底（200 轮），真正的窗口管理由 ContextLimiter
        // 按 usage.input_tokens 精准控制（250k 交接 / 300k 硬截止）。
        let max_turns = self.factory.config.llm.agent_overrides
            .get("fitting")
            .and_then(|o| o.max_turns)
            .unwrap_or(200) as usize;
        #[allow(unused)]
        let max_tokens = self.factory.config.llm.agent_overrides
            .get("fitting")
            .and_then(|o| o.max_tokens)
            .map(|v| v as u64);
        #[allow(unused)]
        let temperature = self.factory.config.llm.agent_overrides
            .get("fitting")
            .and_then(|o| o.temperature);
        let mut agent_builder = client
            .agent(&self.model)
            .preamble(&system_prompt)
            .default_max_turns(max_turns);
        // Applies to Rig AgentBuilder if the method is available
        if let Some(v) = max_tokens {
            agent_builder = agent_builder.max_tokens(v);
        }
        if let Some(v) = temperature {
            agent_builder = agent_builder.temperature(v);
        }

        // ── Register hooks: single-slot composite (safety → trace → snapshot) ──
        // Rig 0.39 AgentBuilder::hook() is a SINGLE slot — each call replaces
        // the previous hook, so chaining .hook(a).hook(b).hook(c) keeps only c.
        // V26.1-3 E2E smoke caught this: missing trace.jsonl + empty tools_used
        // (and, retroactively, the FittingAgent SafetyHook had never actually
        // been mounted since V25). All three hooks must go through
        // FittingHookSet and be mounted in ONE .hook() call.
        let trace_hook = TraceHook::new(&self.engine_ctx, &self.model);
        let safety_hook = self.factory.safety_hook.as_ref().clone();
        let snapshot_hook = crate::hooks::chat_history_snapshot::ChatHistorySnapshotHook::new(
            &self.engine_ctx.task_dir,
        );
        // V29 上下文窗口预算：精确 token 统计替换 max_turns（BCP §8.19）。
        let limits = self.factory.config.runtime.context_limits;
        // V32 第一性原理：编排节点的职责是「快拆」，不是「大干」——信息收集是
        // 子任务的职责。编排节点 handoff 阈值远小于执行节点（60k）：超限 =
        // 任务粒度错误 = 编排失败的硬证据 → BACK_TO_TPN → 带交接重入拆解。
        // 执行节点保持配置阈值（250k）。教学层（模板“先拆解后收集”）已实测
        // 拦不住 LLM 的“先理解再拆解”心理模型，必须注册面强制。
        const ORCH_HANDOFF_TOKENS: u64 = 60_000;
        let handoff = match self.mode {
            AgentMode::Orchestration => limits.handoff_tokens.min(ORCH_HANDOFF_TOKENS),
            AgentMode::Execution => limits.handoff_tokens,
        };
        let limiter = crate::hooks::context_limiter::ContextLimiter::new(
            handoff,
            limits.hard_cutoff_tokens,
        );
        let hook_set = crate::hooks::fitting_hook_set::FittingHookSet::new(
            safety_hook,
            trace_hook.clone(),
            snapshot_hook,
            limiter.clone(),
            // V30 封地边界（BCP §8.20 能看不能写）：write 目标必须在本任务域内
            self.engine_ctx.task_dir.clone(),
        );
        let agent_builder = agent_builder.hook(hook_set);

        // ── Register built-in composite tools ──
        // V27 阴阳配对：recursive_decompose 仅编排模式注册（执行模式 LLM 不可
        // 见拆解工具，专注直接产出）；causal_verify + 5 L1 Skills 两模式均注册。
        // 工具内部另有 mode guard 兜底（belt-and-suspenders）。permit 语义 =
        // 并行分解节点上限：decompose 工具入口自行 acquire，spawn 闭包不持
        // permit，无嵌套持有 → 无死锁。
        let recursive_decompose = RecursiveDecomposeTool::new(
            self.factory.clone(),
            self.engine_ctx.clone(),
            self.depth,
            self.mode,
            self.cancel.clone(),
            self.meta_ctx.clone(),
        );
        let causal_verify = CausalVerifyTool::new(
            self.factory.clone(),
            self.engine_ctx.clone(),
            self.meta_ctx.clone(),
        );

        // ── Register L1 Skills ──
        let skill_registry = SkillRegistry::new();
        let skill_tools: Vec<Box<dyn ToolDyn>> = skill_registry
            .tools()
            .iter()
            .map(|t| Box::new(t.clone()) as Box<dyn ToolDyn>)
            .collect();

        let agent = if self.mode == AgentMode::Orchestration {
            agent_builder
                .tool(recursive_decompose)
                .tool(causal_verify)
                .tools(skill_tools)
                .build()
        } else {
            agent_builder
                .tool(causal_verify)
                .tools(skill_tools)
                .build()
        };

        // ── Execute the prompt — always use Chat::chat for history persistence ──
        let history_path = self.engine_ctx.task_dir.join("chat_history.json");
        let (response, _history) = {
            let mut history: Vec<Message> = match chat_history {
                Some(h) => h,
                None => match crate::infra::trace::load_json_optional::<Vec<Message>>(&history_path) {
                    Ok(Some(h)) => {
                        tracing::debug!("Loaded existing chat_history from checkpoint");
                        h
                    }
                    _ => Vec::new(),
                },
            };

            // Chat::chat appends all new messages (user prompt + assistant + tool calls)
            // to `history`, so BACK_TO_TPN naturally carries forward context.
            let result = agent
                .chat(Message::user(task_description), &mut history)
                .await
                .map_err(|e| TaijiError::LLMCallFailed {
                    context: format!("FittingAgent LLM call failed: {e}"),
                });

            // Save chat_history to disk even on error (partial history is better than none).
            // 注意：Rig chat() 在 hook Terminate（超限）时可能不追加消息——内存 history
            // 为空时**禁止覆盖**磁盘快照（ChatHistorySnapshotHook 已写入完整对话，
            // 覆盖成 `[]` 会毁掉 LLM 压缩收尾的输入，冒烟实证）。
            if !history.is_empty() {
                if let Err(e) = save_json_atomic(&history, &history_path) {
                    tracing::warn!(
                        path = %history_path.display(),
                        error = %e,
                        "Failed to save chat_history"
                    );
                }
            }

            // V28 产出即交接 / V29 上下文预算：LLM 循环结束后（无论 Ok/Err）先检查
            // ContextLimiter 是否触发——触发即写交接文件并返回对应错误。
            // 禁止裸 LLMCallFailed 上抛（残缺产出 > 无产出，BCP §8.18）。
            if let Some(kind) = limiter.triggered() {
                let info = crate::infra::handoff::HandoffInfo {
                    phase: "fitting".into(),
                    failure_reason: match kind {
                        crate::hooks::context_limiter::LimitKind::Handoff => {
                            "context_overflow".into()
                        }
                        crate::hooks::context_limiter::LimitKind::HardCutoff => {
                            "hard_cutoff".into()
                        }
                    },
                    degraded: false,
                    output_refs: crate::infra::handoff::list_deliverables(
                        &self.engine_ctx.task_dir,
                    ),
                };
                // V29+ LLM 压缩收尾（BCP §8.18 交接 = 压缩产物）：把对话压缩为
                // 结构化环境事实作交接正文；失败/超时降级静态正文（仅 warn）。
                // 注意：Terminate 早于首次 completion 时 Rig chat() 不追加消息到
                // 内存 history——压缩输入必须读磁盘快照（chat_history.json）。
                let body = compress_history_to_handoff(
                    &client,
                    &self.model,
                    &self.engine_ctx.task_dir,
                    limits,
                )
                .await;
                if let Err(e) = crate::infra::handoff::write_handoff(
                    &self.engine_ctx.task_dir,
                    &info,
                    body.as_deref(),
                ) {
                    tracing::warn!(
                        path = %self.engine_ctx.task_dir.display(),
                        error = %e,
                        "Failed to write handoff.md — continuing with error propagation"
                    );
                }
                return Err(match kind {
                    crate::hooks::context_limiter::LimitKind::Handoff => {
                        TaijiError::ContextOverflow {
                            threshold: limits.handoff_tokens,
                        }
                    }
                    crate::hooks::context_limiter::LimitKind::HardCutoff => {
                        TaijiError::HardCutoff {
                            threshold: limits.hard_cutoff_tokens,
                        }
                    }
                });
            }

            (result, history)
        };
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                // V28：LLM 调用失败（非预算触发）也先写交接文件（残缺产出 > 无产出），
                // failure_reason=llm_failed 供上层基于产出恢复/拆解参考。
                // V29+：不尝试 LLM 压缩收尾——同一 provider 刚失败，压缩大概率
                // 同样失败（白费 30s 超时）；llm_failed 时对话通常短，静态正文 +
                // output_refs 已足够恢复。
                let info = crate::infra::handoff::HandoffInfo {
                    phase: "fitting".into(),
                    failure_reason: "llm_failed".into(),
                    degraded: true,
                    output_refs: crate::infra::handoff::list_deliverables(
                        &self.engine_ctx.task_dir,
                    ),
                };
                if let Err(werr) = crate::infra::handoff::write_handoff(
                    &self.engine_ctx.task_dir,
                    &info,
                    None,
                ) {
                    tracing::warn!(
                        path = %self.engine_ctx.task_dir.display(),
                        error = %werr,
                        "Failed to write handoff.md — continuing with error propagation"
                    );
                }
                return Err(e);
            }
        };

        // ── Extract tool call info from real TraceHook records ──
        // tools_used comes from TraceHook::on_tool_call, which captures every
        // actual invocation (L1 Skills + recursive_decompose + causal_verify),
        // instead of text-matching the LLM response (which caused false
        // positives when the report merely mentioned a tool name).
        let tools_used = trace_hook.tools_called();

        // Deliverables directory exists per BCP — list files if any
        let deliverables = crate::infra::handoff::list_deliverables(&self.engine_ctx.task_dir);

        Ok(TPNResult {
            task_id: self.engine_ctx.task_id.clone(),
            content: response,
            tools_used,
            deliverables,
            depth: self.depth,
            rounds: self.engine_ctx.round + 1,
        })
    }

    /// Return a reference to the engine context.
    pub fn engine_ctx(&self) -> &EngineContext {
        &self.engine_ctx
    }

    /// Return the configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

/// Build the system prompt for the FittingAgent from the MetaContext and mode.
///
/// V27 起按阴阳配对模式分化（用户框架要求：不再把编排与执行混在一起）：
///
/// **Orchestration（编排·阳）** — 任务分解者/综合者：
/// - 主工具 `recursive_decompose`，MECE 拆解 + 综合
/// - 含子任务模式分配指南（按难度 + 深度规则分配子 SubtaskSpec.mode）
///
/// **Execution（执行·阳）** — 聚焦执行者：
/// - 主工具 L1 Skills，直接产出完整可验证产物
/// - 不注册 recursive_decompose（LLM 不可见拆解工具）
fn build_system_prompt(
    meta_ctx: &MetaContext,
    task_dir: &std::path::Path,
    context_dir: Option<&std::path::Path>,
    mode: AgentMode,
) -> String {
    // Prefer MetaAgent-composed prompt if available.
    if let Some(ref composed) = meta_ctx.fitting_system_prompt {
        // V32：MetaAgent 编排的 prompt 不含 task_dir 上下文——产物路径以
        // 占位符 {deliverables_dir} 表示。必须替换为真实绝对路径，否则 LLM
        // 会猜测产出目录（实测：编排查 761 字符，LLM 写到项目根 deliverables/）。
        let deliverables_dir = task_dir.join("deliverables");
        let dir_str = deliverables_dir.display().to_string();
        return composed
            .replace("{deliverables_dir}", &dir_str)
            .replace("${deliverables_dir}", &dir_str);
    }

    // Fallback: build from mode-paired template (V27).
    let mut prompt = String::with_capacity(1024);

    match mode {
        AgentMode::Orchestration => {
            build_orchestration_prompt(&mut prompt, meta_ctx, task_dir, context_dir)
        }
        AgentMode::Execution => build_execution_prompt(&mut prompt, meta_ctx, task_dir, context_dir),
    }

    prompt
}

/// V29 预算纪律段（BCP §8.19 高效语义）：注入 system prompt，让 LLM 感知
/// token 预算为**保险丝而非配额**——目标是远低于阈值完成、消耗越少越好。
/// 纯函数便于单测；对归藏资产路径与 Base 模板路径统一追加。
fn build_budget_discipline(limits: crate::infra::config::ContextLimits) -> String {
    format!(
        "\n\n## 预算纪律 (Budget Discipline)\n\
         上下文窗口是单次拟合的采样空间，不是无限资源。\n\
         总预算：交接阈值 {} tokens，硬截止 {} tokens——\n\
         **这是保护性上限（保险丝），不是可自由消耗的配额**。\n\
         目标是在远低于预算内完成，token 消耗越少越好：\n\
         - 优先直接产出：能不调用工具就不调用，能不读的文件就不读；\n\
         - 输出控制篇幅：结论与关键证据优先，细节按需展开；\n\
         - 避免重复读取 / 重复验证——工具结果会占用上下文；\n\
         - 完成即止：达到任务要求立即收尾，不要额外扩展；\n\
         - 预算紧张时宁可提前交出残缺产出（交接文件），不要耗尽预算空手而归。\n",
        limits.handoff_tokens, limits.hard_cutoff_tokens
    )
}

/// V34/MVP-4 断言分级教学段（BCP §8.22）：让 LLM 感知产出断言必须与执行
/// 轨迹绑定——证据断言附 `[证据: 工具名]`（引用真实工具调用）、推测断言
/// 标 `(推测)`、禁止编造证据引用。教学层与 SkillEngine TraceConsistency
/// 检查构成双保险：教学层降低违规频率，检查层独立判定（LLM 不遵循时
/// 检查退化为空转——推测计数作为质量信号进 DMN 演化）。
fn build_assertion_discipline_prompt() -> String {
    "\n\n## 断言分级 (Assertion Discipline)\n\
     产出中的事实性断言必须与你的真实执行轨迹绑定：\n\
     - **证据断言**（你通过工具核实过的事实）：紧邻断言处附 `[证据: 工具名]`，\n\
       工具名必须是本任务中真实调用过的工具（webfetch / search / read / bash）——\n\
       例：`调研了 5 个竞品 [证据: webfetch]`；\n\
     - **推测断言**（未核实、推断或估计的内容）：紧邻断言处标 `(推测)`——\n\
       例：`该趋势预计持续（推测）`；\n\
     - **禁止编造证据**：不得引用未调用过的工具——`[证据: X]` 会被机械校验，\n\
       引用不存在的工具调用 = 验证失败；\n\
     - 证据断言优先于推测：能用工具核实的不要标推测；核实不了的就明说。\n\
     格式约定：`[证据: 工具名]` 与 `(推测)` 是唯一合法标记，紧跟断言（同行或紧邻行）。\n"
        .to_string()
}

/// V30 分封制（BCP §8.20）：任务自我认知——「身份与地位」段。
///
/// 全部要素系统确定性赋予，禁止 LLM 分类或运行时推断：
/// - 身份册 meta.json（内容/父/子——创建时入册）
/// - MetaContext.mode（类别：编排/执行——元权重更新阶段确定，V27 §8.8）
/// - 会盟索引（兄弟贡品——分封时快照注入）
///
/// 无降级原则：身份册读取/解析失败 → `Err` 上抛（数据损坏必须暴露，
/// 不用默认值掩盖）。「无父（根任务，parent_id=None）」与「无兄弟」
/// 是状态分支，非降级。
fn build_identity_section(
    engine_ctx: &EngineContext,
    meta_ctx: &MetaContext,
    max_depth: u32,
) -> Result<String, TaijiError> {
    // ── 读本任务身份册（无降级：损坏 → Err）──
    let task_path = engine_ctx.task_dir.join("meta.json");
    let task = parse_task_roll(&task_path)?;

    // 类别：元权重更新阶段确定（V27 阴阳配对，BCP §8.8/§8.20）
    let category = match meta_ctx.mode {
        AgentMode::Orchestration => "编排模式（可递归拆解）",
        AgentMode::Execution => "执行模式（直接产出）",
    };

    // 父任务：parent_id 存在则读父册；根任务（无父）是状态分支。
    let parent_line = match &task.parent_id {
        None => "根任务（天子）——无父".to_string(),
        Some(parent_id) => {
            // 子 task_dir = {父task_dir}/children/{idx} → 父目录 = parent().parent()
            let parent_dir = engine_ctx
                .task_dir
                .parent()
                .and_then(|p| p.parent())
                .ok_or_else(|| {
                    TaijiError::Other(format!(
                        "身份册父目录推导失败: task_dir={} parent_id={parent_id}",
                        engine_ctx.task_dir.display()
                    ))
                })?;
            let parent = parse_task_roll(&parent_dir.join("meta.json"))?;
            format!("{parent_id}（{description}）", description = parent.description)
        }
    };

    // 子任务：身份册 subtask_ids（无 → 状态分支「无」）
    let children_line = if task.subtask_ids.is_empty() {
        "无".to_string()
    } else {
        task.subtask_ids.join("、")
    };

    // 兄弟贡品陈列室：会盟注入的目录（分封时快照，BCP §8.20）——
    // 同批并行兄弟的贡品陆续陈列，执行中可经 read 工具随时发现。
    let sibling_line = if meta_ctx.yang_prompt.sibling_deliverables.is_empty() {
        "无".to_string()
    } else {
        format!(
            "\n  - {}\n    （贡品陈列室：兄弟贡品陆续陈列，需要时用 read 工具查看）",
            meta_ctx.yang_prompt.sibling_deliverables.join("\n  - ")
        )
    };

    Ok(format!(
        "\n\n## 身份与地位（分封制）\n\
         - 任务内容：{description}\n\
         - 任务类别：{category}（元权重更新阶段确定）\n\
         - 地位：第 {depth} 层（共 {max_depth} 层）\n\
         - 父任务：{parent_line}\n\
         - 子任务：{children_line}\n\
         - 兄弟贡品（只读）：{sibling_line}\n\
         - 权限：可读写本任务 deliverables/；父层产出与兄弟贡品只读；\n\
           中间记忆（chat_history / meta_ctx / trace）仅本节点可见\n",
        description = task.description,
        depth = engine_ctx.depth,
    ))
}

/// 读取并解析任务身份册（meta.json）。无降级：读取/解析失败 → Err 上抛，
/// 错误信息必须携带册路径（诊断性——问题暴露后能定位根因）。
fn parse_task_roll(path: &std::path::Path) -> Result<Task, TaijiError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        TaijiError::Other(format!("身份册读取失败: {} — {e}", path.display()))
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        TaijiError::Other(format!("身份册损坏: {} — {e}", path.display()))
    })
}

/// V29+ 收尾压缩（BCP §8.18「交接 = 压缩产物」）：一次聚焦的瞬态调用，把本拟合
/// 对话压缩为结构化交接正文（## 进度 / ## 剩余工作 / ## 决策 / ## 约束状态 / ## 已产出文件）。
///
/// - 输入：chat_history 序列化 → 截断到 `compress_input_tokens`（首部 2k 目标 + 尾部
///   最新状态），超限路径不得再花一次大调用
/// - 失败 / 超时（30s）/ 空输出 → `None`（调用方降级静态正文，仅 warn 不阻断）
async fn compress_history_to_handoff(
    client: &Arc<deepseek::Client>,
    model: &str,
    task_dir: &std::path::Path,
    limits: crate::infra::config::ContextLimits,
) -> Option<String> {
    use crate::infra::handoff::{
        build_compress_prompt, serialize_history, truncate_compress_input,
    };
    use rig::completion::Prompt;

    // 压缩输入 = 磁盘快照（chat_history.json，ChatHistorySnapshotHook 写入）。
    // 不用内存 history：Rig chat() 在 hook Terminate 时可能不追加消息。
    // 压缩输入 = 磁盘快照（chat_history.json，ChatHistorySnapshotHook 写入）。
    // 不用内存 history：Rig chat() 在 hook Terminate 时可能不追加消息。
    let history: Vec<Message> =
        crate::infra::trace::load_json_optional(&task_dir.join("chat_history.json"))
            .ok()
            .flatten()
            .unwrap_or_default();
    let serialized = serialize_history(&history);
    if serialized.trim().is_empty() {
        return None;
    }
    let serialized = truncate_compress_input(&serialized, limits.compress_input_tokens as usize);
    let prompt = build_compress_prompt(&serialized);

    let compress = async {
        client
            .agent(model)
            .preamble(
                "你是交接文件压缩器：把失败任务的对话压缩为结构化环境事实\n\
                 （进度 / 剩余工作 / 决策 / 约束状态 / 已产出文件），供下一个\n\
                 瞬态 agent 恢复执行。只提取可证实的执行事实，不推断不补全，\n\
                 简洁（800 字内），不复述对话过程。",
            )
            .default_max_turns(1)
            .max_tokens(2048u64)
            .temperature(0.2)
            .build()
            .prompt(&prompt)
            .await
    };
    match tokio::time::timeout(std::time::Duration::from_secs(30), compress).await {
        Ok(Ok(resp)) if !resp.trim().is_empty() => {
            tracing::info!(
                out_chars = resp.chars().count(),
                "handoff compression: compressed body produced"
            );
            Some(resp.trim().to_string())
        }
        Ok(Ok(_)) => {
            tracing::warn!("handoff compression returned empty output — falling back to static body");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "handoff compression LLM call failed — falling back to static body");
            None
        }
        Err(_) => {
            tracing::warn!("handoff compression timed out (30s) — falling back to static body");
            None
        }
    }
}

/// Common preamble shared by both mode templates: task description,
/// constraints, deliverables dir, parent deliverables, external context and
/// available tools.
fn build_prompt_common(
    prompt: &mut String,
    meta_ctx: &MetaContext,
    task_dir: &std::path::Path,
    context_dir: Option<&std::path::Path>,
) {
    // Task description
    if !meta_ctx.yang_prompt.task_description.is_empty() {
        prompt.push_str("## Task\n");
        prompt.push_str(&meta_ctx.yang_prompt.task_description);
        prompt.push_str("\n\n");
    }

    // Constraint summaries
    if !meta_ctx.yang_prompt.constraint_summaries.is_empty() {
        prompt.push_str("## Constraints\n");
        for summary in &meta_ctx.yang_prompt.constraint_summaries {
            prompt.push_str(&format!("- {}\n", summary));
        }
        prompt.push('\n');
    }

    // Output directory (absolute path)
    let deliverables_dir = task_dir.join("deliverables");
    prompt.push_str("## 产出目录\n");
    prompt.push_str(&format!(
        "所有产物文件请使用**绝对路径**写入: `{}`\n",
        deliverables_dir.display()
    ));
    prompt.push_str(&format!(
        "产出文件示例: `{}/report.md`（而非相对路径 `report.md`）。\n",
        deliverables_dir.display()
    ));
    prompt.push_str("递归子任务也会有自己的同名目录，结构一致。\n\n");

    // Parent deliverables (injected from recursive parent — read-only reference)
    if !meta_ctx.yang_prompt.parent_deliverables.is_empty() {
        prompt.push_str("## 父层产物参照 (Parent Deliverables - Read Only)\n");
        prompt.push_str("以下文件由当前任务的父层产出，你可读取其内容但不可修改：\n");
        for (i, path) in meta_ctx.yang_prompt.parent_deliverables.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, path));
        }
        prompt.push('\n');
    }

    // External context (from frontend agent via MCP)
    if let Some(ctx_dir) = context_dir {
        let files_dir = ctx_dir.join("files");
        if files_dir.exists() {
            prompt.push_str("## External Context (from Frontend Agent)\n");
            prompt.push_str("以下文件由前端 agent 已读取并传递给此任务：\n");
            if let Ok(entries) = std::fs::read_dir(&files_dir) {
                for entry in entries.flatten() {
                    let index = entry.file_name().to_string_lossy().to_string();
                    prompt.push_str(&format!("- `{}/{}`\n", files_dir.display(), index));
                }
            }
            prompt.push_str("\n使用 `read` 工具检查这些文件的内容——它们包含了你推理所需的前端上下文。\n\n");
        }
    }

    // Available tools
    if !meta_ctx.matched_skills.is_empty() {
        prompt.push_str("## Available Tools\n");
        for skill in &meta_ctx.matched_skills {
            prompt.push_str(&format!(
                "- `{}` ({}): {}\n",
                skill.tool_name, skill.name, skill.id
            ));
        }
        prompt.push('\n');
    }
}

/// Build the prompt for **Orchestration** mode — the agent decomposes tasks
/// and synthesizes results.  The plant-growth analogy guides the LLM not to
/// over-decompose or delegate everything to leaves (V27 编排·阳).
fn build_orchestration_prompt(
    prompt: &mut String,
    meta_ctx: &MetaContext,
    task_dir: &std::path::Path,
    context_dir: Option<&std::path::Path>,
) {
    prompt.push_str("你是概率拟合专家 · 编排模式 (Probability Fitting · Orchestration).\n\n");

    build_prompt_common(prompt, meta_ctx, task_dir, context_dir);

    prompt.push_str(
        "## 编排职责\n\
         你处于编排模式——把复杂任务拆解为子任务（`recursive_decompose`），\n\
         汇聚子任务结果后综合产出。\n\n\
         ### 拆解 (Decomposition)\n\
         分析任务，将其分解为 2-4 个可并行的子任务，通过 `recursive_decompose`\n\
         派发。子任务 description 必须清晰、自包含、含具体目标与产出要求。\n\
         为每个子任务设置 mode：原子/单步 → \"Execution\"，仍需拆解 → \"Orchestration\"。\n\
         叶节点（depth+1 >= max_depth）会被强制覆盖为 Execution。\n\n\
         ### 综合 (Synthesis)\n\
         收集子任务结果（含可能的失败条目），产出综合报告或聚合产物。\n\
         失败子任务的交接产物（handoff.md）可读取后针对性再指导（rerun_of）。\n\
         综合完成后用 `causal_verify` 自检。\n\n\
         ### 协作\n\
         兄弟子任务封地自治——不能互相写入，但可读取兄弟 deliverables 目录。\n\
         拆解优先弱耦合；强依赖通过父层下一轮协调注入。\n\
         通信经父层汇总，子任务间不直连。\n\n\
         ### 产物\n\
         所有产物写入 deliverables 目录（绝对路径）。编排节点核心产出为\n\
         综合报告——覆盖范围、子任务结果摘要、未完成项与原因。\n"
    );
}

/// Build the prompt for **Execution** mode — the agent focuses on direct
/// output using available tools.  `recursive_decompose` is **not registered**
/// in this mode — the LLM cannot see or call it (V27 执行·阳).
fn build_execution_prompt(
    prompt: &mut String,
    meta_ctx: &MetaContext,
    task_dir: &std::path::Path,
    context_dir: Option<&std::path::Path>,
) {
    prompt.push_str("你是概率拟合专家 · 执行模式 (Probability Fitting · Execution).\n\n");

    build_prompt_common(prompt, meta_ctx, task_dir, context_dir);

    prompt.push_str(
        "## 执行职责\n\
         你处于执行模式——任务为原子/单步任务，直接用 L1 工具完成。\n\
         你**没有** `recursive_decompose` 工具，专注直接执行。\n\n\
         ### 核心要求\n\
         1. 使用 read / write / bash / search / webfetch 直接完成任务。\n\
         2. 在单次执行中完整覆盖任务全部要求，不遗漏维度。\n\
         3. 产出后用 `causal_verify` 自检。\n\
         4. 在 deliverables 目录产出具体产物（绝对路径），输出完整、可直接使用。\n"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // V30 测试临时目录唯一性（AGENTS.md §16）：pid 基路径不唯一，需静态计数器。
    static IDENTITY_TEST_COUNTER: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    use crate::agents::factory::AgentFactory;
    use crate::hooks::safety::SafetyHook;
    use crate::infra::config::{LlmConfig, SafetyConfig, TaijiConfig};
    use crate::infra::provider::ProviderRegistry;
    use crate::infra::knowledge::GuizangClient;
    use crate::orchestration::constraint_engine::ConstraintEngine;
    use crate::orchestration::trigger_engine::SkillTriggerEngine;
    use crate::orchestration::worker_pool::WorkerPool;
    use crate::types::agent::{AgentMode, SkillRef, YangPrompt};
    use crate::types::verification::TruthConstraint;

    fn make_config() -> TaijiConfig {
        TaijiConfig {
            version: "0.1.0".into(),
            workspace: "default".into(),
            data_root: "./data".into(),
            llm: LlmConfig {
                default_provider: "deepseek".into(),
                default_model: "deepseek-chat".into(),
                api_key: "test-key".into(),
                base_url: None,
                agent_overrides: std::collections::HashMap::new(),
                ..Default::default()
            },
            runtime: crate::infra::config::RuntimeConfig::default(),
            knowledge: crate::infra::config::KnowledgeConfig::default(),
            safety: SafetyConfig::default(),
            mcp_servers: vec![],
        }
    }

    /// Build an Arc<AgentFactory> for testing.
    async fn build_factory_arc(config: TaijiConfig) -> (Arc<AgentFactory>, std::path::PathBuf) {
        let tmp_dir = std::env::temp_dir().join(format!(
            "taiji_fitting_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let guizang = Arc::new(
            GuizangClient::new(&tmp_dir)
                .await
                .expect("GuizangClient should initialise"),
        );
        let providers = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        let factory = Arc::new(AgentFactory::new(
            guizang,
            providers,
            config,
            Arc::new(SafetyHook::new(&SafetyConfig::default())),
            Arc::new(WorkerPool::new(4)),
            Arc::new(ConstraintEngine::new()),
            Arc::new(SkillTriggerEngine::new()),
        ));
        (factory, tmp_dir)
    }

    fn sample_meta_context() -> MetaContext {
        MetaContext {
            constraints: vec![TruthConstraint::hard(
                "truth:no-fabrication",
                "不编造事实",
                "Don't fabricate facts",
            )],
            matched_skills: vec![SkillRef {
                id: "read".into(),
                name: "文件读取".into(),
                tool_name: "read".into(),
                match_weight: 0.9,
            }],
            yang_prompt: YangPrompt {
                task_description: "Refactor the logging module.".into(),
                constraint_summaries: vec![
                    "Hard: Do not fabricate facts".into(),
                ],
                parent_deliverables: vec![],
                sibling_deliverables: vec![],
            },
            mode: AgentMode::Orchestration,
            degraded: None,
            assets_used: vec![],
            model: None,
            verify_model: None,
            fitting_system_prompt: None,
            verify_system_prompt: None,
            converge_system_prompt: None,
        }
    }

    #[test]
    fn test_build_system_prompt_orchestration_mode() {
        let ctx = sample_meta_context();
        let prompt = build_system_prompt(
            &ctx,
            &std::path::PathBuf::from("./test_data/tasks/prompt-test"),
            None,
            AgentMode::Orchestration,
        );
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("编排模式"));
        assert!(prompt.contains("Refactor the logging module"));
        assert!(prompt.contains("recursive_decompose"));
        assert!(prompt.contains("产出目录"));
        // 编排模板含拆解 + mode 设置指令。
        assert!(prompt.contains("设置 mode"));
        assert!(prompt.contains("max_depth"));
        // 综合 + 协作指令。
        assert!(prompt.contains("综合"));
        assert!(prompt.contains("封地自治"));
    }

    #[test]
    fn test_build_system_prompt_execution_mode() {
        let ctx = sample_meta_context();
        let prompt = build_system_prompt(
            &ctx,
            &std::path::PathBuf::from("./test_data/tasks/prompt-test"),
            None,
            AgentMode::Execution,
        );
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("执行模式"));
        assert!(prompt.contains("Refactor the logging module"));
        assert!(prompt.contains("产出目录"));
        // 执行模板明确无 recursive_decompose。
        assert!(prompt.contains("recursive_decompose"));
        assert!(prompt.contains("没有"));
        assert!(prompt.contains("直接用 L1 工具"));
        // 执行模式含核心工具 + 自检指令。
        assert!(prompt.contains("causal_verify"));
    }

    #[test]
    fn test_build_system_prompt_empty_context() {
        let ctx = MetaContext::empty();
        let prompt = build_system_prompt(
            &ctx,
            &std::path::PathBuf::from("./test_data/tasks/empty-test"),
            None,
            AgentMode::Orchestration,
        );
        // 空上下文降级为编排模式 Base 模板。
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("编排职责"));
        assert!(prompt.contains("产出目录"));
    }

    #[test]
    fn test_budget_discipline_mentions_thresholds_and_efficiency() {
        // V29：预算纪律段必须包含阈值数字 + 高效引导（上限是保险丝不是配额）。
        let s = build_budget_discipline(crate::infra::config::ContextLimits {
            handoff_tokens: 250_000,
            hard_cutoff_tokens: 300_000,
            compress_input_tokens: 20_000,
        });
        assert!(s.contains("250000"), "handoff 阈值必须可见: {s}");
        assert!(s.contains("300000"), "hard cutoff 阈值必须可见: {s}");
        assert!(s.contains("不是可自由消耗的配额"), "必须强调护栏语义");
        assert!(s.contains("远低于预算"), "必须引导尽快收敛");
        assert!(s.contains("残缺产出"), "必须保留残缺产出 > 无产出");
    }

    #[test]
    fn test_empty_meta_context_defaults_to_orchestration_mode() {
        // V27：降级路径 mode 默认 Orchestration（serde default 兼容旧 meta_ctx.json）。
        let ctx = MetaContext::empty();
        assert_eq!(ctx.mode, AgentMode::Orchestration);

        // 旧 JSON（无 mode 字段）反序列化 → Orchestration。
        let legacy = serde_json::json!({
            "constraints": [],
            "matched_skills": [],
            "yang_prompt": {
                "task_description": "legacy",
                "constraint_summaries": []
            },
            "fitting_system_prompt": null,
            "verify_system_prompt": null,
            "converge_system_prompt": null
        });
        let parsed: MetaContext = serde_json::from_value(legacy).expect("legacy parse");
        assert_eq!(parsed.mode, AgentMode::Orchestration);
    }

    #[test]
    fn test_identity_section_root_task() {
        // V30：根任务身份段——无父（天子）、无兄弟、类别来自 mode。
        let tmp = std::env::temp_dir().join(format!(
            "fitting_identity_root_{}_{}",
            std::process::id(),
            IDENTITY_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("meta.json"),
            serde_json::to_string(&Task {
                id: "root-1".into(),
                description: "分析源码".into(),
                depth: 0,
                status: crate::types::task::TaskStatus::Running,
                parent_id: None,
                subtask_ids: vec![],
            })
            .unwrap(),
        )
        .unwrap();

        let mut ctx = MetaContext::empty();
        ctx.mode = AgentMode::Execution;
        let engine_ctx = EngineContext {
            task_id: "root-1".into(),
            depth: 0,
            task_dir: tmp.clone(),
            cycle: 0,
            round: 0,
            context_dir: None,
        };
        let s = build_identity_section(&engine_ctx, &ctx, 3).unwrap();
        assert!(s.contains("身份与地位"), "缺身份段标题: {s}");
        assert!(s.contains("分析源码"), "缺任务内容: {s}");
        assert!(s.contains("执行模式"), "缺类别: {s}");
        assert!(s.contains("根任务（天子）"), "根任务无父分支: {s}");
        assert!(s.contains("第 0 层（共 3 层）"), "缺地位: {s}");
        assert!(s.contains("权限"), "缺权限教学: {s}");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_identity_section_child_with_parent_and_siblings() {
        // V30：子任务身份段——父册注入 + 会盟索引。
        let tmp = std::env::temp_dir().join(format!(
            "fitting_identity_child_{}_{}",
            std::process::id(),
            IDENTITY_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let parent_dir = tmp.join("root");
        let child_dir = parent_dir.join("children").join("0");
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            parent_dir.join("meta.json"),
            serde_json::to_string(&Task {
                id: "root-1".into(),
                description: "父任务描述".into(),
                depth: 0,
                status: crate::types::task::TaskStatus::Running,
                parent_id: None,
                subtask_ids: vec!["child-0".into(), "child-1".into()],
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            child_dir.join("meta.json"),
            serde_json::to_string(&Task {
                id: "child-0".into(),
                description: "子任务描述".into(),
                depth: 1,
                status: crate::types::task::TaskStatus::Running,
                parent_id: Some("root-1".into()),
                subtask_ids: vec![],
            })
            .unwrap(),
        )
        .unwrap();

        let mut ctx = MetaContext::empty();
        ctx.mode = AgentMode::Orchestration;
        ctx.yang_prompt.sibling_deliverables = vec![format!(
            "{}/children/1/deliverables",
            parent_dir.display()
        )];
        let engine_ctx = EngineContext {
            task_id: "child-0".into(),
            depth: 1,
            task_dir: child_dir.clone(),
            cycle: 0,
            round: 0,
            context_dir: None,
        };
        let s = build_identity_section(&engine_ctx, &ctx, 3).unwrap();
        assert!(s.contains("子任务描述"), "缺子任务内容: {s}");
        assert!(s.contains("编排模式"), "缺类别: {s}");
        assert!(s.contains("root-1（父任务描述）"), "父册注入: {s}");
        assert!(s.contains("第 1 层（共 3 层）"), "缺地位: {s}");
        assert!(s.contains("兄弟贡品"), "缺会盟段: {s}");
        assert!(
            s.contains("children/1/deliverables"),
            "会盟陈列室目录: {s}"
        );
        assert!(s.contains("贡品陈列室"), "教学提示: {s}");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn test_identity_section_missing_roll_is_hard_error() {
        // V30 无降级：身份册缺失 → Err 上抛，不静默降级。
        let tmp = std::env::temp_dir().join(format!(
            "fitting_identity_missing_{}_{}",
            std::process::id(),
            IDENTITY_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let engine_ctx = EngineContext {
            task_id: "x".into(),
            depth: 0,
            task_dir: tmp.clone(),
            cycle: 0,
            round: 0,
            context_dir: None,
        };
        let err = build_identity_section(&engine_ctx, &MetaContext::empty(), 3).unwrap_err();
        assert!(err.to_string().contains("meta.json"), "错误必须指向册路径: {err}");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[tokio::test]
    async fn test_fitting_agent_builder_construction() {
        let config = make_config();
        let (factory, tmp_dir) = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();
        let engine_ctx = EngineContext {
            task_id: "test-task-1".into(),
            depth: 0,
            task_dir: std::path::PathBuf::from("./test_data/tasks/test-task-1"),
            cycle: 1,
            round: 0,
            context_dir: None,
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        let builder = factory.create_fitting_agent(0, &meta_ctx, &engine_ctx, cancel);
        assert!(builder.is_ok());
        let builder = builder.unwrap();
        assert_eq!(builder.engine_ctx().task_id, "test-task-1");

        // Cleanup temp directory
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    #[ignore = "requires LLM API key (DEEPSEEK_API_KEY)"]
    async fn test_fitting_agent_run_integration() {
        // Integration test: runs the full Rig agent pipeline.
        // Requires a valid DEEPSEEK_API_KEY.
        let config = make_config();
        let (factory, tmp_dir) = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();
        let engine_ctx = EngineContext {
            task_id: "test-task-2".into(),
            depth: 1,
            task_dir: std::path::PathBuf::from("./test_data/tasks/test-task-2"),
            cycle: 1,
            round: 0,
            context_dir: None,
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        let builder = factory
            .create_fitting_agent(1, &meta_ctx, &engine_ctx, cancel)
            .expect("builder");

        let result = builder.run("Write a test for the logging module", None).await;
        // In an environment without LLM access, expect LLMCallFailed.
        // With a valid API key + Qdrant, this should return Ok(TPNResult).
        match result {
            Ok(tpn) => {
                assert!(!tpn.content.is_empty());
                assert_eq!(tpn.depth, 1);
            }
            Err(e) => {
                // Expected: provider client or LLM call failure.
                assert!(
                    format!("{:?}", e).contains("LLMCallFailed")
                        || format!("{:?}", e).contains("Safety")
                        || format!("{:?}", e).contains("Provider"),
                    "unexpected error: {e:?}"
                );
            }
        }

        // Cleanup temp directory
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_fitting_agent_depth_check() {
        let config = make_config();
        let (factory, tmp_dir) = build_factory_arc(config).await;
        let meta_ctx = sample_meta_context();

        // Depth 1, but max_depth defaults to 2 — should pass the guard.
        // Task dir must live in tmp_dir (AGENTS.md §10: tests never write
        // into tracked test_data/).
        let task_dir = tmp_dir.join("depth-test");
        let engine_ctx = EngineContext {
            task_id: "depth-test".into(),
            depth: 1,
            task_dir,
            cycle: 1,
            round: 0,
            context_dir: None,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let builder = factory
            .create_fitting_agent(1, &meta_ctx, &engine_ctx, cancel)
            .expect("builder");

        // run() should NOT return MaxDepthExceeded because 1 <= 2.
        // It will return LLMCallFailed because no API key is available.
        let result = builder.run("test", None).await;
        match result {
            Ok(_) => { /* valid result — requires API key */ }
            Err(e) => {
                match e {
                    TaijiError::MaxDepthExceeded { .. } => {
                        panic!("depth <= max_depth should not trigger MaxDepthExceeded")
                    }
                    _ => {
                        // Expected: LLMCallFailed or SafetyViolation in test env
                        tracing::debug!("run() returned expected error: {e:?}");
                    }
                }
            }
        }

        // Cleanup temp directory
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }
}

    /// V34/MVP-4：断言分级教学段注入断言（教学层与检查层双保险，§8.22）。
    #[test]
    fn assertion_discipline_prompt_injected() {
        let text = build_assertion_discipline_prompt();
        assert!(text.contains("断言分级"), "section header present");
        assert!(text.contains("[证据: 工具名]"), "evidence marker taught");
        assert!(text.contains("(推测)"), "speculation marker taught");
        assert!(text.contains("禁止编造证据"), "fabrication prohibition taught");
        assert!(text.contains("webfetch"), "allowed tools enumerated");
    }
