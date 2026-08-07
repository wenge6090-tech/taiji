//! FittingHookSet — composes the three FittingAgent hooks into a single
//! Rig 0.39 hook (safety → trace → chat-history snapshot).
//!
//! # Why this exists
//! Rig 0.39's [`AgentBuilder::hook`] is a **single slot**: each call replaces
//! the previously registered hook (`hook: Some(hook)` in the builder). Chaining
//! `.hook(a).hook(b).hook(c)` therefore keeps **only `c`** — first observed in
//! the V26.1-3 E2E smoke run as a missing `trace.jsonl` plus empty `tools_used`
//! (and, retroactively, the FittingAgent SafetyHook had never actually been
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

use super::chat_history_snapshot::ChatHistorySnapshotHook;
use super::safety::SafetyHook;
use super::trace::TraceHook;

/// Composite hook: safety → trace → chat-history snapshot.
///
/// `M` mirrors the agent's completion model so the set satisfies
/// `PromptHook<M>` exactly like each inner hook would. It is `Clone`
/// (required by the `PromptHook` trait bound) because all inner hooks are
/// cheaply `Clone` (`Arc`-backed state).
#[derive(Clone)]
pub struct FittingHookSet<M> {
    safety: SafetyHook,
    trace: TraceHook,
    snapshot: ChatHistorySnapshotHook,
    _marker: PhantomData<fn(M) -> M>,
}

impl<M> FittingHookSet<M> {
    /// Compose the three FittingAgent hooks into a single Rig hook.
    ///
    /// Order matters: safety first (rejections short-circuit), then trace
    /// (real tool-call recording), then chat-history snapshot (last writer).
    pub fn new(
        safety: SafetyHook,
        trace: TraceHook,
        snapshot: ChatHistorySnapshotHook,
    ) -> Self {
        Self {
            safety,
            trace,
            snapshot,
            _marker: PhantomData,
        }
    }
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

impl<M: CompletionModel> PromptHook<M> for FittingHookSet<M> {
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
            "fitting_hook_set_{tag}_{}",
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

    fn make_hook_set<M>(ctx: &EngineContext) -> FittingHookSet<M>
    where
        M: CompletionModel,
    {
        let safety = SafetyHook::new(&SafetyConfig::default());
        let trace = TraceHook::new(ctx, "test-model");
        let snapshot = ChatHistorySnapshotHook::new(&ctx.task_dir);
        FittingHookSet::new(safety, trace, snapshot)
    }

    #[tokio::test]
    async fn test_fitting_hook_set_forwards_completion_call_to_all_hooks() {
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
    async fn test_fitting_hook_set_safety_rejection_short_circuits_tool_call() {
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
    fn test_fitting_hook_set_is_clone() {
        let ctx = make_context("clone");
        let hook_set = make_hook_set::<TestCompletionModel>(&ctx);
        // Clones share the Arc-backed trace state.
        let _cloned = hook_set.clone();
        let _ = std::fs::remove_dir_all(&ctx.task_dir);
    }
}
