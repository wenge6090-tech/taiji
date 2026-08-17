//! YinHookSet — composes the two YinAgent hooks into a single Rig 0.39
//! hook (safety → context limiter).
//!
//! # Why this exists
//! Rig 0.39's [`AgentBuilder::hook`] is a **single slot**: each call replaces
//! the previously registered hook. The YinAgent mounts `SafetyHook` (带工具
//! 必有安全钩子) and, since V49, a `ContextLimiter` (阴预算对称，AGENTS.md §14) —
//! two hooks, one slot, so they must be composed in ONE `.hook()` call.
//!
//! This set forwards every [`PromptHook`] method to the two inner hooks in
//! order, short-circuiting on the first non-continue action — a SafetyHook
//! rejection also stops the downstream budget check.
//!
//! [`AgentBuilder::hook`]: rig::agent::AgentBuilder::hook

use rig::agent::{
    HookAction, InvalidToolCallContext, InvalidToolCallHookAction, PromptHook,
    ToolCallHookAction,
};
use rig::completion::{CompletionModel, CompletionResponse, Message};
use std::marker::PhantomData;

use super::context_limiter::ContextLimiter;
use super::safety::SafetyHook;

/// Composite hook: safety → context limiter.
///
/// `M` mirrors the agent's completion model so the set satisfies
/// `PromptHook<M>` exactly like each inner hook would. It is `Clone`
/// (required by the `PromptHook` trait bound) because both inner hooks are
/// cheaply `Clone` (`Arc`-backed state).
#[derive(Clone)]
pub struct YinHookSet<M> {
    safety: SafetyHook,
    /// V49 阴预算（AGENTS.md §14）：最后执行——Terminate 前 safety 已完成校验。
    limiter: ContextLimiter,
    _marker: PhantomData<fn(M) -> M>,
}

impl<M> YinHookSet<M> {
    /// Compose the YinAgent hooks into a single Rig hook.
    ///
    /// Order matters: safety first (rejections short-circuit), then context
    /// limiter (token budget — terminate as the final gate).
    pub fn new(safety: SafetyHook, limiter: ContextLimiter) -> Self {
        Self {
            safety,
            limiter,
            _marker: PhantomData,
        }
    }
}

/// Forward one method to the safety hook (and, for `on_completion_response`,
/// the limiter) in order; the first non-continue action short-circuits and is
/// returned from the enclosing method. UFCS (`<$T as PromptHook<M>>`) pins
/// the completion-model parameter.
macro_rules! fwd {
    ($T:ty, $hook:expr, $method:ident, $cont:path $(, $arg:expr)*) => {
        match <$T as PromptHook<M>>::$method(&$hook $(, $arg)*).await {
            $cont => {}
            action => return action,
        }
    };
}

impl<M: CompletionModel> PromptHook<M> for YinHookSet<M> {
    async fn on_completion_call(&self, prompt: &Message, history: &[Message]) -> HookAction {
        fwd!(SafetyHook, self.safety, on_completion_call, HookAction::Continue, prompt, history);
        HookAction::cont()
    }

    async fn on_completion_response(
        &self,
        prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        fwd!(SafetyHook, self.safety, on_completion_response, HookAction::Continue, prompt, response);
        // V49: token 预算最后执行——超限 Terminate 前 safety 已完成校验。
        fwd!(ContextLimiter, self.limiter, on_completion_response, HookAction::Continue, prompt, response);
        HookAction::cont()
    }

    async fn on_invalid_tool_call(
        &self,
        context: &InvalidToolCallContext,
    ) -> InvalidToolCallHookAction {
        <SafetyHook as PromptHook<M>>::on_invalid_tool_call(&self.safety, context).await
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        fwd!(SafetyHook, self.safety, on_tool_call, ToolCallHookAction::Continue, tool_name, tool_call_id, internal_call_id, args);
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
        fwd!(SafetyHook, self.safety, on_tool_result, HookAction::Continue, tool_name, tool_call_id, internal_call_id, args, result);
        HookAction::cont()
    }

    async fn on_text_delta(&self, text_delta: &str, aggregated_text: &str) -> HookAction {
        fwd!(SafetyHook, self.safety, on_text_delta, HookAction::Continue, text_delta, aggregated_text);
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
        HookAction::cont()
    }

    async fn on_stream_completion_response_finish(
        &self,
        prompt: &Message,
        response: &<M as CompletionModel>::StreamingResponse,
    ) -> HookAction {
        fwd!(SafetyHook, self.safety, on_stream_completion_response_finish, HookAction::Continue, prompt, response);
        HookAction::cont()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::test_support::TestCompletionModel;
    use crate::infra::config::SafetyConfig;
    use rig::completion::{AssistantContent, Usage};
    use rig::OneOrMany;

    fn response_with_input_tokens(n: u64) -> CompletionResponse<TestCompletionModel> {
        let mut usage = Usage::new();
        usage.input_tokens = n;
        CompletionResponse {
            choice: OneOrMany::one(AssistantContent::text("ok")),
            usage,
            raw_response: TestCompletionModel,
            message_id: None,
        }
    }

    #[test]
    fn test_yin_hook_set_is_clone() {
        let set = YinHookSet::<TestCompletionModel>::new(
            SafetyHook::new(&SafetyConfig::default()),
            ContextLimiter::new(100, 200),
        );
        let _cloned = set.clone();
    }

    #[tokio::test]
    async fn test_yin_hook_set_limiter_terminates_on_budget_exceeded() {
        // V49 阴预算：hook 链尾的 ContextLimiter 超限时 Terminate。
        let safety = SafetyHook::new(&SafetyConfig::default());
        let limiter = ContextLimiter::new(10, 100); // 小阈值便于触发
        let set = YinHookSet::<TestCompletionModel>::new(safety, limiter.clone());

        let action = set
            .on_completion_response(&Message::user("u"), &response_with_input_tokens(60))
            .await;
        assert_eq!(
            action,
            HookAction::Terminate {
                reason: "context_overflow".into()
            },
            "budget exceed must terminate the agent loop"
        );
        assert_eq!(limiter.triggered(), Some(super::super::context_limiter::LimitKind::Handoff));
    }

    #[tokio::test]
    async fn test_yin_hook_set_safety_short_circuits_tool_call() {
        // 越权 read（路径逃逸）→ SafetyHook 拒绝，不进后续。
        let safety = SafetyHook::new(&SafetyConfig::default());
        let limiter = ContextLimiter::new(100, 200);
        let set = YinHookSet::<TestCompletionModel>::new(safety, limiter);

        let action = set
            .on_tool_call("read", None, "call-1", r#"{"path": "../../../etc/passwd"}"#)
            .await;
        assert!(
            matches!(action, ToolCallHookAction::Skip { .. }),
            "safety rejection must short-circuit to Skip, got {action:?}"
        );

        // 良性调用放行
        let action = set
            .on_tool_call("read", None, "call-2", r#"{"path": "deliverables/a.md"}"#)
            .await;
        assert_eq!(action, ToolCallHookAction::Continue);
    }
}
