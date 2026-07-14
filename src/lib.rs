// taiji — lightweight AGI cognitive kernel (TPN-DMN-NSKG)
//
// Module structure (7-layer):
//   L0 types          — foundational data types
//   L1 infra          — infrastructure (config, qdrant, providers, trace, etc.)
//   L2 hooks          — Rig AgentHook implementations (safety, trace)
//   L3 agents         — transient Rig Agents (meta, fitting, causal) + tools
//   L4 orchestration  — runner, constraint engine, trigger engine, cognition evolver
//   L5 mcp            — MCP server/client
//   L6 entry          — main.rs (clap CLI)

pub mod types;
pub mod infra;
pub mod hooks;
pub mod agents;
pub mod orchestration;
pub mod mcp;

// Re-export key types for convenience
pub use types::{
    task::{Task, TaskStatus, TPNResult, SubtaskSpec, DecomposeResult},
    agent::{Chain, ReasoningPath, MetaContext},
    verification::{VerificationReport, ConvergenceDecision, VerificationRoute, ConvergenceStatus},
};
