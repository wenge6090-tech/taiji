//! MCP Server — exposes taiji tools (TPN, DMN, cognition assets) over stdio transport.
//!
//! See AGENTS.md §10 for MCP rules.  
//! Uses `rmcp::ServerHandler` (no macros) for broad API compatibility.
//!
//! ## Exposed tools
//! | Tool           | Description                                      |
//! |----------------|--------------------------------------------------|
//! | `taiji_run`    | Execute a task description via `RecursiveRunner`. |
//! | `taiji_trace`  | Read trace records for a task.                    |
//! | `taiji_list`   | List tasks present in the workspace.              |
//! | `taiji_status` | Engine version, workspace path & task count.      |

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
use crate::orchestration::runner::RecursiveRunner;

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

    /// Advertise the four taiji tools.
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = vec![
            Tool::new(
                "taiji_run",
                "Execute a task via the TPN engine (MetaAgent → FittingAgent → CausalAgent).",
                Arc::new(object_schema(serde_json::json!({
                    "description": {
                        "type": "string",
                        "description": "Natural-language task description"
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
            "taiji_run" => self.handle_run(args).await,
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

        let runner = RecursiveRunner::new(
            self.factory.clone(),
            self.factory.config.clone(),
        );

        match runner.execute(&description).await {
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

        if tasks_dir.exists() {
            if let Ok(mut entries) = tokio::fs::read_dir(&tasks_dir).await {
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
}
