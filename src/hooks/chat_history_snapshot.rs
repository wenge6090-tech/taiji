//! ChatHistorySnapshotHook — snapshots the full chat history before every LLM
//! call so a crashed / failed / timed-out agent run can resume from the last
//! tool loop instead of an empty history.
//!
//! Implements the rig-core [`PromptHook`] trait.  `on_completion_call` is
//! invoked before the prompt is sent to the LLM (including inside tool loops),
//! so each snapshot contains `history + [prompt]` — the same `Vec<Message>`
//! JSON array format used by `chat_history.json` elsewhere (fitting.rs writes
//! the same format with `save_json_atomic`).
//!
//! # Failure handling
//! A failed snapshot write is only logged via `tracing::warn!` — it must never
//! abort the agent run.  Async contexts forbid `panic!` / `unwrap()` (AGENTS.md §6).

use rig::agent::{HookAction, PromptHook};
use rig::completion::{CompletionModel, Message};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::infra::trace::save_json_atomic;

/// Hook that snapshots `chat_history.json` before each LLM completion call.
///
/// # Clone
/// Required by the [`PromptHook`] trait bound. `task_dir` is wrapped in
/// `Arc<PathBuf>` to make the hook `Clone + Send + Sync` cheaply.
///
/// # Fields
/// - `task_dir`: task directory receiving `chat_history.json`.
#[derive(Clone)]
pub struct ChatHistorySnapshotHook {
    task_dir: Arc<PathBuf>,
}

impl ChatHistorySnapshotHook {
    /// Create a new snapshot hook writing to `{task_dir}/chat_history.json`.
    pub fn new(task_dir: &Path) -> Self {
        Self {
            task_dir: Arc::new(task_dir.to_path_buf()),
        }
    }
}

// ── rig-core PromptHook implementation ─────────────────────────────────

impl<M> PromptHook<M> for ChatHistorySnapshotHook
where
    M: CompletionModel,
{
    /// Called before the prompt is sent to the LLM.
    ///
    /// Merges `history + [prompt]` into a single `Vec<Message>` and atomically
    /// writes it to `{task_dir}/chat_history.json`.  Write failures are logged
    /// and swallowed — the agent run must not be affected.
    async fn on_completion_call(
        &self,
        prompt: &Message,
        history: &[Message],
    ) -> HookAction {
        let mut snapshot: Vec<Message> = history.to_vec();
        snapshot.push(prompt.clone());

        let path = self.task_dir.join("chat_history.json");
        if let Err(e) = save_json_atomic(&snapshot, &path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to snapshot chat_history"
            );
        }

        HookAction::cont()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::test_support::TestCompletionModel;

    #[tokio::test]
    async fn on_completion_call_snapshots_history_plus_prompt() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "taiji_chat_snapshot_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let hook = ChatHistorySnapshotHook::new(&tmp_dir);
        let prior: Vec<Message> = vec![Message::user("earlier"), Message::assistant("ok")];
        let action = PromptHook::<TestCompletionModel>::on_completion_call(
            &hook,
            &Message::user("hi"),
            &prior,
        )
        .await;
        assert!(matches!(action, HookAction::Continue));

        let path = tmp_dir.join("chat_history.json");
        let stored: Vec<Message> = serde_json::from_str(
            &std::fs::read_to_string(&path).expect("chat_history.json should exist"),
        )
        .expect("stored JSON should deserialize to Vec<Message>");
        assert_eq!(stored.len(), 3, "history (2) + prompt (1)");
        assert!(
            matches!(stored[2], Message::User { .. }),
            "last message should be the user prompt"
        );

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
