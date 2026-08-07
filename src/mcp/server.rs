//! MCP Server — exposes taiji tools (TPN, DMN, cognition assets) over stdio transport.
//!
//! See AGENTS.md §10 for MCP rules.  
//! Uses `rmcp::ServerHandler` (no macros) for broad API compatibility.
//!
//! ## Exposed tools
//! | Tool             | Description                                                          |
//! |------------------|----------------------------------------------------------------------|
//! | `taiji_plan`     | Pre-execution plan: MetaAgent + LLM plan summary, no TPN loop.       |
//! | `taiji_run`      | Execute a task description via `RecursiveRunner`.                    |
//! | `taiji_explain`  | Post-execution report: reasoning tree summary from trace data.       |
//! | `taiji_trace`    | Read trace records for a task.                                       |
//! | `taiji_list`     | List tasks present in the workspace.                                 |
//! | `taiji_status`   | Engine version, workspace path & task count.                         |

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError,
    model::{
        CallToolRequestParam, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParam, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer, serve_server},
    ServerHandler,
};
use tokio::io::{Stdin, Stdout};
use tracing::{error, info, warn};

use crate::agents::factory::AgentFactory;
use crate::infra::error::TaijiError;
use crate::infra::trace::TraceWriter;
use crate::orchestration::runner::RecursiveRunner;
use crate::types::agent::ExternalContext;
use crate::types::execution::{DecisionSummary, ExplainReport, PhaseSummary};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal JSON Schema for a tool with a given set of property schemas.
fn object_schema(properties: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        "type": "object",
        "properties": properties,
    })
    .as_object()
    .expect("static schema is always an object")
    .clone()
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

/// MCP server that exposes Taiji engine capabilities over stdio.
///
/// Holds an `Arc<AgentFactory>` from which it derives:
/// - A `RecursiveRunner` for task execution
/// - The data-root path for trace / task enumeration
/// - The engine configuration
#[derive(Clone)]
pub struct TaijiMcpServer {
    factory: Arc<AgentFactory>,
}

impl TaijiMcpServer {
    /// Create a new server instance.
    pub fn new(factory: Arc<AgentFactory>) -> Self {
        Self { factory }
    }

    /// Serve the MCP protocol over stdio, blocking until the transport closes
    /// or the process receives SIGTERM / Ctrl+C.
    ///
    /// # Errors
    ///
    /// Returns `TaijiError::Other` if the transport fails during initialisation.
    pub async fn serve(&self) -> Result<(), TaijiError> {
        let stdin: Stdin = tokio::io::stdin();
        let stdout: Stdout = tokio::io::stdout();

        info!("Taiji MCP server starting on stdio");

        let _service = serve_server(self.clone(), (stdin, stdout))
            .await
            .map_err(|e| {
                TaijiError::Other(format!("MCP server initialization failed: {e}"))
            })?;

        // The service runs until stdin is closed or explicitly cancelled.
        // `pending()` keeps the async task alive without busy-waiting.
        std::future::pending::<()>().await;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation  (rmcp 0.8 trait, no macros)
// ---------------------------------------------------------------------------

/// Dispatches incoming MCP requests.  Only `call_tool` and `list_tools` are
/// overridden; everything else uses the default (no-op) trait implementation.
#[allow(unused_variables)]
impl ServerHandler for TaijiMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: Default::default(),
            server_info: Implementation {
                name: "taiji-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: None,
        }
    }

    /// Advertise the six taiji tools.
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = vec![
            Tool::new(
                "taiji_plan",
                "Pre-execution plan: run MetaAgent (权重更新) and LLM-compose a structured execution plan (PlanSummary). Does NOT enter the TPN loop.",
                Arc::new(object_schema(serde_json::json!({
                    "description": {
                        "type": "string",
                        "description": "Natural-language task description to plan for"
                    }
                }))),
            ),
            Tool::new(
                "taiji_run",
                "Execute a task via the TPN cognitive engine (MetaAgent → FittingAgent → CausalAgent).",
                Arc::new(object_schema(serde_json::json!({
                    "description": {
                        "type": "string",
                        "description": "Natural-language task description"
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Optional override for max recursion depth (default: from config, currently 2)"
                    },
                    "context": {
                        "type": "object",
                        "description": "Optional external context from the calling agent (e.g. files, tool results)",
                        "properties": {
                            "files": {
                                "type": "array",
                                "description": "Files the calling agent has already read",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "path": {"type": "string", "description": "Original file path"},
                                        "content": {"type": "string", "description": "Full text content"}
                                    }
                                }
                            },
                            "tool_results": {
                                "type": "array",
                                "description": "Results of tool calls the calling agent has executed",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "tool": {"type": "string", "description": "Tool name"},
                                        "output": {"type": "string", "description": "Tool output"}
                                    }
                                }
                            },
                            "session_summary": {
                                "type": "string",
                                "description": "Summary of the conversation or session history"
                            }
                        }
                    }
                }))),
            ),
            Tool::new(
                "taiji_explain",
                "Post-execution report: read trace.jsonl + meta.json + deliverables/ to produce a human-readable reasoning-tree summary (ExplainReport).",
                Arc::new(object_schema(serde_json::json!({
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to explain"
                    }
                }))),
            ),
            Tool::new(
                "taiji_trace",
                "Read trace records for a task. Supports recursive tree merge and tail.",
                Arc::new(object_schema(serde_json::json!({
                    "task_id": {
                        "type": "string",
                        "description": "Task ID"
                    },
                    "tree": {
                        "type": "boolean",
                        "description": "Recursively merge nested traces",
                        "default": false
                    },
                    "tail": {
                        "type": "integer",
                        "description": "Return only the last N records",
                        "default": null
                    }
                }))),
            ),
            Tool::new(
                "taiji_list",
                "List all tasks known to the engine.",
                Arc::new(serde_json::Map::new()),
            ),
            Tool::new(
                "taiji_status",
                "Return engine version, workspace path, and task count.",
                Arc::new(serde_json::Map::new()),
            ),
        ];

        Ok(ListToolsResult::with_all_items(tools))
    }

    /// Route tool invocations to the appropriate handler.
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let name = request.name.as_ref();
        let args = request.arguments.unwrap_or_default();

        match name {
            "taiji_plan" => self.handle_plan(args).await,
            "taiji_run" => self.handle_run(args).await,
            "taiji_explain" => self.handle_explain(args).await,
            "taiji_trace" => self.handle_trace(args).await,
            "taiji_list" => self.handle_list().await,
            "taiji_status" => self.handle_status().await,
            other => {
                warn!(tool = %other, "Unknown tool requested");
                Err(McpError::method_not_found::<rmcp::model::CallToolRequestMethod>())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool handler implementations
// ---------------------------------------------------------------------------

impl TaijiMcpServer {
    /// `taiji_plan` — pre-execution plan via MetaAgent + LLM plan composition.
    async fn handle_plan(
        &self,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        let description = match args.get("description").and_then(|v| v.as_str()) {
            Some(d) => d.to_owned(),
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "Missing required argument: description",
                )]));
            }
        };

        info!(description = %description, "MCP taiji_plan called");

        // V26.6: readable id for plan context (not persisted as a task dir).
        let task_id = crate::infra::task_id::generate_task_id(&description);
        let plan_agent = match self.factory.create_plan_agent(&task_id) {
            Ok(agent) => agent,
            Err(e) => {
                error!(error = %e, "taiji_plan: failed to create PlanBuilder");
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to create plan agent: {e}"
                ))]));
            }
        };

        match plan_agent.plan(&description, &["general"]).await {
            Ok(summary) => {
                let payload = serde_json::to_value(&summary).unwrap_or_else(|e| {
                    serde_json::json!({"error": format!("Serialization failed: {e}")})
                });
                Ok(CallToolResult::structured(payload))
            }
            Err(e) => {
                error!(error = %e, "taiji_plan failed");
                Ok(CallToolResult::error(vec![Content::text(format!(
                    "Planning failed: {e}"
                ))]))
            }
        }
    }

    /// `taiji_run` —  execute a task via RecursiveRunner and return the result.
    async fn handle_run(
        &self,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        let description = match args.get("description").and_then(|v| v.as_str()) {
            Some(d) => d.to_owned(),
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "Missing required argument: description",
                )]));
            }
        };

        info!(description = %description, "MCP taiji_run called");

        // Optional max_depth override from caller
        let max_depth_override: Option<u32> = args
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        // Clone config and apply override if provided
        let mut config = self.factory.config.clone();
        if let Some(depth) = max_depth_override {
            info!(max_depth = depth, "Overriding max_depth from MCP call");
            config.runtime.max_depth = depth;
        }

        // Parse optional external context from frontend agent
        let external_ctx = args
            .get("context")
            .and_then(|v| serde_json::from_value::<ExternalContext>(v.clone()).ok());

        if let Some(ref ctx) = external_ctx {
            info!(
                files = ctx.files.len(),
                tool_results = ctx.tool_results.len(),
                has_session_summary = ctx.session_summary.is_some(),
                "External context provided"
            );
        }

        let runner = RecursiveRunner::new(
            self.factory.clone(),
            config,
        );

        match runner.execute_with_context(&description, external_ctx, None).await {
            Ok(result) => {
                let payload = serde_json::json!({
                    "task_id": result.task_id,
                    "content": result.content,
                    "tools_used": result.tools_used,
                    "deliverables": result.deliverables,
                    "depth": result.depth,
                    "rounds": result.rounds,
                });
                Ok(CallToolResult::structured(payload))
            }
            Err(e) => {
                error!(error = %e, "taiji_run failed");
                Ok(CallToolResult::error(vec![Content::text(format!(
                    "Task execution failed: {e}"
                ))]))
            }
        }
    }

    /// `taiji_trace` — read trace records for a task.
    async fn handle_trace(
        &self,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        let task_id = match args.get("task_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_owned(),
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "Missing required argument: task_id",
                )]));
            }
        };

        let tree = args.get("tree").and_then(|v| v.as_bool()).unwrap_or(false);
        let tail = args.get("tail").and_then(|v| v.as_u64());

        info!(task_id = %task_id, tree, ?tail, "MCP taiji_trace called");

        let task_dir = self.factory.task_dir(&task_id);

        if !task_dir.exists() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Task not found: {task_id}"
            ))]));
        }

        let records = if tree {
            crate::infra::trace::TraceWriter::read_tree(&task_dir)
        } else {
            let writer = crate::infra::trace::TraceWriter::new(&task_dir);
            writer.read()
        };

        match records {
            Ok(mut recs) => {
                if let Some(n) = tail {
                    let n = n as usize;
                    if n < recs.len() {
                        recs = recs.split_off(recs.len() - n);
                    }
                }

                let payload = serde_json::json!({
                    "task_id": task_id,
                    "record_count": recs.len(),
                    "records": recs,
                });
                Ok(CallToolResult::structured(payload))
            }
            Err(e) => {
                error!(error = %e, "taiji_trace read failed");
                Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to read trace: {e}"
                ))]))
            }
        }
    }

    /// `taiji_list` — enumerate tasks.
    async fn handle_list(
        &self,
    ) -> Result<CallToolResult, McpError> {
        info!("MCP taiji_list called");

        let tasks_dir = self.factory.data_root.join("tasks");
        let mut tasks: Vec<serde_json::Value> = Vec::new();

        if tasks_dir.exists()
            && let Ok(mut entries) = tokio::fs::read_dir(&tasks_dir).await {
                loop {
                    match entries.next_entry().await {
                        Ok(Some(entry)) => {
                            let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
                            if is_dir {
                                let name = entry.file_name().to_string_lossy().to_string();
                                let meta_path = entry.path().join("meta.json");
                                let meta = tokio::fs::read_to_string(&meta_path)
                                    .await
                                    .ok()
                                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

                                tasks.push(serde_json::json!({
                                    "id": name,
                                    "meta": meta,
                                }));
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            }

        Ok(CallToolResult::structured(serde_json::json!({
            "count": tasks.len(),
            "tasks": tasks,
        })))
    }

    /// `taiji_status` — engine health.
    async fn handle_status(
        &self,
    ) -> Result<CallToolResult, McpError> {
        info!("MCP taiji_status called");

        let tasks_dir = self.factory.data_root.join("tasks");
        let task_count = if tasks_dir.exists() {
            let mut count = 0usize;
            if let Ok(mut entries) = tokio::fs::read_dir(&tasks_dir).await {
                while let Ok(Some(_)) = entries.next_entry().await {
                    count += 1;
                }
            }
            count
        } else {
            0
        };

        let payload = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "workspace": self.factory.data_root.to_string_lossy(),
            "task_count": task_count,
            "status": "running",
        });

        Ok(CallToolResult::structured(payload))
    }

    /// `taiji_explain` — post-execution reasoning tree summary.
    async fn handle_explain(
        &self,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, McpError> {
        let task_id = match args.get("task_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_owned(),
            None => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "Missing required argument: task_id",
                )]));
            }
        };

        info!(task_id = %task_id, "MCP taiji_explain called");

        let task_dir = self.factory.task_dir(&task_id);
        if !task_dir.exists() {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Task not found: {task_id}"
            ))]));
        }

        // Read meta.json for description + status
        let (description, status) = read_task_meta(&task_dir).await;

        // Read trace records recursively
        let trace_records = TraceWriter::read_tree(&task_dir).ok();

        // Collect deliverables recursively
        let deliverables = collect_deliverables(&task_dir).await;

        // Build ExplainReport
        let report = build_explain_report(&task_id, &description, &status, trace_records, deliverables);

        let payload = serde_json::to_value(&report).unwrap_or_else(|e| {
            serde_json::json!({"error": format!("Serialization failed: {e}")})
        });

        Ok(CallToolResult::structured(payload))
    }
}

// ---------------------------------------------------------------------------
// Explain helpers
// ---------------------------------------------------------------------------

/// Read task meta.json for description and status.
async fn read_task_meta(task_dir: &std::path::Path) -> (String, String) {
    let meta_path = task_dir.join("meta.json");
    match tokio::fs::read_to_string(&meta_path).await {
        Ok(content) => {
            let v: serde_json::Value = serde_json::from_str(&content).unwrap_or_default();
            let description = v
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .to_string();
            let status = v
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            (description, status)
        }
        Err(_) => (String::new(), "unknown".to_string()),
    }
}

/// Recursively collect deliverable file paths from a task directory.
/// Uses an iterative worklist (Vec stack) to avoid async recursion.
async fn collect_deliverables(task_dir: &std::path::Path) -> Vec<String> {
    let mut paths = Vec::new();
    let mut stack = vec![task_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        // Collect from deliverables/ at this level
        let deliverables_dir = dir.join("deliverables");
        if deliverables_dir.exists() {
            collect_files(&deliverables_dir, &mut paths).await;
        }

        // Push children/ subdirectories onto the stack.
        // Note: not checking children_dir.exists() first — tokio::fs::read_dir
        // handles non-existent directories gracefully (returns Err), and
        // removing the redundant exists() check avoids a clippy collapsible_if
        // lint and a TOCTOU race condition.
        let children_dir = dir.join("children");
        if let Ok(mut entries) = tokio::fs::read_dir(&children_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false) {
                    stack.push(entry.path());
                }
            }
        }
    }

    paths
}

/// List all files in a directory (non-recursive), returning absolute paths.
async fn collect_files(dir: &std::path::Path, out: &mut Vec<String>) {
    if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_type().await.map(|ft| ft.is_file()).unwrap_or(false) {
                out.push(entry.path().to_string_lossy().to_string());
            }
        }
    }
}

/// Build an [`ExplainReport`] from trace records and task metadata.
fn build_explain_report(
    task_id: &str,
    description: &str,
    status: &str,
    trace_records: Option<Vec<crate::infra::trace::TraceRecord>>,
    deliverables: Vec<String>,
) -> ExplainReport {
    let mut max_depth = 0u32;
    let mut max_cycle = 0u32;
    let mut timeline: Vec<PhaseSummary> = Vec::new();
    let mut decisions: Vec<DecisionSummary> = Vec::new();
    let mut total_duration_ms = 0u64;

    if let Some(records) = trace_records {
        if records.is_empty() {
            // No trace records → assume running or just created
            return ExplainReport {
                task_id: task_id.to_string(),
                description: description.to_string(),
                status: "running".to_string(),
                total_cycles: 0,
                total_rounds: 0,
                total_depth: 0,
                total_duration_ms: 0,
                timeline: vec![],
                decisions: vec![],
                final_deliverables: deliverables,
                summary: "任务没有执行记录（可能刚创建或仍在运行）。".into(),
            };
        }

        // Sort by timestamp
        let mut sorted = records.clone();
        sorted.sort_by(|a, b| a.ts.cmp(&b.ts));

        // Calculate total duration from first to last record
        if let (Ok(first), Ok(last)) = (
            chrono::DateTime::parse_from_rfc3339(&sorted[0].ts),
            chrono::DateTime::parse_from_rfc3339(&sorted[sorted.len() - 1].ts),
        ) {
            total_duration_ms = (last.timestamp_millis() - first.timestamp_millis()) as u64;
        } else {
            total_duration_ms = sorted.iter().map(|r| r.duration_ms).sum();
        }

        // Group by cycle for timeline phases
        // Track tool calls per cycle to identify probability fitting phases
        let mut prev_cycle = 0u32;
        let mut current_tools: Vec<String> = Vec::new();
        let mut current_duration = 0u64;

        for record in &sorted {
            if record.cycle != prev_cycle {
                // Cycle boundary → finalize previous phase
                if current_duration > 0 {
                    timeline.push(PhaseSummary {
                        phase: "概率拟合".into(),
                        cycle: prev_cycle,
                        round: 0,
                        depth: record.depth,
                        duration_ms: current_duration,
                        tools_used: std::mem::take(&mut current_tools),
                        key_output: String::new(),
                    });
                }
                current_duration = 0;

                // Detect BACK_TO_META decision (cycle boundary)
                decisions.push(DecisionSummary {
                    cycle: prev_cycle,
                    round: 0,
                    verdict: "BACK_TO_META".into(),
                    reason: "TPN 循环检测到认知偏差，重新运行权重更新（元）。".into(),
                    constraint_violations: vec![],
                });

                prev_cycle = record.cycle;
            }

            // Track tool calls
            if record.phase.starts_with("tool_call::") {
                let tool_name = record.phase.strip_prefix("tool_call::").unwrap_or("");
                if !current_tools.contains(&tool_name.to_string()) {
                    current_tools.push(tool_name.to_string());
                }
            }

            current_duration += record.duration_ms;

            // Track max values
            if record.cycle > max_cycle {
                max_cycle = record.cycle;
            }
            if record.depth > max_depth {
                max_depth = record.depth;
            }
        }

        // Final phase
        if current_duration > 0 {
            timeline.push(PhaseSummary {
                phase: "概率拟合".into(),
                cycle: prev_cycle,
                round: 0,
                depth: max_depth,
                duration_ms: current_duration,
                tools_used: current_tools,
                key_output: String::new(),
            });
        }

        // Final decision based on status
        match status {
            "Completed" => {
                decisions.push(DecisionSummary {
                    cycle: prev_cycle,
                    round: 0,
                    verdict: "PASS".into(),
                    reason: "因果验证通过，任务收敛。".into(),
                    constraint_violations: vec![],
                });
            }
            "Failed" | "Cancelled" => {
                decisions.push(DecisionSummary {
                    cycle: prev_cycle,
                    round: 0,
                    verdict: "FAIL".into(),
                    reason: format!("任务最终状态：{status}"),
                    constraint_violations: vec![],
                });
            }
            _ => {}
        }
    }

    // Build human-readable summary
    let total_seconds = if total_duration_ms > 0 { total_duration_ms / 1000 } else { 0 };
    let total_cycles = max_cycle + 1;
    let total_depth = max_depth + 1;

    let summary = if status == "Completed" {
        format!(
            "任务已完成。共经历 {} 个 TPN 周期、{} 层递归深度，耗时 {} 秒。生成 {} 个交付产物。",
            total_cycles, total_depth, total_seconds, deliverables.len(),
        )
    } else if status == "Failed" || status == "Cancelled" {
        format!(
            "任务失败（{status}）。共经历 {} 个 TPN 周期、{} 层递归深度，耗时 {} 秒。",
            total_cycles, total_depth, total_seconds,
        )
    } else {
        format!(
            "任务当前状态：{status}。已进行 {} 个 TPN 周期、{} 层递归深度，耗时 {} 秒。",
            total_cycles, total_depth, total_seconds,
        )
    };

    ExplainReport {
        task_id: task_id.to_string(),
        description: description.to_string(),
        status: status.to_string(),
        total_cycles,
        total_rounds: 0,
        total_depth,
        total_duration_ms,
        timeline,
        decisions,
        final_deliverables: deliverables,
        summary,
    }
}
