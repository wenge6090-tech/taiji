//! WebSocket server (L6) — the taiji-web frontend bridge over a single
//! connection (ws://127.0.0.1:17890).
//!
//! Design:
//! - Binds to `127.0.0.1:<port>` only (no external exposure).
//! - **Outbound**: every connection owns an unbounded `mpsc` send queue.
//!   Broadcast events ([`TaskEvent`]) are fanned out to every queue;
//!   directed responses ([`ServerResponse`]) go to the queue of the
//!   requesting client only. One send loop per connection drains its queue.
//! - **Inbound**: the read loop parses [`ClientMessage`] frames and spawns a
//!   task that runs the matching handler ([`crate::ws::handler`]) and replies
//!   through the same connection's queue — long handlers (task execution,
//!   chat completion) never block the send/read loops.
//! - The engine emits events via [`crate::orchestration::event_bus::emit_event`]
//!   which is a zero-cost no-op when the server was never started (CLI mode).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::infra::error::TaijiError;
use crate::ws::handler::ServeState;
use crate::ws::types::{ClientMessage, ServerResponse, TaskEvent, WsServerMessage};

/// WebSocket broadcaster + request/response hub.
///
/// Clone is cheap (inner state shared via `Arc`); callers can pass clones
/// into long-lived tasks. `broadcast()` is non-blocking.
#[derive(Clone)]
pub struct WsServer {
    addr: SocketAddr,
    clients: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<WsServerMessage>>>>,
    state: Arc<Mutex<Option<Arc<ServeState>>>>,
    next_client_id: Arc<AtomicU64>,
}

impl WsServer {
    /// Create a new server bound to `127.0.0.1:<port>`.
    ///
    /// The listener is not started until [`start`](Self::start) is called.
    pub fn new(port: u16) -> Self {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        Self {
            addr,
            clients: Arc::new(Mutex::new(HashMap::new())),
            state: Arc::new(Mutex::new(None)),
            next_client_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Attach the engine snapshot used by request handlers.
    ///
    /// Call exactly once at startup (`taiji serve`). Idempotent: a second
    /// call replaces the previous state.
    pub fn set_state(&self, state: Arc<ServeState>) {
        if let Ok(mut slot) = self.state.lock() {
            *slot = Some(state);
        }
    }

    /// Snapshot of the attached engine state, if any.
    pub fn state(&self) -> Option<Arc<ServeState>> {
        self.state.lock().ok().and_then(|s| s.clone())
    }

    /// Start accepting WebSocket connections on the configured port.
    ///
    /// Spawns a background tokio task; returns immediately. Each accepted
    /// connection is handled in its own task.
    ///
    /// # Errors
    ///
    /// Returns `TaijiError::Other` if the listener cannot bind (port busy).
    pub async fn start(&self) -> Result<(), TaijiError> {
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(|e| TaijiError::Other(format!("WebSocket bind {} failed: {e}", self.addr)))?;
        let ws = Arc::new(self.clone());

        info!(addr = %self.addr, "WebSocket server listening");

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        debug!(peer = %peer, "WebSocket client connecting");
                        let (client_id, response_rx) = ws.register_client();
                        let ws_for_conn = ws.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, peer, response_rx, client_id, ws_for_conn)
                                .await;
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "WebSocket accept failed");
                        // Back off briefly on transient accept errors.
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                }
            }
        });

        Ok(())
    }

    /// Register a new client connection; returns its id and outbound queue
    /// receiver.
    fn register_client(
        &self,
    ) -> (u64, mpsc::UnboundedReceiver<WsServerMessage>) {
        let client_id = self.next_client_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut clients) = self.clients.lock() {
            clients.insert(client_id, tx);
        }
        (client_id, rx)
    }

    /// Remove a client connection (called when its socket closes).
    fn unregister_client(&self, client_id: u64) {
        if let Ok(mut clients) = self.clients.lock() {
            clients.remove(&client_id);
        }
    }

    /// Broadcast an event to all connected clients.
    ///
    /// Non-blocking. If no clients are connected, the event is silently
    /// dropped (events are advisory, not authoritative).
    pub fn broadcast(&self, event: TaskEvent) {
        let clients = self.clients.lock();
        let Ok(clients) = clients else {
            return;
        };
        let mut delivered = 0usize;
        for (_, tx) in clients.iter() {
            if tx.send(WsServerMessage::Event(event.clone())).is_ok() {
                delivered += 1;
            }
        }
        if delivered > 0 {
            debug!(delivered, "WebSocket event broadcast");
        }
    }

    /// Send a directed response to one client.
    ///
    /// Non-blocking; silently dropped if the client disconnected.
    pub fn send_to(&self, client_id: u64, response: ServerResponse) {
        if let Ok(clients) = self.clients.lock() {
            if let Some(tx) = clients.get(&client_id) {
                let _ = tx.send(WsServerMessage::Response(response));
            }
        }
    }

    /// Number of currently connected clients.
    pub fn subscriber_count(&self) -> usize {
        self.clients.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Dispatch one client request to the matching handler and reply.
    ///
    /// Runs on a spawned task so long-running handlers (task execution,
    /// chat completion) never block this connection's loops.
    async fn process_request(&self, msg: ClientMessage, client_id: u64) {
        let request_id = request_id_of(&msg).to_string();
        let state = self.state();
        let response = match state {
            Some(st) => match msg {
                ClientMessage::ExecuteTask {
                    description, max_depth, ..
                } => {
                    let snapshot =
                        crate::ws::handler::handle_execute_task(&description, max_depth, &st).await;
                    to_response(&request_id, snapshot)
                }
                ClientMessage::SubmitReview { intervention, .. } => {
                    let r = crate::ws::handler::handle_submit_review(&intervention, &st);
                    to_response(&request_id, r)
                }
                ClientMessage::ListTasks { .. } => {
                    let r = crate::ws::handler::handle_list_tasks(&st);
                    to_response(&request_id, r)
                }
                ClientMessage::GetTaskTree { root_task_id, .. } => {
                    let r = crate::ws::handler::handle_get_task_tree(&root_task_id, &st);
                    to_response(&request_id, r)
                }
                ClientMessage::GetZhouyiState { task_id, .. } => {
                    let r = crate::ws::handler::handle_get_zhouyi_state(&task_id, &st);
                    to_response(&request_id, r)
                }
                ClientMessage::PlanMessage { description, .. } => {
                    let r = crate::ws::handler::handle_plan_message(&description, &st).await;
                    to_response(&request_id, r)
                }
                ClientMessage::ChatMessage {
                    message,
                    session_id,
                    context_task_id,
                    ..
                } => {
                    let ws = self.clone();
                    let req_for_chunk = request_id.clone();
                    let on_chunk: Box<dyn Fn(String) + Send + Sync> =
                        Box::new(move |delta| {
                            ws.send_to(
                                client_id,
                                ServerResponse::chunk(&req_for_chunk, delta),
                            );
                        });
                    let result = crate::ws::handler::handle_chat_message(
                        &message,
                        session_id.as_deref(),
                        context_task_id.as_deref(),
                        &st,
                        on_chunk,
                    )
                    .await;
                    match result {
                        Ok((final_text, resolved_session_id)) => {
                            let payload = serde_json::json!({
                                "text": final_text,
                                "sessionId": resolved_session_id,
                            });
                            self.send_to(
                                client_id,
                                ServerResponse::stream_done(&request_id, payload),
                            );
                        }
                        Err(e) => {
                            self.send_to(
                                client_id,
                                ServerResponse::err(&request_id, e.to_string()),
                            );
                        }
                    }
                    return;
                }
            },
            None => ServerResponse::err(
                &request_id,
                "引擎未初始化(缺少有效配置,请先运行 taiji init)",
            ),
        };
        self.send_to(client_id, response);
    }
}

/// Extract the correlation id from any request variant.
fn request_id_of(msg: &ClientMessage) -> &str {
    match msg {
        ClientMessage::ExecuteTask { request_id, .. }
        | ClientMessage::SubmitReview { request_id, .. }
        | ClientMessage::ListTasks { request_id, .. }
        | ClientMessage::GetTaskTree { request_id, .. }
        | ClientMessage::GetZhouyiState { request_id, .. }
        | ClientMessage::PlanMessage { request_id, .. }
        | ClientMessage::ChatMessage { request_id, .. } => request_id,
    }
}

/// Wrap a handler `Result` into a [`ServerResponse`].
fn to_response<T: serde::Serialize>(request_id: &str, result: Result<T, TaijiError>) -> ServerResponse {
    match result {
        Ok(v) => match serde_json::to_value(v) {
            Ok(value) => ServerResponse::ok(request_id, value),
            Err(e) => ServerResponse::err(request_id, format!("响应序列化失败: {e}")),
        },
        Err(e) => ServerResponse::err(request_id, e.to_string()),
    }
}

/// Handle a single WebSocket connection.
///
/// Two concurrent loops:
/// - send loop: drain the outbound queue (broadcast events + directed
///   responses) as JSON text frames;
/// - read loop: parse [`ClientMessage`] frames and spawn handler tasks.
async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    mut outbound_rx: mpsc::UnboundedReceiver<WsServerMessage>,
    client_id: u64,
    ws: Arc<WsServer>,
) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!(peer = %peer, error = %e, "WebSocket handshake failed");
            return;
        }
    };

    let (mut sender, mut reader) = ws_stream.split();
    info!(peer = %peer, "WebSocket client connected");

    // Send loop: forward broadcast events + directed responses.
    let send_loop = async {
        while let Some(msg) = outbound_rx.recv().await {
            let json = match msg {
                WsServerMessage::Event(event) => match serde_json::to_string(&event) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(peer = %peer, error = %e, "Failed to serialize event");
                        continue;
                    }
                },
                WsServerMessage::Response(response) => match serde_json::to_string(&response) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(peer = %peer, error = %e, "Failed to serialize response");
                        continue;
                    }
                },
            };
            if sender.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    };

    // Read loop: keep alive, dispatch client requests, end on disconnect.
    let read_loop = async {
        while let Some(frame) = reader.next().await {
            match frame {
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => break,
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(msg) => {
                            debug!(peer = %peer, type = request_id_of(&msg), "Client request");
                            let ws_for_req = ws.clone();
                            tokio::spawn(async move {
                                ws_for_req.process_request(msg, client_id).await;
                            });
                        }
                        Err(e) => {
                            debug!(peer = %peer, error = %e, "Unparseable client frame");
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    debug!(peer = %peer, error = %e, "WebSocket read error");
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = send_loop => {}
        _ = read_loop => {}
    }

    ws.unregister_client(client_id);
    debug!(peer = %peer, "WebSocket client disconnected");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::frontend::NodeStatus;
    use crate::ws::types::TaskEvent;

    #[test]
    fn register_and_unregister_tracks_clients() {
        let ws = WsServer::new(17891);
        assert_eq!(ws.subscriber_count(), 0);

        let (id1, _rx1) = ws.register_client();
        let (id2, _rx2) = ws.register_client();
        assert_eq!(ws.subscriber_count(), 2);
        assert_ne!(id1, id2);

        ws.unregister_client(id1);
        assert_eq!(ws.subscriber_count(), 1);
        ws.unregister_client(id2);
        assert_eq!(ws.subscriber_count(), 0);
    }

    #[test]
    fn broadcast_delivers_to_all_clients() {
        let ws = WsServer::new(17892);
        let (id1, mut rx1) = ws.register_client();
        let (id2, mut rx2) = ws.register_client();

        let event = TaskEvent::TaskStatusChanged {
            task_id: "t1".into(),
            old_status: NodeStatus::Running,
            new_status: NodeStatus::Converged,
        };
        ws.broadcast(event);

        let got1 = rx1.try_recv().expect("client 1 should receive event");
        let got2 = rx2.try_recv().expect("client 2 should receive event");
        assert!(matches!(got1, WsServerMessage::Event(_)));
        assert!(matches!(got2, WsServerMessage::Event(_)));

        ws.unregister_client(id1);
        ws.unregister_client(id2);
    }

    #[test]
    fn send_to_delivers_only_to_target_client() {
        let ws = WsServer::new(17893);
        let (id1, mut rx1) = ws.register_client();
        let (id2, mut rx2) = ws.register_client();

        let resp = ServerResponse::ok("req-1", serde_json::json!({"a": 1}));
        ws.send_to(id1, resp);

        assert!(rx1.try_recv().is_ok(), "target client should receive");
        assert!(rx2.try_recv().is_err(), "other client should not receive");

        ws.unregister_client(id1);
        ws.unregister_client(id2);
    }

    #[test]
    fn broadcast_drops_silently_when_no_clients() {
        let ws = WsServer::new(17894);
        // No clients registered: broadcast must not panic, just no-op.
        ws.broadcast(TaskEvent::TaskTreeHeartbeat {
            active_tasks: 0,
            timestamp: "2025-01-01T00:00:00Z".into(),
        });
        assert_eq!(ws.subscriber_count(), 0);
    }

    #[test]
    fn request_id_of_extracts_every_variant() {
        use crate::ws::types::ClientMessage;

        let msgs = [
            ClientMessage::ExecuteTask { request_id: "a".into(), description: "d".into(), max_depth: None },
            ClientMessage::SubmitReview { request_id: "b".into(), intervention: crate::types::frontend::YinIntervention { task_id: "t".into(), action: crate::types::frontend::InterventionAction::Approve, suggestion: String::new() } },
            ClientMessage::ListTasks { request_id: "c".into() },
            ClientMessage::GetTaskTree { request_id: "d".into(), root_task_id: "r".into() },
            ClientMessage::GetZhouyiState { request_id: "e".into(), task_id: "t".into() },
            ClientMessage::PlanMessage { request_id: "f".into(), description: "p".into() },
            ClientMessage::ChatMessage { request_id: "g".into(), message: "m".into(), session_id: None, context_task_id: None },
        ];
        let ids: Vec<&str> = msgs.iter().map(request_id_of).collect();
        assert_eq!(ids, vec!["a", "b", "c", "d", "e", "f", "g"]);
    }

    #[test]
    fn to_response_wraps_ok_and_err() {
        let ok = to_response::<u32>("r1", Ok(42));
        assert!(ok.ok);
        assert_eq!(ok.data, Some(serde_json::json!(42)));

        let err = to_response::<u32>("r2", Err(TaijiError::Other("boom".into())));
        assert!(!err.ok);
        assert_eq!(err.error.as_deref(), Some("boom"));
    }
}
