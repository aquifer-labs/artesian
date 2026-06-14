// SPDX-License-Identifier: Apache-2.0

use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use brunnr_core::{
    Agent, AgentBinding, BrunnrConfig, MemoryBackendKind, MemoryConfig, Mode, Role, SpawnRequest,
};
use brunnr_process_agent::{ProcessAgent, ProcessAgentConfig};
use clap::{Parser, Subcommand, ValueEnum};
use mimisbrunnr::{
    backfill_directory, recover_after_compaction, MemoryBackend, MemoryQuery, MemoryScope,
    MemoryTier, MuninnAnchorStore, SessionAnchor, StoreMemory,
};
use serde_json::{json, Value};
use thingr::{
    ClaimRequest, CommandVerifier, FilesTaskStore, NewTask, TaskKind, TaskStore, VectorTaskStore,
    Verifier, VerifierGate,
};
use toml_edit::{value, Array, DocumentMut, Item, Table};

const DEFAULT_CONFIG: &str = "brunnr.toml";
const MCP_SERVER_NAME: &str = "brunnr-memory";
const MCP_TOOL_HINT: &str =
    "ALWAYS search the project memory before non-trivial work; store durable, reusable learnings.";

mod runtime;
use runtime::{build_orchestrator, load_config, open_memory_backend};

#[derive(Debug, Parser)]
#[command(name = "brunnr", about = "Multi-agent context orchestration")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init {
        #[arg(long, default_value = ".brunnr")]
        memory_root: PathBuf,
        #[arg(long, value_enum, default_value_t = BackendArg::Files)]
        backend: BackendArg,
        #[arg(long, default_value = "brunnr-memory")]
        collection: String,
        #[arg(long, env = "QDRANT_URL")]
        qdrant_url: Option<String>,
        #[arg(long, default_value = "QDRANT_API_KEY")]
        qdrant_api_key_env: String,
        #[arg(long)]
        non_interactive: bool,
        #[arg(long, default_value_t = true)]
        register_mcp: bool,
    },
    Spawn {
        role: String,
        agent: String,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
    },
    Run {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        once: bool,
    },
    Orchestrate {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        once: bool,
    },
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Backfill {
        directory: PathBuf,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    Add {
        title: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long = "blocker")]
        blockers: Vec<String>,
        #[arg(long, value_enum, default_value_t = TaskKindArg::Primitive)]
        kind: TaskKindArg,
        #[arg(long, default_value = "worker")]
        role: String,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
    },
    List {
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
    },
    Claim {
        id: Option<String>,
        #[arg(long, default_value = "worker")]
        claimant: String,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
    },
    Done {
        id: String,
        #[arg(long = "verify-command")]
        verify_commands: Vec<String>,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
    },
    Find {
        query: String,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TaskKindArg {
    Compound,
    Primitive,
}

impl From<TaskKindArg> for TaskKind {
    fn from(value: TaskKindArg) -> Self {
        match value {
            TaskKindArg::Compound => Self::Compound,
            TaskKindArg::Primitive => Self::Primitive,
        }
    }
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    Store {
        content: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        node_id: Option<String>,
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        task_id: Option<String>,
        #[arg(long)]
        user_id: Option<String>,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
    },
    Find {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        node_id: Option<String>,
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        task_id: Option<String>,
        #[arg(long)]
        user_id: Option<String>,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
    },
    Anchor {
        #[command(subcommand)]
        command: AnchorCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AnchorCommand {
    Get {
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
    },
    Set {
        #[arg(long)]
        current_task: String,
        #[arg(long)]
        next_step: String,
        #[arg(long)]
        plan_pointer: Option<String>,
        #[arg(long = "decision")]
        last_decisions: Vec<String>,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
    },
    Recover {
        #[arg(long, default_value_t = 5)]
        limit: usize,
        #[arg(long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
        #[arg(long, default_value = ".brunnr")]
        root: PathBuf,
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendArg {
    Files,
    SqliteVec,
    Qdrant,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScopeArg {
    Shared,
    Agent,
    Session,
    Task,
}

impl From<ScopeArg> for MemoryScope {
    fn from(value: ScopeArg) -> Self {
        match value {
            ScopeArg::Shared => Self::Shared,
            ScopeArg::Agent => Self::Agent,
            ScopeArg::Session => Self::Session,
            ScopeArg::Task => Self::Task,
        }
    }
}

impl From<BackendArg> for MemoryBackendKind {
    fn from(value: BackendArg) -> Self {
        match value {
            BackendArg::Files => Self::Files,
            BackendArg::SqliteVec => Self::SqliteVec,
            BackendArg::Qdrant => Self::Qdrant,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init {
            memory_root,
            backend,
            collection,
            qdrant_url,
            qdrant_api_key_env,
            non_interactive,
            register_mcp,
        } => init(
            memory_root,
            backend,
            collection,
            qdrant_url,
            qdrant_api_key_env,
            non_interactive,
            register_mcp,
        ),
        Command::Spawn {
            role,
            agent,
            args,
            timeout_seconds,
        } => spawn(&role, &agent, args, timeout_seconds).await,
        Command::Run {
            config,
            root,
            dry_run,
            once,
        }
        | Command::Orchestrate {
            config,
            root,
            dry_run,
            once,
        } => run_orchestrator(config, root, dry_run, once).await,
        Command::Memory { command } => memory(command).await,
        Command::Task { command } => task(command).await,
        Command::Backfill {
            directory,
            config,
            root,
            backend,
        } => backfill(directory, config, root, backend).await,
    }
}

async fn task(command: TaskCommand) -> Result<()> {
    match command {
        TaskCommand::Add {
            title,
            id,
            description,
            blockers,
            kind,
            role,
            config,
            root,
            backend,
        } => {
            let source = FilesTaskStore::new(&root);
            let memory = open_backend_for_command(&config, root, backend)?;
            let store = VectorTaskStore::new(source, memory);
            let mut task = NewTask::primitive(title);
            task.id = id;
            task.description = description;
            task.blockers = blockers;
            task.kind = kind.into();
            task.role = Role::from_str(&role)?;
            let task = store.create(task).await?;
            println!("created task id={} status=todo", task.id);
        }
        TaskCommand::List { root } => {
            let store = FilesTaskStore::new(root);
            for task in store.list().await? {
                println!("{}\t{:?}\t{}", task.id, task.status, task.title);
            }
        }
        TaskCommand::Claim { id, claimant, root } => {
            let store = FilesTaskStore::new(root);
            match store
                .claim(ClaimRequest {
                    task_id: id,
                    claimant,
                })
                .await?
            {
                Some(task) => println!(
                    "claimed task id={} claimant={}",
                    task.id,
                    task.claimed_by.unwrap_or_default()
                ),
                None => println!("no dispatch-eligible task"),
            }
        }
        TaskCommand::Done {
            id,
            verify_commands,
            root,
        } => {
            let store = FilesTaskStore::new(root);
            let verifiers: Vec<Arc<dyn Verifier>> = verify_commands
                .into_iter()
                .map(|command| Arc::new(CommandVerifier::new(command.clone(), command)) as _)
                .collect();
            let gate = VerifierGate::new(verifiers);
            let task = gate.mark_done(&store, &id).await?;
            println!("completed task id={} status=done", task.id);
        }
        TaskCommand::Find {
            query,
            config,
            root,
            backend,
        } => {
            let source = FilesTaskStore::new(&root);
            let memory = open_backend_for_command(&config, root, backend)?;
            let store = VectorTaskStore::new(source, memory);
            for task in store.find(&query).await? {
                println!("{}\t{:?}\t{}", task.id, task.status, task.title);
            }
        }
    }
    Ok(())
}

fn init(
    memory_root: PathBuf,
    backend: BackendArg,
    collection: String,
    qdrant_url: Option<String>,
    qdrant_api_key_env: String,
    _non_interactive: bool,
    register_mcp: bool,
) -> Result<()> {
    fs::create_dir_all(memory_root.join("memory"))
        .with_context(|| format!("create memory root {}", memory_root.display()))?;
    let agents = detect_agents();
    let config = BrunnrConfig {
        mode: brunnr_core::Mode::Memory,
        memory: MemoryConfig {
            backend: backend.into(),
            root: memory_root.display().to_string(),
            collection,
            qdrant_url,
            qdrant_api_key_env: Some(qdrant_api_key_env),
        },
        agents,
        coordination: Default::default(),
    };
    let config_path = Path::new(DEFAULT_CONFIG);
    if !config_path.exists() {
        fs::write(config_path, config.to_toml()?)?;
    }
    if register_mcp {
        write_mcp_registrations(&env::current_dir()?.join(config_path))?;
    }
    println!(
        "initialized Brunnr memory mode at {}",
        memory_root.display()
    );
    Ok(())
}

async fn spawn(role: &str, agent: &str, args: Vec<String>, timeout_seconds: u64) -> Result<()> {
    let role = Role::from_str(role)?;
    let cwd = env::current_dir()?;
    let request = SpawnRequest {
        role,
        agent: agent.to_string(),
        model: None,
        working_dir: Some(cwd.display().to_string()),
    };
    let process = ProcessAgent::new(
        ProcessAgentConfig::new(agent)
            .with_args(args)
            .with_working_dir(cwd)
            .with_timeout(Duration::from_secs(timeout_seconds)),
    );
    let session = process.spawn(request.clone()).await?;
    let response = process
        .send(
            &session,
            brunnr_core::AgentMessage {
                content: String::new(),
            },
        )
        .await?;
    println!(
        "spawn completed: role={} alias={} agent={} cwd={}",
        request.role.canonical_alias(),
        request.role.norse_alias(),
        request.agent,
        request.working_dir.as_deref().unwrap_or(".")
    );
    if !response.content.is_empty() {
        print!("{}", response.content);
    }
    Ok(())
}

async fn run_orchestrator(
    config_path: PathBuf,
    root: Option<PathBuf>,
    dry_run: bool,
    once: bool,
) -> Result<()> {
    let config = load_config(&config_path)?;
    if !matches!(config.mode, Mode::Orchestrate | Mode::Full) {
        bail!(
            "orchestration is disabled for mode {:?}; use orchestrate or full",
            config.mode
        );
    }
    let root = root.unwrap_or_else(|| PathBuf::from(&config.memory.root));
    let repo_root = env::current_dir()?;
    let mut orchestrator = build_orchestrator(config, root, repo_root, dry_run)?;
    if once {
        let report = orchestrator.run_once().await?;
        println!(
            "orchestrator tick: dispatched={} completed={} blocked={} idle={}",
            report.dispatched, report.completed, report.blocked, report.idle
        );
    } else {
        let report = orchestrator.run_until_idle(100).await?;
        println!(
            "orchestrator stopped: ticks={} completed={} blocked={} events={}",
            report.ticks,
            report.completed,
            report.blocked,
            orchestrator.run_log().events.len()
        );
    }
    Ok(())
}

async fn memory(command: MemoryCommand) -> Result<()> {
    match command {
        MemoryCommand::Store {
            content,
            tags,
            node_id,
            scope,
            agent_id,
            session_id,
            task_id,
            user_id,
            config,
            root,
            backend,
        } => {
            let backend = open_backend_for_command(&config, root, backend)?;
            let record = backend
                .store(StoreMemory {
                    content,
                    tags,
                    metadata: Default::default(),
                    tier: MemoryTier::L1Atom,
                    node_id,
                    created_at: None,
                    scope: scope.map(Into::into),
                    agent_id,
                    session_id,
                    task_id,
                    user_id,
                })
                .await?;
            println!("stored memory id={} node_id={}", record.id, record.node_id);
        }
        MemoryCommand::Find {
            query,
            limit,
            node_id,
            scope,
            agent_id,
            session_id,
            task_id,
            user_id,
            config,
            root,
            backend,
        } => {
            let backend = open_backend_for_command(&config, root, backend)?;
            let mut memory_query = MemoryQuery::new(query).with_limit(limit);
            memory_query.node_id = node_id;
            memory_query.scope = scope.map(Into::into);
            memory_query.agent_id = agent_id;
            memory_query.session_id = session_id;
            memory_query.task_id = task_id;
            memory_query.user_id = user_id;
            for hit in backend.find(memory_query).await? {
                println!(
                    "{:.4}\t{}\t{}\t{}",
                    hit.score, hit.record.id, hit.record.node_id, hit.record.content
                );
            }
        }
        MemoryCommand::Anchor { command } => anchor(command).await?,
    }
    Ok(())
}

async fn anchor(command: AnchorCommand) -> Result<()> {
    match command {
        AnchorCommand::Get { config, root } => {
            let memory = memory_config_for_command(&config, root, None)?;
            let store = MuninnAnchorStore::new(&memory.root);
            println!("{}", serde_json::to_string_pretty(&store.get().await?)?);
        }
        AnchorCommand::Set {
            current_task,
            next_step,
            plan_pointer,
            last_decisions,
            config,
            root,
        } => {
            let memory = memory_config_for_command(&config, root, None)?;
            let store = MuninnAnchorStore::new(&memory.root);
            let mut anchor = SessionAnchor::new(current_task, next_step);
            anchor.plan_pointer = plan_pointer;
            anchor.last_decisions = last_decisions;
            let written = store.set(anchor).await?;
            println!("{}", serde_json::to_string_pretty(&written)?);
        }
        AnchorCommand::Recover {
            limit,
            config,
            root,
            backend,
        } => {
            let memory = memory_config_for_command(&config, root, backend)?;
            let anchor_store = MuninnAnchorStore::new(&memory.root);
            let backend = open_memory_backend(&memory)?;
            let recovered =
                recover_after_compaction(&anchor_store, backend.as_ref(), limit).await?;
            println!("{}", serde_json::to_string_pretty(&recovered)?);
        }
    }
    Ok(())
}

async fn backfill(
    directory: PathBuf,
    config: PathBuf,
    root: PathBuf,
    backend: Option<BackendArg>,
) -> Result<()> {
    let backend = open_backend_for_command(&config, root, backend)?;
    let stats = backfill_directory(backend.as_ref(), directory).await?;
    println!(
        "backfill scanned={} imported={} skipped_duplicates={}",
        stats.scanned, stats.imported, stats.skipped_duplicates
    );
    Ok(())
}

fn open_backend_for_command(
    config_path: &Path,
    root: PathBuf,
    backend: Option<BackendArg>,
) -> Result<Arc<dyn MemoryBackend>> {
    let config = memory_config_for_command(config_path, root, backend)?;
    open_memory_backend(&config)
}

fn memory_config_for_command(
    config_path: &Path,
    root: PathBuf,
    backend: Option<BackendArg>,
) -> Result<MemoryConfig> {
    let config = if config_path.exists() {
        let text = fs::read_to_string(config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        let config = BrunnrConfig::from_toml(&text)
            .with_context(|| format!("parse {}", config_path.display()))?;
        if config.mode != Mode::Memory {
            bail!("memory commands require mode = memory");
        }
        config.memory
    } else {
        MemoryConfig {
            backend: backend.unwrap_or(BackendArg::Files).into(),
            root: root.display().to_string(),
            collection: "brunnr-memory".to_string(),
            qdrant_url: env::var("QDRANT_URL").ok(),
            qdrant_api_key_env: Some("QDRANT_API_KEY".to_string()),
        }
    };
    let config = if let Some(backend) = backend {
        MemoryConfig {
            backend: backend.into(),
            ..config
        }
    } else {
        config
    };
    Ok(config)
}

fn write_mcp_registrations(config_path: &Path) -> Result<()> {
    let config_path = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());
    write_claude_mcp(config_path.as_path())?;
    write_codex_mcp(config_path.as_path())?;
    write_zed_mcp(config_path.as_path())?;
    Ok(())
}

fn mcp_args(config_path: &Path) -> Vec<String> {
    vec!["--config".to_string(), config_path.display().to_string()]
}

fn write_claude_mcp(config_path: &Path) -> Result<()> {
    let path = Path::new(".mcp.json");
    let mut root = read_json_object(path)?;
    let server = json!({
        "command": "brunnr-mcp",
        "args": mcp_args(config_path),
        "env": {
            "BRUNNR_MCP_TOOL_HINT": MCP_TOOL_HINT
        }
    });
    ensure_object(&mut root, "mcpServers")?.insert(MCP_SERVER_NAME.to_string(), server);
    write_json(path, &root)
}

fn write_codex_mcp(config_path: &Path) -> Result<()> {
    let path = home_dir()?.join(".codex").join("config.toml");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut document = text.parse::<DocumentMut>().unwrap_or_default();
    ensure_toml_table(&mut document, "mcp_servers");
    document["mcp_servers"][MCP_SERVER_NAME]["command"] = value("brunnr-mcp");
    let mut args = Array::new();
    for arg in mcp_args(config_path) {
        args.push(arg);
    }
    document["mcp_servers"][MCP_SERVER_NAME]["args"] = value(args);
    document["mcp_servers"][MCP_SERVER_NAME]["env"]["BRUNNR_MCP_TOOL_HINT"] = value(MCP_TOOL_HINT);
    fs::write(path, document.to_string())?;
    Ok(())
}

fn write_zed_mcp(config_path: &Path) -> Result<()> {
    let path = zed_settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut root = read_json_object(&path)?;
    let server = json!({
        "command": {
            "path": "brunnr-mcp",
            "args": mcp_args(config_path),
            "env": {
                "BRUNNR_MCP_TOOL_HINT": MCP_TOOL_HINT
            }
        }
    });
    ensure_object(&mut root, "context_servers")?.insert(MCP_SERVER_NAME.to_string(), server);
    write_json(&path, &root)
}

fn read_json_object(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let value = serde_json::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(value)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(())
}

fn ensure_object<'a>(
    root: &'a mut Value,
    key: &str,
) -> Result<&'a mut serde_json::Map<String, Value>> {
    if !root.is_object() {
        *root = json!({});
    }
    let object = root.as_object_mut().expect("root object ensured");
    object.entry(key).or_insert_with(|| json!({}));
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .with_context(|| format!("{key} must be a JSON object"))
}

fn ensure_toml_table(document: &mut DocumentMut, key: &str) {
    if !document.as_table().contains_key(key) {
        document[key] = Item::Table(Table::new());
    }
}

fn home_dir() -> Result<PathBuf> {
    if let Some(home) = env::var_os("BRUNNR_HOME").or_else(|| env::var_os("HOME")) {
        return Ok(PathBuf::from(home));
    }
    bail!("HOME is not set")
}

fn zed_settings_path() -> Result<PathBuf> {
    let home = home_dir()?;
    let app_support = home
        .join("Library")
        .join("Application Support")
        .join("Zed")
        .join("settings.json");
    if app_support.parent().is_some_and(|parent| parent.exists()) {
        Ok(app_support)
    } else {
        Ok(home.join(".config").join("zed").join("settings.json"))
    }
}

fn detect_agents() -> Vec<AgentBinding> {
    let detected = [
        "claude",
        "claude-code",
        "codex",
        "gemini",
        "opencode",
        "ollama",
    ]
    .into_iter()
    .filter(|name| command_exists(name))
    .map(str::to_string)
    .collect::<Vec<_>>();

    let master = pick(&detected, &["claude-code", "claude", "codex"]);
    let worker = pick(&detected, &["codex", "opencode", "claude"]);
    let judge = pick(&detected, &["claude-code", "claude", "gemini", "codex"]);

    [
        (Role::Master, master),
        (Role::Worker, worker),
        (Role::Judge, judge),
    ]
    .into_iter()
    .filter_map(|(role, agent)| {
        agent.map(|agent| AgentBinding {
            role,
            command: Some(agent.clone()),
            agent,
            model: None,
            args: Vec::new(),
            timeout_seconds: Some(120),
        })
    })
    .collect()
}

fn pick(detected: &[String], preferred: &[&str]) -> Option<String> {
    preferred
        .iter()
        .find_map(|candidate| detected.iter().find(|agent| agent == candidate).cloned())
}

fn command_exists(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| dir.join(command).is_file())
}
