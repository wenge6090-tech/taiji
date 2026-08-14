//! V50 编译任务（§6.0 编译任务契约）——「蓝图 → skills」目的论链。
//!
//! 编译 = 一次周易任务执行（复用整个周易网络：阳 LLM 编程生成 SkillAsset、
//! 阴符号复跑验证），非独立 SkillCompiler 模块（§6.0 定论）。
//!
//! 流程：
//! 1. 连山拓扑产出后单写者入队 `compile/{root_task}.json`（与 pending/ 分离，
//!    payload 引用 `manifold/{root_task}.yaml`）；
//! 2. 编译执行器在**空闲窗口**（pending 空 + `compile_enabled` 开）消费队列：
//!    读拓扑 → 注入「标准 skill 编写规范」模板 → RecursiveRunner Execution 模式
//!    执行 → 解析 `deliverables/skill.yaml` → `save_skill`（dual 校验 + git commit）；
//! 3. 编译任务**不写 model_stats**（删除本任务 pending，只产 skill YAML，不污染
//!    路由统计）；失败不产 skill，重试上限 3 次 → `.failed` + 失败日志（记录
//!    manifold 引用 + 错误）。

use crate::agents::factory::AgentFactory;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::infra::knowledge::GuizangClient;
use crate::infra::trace::save_json_atomic;
use crate::orchestration::runner::RecursiveRunner;
use crate::types::manifold::ManifoldTopology;
use crate::types::verification::SkillAsset;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 编译失败重试上限（§6.0 调度定稿：3 次总尝试）。
pub(crate) const MAX_COMPILE_RETRIES: u32 = 3;

/// 编译空闲轮询间隔（与主动学习执行器一致）。
const IDLE_SLEEP_MS: u64 = 5_000;

/// 入队编译任务（单写者 = Lianshan Consumer，§6.0 调度定稿）。
///
/// 幂等：同 root_task 已存在（含 .failed）→ 跳过，不覆盖。
pub async fn enqueue_compile_task(data_root: &Path, root_task: &str) -> Result<(), TaijiError> {
    let compile_dir = data_root.join("compile");
    tokio::fs::create_dir_all(&compile_dir).await?;
    let path = compile_dir.join(format!("{root_task}.json"));
    if path.exists() {
        return Ok(());
    }
    let payload = serde_json::json!({
        "root_task": root_task,
        "manifold": format!("manifold/{root_task}.yaml"),
        "retries": 0,
        "enqueued_at_ms": now_ms(),
    });
    save_json_atomic(&payload, &path)?;
    tracing::info!(root_task = %root_task, "[compile] compile task queued");
    Ok(())
}

/// 编译执行器入口（main.rs `--with-lianshan` 时 spawn；`compile_enabled` 关 → 不启动）。
pub fn spawn_compiler(
    factory: Arc<AgentFactory>,
    config: TaijiConfig,
    data_root: &Path,
    cancel: CancellationToken,
    guizang: Arc<GuizangClient>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !config.runtime.lianshan.compile_enabled {
        return None;
    }
    let data_root = data_root.to_path_buf();
    Some(tokio::spawn(async move {
        let runner = RecursiveRunner::new(factory, config.clone());
        loop {
            if cancel.is_cancelled() {
                tracing::info!("[compile] compiler cancelled, exiting");
                return;
            }
            match run_compile_queue(&runner, &guizang, &data_root, &cancel).await {
                Ok(processed) if processed == 0 => {
                    // 空闲（无队列 / pending 忙）等待
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(IDLE_SLEEP_MS)) => {}
                        _ = cancel.cancelled() => {}
                    }
                }
                Ok(_) => {} // 处理完立即再扫
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "[compile] compile queue scan failed — retry next cycle"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(IDLE_SLEEP_MS)) => {}
                        _ = cancel.cancelled() => {}
                    }
                }
            }
        }
    }))
}

/// 消费 compile/ 队列（单次扫描）。
///
/// 空闲窗口：pending 非空 → 返回 0（等 Lianshan 消费主学习队列，编译不与主
/// 学习竞争，§6.0 调度定稿）。每个编译任务：读拓扑 → 执行周易 → 解析 skill →
/// save_skill → 删 compile 文件 + 本任务 pending。失败重试 ≤3 → .failed。
///
/// # Returns
/// 本次成功编译的任务数。
async fn run_compile_queue(
    runner: &RecursiveRunner,
    guizang: &GuizangClient,
    data_root: &Path,
    cancel: &CancellationToken,
) -> Result<u32, TaijiError> {
    if pending_has_work(data_root).await {
        return Ok(0);
    }
    let compile_dir = data_root.join("compile");
    if !compile_dir.exists() {
        return Ok(0);
    }
    let mut entries = tokio::fs::read_dir(&compile_dir).await?;
    let mut processed = 0u32;
    while let Some(entry) = entries.next_entry().await.transpose() {
        if cancel.is_cancelled() {
            break;
        }
        let path = entry?.path();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if path.extension().is_none_or(|x| x != "json") || file_name.ends_with(".failed") {
            continue;
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let value: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                let _ = tokio::fs::rename(&path, path.with_extension("json.failed")).await;
                tracing::warn!(file = %file_name, error = %e, "[compile] queue file parse failed — marked .failed");
                continue;
            }
        };
        let Some(root_task) = value.get("root_task").and_then(|v| v.as_str()) else {
            let _ = tokio::fs::rename(&path, path.with_extension("json.failed")).await;
            tracing::warn!(file = %file_name, "[compile] queue file missing root_task — marked .failed");
            continue;
        };
        let retries = value.get("retries").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

        // 读 manifold/{root_task}.yaml（缺失 = 不可编译，终态失败）
        let Some(topo) = guizang.load_topology(root_task).await? else {
            final_fail(&path, &file_name, &format!("manifold/{root_task}.yaml missing")).await;
            continue;
        };

        // 构建编译任务描述（标准 skill 编写规范模板 + 拓扑注入）
        let desc = compile_task_description(&topo);

        // 执行编译任务 = 一次周易任务执行（阳 LLM 编程 + 阴符号复跑验证）
        match runner.execute_with_context(&desc, None, None).await {
            Ok(result) => {
                let task_dir = data_root.join("tasks").join(&result.task_id);
                // §6.0：编译不写 model_stats、不触发二次拓扑/编译——
                // 删除本编译任务的 pending（zhouyi PASS 会入队，但编译只产 skill YAML）。
                let _ = tokio::fs::remove_file(
                    data_root
                        .join("pending")
                        .join(format!("{}.json", result.task_id)),
                )
                .await;
                match extract_skill(&task_dir, &result.content).await {
                    Ok(mut skill) => {
                        if skill.implementations.is_empty() {
                            handle_retry_or_fail(
                                &path,
                                &file_name,
                                root_task,
                                retries,
                                "implementations empty",
                            )
                            .await;
                            continue;
                        }
                        match guizang.save_skill(&mut skill).await {
                            Ok(()) => {
                                tokio::fs::remove_file(&path).await?;
                                processed += 1;
                                tracing::info!(
                                    task_id = %result.task_id,
                                    skill_id = %skill.id,
                                    "[compile] skill compiled + saved"
                                );
                            }
                            Err(e) => {
                                handle_retry_or_fail(
                                    &path,
                                    &file_name,
                                    root_task,
                                    retries,
                                    &e.to_string(),
                                )
                                .await;
                            }
                        }
                    }
                    Err(e) => {
                        handle_retry_or_fail(&path, &file_name, root_task, retries, &e.to_string())
                            .await;
                    }
                }
            }
            Err(e) => {
                handle_retry_or_fail(&path, &file_name, root_task, retries, &e.to_string()).await;
            }
        }
    }
    Ok(processed)
}

/// 失败处理：未达重试上限 → 更新 retries 留队；达上限 → 终态失败（.failed + 日志）。
async fn handle_retry_or_fail(
    path: &Path,
    file_name: &str,
    root_task: &str,
    retries: u32,
    error: &str,
) {
    let next = retries + 1;
    if next >= MAX_COMPILE_RETRIES {
        final_fail(path, file_name, error).await;
    } else {
        let payload = serde_json::json!({
            "root_task": root_task,
            "manifold": format!("manifold/{root_task}.yaml"),
            "retries": next,
            "last_error": error,
        });
        if let Err(e) = save_json_atomic(&payload, path) {
            tracing::warn!(file = %file_name, error = %e, "[compile] rewrite retries failed");
        }
        tracing::warn!(
            file = %file_name,
            root_task = %root_task,
            retries = next,
            error,
            "[compile] attempt failed — will retry ({next}/{MAX_COMPILE_RETRIES})"
        );
    }
}

/// 终态失败：写失败日志（记录 manifold 引用 + 错误）+ 改名 `.failed`。
async fn final_fail(path: &Path, file_name: &str, error: &str) {
    let log_path = path.with_extension("json.error");
    let log = serde_json::json!({
        "manifold": file_name.strip_suffix(".json").unwrap_or(file_name),
        "error": error,
        "failed_at_ms": now_ms(),
    });
    if let Err(e) = save_json_atomic(&log, &log_path) {
        tracing::warn!(file = %file_name, error = %e, "[compile] write failure log failed");
    }
    if let Err(e) = tokio::fs::rename(path, path.with_extension("json.failed")).await {
        tracing::warn!(file = %file_name, error = %e, "[compile] rename .failed failed");
    }
    tracing::warn!(
        file = %file_name,
        error,
        "[compile] compile failed after {MAX_COMPILE_RETRIES} attempts — marked .failed"
    );
}

/// pending/ 是否还有未处理任务（空闲窗口判断；dead/ 子目录不计）。
async fn pending_has_work(data_root: &Path) -> bool {
    let pending_dir = data_root.join("pending");
    let Ok(mut rd) = tokio::fs::read_dir(&pending_dir).await else {
        return false;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "dead" {
            continue;
        }
        let is_file = entry.file_type().await.map(|t| t.is_file()).unwrap_or(false);
        if is_file && entry.path().extension().is_some_and(|x| x == "json") {
            return true;
        }
    }
    false
}

/// 从任务目录提取 skill 文件内容并解析为 [`SkillAsset`]。
///
/// 查找顺序：`deliverables/skill.yaml` → `deliverables/skill.json` →
/// `deliverables/` 下首个 yaml/json → 兜底 `result.content`（最终答案正文）。
async fn extract_skill(task_dir: &Path, fallback_content: &str) -> Result<SkillAsset, TaijiError> {
    let deliverables = task_dir.join("deliverables");
    for name in ["skill.yaml", "skill.json"] {
        let p = deliverables.join(name);
        if p.exists() {
            let content = tokio::fs::read_to_string(&p).await?;
            return parse_skill_deliverable(&content);
        }
    }
    if let Ok(mut rd) = tokio::fs::read_dir(&deliverables).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|x| x == "yaml" || x == "json") {
                let content = tokio::fs::read_to_string(&p).await?;
                return parse_skill_deliverable(&content);
            }
        }
    }
    parse_skill_deliverable(fallback_content)
}

/// 解析 LLM 产出的 skill 内容（容忍 markdown 围栏 + 叙述前后缀）。
///
/// 顺序：去围栏 → YAML → JSON → `parse_llm_json`（首尾大括号切片）。
pub fn parse_skill_deliverable(raw: &str) -> Result<SkillAsset, TaijiError> {
    let stripped = strip_fences(raw);
    if let Ok(s) = serde_yaml::from_str::<SkillAsset>(&stripped) {
        return Ok(s);
    }
    if let Ok(s) = serde_json::from_str::<SkillAsset>(&stripped) {
        return Ok(s);
    }
    match crate::infra::json_util::parse_llm_json::<SkillAsset>(&stripped) {
        Ok(s) => Ok(s),
        Err(e) => Err(TaijiError::StructuredOutputParseFailed {
            context: format!("skill 解析失败（yaml/json 均失败）: {e}"),
        }),
    }
}

/// 提取 ``` 围栏内容（跳过语言标签行），无围栏返回原文 trim。
fn strip_fences(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(start) = raw.find("```") {
        let after_start = &raw[start + 3..];
        let content_start = after_start.find('\n').map(|n| n + 1).unwrap_or(0);
        let after_start = &after_start[content_start..];
        if let Some(end) = after_start.find("```") {
            return after_start[..end].trim().to_string();
        }
    }
    raw.to_string()
}

/// 「标准 skill 编写规范」模板（§6.0 编译任务契约）——教 LLM 按 SkillAsset 契约产出。
pub fn compile_task_description(topo: &ManifoldTopology) -> String {
    let topo_yaml =
        serde_yaml::to_string(topo).unwrap_or_else(|_| format!("{topo:#?}"));
    let dual_candidates = build_dual_candidates();
    format!(
        r#"你是「技能编译专家」（Skill Compiler · 连山编译任务）。

## 背景

taiji 把一个成功任务的执行迹压缩成了「迹拓扑」（离散状态转移图）。你的任务：从迹拓扑
中提取一个**可复用的程序 + 说明书**，编译为归藏 Skill 资产——供未来相似任务直接调用
（持续学习闭环「迹 → 蓝图 → skills → 新迹」，§6.0 目的论）。

## 输入：执行迹拓扑

{topo_yaml}

## 分析要点

- `Task` 节点 + `decompose` 边 = 任务如何拆解（编排能力）
- `invoke` 边（task → asset）= 用了什么资产/技能
- `dataflow` 边（task → deliverable）= 产出了什么
- `verify` 边（task → check）= 如何验证产出
提炼一个**原子可复用能力**：它做什么、产出什么、如何机械验证。

## 输出：用 write 工具写 deliverables/skill.yaml（YAML，必须含 `type: skill`）

```yaml
type: skill
id: <kebab-case 唯一标识>
name: <人类可读短名>
summary: <一句话摘要，≤30 字>
description: <功能描述：何时调用、做什么>
tags: [<分类标签>]
examples: [<自然语言使用示例>]
input_modes: ["text"]        # 或 ["json"] 或 ["json","text"]
output_modes: ["text"]
category: <orch|exec|verify|converge>
dual: <对偶 skill id，见下表硬约束>
implementations:
  - kind: <SkillKind>
    target: <相对 task_dir 路径，阴判据用；阳执行体留空>
    params: {{}}
    severity: <hard|soft>
    pass_condition: <人读判据>
agent_target: <YangAgent|YinAgent>
confidence: <0.0-1.0 先验>
version: 0
status: active
```

## SkillKind 表

- 阴（YinAgent 机械/裁决判据）：file_exists / schema_valid / reference_resolves /
  command_succeeds / trace_consistency / llm_judgement
- 阳（YangAgent builtin 执行体）：bash / write / read / search / webfetch / recursive_decompose

## 类别-对偶互补（硬约束）

- orch ↔ converge；exec ↔ verify
- dual 必须从下方「可用对偶表」选，且与你选的 category 类别互补
- save_skill 会机械校验 dual 存在 + 类别互补，不满足 = 编译失败

## 可用对偶表（元层保底）

{dual_candidates}

## 硬约束

1. 本任务是编译任务：直接产出 skill.yaml，**不拆解、不递归、控制篇幅、完成即止**。
2. 只产出能机械执行的判据/执行体，不虚构不存在的验证能力。
3. 产出后简述你编译了什么 skill + 为什么它可复用。
"#
    )
}

/// 枚举元层 skill（id + 类别）作为对偶候选表。
fn build_dual_candidates() -> String {
    crate::infra::meta_skills::all_meta_skills()
        .iter()
        .map(|s| {
            let cat = s
                .effective_category()
                .map(|c| format!("{c:?}").to_lowercase())
                .unwrap_or_else(|| "?".into());
            format!("- {} ({})", s.id, cat)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::manifold::{TopologyNode, TopologyNodeKind};

    fn mk_topo() -> ManifoldTopology {
        ManifoldTopology {
            root_task: "task-x".into(),
            generated_at: 0,
            nodes: vec![TopologyNode {
                id: "task-x".into(),
                kind: TopologyNodeKind::Task,
                depth: 0,
                stats: Default::default(),
            }],
            edges: vec![],
        }
    }

    #[test]
    fn parse_skill_deliverable_parses_yaml() {
        let raw = r#"type: skill
id: compile-report
name: 编译报告
summary: 生成报告
description: 测试技能
dual: write
implementations:
  - kind: file_exists
    target: deliverables/*
    severity: hard
    pass_condition: 产出存在
agent_target: YinAgent
confidence: 0.7
version: 0
status: active
"#;
        let skill = parse_skill_deliverable(raw).expect("valid skill yaml");
        assert_eq!(skill.id, "compile-report");
        assert_eq!(skill.dual, "write");
        assert_eq!(skill.implementations.len(), 1);
        assert_eq!(skill.implementations[0].kind, crate::types::verification::SkillKind::FileExists);
        assert_eq!(skill.effective_category(), Some(crate::types::verification::SkillCategory::Verify));
    }

    #[test]
    fn parse_skill_deliverable_handles_fence() {
        let raw = "分析如下：\n```yaml\ntype: skill\nid: x\nname: X\nsummary: s\ndual: write\nimplementations:\n  - kind: file_exists\nconfidence: 0.5\nversion: 0\n```\n完毕。";
        let skill = parse_skill_deliverable(raw).expect("fenced skill yaml");
        assert_eq!(skill.id, "x");
    }

    #[test]
    fn compile_task_description_includes_topo_and_duals() {
        let desc = compile_task_description(&mk_topo());
        assert!(desc.contains("task-x"), "topology root injected");
        assert!(desc.contains("file-exists"), "dual candidates listed");
        assert!(desc.contains("exec"), "category names present");
        assert!(desc.contains("deliverables/skill.yaml"), "output instruction present");
        assert!(desc.contains("不拆解、不递归"), "Execution mode hint present");
    }

    #[test]
    fn strip_fences_extracts_content() {
        let raw = "```yaml\ntype: skill\n```";
        assert_eq!(strip_fences(raw), "type: skill");
        assert_eq!(strip_fences("plain text"), "plain text");
    }

    #[tokio::test]
    async fn enqueue_compile_task_writes_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "taiji_compile_enq_{}_{}",
            std::process::id(),
            now_ms()
        ));
        enqueue_compile_task(&dir, "task-x").await.unwrap();
        let path = dir.join("compile").join("task-x.json");
        assert!(path.exists(), "queue file written");

        // 幂等：已存在 → 不覆盖（retries 保持 0）
        enqueue_compile_task(&dir, "task-x").await.unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["retries"], 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
