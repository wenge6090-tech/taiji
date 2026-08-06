//! WebSocket protocol types (L6) — the taiji-web React frontend bridge.
//!
//! Two channels share a single WebSocket connection (ws://127.0.0.1:17890):
//!
//! - **Server → client broadcast**: [`TaskEvent`], tagged `type` + `data`,
//!   pushed by the engine via the global event bus.
//! - **Client → server request / server → client response**:
//!   [`ClientMessage`] → [`ServerResponse`], correlated by `requestId`.
//!
//! The TypeScript frontend discriminates response frames from event frames
//! with a single check: `"requestId" in msg`.

use serde::{Deserialize, Serialize};

use crate::types::frontend::{EvolutionSummary, NodeStatus, TpnPhase};

/// Event pushed from the engine to all connected WebSocket clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all_fields = "camelCase")]
pub enum TaskEvent {
    /// A new task directory was created (root or child).
    TaskCreated {
        task_id: String,
        description: String,
        parent_id: Option<String>,
        depth: u32,
    },
    /// A task node's status changed (e.g. Running → Converged).
    TaskStatusChanged {
        task_id: String,
        old_status: NodeStatus,
        new_status: NodeStatus,
    },
    /// A task node entered a new TPN phase (Meta/Fitting/Causal).
    PhaseChanged {
        task_id: String,
        phase: TpnPhase,
    },
    /// The parent FittingAgent spawned a child subtask.
    ChildSpawned {
        parent_task_id: String,
        child_task_id: String,
        description: String,
        depth: u32,
    },
    /// A child subtask finished its TPN cycle.
    ChildCompleted {
        child_task_id: String,
        status: NodeStatus,
        deliverables: Vec<String>,
        rounds: u32,
    },
    /// CausalAgent issued a route decision for a node.
    TpnRouteDecision {
        task_id: String,
        route: String,
        cycle: u32,
        round: u32,
        verdict: String,
    },
    /// A deliverable file was written under `deliverables/`.
    DeliverableCreated {
        task_id: String,
        path: String,
        size_bytes: u64,
    },
    /// The DMN consumer performed δ₀-δ₃ cognition evolutions.
    DmnEvolution { evolutions: Vec<EvolutionSummary> },
    /// Periodic heartbeat for connection liveness & task counts.
    TaskTreeHeartbeat {
        active_tasks: u32,
        timestamp: String,
    },
}

/// Request sent from the frontend to the engine over WebSocket.
///
/// Serialized as `{ "type": "<Variant>", "data": { ...camelCase fields } }`,
/// mirroring [`TaskEvent`] so the wire format stays uniform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    /// Execute a new root task (the `/run` command).
    ExecuteTask {
        request_id: String,
        description: String,
        max_depth: Option<u32>,
    },
    /// Submit a human yin-intervention review for a node.
    SubmitReview {
        request_id: String,
        intervention: crate::types::frontend::YinIntervention,
    },
    /// List root task ids (newest first).
    ListTasks {
        request_id: String,
    },
    /// Build the spindle tree snapshot of a root task.
    GetTaskTree {
        request_id: String,
        root_task_id: String,
    },
    /// Fetch the TPN phase detail of one node.
    GetTpnState {
        request_id: String,
        task_id: String,
    },
    /// Chat with the long-lived ChatAgent (streaming).
    ///
    /// When `session_id` is absent the server creates a new session and
    /// returns its id in the final response. Chunks are delivered as
    /// interim `ServerResponse` frames with `chunk` set; the final frame
    /// carries `stream_done = true`.
    ChatMessage {
        request_id: String,
        message: String,
        #[serde(default)]
        session_id: Option<String>,
        context_task_id: Option<String>,
    },
}

/// Directed response from the engine to the requesting frontend client.
///
/// Correlated with the originating [`ClientMessage`] via `requestId`; the
/// frontend resolves the matching pending Promise. Streaming chat responses
/// additionally set `chunk` (interim text delta) and `stream_done` (final
/// frame marker) — both fields are absent on non-streaming responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerResponse {
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Streaming text delta (chat only); absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<String>,
    /// True on the final frame of a streaming response; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_done: Option<bool>,
}

impl ServerResponse {
    /// Build a success response carrying arbitrary JSON payload.
    pub fn ok(request_id: &str, data: serde_json::Value) -> Self {
        Self {
            request_id: request_id.to_string(),
            ok: true,
            data: Some(data),
            error: None,
            chunk: None,
            stream_done: None,
        }
    }

    /// Build a failure response carrying a human-readable error string.
    pub fn err(request_id: &str, error: impl Into<String>) -> Self {
        Self {
            request_id: request_id.to_string(),
            ok: false,
            data: None,
            error: Some(error.into()),
            chunk: None,
            stream_done: None,
        }
    }

    /// Build an interim streaming chunk frame (no `data`, no `error`).
    pub fn chunk(request_id: &str, text: String) -> Self {
        Self {
            request_id: request_id.to_string(),
            ok: true,
            data: None,
            error: None,
            chunk: Some(text),
            stream_done: None,
        }
    }

    /// Build the final frame of a streaming response.
    pub fn stream_done(request_id: &str, data: serde_json::Value) -> Self {
        Self {
            request_id: request_id.to_string(),
            ok: true,
            data: Some(data),
            error: None,
            chunk: Some(String::new()),
            stream_done: Some(true),
        }
    }
}

/// Unified outbound message on a single connection's send queue:
/// either a broadcast event or a directed response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WsServerMessage {
    Event(TaskEvent),
    Response(ServerResponse),
}
