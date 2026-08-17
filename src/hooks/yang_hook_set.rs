//! YangHookSet — composes the three YangAgent hooks into a single
//! Rig 0.39 hook (safety → trace → chat-history snapshot).
//!
//! # Why this exists
//! Rig 0.39's [`AgentBuilder::hook`] is a **single slot**: each call replaces
//! the previously registered hook (`hook: Some(hook)` in the builder). Chaining
//! `.hook(a).hook(b).hook(c)` therefore keeps **only `c`** — first observed in
//! the V26.1-3 E2E smoke run as a missing `trace.jsonl` plus empty `tools_used`
//! (and, retroactively, the YangAgent SafetyHook had never actually been
//! mounted since V25, which chained `.hook(safety).hook(trace)`).
//!
//! This set forwards every [`PromptHook`] method to the three inner hooks in
//! order, short-circuiting on the first non-continue action — so a SafetyHook
//! rejection also stops downstream trace recording / snapshotting.
//!
//! [`AgentBuilder::hook`]: rig::agent::AgentBuilder::hook

use rig::agent::{
    HookAction, InvalidToolCallContext, InvalidToolCallHookAction, PromptHook,
    ToolCallHookAction,
};
use rig::completion::{CompletionModel, CompletionResponse, Message};
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};

use super::chat_history_snapshot::ChatHistorySnapshotHook;
use super::context_limiter::ContextLimiter;
use super::safety::SafetyHook;
use super::trace::TraceHook;

/// Composite hook: safety → trace → chat-history snapshot → context limiter.
///
/// `M` mirrors the agent's completion model so the set satisfies
/// `PromptHook<M>` exactly like each inner hook would. It is `Clone`
/// (required by the `PromptHook` trait bound) because all inner hooks are
/// cheaply `Clone` (`Arc`-backed state).
#[derive(Clone)]
pub struct YangHookSet<M> {
    safety: SafetyHook,
    trace: TraceHook,
    snapshot: ChatHistorySnapshotHook,
    /// V29 上下文窗口预算（AGENTS.md §14）：最后执行——Terminate 前 trace 与
    /// snapshot 已完整记录本轮 LLM 调用，不丢审计。
    limiter: ContextLimiter,
    /// V30 封地边界（AGENTS.md §13 能看不能写）：write 工具目标路径必须落在
    /// 本任务 task_dir 内（兄弟/父/无关路径写入拒绝）——SafetyHook 黑名单
    /// 只拦 `..`/`~`/`/etc`，绝对路径直写兄弟目录不触发，域校验兜底。
    task_dir: PathBuf,
    _marker: PhantomData<fn(M) -> M>,
}

impl<M> YangHookSet<M> {
    /// Compose the YangAgent hooks into a single Rig hook.
    ///
    /// Order matters: safety first (rejections short-circuit), then trace
    /// (real tool-call recording), then chat-history snapshot (last writer),
    /// then context limiter (V29 token budget — terminate as the final gate).
    pub fn new(
        safety: SafetyHook,
        trace: TraceHook,
        snapshot: ChatHistorySnapshotHook,
        limiter: ContextLimiter,
        task_dir: PathBuf,
    ) -> Self {
        Self {
            safety,
            trace,
            snapshot,
            limiter,
            task_dir,
            _marker: PhantomData,
        }
    }
}

/// V30 封地边界（AGENTS.md §13 能看不能写）：write 工具目标路径归一化后必须
/// 落在本任务 task_dir 内（词法级，无需文件系统访问——目标文件可能尚不存在）。
/// 相对路径按 task_dir 解析（sandbox 语义：相对路径永不出封地）。
/// 非 write 工具放行；args 非 JSON 或缺 path → 拒绝（工具契约 §15 违反）。
pub(crate) fn check_write_domain(
    task_dir: &Path,
    tool_name: &str,
    args: &str,
) -> Result<(), String> {
    if tool_name != "write" {
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_str(args)
        .map_err(|e| format!("write args 非 JSON（工具契约违反）: {e}"))?;
    let path = value
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| "write args 缺 path 字段（工具契约违反）".to_string())?;
    let target = normalize_path(&task_dir.join(path));
    if !target.starts_with(task_dir) {
        return Err(format!(
            "写路径越出封地: {}（task_dir={}）",
            target.display(),
            task_dir.display()
        ));
    }
    Ok(())
}

/// 词法归一化：解析 `.`/`..`，不访问文件系统（目标可能尚不存在）。
pub(crate) fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Forward one method to all three hooks in order; the first non-continue
/// action short-circuits and is returned from the enclosing method.
/// UFCS (`<$T as PromptHook<M>>`) pins the completion-model parameter: the
/// field types are concrete, so plain method syntax cannot infer `M`.
macro_rules! fwd {
    ($T:ty, $hook:expr, $method:ident, $cont:path $(, $arg:expr)*) => {
        match <$T as PromptHook<M>>::$method(&$hook $(, $arg)*).await {
            $cont => {}
            action => return action,
        }
    };
}

impl<M: CompletionModel> PromptHook<M> for YangHookSet<M> {
    async fn on_completion_call(&self, prompt: &Message, history: &[Message]) -> HookAction {
        fwd!(SafetyHook, self.safety, on_completion_call, HookAction::Continue, prompt, history);
        fwd!(TraceHook, self.trace, on_completion_call, HookAction::Continue, prompt, history);
        fwd!(ChatHistorySnapshotHook, self.snapshot, on_completion_call, HookAction::Continue, prompt, history);
        HookAction::cont()
    }

    async fn on_completion_response(
        &self,
        prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        fwd!(SafetyHook, self.safety, on_completion_response, HookAction::Continue, prompt, response);
        fwd!(TraceHook, self.trace, on_completion_response, HookAction::Continue, prompt, response);
        fwd!(ChatHistorySnapshotHook, self.snapshot, on_completion_response, HookAction::Continue, prompt, response);
        // V29: token 预算最后执行——超限 Terminate 前，trace/snapshot 已完整记录
        fwd!(ContextLimiter, self.limiter, on_completion_response, HookAction::Continue, prompt, response);
        HookAction::cont()
    }

    async fn on_invalid_tool_call(
        &self,
        context: &InvalidToolCallContext,
    ) -> InvalidToolCallHookAction {
        let action =
            <SafetyHook as PromptHook<M>>::on_invalid_tool_call(&self.safety, context).await;
        if action != InvalidToolCallHookAction::Fail {
            return action;
        }
        let action =
            <TraceHook as PromptHook<M>>::on_invalid_tool_call(&self.trace, context).await;
        if action != InvalidToolCallHookAction::Fail {
            return action;
        }
        let action = <ChatHistorySnapshotHook as PromptHook<M>>::on_invalid_tool_call(
            &self.snapshot,
            context,
        )
        .await;
        if action != InvalidToolCallHookAction::Fail {
            return action;
        }
        InvalidToolCallHookAction::fail()
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        fwd!(SafetyHook, self.safety, on_tool_call, ToolCallHookAction::Continue, tool_name, tool_call_id.clone(), internal_call_id, args);
        // V30 封地边界（AGENTS.md §13 能看不能写）：safety 黑名单后追加 write
        // 域校验——write 目标必须在 task_dir 内，兄弟/父/无关路径写入拒绝。
        if let Err(e) = check_write_domain(&self.task_dir, tool_name, args) {
            tracing::warn!(
                tool_name,
                args_len = args.len(),
                task_dir = %self.task_dir.display(),
                "{e}"
            );
            return ToolCallHookAction::skip(format!("封地边界: {e}"));
        }
        fwd!(TraceHook, self.trace, on_tool_call, ToolCallHookAction::Continue, tool_name, tool_call_id.clone(), internal_call_id, args);
        fwd!(ChatHistorySnapshotHook, self.snapshot, on_tool_call, ToolCallHookAction::Continue, tool_name, tool_call_id, internal_call_id, args);
        ToolCallHookAction::cont()
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        result: &str,
    ) -> HookAction {
        fwd!(SafetyHook, self.safety, on_tool_result, HookAction::Continue, tool_name, tool_call_id.clone(), internal_call_id, args, result);
        fwd!(TraceHook, self.trace, on_tool_result, HookAction::Continue, tool_name, tool_call_id.clone(), internal_call_id, args, result);
        fwd!(ChatHistorySnapshotHook, self.snapshot, on_tool_result, HookAction::Continue, tool_name, tool_call_id, internal_call_id, args, result);
        HookAction::cont()
    }

    async fn on_text_delta(&self, text_delta: &str, aggregated_text: &str) -> HookAction {
        fwd!(SafetyHook, self.safety, on_text_delta, HookAction::Continue, text_delta, aggregated_text);
        fwd!(TraceHook, self.trace, on_text_delta, HookAction::Continue, text_delta, aggregated_text);
        fwd!(ChatHistorySnapshotHook, self.snapshot, on_text_delta, HookAction::Continue, text_delta, aggregated_text);
        HookAction::cont()
    }

    async fn on_tool_call_delta(
        &self,
        tool_call_id: &str,
        internal_call_id: &str,
        tool_name: Option<&str>,
        tool_call_delta: &str,
    ) -> HookAction {
        fwd!(SafetyHook, self.safety, on_tool_call_delta, HookAction::Continue, tool_call_id, internal_call_id, tool_name, tool_call_delta);
        fwd!(TraceHook, self.trace, on_tool_call_delta, HookAction::Continue, tool_call_id, internal_call_id, tool_name, tool_call_delta);
        fwd!(ChatHistorySnapshotHook, self.snapshot, on_tool_call_delta, HookAction::Continue, tool_call_id, internal_call_id, tool_name, tool_call_delta);
        HookAction::cont()
    }

    async fn on_stream_completion_response_finish(
        &self,
        prompt: &Message,
        response: &<M as CompletionModel>::StreamingResponse,
    ) -> HookAction {
        fwd!(SafetyHook, self.safety, on_stream_completion_response_finish, HookAction::Continue, prompt, response);
        fwd!(TraceHook, self.trace, on_stream_completion_response_finish, HookAction::Continue, prompt, response);
        fwd!(ChatHistorySnapshotHook, self.snapshot, on_stream_completion_response_finish, HookAction::Continue, prompt, response);
        HookAction::cont()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::test_support::TestCompletionModel;
    use crate::infra::config::SafetyConfig;
    use crate::infra::trace::{load_json_optional, TraceRecord};
    use crate::types::execution::EngineContext;

    fn make_context(tag: &str) -> EngineContext {
        let dir = std::env::temp_dir().join(format!(
            "yang_hook_set_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tmp task dir");
        EngineContext {
            task_id: format!("hook-set-{tag}"),
            depth: 0,
            task_dir: dir,
            cycle: 0,
            round: 0,
            context_dir: None,
        }
    }

    fn make_hook_set<M>(ctx: &EngineContext) -> YangHookSet<M>
    where
        M: CompletionModel,
    {
        let safety = SafetyHook::new(&SafetyConfig::default());
        let trace = TraceHook::new(ctx, "test-model");
        let snapshot = ChatHistorySnapshotHook::new(&ctx.task_dir);
        let limiter = ContextLimiter::new(250_000, 300_000);
        YangHookSet::new(safety, trace, snapshot, limiter, ctx.task_dir.clone())
    }

    #[tokio::test]
    async fn test_yang_hook_set_forwards_completion_call_to_all_hooks() {
        let ctx = make_context("forward");
        let hook_set = make_hook_set::<TestCompletionModel>(&ctx);

        let msg = Message::user("hello");
        let action = hook_set.on_completion_call(&msg, &[]).await;

        assert_eq!(action, HookAction::Continue);
        // trace hook wrote trace.jsonl (JSONL format — one record per line)
        let content = std::fs::read_to_string(&ctx.task_dir.join("trace.jsonl"))
            .expect("trace.jsonl exists — trace hook must be reached");
        let has_completion_call = content.lines().any(|line| {
            serde_json::from_str::<TraceRecord>(line)
                .map(|r| r.phase == "completion_call")
                .unwrap_or(false)
        });
        assert!(
            has_completion_call,
            "trace hook must record completion_call"
        );
        // snapshot hook wrote chat_history.json
        let history = load_json_optional::<Vec<Message>>(&ctx.task_dir.join("chat_history.json"))
            .expect("chat_history loadable")
            .expect("chat_history.json exists — snapshot hook must be reached");
        assert_eq!(history.len(), 1, "snapshot must contain the single prompt");

        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }

    #[tokio::test]
    async fn test_yang_hook_set_safety_rejection_short_circuits_tool_call() {
        let ctx = make_context("shortcircuit");
        let hook_set = make_hook_set::<TestCompletionModel>(&ctx);

        // Path traversal on the `read` tool → SafetyHook must reject with Skip,
        // and the rejected tool must NOT reach the trace hook (no recording).
        let action = hook_set
            .on_tool_call("read", None, "call-1", r#"{"path": "../../../etc/passwd"}"#)
            .await;

        assert!(
            matches!(action, ToolCallHookAction::Skip { .. }),
            "safety rejection must short-circuit to Skip, got {action:?}"
        );
        assert_eq!(
            hook_set.trace.tools_called(),
            Vec::<String>::new(),
            "rejected tool call must not be recorded by trace"
        );

        // A benign call passes through and IS recorded.
        let action = hook_set
            .on_tool_call("read", None, "call-2", r#"{"path": "src/lib.rs"}"#)
            .await;
        assert_eq!(action, ToolCallHookAction::Continue);
        assert_eq!(
            hook_set.trace.tools_called(),
            vec!["read".to_string()],
            "benign tool call must reach the trace hook"
        );

        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }

    #[test]
    fn test_yang_hook_set_is_clone() {
        let ctx = make_context("clone");
        let hook_set = make_hook_set::<TestCompletionModel>(&ctx);
        // Clones share the Arc-backed trace state.
        let _cloned = hook_set.clone();
        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }

    #[test]
    fn test_write_domain_allows_inside_task_dir() {
        // V30 封地边界：write 落在 task_dir 内（绝对与相对路径）→ 放行。
        let ctx = make_context("domain_allow");
        let dir = ctx.task_dir.clone();
        let inner_abs = dir.join("deliverables/report.md");
        assert!(check_write_domain(&dir, "write", &serde_json::json!({
            "path": inner_abs.to_string_lossy(),
            "content": "x"
        }).to_string()).is_ok());
        // 相对路径按 task_dir 解析（sandbox 语义）
        assert!(check_write_domain(&dir, "write", &serde_json::json!({
            "path": "deliverables/a.md",
            "content": "x"
        }).to_string()).is_ok());
        // 非 write 工具放行
        assert!(check_write_domain(&dir, "read", r#"{"path": "/etc/passwd"}"#).is_ok());
        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }

    #[test]
    fn test_write_domain_rejects_sibling_and_escape() {
        // V30 封地边界：写兄弟目录（绝对路径直写，无 `..`）→ 拒绝；
        // `..` 逃逸 → 拒绝；args 缺 path → 拒绝。
        let ctx = make_context("domain_deny");
        let dir = ctx.task_dir.clone();
        let sibling = dir.parent().unwrap().join("1/deliverables/pollute.md");
        let err = check_write_domain(&dir, "write", &serde_json::json!({
            "path": sibling.to_string_lossy(),
            "content": "pollute"
        }).to_string()).unwrap_err();
        assert!(err.contains("越出封地"), "错误信息: {err}");

        let err = check_write_domain(&dir, "write", &serde_json::json!({
            "path": "../../etc/passwd",
            "content": "x"
        }).to_string()).unwrap_err();
        assert!(err.contains("越出封地"), "`..` 逃逸必须拒绝: {err}");

        let err = check_write_domain(&dir, "write", "{}").unwrap_err();
        assert!(err.contains("缺 path"), "缺 path 必须拒绝: {err}");
        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }

    #[tokio::test]
    async fn test_yang_hook_set_write_domain_short_circuits_tool_call() {
        // V30 封地边界：hook 链上 write 越出 task_dir → Skip，且不进 trace。
        let ctx = make_context("domain_hook");
        let hook_set = make_hook_set::<TestCompletionModel>(&ctx);
        let sibling = ctx
            .task_dir
            .parent()
            .unwrap()
            .join("9/deliverables/pollute.md");
        let args = serde_json::json!({"path": sibling.to_string_lossy(), "content": "x"}).to_string();
        let action = hook_set
            .on_tool_call("write", None, "call-1", &args)
            .await;
        assert!(
            matches!(action, ToolCallHookAction::Skip { .. }),
            "越界 write 必须 Skip, got {action:?}"
        );
        assert_eq!(
            hook_set.trace.tools_called(),
            Vec::<String>::new(),
            "越界 write 不得进 trace"
        );

        // 域内 write 放行并记录
        let ok_args = serde_json::json!({
            "path": ctx.task_dir.join("deliverables/ok.md").to_string_lossy(),
            "content": "x"
        }).to_string();
        let action = hook_set
            .on_tool_call("write", None, "call-2", &ok_args)
            .await;
        assert_eq!(action, ToolCallHookAction::Continue);
        assert_eq!(
            hook_set.trace.tools_called(),
            vec!["write".to_string()],
            "域内 write 必须进 trace"
        );
        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }

    #[test]
    fn test_normalize_path_resolves_dots() {
        assert_eq!(
            normalize_path(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(
            normalize_path(Path::new("/a/../../b")),
            PathBuf::from("/b")
        );
        assert_eq!(normalize_path(Path::new("a/b")), PathBuf::from("a/b"));
    }

    #[tokio::test]
    async fn test_yang_hook_set_limiter_terminates_on_budget_exceeded() {
        // V29 上下文预算：hook 链尾的 ContextLimiter 超限时 Terminate
        // （safety → trace → snapshot 已记录，最后 gate 生效）。
        let ctx = make_context("limiter");
        let safety = SafetyHook::new(&SafetyConfig::default());
        let trace = TraceHook::new(&ctx, "test-model");
        let snapshot = ChatHistorySnapshotHook::new(&ctx.task_dir);
        let limiter = ContextLimiter::new(10, 100); // 小阈值便于触发
        let hook_set = YangHookSet::<TestCompletionModel>::new(
            safety,
            trace,
            snapshot,
            limiter.clone(),
            ctx.task_dir.clone(),
        );

        // 一次响应 input_tokens=60 >= 10 → Terminate + triggered=Handoff
        let mut usage = rig::completion::Usage::new();
        usage.input_tokens = 60;
        let response = CompletionResponse {
            choice: rig::OneOrMany::one(rig::completion::AssistantContent::text("ok")),
            usage,
            raw_response: TestCompletionModel,
            message_id: None,
        };
        let action = hook_set
            .on_completion_response(&Message::user("u"), &response)
            .await;
        assert_eq!(
            action,
            HookAction::Terminate {
                reason: "context_overflow".into()
            },
            "budget exceed must terminate the agent loop"
        );
        assert_eq!(
            limiter.triggered(),
            Some(crate::hooks::context_limiter::LimitKind::Handoff)
        );

        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }
}
