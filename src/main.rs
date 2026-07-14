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
    },
    /// Initialize workspace (.taiji/ + 理络 knowledge store)
    Init,
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
    /// Show DMN / cognition status
    Status,
    /// Start MCP server for tool integration
    Mcp,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Run { description } => cmd_run(description).await?,
        Command::Init => cmd_init().await?,
        Command::Trace {
            task_id,
            tree,
            tail,
        } => cmd_trace(&task_id, tree, tail).await?,
        Command::List => cmd_list()?,
        Command::Status => cmd_status()?,
        Command::Mcp => cmd_mcp().await?,
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

async fn cmd_run(description: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let desc = description.join(" ");
    let config = load_config()?;

    // Provider registry (LLM clients).
    let providers = Arc::new(taiji::infra::provider::ProviderRegistry::new(&config)?);

    // 理络 client — file-system cognitive knowledge warehouse.
    let knowledge_dir = std::path::PathBuf::from(&config.knowledge.data_dir);
    let liluo = match taiji::infra::knowledge::LiluoClient::new(&knowledge_dir).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::warn!("理络 knowledge store unavailable, creating sparse client: {e}");
            Arc::new(taiji::infra::knowledge::LiluoClient::new_sparse(&knowledge_dir).await?)
        }
    };

    // Infrastructure engines.
    let safety_hook = Arc::new(taiji::hooks::safety::SafetyHook::new(&config.safety));
    let worker_pool = Arc::new(taiji::orchestration::worker_pool::WorkerPool::new(
        config.runtime.max_concurrent_agents,
    ));
    let constraint_engine =
        Arc::new(taiji::orchestration::constraint_engine::ConstraintEngine::new());
    let trigger_engine =
        Arc::new(taiji::orchestration::trigger_engine::SkillTriggerEngine::new());

    // Agent factory — creates transient Rig Agents on demand.
    let factory = Arc::new(taiji::agents::factory::AgentFactory::new(
        liluo,
        providers,
        config.clone(),
        safety_hook,
        worker_pool,
        constraint_engine,
        trigger_engine,
    ));

    // Execute task via RecursiveRunner.
    let runner = taiji::orchestration::runner::RecursiveRunner::new(factory, config);
    let result = runner.execute(&desc).await?;

    println!("✓ Task completed: {}", result.task_id);
    println!("  Content: {}", result.content);
    println!("  Tools used: {}", result.tools_used.join(", "));

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

    // Initialize 理络 knowledge store.
    let knowledge_dir = std::path::PathBuf::from(&config.knowledge.data_dir);
    match taiji::infra::knowledge::LiluoClient::new(&knowledge_dir).await {
        Ok(_) => {
            println!(
                "✓ 理络 knowledge store initialised at {}",
                knowledge_dir.display()
            );
        }
        Err(e) => {
            println!("⚠ 理络 knowledge store initialisation failed: {e}");
            println!("  The system will run with a sparse (empty) knowledge store");
        }
    }

    println!("✓ taiji workspace initialized at {}", config.data_root);
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

fn cmd_status() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;
    let data_root = std::path::PathBuf::from(&config.data_root);

    println!("taiji status:");
    println!("  Workspace: {}", config.workspace);
    println!("  Data root: {}", data_root.display());
    println!(
        "  理络 knowledge store: {}",
        config.knowledge.data_dir
    );
    println!(
        "  LLM provider: {} / {}",
        config.llm.default_provider, config.llm.default_model
    );
    println!("  Max depth: {}", config.runtime.max_depth);
    println!("  Max rounds: {}", config.runtime.max_rounds);

    // Count pending/dead items in the DMN pending queue.
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

    println!("  Pending DMN tasks: {}", pending_count);
    println!("  Completed tasks: {}", task_count);

    Ok(())
}

async fn cmd_mcp() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config()?;

    // Provider registry (LLM clients).
    let providers = Arc::new(taiji::infra::provider::ProviderRegistry::new(&config)?);

    // 理络 client — file-system cognitive knowledge warehouse.
    let knowledge_dir = std::path::PathBuf::from(&config.knowledge.data_dir);
    let liluo = match taiji::infra::knowledge::LiluoClient::new(&knowledge_dir).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::warn!("理络 knowledge store unavailable, creating sparse client: {e}");
            Arc::new(taiji::infra::knowledge::LiluoClient::new_sparse(&knowledge_dir).await?)
        }
    };

    // Infrastructure engines.
    let safety_hook = Arc::new(taiji::hooks::safety::SafetyHook::new(&config.safety));
    let worker_pool = Arc::new(taiji::orchestration::worker_pool::WorkerPool::new(
        config.runtime.max_concurrent_agents,
    ));
    let constraint_engine =
        Arc::new(taiji::orchestration::constraint_engine::ConstraintEngine::new());
    let trigger_engine =
        Arc::new(taiji::orchestration::trigger_engine::SkillTriggerEngine::new());

    // Agent factory.
    let factory = Arc::new(taiji::agents::factory::AgentFactory::new(
        liluo,
        providers,
        config.clone(),
        safety_hook,
        worker_pool,
        constraint_engine,
        trigger_engine,
    ));

    // Start MCP server (blocks until stdin closes).
    let server = taiji::mcp::server::TaijiMcpServer::new(factory);
    server.serve().await?;

    Ok(())
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
