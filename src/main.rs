use std::sync::Arc;

use clap::{Parser, Subcommand};

/// taiji — lightweight AGI cognitive kernel
#[derive(Parser)]
#[command(name = "taiji", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execute a task
    Run {
        /// Task description (space-separated words, joined internally)
        description: Vec<String>,
        /// Resume an interrupted/failed task by ID (reuses task dir + recovery chain)
        #[arg(long)]
        resume: Option<String>,
        /// Activate the Lianshan Consumer (passive learning — backprop check stats)
        #[arg(long)]
        with_lianshan: bool,
    },
    /// Initialize workspace (.taiji/ + 归藏 knowledge store)
    Init,
    /// Restore seed assets from an existing model partition directory
    /// (prompts/ + verifications/) into the knowledge root (V44 去分区化恢复工具)。
    /// Stats/models NOT copied — learning units accumulate from zero.
    Seed {
        /// Source partition key (`{provider}-{model}` slug) to restore from
        model_key: String,
    },
    /// Show task trace
    Trace {
        /// Task ID
        task_id: String,
        /// Recursively merge nested traces
        #[arg(long)]
        tree: bool,
        /// Tail last N records
        #[arg(long)]
        tail: Option<usize>,
    },
    /// List tasks
    List,
    /// Show Lianshan / cognition status
    Status,
    /// Migrate legacy persisted values in task dirs (V45 曾用名→新值，一次性)
    Migrate,
    /// Invoke a Rust seed-layer builtin as a syscall primitive (userland → kernel).
    /// Used by Python asset-layer skills via subprocess.
    Builtin {
        /// builtin name: read | write | bash | search | webfetch
        name: String,
        /// JSON args (e.g. '{"command":"echo hi"}')
        #[arg(long)]
        args: Option<String>,
        /// task dir for cwd-scoped operations (default: current dir)
        #[arg(long)]
        task_dir: Option<String>,
    },
    /// Invoke an asset-layer Python skill as userland (V53: skill 嵌套 skill)。
    /// 循环/深度护栏经 TAIJI_SKILL_CHAIN 环境变量传递调用链。
    Skill {
        /// skill id (asset-layer Python skill)
        id: String,
        /// JSON args
        #[arg(long)]
        args: Option<String>,
        /// task dir for cwd-scoped operations (default: current dir)
        #[arg(long)]
        task_dir: Option<String>,
    },
    /// Start MCP server for tool integration
    Mcp,
    /// Serve the taiji-web frontend (pure-Web mode): HTTP static hosting +
    /// WebSocket bridge on 17890 + auto-open browser
    Serve {
        /// HTTP port for the frontend (default 1420)
        #[arg(long, default_value_t = 1420)]
        port: u16,
        /// Do not auto-open the browser
        #[arg(long)]
        no_open: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            description,
            resume,
            with_lianshan,
        } => cmd_run(description, resume, with_lianshan).await?,
        Command::Init => cmd_init().await?,
        Command::Seed { model_key } => cmd_seed(&model_key).await?,
        Command::Trace {
            task_id,
            tree,
            tail,
        } => cmd_trace(&task_id, tree, tail).await?,
        Command::List => cmd_list()?,
        Command::Status => cmd_status()?,
        Command::Migrate => cmd_migrate().await?,
        Command::Builtin {
            name,
            args,
            task_dir,
        } => cmd_builtin(&name, args, task_dir).await?,
        Command::Skill {
            id,
            args,
            task_dir,
        } => cmd_skill(&id, args, task_dir).await?,
        Command::Mcp => cmd_mcp().await?,
        Command::Serve { port, no_open } => cmd_serve(port, no_open).await?,
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

async fn cmd_run(
    description: Vec<String>,
    resume: Option<String>,
    with_lianshan: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let desc = description.join(" ");
    let config = load_config()?;
    let data_root = std::path::PathBuf::from(&config.data_root);

    // Provider registry (LLM clients).
    let factory = build_engine(&config).await?;

    // ── V33/MVP-2: --with-lianshan 激活 Lianshan Consumer（被动学习 — Blueprint §5.3/AGENTS.md）──
    // Zhouyi PASS 入队 pending/（zhouyi.rs enqueue_lianshan_pending）→ 本进程内
    // 消费者单写归藏统计。MVP 时序：任务结束后等待 3s（消费者 1s 首扫 + 处理）
    // 再退出；正式生命周期（serve 常驻/主动学习）归 MVP-3。
    let mut lianshan_handle: Option<(
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
        tokio_util::sync::CancellationToken,
    )> = None;
    if with_lianshan {
        let evolver = Arc::new(
            taiji::orchestration::cognition_evolver::CognitionEvolver::new(
                factory.guizang.clone(),
            ),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let lianshan_config = config.runtime.lianshan.clone();
        let consumer = taiji::orchestration::lianshan::LianshanConsumer::new(
            evolver,
            cancel.clone(),
            &data_root,
            lianshan_config.clone(),
        );
        // V33/MVP-3 主动学习执行器（默认关闭；开启时消费 experiments/ 队列）
        // V44：探索目标直接使用根级资产树。
        let al_handle = taiji::orchestration::active_learning::spawn_runner(
            factory.clone(),
            config.clone(),
            &data_root,
            cancel.clone(),
        );
        // V50 编译执行器（默认关闭；开启时消费 compile/ 队列，§6.0）
        let compile_handle = taiji::orchestration::compile::spawn_compiler(
            factory.clone(),
            config.clone(),
            &data_root,
            cancel.clone(),
            factory.guizang.clone(),
        );
        lianshan_handle = Some((
            consumer.spawn(),
            al_handle.unwrap_or_else(|| tokio::task::spawn(async {})),
            compile_handle.unwrap_or_else(|| tokio::task::spawn(async {})),
            cancel,
        ));
        tracing::info!(
            pending_dir = %data_root.join("pending").display(),
            active_learning = lianshan_config.active_learning_enabled,
            compile = lianshan_config.compile_enabled,
            "--with-lianshan: Lianshan Consumer spawned (passive learning)"
        );
    }

    // Execute task via RecursiveRunner (V26: --resume reuses task dir + recovery chain).
    let runner = taiji::orchestration::runner::RecursiveRunner::new(factory, config);
    let result = runner.execute_with_context(&desc, None, resume).await?;

    println!("✓ Task completed: {}", result.task_id);
    println!("  Content: {}", result.content);
    println!("  Tools used: {}", result.tools_used.join(", "));

    // ── 等待 Lianshan Consumer 处理 pending 后退出（MVP 时序修正）──
    // 固定 3s 不够：消费者 backoff 指数增长（1,2,4,8,16,32,60s），长任务
    // 结束时 backoff 已大，3s 内不会扫描到新 pending。改为轮询 pending
    // 清空（上限 60s，1s 间隔）——pending 目录下的 dead/ 子目录不计。
    if let Some((handle, al_handle, compile_handle, cancel)) = lianshan_handle {
        tracing::info!("--with-lianshan: waiting for Lianshan Consumer to drain pending/");
        let pending_dir = data_root.join("pending");
        for _ in 0..60 {
            let mut remaining = 0usize;
            if let Ok(mut rd) = tokio::fs::read_dir(&pending_dir).await {
                while let Ok(Some(entry)) = rd.next_entry().await {
                    let is_dead = entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("dead");
                    if entry.file_type().await.map(|t| t.is_file()).unwrap_or(false)
                        && !is_dead
                    {
                        remaining += 1;
                    }
                }
            }
            if remaining == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        // ── 等待 compile 队列清空（S4 编译执行器）──
        // 修复：pending 清空后立即 cancel 会抢在 compile runner 的空闲窗口前
        // 杀掉它，导致 compile/ 队列永远不被消费（S4 断链）。编译任务 = 完整
        // 周易执行（较慢），故给 300s 上限、1s 间隔轮询 compile 目录清空。
        tracing::info!("--with-lianshan: waiting for compile queue to drain");
        let compile_dir = data_root.join("compile");
        for _ in 0..300 {
            let mut remaining = 0usize;
            if let Ok(mut rd) = tokio::fs::read_dir(&compile_dir).await {
                while let Ok(Some(entry)) = rd.next_entry().await {
                    if entry.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
                        remaining += 1;
                    }
                }
            }
            if remaining == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        cancel.cancel();
        let _ = handle.await;
        let _ = al_handle.await;
        let _ = compile_handle.await;
        tracing::info!("--with-lianshan: Lianshan Consumer stopped");
    }

    Ok(())
}

async fn cmd_init() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let data_root = std::path::PathBuf::from(&config.data_root);

    // Create directory structure: .taiji/pending/dead/  .taiji/tasks/
    let dirs = [
        data_root.join("pending").join("dead"),
        data_root.join("tasks"),
    ];
    for dir in &dirs {
        std::fs::create_dir_all(dir)?;
    }

    // Initialize 归藏 knowledge store.
    let knowledge_dir = std::path::PathBuf::from(&config.knowledge.data_dir);
    match taiji::infra::knowledge::GuizangClient::new(&knowledge_dir).await {
        Ok(_) => {
            // V44：旧 `{model_key}/` 分区资产合并回根（幂等；init 是人工可重跑步骤，
            // 失败仅提示不中断——build_engine 运行时迁移失败才上抛）。
            if let Err(e) = taiji::infra::knowledge::migrate_from_partitioned(&knowledge_dir).await
            {
                println!("⚠ legacy partition merge to knowledge root failed: {e}");
            }
            // V42：yang/yin 对偶目录迁移（幂等，Blueprint §6.1）
            if let Err(e) = taiji::infra::knowledge::migrate_to_yang_yin(&knowledge_dir).await {
                println!("⚠ yang/yin directory migration failed: {e}");
            }
            println!(
                "✓ 归藏 knowledge store initialised at {}",
                knowledge_dir.display()
            );
        }
        Err(e) => {
            println!("⚠ 归藏 knowledge store initialisation failed: {e}");
            println!("  The system will run with a sparse (empty) knowledge store");
        }
    }

    println!("✓ taiji workspace initialized at {}", config.data_root);
    Ok(())
}

/// V44：种子复制——把指定模型分区的活跃种子资产（prompts/ + verifications/）
/// 恢复到知识根（去分区化恢复工具）。stats/models 不复制（学习单元从零积累）。
async fn cmd_seed(
    model_key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let knowledge_dir = std::path::PathBuf::from(&config.knowledge.data_dir);

    let report =
        taiji::infra::knowledge::seed_partition(&knowledge_dir, model_key).await?;

    println!(
        "✓ restored {} active seed assets from partition '{}' ({} skipped existing, {} pruned excluded)",
        report.copied,
        model_key,
        report.skipped,
        report.pruned_skipped
    );
    println!("  models/ 统计与贝叶斯后验未复制——学习单元从零积累（Blueprint §6.1）");
    Ok(())
}

async fn cmd_trace(
    task_id: &str,
    tree: bool,
    tail: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let task_dir = std::path::PathBuf::from(&config.data_root)
        .join("tasks")
        .join(task_id);

    if !task_dir.exists() {
        eprintln!(
            "Error: task '{}' not found at {:?}",
            task_id,
            task_dir.display()
        );
        std::process::exit(1);
    }

    let writer = taiji::infra::trace::TraceWriter::new(&task_dir);
    let mut records = if tree {
        taiji::infra::trace::TraceWriter::read_tree(&task_dir)?
    } else {
        writer.read()?
    };

    // Tail: keep only the last N records（批19 P2 修复：保序，与 mcp
    // handle_trace 的 split_off 一致——旧 rev().take(n) 输出倒序）。
    if let Some(n) = tail {
        let start = records.len().saturating_sub(n);
        records = records.into_iter().skip(start).collect();
    }

    if records.is_empty() {
        println!("No trace records found for task '{}'", task_id);
    } else {
        println!(
            "Trace for task '{}' ({} records):",
            task_id,
            records.len()
        );
        for record in &records {
            let ts_trimmed = &record.ts[..std::cmp::min(record.ts.len(), 19)];
            let input_str = serde_json::to_string(&record.input).unwrap_or_default();
            println!(
                "  [{ts}] [{phase}] {input}",
                ts = ts_trimmed,
                phase = record.phase,
                input = input_str,
            );
        }
    }

    Ok(())
}

fn cmd_list() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let tasks_dir = std::path::PathBuf::from(&config.data_root).join("tasks");

    if !tasks_dir.exists() {
        println!("No tasks yet. Run `taiji run <description>` first.");
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&tasks_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        println!("No tasks found.");
    } else {
        println!("Tasks ({} total):", entries.len());
        for entry in &entries {
            let task_id = entry.file_name().to_string_lossy().to_string();
            let meta_path = entry.path().join("meta.json");
            let status = if meta_path.exists() {
                match std::fs::read_to_string(&meta_path) {
                    Ok(content) => match serde_json::from_str::<taiji::types::task::Task>(&content)
                    {
                        Ok(task) => format!("{:?}", task.status),
                        Err(_) => "Unknown".into(),
                    },
                    Err(_) => "Unknown".into(),
                }
            } else {
                "No meta".into()
            };
            println!("  {} [{}]", task_id, status);
        }
    }

    Ok(())
}

/// `taiji builtin <name>` — Rust 种子层 syscall 原语（V52）。
///
/// 资产层 Python skill 经 `subprocess.run(["taiji","builtin",<name>,"--args",<json>])`
/// 调用 Rust builtin（用户态调 syscall）。输出 JSON 到 stdout。
async fn cmd_builtin(
    name: &str,
    args: Option<String>,
    task_dir: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let runner = taiji::agents::tools::skills::lookup_builtin(name).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("unknown builtin '{name}' (known: read/write/bash/search/webfetch)"),
        )
    })?;
    let args_val: serde_json::Value = match args {
        Some(s) => serde_json::from_str(&s).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("--args 不是合法 JSON: {e}"),
            )
        })?,
        None => serde_json::json!({}),
    };
    let dir = std::path::PathBuf::from(task_dir.unwrap_or_else(|| ".".to_string()));
    let result = runner.call(&dir, &args_val).await?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// `taiji skill <id>` — 资产层 Python skill 执行体（V53 skill 嵌套 skill，用户态调用户态）。
///
/// 与 `taiji builtin` 正交：builtin = 用户态调 syscall（Rust 种子层）；
/// skill = 用户态调库函数（资产层 Python skill）。循环/深度护栏经
/// `TAIJI_SKILL_CHAIN` 环境变量（JSON 数组）传递调用链：
/// ① 循环检测——id 已在链中 → 拒绝；② 深度限制——链长 ≥ max_depth → 拒绝。
async fn cmd_skill(
    id: &str,
    args: Option<String>,
    task_dir: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // ── 1. 读调用链（TAIJI_SKILL_CHAIN；不存在 = 顶层调用）──
    let mut chain: Vec<String> = match std::env::var("TAIJI_SKILL_CHAIN") {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // ── 2. 循环/深度护栏（§V53 定论）──
    if chain.iter().any(|c| c == id) {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("skill '{id}' cycle detected: {} → {id}", chain.join(" → ")),
        )));
    }
    let config = load_config()?;
    let max_depth = config.runtime.max_depth as usize;
    if chain.len() >= max_depth {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "skill '{id}' nesting depth {} exceeds max_depth {max_depth}",
                chain.len()
            ),
        )));
    }

    // ── 3. 解析 args + task_dir ──
    let args_val: serde_json::Value = match args {
        Some(s) => serde_json::from_str(&s).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("--args 不是合法 JSON: {e}"),
            )
        })?,
        None => serde_json::json!({}),
    };
    let dir = std::path::PathBuf::from(task_dir.unwrap_or_else(|| ".".to_string()));

    // ── 4. 四类扫描找 id 的 Python 执行体 ──
    use taiji::types::verification::{SkillCategory, SkillKind};
    let knowledge_dir = std::path::PathBuf::from(&config.knowledge.data_dir);
    let guizang = taiji::infra::knowledge::GuizangClient::new(&knowledge_dir).await?;
    let mut script_path: Option<std::path::PathBuf> = None;
    for category in [
        SkillCategory::Exec,
        SkillCategory::Orch,
        SkillCategory::Verify,
        SkillCategory::Converge,
    ] {
        let catalog = taiji::infra::skill_catalog::load_skill_catalog(
            &guizang,
            category,
            taiji::infra::skill_catalog::ToolProfile::Full,
        )
        .await?;
        if let Some(skill) = catalog.iter().find(|s| s.id == id) {
            if let Some(impl_) = skill
                .implementations
                .iter()
                .find(|i| i.kind == SkillKind::Python)
            {
                let p = guizang.skill_script_path(category, &skill.id, &impl_.target);
                if p.exists() {
                    script_path = Some(p);
                    break;
                }
            }
        }
    }
    let Some(script_path) = script_path else {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("unknown skill '{id}' (asset-layer Python skill not found)"),
        )));
    };

    // ── 5. 追加 id → 执行（chain 注入子进程环境）──
    chain.push(id.to_string());
    let result =
        taiji::orchestration::python_engine::run_python_skill(&script_path, &args_val, &dir, &chain)
            .await?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// V45 曾用名数据迁移：扫描任务目录，将旧值（FittingDone / BackToTpn /
/// fitting_system_prompt）替换为新值（YangDone / BackToZhouyi /
/// yang_system_prompt）。幂等，可重复执行。
async fn cmd_migrate() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let data_root = std::path::PathBuf::from(&config.data_root);
    let touched =
        taiji::infra::migrate::migrate_all(&data_root).await?;
    println!("taiji migrate:");
    println!("  Scanned tasks root: {}", data_root.join("tasks").display());
    println!("  Migrated task dirs: {touched}");
    if touched == 0 {
        println!("  （无旧值需迁移——任务目录为空或已是最新格式）");
    }
    Ok(())
}

fn cmd_status() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let data_root = std::path::PathBuf::from(&config.data_root);

    println!("taiji status:");
    println!("  Workspace: {}", config.workspace);
    println!("  Data root: {}", data_root.display());
    println!(
        "  归藏 knowledge store: {}",
        config.knowledge.data_dir
    );
    println!(
        "  LLM provider: {} / {}",
        config.llm.default_provider, config.llm.default_model
    );
    println!("  Max depth: {}", config.runtime.max_depth);
    println!("  Max rounds: {}", config.runtime.max_rounds);

    // Count pending/dead items in the Lianshan pending queue.
    let pending_dir = data_root.join("pending");
    let pending_count = if pending_dir.exists() {
        std::fs::read_dir(&pending_dir)
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    } else {
        0
    };

    // Count completed task directories.
    let tasks_dir = data_root.join("tasks");
    let task_count = if tasks_dir.exists() {
        std::fs::read_dir(&tasks_dir)
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    } else {
        0
    };

    println!("  Pending Lianshan tasks: {}", pending_count);
    println!("  Completed tasks: {}", task_count);

    Ok(())
}

async fn cmd_mcp() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;

    // Provider registry (LLM clients).
    let factory = build_engine(&config).await?;

    // Start MCP server (blocks until stdin closes).
    let server = taiji::mcp::server::TaijiMcpServer::new(factory);
    server.serve().await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Shared engine construction
// ---------------------------------------------------------------------------

/// Build the full engine component set (provider registry → 归藏 client →
/// safety hook → worker pool → agent factory).
///
/// Shared by `run`, `mcp` and `serve` — the single initialization path.
async fn build_engine(
    config: &taiji::infra::config::TaijiConfig,
) -> Result<Arc<taiji::agents::factory::AgentFactory>, Box<dyn std::error::Error>> {
    let providers = Arc::new(taiji::infra::provider::ProviderRegistry::new(config)?);

    let knowledge_dir = std::path::PathBuf::from(&config.knowledge.data_dir);
    let guizang = match taiji::infra::knowledge::GuizangClient::new(&knowledge_dir).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::warn!("归藏 knowledge store unavailable, creating sparse client: {e}");
            Arc::new(taiji::infra::knowledge::GuizangClient::new_sparse(&knowledge_dir).await?)
        }
    };

    // V44 去分区化：旧 `{model_key}/` 分区资产合并回根（幂等——目标存在即跳过）。
    // 无降级原则（AGENTS.md §23）：迁移是数据完整性操作，失败上抛（部分迁移
    // 会导致根/分区资产不一致，检索结果不可信）。
    if let Err(e) = taiji::infra::knowledge::migrate_from_partitioned(&knowledge_dir).await {
        tracing::error!(
            error = %e,
            "legacy partition merge to knowledge root failed"
        );
        return Err(Box::new(e));
    }

    let safety_hook = Arc::new(taiji::hooks::safety::SafetyHook::new(&config.safety));
    let worker_pool = Arc::new(taiji::orchestration::worker_pool::WorkerPool::new(
        config.runtime.max_concurrent_agents,
    ));
    let constraint_engine =
        Arc::new(taiji::orchestration::constraint_engine::ConstraintEngine::new());
    let trigger_engine =
        Arc::new(taiji::orchestration::trigger_engine::SkillTriggerEngine::new());

    Ok(Arc::new(taiji::agents::factory::AgentFactory::new(
        guizang,
        providers,
        config.clone(),
        safety_hook,
        worker_pool,
        constraint_engine,
        trigger_engine,
    )))
}

// ---------------------------------------------------------------------------
// `taiji serve` — pure-Web frontend mode
// ---------------------------------------------------------------------------

/// WebSocket bridge port (taiji pinyin initial digits), loopback only.
const WS_PORT: u16 = 17890;

/// Serve the taiji-web frontend:
/// 1. Engine init (same chain as `run` / `mcp`).
/// 2. WebSocket bridge on 127.0.0.1:17890 + global event bus.
/// 3. HTTP static hosting of `taiji-web/dist/` on 127.0.0.1:<port>.
/// 4. Auto-open the browser (unless `--no-open`).
async fn cmd_serve(port: u16, no_open: bool) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let factory = build_engine(&config).await?;
    let data_root = std::path::PathBuf::from(&config.data_root);

    // ── WebSocket bridge + global event bus ────────────────────────────
    let ws_server = std::sync::Arc::new(taiji::ws::server::WsServer::new(WS_PORT));
    let serve_state = std::sync::Arc::new(taiji::ws::handler::ServeState {
        factory: factory.clone(),
        config: config.clone(),
        data_root: data_root.clone(),
    });
    ws_server.set_state(serve_state);
    if let Err(e) = taiji::orchestration::event_bus::init_event_bus(ws_server.clone()) {
        tracing::warn!("事件总线初始化失败: {e}");
    }
    ws_server.start().await?;

    // ── HTTP static hosting of the frontend build output ───────────────
    let dist_dir = std::path::PathBuf::from("taiji-web/dist");
    if !dist_dir.join("index.html").exists() {
        eprintln!(
            "⚠ 前端构建产物不存在于 {}，请先运行: cd taiji-web && npm run build",
            dist_dir.display()
        );
    }
    let app = axum::Router::new().fallback_service(
        tower_http::services::ServeDir::new(&dist_dir).append_index_html_on_directories(true),
    );

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("✓ taiji serve 已启动");
    println!("  HTTP 前端:   http://127.0.0.1:{port}");
    println!("  WS 事件桥:   ws://127.0.0.1:{WS_PORT}");
    println!("  数据根目录:  {}", data_root.display());
    println!("  按 Ctrl+C 退出");

    if !no_open {
        open_browser(&format!("http://127.0.0.1:{port}"));
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// Open the system browser on Linux via `xdg-open`. Failure is advisory
/// (the URL is printed above; users may open it manually).
fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
            tracing::warn!("自动打开浏览器失败(请手动访问 {url}): {e}");
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        tracing::info!("请手动访问 {url}");
    }
}

// ---------------------------------------------------------------------------
// Configuration loading
// ---------------------------------------------------------------------------

/// Load `TaijiConfig` from well-known paths.
///
/// The config file is the **sole** source of configuration. No environment
/// variables are consulted. An empty `api_key` is a hard error.
///
/// Search order:
///   1. `.taiji/config.json`
///   2. `taiji.config.json`
fn load_config() -> Result<taiji::infra::config::TaijiConfig, Box<dyn std::error::Error>> {
    let config_paths = [".taiji/config.json", "taiji.config.json"];
    let mut last_err = "no config file found (looked for .taiji/config.json and taiji.config.json)".to_string();

    for path in &config_paths {
        let p = std::path::Path::new(path);
        if p.exists() {
            let content = std::fs::read_to_string(p)?;
            let config: taiji::infra::config::TaijiConfig =
                serde_json::from_str(&content)?;
            if config.llm.api_key.is_empty() {
                last_err = format!("{path}: llm.api_key is empty — the config file is the sole source of credentials");
                continue;
            }
            return Ok(config);
        }
    }

    Err(Box::new(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        last_err,
    )))
}
