//! MCP Client Manager — connects to external MCP servers, wraps their tools
//! for injection into YangAgent (概率拟合·阳).
//!
//! See AGENTS.md §10 for MCP rules.
//!
//! # Architecture
//!
//! [`McpClientManager`] holds one [`McpClientConnection`] per configured server.
//! Each connection spawns a child process (e.g. `uvx mcp-server-git`) and speaks
//! the MCP protocol over stdio via `rmcp`.  The manager provides:
//!
//! - `connect_all` — batch-connect to all configured servers
//! - `disconnect_all` — graceful shutdown
//! - `call_tool` — invoke a tool on a specific server by name
//!
//! The `connected_servers()` method returns the list of server names whose
//! connections are currently alive.  Non-trusted servers have their tool
//! invocations subject to [`SafetyHook`] checks (see AGENTS.md §3).

use rmcp::{
    model::{CallToolRequestParam, CallToolResult},
    service::{RoleClient, RunningService, ServiceExt},
    transport::TokioChildProcess,
};
use tokio::process::Command;
use tracing::{error, info, warn};

use std::collections::HashMap;
use std::time::Duration;

use crate::infra::config::McpServerConfig;
use crate::infra::error::TaijiError;

// ---------------------------------------------------------------------------
// Connection state
// ---------------------------------------------------------------------------

/// A single connection to an external MCP server.
///
/// The `client` field holds a `RunningService<RoleClient, ()>` that drives
/// bidirectional JSON-RPC communication over the child's stdio.  We discard
/// the handler parameter because `()` implements [`rmcp::ClientHandler`] with
/// no custom behaviour — we only need the outbound `Peer` half for tool calls.
pub struct McpClientConnection {
    /// Human-readable server name (from config).
    pub name: String,
    /// Executable path (e.g. `uvx`, `npx`, or a local binary path).
    pub command: String,
    /// Command-line arguments.
    pub args: Vec<String>,
    /// Whether this server is in the trusted list (bypasses SafetyHook).
    pub trusted: bool,
    /// Connection timeout in seconds.
    pub timeout: u64,
    /// Extra environment variables to set for the child process.
    pub env: HashMap<String, String>,
    /// Active service handle; `None` when disconnected.
    pub client: Option<RunningService<RoleClient, ()>>,
}

impl std::fmt::Debug for McpClientConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClientConnection")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("trusted", &self.trusted)
            .field("timeout", &self.timeout)
            .field("env", &self.env)
            .field("client", &self.client.is_some())
            .finish()
    }
}

impl McpClientConnection {
    /// Build a connection descriptor from configuration.
    pub fn from_config(cfg: &McpServerConfig) -> Self {
        Self {
            name: cfg.name.clone(),
            command: cfg.command.clone(),
            args: cfg.args.clone(),
            trusted: cfg.trusted,
            timeout: cfg.timeout,
            env: cfg.env.clone(),
            client: None,
        }
    }

    /// Spawn the child process and establish the MCP session.
    ///
    /// 1. Build a `tokio::process::Command` from `self.command` + `self.args`.
    /// 2. Create a `TokioChildProcess` transport (piped stdio).
    /// 3. Call `().serve(transport)` to handshake and obtain a `RunningService`.
    ///
    /// # Errors
    ///
    /// Returns `TaijiError::Other` if the process cannot be spawned or the
    /// MCP initialisation handshake fails.
    pub async fn connect(&mut self) -> Result<(), TaijiError> {
        info!(name = %self.name, command = %self.command, "MCP connecting");

        let mut cmd = Command::new(&self.command);
        for arg in &self.args {
            cmd.arg(arg);
        }
        cmd.envs(&self.env);

        let transport = TokioChildProcess::new(cmd).map_err(|e| {
            TaijiError::Other(format!(
                "Failed to spawn MCP server '{}': {e}",
                self.name
            ))
        })?;

        let service = tokio::time::timeout(
            Duration::from_secs(self.timeout),
            ().serve(transport),
        )
        .await
        .map_err(|_| {
            TaijiError::Other(format!(
                "MCP connection timed out for '{}' after {}s",
                self.name, self.timeout
            ))
        })?
        .map_err(|e| {
            TaijiError::Other(format!(
                "MCP handshake failed for '{}': {e}",
                self.name
            ))
        })?;

        info!(name = %self.name, "MCP connected successfully");
        self.client = Some(service);
        Ok(())
    }

    /// Gracefully shut down the child process and drop the service handle.
    pub async fn disconnect(&mut self) {
        if let Some(service) = self.client.take() {
            info!(name = %self.name, "MCP disconnecting");
            if let Err(e) = service.cancel().await {
                warn!(name = %self.name, error = %e, "Error during MCP disconnect");
            }
        }
    }

    /// Whether the connection is currently alive.
    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    /// List tools advertised by the remote server.
    ///
    /// Requires an active connection.  Returns an empty vector on error.
    pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>, TaijiError> {
        let service = self.client.as_ref().ok_or_else(|| {
            TaijiError::Other(format!("MCP server '{}' is not connected", self.name))
        })?;

        service.list_all_tools().await.map_err(|e| {
            TaijiError::Other(format!(
                "Failed to list tools on '{}': {e}",
                self.name
            ))
        })
    }

    /// Call a tool on the remote server with the given JSON arguments.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, TaijiError> {
        let service = self.client.as_ref().ok_or_else(|| {
            TaijiError::Other(format!("MCP server '{}' is not connected", self.name))
        })?;

        let args_obj = arguments.as_object().cloned().unwrap_or_default();

        service
            .call_tool(CallToolRequestParam {
                name: tool_name.to_string().into(),
                arguments: Some(args_obj),
            })
            .await
            .map_err(|e| {
                TaijiError::Other(format!(
                    "Tool call '{tool_name}' on '{}' failed: {e}",
                    self.name
                ))
            })
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// Manages a collection of external MCP server connections.
///
/// Created from a slice of [`McpServerConfig`] (typically loaded from
/// `TaijiConfig::mcp_servers`).  Call [`connect_all`](Self::connect_all) to
/// establish all connections and [`disconnect_all`](Self::disconnect_all) to
/// tear them down.
pub struct McpClientManager {
    /// Active connections.
    pub servers: Vec<McpClientConnection>,
}

impl std::fmt::Debug for McpClientManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClientManager")
            .field("servers", &self.servers)
            .finish()
    }
}

impl McpClientManager {
    /// Build a manager from configuration slices.
    ///
    /// No connections are established yet — call [`connect_all`](Self::connect_all)
    /// to spawn the subprocesses.
    pub fn new(servers: &[McpServerConfig]) -> Self {
        let connections: Vec<McpClientConnection> = servers
            .iter()
            .map(McpClientConnection::from_config)
            .collect();

        info!(count = connections.len(), "McpClientManager initialised");
        Self { servers: connections }
    }

    /// Connect to every configured MCP server.
    ///
    /// Iterates through the server list and attempts to spawn each child
    /// process and complete the MCP handshake.  Failures are **collected**
    /// rather than short-circuiting so that a single flaky server does not
    /// prevent others from connecting.
    ///
    /// Returns a vector of errors for connections that failed.
    pub async fn connect_all(&mut self) -> Vec<TaijiError> {
        let mut errors = Vec::new();

        for conn in &mut self.servers {
            if let Err(e) = conn.connect().await {
                error!(name = %conn.name, error = %e, "MCP connection failed");
                errors.push(e);
            }
        }

        let ok_count = self.servers.iter().filter(|s| s.is_connected()).count();
        info!(
            connected = ok_count,
            failed = errors.len(),
            total = self.servers.len(),
            "MCP connect_all completed"
        );

        errors
    }

    /// Gracefully shut down all connections.
    pub async fn disconnect_all(&mut self) {
        for conn in &mut self.servers {
            conn.disconnect().await;
        }
        info!("All MCP connections closed");
    }

    /// Return the names of servers that are currently connected.
    pub fn connected_servers(&self) -> Vec<&str> {
        self.servers
            .iter()
            .filter(|s| s.is_connected())
            .map(|s| s.name.as_str())
            .collect()
    }

    /// Call a tool on a specific server by name.
    ///
    /// # Errors
    ///
    /// - `TaijiError::Other` if the server name is not found or the connection
    ///   is not alive.
    /// - Propagates the underlying MCP transport / protocol error.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, TaijiError> {
        let conn = self
            .servers
            .iter()
            .find(|s| s.name == server_name)
            .ok_or_else(|| {
                TaijiError::Other(format!("No MCP server configured with name '{server_name}'"))
            })?;

        if !conn.is_connected() {
            return Err(TaijiError::Other(format!(
                "MCP server '{server_name}' is not connected"
            )));
        }

        let result = conn.call_tool(tool_name, arguments).await?;

        // Prefer structured_content; fall back to text content.
        if let Some(structured) = result.structured_content {
            Ok(structured)
        } else if let Some(text) = result.content.first().and_then(|c| c.as_text()) {
            Ok(serde_json::Value::String(text.text.clone()))
        } else {
            Ok(serde_json::Value::Null)
        }
    }

    /// List all tools from a specific server by name.
    ///
    /// Returns `None` if the server is not found or not connected.
    pub async fn list_server_tools(
        &self,
        server_name: &str,
    ) -> Option<Vec<rmcp::model::Tool>> {
        let conn = self.servers.iter().find(|s| s.name == server_name)?;
        conn.list_tools().await.ok()
    }
}
