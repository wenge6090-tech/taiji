//! TraceHook — auto-captures StepEvents and writes them to `trace.jsonl`.
//!
//! Implements the rig-core [`PromptHook`] trait to observe completion calls,
//! completion responses, tool calls, and tool results. Each event is recorded
//! as a [`TraceRecord`] with a timestamp, execution context, duration, and
//! redacted input/output payloads.
//!
//! # Sensitive data
//! All payloads are run through [`redact_sensitive`] before writing, which
//! replaces values of keys named `"api_key"` or `"token"` with `"***REDACTED***"`,
//! plus values matching prefixed key patterns (`sk-…`/`ds-…`/`ghp_…`/`AKIA…`).

use crate::infra::trace::{TraceRecord, TraceWriter};
use crate::types::execution::EngineContext;
use chrono::Utc;
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Hook that records agent execution steps to a JSONL trace file.
///
/// # Clone
/// Required by the [`PromptHook`] trait bound. Interior mutability via
/// `Arc<…>` ensures thread-safe state across async hook invocations.
///
/// # Fields
/// - `writer`: writes [`TraceRecord`] entries to `trace.jsonl` under `task_dir`.
/// - `context`: execution context (task_id, cycle, depth, round).
/// - `provider_model`: identifier string for the LLM provider + model.
/// - `completion_start`: tracks `Instant` of the most recent completion call.
/// - `last_completion_input`: cached input for pairing with the response hook.
/// - `tool_starts`: maps `internal_call_id → (Instant, input_string)` for
///   pairing tool-call hooks with their result hooks.
/// - `tools_called`: de-duplicated, insertion-ordered names of every tool
///   actually invoked (recorded in [`PromptHook::on_tool_call`]).
#[derive(Clone)]
pub struct TraceHook {
    writer: Arc<TraceWriter>,
    context: EngineContext,
    provider_model: String,
    completion_start: Arc<Mutex<Option<Instant>>>,
    last_completion_input: Arc<Mutex<Option<String>>>,
    tool_starts: Arc<Mutex<HashMap<String, (Instant, String)>>>,
    tools_called: Arc<Mutex<Vec<String>>>,
}

impl TraceHook {
    /// Create a new trace hook that writes records into
    /// `{context.task_dir}/trace.jsonl`.
    pub fn new(context: &EngineContext, provider_model: &str) -> Self {
        Self {
            writer: Arc::new(TraceWriter::new(&context.task_dir)),
            context: context.clone(),
            provider_model: provider_model.to_string(),
            completion_start: Arc::new(Mutex::new(None)),
            last_completion_input: Arc::new(Mutex::new(None)),
            tool_starts: Arc::new(Mutex::new(HashMap::new())),
            tools_called: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Return the names of every tool actually invoked during this agent run,
    /// de-duplicated and in first-call order.
    ///
    /// Populated by [`PromptHook::on_tool_call`] — this covers both L1 Skills
    /// and built-in composite tools (`recursive_decompose`, `yin_verify`),
    /// and does not rely on text-matching the LLM response.
    pub fn tools_called(&self) -> Vec<String> {
        if let Ok(guard) = self.tools_called.lock() {
            guard.clone()
        } else {
            Vec::new()
        }
    }

    // ── Public API ────────────────────────────────────────────────────

    /// Convenience helper: create a [`TraceRecord`] with the current timestamp,
    /// the hook's context and provider model, and write it via the inner writer.
    ///
    /// Sensitive keys in both `input` and `output` are redacted automatically.
    /// Duration is set to 0 — call [`TraceWriter::write`] directly if you need
    /// a non-zero `duration_ms`.
    pub fn write_record(&self, phase: &str, input: Value, output: Value) {
        let record = TraceRecord {
            ts: Utc::now().to_rfc3339(),
            cycle: self.context.cycle,
            depth: self.context.depth,
            task_id: self.context.task_id.clone(),
            phase: phase.to_string(),
            provider_model: self.provider_model.clone(),
            duration_ms: 0,
            input: Self::redact_sensitive(&input),
            output: Self::redact_sensitive(&output),
            degraded: false,
            constraint_violations: None,
        };

        if let Err(e) = self.writer.write(&record) {
            tracing::error!(
                task_id = %self.context.task_id,
                phase = %phase,
                error = %e,
                "Failed to write trace record"
            );
        }
    }

    /// Recursively walk a JSON value and redact sensitive information.
    ///
    /// Single-source-of-truth implementation lives in [`TraceWriter::redact_sensitive`]
    /// (`infra/trace.rs`) — this is a thin re-export so hooks and callers share
    /// one rule set and cannot drift (V26.3 E3 + V26.5: removed the duplicate
    /// copy that still carried the generic `{40,}` rule and nuked innocent long
    /// strings during `TraceWriter::write`'s second pass).
    ///
    /// # Rules (see infra for details)
    /// - Key-based: `api_key` / `token` / `secret` / `password` (case-insensitive)
    /// - Value-based: prefixed key patterns only (`sk-…`/`ds-…`/`ghp_…`/`AKIA…`)
    pub fn redact_sensitive(value: &Value) -> Value {
        TraceWriter::redact_sensitive(value)
    }

    // ── Private helpers ───────────────────────────────────────────────

    fn write_tool_record(
        &self,
        phase: &str,
        input: Value,
        output: Value,
        duration_ms: u64,
    ) {
        let record = TraceRecord {
            ts: Utc::now().to_rfc3339(),
            cycle: self.context.cycle,
            depth: self.context.depth,
            task_id: self.context.task_id.clone(),
            phase: phase.to_string(),
            provider_model: self.provider_model.clone(),
            duration_ms,
            input: Self::redact_sensitive(&input),
            output: Self::redact_sensitive(&output),
            degraded: false,
            constraint_violations: None,
        };

        if let Err(e) = self.writer.write(&record) {
            tracing::error!(
                task_id = %self.context.task_id,
                phase = %phase,
                duration_ms,
                error = %e,
                "Failed to write trace record"
            );
        }
    }
}

// ── rig-core PromptHook implementation ─────────────────────────────────

impl<M> PromptHook<M> for TraceHook
where
    M: CompletionModel,
{
    /// Called before the prompt is sent to the LLM.
    ///
    /// Records the start time and caches the serialised input so the
    /// corresponding [`on_completion_response`](PromptHook::on_completion_response)
    /// can compute the round-trip duration.
    async fn on_completion_call(
        &self,
        prompt: &Message,
        _history: &[Message],
    ) -> HookAction {
        let input_value = serde_json::to_value(prompt).unwrap_or(Value::Null);
        let input_str = input_value.to_string();

        // Store timing/input for the response hook.
        if let Ok(mut guard) = self.completion_start.lock() {
            *guard = Some(Instant::now());
        }
        if let Ok(mut guard) = self.last_completion_input.lock() {
            *guard = Some(input_str);
        }

        // Write the initial call record (duration=0; actual timing is on response).
        self.write_record("completion_call", input_value, Value::Null);

        HookAction::cont()
    }

    /// Called after the LLM returns a response.
    ///
    /// Computes the round-trip duration from the start time saved in
    /// [`on_completion_call`](PromptHook::on_completion_call) and writes a
    /// record with the response output.
    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let duration = if let Ok(mut guard) = self.completion_start.lock() {
            guard.take().map(|start| start.elapsed().as_millis() as u64)
        } else {
            None
        }
        .unwrap_or(0);

        // Recover the cached input (best-effort).
        let input_value = if let Ok(mut guard) = self.last_completion_input.lock() {
            guard
                .take()
                .and_then(|s| serde_json::from_str(&s).ok())
        } else {
            None
        }
        .unwrap_or(Value::Null);

        // Build the output JSON from the serializable parts of the response.
        let output_value = serde_json::json!({
            "choice": response.choice,
            "message_id": response.message_id,
            "usage": response.usage,
        });

        self.write_tool_record("completion_response", input_value, output_value, duration);

        HookAction::cont()
    }

    /// Called before a tool is invoked.
    ///
    /// Records the start time and arguments keyed by `internal_call_id` so
    /// [`on_tool_result`](PromptHook::on_tool_result) can pair them up.
    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        if let Ok(mut guard) = self.tool_starts.lock() {
            guard.insert(internal_call_id.to_string(), (Instant::now(), args.to_string()));
        }
        if let Ok(mut guard) = self.tools_called.lock() {
            if !guard.iter().any(|n| n == tool_name) {
                guard.push(tool_name.to_string());
            }
        }

        ToolCallHookAction::cont()
    }

    /// Called after a tool returns its result.
    ///
    /// Looks up the cached start time and input for `internal_call_id`,
    /// computes the duration, and writes the record.
    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        internal_call_id: &str,
        _args: &str,
        result: &str,
    ) -> HookAction {
        let (duration, input_value) = if let Ok(mut guard) = self.tool_starts.lock() {
            match guard.remove(internal_call_id) {
                Some((start, input_str)) => {
                    let dur = start.elapsed().as_millis() as u64;
                    let inp = serde_json::from_str(&input_str).unwrap_or({
                        Value::String(input_str)
                    });
                    (dur, inp)
                }
                None => (0, Value::Null),
            }
        } else {
            (0, Value::Null)
        };

        let output_value = serde_json::from_str(result).unwrap_or_else(|_| {
            Value::String(result.to_string())
        });

        self.write_tool_record(
            &format!("tool_call::{}", tool_name),
            input_value,
            output_value,
            duration,
        );

        HookAction::cont()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── redact_sensitive ─────────────────────────────────────────────

    #[test]
    fn redact_sensitive_replaces_top_level_api_key() {
        let input = json!({"api_key": "sk-1234567890abcdef", "model": "gpt-4"});
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted["api_key"], "***REDACTED***");
        assert_eq!(redacted["model"], "gpt-4");
    }

    #[test]
    fn redact_sensitive_replaces_token_in_nested_object() {
        let input = json!({
            "config": {
                "token": "ghp_aaaaaaaaaaaaaaaaaaaaaa",
                "url": "https://example.com"
            }
        });
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted["config"]["token"], "***REDACTED***");
        assert_eq!(redacted["config"]["url"], "https://example.com");
    }

    #[test]
    fn redact_sensitive_replaces_in_arrays() {
        let input = json!([
            {"api_key": "sk-first"},
            {"api_key": "sk-second", "name": "test"}
        ]);
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted[0]["api_key"], "***REDACTED***");
        assert_eq!(redacted[1]["api_key"], "***REDACTED***");
        assert_eq!(redacted[1]["name"], "test");
    }

    #[test]
    fn redact_sensitive_preserves_unrelated_values() {
        let input = json!({
            "temperature": 0.7,
            "max_tokens": 2048,
            "messages": [{"role": "user", "content": "hello"}]
        });
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted, input);
    }

    #[test]
    fn redact_sensitive_handles_api_key_case_insensitively() {
        // "api-key" is also in the check list, but "API_KEY" should match
        // via to_lowercase on "api_key".
        let input = json!({"API_KEY": "sk-test"});
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted["API_KEY"], "***REDACTED***");
    }

    // ── V26.3 E3: value-based redaction is prefix-only ──────────────────

    #[test]
    fn redact_sensitive_preserves_long_plain_strings() {
        // UUID / task ids / file bodies must no longer be nuked by the old
        // generic 40+ char rule.
        let uuid = "6f95dacd-d5bb-4b33-8d38-b2b1d5c87c24";
        let long_code = "fn verify_everything_across_the_entire_codebase() -> Result<(), TaijiError> { /* 40+ chars of innocent text */ }";
        let input = json!({
            "task_id": uuid,
            "content": long_code,
            "nested": {"body": uuid}
        });
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted["task_id"], uuid);
        assert_eq!(redacted["content"], long_code);
        assert_eq!(redacted["nested"]["body"], uuid);
    }

    #[test]
    fn redact_sensitive_still_redacts_prefixed_keys() {
        let input = json!({
            "openai_key": "sk-abcdefghijklmnopqrstuvwxyz1234567890",
            "deepseek_key": "ds-abcdefghijklmnopqrstuvwxyz1234567890",
            "github_token": "ghp_abcdefghijklmnopqrstuvwxyz123456",
            "aws_key": "AKIAIOSFODNN7EXAMPLE",
            "plain": "short"
        });
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted["openai_key"], "***REDACTED***");
        assert_eq!(redacted["deepseek_key"], "***REDACTED***");
        assert_eq!(redacted["github_token"], "***REDACTED***");
        assert_eq!(redacted["aws_key"], "***REDACTED***");
        assert_eq!(redacted["plain"], "short");
    }

    #[test]
    fn redact_sensitive_sk_prefix_requires_20_chars() {
        // `sk-` with fewer than 20 alphanumerics is not a real key pattern.
        let input = json!({"value": "sk-short"});
        let redacted = TraceHook::redact_sensitive(&input);
        assert_eq!(redacted["value"], "sk-short");
    }

    // ── write_record integration (temp file) ─────────────────────────

    #[test]
    fn write_record_creates_jsonl_entry() {
        let dir = std::env::temp_dir().join(format!("trace_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let context = EngineContext {
            task_id: "test-task-1".into(),
            depth: 0,
            task_dir: dir.clone(),
            cycle: 1,
            round: 0,
            context_dir: None,
        };

        let hook = TraceHook::new(&context, "test-provider/test-model");
        hook.write_record(
            "test_phase",
            json!({"input": "hello"}),
            json!({"output": "world"}),
        );

        // Read back and verify.
        let trace_path = dir.join("trace.jsonl");
        let contents = std::fs::read_to_string(&trace_path)
            .expect("trace file should exist");
        let record: TraceRecord = serde_json::from_str(contents.trim())
            .expect("trace file should contain valid JSON");

        assert_eq!(record.task_id, "test-task-1");
        assert_eq!(record.cycle, 1);
        assert_eq!(record.depth, 0);
        assert_eq!(record.phase, "test_phase");
        assert_eq!(record.provider_model, "test-provider/test-model");
        assert_eq!(record.input["input"], "hello");
        assert_eq!(record.output["output"], "world");

        // Clean up.
        let _ = std::fs::remove_file(&trace_path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn write_record_redacts_sensitive_fields() {
        let dir = std::env::temp_dir().join(format!("trace_redact_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let context = EngineContext {
            task_id: "redact-test".into(),
            depth: 0,
            task_dir: dir.clone(),
            cycle: 0,
            round: 0,
            context_dir: None,
        };

        let hook = TraceHook::new(&context, "test-model");
        hook.write_record(
            "llm_call",
            json!({"api_key": "should-be-redacted", "prompt": "hello"}),
            json!({"token": "ghp_xxx", "response": "hi"}),
        );

        let trace_path = dir.join("trace.jsonl");
        let contents = std::fs::read_to_string(&trace_path)
            .expect("trace file should exist");
        let record: TraceRecord = serde_json::from_str(contents.trim())
            .expect("valid JSON");

        assert_eq!(record.input["api_key"], "***REDACTED***");
        assert_eq!(record.input["prompt"], "hello");
        assert_eq!(record.output["token"], "***REDACTED***");
        assert_eq!(record.output["response"], "hi");

        let _ = std::fs::remove_file(&trace_path);
        let _ = std::fs::remove_dir(&dir);
    }

    // ── V26.5: end-to-end regression for double-redaction ──────────────
    //
    // TraceHook redacts with the prefix-only rule, then TraceWriter::write
    // must NOT re-redact innocent long strings (UUIDs, file bodies) with the
    // old generic {40,} rule — the V26.3 E3 fix used to be nullified by the
    // second pass in infra/trace.rs.

    static TRACE_E2E_SEQ: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn write_record_preserves_long_plain_strings_end_to_end() {
        let seq = TRACE_E2E_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "trace_e2e_test_{}_{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::create_dir_all(&dir);

        let context = EngineContext {
            task_id: "6f95dacd-d5bb-4b33-8d38-b2b1d5c87c24".into(),
            depth: 0,
            task_dir: dir.clone(),
            cycle: 0,
            round: 0,
            context_dir: None,
        };

        let hook = TraceHook::new(&context, "test-model");
        let long_body = "fn verify_everything_across_the_entire_codebase() -> Result<(), TaijiError> { /* 40+ chars of innocent text that must survive */ }";
        hook.write_record(
            "tool_call::read",
            json!({
                "path": "src/lib.rs",
                "task_id": "6f95dacd-d5bb-4b33-8d38-b2b1d5c87c24"
            }),
            json!({"content": long_body}),
        );

        let trace_path = dir.join("trace.jsonl");
        let contents = std::fs::read_to_string(&trace_path)
            .expect("trace file should exist");
        let record: TraceRecord = serde_json::from_str(contents.trim())
            .expect("valid JSON");

        // Innocent long strings survive the full write path.
        assert_eq!(record.input["path"], "src/lib.rs");
        assert_eq!(
            record.input["task_id"],
            "6f95dacd-d5bb-4b33-8d38-b2b1d5c87c24"
        );
        assert_eq!(record.output["content"], long_body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── tools_called (real tool invocation tracking) ───────────────────

    #[tokio::test]
    async fn tools_called_records_real_tool_names_deduplicated() {
        use crate::hooks::test_support::TestCompletionModel;

        let dir = std::env::temp_dir().join(format!("trace_tools_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let context = EngineContext {
            task_id: "tools-test".into(),
            depth: 0,
            task_dir: dir.clone(),
            cycle: 0,
            round: 0,
            context_dir: None,
        };

        let hook = TraceHook::new(&context, "test-model");

        // on_tool_call is async in Rig 0.39's PromptHook — await each call.
        let _ = PromptHook::<TestCompletionModel>::on_tool_call(&hook, "read", None, "call-1", "{}").await;
        let _ = PromptHook::<TestCompletionModel>::on_tool_call(&hook, "read", None, "call-2", "{}").await;
        let _ = PromptHook::<TestCompletionModel>::on_tool_call(&hook, "yin_verify", None, "call-3", "{}").await;
        let _ = PromptHook::<TestCompletionModel>::on_tool_call(&hook, "read", None, "call-4", "{}").await;

        let called = hook.tools_called();
        assert_eq!(
            called,
            vec!["read".to_string(), "yin_verify".to_string()],
            "tools_called should be de-duplicated and first-call ordered"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
