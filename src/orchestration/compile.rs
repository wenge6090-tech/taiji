//! V50 编译任务（§6.0 编译任务契约）——「蓝图 → skills」目的论链。
//!
//! 编译 = 一次周易任务执行（复用整个周易网络：阳 LLM 编程生成 SkillAsset、
//! 阴符号复跑验证），非独立 SkillCompiler 模块（§6.0 定论）。
//!
//! 流程：
//! 1. 连山压缩后单写者入队 `compile/{root_task}.json`（与 pending/ 分离，
//!    payload 携带 `task_dir` 引用原任务递归分解树——V68 蓝图 = 树）；
//! 2. 编译执行器在**空闲窗口**（pending 空 + `compile_enabled` 开）消费队列：
//!    读树摘要 + 物化根级产出 → 注入「标准 skill 编写规范」模板 → RecursiveRunner
//!    Execution 模式执行 → 解析 `deliverables/skill.yaml` → `save_skill`（dual
//!    校验 + git commit）；
//! 3. 编译任务**不写 model_stats**（删除本任务 pending，只产 skill YAML，不污染
//!    路由统计）；失败不产 skill，重试上限 3 次 → `.failed` + 失败日志（记录
//!    原任务树引用 + 错误）。

use crate::agents::factory::AgentFactory;
use crate::infra::config::TaijiConfig;
use crate::infra::error::TaijiError;
use crate::infra::knowledge::GuizangClient;
use crate::infra::trace::save_json_atomic;
use crate::orchestration::runner::RecursiveRunner;
use crate::types::agent::{ExternalContext, ExternalFile};
use crate::types::manifold::ManifoldTopology;
use crate::types::verification::SkillAsset;
use crate::orchestration::treeio::{TaskTreeView, summarize_tree};
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
pub async fn enqueue_compile_task(
    data_root: &Path,
    root_task: &str,
    task_dir: &str,
) -> Result<(), TaijiError> {
    let compile_dir = data_root.join("compile");
    tokio::fs::create_dir_all(&compile_dir).await?;
    let path = compile_dir.join(format!("{root_task}.json"));
    if path.exists() {
        return Ok(());
    }
    let payload = serde_json::json!({
        "root_task": root_task,
        // V68：蓝图 = 原任务递归分解树（引用 task_dir，非 manifold 拓扑）——
        // 树仍在磁盘，信息完整；编译 = 树→点收束。
        "task_dir": task_dir,
        "retries": 0,
        "enqueued_at_ms": now_ms(),
    });
    save_json_atomic(&payload, &path)?;
    tracing::info!(root_task = %root_task, task_dir = %task_dir, "[compile] compile task queued");
    Ok(())
}

/// V53 重编译变体入队（单写者 = Lianshan fork，§6.0 V53 定论）。
///
/// 低通过率 Python skill → fork 变体 → **不 clone 执行体**，而是入队 compile
/// 重新生成执行体。幂等：同 variant_id 已存在（含 .failed）→ 跳过。
pub async fn enqueue_compile_task_variant(
    data_root: &Path,
    variant_id: &str,
    variant_of: &str,
    failure_detail: &str,
) -> Result<(), TaijiError> {
    let compile_dir = data_root.join("compile");
    tokio::fs::create_dir_all(&compile_dir).await?;
    let path = compile_dir.join(format!("{variant_id}.json"));
    if path.exists() {
        return Ok(());
    }
    let payload = serde_json::json!({
        "root_task": variant_id,
        "variant_of": variant_of,
        "recompile": true,
        "failure_detail": failure_detail,
        "retries": 0,
        "enqueued_at_ms": now_ms(),
    });
    save_json_atomic(&payload, &path)?;
    tracing::info!(
        variant_id = %variant_id,
        variant_of = %variant_of,
        "[compile] recompile variant queued"
    );
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

        // 构建编译任务描述：重编译变体（V53）| 原任务递归树收束（V68）
        // V68：蓝图 = 原任务递归分解树——树摘要骨架（纯符号拼接全树
        // meta.description + deliverables 路径）+ 根级 deliverables/handoff
        // 物化进 external_ctx（编译 LLM 用 read 读 context/files/ 获取实际内容）——
        // 双注入：结构给骨架、内容给血肉。不再读拓扑（manifold 重定位为语义层链）。
        let recompile = value.get("recompile").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut external_ctx: Option<ExternalContext> = None;
        let desc = if recompile {
            let variant_of = value
                .get("variant_of")
                .and_then(|v| v.as_str())
                .unwrap_or(root_task);
            let failure_detail = value
                .get("failure_detail")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let Some(skill) = find_skill(&guizang, variant_of).await? else {
                final_fail(
                    &path,
                    &file_name,
                    &format!("skill {variant_of} not found for recompile"),
                )
                .await;
                continue;
            };
            compile_recompile_description(&skill, root_task, failure_detail)
        } else {
            let Some(td) = value.get("task_dir").and_then(|v| v.as_str()) else {
                final_fail(&path, &file_name, "compile queue file missing task_dir (V68)")
                    .await;
                continue;
            };
            let tree = match crate::orchestration::treeio::load_task_tree(Path::new(td)) {
                Ok(t) => t,
                Err(e) => {
                    final_fail(
                        &path,
                        &file_name,
                        &format!("failed to load task tree at {td}: {e}"),
                    )
                    .await;
                    continue;
                }
            };
            // 根级产出物/交接物物化（结构源 = 树摘要；内容源 = context/files/）
            let sources = crate::orchestration::treeio::collect_root_sources(Path::new(td), 6000);
            external_ctx = Some(ExternalContext {
                files: sources
                    .iter()
                    .map(|(p, c)| ExternalFile {
                        path: format!("context/files/{p}"),
                        content: c.clone(),
                    })
                    .collect(),
                tool_results: vec![],
                session_summary: None,
            });
            compile_task_description(&tree, &sources)
        };

        // 执行编译任务 = 一次周易任务执行（阳 LLM 编程 + 阴符号复跑验证）；
        // V68：携带物化的原任务产出（external_ctx）——编译 LLM 直接读实际内容。
        match runner
            .execute_with_context(&desc, external_ctx, None)
            .await {
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
                        // V55 判据类强制归阴（机械护栏）：编译 LLM 易按来源任务分类，
                        // 把「检查/判定」类误标 exec（实测 check-file-exists）。
                        enforce_judgment_category(&mut skill);
                        // V61（A 定论）：阴/元 = 归藏因果世界模型的消费者，不持有晶体
                        // 资产（Blueprint §6.0 V57 落定）。判据/收敛类 skill 产物弃置——
                        // 内置原子判据 + 语义裁决已覆盖，落盘即死资产（无消费者）。
                        // 弃置 = 成功消费（删 compile 文件不重试：同类产出不会变化）。
                        if let Some(cat) = discard_yin_category(&skill) {
                            tracing::warn!(
                                skill_id = %skill.id,
                                category = ?cat,
                                root_task = %root_task,
                                "[compile] 阴面类别 skill 弃置（V61 定论：阴不持有晶体资产）"
                            );
                            let _ = tokio::fs::remove_file(&path).await;
                            continue;
                        }
                        // V52：提取 Python 脚本（声明 python implementation 则必须有脚本）。
                        let has_python = skill.implementations.iter().any(|i| {
                            i.kind == crate::types::verification::SkillKind::Python
                        });
                        let script = match extract_skill_script(&task_dir).await {
                            Ok(s) => s,
                            Err(e) => {
                                handle_retry_or_fail(
                                    &path,
                                    &file_name,
                                    root_task,
                                    retries,
                                    &e.to_string(),
                                )
                                .await;
                                continue;
                            }
                        };
                        if has_python && script.is_none() {
                            handle_retry_or_fail(
                                &path,
                                &file_name,
                                root_task,
                                retries,
                                "python implementation 缺 deliverables/skill.py",
                            )
                            .await;
                            continue;
                        }
                        // V53 冒烟压测：save_skill 前用 python_engine 跑空 params
                        // 确认脚本可执行（连山符号裁决第一道闸，§6.0 V53 定论）。
                        // 脚本 crash / 非法 JSON / 超时 = 编译失败重试。
                        if has_python {
                            let smoke_path = task_dir.join("deliverables").join("skill.py");
                            if smoke_path.exists() {
                                match crate::orchestration::python_engine::run_python_skill(
                                    &smoke_path,
                                    &serde_json::json!({}),
                                    &task_dir,
                                    &[],
                                )
                                .await
                                {
                                    Ok(_) => {}
                                    Err(e) => {
                                        handle_retry_or_fail(
                                            &path,
                                            &file_name,
                                            root_task,
                                            retries,
                                            &format!("smoke test failed: {e}"),
                                        )
                                        .await;
                                        continue;
                                    }
                                }
                            }
                        }
                        match guizang.save_skill(&mut skill).await {
                            Ok(()) => {
                                // 旁车脚本落盘（skill.yaml 已 commit；脚本再 commit 一次——MVP 边界）。
                                if let Some(content) = script {
                                    if let Err(e) =
                                        guizang.save_skill_script(&skill, &content).await
                                    {
                                        handle_retry_or_fail(
                                            &path,
                                            &file_name,
                                            root_task,
                                            retries,
                                            &e.to_string(),
                                        )
                                        .await;
                                        continue;
                                    }
                                }
                                tokio::fs::remove_file(&path).await?;
                                processed += 1;
                                tracing::info!(
                                    task_id = %result.task_id,
                                    skill_id = %skill.id,
                                    has_python,
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

/// V52：提取编译产出的 Python 脚本（`deliverables/skill.py`）；无脚本返回 None。
async fn extract_skill_script(task_dir: &Path) -> Result<Option<String>, TaijiError> {
    let script = task_dir.join("deliverables").join("skill.py");
    if !script.exists() {
        return Ok(None);
    }
    let content = tokio::fs::read_to_string(&script).await?;
    Ok(Some(content))
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

/// V55 编译分类修正——判据类 skill 强制归阴（exec/orch → verify）。
///
/// 实测：编译 LLM 按「来源任务类型」（写脚本 → exec）分类，把「检查文件是否存在」
/// 这类**判据**（输出 passed 布尔、机械判定是否满足）误标成 exec 落到阳面；且 dual
/// 也选了同侧同类（file-exists，verify 侧）——非互补。本函数是机械护栏：
/// description 命中强判据词 且 pass_condition 含 passed 布尔判定 → 强制
/// category=verify + agent_target=YinAgent；dual 若仍同侧（verify）→ 取其对偶
/// exec skill（如 file-exists.dual = write）。返回是否改写（供 warn 审计）。
fn enforce_judgment_category(skill: &mut SkillAsset) -> bool {
    use crate::types::verification::SkillCategory;
    let Some(cat) = skill.effective_category() else {
        return false;
    };
    if !matches!(cat, SkillCategory::Exec | SkillCategory::Orch) {
        return false;
    }
    // 强判据词——动作类（执行/编排）skill 极少使用；弱词（检查/判断）易误伤不取。
    const JUDGMENT_WORDS: [&str; 6] =
        ["判定", "验证", "存在性", "当且仅当", "合法性", "一致性"];
    let desc = skill.description.to_lowercase();
    if !JUDGMENT_WORDS.iter().any(|w| desc.contains(w)) {
        return false;
    }
    let pc = skill
        .implementations
        .first()
        .map(|i| i.pass_condition.to_lowercase())
        .unwrap_or_default();
    if !pc.contains("passed") {
        return false;
    }
    // 强制归阴：verify + YinAgent。
    let old = cat;
    skill.category = Some(SkillCategory::Verify);
    if skill.agent_target.to_lowercase().contains("yang") {
        skill.agent_target = "YinAgent".into();
    }
    // dual 修正：verify 类 skill 的 dual 必须 exec 侧；原 dual 若同侧 → 取其自身对偶。
    if let Some(d) = crate::infra::meta_skills::meta_skill(&skill.dual) {
        if d.effective_category() == Some(SkillCategory::Verify) {
            let counter = d.dual.as_str();
            if let Some(c) = crate::infra::meta_skills::meta_skill(counter) {
                if c.effective_category() == Some(SkillCategory::Exec) {
                    skill.dual = counter.to_string();
                }
            }
        }
    }
    tracing::warn!(
        id = %skill.id,
        old = ?old,
        dual = %skill.dual,
        "[compile] 判据类 skill 强制归阴（exec→verify）"
    );
    true
}

/// V61 弃置闸（A 定论）：阴/元 = 归藏因果世界模型的消费者，不持有晶体资产
/// （Blueprint §6.0 V57 落定）。判据/收敛类 skill 产物弃置——内置原子判据
/// （file-exists/schema-valid/reference-resolves/trace-consistency，约束引擎 Rust
/// 内置）+ 语义裁决已覆盖，落盘即死资产（无消费者）。返回被弃置的类别。
fn discard_yin_category(skill: &SkillAsset) -> Option<crate::types::verification::SkillCategory> {
    use crate::types::verification::SkillCategory;
    let cat = skill.effective_category()?;
    if matches!(cat, SkillCategory::Verify | SkillCategory::Converge) {
        Some(cat)
    } else {
        None
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

/// V52 Python skill 脚本契约（编译模板 few-shot——教 LLM 生成资产层执行体）。
const PYTHON_SKILL_CONTRACT: &str = r#"import sys, json

def execute(params):
    # params: LLM 工具调用参数（JSON 对象，stdin 传入）
    # 确定性操作经 subprocess 调 Rust 种子层原语（用户态调 syscall）：
    #   subprocess.run(["taiji", "builtin", "write", "--args", json.dumps(...)])
    # return: {"passed": bool, "detail": str, ...}
    return {"passed": True, "detail": "..."}

if __name__ == "__main__":
    print(json.dumps(execute(json.loads(sys.stdin.read())), ensure_ascii=False))
"#;

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

## 输出：用 write 工具写三个文件

1. `deliverables/skill.py`——可执行 Python 脚本（资产层执行体）
2. `deliverables/skill.yaml`——SkillAsset 契约（YAML，必须含 `type: skill`）
3. `deliverables/handoff.md`——标准交接文档（**必须以 YAML front matter 开头**，见硬约束 6）

**路径纪律（最重要，违反必失败）**：`deliverables/skill.py` / `deliverables/skill.yaml` / `deliverables/handoff.md`
是**相对路径**，由 write 工具自动解析到**本任务目录**下——不要拼绝对路径、不要写
`/home/...` 前缀。写产物**只用 write 工具**；**禁止**用 bash 执行 `cp` / `mkdir` /
`echo >` 把文件写到绝对路径或项目根目录——bash 只用于读源码、跑测试，不用它落盘。
（阴验证扫的是本任务目录的 deliverables/，写到别处 = 收不到产物 = 编译失败。）

### skill.py 脚本契约（V52 资产层统一 Python）

```python
{PYTHON_SKILL_CONTRACT}
```

- 脚本内拿不到 OPENAI_API_KEY（env_clear 第一闸门）——**禁止**尝试调 LLM
- 30s 超时硬截止，禁止死循环 / 长网络操作
- 确定性操作一律经 `taiji builtin <name>` 原语（write/bash/read/search/webfetch），不自行重写

### skill.yaml 契约

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
  - kind: python             # 资产层统一 Python 执行体
    target: skill.py         # 脚本相对路径（skill 文件夹内）
    params: {{}}
    severity: <hard|soft>
    pass_condition: <人读判据>
agent_target: <YangAgent|YinAgent>
confidence: <0.0-1.0 先验>
version: 0
status: active
```

## SkillKind 表（V52 资产层统一 Python）

- builtin：Rust 种子层（builtin 名 = skill.id）——阳 write/bash/read/search/webfetch/recursive-decompose；阴 file-exists/schema-valid/reference-resolves/command-succeeds/trace-consistency
- python：资产层 Python 脚本（脚本相对路径 = impl.target，默认 skill.py）
- llm_judgement：唯一 LLM 裁决 kind

## skill 分类规则（按功能本质，不是按来源任务——硬约束）

编译 skill 的 category 由**它自己的功能本质**决定，**不是**由编译任务/主任务的类型决定。

- **执行类**：主动操作（写文件、跑命令、搜索、抓取、生成内容）→ `category: exec`，`agent_target: YangAgent`，dual 从 verify 侧选（file-exists/schema-valid/reference-resolves/command-succeeds/trace-consistency）
- **拆解类** → `category: orch`
- **判据类**：输入「目标/引用/内容」，输出「passed 布尔 + detail」，机械判定是否满足
  （文件存在、格式合法、引用可解析、内容一致）→ **不产出 skill**：内置原子判据
  （file-exists/schema-valid/reference-resolves/trace-consistency）已由约束引擎 Rust 内置覆盖，
  产出判据类 skill 会被系统弃置。判据类编译任务的产出 = 在 handoff.md 说明「复用内置判据」即可。
- **收敛类** → 同样**不产出**（归藏语义裁决覆盖，阴不持资产）。

**反例（已实测踩坑）**：「检查文件是否存在的脚本」功能本质是**判据**——不要把它编译成
skill（会被系统弃置）：它不该是 exec（会被 YangAgent 当执行工具误用），也不是 verify
（阴不持有晶体资产，V61 定论）。判据需求一律引用内置原子判据。

## 类别-对偶互补（硬约束）

- orch ↔ converge；exec ↔ verify
- dual 必须从下方「可用对偶表」选，且与你选的 category 类别互补
- save_skill 会机械校验 dual 存在 + 类别互补，不满足 = 编译失败

## 可用对偶表（元层保底）

{dual_candidates}

## 硬约束

1. 本任务是编译任务：直接产出 skill.py + skill.yaml，**不拆解、不递归、控制篇幅、完成即止**。
2. 只产出能机械执行的判据/执行体，不虚构不存在的验证能力。
3. skill.py 必须是可独立执行的脚本（`execute(params)` 入口，stdin JSON / stdout JSON）。
4. 产出后简述你编译了什么 skill + 为什么它可复用。
5. 写文件只用 write 工具（相对路径），禁止 bash cp/mkdir/重定向写产物。
6. `deliverables/handoff.md` **必须以 YAML front matter 开头**（首行 `---` 起、第二段 `---` 止），字段至少含 `task`、`result`、`status: complete`、`output_refs: [deliverables/skill.py, deliverables/skill.yaml]`——reference-resolves 机械检查解析 front matter 的 output_refs 逐项验存在，缺 front matter 或 output_refs 会导致阴验证 FAIL 重跑。
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

/// V53 重编译任务描述——优化低通过率 Python skill 的执行体（变体重生）。
///
/// 与 [`compile_task_description`] 同源（都教 LLM 按 SkillAsset 契约产出），但
/// 输入是「原 skill 契约 + 失败详情」，产出是**变体**（新 id = variant_id，
/// 继承原 dual/parent_id）——连山发现（符号）、周易生成（神经）、连山裁决（符号）。
pub fn compile_recompile_description(
    skill: &SkillAsset,
    variant_id: &str,
    failure_detail: &str,
) -> String {
    let skill_yaml = serde_yaml::to_string(skill).unwrap_or_else(|_| format!("{skill:#?}"));
    format!(
        r#"你是「技能编译专家」（Skill Compiler · 连山重编译任务）。

## 背景

归藏 skill「{id}」（{name}）在运行中被调用 {n} 次，通过率低于阈值（{failure}）。
这是一个**重编译任务**：fork 变体 `{variant_id}`，重新生成 Python 执行体，
解决失败原因。

## 失败详情

{failure_detail}

## 当前 skill 契约（原根资产，只读参考）

```yaml
{skill_yaml}
```

## 输出：用 write 工具写三个文件（变体）

1. `deliverables/skill.py`——优化后的 Python 脚本（执行体重生）
2. `deliverables/skill.yaml`——变体契约（YAML，必须含 `type: skill`）
3. `deliverables/handoff.md`——标准交接文档（**必须以 YAML front matter 开头**，见硬约束 5）

**路径纪律（最重要，违反必失败）**：`deliverables/skill.py` / `deliverables/skill.yaml` / `deliverables/handoff.md`
是**相对路径**，由 write 工具自动解析到**本任务目录**下——不要拼绝对路径。写产物**只用
write 工具**；**禁止**用 bash 执行 `cp` / `mkdir` / `echo >` 写到绝对路径或项目根目录
（bash 只用于读源码、跑测试，不用它落盘）。

### 变体契约硬约束

- `id` 必须 = `{variant_id}`（不要用原 id）
- `dual` 必须 = `{dual}`（继承原根）
- `parent_id` 填 `{parent}`（溯源）
- `category` 继承原根类别（重编译对象恒为阳面 exec/orch Python skill——阴面判据/收敛已在编译时弃置，见分类规则）
- `agent_target` 与 category 一致：exec → YangAgent；orch → YangAgent

### skill 分类规则（按功能本质，不是按来源任务）

- **执行类**（主动操作：写文件/跑命令/搜索/抓取/生成内容）→ `category: exec` + `agent_target: YangAgent`
- **拆解类** → orch
- **判据类**（输入目标，输出 passed 布尔，机械判定是否满足：文件存在/格式合法/引用可解析/内容一致）→ **不产出 skill**（内置原子判据已由约束引擎覆盖，产出即弃置，V61 定论）
- **收敛类** → 同样不产出（归藏语义裁决覆盖）
- dual 必须与 category **类别互补**（exec↔verify、orch↔converge），不能同侧同类别
- 反例：「检查文件是否存在」是判据——不要编译成 skill，引用内置 file-exists 原子判据
- `implementations` 的 kind 用 `python`，target = skill.py

### skill.py 脚本契约

```python
{PYTHON_SKILL_CONTRACT}
```

- 脚本内拿不到 OPENAI_API_KEY——禁止调 LLM
- 30s 超时硬截止，禁止死循环/长网络
- 确定性操作经 `taiji builtin <name>` 调 Rust 原语，或 `taiji skill <id>` 复用其他 skill

### 优化方向

- 修复失败详情指出的问题（判据过松/过严、边界未处理、参数解析错误等）
- 保持确定性、机械可执行

## 硬约束

1. 本任务是重编译任务：直接产出 skill.py + skill.yaml，不拆解、不递归、完成即止。
2. 只产出能机械执行的判据/执行体，不虚构不存在的验证能力。
3. 完成后简述你优化了什么 + 为什么能提高通过率。
4. 写文件只用 write 工具（相对路径），禁止 bash cp/mkdir/重定向写产物。
5. `deliverables/handoff.md` **必须以 YAML front matter 开头**（`---` 起止围栏），字段至少含 `task`、`result`、`status: complete`、`output_refs: [deliverables/skill.py, deliverables/skill.yaml]`——reference-resolves 机械检查解析 front matter 的 output_refs 逐项验存在。
"#,
        id = skill.id,
        name = skill.name,
        n = skill.stats.n,
        failure = failure_detail,
        variant_id = variant_id,
        failure_detail = failure_detail,
        skill_yaml = skill_yaml,
        dual = skill.dual,
        parent = skill.id,
    )
}

/// 四类扫描找 skill（重编译输入；合并视图元层∪资产层）。
async fn find_skill(
    guizang: &GuizangClient,
    id: &str,
) -> Result<Option<SkillAsset>, TaijiError> {
    use crate::infra::skill_catalog::{load_skill_catalog, ToolProfile};
    use crate::types::verification::SkillCategory;
    for category in [
        SkillCategory::Exec,
        SkillCategory::Orch,
        SkillCategory::Verify,
        SkillCategory::Converge,
    ] {
        let catalog = load_skill_catalog(guizang, category, ToolProfile::Full).await?;
        if let Some(s) = catalog.into_iter().find(|s| s.id == id) {
            return Ok(Some(s));
        }
    }
    Ok(None)
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
    use crate::types::verification::SkillCategory;

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
category: verify
implementations:
  - kind: builtin
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
        assert_eq!(skill.implementations[0].kind, crate::types::verification::SkillKind::Builtin);
        assert_eq!(skill.effective_category(), Some(crate::types::verification::SkillCategory::Verify));
    }

    #[test]
    fn parse_skill_deliverable_handles_fence() {
        let raw = "分析如下：\n```yaml\ntype: skill\nid: x\nname: X\nsummary: s\ndual: write\nimplementations:\n  - kind: builtin\nconfidence: 0.5\nversion: 0\n```\n完毕。";
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

    /// 构造 SkillAsset 字面量（测试用最小字段集）。
    fn mk_skill(id: &str, category: SkillCategory, desc: &str, pc: &str, dual: &str) -> SkillAsset {
        use crate::types::verification::{SkillImpl, SkillKind};
        SkillAsset {
            id: id.into(),
            name: id.into(),
            summary: "s".into(),
            description: desc.into(),
            detail: None,
            tags: vec![],
            examples: vec![],
            input_modes: vec!["json".into()],
            output_modes: vec!["text".into()],
            category: Some(category),
            dual: dual.into(),
            implementations: vec![SkillImpl {
                kind: SkillKind::Python,
                target: "skill.py".into(),
                params: serde_json::json!({}),
                severity: None,
                pass_condition: pc.into(),
            }],
            agent_target: "YangAgent".into(),
            confidence: 0.9,
            version: 0,
            status: "active".into(),
            stats: Default::default(),
            env_tags: vec![],
            safe_for_exploration: false,
            parent_id: None,
            variant_of: None,
        }
    }

    /// V55 实测样本：判据类（检查文件存在）被误标 exec + 同侧 dual（file-exists）→
    /// 机械强制 verify + dual 改 write + agent_target 改 YinAgent。
    #[test]
    fn enforce_judgment_category_rewrites_verify() {
        use crate::types::verification::SkillCategory;
        let mut skill = mk_skill(
            "check-file-exists",
            SkillCategory::Exec,
            "机械检查目标文件是否存在，返回 passed 布尔判定与详情，适用于验证交付物是否落盘、路径引用是否有效等存在性场景",
            "stdout 返回 JSON 含 passed 布尔值，passed=true 当且仅当目标文件存在",
            "file-exists",
        );
        assert!(enforce_judgment_category(&mut skill), "应触发判据归阴");
        assert_eq!(skill.effective_category(), Some(SkillCategory::Verify));
        assert_eq!(skill.agent_target, "YinAgent");
        // dual 修正：file-exists（verify 同侧）→ 其对偶 write（exec 侧）
        assert_eq!(skill.dual, "write");
    }

    /// 执行类（动作）skill 不受影响——描述无强判据词。
    #[test]
    fn enforce_judgment_category_keeps_action_skill() {
        use crate::types::verification::SkillCategory;
        let mut skill = mk_skill(
            "write-report",
            SkillCategory::Exec,
            "将报告内容写入目标路径，支持相对/绝对路径解析，返回写入结果",
            "脚本将内容写入目标路径并返回成功或失败",
            "file-exists",
        );
        assert!(!enforce_judgment_category(&mut skill), "动作类不应改写");
        assert_eq!(skill.effective_category(), Some(SkillCategory::Exec));
        assert_eq!(skill.dual, "file-exists");
        assert_eq!(skill.agent_target, "YangAgent");
    }

    /// 已是 verify 的 skill 不受影响。
    #[test]
    fn enforce_judgment_category_keeps_verify() {
        use crate::types::verification::SkillCategory;
        let mut skill = mk_skill(
            "already-verify",
            SkillCategory::Verify,
            "检查交付物存在性，输出 passed 判定",
            "passed=true 当且仅当目标存在",
            "write",
        );
        assert!(!enforce_judgment_category(&mut skill));
        assert_eq!(skill.effective_category(), Some(SkillCategory::Verify));
    }

    /// V61 弃置闸（A 定论）：verify/converge 类别 → 弃置；exec/orch → 放行。
    #[test]
    fn discard_yin_category_disposes_verify_and_converge() {
        use crate::types::verification::SkillCategory;
        for cat in [SkillCategory::Verify, SkillCategory::Converge] {
            let skill = mk_skill(
                &format!("yin-{cat:?}"),
                cat,
                "判据类描述",
                "passed 布尔判定",
                "write",
            );
            assert_eq!(discard_yin_category(&skill), Some(cat));
        }
        for cat in [SkillCategory::Exec, SkillCategory::Orch] {
            let skill = mk_skill(
                &format!("yang-{cat:?}"),
                cat,
                "执行类描述",
                "写入文件",
                "file-exists",
            );
            assert_eq!(discard_yin_category(&skill), None, "阳面 {cat:?} 应放行");
        }
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

    #[tokio::test]
    async fn enqueue_compile_task_variant_writes_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "taiji_compile_var_{}_{}",
            std::process::id(),
            now_ms()
        ));
        enqueue_compile_task_variant(&dir, "skill-x-v1", "skill-x", "pass_rate 0.3 < 0.6")
            .await
            .unwrap();
        let path = dir.join("compile").join("skill-x-v1.json");
        assert!(path.exists(), "variant queue file written");

        // 幂等：同 variant_id 已存在 → 不覆盖（retries 保持 0）
        enqueue_compile_task_variant(&dir, "skill-x-v1", "skill-x", "another detail")
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["retries"], 0);
        assert_eq!(value["variant_of"], "skill-x");
        assert_eq!(value["recompile"], true);
        assert_eq!(value["failure_detail"], "pass_rate 0.3 < 0.6", "first write preserved");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn compile_recompile_description_includes_variant_constraints() {
        let skill = parse_skill_deliverable(
            r#"type: skill
id: check-x
name: 检查X
summary: s
dual: write
category: verify
implementations:
  - kind: python
    target: skill.py
    severity: hard
confidence: 0.7
version: 0
status: active
"#,
        )
        .expect("valid skill");
        let desc = compile_recompile_description(&skill, "check-x-v1", "pass_rate 0.3 < 0.6");
        assert!(desc.contains("check-x-v1"), "variant id injected");
        assert!(desc.contains("parent_id"), "parent instruction present");
        assert!(desc.contains("pass_rate 0.3 < 0.6"), "failure detail injected");
        assert!(desc.contains("skill.py"), "script contract present");
        assert!(desc.contains("dual"), "dual inheritance instruction present");
    }
}
