use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB
const MAX_GENERATIONS: u32 = 5;

/// TPN execution trace writer (JSONL format, 10MB rotation).
pub struct TraceWriter {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    pub ts: String,
    pub cycle: u32,
    pub depth: u32,
    pub task_id: String,
    /// 权重更新 | 概率拟合·turn | 工具调用 | 因果验证 | 收敛判定
    pub phase: String,
    pub provider_model: String,
    pub duration_ms: u64,
    pub input: Value,
    pub output: Value,
    pub degraded: bool,
    pub constraint_violations: Option<Vec<String>>,
}

impl TraceWriter {
    /// Create a new trace writer for a task directory.
    pub fn new(task_dir: &Path) -> Self {
        Self {
            path: task_dir.join("trace.jsonl"),
        }
    }

    /// Write a single trace record, with sensitive fields redacted.
    pub fn write(&self, record: &TraceRecord) -> std::io::Result<()> {
        // Redact sensitive fields before writing.
        let redacted = TraceRecord {
            input: Self::redact_sensitive(&record.input),
            output: Self::redact_sensitive(&record.output),
            ..record.clone()
        };

        // Rotate if file exceeds max size.
        if self.path.exists() {
            let meta = fs::metadata(&self.path)?;
            if meta.len() >= MAX_FILE_SIZE {
                self.rotate()?;
            }
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        let mut writer = BufWriter::new(&mut file);
        serde_json::to_writer(&mut writer, &redacted)?;
        writer.write_all(b"\n")?;

        Ok(())
    }

    /// Recursively walk a JSON value and redact sensitive information.
    ///
    /// # Key-based redaction
    /// Keys (case-insensitive) containing any of the following trigger a full
    /// value replacement with `"***REDACTED***"`:
    /// `api_key`, `api-key`, `apikey`, `token`, `secret`, `password`
    ///
    /// # Value-based redaction
    /// All string values are scanned for patterns that match common prefixed
    /// key formats (V26.3 E3: prefix-only — the generic 40+ char rule was
    /// removed because it nuked long-but-innocent strings like UUIDs, task ids
    /// and file bodies, hiding the content the LLM reads):
    /// - `sk-` followed by 20+ alphanumeric chars (OpenAI-style)
    /// - `ds-` followed by 20+ alphanumeric chars (DeepSeek-style)
    /// - `ghp_` followed by 20+ alphanumeric chars (GitHub PAT)
    /// - `AKIA` + 16 uppercase alphanumerics (AWS access key id)
    pub fn redact_sensitive(value: &Value) -> Value {
        static KEY_VALUE_PATTERN: OnceLock<Regex> = OnceLock::new();
        let key_value_re = KEY_VALUE_PATTERN.get_or_init(|| {
            Regex::new(
                r"(?i)(sk-[a-zA-Z0-9]{20,}|ds-[a-zA-Z0-9]{20,}|ghp_[a-zA-Z0-9]{20,}|AKIA[0-9A-Z]{16})",
            )
            .expect("invalid key-value regex")
        });

        match value {
            Value::Object(map) => {
                let mut new_map = serde_json::Map::with_capacity(map.len());
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
                        new_map.insert(k.clone(), Value::String("***REDACTED***".into()));
                    } else {
                        new_map.insert(k.clone(), Self::redact_sensitive(v));
                    }
                }
                Value::Object(new_map)
            }
            Value::Array(arr) => {
                Value::Array(arr.iter().map(Self::redact_sensitive).collect())
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

    /// Rotate trace files: trace.1.jsonl → trace.2.jsonl → ... → trace.5.jsonl.
    fn rotate(&self) -> std::io::Result<()> {
        // Remove oldest generation.
        let oldest = self.path.with_file_name(format!("trace.{}.jsonl", MAX_GENERATIONS));
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }

        // Shift generations down.
        for generation in (1..MAX_GENERATIONS).rev() {
            let from = self.path.with_file_name(format!("trace.{}.jsonl", generation));
            let to = self.path.with_file_name(format!("trace.{}.jsonl", generation + 1));
            if from.exists() {
                fs::rename(&from, &to)?;
            }
        }

        // Rename current to trace.1.jsonl.
        let first = self.path.with_file_name("trace.1.jsonl");
        fs::rename(&self.path, &first)?;

        Ok(())
    }

    /// Read a single task's trace file.
    pub fn read(&self) -> std::io::Result<Vec<TraceRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        let records: Vec<TraceRecord> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Ok(records)
    }

    /// Recursively merge all trace.jsonl files under a task directory tree.
    pub fn read_tree(root: &Path) -> std::io::Result<Vec<TraceRecord>> {
        let mut records = Vec::new();
        Self::collect_traces(root, &mut records)?;
        records.sort_by(|a, b| a.ts.cmp(&b.ts));
        Ok(records)
    }

    fn collect_traces(dir: &Path, records: &mut Vec<TraceRecord>) -> std::io::Result<()> {
        let trace_file = dir.join("trace.jsonl");
        if trace_file.exists() {
            let content = fs::read_to_string(&trace_file)?;
            for line in content.lines() {
                if let Ok(record) = serde_json::from_str::<TraceRecord>(line) {
                    records.push(record);
                }
            }
        }

        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    Self::collect_traces(&entry.path(), records)?;
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Persistence helpers — atomic JSON save + graceful load
// Used by TpnCycle, FittingAgent, CausalAgent, MetaAgent for checkpoint
// and conversation-history persistence.
// ---------------------------------------------------------------------------

/// Atomically write a serializable value to a JSON file using
/// temp-file + rename.  If the write is interrupted (crash, SIGKILL),
/// the target file is never partially written — only the temp file is.
pub fn save_json_atomic<T: Serialize>(value: &T, path: &Path) -> Result<(), std::io::Error> {
    let tmp = path.with_extension("tmp");
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    {
        let file = std::fs::File::create(&tmp)?;
        serde_json::to_writer(file, value)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Load a JSON file and deserialize it.  Returns `Ok(Some(T))` on success,
/// `Ok(None)` if the file does not exist, or `Err(io_error)` on I/O failure.
/// Malformed JSON also returns `Ok(None)` with a logged warning — callers
/// should treat this as "file unavailable" and degrade gracefully.
pub fn load_json_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, std::io::Error> {
    if !path.exists() {
        return Ok(None);
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    match serde_json::from_str(&content) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to parse JSON file, treating as unavailable"
            );
            Ok(None)
        }
    }
}
