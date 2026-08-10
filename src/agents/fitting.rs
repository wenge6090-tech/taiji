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
            cancel,
        }
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

        // ── Obtain LLM client ──
        let client: Arc<deepseek::Client> = self.factory.providers.client("deepseek")?;

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
        let limiter = crate::hooks::context_limiter::ContextLimiter::new(
            limits.handoff_tokens,
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
        return composed.clone();
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

    // Orchestration-specific instructions with plant-growth guidance
    prompt.push_str(
        "## Instructions\n\
         你处于**编排模式 (Orchestration)**。你的职责是把复杂任务拆解为子任务\n\
         （`recursive_decompose`），汇聚子任务结果后综合产出。\n\n\
         ### 子任务模式分配指南 (Subtask Mode Assignment)\n\
         调用 `recursive_decompose` 时，为每个子任务按**任务难易程度**与\n\
         **递归层数规则**设置 `mode`：\n\n\
         - `mode: \"Execution\"` — 子任务原子、边界清晰、可用 L1 工具直接完成：\n\
           ✓ 不再需要进一步拆解\n\
           ✓ 一个聚焦的执行者即可产出完整结果\n\n\
         - `mode: \"Orchestration\"` — 子任务仍复杂、跨多个独立维度：\n\
           ✓ 需要分阶段推进（步骤 A → 验证 → 基于 A 的步骤 B）\n\
           ✓ 单次执行无法覆盖，需要继续拆解\n\n\
         ⚠️ 深度规则：当 depth+1 >= max_depth 时，子任务模式会被工具**强制**\n\
         覆盖为 Execution（叶节点无法再拆解），计划时需考虑。\n\n\
         ### 子任务协作原则（V30 分封制：能看不能写）\n\
         1. 🏰 兄弟封地自治 — 子任务之间**不能互相写入**（write 被限制在本任务\n\
            目录内），但**可以读取**兄弟贡品（会盟：身份段会列出兄弟的\n\
            deliverables 陈列室目录，需要时用 read 工具查看）。\n\
         2. 🔀 拆解应弱耦合 — 优先拆解为可并行的独立子任务；若子任务确需\n\
            兄弟产出才能完成（强依赖），拆解时标注依赖关系，由父层在\n\
            下一轮拆解时协调注入（兄弟间不直连通信）。\n\
         3. 📮 通信经父层 — 子任务间的信息往来统一由父层汇总（聚合 → 收敛 →\n\
            下一轮注入），子任务不应尝试向兄弟写入反馈。\n\n\
         ### 关键原则 (Plant Growth Principle)\n\
         1. 🌱 自然分叉 — 只在真正需要处拆解。任务树应像植物：主干 → 分支 → 叶。\n\
         2. ⚖️ 拿不准就 Execution — 过度拆解浪费轮次。能直接完成的子任务\n\
            直接设为 Execution；宁可先直接执行再修补，不要过度拆解。\n\
         3. 📊 每个节点都要产出价值 — 编排节点产出综合报告；执行节点产出\n\
            具体产物。不允许空壳节点。\n\
         4. 📏 规模感知 (Scale-Aware): 任务规模过大（涉及大量文件/大量行数）时，\n\
            优先按模块分批拆解执行；若单轮预算（轮次/超时）内无法逐一完成\n\
            全部内容，在最终报告/交付物中明确说明已覆盖范围与未覆盖部分，\n\
            不要无限重试。\n\
         5. ✅ 用 `causal_verify` 检查中间结果与约束。全部子任务完成后，\n\
            提供综合摘要（阴·收敛将据此判决）。\n\n\
         ### 产物路径 (Deliverable Paths)\n\
         Write all output files to the deliverables directory using their\n\
         **absolute paths**.  After execution, your deliverables will be\n\
         automatically collected from the directory.  If you used\n\
         `recursive_decompose`, your subtasks' deliverables will be available\n\
         in `parent_deliverables` for the synthesis phase.\n\n\
         Follow all constraints strictly — hard violations cause immediate failure.\n"
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

    // Execution-specific instructions
    prompt.push_str(
        "## Instructions\n\
         你处于**执行模式 (Execution)**。你的职责是直接使用 L1 工具完成\n\
         当前任务，产出完整、可验证的产物。你**没有** `recursive_decompose`\n\
         工具——当前任务已由元 Agent 判定为原子/单步任务，专注直接执行。\n\n\
         ### 执行原则 (Execution-First)\n\
         1. 🎯 直接用可用工具完成任务：读文件、写代码、跑命令，把工作做完。\n\n\
         2. 🎯 在单次执行中完整覆盖任务；不要尝试拆解（工具不存在）。\n\n\
         3. ✅ 用 `causal_verify` 自检输出后再收尾，检查交付物满足要求。\n\n\
         4. 📦 在 deliverables 目录产出具体产物，输出应完整、可直接使用。\n\n\
         5. 📏 规模感知 (Scale-Aware): 若任务规模超出单轮预算（轮次/超时），\n\
            在最终报告/交付物中明确说明已覆盖范围与未覆盖部分，不要无限重试。\n\n\
         遵循所有约束——硬约束违反将导致立即失败。\n"
    );

    // Deliverable path instruction (uses runtime path)
    let deliv_dir_display = task_dir.join("deliverables").display().to_string();
    prompt.push_str(&format!(
        "\n### 产物路径 (Deliverable Paths)\n\
         All files written to the deliverables directory will be automatically\n\
         collected by absolute path.  Ensure you use the full absolute path when\n\
         calling the `write` tool, e.g. `{deliv_dir_display}/report.md`.\n"
    ));
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
    use crate::infra::knowledge::LiluoClient;
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
        let liluo = Arc::new(
            LiluoClient::new(&tmp_dir)
                .await
                .expect("LiluoClient should initialise"),
        );
        let providers = Arc::new(
            ProviderRegistry::new(&config).expect("ProviderRegistry"),
        );

        let factory = Arc::new(AgentFactory::new(
            liluo,
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
        // V27 配对：编排模板教子任务模式分配（深度规则 + 难度）。
        assert!(prompt.contains("子任务模式分配指南"));
        assert!(prompt.contains("max_depth"));
        // V26.3 E4：规模感知引导保留。
        assert!(prompt.contains("规模感知"));
        assert!(prompt.contains("未覆盖"));
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
        // V27 配对：执行模板明确无 recursive_decompose（不注册，LLM 不可见）。
        assert!(prompt.contains("recursive_decompose"));
        assert!(prompt.contains("没有"));
        assert!(prompt.contains("直接使用 L1 工具"));
        // V26.3 E4：规模感知引导保留。
        assert!(prompt.contains("规模感知"));
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
        // Should still have the role header and instructions.
        assert!(prompt.contains("你是概率拟合专家"));
        assert!(prompt.contains("Instructions"));
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
