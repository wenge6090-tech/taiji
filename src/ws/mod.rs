//! L6 — Real-time event layer.
//!
//! WebSocket server that broadcasts [`types::TaskEvent`]s to connected
//! frontends (taiji-web React UI) and answers [`types::ClientMessage`]
//! requests (pure-Web mode).

pub mod handler;
pub mod server;
pub mod types;
