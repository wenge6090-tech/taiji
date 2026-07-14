//! TraceHook — auto-captures StepEvents and writes them to `trace.jsonl`.
//!
//! Implements the rig-core [`PromptHook`] trait to observe completion calls,
//! completion responses, tool calls, and tool results. Each event is recorded
//! as a [`TraceRecord`] with a timestamp, execution context, duration, and
//! redacted input/output payloads.
//!
//! # Sensitive data
//! All payloads are run through [`redact_sensitive`] before writing, which
//! replaces values of keys named `"api_key"` or `"token"` with `"***REDACTED***"`.

use crate::infra::trace::{TraceRecord, TraceWriter};
use crate::types::execution::EngineContext;
use chrono::Utc;
use regex::Regex;
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
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
///    pairing tool-call hooks with their result hooks.
#[derive(Clone)]
pub struct TraceHook {
    writer: Arc<TraceWriter>,
    context: EngineContext,
    provider_model: String,
    completion_start: Arc<Mutex<Option<Instant>>>,
    last_completion_input: Arc<Mutex<Option<String>>>,
    tool_starts: Arc<Mutex<HashMap<String, (Instant, String)>>>,
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
            reasoning_path_ids: None,
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
    /// # Key-based redaction
    /// Keys (case-insensitive) containing any of the following trigger a full
    /// value replacement with `"***REDACTED***"`:
    /// `api_key`, `api-key`, `apikey`, `token`, `secret`, `password`
    ///
    /// # Value-based redaction
    /// All string values are scanned for patterns that match common API key
    /// formats:
    /// - `sk-` followed by 20+ alphanumeric chars (OpenAI-style)
    /// - `ds-` followed by 20+ alphanumeric chars (DeepSeek-style)
    /// - Any 40+ character alphanumeric string (generic key/token)
    ///
    /// The original value is not mutated; a new `Value` tree is returned.
    pub fn redact_sensitive(value: &Value) -> Value {
        static KEY_VALUE_PATTERN: OnceLock<Regex> = OnceLock::new();
        let key_value_re = KEY_VALUE_PATTERN.get_or_init(|| {
            Regex::new(r"(?i)(sk-[a-zA-Z0-9]{20,}|ds-[a-zA-Z0-9]{20,}|[a-zA-Z0-9_-]{40,})")
                .expect("invalid key-value regex")
        });

        match value {
            Value::Object(map) => {
                let mut redacted = serde_json::Map::with_capacity(map.len());
                for (k, v) in map {
                    let key_lower = k.to_lowercase();
                    if key_lower.contains("api_key")
                        || key_lower.contains("api-key")
                        || key_lower.contains("apikey")
                        || key_lower == "token"
                        || key_lower.ends_with("_token")
                        || key_lower.ends_with("-token")
                        || key_lower.contains("secret")
                        || key_lower.contains("password")
                    {
                        redacted.insert(k.clone(), Value::String("***REDACTED***".into()));
                    } else {
                        redacted.insert(k.clone(), Self::redact_sensitive(v));
                    }
                }
                Value::Object(redacted)
            }
            Value::Array(arr) => {
                let redacted: Vec<Value> = arr.iter().map(Self::redact_sensitive).collect();
                Value::Array(redacted)
            }
            Value::String(s) => {
                if key_value_re.is_match(s) {
                    Value::String("***REDACTED***".into())
                } else {
                    Value::String(s.clone())
                }
            }
            other => other.clone(),
        }
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
            reasoning_path_ids: None,
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
        _tool_name: &str,
        _tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        if let Ok(mut guard) = self.tool_starts.lock() {
            guard.insert(internal_call_id.to_string(), (Instant::now(), args.to_string()));
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
                    let inp = serde_json::from_str(&input_str).unwrap_or_else(|_| {
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
}
