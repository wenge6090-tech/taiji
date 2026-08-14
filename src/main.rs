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

    // ── V33/MVP-2: --with-lianshan 激活 Lianshan Consumer（被动学习 — BCP §6.4/§8.23）──
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
            factory.guizang.clone(),
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
            // V42：yang/yin 对偶目录迁移（幂等，BCP §10.1）
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

/// V39：种子复制——把源分区（默认 = 配置的默认模型分区）的活跃种子资产
/// （prompts/ + verifications/）复制到目标模型分区。stats/models 不复制
/// （每个分区是独立学习单元，新模型从零积累——BCP §6.1）。
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
    println!("  models/ 统计与贝叶斯后验未复制——学习单元从零积累（BCP §10.1）");
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

    // Tail: keep only the last N records.
    if let Some(n) = tail {
        records = records.into_iter().rev().take(n).collect();
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
