// taiji — lightweight AGI cognitive kernel (Zhouyi-Lianshan-归藏)
//
// Module structure (7-layer):
//   L0 types          — foundational data types
//   L1 infra          — infrastructure (config, knowledge, providers, trace, etc.)
//   L2 hooks          — Rig AgentHook implementations (safety, trace)
//   L3 agents         — transient Rig Agents (meta, yang, yin) + tools
//   L4 orchestration  — runner, constraint engine, trigger engine, cognition evolver
//   L5 mcp            — MCP server/client
//   L6 ws             — WebSocket event server (frontend bridge)
//   L7 entry          — main.rs (clap CLI)

pub mod types;
pub mod infra;
pub mod hooks;
pub mod agents;
pub mod orchestration;
pub mod mcp;
pub mod ws;

// Re-export key types for convenience
pub use types::{
    task::{Checkpoint, ChildResultSummary, CyclePhase, DecomposeResult, SubtaskSpec, Task, TaskStatus, ZhouyiResult},
    agent::MetaContext,
    verification::{VerificationReport, ConvergenceDecision, VerificationRoute, ConvergenceStatus},
};
