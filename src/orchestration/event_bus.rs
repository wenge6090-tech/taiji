//! Global event bus (L4) — lets any engine component broadcast a
//! [`TaskEvent`] to connected frontends without holding a server reference.
//!
//! Uses a `OnceLock` to hold an optional `Arc<WsServer>`:
//! - `init_event_bus(server)` — called once at startup (serve / MCP mode).
//! - `emit_event(event)` — no-op when uninitialized (pure CLI mode).

use std::sync::Arc;

use crate::infra::error::TaijiError;
use crate::ws::server::WsServer;
use crate::ws::types::TaskEvent;

static EVENT_BUS: std::sync::OnceLock<Arc<WsServer>> = std::sync::OnceLock::new();

/// Initialise the global event bus with the shared WebSocket server.
///
/// Idempotent: a second call with a different server is rejected with an
/// error (call it exactly once at startup).
///
/// # Errors
///
/// Returns `TaijiError::Other` if the bus was already initialised with a
/// different server.
pub fn init_event_bus(server: Arc<WsServer>) -> Result<(), TaijiError> {
    match EVENT_BUS.set(server) {
        Ok(()) => Ok(()),
        Err(_) => Err(TaijiError::Other("event bus already initialised".into())),
    }
}

/// Broadcast an event to all connected frontends.
///
/// Safe to call from anywhere in the engine. No-op when the bus was never
/// initialised (CLI-only mode), so existing call paths need no changes.
pub fn emit_event(event: TaskEvent) {
    if let Some(server) = EVENT_BUS.get() {
        server.broadcast(event);
    }
}

/// Whether the event bus is active (a WebSocket server was started).
pub fn is_active() -> bool {
    EVENT_BUS.get().is_some()
}
