//! WebSocket request handlers (L6) — the pure-Web backend of the taiji-web
//! frontend.
//!
//! Each function handles one [`ClientMessage`] variant. They receive a
//! [`ServeState`] snapshot (engine factory + config + data root) and return
//! `Result<T, TaijiError>`; the WS server wraps results into
//! [`ServerResponse`] frames correlated by `requestId`.

use std::sync::Arc;

use crate::agents::factory::AgentFactory;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::orchestration::runner::RecursiveRunner;
use crate::orchestration::task_tree_builder::build_task_tree;
use crate::types::frontend::{TaskTreeSnapshot, ZhouyiPhaseState, YinIntervention};
use crate::types::plan::PlanSummary;
use crate::types::task::Task;

/// Shared engine snapshot injected into WS request handling.
///
/// Constructed once by `taiji serve` and attached to the [`WsServer`]
/// (`src/ws/server.rs`); clones are cheap (all fields are `Arc`-backed or
/// immutable).
#[derive(Clone)]
pub struct ServeState {
    pub factory: Arc<AgentFactory>,
    pub config: TaijiConfig,
    pub data_root: std::path::PathBuf,
}

/// Execute a new root task (the `/run` command).
///
/// Runs the recursive Zhouyi loop via [`RecursiveRunner`], then builds and
/// returns the resulting spindle tree snapshot. The engine broadcasts
/// `TaskCreated` / `ChildSpawned` events through the event bus as the task
/// unfolds; this function only returns once the whole recursion converges.
pub async fn handle_execute_task(
    description: &str,
    max_depth: Option<u32>,
    state: &ServeState,
) -> Result<TaskTreeSnapshot, TaijiError> {
    // 批19 P2：max_depth override 同步 factory.config（与 mcp taiji_run 同构——
    // 否则 RecursiveDecomposeTool 读旧值，与 ZhouyiCycle override 分裂）。
    let factory = if let Some(depth) = max_depth {
        let mut config = state.config.clone();
        config.runtime.max_depth = depth;
        state.factory.with_config(config)
    } else {
        state.factory.clone()
    };
    let runner = RecursiveRunner::new(factory.clone(), factory.config.clone());
    let result = runner.execute(description).await?;
    build_task_tree(&state.data_root, &result.task_id)
}

/// Yin-intervention review: write `{data_root}/tasks/{task_id}/review.json`.
///
/// The Zhouyi cycle injects this on resume (approval closed loop).
pub fn handle_submit_review(
    intervention: &YinIntervention,
    state: &ServeState,
) -> Result<(), TaijiError> {
    let dir = state.data_root.join("tasks").join(&intervention.task_id);
    if !dir.is_dir() {
        return Err(TaijiError::Other(format!(
            "任务 {} 不存在",
            intervention.task_id
        )));
    }
    let path = dir.join("review.json");
    let json = serde_json::to_string_pretty(intervention)?;
    std::fs::write(&path, json)?;
    tracing::info!(
        task_id = %intervention.task_id,
        action = ?intervention.action,
        "审批已写入"
    );
    Ok(())
}

/// List root task ids (newest first by directory mtime), for the frontend
/// multi-task dropdown.
pub fn handle_list_tasks(state: &ServeState) -> Result<Vec<String>, TaijiError> {
    let tasks_dir = state.data_root.join("tasks");
    let mut entries: Vec<(std::time::SystemTime, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&tasks_dir) {
        for entry in rd.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            entries.push((mtime, name));
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, id)| id).collect())
}

/// Build the spindle tree snapshot of one root task (fresh from disk).
pub fn handle_get_task_tree(
    root_task_id: &str,
    state: &ServeState,
) -> Result<TaskTreeSnapshot, TaijiError> {
    build_task_tree(&state.data_root, root_task_id)
}

/// Fetch the Zhouyi phase detail of one node, built live from
/// `checkpoint.json` / `meta.json` / `deliverables/` / `trace.jsonl`.
pub fn handle_get_zhouyi_state(
    task_id: &str,
    state: &ServeState,
) -> Result<ZhouyiPhaseState, TaijiError> {
    let task_dir = state.data_root.join("tasks").join(task_id);
    if !task_dir.is_dir() {
        return Err(TaijiError::Other(format!("任务 {task_id} 不存在")));
    }

    // Phase from checkpoint.json
    let mut current_phase = crate::types::frontend::ZhouyiPhase::Idle;
    let checkpoint_path = task_dir.join("checkpoint.json");
    if let Ok(content) = std::fs::read_to_string(&checkpoint_path) {
        if let Ok(cp) = serde_json::from_str::<crate::types::task::Checkpoint>(&content) {
            current_phase = match cp.phase {
                crate::types::task::CyclePhase::MetaDone => {
                    crate::types::frontend::ZhouyiPhase::Meta
                }
                crate::types::task::CyclePhase::YangDone => {
                    crate::types::frontend::ZhouyiPhase::Yang
                }
                crate::types::task::CyclePhase::YinDone => {
                    crate::types::frontend::ZhouyiPhase::Yin
                }
            };
        }
    }
    // Converged from meta.json status
    let meta_path = task_dir.join("meta.json");
    if let Ok(content) = std::fs::read_to_string(&meta_path) {
        if let Ok(task) = serde_json::from_str::<Task>(&content) {
            if matches!(
                task.status,
                crate::types::task::TaskStatus::Completed
            ) {
                current_phase = crate::types::frontend::ZhouyiPhase::Converged;
            }
        }
    }

    // Deliverables listing
    let mut deliverables: Vec<String> = Vec::new();
    let deliv_dir = task_dir.join("deliverables");
    if let Ok(entries) = std::fs::read_dir(&deliv_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                deliverables.push(name.to_string());
            }
        }
    }

    // Last 8 trace.jsonl records as preview
    let mut trace_preview: Vec<crate::types::frontend::TraceRecordPreview> = Vec::new();
    let trace_path = task_dir.join("trace.jsonl");
    if let Ok(content) = std::fs::read_to_string(&trace_path) {
        for line in content.lines().rev().take(8) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                let summary = v["output"]
                    .as_str()
                    .map(|s| s.chars().take(200).collect::<String>())
                    .or_else(|| {
                        // 批19 P2 修复：output 为对象（completion/tool_call 结果）
                        // 时 .as_str() 恒 None → 序列化为 JSON 截断。
                        serde_json::to_string(&v["output"])
                            .ok()
                            .map(|s| s.chars().take(200).collect::<String>())
                    })
                    .unwrap_or_default();
                trace_preview.push(crate::types::frontend::TraceRecordPreview {
                    ts: v["ts"].as_str().unwrap_or("").to_string(),
                    phase: v["phase"].as_str().unwrap_or("").to_string(),
                    cycle: v["cycle"].as_u64().unwrap_or(0) as u32,
                    round: 0,
                    tool: None,
                    summary,
                });
            }
        }
        trace_preview.reverse();
    }

    Ok(ZhouyiPhaseState {
        task_id: task_id.to_string(),
        current_phase,
        meta_summary: None,
        yang_summary: None,
        yin_verdict: None,
        deliverables,
        trace_preview,
    })
}

/// Pre-execution planning (the chat panel's `/plan` command).
///
/// Runs the PlanBuilder (MetaAgent + LLM plan composition) without entering
/// the Zhouyi loop, returning a speculative [`PlanSummary`]. Uses the same
/// readable task-id scheme as the `taiji_plan` MCP tool (not persisted as a
/// task dir).
pub async fn handle_plan_message(
    description: &str,
    state: &ServeState,
) -> Result<PlanSummary, TaijiError> {
    let task_id = crate::infra::task_id::generate_task_id(description);
    let plan_agent = state.factory.create_plan_agent(&task_id)?;
    let tags = crate::agents::meta::classify_task_tags(description);
    let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
    plan_agent.plan(description, &tag_refs).await
}

/// Stream a chat message to the long-lived ChatAgent (the chat panel's
/// normal message).
///
/// Resolves (or creates) a session id, loads the session history from
/// `{data_root}/chat/{session_id}.json`, then streams the agent's reply.
/// Every text delta is delivered through `on_chunk` (the WS server forwards
/// them as interim `ServerResponse` frames). Returns
/// `(final_text, resolved_session_id)`.
///
/// LLM latency can reach tens of seconds — the frontend uses a dedicated
/// longer timeout for this request type.
pub async fn handle_chat_message(
    message: &str,
    session_id: Option<&str>,
    context_task_id: Option<&str>,
    state: &ServeState,
    on_chunk: Box<dyn Fn(String) + Send + Sync>,
) -> Result<(String, String), TaijiError> {
    let resolved_session = match session_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => uuid::Uuid::new_v4().to_string(),
    };

    let builder = state.factory.create_chat_agent(
        resolved_session.clone(),
        context_task_id.map(str::to_string),
        None,
        None,
    )?;
    let mut history = builder.load_history();
    let final_text = builder
        .chat(message, &mut history, on_chunk)
        .await?;
    Ok((final_text, resolved_session))
}
