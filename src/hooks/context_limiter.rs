//! ContextLimiter — V29 上下文窗口预算（BCP §8.19）。
//!
//! 精准 token 统计替换 max_turns 轮次：`on_completion_response` 累计
//! `response.usage.input_tokens`（provider 报告的真实请求 token 数，含历史
//! 重放与工具结果），累计值 >= handoff_tokens 返回
//! `HookAction::Terminate("context_overflow")`（必须产出交接文件 → BACK_TO_TPN
//! 拆解）；>= hard_cutoff_tokens 返回 `Terminate("hard_cutoff")`（硬截止 →
//! 直接 FAIL，预算保护）。
//!
//! 轮次计数器（max_rounds / max_cycles）降级为循环防护，不再承担上下文管理。

use rig::agent::{HookAction, PromptHook};
use rig::completion::{CompletionModel, CompletionResponse, Message};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 已触发的预算限制类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// >= handoff_tokens：必须写交接文件，BACK_TO_TPN 拆解（粒度错误信号）
    Handoff,
    /// >= hard_cutoff_tokens：硬截止，直接 FAIL（预算保护）
    HardCutoff,
}

/// 上下文窗口预算 hook。`Clone` 满足 `PromptHook` trait bound（Arc 共享状态）。
#[derive(Clone, Default)]
pub struct ContextLimiter {
    handoff: u64,
    hard: u64,
    used: Arc<AtomicU64>,
    triggered: Arc<Mutex<Option<LimitKind>>>,
}

impl ContextLimiter {
    pub fn new(handoff_tokens: u64, hard_cutoff_tokens: u64) -> Self {
        Self {
            handoff: handoff_tokens,
            hard: hard_cutoff_tokens,
            used: Arc::new(AtomicU64::new(0)),
            triggered: Arc::new(Mutex::new(None)),
        }
    }

    /// 已触发的限制类型（供 Fitting 错误路径映射为 ContextOverflow / HardCutoff）。
    /// 未触发返回 None。
    pub fn triggered(&self) -> Option<LimitKind> {
        self.triggered.lock().map(|g| *g).unwrap_or(None)
    }

    /// 累计上下文 token 数（审计 / 冒烟验证）。
    pub fn tokens_used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }
}

impl<M: CompletionModel> PromptHook<M> for ContextLimiter {
    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let used = self
            .used
            .fetch_add(response.usage.input_tokens, Ordering::Relaxed)
            + response.usage.input_tokens;
        tracing::debug!(tokens_used = used, "ContextLimiter: {used} tokens used");

        if used >= self.hard {
            if let Ok(mut g) = self.triggered.lock() {
                *g = Some(LimitKind::HardCutoff);
            }
            return HookAction::Terminate {
                reason: "hard_cutoff".into(),
            };
        }
        if used >= self.handoff {
            if let Ok(mut g) = self.triggered.lock() {
                *g = Some(LimitKind::Handoff);
            }
            return HookAction::Terminate {
                reason: "context_overflow".into(),
            };
        }
        HookAction::cont()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::test_support::TestCompletionModel;
    use rig::completion::{AssistantContent, Usage};
    use rig::OneOrMany;

    /// 构造带指定 input_tokens 的 CompletionResponse（字段全 pub，可直接构造）。
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

    #[tokio::test]
    async fn test_continues_below_threshold() {
        let limiter = ContextLimiter::new(100, 200);
        let action = <ContextLimiter as PromptHook<TestCompletionModel>>::on_completion_response(
            &limiter,
            &Message::user("u"),
            &response_with_input_tokens(60),
        )
        .await;
        assert_eq!(action, HookAction::Continue);
        assert!(limiter.triggered().is_none());
        assert_eq!(limiter.tokens_used(), 60);
    }

    #[tokio::test]
    async fn test_handoff_threshold_triggers_terminate() {
        let limiter = ContextLimiter::new(100, 200);
        // 60 + 60 = 120 >= 100 → Handoff
        let _ = <ContextLimiter as PromptHook<TestCompletionModel>>::on_completion_response(
            &limiter,
            &Message::user("u"),
            &response_with_input_tokens(60),
        )
        .await;
        let action = <ContextLimiter as PromptHook<TestCompletionModel>>::on_completion_response(
            &limiter,
            &Message::user("u"),
            &response_with_input_tokens(60),
        )
        .await;
        assert_eq!(
            action,
            HookAction::Terminate {
                reason: "context_overflow".into()
            }
        );
        assert_eq!(limiter.triggered(), Some(LimitKind::Handoff));
        assert_eq!(limiter.tokens_used(), 120);
    }

    #[tokio::test]
    async fn test_hard_cutoff_takes_priority() {
        let limiter = ContextLimiter::new(100, 200);
        // 一次 250 >= 200 → HardCutoff（跳过 Handoff）
        let action = <ContextLimiter as PromptHook<TestCompletionModel>>::on_completion_response(
            &limiter,
            &Message::user("u"),
            &response_with_input_tokens(250),
        )
        .await;
        assert_eq!(
            action,
            HookAction::Terminate {
                reason: "hard_cutoff".into()
            }
        );
        assert_eq!(limiter.triggered(), Some(LimitKind::HardCutoff));
    }
}
